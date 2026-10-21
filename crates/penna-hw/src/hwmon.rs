//! hwmon through sysfs (DESIGN.md §3.4). The sysfs root is injectable, so the same code reads `/sys` and a captured fixture.
use crate::{Backend, Readback};
use anyhow::{Context, Result, anyhow, bail};
use penna_core::drivers::{self, Class, Driver, Offers};
use penna_core::held::HeldEntry;
use penna_core::{
    ControlInfo, ControlKind, ControlMode, ControlReading, Device, Key, Limit, LimitKind, Readings, Sample, SensorInfo, Source, TachInfo,
    sanity,
};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

pub struct Hwmon {
    root: PathBuf,
    chips: Vec<Chip>,
    /// Writes allowed: only through [`Hwmon::controlling`].
    control: bool,
    /// `class/hwmon` as `discover` last saw it: each entry and where its link points (§3.6).
    listing: Vec<(String, PathBuf)>,
}

struct Chip {
    temps: Vec<(Key, PathBuf)>,
    fans: Vec<(Key, PathBuf)>,
    pwms: Vec<Pwm>,
}

struct Pwm {
    key: Key,
    kind: ControlKind,
    value: PathBuf,
    enable: PathBuf,
}

impl Pwm {
    /// What 100 % is written as, for a duty control.
    fn scale(&self) -> u32 {
        match self.kind {
            ControlKind::Duty { scale } => scale,
            _ => 255,
        }
    }
}

impl Hwmon {
    /// Read-only.
    pub fn new(sysfs_root: impl Into<PathBuf>) -> Hwmon {
        Hwmon { root: sysfs_root.into(), chips: Vec::new(), control: false, listing: Vec::new() }
    }

    /// Reads and writes. The root is made absolute so `held.json` names files anyone can find.
    pub fn controlling(sysfs_root: &Path) -> Result<Hwmon> {
        let root = fs::canonicalize(sysfs_root).with_context(|| format!("opening {}", sysfs_root.display()))?;
        Ok(Hwmon { root, chips: Vec::new(), control: true, listing: Vec::new() })
    }

    /// A control Penna may write: opened for control, and of a kind it drives (§3.5).
    fn pwm(&self, key: &Key) -> Result<&Pwm> {
        if !self.control {
            bail!("hwmon was opened read-only");
        }
        let p =
            self.chips.iter().flat_map(|c| &c.pwms).find(|p| &p.key == key).ok_or_else(|| anyhow!("{key} is not a PWM control here"))?;
        match p.kind.undrivable_reason() {
            Some(why) => bail!("{key}: {why}"),
            None => Ok(p),
        }
    }

    fn listing(&self) -> Vec<(String, PathBuf)> {
        let Ok(entries) = fs::read_dir(self.root.join("class/hwmon")) else { return Vec::new() };
        let mut out: Vec<(String, PathBuf)> = entries
            .filter_map(|e| e.ok())
            .map(|e| (e.file_name().to_string_lossy().into_owned(), fs::read_link(e.path()).unwrap_or_default()))
            .collect();
        out.sort();
        out
    }
}

fn read_number(path: &Path) -> Result<i64> {
    let s = fs::read_to_string(path).map_err(|e| anyhow!("reading {}: {}", path.display(), io_reason(&e)))?;
    s.trim().parse().map_err(|_| anyhow!("{} holds {:?}, not a number", path.display(), s.trim()))
}

pub(crate) fn write_number(path: &Path, v: i64) -> Result<()> {
    fs::write(path, format!("{v}\n")).map_err(|e| anyhow!("writing {v} to {}: {}", path.display(), io_reason(&e)))
}

