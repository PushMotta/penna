//! Penna's daemon (DESIGN.md §6, §7.1). Three ways to run it:
//! * production, as root: `--config` (default /etc/penna/config.toml) on this machine. NVIDIA fans are controlled for real
//!   (M4); motherboard fans are read but not driven until M5, so a configuration that assigns one is refused;
//! * `--mock <scenario>` builds a mock desk and runs the simulator in lockstep with the control loop;
//! * `--sysfs-root <dir>` drives a tree kept by someone else.
//!
//! Surfaces talk to it over the socket in [`ipc`] (§7.3); the loop never waits for them.
//!
//! Every way out hands the fans back: a clean stop restores each held control; a panic restores from `held.json` in the panic
//! hook; anything that kills the process outright leaves `held.json` for `penna restore` (the unit's `ExecStopPost`).
mod auth;
mod history;
mod ipc;
mod requests;
mod trace;

use anyhow::{Context, Result, anyhow, bail};
use clap::Parser;
use penna_core::config::{Config, Measured};
use penna_core::control::{Command, LoopState, Outcome, tick};
use penna_core::plan::Plan;
use penna_core::{Inventory, Readings};
use penna_hw::held::{self, Held};
use penna_hw::sim::{self, Sim};
use penna_hw::{Machine, Options};
use penna_proto::{Event, Hello, Transition};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{broadcast, watch};

const DEFAULT_RUNTIME_DIR: &str = "/run/penna";
const DEFAULT_CONFIG: &str = "/etc/penna/config.toml";
const DEFAULT_MEASURED: &str = "/var/lib/penna/measured.toml";
const NOT_ROOT: &str = "pennad controls fans and has to run as root. For a simulated desk: pennad --mock render-burst";
/// How often the loop looks for requests between ticks.
const REQUEST_POLL: Duration = Duration::from_millis(20);

#[derive(Parser)]
#[command(
    name = "pennad",
    version = env!("PENNA_VERSION"),
    about = "Penna's daemon: as root, this machine's fans (NVIDIA from M4, motherboard from M5); or a mock desk."
)]
struct Args {
    /// Build a mock desk from a scenario (render-burst, faults, or a scenario file) and run the simulator in lockstep.
    #[arg(long, value_name = "SCENARIO", conflicts_with = "sysfs_root")]
    mock: Option<String>,
    /// Drive the hwmon tree under DIR, which is never the real /sys until M5. Needs --config.
    #[arg(long, value_name = "DIR")]
    sysfs_root: Option<PathBuf>,
    /// The configuration, where changes from surfaces are saved. Default: /etc/penna/config.toml; for --mock, the one the
    /// scenario names, copied into the mock tree.
    #[arg(long, value_name = "FILE")]
    config: Option<PathBuf>,
    /// Pairing and calibration. Default: /var/lib/penna/measured.toml when it exists; for --mock, what the simulator knows.
    #[arg(long, value_name = "FILE")]
    measured: Option<PathBuf>,
    /// Where surfaces connect. Default: /run/penna/penna.sock, or $XDG_RUNTIME_DIR/penna/penna.sock with --mock.
    #[arg(long, value_name = "PATH")]
    socket: Option<PathBuf>,
    /// Seconds between ticks, 0.25 to 5.
    #[arg(long, default_value_t = 1.0, value_parser = parse_tick)]
    tick: f64,
    /// Where held.json lives. Default: /run/penna, or a directory beside the mock tree.
    #[arg(long, value_name = "DIR")]
    runtime_dir: Option<PathBuf>,
    /// Mock: build the tree here, and keep it on exit, instead of in a new temporary directory.
    #[arg(long, value_name = "DIR")]
    tree: Option<PathBuf>,
    /// Mock: run simulated time this many times faster than the wall clock.
    #[arg(long, default_value_t = 1.0, value_parser = parse_speed)]
    speed: f64,
    /// Stop, handing every fan back, after this many seconds of loop time. Default for --mock: the scenario's length.
    #[arg(long)]
    seconds: Option<f64>,
    /// Run until stopped, even in mock mode.
    #[arg(long, conflicts_with = "seconds")]
    forever: bool,
    /// Only what changes: states, faults, emergencies. No line per tick.
    #[arg(long)]
    quiet: bool,
    /// Test only: panic after this many ticks, to prove the panic hook hands the fans back.
    #[arg(long, hide = true)]
    test_panic_after: Option<u64>,
    /// Test only: after this many ticks the loop stops ticking and stops pinging the watchdog, as a deadlock would, so systemd's
    /// WatchdogSec kills it. SIGTERM still stops it cleanly.
    #[arg(long, hide = true)]
    test_hang_after: Option<u64>,
    /// Print the configuration pennad would write on its first start on this machine, read-only, and exit.
    #[arg(long, hide = true)]
    print_starter: bool,
}

