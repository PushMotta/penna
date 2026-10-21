//! `penna`: watch the machine, change what the daemon does, check a configuration, and put every fan back from `held.json`.
//!
//! Commands that change fans talk to `pennad` over its socket (DESIGN.md §7.3, [`client`]). `watch` uses the daemon when one
//! answers and otherwise reads the hardware directly, as M0 did; `restore`, `check` and `capture` never need the daemon.
mod client;
mod doctor;
mod render;
mod waybar;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand, ValueEnum};
use client::Client;
use penna_hw::{Machine, Options};
use penna_proto::hardware::Snapshot;
use penna_proto::{Event, Level, Op, Tick};
use serde_json::Value;
use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Parser)]
#[command(name = "penna", version = env!("PENNA_VERSION"), about = "Fan control for Linux, made to be understood.")]
struct Cli {
    /// Read sysfs from here instead of /sys (a `penna capture`). Turns NVML off: a fixture has no live GPU.
    #[arg(long, global = true, value_name = "DIR")]
    sysfs_root: Option<PathBuf>,
    /// Leave NVIDIA GPUs out.
    #[arg(long, global = true)]
    no_nvml: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Live table of temperatures and fans: from pennad when it answers, otherwise read directly from the hardware.
    Watch {
        /// Print one frame and exit.
        #[arg(long)]
        once: bool,
        /// One JSON event per tick: pennad's `tick`, or `hardware` when reading directly.
        #[arg(long)]
        json: bool,
        /// Seconds between frames when reading directly (pennad sets its own tick).
        #[arg(long, default_value_t = 1.0, value_parser = parse_interval)]
        interval: f64,
        /// Read the hardware directly even when pennad answers.
        #[arg(long)]
        direct: bool,
    },
    /// What pennad is doing now: the same table as `watch --once`, or its `state` as JSON.
    State {
        #[arg(long)]
        json: bool,
    },
    /// Switch to a profile (by id or name), and keep it.
    Profile { name: String },
    /// Hold a fan (or `all`) at a duty, or put it back on its curve with `auto`.
    Set {
        /// A control's name or key, or `all`.
        control: String,
        /// A duty from 0 to 100 (`60` or `60%`), or `auto`.
        value: String,
        /// Only for this long: `30s`, `2m`.
        #[arg(long = "for", value_name = "DURATION")]
        duration: Option<String>,
    },
    /// Every fan at 100 % until turned off.
    FullSpeed { state: OnOff },
    /// Try a contested or faulted fan again.
    Retry { control: String },
    /// Give a fan a curve in the active profile, and keep it.
    Adopt { control: String, curve: String },
    /// Give a fan back to its firmware in the active profile, and keep it.
    Release { control: String },
    /// Rename a sensor, tach or control (by key or current name).
    Label { key: String, label: String },
    /// Open the curve studio in the Omarchy shell.
    Open,
    /// Stream what changes: takes, hand-backs, failsafes, emergencies.
    Events {
        #[arg(long)]
        json: bool,
    },
    /// One JSON line for a Waybar custom module.
    Waybar {
        /// Keep running and print a line whenever it changes (Waybar's continuous mode).
        #[arg(long)]
        follow: bool,
        /// Show this sensor's temperature beside the glyph (key or name).
        #[arg(long)]
        sensor: Option<String>,
    },
    /// Snapshot /sys/class/hwmon and the PCI display devices into DIR, read-only, for test fixtures.
    Capture { dir: PathBuf },
    /// Put every fan in held.json back exactly as Penna found it. Works without the daemon: it is the unit's ExecStopPost.
    Restore {
        #[arg(long, value_name = "FILE", default_value = "/run/penna/held.json")]
        from: PathBuf,
    },
    /// Every fan back to its BIOS or driver, and kept there: the Defaults profile. Through pennad when it runs; without it
    /// (as root), restores held.json and saves Defaults so a restarted pennad leaves the fans alone.
    Reset {
        #[arg(long, value_name = "FILE", default_value = "/run/penna/held.json")]
        held: PathBuf,
        #[arg(long, value_name = "FILE", default_value = "/etc/penna/config.toml")]
        config: PathBuf,
    },
    /// What stands between this machine and Penna running its fans, each with what to do. Read-only. Exits 1 when something
    /// needs attention.
    Doctor {
        #[arg(long)]
        json: bool,
    },
    /// Check a configuration without applying it, against this machine.
    Check {
        #[arg(default_value = "/etc/penna/config.toml")]
        config: PathBuf,
        /// Pairing and calibration to check against.
        #[arg(long, value_name = "FILE")]
        measured: Option<PathBuf>,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum OnOff {
    On,
    Off,
}

/// The plugin whose overlay is the studio.
const PLUGIN: &str = "io.github.pushmotta.penna";

/// Summons the studio through the shell's IPC (§8.3).
fn open_studio() -> Result<()> {
    let status = std::process::Command::new("omarchy-shell").args(["shell", "summon", PLUGIN, "{}"]).status();
    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => bail!("omarchy-shell couldn't open the studio ({s}); is the Penna plugin enabled? (omarchy plugin enable {PLUGIN})"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            bail!("the studio needs the Omarchy shell, and omarchy-shell was not found")
        }
        Err(e) => bail!("couldn't run omarchy-shell: {e}"),
    }
}

fn parse_interval(s: &str) -> Result<f64, String> {
    let v: f64 = s.parse().map_err(|_| format!("not a number: {s}"))?;
    if (0.1..=60.0).contains(&v) { Ok(v) } else { Err("between 0.1 and 60 seconds".to_owned()) }
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("penna: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    let opts = Options {
        nvml: !cli.no_nvml && cli.sysfs_root.is_none(),
        sysfs_root: cli.sysfs_root.clone().unwrap_or_else(|| PathBuf::from("/sys")),
        ..Options::default()
    };
    match cli.command {
        Command::Watch { once, json, interval, direct } => {
            let daemon = if direct || cli.sysfs_root.is_some() { None } else { Client::find() };
            match daemon {
                Some(c) => watch_daemon(c, once, json),
                None => watch(&opts, once, json, interval, !direct && cli.sysfs_root.is_none()),
            }
        }
        Command::State { json } => {
            let mut c = Client::find().ok_or_else(client::not_running)?;
            let state = c.call(Op::State)?.ok_or_else(|| anyhow!("pennad sent no state"))?;
            if json {
                println!("{state}");
            } else {
                let tick: Tick = serde_json::from_value(state)?;
                print!("{}", render::tick_frame(&tick, style(), &c.via(), c.hello.mock));
            }
            Ok(())
        }
        Command::Profile { name } => say(Op::ProfileSet { name }),
        Command::Set { control, value, duration } => {
            let duty = match value.trim().trim_end_matches('%') {
                "auto" | "curve" => None,
                v => Some(v.trim().parse::<f64>().map_err(|_| anyhow!("{value:?} is not a duty: use a number from 0 to 100, or auto"))?),
            };
            let seconds = match duration {
                Some(d) => Some(penna_core::config::parse_seconds(&d).ok_or_else(|| anyhow!("{d:?} is not a duration: try 30s or 2m"))?),
                None => None,
            };
            let control = (!control.eq_ignore_ascii_case("all")).then_some(control);
            say(Op::Override { control, duty, seconds })
        }
        Command::FullSpeed { state } => say(Op::FullSpeed { on: matches!(state, OnOff::On) }),
        Command::Retry { control } => say(Op::Retry { control }),
        Command::Adopt { control, curve } => say(Op::Adopt { control, curve }),
        Command::Release { control } => say(Op::Release { control }),
        Command::Label { key, label } => say(Op::Label { key, label }),
        Command::Open => open_studio(),
        Command::Events { json } => events(json),
        Command::Waybar { follow, sensor } => bar(follow, sensor.as_deref()),
        Command::Restore { from } => restore(&from),
        Command::Reset { held, config } => reset(&held, &config),
        Command::Check { config, measured } => check(&opts, &config, measured.as_deref()),
        Command::Doctor { json } => {
            if doctor::run(json)? > 0 {
                std::io::stdout().flush()?;
                std::process::exit(1);
            }
            Ok(())
        }
        Command::Capture { dir } => {
            let report = penna_hw::capture::capture(&opts.sysfs_root, &dir).with_context(|| format!("capturing into {}", dir.display()))?;
            println!(
                "Captured {} hwmon chips, {} PCI display devices, {} files into {}",
                report.chips,
                report.pci_devices,
                report.files,
                dir.display()
            );
            for s in &report.skipped {
                println!("  skipped {s}");
            }
            Ok(())
        }
    }
}

fn style() -> render::Style {
    render::Style { color: std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none() }
}

/// A request that changes something, and the daemon's own sentence about what changed.
fn say(op: Op) -> Result<()> {
    let mut c = Client::find().ok_or_else(client::not_running)?;
    if !c.hello.can_write {
        bail!(c.hello.why.clone().unwrap_or_else(|| "pennad doesn't let this user change fans.".into()));
    }
    let result = c.call(op)?;
    match result.as_ref().and_then(|r| r.get("text")).and_then(Value::as_str) {
        Some(text) if !text.is_empty() => println!("{text}."),
        _ => println!("Done."),
    }
    Ok(())
}

fn watch_daemon(c: Client, once: bool, json: bool) -> Result<()> {
    let via = c.via();
    let mock = c.hello.mock;
    if once {
        let mut c = c;
        let state = c.call(Op::State)?.ok_or_else(|| anyhow!("pennad sent no state"))?;
        if json {
            let tick: Tick = serde_json::from_value(state)?;
            println!("{}", Event::Tick(tick).line());
        } else {
            let tick: Tick = serde_json::from_value(state)?;
            print!("{}", render::tick_frame(&tick, style(), &via, mock));
        }
        return Ok(());
    }
    let stdout = std::io::stdout();
    let live = !json && stdout.is_terminal();
    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = stop.clone();
        ctrlc::set_handler(move || stop.store(true, Ordering::SeqCst)).context("installing the Ctrl-C handler")?;
    }
    let mut out = stdout.lock();
    if live {
        write!(out, "\x1b[?1049h\x1b[?25l")?;
    }
    let result = c.subscribe(|line, ev| {
        if stop.load(Ordering::SeqCst) {
            return Ok(false);
        }
        let Some(Event::Tick(tick)) = ev else { return Ok(true) };
        let written = if json {
            writeln!(out, "{line}").map_err(anyhow::Error::from)
        } else {
            let frame = render::tick_frame(&tick, style(), &via, mock);
            if live { write_in_place(&mut out, &frame) } else { Ok(writeln!(out, "{frame}")?) }
        };
        match written.and_then(|_| Ok(out.flush()?)) {
            Ok(()) => Ok(true),
            Err(e) if broken_pipe(&e) => Ok(false),
            Err(e) => Err(e),
        }
    });
    if live {
        write!(out, "\x1b[?25h\x1b[?1049l")?;
        out.flush()?;
    }
    if stop.load(Ordering::SeqCst) { Ok(()) } else { result }
}

