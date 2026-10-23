//! `penna doctor` (DESIGN.md §8.5): what stands between this machine and Penna running its fans, each finding with what to do.
//! Read-only, and it works as an ordinary user: [`gather`] reads the machine and asks every daemon that answers, and
//! [`findings`] turns that into sentences, so the wording is tested without any of the I/O.
use crate::client::Client;
use penna_core::boards::DriverAdvice;
use penna_core::drivers::{self, Class, Tier};
use penna_core::{Source, State, Unreachable};
use penna_hw::{Machine, Options};
use penna_proto::Tick;
use serde::Serialize;
use std::path::Path;
use std::process::Command;

/// Other programs that drive fans; with Penna they would fight over the same controls (§6.4).
pub const OTHER_DAEMONS: &[&str] = &["coolercontrold", "fan2go", "fancontrol", "thinkfan", "nbfc_service"];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Ok,
    Attention,
    Info,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Finding {
    pub area: &'static str,
    pub status: Status,
    pub text: String,
    /// What to do about it, when there is something to do.
    #[serde(rename = "do", skip_serializing_if = "Option::is_none")]
    pub todo: Option<String>,
}

/// A daemon that answered, and what it said.
pub struct Daemon {
    pub via: String,
    pub mock: bool,
    pub version: String,
    pub can_write: bool,
    pub why: Option<String>,
    pub tick: Option<Tick>,
}

pub enum KernelLog {
    /// The ACPI resource-conflict line.
    Conflict(String),
    NoConflict,
    Unreadable,
}

/// An hwmon device with fan controls.
pub struct FanDriver {
    /// hwmon's `name`: `it8688`.
    pub driver: String,
    /// The chip, when the board database names it: "IT8688E".
    pub model: Option<String>,
    pub class: Class,
    pub controls: usize,
    /// Controls Penna drives (§3.5).
    pub drivable: usize,
    /// Why the others can't be driven.
    pub why: Option<String>,
    pub tier: Tier,
    /// What its driver needs for writes that a default install may not have: `thinkpad_acpi fan_control=1` (§3.5).
    pub needs: Option<&'static str>,
}

pub struct Facts {
    pub daemons: Vec<Daemon>,
    pub production_socket: String,
    pub fan_drivers: Vec<FanDriver>,
    /// Vendor and model, from DMI.
    pub board: Option<(String, String)>,
    /// The machine's name for a person, past the firmware's placeholders (`penna_hw::dmi_machine`).
    pub machine: Option<String>,
    /// Fans the firmware runs through ACPI, only on or off, with no speed to read.
    pub acpi_fans: usize,
    /// The board database knows this board (§3.6).
    pub board_known: bool,
    /// What the board database says to load, when it says.
    pub advice: Option<DriverAdvice>,
    /// A laptop or other portable, from the DMI chassis type.
    pub portable: bool,
    /// `None` when it can't be told (no EFI).
    pub secure_boot: Option<bool>,
    pub kernel_log: KernelLog,
    /// The chips the stock it87 module knows, when the module is there.
    pub it87_chips: Option<Vec<String>>,
    pub other_daemons: Vec<String>,
    /// NVIDIA GPUs through NVML (label, fans), or why there is no NVML.
    pub nvidia: Result<Vec<(String, usize)>, String>,
    pub unreachable: Vec<Unreachable>,
}