fn parse_tick(s: &str) -> Result<f64, String> {
    let v = penna_core::config::parse_seconds(s).ok_or_else(|| format!("not a duration: {s}"))?;
    if (0.25..=5.0).contains(&v) { Ok(v) } else { Err("between 0.25 and 5 seconds".into()) }
}

fn parse_speed(s: &str) -> Result<f64, String> {
    let v: f64 = s.parse().map_err(|_| format!("not a number: {s}"))?;
    if (0.1..=1000.0).contains(&v) { Ok(v) } else { Err("between 0.1 and 1000".into()) }
}

fn main() -> ExitCode {
    let args = Args::parse();
    let result = if args.print_starter { print_starter() } else { run(args) };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            let text = format!("{e:#}");
            if text.starts_with("pennad ") {
                eprintln!("{text}")
            } else {
                eprintln!("pennad: {text}")
            }
            ExitCode::FAILURE
        }
    }
}

struct Setup {
    sim: Option<Sim>,
    /// This machine's real hardware: NVIDIA under control, motherboard fans read only (M4).
    real: bool,
    sysfs_root: PathBuf,
    /// `None` on a first start: there is no configuration yet, and the loop writes a starter (§7.5).
    config: Option<String>,
    /// Where changes from surfaces are saved.
    config_path: Option<PathBuf>,
    measured: Measured,
    cpu_model: Option<String>,
    runtime_dir: PathBuf,
    socket: PathBuf,
    /// A temporary tree to remove after a clean exit.
    scratch: Option<PathBuf>,
    seconds: Option<f64>,
}