fn broken_pipe(e: &anyhow::Error) -> bool {
    e.downcast_ref::<std::io::Error>().is_some_and(|io| io.kind() == std::io::ErrorKind::BrokenPipe)
}

fn events(json: bool) -> Result<()> {
    let c = Client::find().ok_or_else(client::not_running)?;
    let mut out = std::io::stdout().lock();
    let st = style();
    c.subscribe(|line, ev| {
        let Some(Event::Event(e)) = ev else { return Ok(true) };
        let text = if json {
            line.to_owned()
        } else {
            let level = match e.level {
                Level::Info => "     ",
                Level::Warn => "warn ",
                Level::Alert => "ALERT",
            };
            let body = format!("{level}  {}", e.text);
            format!("{}  {}", clock(e.t), if e.level == Level::Alert { st_urgent(st, &body) } else { body })
        };
        match writeln!(out, "{text}").and_then(|_| out.flush()) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(false),
            Err(e) => Err(e.into()),
        }
    })
}

fn st_urgent(style: render::Style, s: &str) -> String {
    if style.color { format!("\x1b[31m{s}\x1b[0m") } else { s.to_owned() }
}

/// HH:MM:SS in local time.
fn clock(t: f64) -> String {
    let secs = t as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // localtime_r only reads `secs` and writes `tm`, both ours.
    if unsafe { libc::localtime_r(&secs, &mut tm) }.is_null() {
        return format!("{t:.0}");
    }
    format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
}

