//! The support tables in README.md and docs/SUPPORT.md are generated from what Penna actually knows: the driver table, the board
//! database, the chips each motherboard driver lists (data/chips.toml), the sensor names and the TjMax table. This test renders
//! them and fails when a document has drifted; `PENNA_BLESS=1 cargo test -p penna-hw --test support_doc` writes them.
use penna_core::boards::Board;
use penna_core::drivers::{Class, DRIVERS, Driver, Offers, Tier};
use penna_core::plan::{DEFAULT_EMERGENCY, TJMAX};
use penna_core::sanity::{AMBIENT_C, LIMIT_C, READING_C};
use penna_hw::hwmon::friendly_label;
use std::fs;
use std::path::{Path, PathBuf};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn tier(t: Tier) -> &'static str {
    match t {
        Tier::Verified => "Verified",
        Tier::Captured => "Captured",
        Tier::Documented => "Documented",
    }
}

/// What Penna does with a driver's fans, in a few words.
fn can(d: &Driver) -> String {
    let mut s = match d.offers {
        Offers::Duty { .. } => "set a duty".to_owned(),
        Offers::Levels { steps } => format!("pick one of {steps} levels"),
        Offers::Speed => "set a target speed".to_owned(),
        Offers::FirmwareCurve { points } => format!("write the firmware's {points}-point curve"),
        Offers::Modes { duty: true } => "automatic or full speed; a duty where the channel has one".to_owned(),
        Offers::Modes { duty: false } => "automatic or full speed".to_owned(),
        Offers::ReadOnly => "read only".to_owned(),
    };
    if d.curve.is_some() {
        s.push_str(", or write its OverDrive curve where `pwm1` is read-only (RDNA4)");
    }
    if d.watchdog.is_some() {
        s.push_str(", under the firmware's watchdog");
    }
    s
}

fn hardware(d: &Driver) -> String {
    if d.label.is_empty() {
        // The generic read-only entry: the drivers it covers say what they are.
        return "System76, Surface, Intel GPUs (`i915`, `xe`), MSI laptops".to_owned();
    }
    d.label.to_owned()
}

fn code_list(items: &[&str]) -> String {
    items.iter().map(|h| format!("`{h}`")).collect::<Vec<_>>().join(", ")
}