fn setup(args: &Args) -> Result<Setup> {
    match (&args.mock, &args.sysfs_root) {
        (None, None) => {
            // SAFETY: geteuid has no preconditions.
            let root = unsafe { libc::geteuid() } == 0;
            let path = args.config.clone().unwrap_or_else(|| PathBuf::from(DEFAULT_CONFIG));
            let config = match std::fs::read_to_string(&path) {
                Ok(c) => Some(c),
                Err(_) if !root => bail!("{NOT_ROOT}"),
                // A first start: the loop writes a starter once it knows the machine.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => {
                    return Err(anyhow!(e)
                        .context(format!("reading {} (write one, check it with `penna check`, or pass --config)", path.display())));
                }
            };
            // What the configuration asks for is explained first, root or not.
            let assigned = config.as_deref().and_then(|c| Config::parse(c).ok()).map(|c| c.assigned_motherboard()).unwrap_or_default();
            if !assigned.is_empty() {
                bail!("Motherboard fans arrive in M5; this config assigns {}. Set them to \"firmware\" for now.", assigned.join(", "));
            }
            if !root {
                bail!("{NOT_ROOT}");
            }
            let measured = match &args.measured {
                Some(p) => read_measured(p)?,
                None if Path::new(DEFAULT_MEASURED).exists() => read_measured(Path::new(DEFAULT_MEASURED))?,
                None => Measured::default(),
            };
            Ok(Setup {
                sim: None,
                real: true,
                sysfs_root: PathBuf::from("/sys"),
                config,
                config_path: Some(path),
                measured,
                cpu_model: cpu_model(),
                runtime_dir: args.runtime_dir.clone().unwrap_or_else(|| PathBuf::from(DEFAULT_RUNTIME_DIR)),
                socket: args.socket.clone().unwrap_or_else(|| PathBuf::from(penna_proto::PRODUCTION_SOCKET)),
                scratch: None,
                seconds: args.seconds,
            })
        }
        (Some(name), _) => {
            let (scenario, config) = sim::load(name)?;
            let (tree, scratch) = match &args.tree {
                Some(t) => (t.clone(), None),
                None => {
                    let t = std::env::temp_dir().join(format!("penna-mock-{}", std::process::id()));
                    (t.clone(), Some(t))
                }
            };
            let seconds = if args.forever { None } else { args.seconds.or(Some(scenario.seconds)) };
            let socket = match &args.socket {
                Some(s) => s.clone(),
                None => penna_proto::mock_socket()
                    .ok_or_else(|| anyhow!("XDG_RUNTIME_DIR is not set, so there is no default socket; pass --socket"))?,
            };
            let sim = Sim::create(scenario, &tree.join("sys")).with_context(|| format!("building the mock desk in {}", tree.display()))?;
            let config = match &args.config {
                Some(p) => std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?,
                None => config,
            };
            let config_path = tree.join("etc/penna/config.toml");
            held::write_atomically(&config_path, config.as_bytes())
                .with_context(|| format!("copying the configuration to {}", config_path.display()))?;
            let measured = match &args.measured {
                Some(p) => read_measured(p)?,
                None => sim.measured(),
            };
            Ok(Setup {
                real: false,
                sysfs_root: sim.root().to_path_buf(),
                cpu_model: Some(sim.scenario().cpu_model.clone()),
                runtime_dir: args.runtime_dir.clone().unwrap_or_else(|| tree.join("run")),
                sim: Some(sim),
                config: Some(config),
                config_path: Some(config_path),
                measured,
                socket,
                scratch,
                seconds,
            })
        }
        (None, Some(root)) => {
            penna_hw::refuse_real_sysfs(root)?;
            let path = args.config.as_ref().ok_or_else(|| anyhow!("--sysfs-root needs --config"))?;
            Ok(Setup {
                sim: None,
                real: false,
                sysfs_root: root.clone(),
                config: Some(std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?),
                config_path: Some(path.clone()),
                measured: match &args.measured {
                    Some(p) => read_measured(p)?,
                    None => Measured::default(),
                },
                cpu_model: cpu_model(),
                runtime_dir: args.runtime_dir.clone().unwrap_or_else(|| PathBuf::from(DEFAULT_RUNTIME_DIR)),
                socket: args.socket.clone().unwrap_or_else(|| PathBuf::from(penna_proto::PRODUCTION_SOCKET)),
                scratch: None,
                seconds: args.seconds,
            })
        }
    }
}

/// No configuration yet (§7.5): write a starter for this machine, which adopts nothing, or run with it in memory when its
/// directory can't be written.
fn first_start(inventory: &Inventory, path: Option<&Path>) -> Config {
    let config = penna_core::starter::starter(inventory);
    let summary = penna_core::starter::summary(&config);
    match path {
        Some(p) => match held::write_atomically(p, penna_core::starter::text(&config).as_bytes()) {
            Ok(()) => eprintln!("pennad: no configuration yet, so wrote a starter to {}: {summary}.", p.display()),
            Err(e) => eprintln!(
                "pennad: no configuration yet, and {} can't be written ({e:#}): running with a starter in memory: {summary}.",
                p.display()
            ),
        },
        None => eprintln!("pennad: no configuration yet: running with a starter in memory: {summary}."),
    }
    config
}

/// `--print-starter`: what a first start would write on this machine. Read-only, and needs no root.
fn print_starter() -> Result<()> {
    let machine = Machine::open(&Options::default())?;
    print!("{}", penna_core::starter::text(&penna_core::starter::starter(&machine.inventory)));
    Ok(())
}