fn bar(follow: bool, sensor: Option<&str>) -> Result<()> {
    let mut out = std::io::stdout().lock();
    let mut last = String::new();
    let mut emit = |line: &waybar::Line, out: &mut std::io::StdoutLock| -> Result<bool> {
        let s = serde_json::to_string(line)?;
        if s != last {
            match writeln!(out, "{s}").and_then(|_| out.flush()) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => return Ok(false),
                Err(e) => return Err(e.into()),
            }
            last = s;
        }
        Ok(true)
    };
    loop {
        match Client::find() {
            Some(mut c) if !follow => {
                let state = c.call(Op::State)?.ok_or_else(|| anyhow!("pennad sent no state"))?;
                emit(&waybar::line(&serde_json::from_value(state)?, sensor), &mut out)?;
                return Ok(());
            }
            Some(c) => {
                let mut going = true;
                let _ = c.subscribe(|_, ev| {
                    if let Some(Event::Tick(t)) = ev {
                        going = emit(&waybar::line(&t, sensor), &mut out)?;
                    }
                    Ok(going)
                });
                if !going {
                    return Ok(());
                }
            }
            None => {
                if !emit(&waybar::offline(), &mut out)? || !follow {
                    return Ok(());
                }
            }
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

fn restore(from: &std::path::Path) -> Result<()> {
    if !from.exists() {
        println!("Nothing to restore: {} does not exist, so Penna holds no fan.", from.display());
        return Ok(());
    }
    // Real fans need root to put back; a simulated desk's files belong to whoever made them.
    let held = penna_hw::held::load(from)?;
    let real = held.controls.iter().any(|e| match e {
        penna_core::held::HeldEntry::Nvml { .. } => true,
        penna_core::held::HeldEntry::Hwmon { enable, .. } => penna_hw::is_real_sysfs(enable),
        penna_core::held::HeldEntry::MockNvml { .. } => false,
    });
    // SAFETY: geteuid has no preconditions.
    if real && unsafe { libc::geteuid() } != 0 {
        bail!(
            "{} holds this machine's real fans, and putting them back needs root: sudo penna restore --from {}",
            from.display(),
            from.display()
        );
    }
    let r = penna_hw::held::restore_file(from)?;
    for d in &r.done {
        println!("restored {d}");
    }
    for g in &r.gone {
        println!("{}: its device is gone (a driver unloaded?), so it stays in {} until it returns", g.key(), from.display());
    }
    if r.failed.is_empty() && r.gone.is_empty() {
        println!("Every fan is back as Penna found it; removed {}.", from.display());
        Ok(())
    } else if r.failed.is_empty() {
        println!("Every fan that is here is back as Penna found it.");
        Ok(())
    } else {
        for f in &r.failed {
            eprintln!("could not restore {f}");
        }
        bail!("{} of {} controls could not be restored; {} is kept", r.failed.len(), r.failed.len() + r.done.len(), from.display())
    }
}

/// Defaults, with or without the daemon (§6.7).
fn reset(held: &std::path::Path, config: &std::path::Path) -> Result<()> {
    // SAFETY: geteuid has no preconditions.
    let root = unsafe { libc::geteuid() } == 0;
    if let Some(c) = Client::find()
        && (c.hello.can_write || !root)
    {
        drop(c);
        return say(Op::ProfileSet { name: penna_core::config::DEFAULTS.to_owned() });
    }
    if !root {
        bail!("pennad isn't running, and handing fans back without it needs root: sudo penna reset");
    }
    restore(held)?;
    let text = match std::fs::read_to_string(config) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            println!("No configuration at {}, so pennad starts in Defaults anyway.", config.display());
            return Ok(());
        }
        Err(e) => return Err(anyhow!(e).context(format!("reading {}", config.display()))),
    };
    let mut cfg =
        penna_core::config::Config::parse(&text).map_err(|e| anyhow!("{}: {e}; its profile was left as it was", config.display()))?;
    if cfg.profile == penna_core::config::DEFAULTS {
        println!("{} is already in Defaults.", config.display());
        return Ok(());
    }
    cfg.profile = penna_core::config::DEFAULTS.to_owned();
    penna_hw::held::write_atomically(config, cfg.saved_text().as_bytes()).with_context(|| format!("saving {}", config.display()))?;
    println!("Saved Defaults in {}: pennad leaves every fan with its BIOS or driver when it starts.", config.display());
    Ok(())
}

