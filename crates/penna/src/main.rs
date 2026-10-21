//! `penna`: watch the machine's temperatures and fans, check a configuration, and put every fan back from `held.json`.
//! Everything `watch` prints comes from a [`penna_proto::Snapshot`], so the table and `--json` never disagree.
mod render;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use penna_hw::{Machine, Options};
use penna_proto::{Event, Snapshot};
use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Parser)]
#[command(name = "penna", version, about = "Fan control for Linux, made to be understood.")]
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
    /// Live table of every sensor, fan and control, once a second.
    Watch {
        /// Print one frame and exit.
        #[arg(long)]
        once: bool,
        /// One JSON event per tick (`{"ev":"tick",…}`), as the daemon will stream them.
        #[arg(long)]
        json: bool,
        /// Seconds between ticks.
        #[arg(long, default_value_t = 1.0, value_parser = parse_interval)]
        interval: f64,
    },
    /// Snapshot /sys/class/hwmon and the PCI display devices into DIR, read-only, for test fixtures.
    Capture { dir: PathBuf },
    /// Put every fan in held.json back exactly as Penna found it. Works without the daemon: it is the unit's ExecStopPost.
    Restore {
        #[arg(long, value_name = "FILE", default_value = "/run/penna/held.json")]
        from: PathBuf,
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

fn parse_interval(s: &str) -> Result<f64, String> {
    let v: f64 = s.parse().map_err(|_| format!("not a number: {s}"))?;
    if (0.1..=60.0).contains(&v) { Ok(v) } else { Err("between 0.1 and 60 seconds".to_owned()) }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let opts = Options {
        nvml: !cli.no_nvml && cli.sysfs_root.is_none(),
        sysfs_root: cli.sysfs_root.clone().unwrap_or_else(|| PathBuf::from("/sys")),
        ..Options::default()
    };
    match cli.command {
        Command::Watch { once, json, interval } => watch(&opts, once, json, interval),
        Command::Restore { from } => restore(&from),
        Command::Check { config, measured } => check(&opts, &config, measured.as_deref()),
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

fn restore(from: &std::path::Path) -> Result<()> {
    if !from.exists() {
        println!("Nothing to restore: {} does not exist, so Penna holds no fan.", from.display());
        return Ok(());
    }
    let r = penna_hw::held::restore_file(from)?;
    for d in &r.done {
        println!("restored {d}");
    }
    if r.failed.is_empty() {
        println!("Every fan is back as Penna found it; removed {}.", from.display());
        Ok(())
    } else {
        for f in &r.failed {
            eprintln!("could not restore {f}");
        }
        anyhow::bail!("{} of {} controls could not be restored; {} is kept", r.failed.len(), r.failed.len() + r.done.len(), from.display())
    }
}

fn check(opts: &Options, path: &std::path::Path, measured: Option<&std::path::Path>) -> Result<()> {
    use penna_core::config::{Config, Measured};
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let config = Config::parse(&text).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
    let problems = config.validate();
    if !problems.is_empty() {
        for p in &problems {
            eprintln!("{}: {p}", path.display());
        }
        anyhow::bail!("{} problem{} in {}", problems.len(), if problems.len() == 1 { "" } else { "s" }, path.display());
    }
    let measured = match measured {
        Some(p) => Measured::parse(&std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?)
            .map_err(|e| anyhow::anyhow!("{}: {e}", p.display()))?,
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
            anyhow::bail!("{} does not fit this machine", path.display());
        }
    }
    for k in &absent {
        println!("  note: {k} is not on this machine; Penna keeps its settings and shows it as absent");
    }
    Ok(())
}

fn watch(opts: &Options, once: bool, json: bool, interval: f64) -> Result<()> {
    let mut machine = Machine::open(opts)?;
    let stdout = std::io::stdout();
    let tty = stdout.is_terminal();
    let style = render::Style { color: tty && std::env::var_os("NO_COLOR").is_none() };
    let live = !once && !json && tty;

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
            let line = serde_json::to_string(&Event::Tick(snapshot))?;
            writeln!(out, "{line}").map_err(anyhow::Error::from)
        } else {
            let frame = render::frame(&snapshot, style, (!once).then_some(interval));
            if live { write_in_place(&mut out, &frame) } else { Ok(write!(out, "{frame}")?) }
        };
        if let Err(e) = written.and_then(|_| Ok(out.flush()?)) {
            // `penna watch --json | head` closing the pipe is how a reader says it has enough.
            let closed = e.downcast_ref::<std::io::Error>().is_some_and(|io| io.kind() == std::io::ErrorKind::BrokenPipe);
            break if closed { Ok(()) } else { Err(e) };
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