fn read_measured(path: &Path) -> Result<Measured> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    Measured::parse(&text).map_err(|e| anyhow!("{}: {e}", path.display()))
}

fn cpu_model() -> Option<String> {
    let info = std::fs::read_to_string("/proc/cpuinfo").ok()?;
    info.lines().find_map(|l| l.strip_prefix("model name")).and_then(|l| l.split_once(':')).map(|(_, m)| m.trim().to_owned())
}

fn unix_now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}

/// The loop's state between ticks, which requests change.
pub struct Live {
    /// What the loop runs: the saved configuration, or a studio's draft on top of it.
    pub plan: Plan,
    /// The configuration as saved (§7.5).
    pub saved: Config,
    /// A studio's unsaved changes, previewing on the fans (§7.4). There is one only while it differs from `saved`.
    pub draft: Option<Draft>,
    pub state: LoopState,
    pub config_path: Option<PathBuf>,
    pub measured: Measured,
    pub cpu_model: Option<String>,
    pub readings: Readings,
    /// The loop's clock at the last tick.
    pub now: f64,
    /// Loop seconds per wall-clock second.
    pub rate: f64,
    /// Motherboard fans may be assigned curves: false on this machine's real hardware until M5.
    pub motherboard: bool,
}

/// A draft belongs to the connection that made it: when that connection closes, the daemon goes back to `saved`.
pub struct Draft {
    pub owner: u64,
    pub config: Config,
}

impl Live {
    pub fn draft_tick(&self) -> Option<penna_proto::DraftTick> {
        self.draft.as_ref().map(|d| {
            let (curves, controls) = penna_core::config::changes(&self.saved, &d.config);
            penna_proto::DraftTick { curves, controls }
        })
    }

    pub fn tick_body(&self, inventory: &Inventory) -> penna_proto::Tick {
        penna_proto::build::tick(&penna_proto::build::Context {
            plan: &self.plan,
            state: &self.state,
            readings: &self.readings,
            inventory,
            measured: &self.measured,
            seq: self.state.seq,
            t: unix_now(),
            now: self.now,
            rate: self.rate,
            draft: self.draft_tick(),
        })
    }
}