impl Backend for Hwmon {
    fn name(&self) -> &'static str {
        "hwmon"
    }

    fn changed(&mut self) -> bool {
        self.listing() != self.listing
    }

    fn discover(&mut self) -> Result<Vec<Device>> {
        self.chips.clear();
        self.listing = self.listing();
        let class = self.root.join("class/hwmon");
        let mut dirs: Vec<(u32, PathBuf)> = match fs::read_dir(&class) {
            Ok(entries) => entries
                .filter_map(|e| e.ok())
                .filter_map(|e| Some((e.file_name().to_str()?.strip_prefix("hwmon")?.parse().ok()?, e.path())))
                .collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e.into()),
        };
        // Numeric order keeps the listing stable; the numbers themselves are never used (and may have gaps: no hwmon1 here).
        dirs.sort();

        let mut devices: Vec<Device> = Vec::new();
        for (_, link) in dirs {
            // Files are named through the device's real path, not `class/hwmon/hwmonN`: `held.json` then can't point at another
            // device that took the same number after a driver was reloaded.
            let dir = fs::canonicalize(&link).unwrap_or(link);
            let Some(name) = read_trimmed(&dir.join("name")) else { continue };
            let entry = drivers::lookup(&name);
            let device_dir = fs::canonicalize(dir.join("device")).ok();
            let mut location = device_dir.as_deref().map_or_else(|| "virtual".to_owned(), device_component);
            let base = location.clone();
            let mut n = 2;
            while devices.iter().any(|d| d.id == format!("hwmon/{name}/{location}")) {
                location = format!("{base}#{n}");
                n += 1;
            }

            let channels = channels(&dir);
            let mut chip = Chip { temps: Vec::new(), fans: Vec::new(), pwms: Vec::new() };
            let mut device = Device {
                id: format!("hwmon/{name}/{location}"),
                source: Source::Hwmon,
                driver: name.clone(),
                label: friendly_label(&name).to_owned(),
                location: location.clone(),
                model: device_dir.as_deref().and_then(|d| read_trimmed(&d.join("model"))),
                sensors: Vec::new(),
                tachs: Vec::new(),
                controls: Vec::new(),
            };

            for n in channels.temps {
                let key = Key::hwmon(&name, &location, &format!("temp{n}"));
                let mut sensor = SensorInfo {
                    key: key.clone(),
                    label: read_trimmed(&dir.join(format!("temp{n}_label"))).unwrap_or_else(|| format!("temp{n}")),
                    limits: Vec::new(),
                    ignored_limits: Vec::new(),
                    below_ambient: false,
                };
                for (kind, attr) in [(LimitKind::Max, "max"), (LimitKind::Crit, "crit")] {
                    if let Some(c) = read_int(&dir.join(format!("temp{n}_{attr}"))).map(sanity::millidegrees) {
                        match sanity::limit(c) {
                            Some(c) => sensor.limits.push(Limit { kind, celsius: c }),
                            None => sensor.ignored_limits.push(Limit { kind, celsius: c }),
                        }
                    }
                }
                let input = dir.join(format!("temp{n}_input"));
                if let Some(c) = read_int(&input).map(sanity::millidegrees) {
                    sensor.below_ambient = sanity::temperature(c).value().is_some_and(sanity::below_ambient);
                }
                device.sensors.push(sensor);
                chip.temps.push((key, input));
            }
            for n in channels.fans {
                let key = Key::hwmon(&name, &location, &format!("fan{n}"));
                let label = read_trimmed(&dir.join(format!("fan{n}_label"))).unwrap_or_else(|| format!("fan{n}"));
                device.tachs.push(TachInfo { key: key.clone(), label });
                chip.fans.push((key, dir.join(format!("fan{n}_input"))));
            }
            for n in channels.pwms {
                let key = Key::hwmon(&name, &location, &format!("pwm{n}"));
                let (value, enable) = (dir.join(format!("pwm{n}")), dir.join(format!("pwm{n}_enable")));
                let kind = control_kind(entry, &value, &enable);
                device.controls.push(ControlInfo { key: key.clone(), label: format!("pwm{n}"), kind });
                chip.pwms.push(Pwm { key, kind, value, enable });
            }

            devices.push(device);
            self.chips.push(chip);
        }
        Ok(devices)
    }

    fn read(&mut self, out: &mut Readings) -> Result<()> {
        for chip in &self.chips {
            for (key, path) in &chip.temps {
                let sample = match read_int_sample(path) {
                    Sample::Value(raw) => sanity::temperature(sanity::millidegrees(raw)),
                    Sample::Implausible(v) => Sample::Implausible(v),
                    Sample::Error(e) => Sample::Error(e),
                };
                out.temps.insert(key.clone(), sample);
            }
            for (key, path) in &chip.fans {
                let sample = match read_int_sample(path) {
                    Sample::Value(rpm) => u32::try_from(rpm).map_or(Sample::Implausible(rpm as f64), Sample::Value),
                    Sample::Implausible(v) => Sample::Implausible(v),
                    Sample::Error(e) => Sample::Error(e),
                };
                out.rpms.insert(key.clone(), sample);
            }
            for pwm in &chip.pwms {
                let scale = pwm.scale() as i64;
                let sample = match read_int_sample(&pwm.value) {
                    Sample::Value(raw) if (0..=scale).contains(&raw) => {
                        let mode = read_int(&pwm.enable).map_or(ControlMode::Unknown, ControlMode::from_pwm_enable);
                        Sample::Value(ControlReading { percent: raw as f64 * 100.0 / scale as f64, raw: Some(raw as u32), mode })
                    }
                    Sample::Value(raw) => Sample::Implausible(raw as f64),
                    Sample::Implausible(v) => Sample::Implausible(v),
                    Sample::Error(e) => Sample::Error(e),
                };
                out.controls.insert(pwm.key.clone(), sample);
            }
        }
        Ok(())
    }

    fn original(&mut self, key: &Key) -> Result<HeldEntry> {
        let p = self.pwm(key)?;
        let original_enable = read_number(&p.enable)?;
        let original_pwm = read_number(&p.value)?.clamp(0, p.scale() as i64) as u32;
        Ok(HeldEntry::Hwmon { key: key.clone(), enable: p.enable.clone(), original_enable, pwm: p.value.clone(), original_pwm })
    }

    /// Manual mode, then the duty the firmware was running written straight back: no jump (§6.1).
    /// Manual first: several drivers refuse a duty outside manual mode (`EBUSY`, `EROFS`, §3.5).
    fn take(&mut self, key: &Key) -> Result<f64> {
        let p = self.pwm(key)?;
        let scale = p.scale() as i64;
        let raw = read_number(&p.value)?.clamp(0, scale);
        write_number(&p.enable, 1)?;
        write_number(&p.value, raw)?;
        Ok(raw as f64 * 100.0 / scale as f64)
    }

    fn write(&mut self, key: &Key, raw: u32) -> Result<Readback> {
        let p = self.pwm(key)?;
        write_number(&p.value, raw.min(p.scale()) as i64)?;
        readback(p)
    }

    fn reassert(&mut self, key: &Key, raw: u32) -> Result<Readback> {
        let p = self.pwm(key)?;
        write_number(&p.enable, 1)?;
        write_number(&p.value, raw.min(p.scale()) as i64)?;
        readback(p)
    }

    fn relocate(&self, entry: &HeldEntry) -> Option<HeldEntry> {
        let HeldEntry::Hwmon { key, original_enable, original_pwm, .. } = entry else { return None };
        let p = self.chips.iter().flat_map(|c| &c.pwms).find(|p| &p.key == key)?;
        Some(HeldEntry::Hwmon {
            key: key.clone(),
            enable: p.enable.clone(),
            original_enable: *original_enable,
            pwm: p.value.clone(),
            original_pwm: *original_pwm,
        })
    }
}