/// Reads everything [`findings`] needs. Nothing here changes anything.
pub fn gather() -> Facts {
    let mut daemons = Vec::new();
    for path in penna_proto::socket_candidates() {
        if let Ok(mut c) = Client::connect(&path) {
            let tick = c.call(penna_proto::Op::State).ok().flatten().and_then(|v| serde_json::from_value::<Tick>(v).ok());
            daemons.push(Daemon {
                via: c.via(),
                mock: c.hello.mock,
                version: c.hello.daemon.clone(),
                can_write: c.hello.can_write,
                why: c.hello.why.clone(),
                tick,
            });
        }
    }
    let (fan_drivers, nvidia, unreachable, known) = match Machine::open(&Options::default()) {
        Ok(m) => {
            let known = m.board.clone();
            let inv = m.inventory;
            let fan_drivers = inv
                .devices
                .iter()
                .filter(|d| d.source == Source::Hwmon && (!d.controls.is_empty() || penna_hw::hwmon::is_motherboard(&d.driver)))
                .map(|d| FanDriver {
                    driver: d.driver.clone(),
                    model: d.model.clone(),
                    class: drivers::lookup(&d.driver).map_or(Class::Other, |e| e.class),
                    controls: d.controls.len(),
                    drivable: d.controls.iter().filter(|c| c.kind.drivable()).count(),
                    why: d.controls.iter().find_map(|c| c.kind.undrivable_reason()),
                    tier: drivers::tier(&d.driver),
                    needs: drivers::lookup(&d.driver).and_then(|e| e.needs),
                })
                .collect();
            let gpus: Vec<(String, usize)> =
                inv.devices.iter().filter(|d| d.source == Source::Nvml).map(|d| (d.label.clone(), d.controls.len())).collect();
            let nvml_note = inv.notes.iter().find(|n| n.starts_with("NVML")).cloned();
            let nvidia = match (gpus.is_empty(), nvml_note) {
                (true, Some(why)) => Err(why),
                _ => Ok(gpus),
            };
            (fan_drivers, nvidia, inv.unreachable, known)
        }
        Err(e) => (Vec::new(), Err(format!("reading the machine failed: {e:#}")), Vec::new(), None),
    };
    Facts {
        daemons,
        production_socket: penna_proto::PRODUCTION_SOCKET.to_owned(),
        fan_drivers,
        board: penna_hw::dmi_board(Path::new("/sys")),
        machine: penna_hw::dmi_machine(Path::new("/sys")),
        acpi_fans: penna_hw::acpi_fans(Path::new("/sys")),
        board_known: known.is_some(),
        advice: known.and_then(|b| b.driver),
        portable: portable(),
        secure_boot: secure_boot(),
        kernel_log: kernel_log(),
        it87_chips: it87_chips(),
        other_daemons: OTHER_DAEMONS.iter().filter(|d| active(d)).map(|d| (*d).to_owned()).collect(),
        nvidia,
        unreachable,
    }
}

/// DMI chassis types 8–14, 30–32: portable, laptop, notebook, hand-held, sub-notebook, tablet, convertible, detachable.
fn portable() -> bool {
    std::fs::read_to_string("/sys/class/dmi/id/chassis_type")
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .is_some_and(|t| matches!(t, 8..=14 | 30..=32))
}

/// The UEFI `SecureBoot` variable: four attribute bytes, then 1 when it is on.
fn secure_boot() -> Option<bool> {
    let dir = std::fs::read_dir("/sys/firmware/efi/efivars").ok()?;
    let var = dir.filter_map(|e| e.ok()).find(|e| e.file_name().to_string_lossy().starts_with("SecureBoot-"))?;
    std::fs::read(var.path()).ok()?.last().map(|b| *b == 1)
}