fn run(args: Args) -> Result<()> {
    let Setup { mut sim, real, sysfs_root, config, config_path, measured, cpu_model, runtime_dir, socket, scratch, seconds } =
        setup(&args)?;
    let mock = sim.is_some();
    let opts = if real {
        // Motherboard fans read only; NVIDIA fans under control.
        Options { sysfs_root, nvml: true, mock_nvml: None, control: false }
    } else {
        Options { sysfs_root, nvml: false, mock_nvml: sim.as_ref().map(Sim::mock_nvml_root), control: true }
    };
    let mut machine = Machine::open(&opts)?;
    let config = match config {
        Some(text) => Config::parse(&text).map_err(|e| anyhow!("configuration: {e}"))?,
        None => first_start(&machine.inventory, config_path.as_deref()),
    };
    let plan = Plan::new(config, &machine.inventory, &measured, cpu_model.as_deref()).map_err(|problems| {
        anyhow!(
            "the configuration does not fit this machine:\n  {}",
            problems.iter().map(|p| p.to_string()).collect::<Vec<_>>().join("\n  ")
        )
    })?;

    std::fs::create_dir_all(&runtime_dir).with_context(|| format!("creating {} (try --runtime-dir)", runtime_dir.display()))?;
    if real {
        // Anyone may reach the socket inside; every request is then checked against its peer's credentials (§7.4).
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&runtime_dir, std::fs::Permissions::from_mode(0o755))
            .with_context(|| format!("opening up {}", runtime_dir.display()))?;
    }
    let held_path = runtime_dir.join(held::FILE);
    if held_path.exists() {
        // A daemon before this one died holding fans: put them back before anything else.
        let r = held::restore_file(&held_path)?;
        eprintln!("pennad: restored what a previous run left in {}: {}", held_path.display(), r.done.join(", "));
        if !r.failed.is_empty() {
            bail!("could not restore {}: {}", held_path.display(), r.failed.join("; "));
        }
    }

    let rate = if mock { args.speed } else { 1.0 };
    let saved = plan.config.clone();
    let mut live = Live {
        state: LoopState::new(&plan),
        plan,
        saved,
        draft: None,
        config_path,
        measured,
        cpu_model,
        readings: machine.read(),
        now: 0.0,
        rate,
        motherboard: !real,
    };
    let mut ring = history::Ring::default();

    // The socket comes up before any fan is taken: if it cannot, nothing has changed.
    let first = ipc::Published::new(&live.tick_body(&machine.inventory), ring.snapshot(0, args.tick), &live.plan.config);
    let (publish, published) = watch::channel(Arc::new(first));
    let (events, _) = broadcast::channel::<Arc<str>>(ipc::EVENT_BACKLOG);
    let (jobs_tx, jobs) = std::sync::mpsc::sync_channel::<ipc::Job>(ipc::QUEUE);
    // Connections that closed, so a draft dies with the studio that made it. Unbounded: one notice per connection.
    let (closed_tx, closed) = std::sync::mpsc::channel::<u64>();
    let server = ipc::start(
        &socket,
        ipc::Setup {
            hello: Hello {
                v: penna_proto::VERSION,
                daemon: env!("CARGO_PKG_VERSION").to_owned(),
                mock,
                tick: args.tick,
                can_write: false,
                why: None,
            },
            policy: auth::Policy { daemon_uid: unsafe { libc::geteuid() }, mock, penna_gid: auth::penna_gid() },
            published,
            events: events.clone(),
            jobs: jobs_tx,
            closed: closed_tx,
        },
    )?;

    install_panic_hook(held_path.clone());
    let stop = Arc::new(AtomicBool::new(false));
    for sig in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT] {
        signal_hook::flag::register(sig, stop.clone()).context("installing signal handlers")?;
    }

    let mut out = trace::Trace::new(&live.plan, &live.measured, args.quiet);
    out.banner(&live.plan, &machine, sim.as_ref(), (&held_path, &socket), args.tick, args.speed, seconds);
    let mut held = Held::new(&held_path);
    // The loop pings the watchdog after every tick, and between ticks whenever half of WatchdogSec has passed: never from a
    // thread of its own, so a loop that stops stops the pings (§6.2).
    let ping_every = sd_notify::watchdog_enabled().map(|d| d / 2);
    if let Some(p) = ping_every
        && p.as_secs_f64() < args.tick / rate
    {
        eprintln!(
            "pennad: WatchdogSec ({:.1} s) is shorter than two ticks; the loop pings between ticks to keep up",
            p.as_secs_f64() * 2.0
        );
    }
    let mut last_ping: Instant;
    let _ = sd_notify::notify(&[sd_notify::NotifyState::Ready]);

    let start = Instant::now();
    let pace = Duration::from_secs_f64(args.tick / rate);
    let mut ticks: u64 = 0;
    let result = loop {
        if stop.load(Ordering::SeqCst) {
            break Ok(());
        }
        let now = if mock { ticks as f64 * args.tick } else { start.elapsed().as_secs_f64() };
        if let Some(sim) = sim.as_mut()
            && ticks > 0
        {
            match sim.advance(args.tick) {
                Ok(events) => out.sim_events(now, &events),
                Err(e) => break Err(e.context("simulator")),
            }
        }
        let readings = machine.read();
        let (next, cmds) = tick(&live.plan, &live.state, &readings, now);
        live.state = next;
        let mut queue: std::collections::VecDeque<Command> = cmds.into();
        while let Some(cmd) = queue.pop_front() {
            let outcome = carry_out(&mut machine, &mut held, &cmd);
            out.command(now, &live.plan, &live.state, &cmd, &outcome);
            if let Some(follow) = live.state.record(&live.plan, &cmd, &outcome) {
                queue.push_back(follow);
            }
        }
        let _ = sd_notify::notify(&[sd_notify::NotifyState::Watchdog]);
        last_ping = Instant::now();
        out.tick(now, &live.plan, &live.state, &readings);
        live.readings = readings;
        live.now = now;

        let body = live.tick_body(&machine.inventory);
        ring.push(&body);
        send_events(&events, &mut out, live.state.seq, body.t);
        publish.send_replace(Arc::new(ipc::Published::new(&body, ring.snapshot(body.seq, args.tick), &live.plan.config)));

        ticks += 1;
        if args.test_panic_after == Some(ticks) {
            panic!("test panic after {ticks} ticks");
        }
        if args.test_hang_after == Some(ticks) {
            eprintln!("pennad: test hang after {ticks} ticks: the loop no longer ticks or pings the watchdog");
            while !stop.load(Ordering::SeqCst) {
                std::thread::sleep(REQUEST_POLL);
            }
            break Ok(());
        }
        if seconds.is_some_and(|s| now >= s) {
            break Ok(());
        }
        let due = start + pace * ticks as u32;
        while Instant::now() < due && !stop.load(Ordering::SeqCst) {
            let mut changed = false;
            while let Ok(job) = jobs.try_recv() {
                let writes = job.op.writes();
                let result = requests::handle(&mut live, &machine.inventory, &mut out, job.op, job.uid, job.conn);
                changed |= writes && result.is_ok();
                let _ = job.reply.send(result);
            }
            while let Ok(conn) = closed.try_recv() {
                changed |= requests::closed(&mut live, &machine.inventory, &mut out, conn);
            }
            if changed {
                // `state` right after a change shows it, without waiting for the next tick.
                let body = live.tick_body(&machine.inventory);
                send_events(&events, &mut out, live.state.seq, body.t);
                publish.send_replace(Arc::new(ipc::Published::new(&body, ring.snapshot(body.seq, args.tick), &live.plan.config)));
            }
            if ping_every.is_some_and(|p| last_ping.elapsed() >= p) {
                let _ = sd_notify::notify(&[sd_notify::NotifyState::Watchdog]);
                last_ping = Instant::now();
            }
            std::thread::sleep((due - Instant::now()).min(REQUEST_POLL));
        }
    };

    let _ = sd_notify::notify(&[sd_notify::NotifyState::Stopping]);
    drop(publish);
    server.stop();
    let handed_back = hand_back(&mut machine, &mut held);
    out.shutdown(&handed_back);
    if let Some(dir) = scratch
        && result.is_ok()
        && handed_back.iter().all(|r| r.is_ok())
    {
        let _ = std::fs::remove_dir_all(dir);
    }
    result?;
    if let Some(Err(e)) = handed_back.into_iter().find(Result::is_err) {
        bail!("could not hand every fan back: {e}");
    }
    Ok(())
}