/// What a channel offers: its driver's kind (§3.5), or the generic hwmon ABI for a driver Penna doesn't know. A duty control
/// needs a writable `pwmN` and a `pwmN_enable`, or nobody can tell who is in charge of it.
fn control_kind(entry: Option<&Driver>, value: &Path, enable: &Path) -> ControlKind {
    match entry.map_or(Offers::Duty { scale: 255 }, |d| d.offers) {
        Offers::Duty { scale } if writable(value) && enable.exists() => ControlKind::Duty { scale },
        Offers::Duty { .. } | Offers::ReadOnly => ControlKind::ReadOnly,
        Offers::Levels { steps } => ControlKind::Levels { steps },
        Offers::FirmwareCurve { points } => ControlKind::FirmwareCurve { points },
        Offers::Modes => ControlKind::Modes,
    }
}

/// sysfs marks attributes a driver won't take writes for as read-only (0444), for root as well.
fn writable(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o222 != 0)
}

fn readback(p: &Pwm) -> Result<Readback> {
    let raw = read_number(&p.value).ok().map(|v| v.clamp(0, p.scale() as i64) as u32);
    let mode = read_number(&p.enable).map_or(ControlMode::Unknown, ControlMode::from_pwm_enable);
    Ok(Readback { raw, mode })
}