/// README's short table: drivers with the same hardware, kind and tier share a row, and NVML has its own.
fn summary() -> String {
    let mut rows: Vec<(String, String, String, &'static str)> = Vec::new();
    rows.push(("NVML".into(), "NVIDIA GPUs, NVIDIA's own driver (520 or newer)".into(), "set a duty".into(), "Verified"));
    for d in DRIVERS {
        let hw = if d.class == Class::Motherboard { "Motherboards (Super I/O chips)".to_owned() } else { hardware(d) };
        let (kind, t) = (can(d), tier(d.tier));
        match rows.iter_mut().find(|r| r.1 == hw && r.2 == kind && r.3 == t) {
            Some(r) => r.0.push_str(&format!(", `{}`", d.name)),
            None => rows.push((format!("`{}`", d.name), hw, kind, t)),
        }
    }
    let mut out = String::from("| Driver | Hardware | Penna can | Tier |\n|---|---|---|---|\n");
    for (name, hw, kind, t) in rows {
        let name = if name == "`read-only`" { "others".to_owned() } else { name };
        out.push_str(&format!("| {name} | {hw} | {kind} | {t} |\n"));
    }
    out
}

/// SUPPORT.md's full table: one row per driver, with what a write needs.
fn drivers() -> String {
    let mut out =
        String::from("| Driver | Registers as | Hardware | Penna can | Before Penna can write | Tier |\n|---|---|---|---|---|---|\n");
    out.push_str("| NVML | NVML | NVIDIA GPUs | set a duty | NVIDIA's driver 520 or newer; 565 for RPM | Verified |\n");
    for d in DRIVERS {
        let name = if d.name == "read-only" { "others".to_owned() } else { format!("`{}`", d.name) };
        let needs = d.needs.map_or("—".to_owned(), |n| format!("`{n}`"));
        out.push_str(&format!("| {name} | {} | {} | {} | {needs} | {} |\n", code_list(d.hwmon), hardware(d), can(d), tier(d.tier)));
    }
    out
}

fn chips() -> String {
    let text = fs::read_to_string(repo().join("data/chips.toml")).unwrap();
    let doc: toml::Value = toml::from_str(&text).unwrap();
    let mut out = String::new();
    for d in doc["driver"].as_array().unwrap() {
        let chips: Vec<&str> = d["chips"].as_array().unwrap().iter().map(|c| c.as_str().unwrap()).collect();
        out.push_str(&format!(
            "- **`{}`**: {}.\n  <sub>From {}.</sub>\n",
            d["name"].as_str().unwrap(),
            chips.join(", "),
            d["source"].as_str().unwrap()
        ));
    }
    out
}

fn boards() -> String {
    let mut paths: Vec<PathBuf> = fs::read_dir(repo().join("data/boards")).unwrap().map(|e| e.unwrap().path()).collect();
    paths.sort();
    let mut out = String::from("| Board | Chips | Headers, as the board names them | Driver |\n|---|---|---|---|\n");
    for p in paths.iter().filter(|p| p.extension().is_some_and(|x| x == "toml")) {
        let b = Board::parse(&fs::read_to_string(p).unwrap()).unwrap();
        // Fan chips only: an entry that just explains a sensor (a cold acpitz) isn't one.
        let chips: Vec<String> = b
            .chips
            .iter()
            .filter(|c| !c.fans.is_empty() || c.chip.is_some())
            .map(|c| c.chip.clone().unwrap_or_else(|| c.hwmon.clone()))
            .collect();
        let headers: Vec<String> = b.chips.iter().flat_map(|c| c.fans.values().map(|h| format!("`{h}`"))).collect();
        let driver = b.driver.as_ref().map_or("—".to_owned(), |d| match &d.package {
            Some(pkg) => format!("`{}` from `{pkg}`", d.module),
            None => format!("`{}`", d.module),
        });
        out.push_str(&format!("| {} {} | {} | {} | {driver} |\n", b.vendor, b.board, chips.join(", "), headers.join(", ")));
    }
    out
}

fn sensors() -> String {
    let mut out = String::from("| hwmon name | Shown as |\n|---|---|\n");
    for name in ["k10temp", "zenpower", "coretemp", "nvme", "drivetemp", "acpitz", "asus_ec_sensors", "gigabyte_wmi"] {
        out.push_str(&format!("| `{name}` | {} |\n", friendly_label(name)));
    }
    out.push_str(
        "\nA sensor that reports a critical or GPU-max limit of its own has that as its emergency limit: an NVIDIA GPU's \"GPU max\", \
         an amdgpu or NVMe sensor's critical temperature. A CPU sensor without one uses its model's maximum, from AMD's published \
         figures:\n\n| CPU | Limit |\n|---|---|\n",
    );
    for (model, t) in TJMAX {
        out.push_str(&format!("| {model} | {t} °C |\n"));
    }
    out.push_str(&format!(
        "\nAny other CPU gets {DEFAULT_EMERGENCY} °C. Any other sensor declares an emergency only when a curve reads it, at \
         {DEFAULT_EMERGENCY} °C: Super I/O chips report unconnected inputs as 127 °C. A reading outside {}…{} °C is a failed read, \
         and a limit outside {}…{} °C is ignored as a firmware placeholder. A sensor that reads below {AMBIENT_C} °C, such as a \
         firmware's fixed ACPI zone, is marked as below room temperature and set apart.\n",
        READING_C.start(),
        READING_C.end(),
        LIMIT_C.start(),
        LIMIT_C.end()
    ));
    out
}

/// Replaces what sits between `<!-- generated: NAME -->` and `<!-- end: NAME -->`.
fn fill(text: &str, name: &str, body: &str) -> String {
    let (open, close) = (format!("<!-- generated: {name} -->"), format!("<!-- end: {name} -->"));
    let start = text.find(&open).unwrap_or_else(|| panic!("no {open}")) + open.len();
    let end = text[start..].find(&close).unwrap_or_else(|| panic!("no {close}")) + start;
    format!("{}\n{}{}", &text[..start], body, &text[end..])
}

fn check(file: &str, sections: &[(&str, String)]) {
    let path = repo().join(file);
    let text = fs::read_to_string(&path).unwrap();
    let mut want = text.clone();
    for (name, body) in sections {
        want = fill(&want, name, body);
    }
    if std::env::var_os("PENNA_BLESS").is_some() {
        fs::write(&path, &want).unwrap();
        return;
    }
    assert!(text == want, "{file} has drifted from the code: run PENNA_BLESS=1 cargo test -p penna-hw --test support_doc");
}

#[test]
fn the_support_documents_match_the_code() {
    check("README.md", &[("drivers", summary())]);
    check("docs/SUPPORT.md", &[("drivers", drivers()), ("chips", chips()), ("boards", boards()), ("sensors", sensors())]);
}