fn send_events(events: &broadcast::Sender<Arc<str>>, out: &mut trace::Trace, seq: u64, t: f64) {
    for (level, control, text) in out.take_events() {
        // No subscriber is not an error: the event is simply not wanted yet.
        let _ = events.send(Event::Event(Transition { seq, t, level, control, text }).line().into());
    }
}

/// One command against the hardware. A take writes the way back to held.json first, and undoes itself if it fails half way.
fn carry_out(machine: &mut Machine, held: &mut Held, cmd: &Command) -> Outcome {
    let r: Result<Outcome> = (|| match cmd {
        Command::Take { key } => {
            let entry = machine.original(key)?;
            held.add(entry.clone()).context("writing held.json")?;
            match machine.take(key) {
                Ok(duty) => Ok(Outcome::Taken { duty }),
                Err(e) => {
                    let _ = machine.release(&entry);
                    let _ = held.remove(key);
                    Err(e)
                }
            }
        }
        Command::Write { key, raw } => machine.write(key, *raw).map(|_| Outcome::Done),
        Command::Reassert { key, raw } => machine.reassert(key, *raw).map(|_| Outcome::Done),
        Command::Handback { key } => machine.handback(key).map(|_| Outcome::Done),
        Command::Release { key } => {
            let entry = held.get(key).cloned().ok_or_else(|| anyhow!("{key} is not in held.json"))?;
            machine.release(&entry)?;
            held.remove(key)?;
            Ok(Outcome::Done)
        }
    })();
    r.unwrap_or_else(|e| Outcome::Failed(format!("{e:#}")))
}