/// The `<device>` part of a key: the final component of the `device` link's real path. Class devices such as
/// `…/0000:04:00.0/nvme/nvme1` are numbered in enumeration order, so they take the bus device above them instead.
pub fn device_component(real: &Path) -> String {
    let parts: Vec<&str> = real.iter().filter_map(|c| c.to_str()).collect();
    if let [.., bus, class, last] = parts.as_slice() {
        let stem = last.trim_end_matches(|c: char| c.is_ascii_digit());
        if stem == *class && stem.len() < last.len() {
            return (*bus).to_owned();
        }
    }
    parts.last().map_or_else(|| "virtual".to_owned(), |s| (*s).to_owned())
}

pub fn friendly_label(driver: &str) -> &str {
    match driver {
        "k10temp" | "zenpower" => "AMD CPU",
        "coretemp" => "Intel CPU",
        "nvme" => "NVMe",
        "drivetemp" => "Drive",
        "acpitz" => "ACPI thermal zone",
        "asus_ec_sensors" | "gigabyte_wmi" => "Motherboard",
        d => match drivers::lookup(d) {
            Some(e) if !e.label.is_empty() => e.label,
            _ => d,
        },
    }
}

/// Whether an hwmon `name` belongs to a motherboard's fan chip.
pub fn is_motherboard(driver: &str) -> bool {
    drivers::lookup(driver).is_some_and(|e| e.class == Class::Motherboard)
}

#[derive(Default)]
struct Channels {
    temps: Vec<u32>,
    fans: Vec<u32>,
    pwms: Vec<u32>,
}

fn channels(dir: &Path) -> Channels {
    let mut ch = Channels::default();
    let Ok(entries) = fs::read_dir(dir) else { return ch };
    for entry in entries.filter_map(|e| e.ok()) {
        let file = entry.file_name();
        let Some(f) = file.to_str() else { continue };
        let index = |prefix: &str, suffix: &str| f.strip_prefix(prefix)?.strip_suffix(suffix)?.parse::<u32>().ok();
        if let Some(n) = index("temp", "_input") {
            ch.temps.push(n);
        } else if let Some(n) = index("fan", "_input") {
            ch.fans.push(n);
        } else if let Some(n) = index("pwm", "") {
            ch.pwms.push(n);
        }
    }
    ch.temps.sort();
    ch.fans.sort();
    ch.pwms.sort();
    ch
}

fn read_trimmed(path: &Path) -> Option<String> {
    let s = fs::read_to_string(path).ok()?;
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_owned())
}

fn read_int(path: &Path) -> Option<i64> {
    read_trimmed(path)?.parse().ok()
}

fn read_int_sample(path: &Path) -> Sample<i64> {
    match fs::read_to_string(path) {
        Ok(s) => s.trim().parse().map_or_else(|_| Sample::Error(format!("not a number: {:?}", s.trim())), Sample::Value),
        Err(e) => Sample::Error(io_reason(&e)),
    }
}

/// `EIO`, `ENODATA`, gone: the reason without the noise of `(os error 5)`.
fn io_reason(e: &std::io::Error) -> String {
    let s = e.to_string();
    s.split(" (os error").next().unwrap_or(&s).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_component_takes_the_last_component() {
        assert_eq!(device_component(Path::new("/sys/devices/pci0000:00/0000:00:18.3")), "0000:00:18.3");
        assert_eq!(device_component(Path::new("/sys/devices/virtual/thermal/thermal_zone0")), "thermal_zone0");
        assert_eq!(device_component(Path::new("/sys/devices/platform/it87.2608")), "it87.2608");
    }

    #[test]
    fn class_devices_take_the_bus_device_above_them() {
        let nvme = Path::new("/sys/devices/pci0000:00/0000:00:01.2/0000:02:00.0/0000:03:01.0/0000:04:00.0/nvme/nvme1");
        assert_eq!(device_component(nvme), "0000:04:00.0");
    }

    #[test]
    fn friendly_labels() {
        assert_eq!(friendly_label("k10temp"), "AMD CPU");
        assert_eq!(friendly_label("it87"), "Motherboard");
        assert_eq!(friendly_label("it8688"), "Motherboard");
        assert_eq!(friendly_label("thinkpad"), "ThinkPad");
        assert_eq!(friendly_label("mystery"), "mystery");
    }
}
