//! Penna's daemon (DESIGN.md §6, §7.1). Three ways to run it:
//! * production, as root: `--config` (default /etc/penna/config.toml) on this machine: NVIDIA fans (M4) and every hwmon fan of a
//!   kind Penna drives (M5, §3.5). New hwmon devices are found while it runs (§3.6);
//! * `--mock <scenario>` builds a mock desk and runs the simulator in lockstep with the control loop;
//! * `--sysfs-root <dir>` drives a tree kept by someone else.
//!
//! Surfaces talk to it over the socket in [`ipc`] (§7.3); the loop never waits for them.
//!
//! Every way out hands the fans back: a clean stop restores each held control; a panic restores from `held.json` in the panic
//! hook; anything that kills the process outright leaves `held.json` for `penna restore` (the unit's `ExecStopPost`).
mod auth;
mod burn;
mod history;
mod ipc;
mod platform_lever;
mod requests;
mod setup;
mod sleep;
mod trace;

use anyhow::{Context, Result, anyhow, bail};
use clap::Parser;
use penna_core::config::{Config, Measured};
use penna_core::control::{Command, LoopState, Outcome, tick};
use penna_core::generate::Anchors;
use penna_core::plan::Plan;
use penna_core::{Inventory, Key, Readings};
use penna_hw::held::{self, Held};
use penna_hw::sim::{self, Sim};
use penna_hw::{Machine, Options};
use penna_proto::{Event, Hello, Transition};
use std::collections::BTreeMap;
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
    about = "Penna's daemon: as root, this machine's fans; or a mock desk."
)]
struct Args {
    /// Build a mock desk from a scenario (render-burst, faults, or a scenario file) and run the simulator in lockstep.
    #[arg(long, value_name = "SCENARIO", conflicts_with = "sysfs_root")]
    mock: Option<String>,
    /// Drive the hwmon tree under DIR instead of /sys: a test tree kept by someone else. Needs --config.
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
    /// The board database (DESIGN.md §3.6).
    #[arg(long, value_name = "DIR", default_value = penna_hw::BOARDS)]
    boards: PathBuf,
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
    /// Test only: asleep from tick FROM to tick TO, as logind would say it, without logind.
    #[arg(long, hide = true, value_name = "FROM:TO", value_parser = parse_span)]
    test_sleep: Option<(u64, u64)>,
    /// Print the configuration pennad would write on its first start on this machine, read-only, and exit.
    #[arg(long, hide = true)]
    print_starter: bool,
}

fn parse_tick(s: &str) -> Result<f64, String> {
    let v = penna_core::config::parse_seconds(s).ok_or_else(|| format!("not a duration: {s}"))?;
    if (0.25..=5.0).contains(&v) { Ok(v) } else { Err("between 0.25 and 5 seconds".into()) }
}

fn parse_span(s: &str) -> Result<(u64, u64), String> {
    let (a, b) = s.split_once(':').ok_or("FROM:TO, in ticks")?;
    let (a, b) = (a.parse().map_err(|_| "FROM:TO, in ticks")?, b.parse().map_err(|_| "FROM:TO, in ticks")?);
    if a < b { Ok((a, b)) } else { Err("FROM comes before TO".into()) }
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
    /// This machine's real hardware, rather than a mock desk or a test tree.
    real: bool,
    sysfs_root: PathBuf,
    /// `None` on a first start: there is no configuration yet, and the loop writes a starter (§7.5).
    config: Option<String>,
    /// Where changes from surfaces are saved.
    config_path: Option<PathBuf>,
    measured: Measured,
    /// Where setup saves what it measures; `None`: in memory only.
    measured_path: Option<PathBuf>,
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
                measured_path: Some(args.measured.clone().unwrap_or_else(|| PathBuf::from(DEFAULT_MEASURED))),
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
                None if sim.scenario().fresh => Measured::default(),
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
                measured_path: Some(args.measured.clone().unwrap_or_else(|| tree.join("var/lib/penna/measured.toml"))),
                socket,
                scratch,
                seconds,
            })
        }
        (None, Some(root)) => {
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
                measured_path: args.measured.clone(),
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
    pub measured_path: Option<PathBuf>,
    pub cpu_model: Option<String>,
    pub readings: Readings,
    /// The loop's clock at the last tick.
    pub now: f64,
    /// Loop seconds per wall-clock second.
    pub rate: f64,
    /// A setup run (§5) and the connection it belongs to.
    pub setup: Option<setup::SetupRun>,
    /// Fans being identified (§5.3), until when.
    pub identify: std::collections::BTreeMap<penna_core::Key, f64>,
    /// What the board database knows about this machine (§3.6).
    pub board: Option<penna_core::boards::Board>,
    /// This machine's own hardware, not a mock desk or a test tree.
    pub real: bool,
    /// Calibrated fans that turn too slowly (§5.6).
    pub drift: penna_core::drift::DriftWatch,
    /// Setup's load check wants every CPU thread busy (§5.5).
    pub load_wanted: bool,
    /// Profile → the power profile it sets, where that is the only fan lever (§4.7).
    pub power: std::collections::BTreeMap<String, String>,
}