fn check(opts: &Options, path: &std::path::Path, measured: Option<&std::path::Path>) -> Result<()> {
    use penna_core::config::{Config, Measured};
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let config = Config::parse(&text).map_err(|e| anyhow!("{}: {e}", path.display()))?;
    let problems = config.validate();
    if !problems.is_empty() {
        for p in &problems {
            eprintln!("{}: {p}", path.display());
        }
        bail!("{} problem{} in {}", problems.len(), if problems.len() == 1 { "" } else { "s" }, path.display());
    }
    let measured = match measured {
        Some(p) => Measured::parse(&std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?)
            .map_err(|e| anyhow!("{}: {e}", p.display()))?,
        None => Measured::default(),
    };
    let machine = Machine::open(opts)?;
    let present: std::collections::BTreeSet<&penna_core::Key> =
        machine.inventory.devices.iter().flat_map(|d| d.sensors.iter().map(|s| &s.key).chain(d.controls.iter().map(|c| &c.key))).collect();
    let mut absent = Vec::new();
    for key in config.physical_sources().iter().chain(config.profiles.values().flat_map(|p| p.keys())) {
        if !present.contains(key) && !absent.contains(key) {
            absent.push(key.clone());
        }
    }
    match penna_core::plan::Plan::new(config.clone(), &machine.inventory, &measured, None) {
        Ok(plan) => {
            let adopted = plan.controls.keys().filter(|k| plan.assignment(&config.profile, k).is_some()).count();
            let n = |count: usize, one: &str| format!("{count} {one}{}", if count == 1 { "" } else { "s" });
            println!(
                "{} is valid: {}, {}, {}; the active profile {:?} adopts {}.",
                path.display(),
                n(config.curves.len(), "curve"),
                n(config.sensors.len(), "virtual sensor"),
                n(config.profiles.len(), "profile"),
                config.profile,
                n(adopted, "control")
            );
        }
        Err(problems) => {
            for p in &problems {
                eprintln!("{}: {p}", path.display());
            }
            bail!("{} does not fit this machine", path.display());
        }
    }
    for k in &absent {
        println!("  note: {k} is not on this machine; Penna keeps its settings and shows it as absent");
    }
    Ok(())
}