/// Every held control back to exactly what it was. What cannot go back stays in held.json for `penna restore`.
fn hand_back(machine: &mut Machine, held: &mut Held) -> Vec<Result<String, String>> {
    let entries: Vec<_> = held.entries().to_vec();
    let mut out = Vec::new();
    for e in entries {
        match machine.release(&e) {
            Ok(s) => {
                let _ = held.remove(e.key());
                out.push(Ok(s));
            }
            Err(err) => out.push(Err(format!("{err:#}"))),
        }
    }
    out
}

/// The hook restores from held.json on disk: an immutable snapshot, no locks, nothing shared with the loop that panicked.
fn install_panic_hook(held_path: PathBuf) {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        eprintln!("pennad: {info}");
        if held_path.exists() {
            match held::restore_file(&held_path) {
                Ok(r) if r.failed.is_empty() => eprintln!("pennad: handed every fan back: {}", r.done.join(", ")),
                Ok(r) => eprintln!("pennad: could not hand back {}; held.json kept for penna restore", r.failed.join("; ")),
                Err(e) => eprintln!("pennad: could not read {}: {e:#}", held_path.display()),
            }
        }
        default(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use penna_core::{ControlInfo, ControlKind, Device, Key, SensorInfo, Source};

    fn desk() -> Inventory {
        let uuid = "GPU-1";
        let sensor =
            |key: Key, label: &str| SensorInfo { key, label: label.into(), limits: vec![], ignored_limits: vec![], below_ambient: false };
        Inventory {
            devices: vec![
                Device {
                    id: "hwmon/k10temp/0000:00:18.3".into(),
                    source: Source::Hwmon,
                    driver: "k10temp".into(),
                    label: "AMD CPU".into(),
                    location: "0000:00:18.3".into(),
                    model: None,
                    sensors: vec![sensor(Key::hwmon("k10temp", "0000:00:18.3", "temp1"), "Tctl")],
                    tachs: vec![],
                    controls: vec![],
                },
                Device {
                    id: format!("nvml/{uuid}"),
                    source: Source::Nvml,
                    driver: "nvml".into(),
                    label: "GPU".into(),
                    location: String::new(),
                    model: None,
                    sensors: vec![sensor(Key::nvml_temp(uuid), "GPU")],
                    tachs: vec![],
                    controls: vec![ControlInfo {
                        key: Key::nvml_fan(uuid, 0),
                        label: "Fan 1".into(),
                        kind: ControlKind::Nvml { min: 30, max: 100 },
                    }],
                },
            ],
            ..Inventory::default()
        }
    }

    #[test]
    fn a_first_start_writes_a_starter_that_reads_back_and_adopts_nothing() {
        let dir = std::env::temp_dir().join(format!("pennad-first-start-{}", std::process::id()));
        let path = dir.join("etc/penna/config.toml");
        let config = first_start(&desk(), Some(&path));
        let text = std::fs::read_to_string(&path).expect("the starter was written, directories and all");
        assert!(text.starts_with("# Penna's configuration, generated by pennad on its first start"));
        assert_eq!(Config::parse(&text).expect("it reads back"), config);
        assert!(config.profiles.values().all(|p| p.is_empty()));
        Plan::new(config, &desk(), &Measured::default(), None).expect("it fits the machine it was made for");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_first_start_that_cannot_write_runs_with_the_starter_in_memory() {
        let config = first_start(&desk(), Some(Path::new("/proc/penna/config.toml")));
        assert_eq!(config.curves.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), ["gpu", "cpu-quiet"]);
        assert!(!Path::new("/proc/penna/config.toml").exists());
    }
}