/// A draft belongs to the connection that made it: when that connection closes, the daemon goes back to `saved`.
pub struct Draft {
    pub owner: u64,
    pub config: Config,
    /// Setup generated it from this configuration (§5.5): saving it switches to Balanced (§11.3 call 9), and setup generates
    /// again from here when the roles change, so a role given up leaves no curve behind.
    pub generated: Option<Config>,
    /// The anchors of every source its generated curves follow, for the studio to draw (§5.5).
    pub anchors: BTreeMap<Key, Anchors>,
}

impl Live {
    /// Replaces the plan. Every control Penna still holds that the new plan doesn't name comes along until it is given back (§6.1).
    pub fn replace_plan(&mut self, mut plan: Plan, inventory: &Inventory) {
        plan.keep_held(&self.plan, &self.state, inventory);
        self.plan = plan;
    }

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
            setup: self.setup.as_ref().map(|s| {
                let mut p = s.run.progress();
                p.proposed = s.proposed.clone();
                p
            }),
            drift: &self.drift.drifting,
            power: self.power.clone(),
        })
    }
}

fn run(args: Args) -> Result<()> {
    let Setup { mut sim, real, sysfs_root, config, config_path, measured, measured_path, cpu_model, runtime_dir, socket, scratch, seconds } =
        setup(&args)?;
    let mock = sim.is_some();
    let opts = if real {
        Options { sysfs_root, nvml: true, mock_nvml: None, control: true, boards: Some(args.boards.clone()) }
    } else {
        Options {
            sysfs_root,
            nvml: false,
            mock_nvml: sim.as_ref().map(Sim::mock_nvml_root),
            control: true,
            boards: Some(args.boards.clone()),
        }
    };
    let mut machine = Machine::open(&opts)?;
    machine.set_pairing(&measured);
    // The power profile, through power-profiles-daemon on a real machine, or the mock tree's file (§3.5, §4.7).
    let platform = penna_hw::platform::Platform::open(&opts.sysfs_root, real);
    // Beside measured.toml: the way back for what outlives a reboot (§6.7).
    let firmware_path = measured_path.as_ref().and_then(|p| p.parent()).map(|d| d.join(held::FIRMWARE_FILE));
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
    let mut waiting = Vec::new();
    if held_path.exists() || firmware_path.as_ref().is_some_and(|p| p.exists()) {
        // A daemon before this one died holding fans, or a reboot came before it could put back what outlives one: put them
        // back before anything else.
        let r = match &firmware_path {
            Some(fw) => held::restore_both(&held_path, fw)?,
            None => held::restore_file(&held_path)?,
        };
        if !r.done.is_empty() {
            eprintln!("pennad: restored what a previous run left in {}: {}", held_path.display(), r.done.join(", "));
        }
        if !r.failed.is_empty() {
            bail!("could not restore {}: {}", held_path.display(), r.failed.join("; "));
        }
        if !r.gone.is_empty() {
            let keys: Vec<String> = r.gone.iter().map(|e| e.key().to_string()).collect();
            eprintln!("pennad: {} stay in held.json until their device returns: {}", keys.len(), keys.join(", "));
        }
        waiting = r.gone;
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
        measured_path,
        cpu_model,
        readings: machine.read(),
        now: 0.0,
        rate,
        setup: None,
        identify: std::collections::BTreeMap::new(),
        board: machine.board.clone(),
        real,
        drift: penna_core::drift::DriftWatch::default(),
        load_wanted: false,
        power: Default::default(),
    };
    let mut ring = history::Ring::default();
    let mut burn = burn::Burn::default();

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
    let mut held = Held::resume(&held_path, waiting);
    if let Some(fw) = &firmware_path {
        held = held.with_firmware(fw);
    }
    let mut lever = platform_lever::Lever::new(platform);
    let mut sleep = match args.test_sleep {
        Some((from, to)) => sleep::Sleep::script(from, to),
        None if real => sleep::Sleep::watch(),
        None => sleep::Sleep::off(),
    };
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
        if slept(&mut sleep, &mut live, &machine.inventory, &mut out, ticks, now) {
            send_events(&events, &mut out, live.state.seq, now);
        }
        // While the machine goes to sleep and until it wakes, the hardware is left alone: the loop keeps time and answers (§6.5).
        if !sleep.asleep() {
            if let Some(sim) = sim.as_mut()
                && ticks > 0
            {
                match sim.advance(args.tick) {
                    Ok(events) => out.sim_events(now, &events),
                    Err(e) => break Err(e.context("simulator")),
                }
            }
            if machine.rediscover() {
                rediscovered(&mut live, &mut machine, &mut held, &mut out, now);
            }
            // A GPU fan's speed is for people to read, and costs about a millisecond a fan; setup measures it (§3.4).
            machine.pace(if live.setup.is_some() { None } else { Some(penna_hw::nvml::SPEED_EVERY) });
            let readings = machine.read();
            if setup::step(&mut live, &machine.inventory, &readings, &mut out, now, unix_now()) {
                machine.set_pairing(&live.measured);
                setup::replan(&mut live, &machine.inventory, &mut out, now);
                setup::propose(&mut live, &machine.inventory, &mut out);
            }
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
            live.plan.drop_released(&live.state);
            lever.step(&machine.inventory, &live.state.profile, &mut held, &mut out, now);
            live.power = lever.profiles(&machine.inventory, live.plan.config.profiles.keys());
            for change in live.drift.update(&live.state, &readings, &live.measured, now) {
                drifted(&live, &mut out, now, change);
            }
            out.tick(now, &live.plan, &live.state, &readings);
            live.readings = readings;
            live.now = now;

            let body = live.tick_body(&machine.inventory);
            ring.push(&body);
            send_events(&events, &mut out, live.state.seq, body.t);
            publish.send_replace(Arc::new(ipc::Published::new(&body, ring.snapshot(body.seq, args.tick), &live.plan.config)));
        }
        match sim.as_mut() {
            Some(sim) => sim.full_cpu(live.load_wanted),
            None => burn.set(live.load_wanted),
        }
        let _ = sd_notify::notify(&[sd_notify::NotifyState::Watchdog]);
        last_ping = Instant::now();

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
        // Requests are taken at least once a tick, then until the next tick is due: a loop behind its schedule (a slow tick, a
        // fast mock) still answers them, where waiting to be ahead left them to time out.
        loop {
            // `state` right after a change shows it, without waiting for the next tick: a change is published before its reply
            // goes out, so a client that subscribes on the reply never starts from the tick before it.
            let publish_now = |live: &Live, out: &mut trace::Trace| {
                let body = live.tick_body(&machine.inventory);
                send_events(&events, out, live.state.seq, body.t);
                publish.send_replace(Arc::new(ipc::Published::new(&body, ring.snapshot(body.seq, args.tick), &live.plan.config)));
            };
            while let Ok(job) = jobs.try_recv() {
                let writes = job.op.writes();
                let result = requests::handle(&mut live, &machine.inventory, &mut out, job.op, job.uid, job.conn);
                if writes && result.is_ok() {
                    publish_now(&live, &mut out);
                }
                let _ = job.reply.send(result);
            }
            let mut changed = false;
            while let Ok(conn) = closed.try_recv() {
                changed |= requests::closed(&mut live, &machine.inventory, &mut out, conn);
            }
            if changed {
                publish_now(&live, &mut out);
            }
            if slept(&mut sleep, &mut live, &machine.inventory, &mut out, ticks, now) {
                send_events(&events, &mut out, live.state.seq, now);
            }
            if ping_every.is_some_and(|p| last_ping.elapsed() >= p) {
                let _ = sd_notify::notify(&[sd_notify::NotifyState::Watchdog]);
                last_ping = Instant::now();
            }
            let left = due.saturating_duration_since(Instant::now());
            if left.is_zero() || stop.load(Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(left.min(REQUEST_POLL));
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

/// The hwmon devices changed (§3.6): plan again against what is there now, say what came and went, and point `held.json` at
/// the files of a held control that came back under a new path.
fn rediscovered(live: &mut Live, machine: &mut Machine, held: &mut Held, out: &mut trace::Trace, now: f64) {
    let before: std::collections::BTreeMap<penna_core::Key, String> =
        live.plan.controls.iter().filter(|(_, c)| c.kind.is_some()).map(|(k, c)| (k.clone(), c.label.clone())).collect();
    out.devices_changed(now, &machine.inventory);
    let cfg = live.draft.as_ref().map_or_else(|| live.saved.clone(), |d| d.config.clone());
    match Plan::new(cfg, &machine.inventory, &live.measured, live.cpu_model.as_deref()) {
        Ok(plan) => live.replace_plan(plan, &machine.inventory),
        Err(problems) => {
            let list: Vec<String> = problems.iter().map(|p| p.to_string()).collect();
            out.note(
                now,
                penna_proto::Level::Alert,
                None,
                format!("The machine changed, and the configuration no longer fits it: {}. Penna keeps the last plan.", list.join("; ")),
            );
            return;
        }
    }
    for (key, label) in &before {
        let gone = live.plan.controls.get(key).is_none_or(|c| c.kind.is_none());
        if gone && live.state.controls.get(key).is_some_and(|c| c.held) {
            out.note(
                now,
                penna_proto::Level::Alert,
                Some(key.clone()),
                format!(
                    "{label} went away while Penna held it: its chip keeps the last duty until the driver is loaded again or the \
                     machine restarts"
                ),
            );
        }
    }
    let entries: Vec<_> = held.entries().to_vec();
    for e in entries {
        let Some(moved) = machine.relocate(&e) else { continue };
        let key = e.key().clone();
        if live.state.controls.get(&key).is_some_and(|c| c.held) {
            if moved != e
                && let Err(err) = held.add(moved)
            {
                out.note(now, penna_proto::Level::Alert, Some(key), format!("could not update held.json: {err:#}"));
            }
            continue;
        }
        // Back from a previous run's held.json: it was waiting for this device to return, to be put back as it was found.
        let result =
            machine.release(&moved).map_err(|e| format!("{e:#}")).and_then(|s| held.remove(&key).map(|_| s).map_err(|e| format!("{e:#}")));
        match result {
            Ok(s) => out.note(now, penna_proto::Level::Info, Some(key), format!("Back, and put back as Penna found it: {s}")),
            Err(err) => out.note(now, penna_proto::Level::Alert, Some(key), format!("Back, but could not be put back: {err}")),
        }
    }
}

/// What logind said about sleep, taken between ticks (§6.5). Returns whether anything was said.
fn slept(sleep: &mut sleep::Sleep, live: &mut Live, inventory: &Inventory, out: &mut trace::Trace, ticks: u64, now: f64) -> bool {
    use penna_proto::Level;
    let changes = sleep.poll(ticks, now);
    for change in &changes {
        match change {
            sleep::Change::Going => {
                setup::stop(live, inventory, out, "The machine went to sleep.");
                out.note(now, Level::Info, None, "Going to sleep: Penna leaves the fans alone until the machine wakes".into());
            }
            sleep::Change::Woke => {
                live.state.woke();
                out.note(now, Level::Info, None, "Awake: every fan Penna drives is written again".into());
            }
            sleep::Change::GaveUp => {
                live.state.woke();
                out.note(
                    now,
                    Level::Warn,
                    None,
                    format!("No word that the machine woke {:.0} s after it was to sleep: Penna drives the fans again", sleep::GIVE_UP),
                );
            }
            sleep::Change::Trouble(why) => {
                out.note(now, Level::Warn, None, format!("{why}. Read-back catches what sleep changes instead (§6.4)"))
            }
        }
    }
    !changes.is_empty()
}

/// A fan that drifted, or recovered, said once (§5.6).
fn drifted(live: &Live, out: &mut trace::Trace, now: f64, change: penna_core::drift::DriftChange) {
    use penna_core::drift::DriftChange;
    let label = |k: &penna_core::Key| live.plan.controls.get(k).map_or_else(|| k.to_string(), |c| c.label.clone());
    match change {
        DriftChange::Started { key, slower } => {
            let text = format!(
                "{} turns {:.0} % slower than when it was set up: dust, or a tired bearing. Recalibrate it from its card",
                label(&key),
                slower * 100.0
            );
            out.note(now, penna_proto::Level::Warn, Some(key), text);
        }
        DriftChange::Recovered { key } => {
            let text = format!("{} turns as calibrated again", label(&key));
            out.note(now, penna_proto::Level::Info, Some(key), text);
        }
    }
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
            // A control Penna already held before its device went away keeps what it was found as, not the manual mode Penna left.
            let entry = match held.get(key).and_then(|old| machine.relocate(old)) {
                Some(kept) => kept,
                None => machine.original(key)?,
            };
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
        Command::Curve { key, points } => machine.write_curve(key, points).map(|_| Outcome::Done),
        Command::Release { key } => {
            let entry = held.get(key).cloned().ok_or_else(|| anyhow!("{key} is not in held.json"))?;
            machine.release(&entry)?;
            held.remove(key)?;
            Ok(Outcome::Done)
        }
    })();
    r.unwrap_or_else(|e| Outcome::Failed(format!("{e:#}")))
}

/// Every held control back to exactly what it was. What cannot go back stays in held.json for `penna restore`, and so does a
/// control whose device is gone, which waits there for the device to return (§3.6).
fn hand_back(machine: &mut Machine, held: &mut Held) -> Vec<Result<String, String>> {
    let entries: Vec<_> = held.entries().to_vec();
    let mut out = Vec::new();
    for e in entries {
        if held::gone(&e) {
            out.push(Ok(format!("{}: its device is gone; kept in held.json until it returns", e.key())));
            continue;
        }
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