fn kernel_log() -> KernelLog {
    let Ok(out) = Command::new("journalctl").args(["-k", "-b", "--no-pager", "-q", "-o", "cat", "-g", "resource conflict"]).output() else {
        return KernelLog::Unreadable;
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(line) = stdout.lines().find(|l| l.to_ascii_lowercase().contains("resource conflict")) {
        return KernelLog::Conflict(line.trim().to_owned());
    }
    // Without the right group, journalctl shows nothing and says why on stderr.
    if stderr.contains("not seeing messages") || stderr.contains("No journal files") || stderr.contains("ermission") {
        KernelLog::Unreadable
    } else {
        KernelLog::NoConflict
    }
}

fn active(unit: &str) -> bool {
    Command::new("systemctl").args(["is-active", "--quiet", unit]).status().is_ok_and(|s| s.success())
}

fn it87_chips() -> Option<Vec<String>> {
    let out = Command::new("modinfo").args(["-F", "filename", "it87"]).output().ok()?;
    let file = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if file.is_empty() {
        return None;
    }
    let decompress = |prog: &str| Command::new(prog).args(["-dc", &file]).output().ok().map(|o| o.stdout);
    let bytes = if file.ends_with(".zst") {
        decompress("zstd")?
    } else if file.ends_with(".xz") {
        decompress("xz")?
    } else if file.ends_with(".gz") {
        decompress("gzip")?
    } else {
        std::fs::read(&file).ok()?
    };
    let chips = chip_names(&bytes);
    (!chips.is_empty()).then_some(chips)
}

/// ITE chip names in a module's strings: `IT8792E/IT8795E` and the like, each part `IT8` followed by digits and `E` or `F`.
fn chip_names(bytes: &[u8]) -> Vec<String> {
    let is_chip = |s: &str| {
        s.split('/').all(|p| {
            let digits = p.strip_prefix("IT").and_then(|r| r.strip_suffix(['E', 'F']));
            digits.is_some_and(|d| d.starts_with('8') && (3..=5).contains(&d.len()) && d.bytes().all(|b| b.is_ascii_digit()))
        })
    };
    let mut out: Vec<String> = bytes
        .split(|b| !b.is_ascii_graphic())
        .filter_map(|w| std::str::from_utf8(w).ok())
        .filter(|w| is_chip(w))
        .map(str::to_owned)
        .collect();
    out.sort();
    out.dedup();
    out
}

/// The findings, most important first within each area.
pub fn findings(f: &Facts) -> Vec<Finding> {
    let mut out = Vec::new();
    let push = |out: &mut Vec<Finding>, area, status, text: String, todo: Option<String>| out.push(Finding { area, status, text, todo });

    // The daemon.
    let production = f.daemons.iter().find(|d| !d.mock);
    match production {
        Some(d) if d.can_write => {
            push(&mut out, "daemon", Status::Ok, format!("pennad {} runs at {}, and you can change fans.", d.version, d.via), None)
        }
        Some(d) => push(
            &mut out,
            "daemon",
            Status::Attention,
            format!("pennad {} runs at {}, but you can't change fans from this account.", d.version, d.via),
            Some(d.why.clone().unwrap_or_else(|| "Ask pennad's administrator for membership of the penna group.".into())),
        ),
        None => push(
            &mut out,
            "daemon",
            Status::Attention,
            format!("No pennad is running at {}, so nothing manages this machine's fans.", f.production_socket),
            Some("Start it: sudo systemctl enable --now pennad".into()),
        ),
    }
    for d in f.daemons.iter().filter(|d| d.mock) {
        push(
            &mut out,
            "daemon",
            Status::Info,
            format!("A mock desk (pennad {}) answers at {}: it drives simulated fans only.", d.version, d.via),
            None,
        );
    }

    // Fans that need someone.
    for d in &f.daemons {
        let Some(tick) = &d.tick else { continue };
        let whose = if d.mock { "Mock desk: " } else { "" };
        let mut any = false;
        for e in &tick.emergency {
            any = true;
            push(
                &mut out,
                "fans",
                Status::Attention,
                format!(
                    "{whose}{} is at or over its {} °C limit: every fan is at 100 % until it cools to {} °C.",
                    e.label, e.limit, e.release
                ),
                Some("Find out what is heating it; Penna releases the fans on its own once it cools.".into()),
            );
        }
        for c in &tick.controls {
            let reason = c.reason.as_deref().map(|r| format!(": {r}")).unwrap_or_default();
            let (text, todo) = match c.state {
                State::Contested => (
                    format!("{whose}{} is contested{reason}.", c.label),
                    format!(
                        "Something else keeps setting it: check the BIOS fan setting for it, or another fan tool. Then: penna retry \"{}\"",
                        c.label
                    ),
                ),
                State::Fault => {
                    (format!("{whose}{} has a fault{reason}.", c.label), format!("When the cause is fixed: penna retry \"{}\"", c.label))
                }
                State::Failsafe => (
                    format!("{whose}{} is at its failsafe speed{reason}.", c.label),
                    "A sensor its curve reads failed; it goes back to its curve once the sensor reads again.".to_owned(),
                ),
                _ => continue,
            };
            any = true;
            push(&mut out, "fans", Status::Attention, text, Some(todo));
        }
        if !any {
            push(&mut out, "fans", Status::Ok, format!("{whose}No fan needs attention."), None);
        }
    }

    // Motherboard and laptop fans.
    let motherboard = f.fan_drivers.iter().any(|d| d.class == Class::Motherboard);
    if !motherboard && !f.portable {
        let board = f.machine.as_ref().map(|m| format!(" Its firmware calls it {}.", m.trim_end_matches('.'))).unwrap_or_default();
        let mut text = if f.acpi_fans > 0 {
            format!(
                "No motherboard fan driver is bound. Its firmware runs its fan{} through ACPI: {} fan device{}, each only on or off, \
                 with no speed to read. Penna shows the temperatures, and can neither read nor drive these.{board}",
                if f.acpi_fans == 1 { "" } else { "s" },
                f.acpi_fans,
                if f.acpi_fans == 1 { "" } else { "s" }
            )
        } else {
            format!("No motherboard fan driver is bound, so Penna can't see the motherboard's fans.{board}")
        };
        match &f.kernel_log {
            KernelLog::Conflict(line) => text.push_str(&format!(" The kernel log says: \"{line}\".")),
            KernelLog::NoConflict => {}
            KernelLog::Unreadable => text.push_str(
                " The kernel log isn't readable from this account, so the ACPI-conflict check was skipped (it needs the wheel, adm or \
                 systemd-journal group).",
            ),
        }
        let vendor = f.board.as_ref().map(|(v, _)| v.to_ascii_lowercase()).unwrap_or_default();
        let mut todo = if let Some(a) = &f.advice {
            let install = match (&a.package, a.loads_at_boot) {
                (Some(p), true) => format!("Install {p}: it loads at every boot from then on, and now with"),
                (Some(p), false) => format!("Install {p}, then load it:"),
                (None, _) => "Load it:".into(),
            };
            let options = a.options.as_ref().map(|o| format!(" {o}")).unwrap_or_default();
            let risk = a.risk.as_ref().map(|r| format!(" {r}")).unwrap_or_default();
            format!("From Penna's board database: {} {install} sudo modprobe {}{options}.{risk}", a.why, a.module)
        } else if vendor.contains("gigabyte") {
            let known = f
                .it87_chips
                .as_ref()
                .map(|c| format!(" The stock it87 module knows {}.", c.join(", ")))
                .unwrap_or_else(|| " The stock it87 module isn't installed.".to_owned());
            format!(
                "Gigabyte boards use ITE chips, driven by it87.{known} Newer ITE chips need the out-of-tree it87-dkms-git (AUR). On \
                 boards it knows, it loads at boot and ignores the ACPI resource conflict by itself; on others, load it with sudo \
                 modprobe it87 ignore_resource_conflict=1. Its README calls ignoring the conflict inherently risky: ACPI and the \
                 driver may use the chip at the same time, which can cause unexpected reboots."
            )
        } else if ["asus", "micro-star", "msi", "asrock"].iter().any(|v| vendor.contains(v)) {
            "Most ASUS, MSI and ASRock boards use a Nuvoton chip: try sudo modprobe nct6775 (or nct6683). sudo sensors-detect, from \
             lm_sensors, names the chip."
                .to_owned()
        } else if f.acpi_fans > 0 {
            "If the board has a fan chip as well, sudo sensors-detect, from lm_sensors, names it and the driver it needs.".to_owned()
        } else {
            "sudo sensors-detect, from lm_sensors, names the chip and the driver it needs.".to_owned()
        };
        let dkms =
            f.advice.as_ref().is_some_and(|a| a.package.as_deref().is_some_and(|p| p.contains("dkms"))) || vendor.contains("gigabyte");
        if dkms && f.secure_boot == Some(true) {
            todo.push_str(" Secure Boot is on: a DKMS module loads only once it is signed with a key you have enrolled.");
        }
        // A fan the firmware runs through ACPI is how many mini PCs are built, not something to fix.
        let status = if f.acpi_fans > 0 && f.advice.is_none() { Status::Info } else { Status::Attention };
        push(&mut out, "motherboard", status, text, Some(todo));
    }
    for d in &f.fan_drivers {
        let area = match d.class {
            Class::Motherboard => "motherboard",
            Class::Laptop => "laptop",
            Class::Gpu => "gpu",
            Class::Other => "fans",
        };
        let name = d.model.as_ref().map_or_else(|| d.driver.clone(), |m| format!("{m} ({})", d.driver));
        let plural = |n: usize| if n == 1 { "" } else { "s" };
        if let (0, Some(option)) = (d.drivable, d.needs) {
            // The driver is there and could drive them, with a module option it wasn't loaded with.
            let (module, value) = option.split_once(' ').unwrap_or((option, ""));
            push(
                &mut out,
                area,
                Status::Attention,
                format!("{name} is bound, but its fans can't be set: writes need the module option {option}."),
                Some(format!(
                    "echo 'options {module} {value}' | sudo tee /etc/modprobe.d/{module}.conf, then reboot (or reload {module}). With it                      the driver takes fan writes from any root program, Penna among them."
                )),
            );
            continue;
        }
        if d.drivable == 0 {
            let why = d.why.as_deref().map(|w| format!(" {w}")).unwrap_or_default();
            push(&mut out, area, Status::Info, format!("{name} is bound; Penna can't drive its fans.{why}"), None);
            continue;
        }
        let tier = match d.tier {
            Tier::Verified => " Its driver is Verified: Penna's hardware test has passed with it.".to_owned(),
            Tier::Captured => {
                " Its driver is Captured: it runs in the simulator from a real machine, but hasn't had the hardware test.".into()
            }
            Tier::Documented => " Its driver is Documented: nobody has run Penna's hardware test with it yet.".into(),
        };
        let count = if d.drivable == d.controls {
            format!("{} fan control{} Penna can drive", d.drivable, plural(d.drivable))
        } else {
            let why = d.why.as_deref().map(|w| format!(" ({})", w.trim_end_matches('.'))).unwrap_or_default();
            format!("{} fan controls, {} of which Penna can drive{why}", d.controls, d.drivable)
        };
        push(&mut out, area, Status::Ok, format!("{name} is bound, with {count}.{tier}"), None);
    }
    match (&f.board, f.board_known, motherboard) {
        (Some((v, n)), true, _) => {
            push(&mut out, "board", Status::Info, format!("{v} {n} is in Penna's board database: its headers and sensors are named."), None)
        }
        (Some((v, n)), false, true) => push(
            &mut out,
            "board",
            Status::Info,
            format!("{v} {n} isn't in Penna's board database yet, so its fans and sensors are named by channel."),
            Some("penna capture <dir> records what a new entry is made from (DESIGN.md §3.6).".into()),
        ),
        _ => {}
    }

    // Other fan programs.
    if f.other_daemons.is_empty() {
        push(&mut out, "other tools", Status::Ok, format!("No other fan daemon is running ({}).", OTHER_DAEMONS.join(", ")), None);
    }
    for d in &f.other_daemons {
        push(
            &mut out,
            "other tools",
            Status::Attention,
            format!("{d} is running and drives fans too: Penna would find the fans they share contested."),
            Some(format!("Choose one: sudo systemctl disable --now {d}")),
        );
    }

    // GPUs.
    match &f.nvidia {
        Ok(gpus) if !gpus.is_empty() => {
            let list: Vec<String> = gpus.iter().map(|(l, n)| format!("{l} ({n} fan{})", if *n == 1 { "" } else { "s" })).collect();
            push(&mut out, "nvidia", Status::Ok, format!("NVML sees {}.", list.join(", ")), None);
        }
        Ok(_) => push(&mut out, "nvidia", Status::Info, "NVML is here but sees no NVIDIA GPU.".into(), None),
        Err(why) => push(&mut out, "nvidia", Status::Info, format!("No NVIDIA fans: {why}."), None),
    }
    for u in &f.unreachable {
        push(&mut out, "gpu", Status::Info, format!("{} is {}: its fans belong to the VM, not to this machine.", u.label, u.reason), None);
    }
    out
}

/// Prints the findings and returns how many need attention.
pub fn run(json: bool) -> anyhow::Result<usize> {
    let all = findings(&gather());
    let attention = all.iter().filter(|f| f.status == Status::Attention).count();
    if json {
        println!("{}", serde_json::json!({ "attention": attention, "findings": all }));
        return Ok(attention);
    }
    println!("Penna doctor\n");
    for f in &all {
        let mark = match f.status {
            Status::Ok => "✓",
            Status::Attention => "!",
            Status::Info => "·",
        };
        println!("{mark} {:<12} {}", f.area, f.text);
        if let Some(todo) = &f.todo {
            println!("  {:<12} → {todo}", "");
        }
    }
    println!();
    println!(
        "{}",
        if attention == 0 {
            "Nothing needs attention.".to_owned()
        } else {
            format!("{attention} {} attention.", if attention == 1 { "thing needs" } else { "things need" })
        }
    );
    Ok(attention)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> Facts {
        Facts {
            daemons: vec![],
            production_socket: "/run/penna/penna.sock".into(),
            fan_drivers: vec![],
            board: Some(("Gigabyte Technology Co., Ltd.".into(), "X570 AORUS PRO".into())),
            machine: Some("Gigabyte Technology Co., Ltd. X570 AORUS PRO".into()),
            acpi_fans: 0,
            board_known: false,
            advice: None,
            portable: false,
            secure_boot: Some(false),
            kernel_log: KernelLog::Conflict("ACPI: OSL: Resource conflict; ACPI support missing from driver?".into()),
            it87_chips: Some(vec!["IT8689E".into(), "IT8792E/IT8795E".into()]),
            other_daemons: vec![],
            nvidia: Ok(vec![("NVIDIA GeForce RTX 3090".into(), 2)]),
            unreachable: vec![Unreachable {
                id: "pci/0000:0a:00.0".into(),
                label: "NVIDIA TU106 [GeForce RTX 2060 SUPER]".into(),
                reason: "passed through to a VM (vfio-pci)".into(),
            }],
        }
    }

    fn by_area<'a>(all: &'a [Finding], area: &str) -> Vec<&'a Finding> {
        all.iter().filter(|f| f.area == area).collect()
    }

    #[test]
    fn this_desk_today_names_the_board_the_conflict_and_the_risk() {
        let all = findings(&facts());
        let mb = by_area(&all, "motherboard");
        assert_eq!(mb[0].status, Status::Attention);
        assert!(mb[0].text.contains("X570 AORUS PRO") && mb[0].text.contains("Resource conflict"), "{}", mb[0].text);
        let todo = mb[0].todo.as_deref().unwrap();
        assert!(todo.contains("it87-dkms-git") && todo.contains("inherently risky") && todo.contains("IT8792E/IT8795E"), "{todo}");
        assert_eq!(by_area(&all, "daemon")[0].status, Status::Attention, "no production pennad");
        assert_eq!(by_area(&all, "nvidia")[0].text, "NVML sees NVIDIA GeForce RTX 3090 (2 fans).");
        assert!(by_area(&all, "gpu")[0].text.contains("2060 SUPER"));
        assert_eq!(by_area(&all, "other tools")[0].status, Status::Ok);
    }

    #[test]
    fn a_daemon_that_refuses_writes_says_why_and_fans_in_trouble_say_what_to_do() {
        let mut f = facts();
        let mut tick: Tick = serde_json::from_str(include_str!("../../penna-proto/tests/fixtures/tick.json")).unwrap();
        tick.emergency.clear();
        f.daemons.push(Daemon {
            via: "/run/penna/penna.sock".into(),
            mock: false,
            version: "0.6.0".into(),
            can_write: false,
            why: Some("Changing fans needs membership of the penna group: sudo usermod -aG penna $USER (it applies at once).".into()),
            tick: Some(tick),
        });
        let all = findings(&f);
        let daemon = by_area(&all, "daemon")[0];
        assert_eq!(daemon.status, Status::Attention);
        assert!(daemon.todo.as_deref().unwrap().contains("usermod -aG penna"));
        let fans = by_area(&all, "fans");
        assert!(fans.iter().any(|x| x.text.contains("contested") && x.todo.as_deref().unwrap().contains("penna retry")), "{fans:?}");
    }

    #[test]
    fn a_healthy_machine_needs_nothing() {
        let mut f = facts();
        f.fan_drivers = vec![FanDriver {
            driver: "it8688".into(),
            model: Some("IT8688E".into()),
            class: Class::Motherboard,
            controls: 5,
            drivable: 5,
            why: None,
            tier: drivers::tier("it8688"),
            needs: None,
        }];
        f.board_known = true;
        let mut tick: Tick = serde_json::from_str(include_str!("../../penna-proto/tests/fixtures/tick.json")).unwrap();
        tick.emergency.clear();
        tick.controls.retain(|c| matches!(c.state, State::Curve | State::Handback | State::Override));
        f.daemons.push(Daemon {
            via: "/run/penna/penna.sock".into(),
            mock: false,
            version: "0.6.0".into(),
            can_write: true,
            why: None,
            tick: Some(tick),
        });
        let all = findings(&f);
        assert!(all.iter().all(|x| x.status != Status::Attention), "{all:?}");
        assert_eq!(
            by_area(&all, "motherboard")[0].text,
            "IT8688E (it8688) is bound, with 5 fan controls Penna can drive. Its driver is Verified: Penna's hardware test has \
             passed with it."
        );
        assert_eq!(
            by_area(&all, "board")[0].text,
            "Gigabyte Technology Co., Ltd. X570 AORUS PRO is in Penna's board database: its headers and sensors are named."
        );
    }

    #[test]
    fn other_fan_daemons_and_unreadable_logs_are_named() {
        let mut f = facts();
        f.other_daemons = vec!["coolercontrold".into()];
        f.kernel_log = KernelLog::Unreadable;
        f.board = Some(("ASUSTeK COMPUTER INC.".into(), "ROG STRIX B550-F".into()));
        let all = findings(&f);
        assert!(by_area(&all, "other tools")[0].todo.as_deref().unwrap().contains("disable --now coolercontrold"));
        let mb = by_area(&all, "motherboard")[0];
        assert!(mb.text.contains("isn't readable") && mb.todo.as_deref().unwrap().contains("nct6775"));
    }

    #[test]
    fn the_board_database_names_the_driver_and_secure_boot_is_checked() {
        let mut f = facts();
        let board = penna_core::boards::Board::parse(include_str!("../../../data/boards/gigabyte-x570-aorus-pro.toml")).unwrap();
        f.board_known = true;
        f.advice = board.driver;
        f.secure_boot = Some(true);
        let all = findings(&f);
        let todo = by_area(&all, "motherboard")[0].todo.clone().unwrap();
        assert!(todo.starts_with("From Penna's board database: The stock it87 in Linux 7.2 doesn't know the IT8688E"), "{todo}");
        assert!(
            todo.contains("Install it87-dkms-git: it loads at every boot from then on, and now with sudo modprobe it87. "),
            "the fork knows this board, so no option is needed: {todo}"
        );
        assert!(todo.contains("inherently risky") && todo.ends_with("signed with a key you have enrolled."), "{todo}");
    }

    #[test]
    fn a_laptop_without_a_motherboard_chip_is_not_asked_for_one() {
        let mut f = facts();
        f.portable = true;
        f.fan_drivers = vec![FanDriver {
            driver: "legion_hwmon".into(),
            model: None,
            class: Class::Laptop,
            controls: 1,
            drivable: 0,
            why: penna_core::ControlKind::ReadOnly.undrivable_reason(),
            tier: Tier::Documented,
            needs: None,
        }];
        let all = findings(&f);
        assert!(by_area(&all, "motherboard").is_empty(), "{all:?}");
        let laptop = by_area(&all, "laptop")[0];
        assert_eq!(laptop.status, Status::Info);
        assert!(
            laptop.text == "legion_hwmon is bound; Penna can't drive its fans. Its driver offers no control Penna can use.",
            "{}",
            laptop.text
        );
    }

    #[test]
    fn a_thinkpad_loaded_without_fan_control_is_told_the_option() {
        let mut f = facts();
        f.portable = true;
        f.fan_drivers = vec![FanDriver {
            driver: "thinkpad".into(),
            model: None,
            class: Class::Laptop,
            controls: 1,
            drivable: 0,
            why: penna_core::ControlKind::ReadOnly.undrivable_reason(),
            tier: Tier::Documented,
            needs: drivers::lookup("thinkpad").and_then(|e| e.needs),
        }];
        let all = findings(&f);
        let laptop = by_area(&all, "laptop")[0];
        assert_eq!(laptop.status, Status::Attention);
        assert_eq!(laptop.text, "thinkpad is bound, but its fans can't be set: writes need the module option thinkpad_acpi fan_control=1.");
        assert!(
            laptop
                .todo
                .as_deref()
                .unwrap()
                .starts_with("echo 'options thinkpad_acpi fan_control=1' | sudo tee /etc/modprobe.d/thinkpad_acpi.conf")
        );
    }

    #[test]
    fn chip_names_come_out_of_the_module_strings() {
        let bytes = b"\0it8792\0IT8792E/IT8795E\0IT8689E\0IT87952E\0IT8705F\0not IT9999E\0ITE\0IT8\0";
        assert_eq!(chip_names(bytes), ["IT8689E", "IT8705F", "IT8792E/IT8795E", "IT87952E"]);
    }

    /// The AC8-N mini PC (test/fixtures/machines/ac8-n): placeholder DMI, five ACPI on/off fans, no fan chip. That is how the
    /// machine is built, not something to fix, and no sensors-detect will turn up a driver for it.
    #[test]
    fn a_mini_pc_whose_firmware_runs_its_fan_is_told_so_plainly() {
        let mut f = facts();
        f.board = Some(("To be filled by O.E.M.".into(), "To be filled by O.E.M.".into()));
        f.machine = Some("AC8-N".into());
        f.acpi_fans = 5;
        f.kernel_log = KernelLog::NoConflict;
        let all = findings(&f);
        let mb = by_area(&all, "motherboard");
        assert_eq!(mb[0].status, Status::Info);
        assert!(mb[0].text.contains("5 fan devices, each only on or off") && mb[0].text.contains("calls it AC8-N."), "{}", mb[0].text);
        assert!(!mb[0].text.contains("O.E.M"), "{}", mb[0].text);
        assert!(mb[0].todo.as_deref().unwrap().starts_with("If the board has a fan chip as well"));
    }
}