/// The hardware alone, as M0 read it: for when no daemon answers, or `--direct`.
fn watch(opts: &Options, once: bool, json: bool, interval: f64, say_why: bool) -> Result<()> {
    let mut machine = Machine::open(opts)?;
    let stdout = std::io::stdout();
    let tty = stdout.is_terminal();
    let style = render::Style { color: tty && std::env::var_os("NO_COLOR").is_none() };
    let live = !once && !json && tty;
    if say_why && !json {
        eprintln!("penna: pennad isn't running, so this reads the hardware directly and controls nothing.");
    }

    let stop = Arc::new(AtomicBool::new(false));
    if !once {
        let stop = stop.clone();
        ctrlc::set_handler(move || stop.store(true, Ordering::SeqCst)).context("installing the Ctrl-C handler")?;
    }
    let mut out = stdout.lock();
    if live {
        // Alternate screen, hidden cursor; both restored on the way out.
        write!(out, "\x1b[?1049h\x1b[?25l")?;
    }

    let period = Duration::from_secs_f64(interval);
    let start = Instant::now();
    let mut seq = 0u64;
    let result = loop {
        seq += 1;
        let readings = machine.read();
        let t = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
        let snapshot = Snapshot::new(seq, t, &machine.inventory, &readings);
        let written = if json {
            let line = Event::Hardware(snapshot).line();
            writeln!(out, "{line}").map_err(anyhow::Error::from)
        } else {
            let frame = render::frame(&snapshot, style, (!once).then_some(interval));
            if live { write_in_place(&mut out, &frame) } else { Ok(write!(out, "{frame}")?) }
        };
        if let Err(e) = written.and_then(|_| Ok(out.flush()?)) {
            // `penna watch --json | head` closing the pipe is how a reader says it has enough.
            break if broken_pipe(&e) { Ok(()) } else { Err(e) };
        }
        if once {
            break Ok(());
        }
        // Ticks are scheduled from the start, so a slow read does not drift the clock.
        let next = start + period * seq as u32;
        while Instant::now() < next {
            if stop.load(Ordering::SeqCst) {
                break;
            }
            std::thread::sleep((next - Instant::now()).min(Duration::from_millis(50)));
        }
        if stop.load(Ordering::SeqCst) {
            break Ok(());
        }
    };
    if live {
        write!(out, "\x1b[?25h\x1b[?1049l")?;
        out.flush()?;
    }
    result
}

/// Redraws over the previous frame line by line, clearing each line's tail, so nothing flashes between frames.
fn write_in_place(out: &mut impl Write, frame: &str) -> Result<()> {
    write!(out, "\x1b[H")?;
    for line in frame.lines() {
        write!(out, "{line}\x1b[K\r\n")?;
    }
    write!(out, "\x1b[J")?;
    Ok(())
}
