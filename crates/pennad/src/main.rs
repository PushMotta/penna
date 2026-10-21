//! Penna's daemon (DESIGN.md §6, §7.1). It drives simulated hardware only until M4: `--mock <scenario>` builds a mock desk and
//! runs the simulator in lockstep with the control loop, and `--sysfs-root <dir>` drives a tree kept by someone else. Surfaces
//! talk to it over the socket in [`ipc`] (§7.3); the loop never waits for them.
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
/// How often the loop looks for requests between ticks.
const REQUEST_POLL: Duration = Duration::from_millis(20);

#[derive(Parser)]
#[command(name = "pennad", version, about = "Penna's daemon. Until M4 it controls simulated hardware only.")]
struct Args {
    /// Build a mock desk from a scenario (render-burst, faults, or a scenario file) and run the simulator in lockstep.
    #[arg(long, value_name = "SCENARIO", conflicts_with = "sysfs_root")]
    mock: Option<String>,
    /// Drive the hwmon tree under DIR, which is never the real /sys until M4. Needs --config.
    #[arg(long, value_name = "DIR")]
    sysfs_root: Option<PathBuf>,
    /// The configuration. Default for --mock: the one the scenario names, copied into the mock tree, where changes are saved.
    #[arg(long, value_name = "FILE")]
    config: Option<PathBuf>,
    /// Pairing and calibration. Default for --mock: what the simulator knows.
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
    match run(Args::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("pennad: {e:#}");
            ExitCode::FAILURE
        }
    }
}

struct Setup {
    sim: Option<Sim>,
    sysfs_root: PathBuf,
    config: String,
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
        (None, None) => bail!(
            "until M4 Penna controls simulated hardware only; real control arrives in M4 (NVIDIA) and M5 (motherboard).\n\
             Try: pennad --mock render-burst --forever, or pennad --mock faults --speed 10"
        ),
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
                sysfs_root: sim.root().to_path_buf(),
                cpu_model: Some(sim.scenario().cpu_model.clone()),
                runtime_dir: args.runtime_dir.clone().unwrap_or_else(|| tree.join("run")),
                sim: Some(sim),
                config,
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
                sysfs_root: root.clone(),
                config: std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?,
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
    let Setup { mut sim, sysfs_root, config, config_path, measured, cpu_model, runtime_dir, socket, scratch, seconds } = setup(&args)?;
    let mock = sim.is_some();
    let config = Config::parse(&config).map_err(|e| anyhow!("configuration: {e}"))?;
    let opts = Options { sysfs_root, nvml: false, mock_nvml: sim.as_ref().map(Sim::mock_nvml_root), control: true };
    let mut machine = Machine::open(&opts)?;
    let plan = Plan::new(config, &machine.inventory, &measured, cpu_model.as_deref()).map_err(|problems| {
        anyhow!(
            "the configuration does not fit this machine:\n  {}",
            problems.iter().map(|p| p.to_string()).collect::<Vec<_>>().join("\n  ")
        )
    })?;

    std::fs::create_dir_all(&runtime_dir).with_context(|| format!("creating {} (try --runtime-dir)", runtime_dir.display()))?;
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
            std::thread::sleep((due - Instant::now()).min(REQUEST_POLL));
        }
    };

    let _ = sd_notify::notify(&[sd_notify::NotifyState::Stopping]);
    drop(publish);
    server.stop();
    let handed_back = hand_back(&mut held);
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
                    let _ = held::restore(&entry);
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
            held::restore(&entry)?;
            held.remove(key)?;
            Ok(Outcome::Done)
        }
    })();
    r.unwrap_or_else(|e| Outcome::Failed(format!("{e:#}")))
}

/// Every held control back to exactly what it was. What cannot go back stays in held.json for `penna restore`.
fn hand_back(held: &mut Held) -> Vec<Result<String, String>> {
    let entries: Vec<_> = held.entries().to_vec();
    let mut out = Vec::new();
    for e in entries {
        match held::restore(&e) {
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
