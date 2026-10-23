//! hwmon through sysfs (DESIGN.md §3.4). The sysfs root is injectable, so the same code reads `/sys` and a captured fixture.
use crate::{Backend, Readback};
use anyhow::{Context, Result, anyhow, bail};
use penna_core::drivers::{self, Class, Driver, Offers};
use penna_core::held::{Also, HeldEntry};
use penna_core::model::level_duty;
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
    /// The mode to put back when `pwmN_enable` can't be read (§6.1).
    restore: Option<i64>,
    /// The firmware's watchdog file and the seconds Penna arms it with (§3.5).
    watchdog: Option<(PathBuf, u32)>,
}

impl Pwm {
    /// What 100 % is written as in `pwmN`: the scale of a duty control, 255 for the rest.
    fn scale(&self) -> u32 {
        match self.kind {
            ControlKind::Duty { scale } => scale,
            _ => 255,
        }
    }

    /// What `pwmN` is written for a raw value: the duty, or for a stepped control the value that stands for its level, which
    /// the driver reads back the same way (a ThinkPad's level × 255 / 7, Dell's 0, 128 and 255).
    fn file_of(&self, raw: u32) -> i64 {
        match self.kind {
            ControlKind::Levels { steps, .. } => (raw.min(steps - 1) as f64 * 255.0 / (steps - 1).max(1) as f64).round() as i64,
            _ => raw.min(self.scale()) as i64,
        }
    }

    /// The raw value `pwmN` stands for: the duty, or the nearest level.
    fn raw_of(&self, v: i64) -> u32 {
        let v = v.clamp(0, self.scale() as i64);
        match self.kind {
            ControlKind::Levels { steps, .. } => (v as f64 * (steps - 1).max(1) as f64 / 255.0).round() as u32,
            _ => v as u32,
        }
    }

    /// The duty a raw value is.
    fn percent(&self, raw: u32) -> f64 {
        match self.kind {
            ControlKind::Levels { steps, .. } => level_duty(steps, raw),
            _ => raw as f64 * 100.0 / self.scale() as f64,
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
            None if matches!(p.kind, ControlKind::FirmwareCurve { .. }) => bail!("{key}: its firmware curve isn't written through pwmN"),
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
            // A driver with modes may offer `pwmN_enable` alone: the mode is all there is to set (§3.5).
            let mut pwms = channels.pwms.clone();
            if entry.is_some_and(|d| d.offers == Offers::Modes) {
                pwms.extend(channels.enables.iter().filter(|n| !channels.pwms.contains(n)));
                pwms.sort();
            }
            for n in pwms {
                let key = Key::hwmon(&name, &location, &format!("pwm{n}"));
                let (value, enable) = (dir.join(format!("pwm{n}")), dir.join(format!("pwm{n}_enable")));
                let kind = control_kind(entry, &value, &enable);
                device.controls.push(ControlInfo { key: key.clone(), label: format!("pwm{n}"), kind });
                let watchdog = entry.and_then(|d| d.watchdog).map(|w| (self.root.join(w.path), w.seconds)).filter(|(p, _)| p.exists());
                chip.pwms.push(Pwm { key, kind, value, enable, restore: entry.and_then(|d| d.restore), watchdog });
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
                if pwm.kind == ControlKind::Modes {
                    // Its mode is its reading: full speed or not.
                    let sample = match read_int_sample(&pwm.enable) {
                        Sample::Value(v) => {
                            let raw = u32::from(v == 0);
                            Sample::Value(ControlReading {
                                percent: 100.0 * raw as f64,
                                raw: Some(raw),
                                mode: ControlMode::from_pwm_enable(v),
                            })
                        }
                        Sample::Implausible(v) => Sample::Implausible(v),
                        Sample::Error(e) => Sample::Error(e),
                    };
                    out.controls.insert(pwm.key.clone(), sample);
                    continue;
                }
                let scale = pwm.scale() as i64;
                let sample = match read_int_sample(&pwm.value) {
                    Sample::Value(v) if (0..=scale).contains(&v) => {
                        let mode = read_int(&pwm.enable).map_or(ControlMode::Unknown, ControlMode::from_pwm_enable);
                        let raw = pwm.raw_of(v);
                        Sample::Value(ControlReading { percent: pwm.percent(raw), raw: Some(raw), mode })
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

    /// The mode and duty as found. A driver that can't say which mode it is in (`dell_smm`'s write-only enable) gets the mode its
    /// table entry names, and one that can't read its duty in automatic (`dell_smm`, `ENODATA`) has none to put back (§3.5).
    fn original(&mut self, key: &Key) -> Result<HeldEntry> {
        let p = self.pwm(key)?;
        let original_enable = match (read_number(&p.enable), p.restore) {
            (Ok(v), _) => v,
            (Err(_), Some(mode)) => mode,
            (Err(e), None) => return Err(e),
        };
        let original_pwm = match (read_number(&p.value), p.restore) {
            // A mode is all a control with modes has: no duty goes back, and no manual mode is asked for on the way.
            _ if p.kind == ControlKind::Modes => None,
            (Ok(v), _) => Some(v.clamp(0, p.scale() as i64) as u32),
            (Err(_), Some(_)) => None,
            (Err(e), None) => return Err(e),
        };
        let mut also = Vec::new();
        if let Some((path, _)) = &p.watchdog {
            let value = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?.trim().to_owned();
            also.push(Also { path: path.clone(), value });
        }
        Ok(HeldEntry::Hwmon { key: key.clone(), enable: p.enable.clone(), original_enable, pwm: p.value.clone(), original_pwm, also })
    }

    /// Manual mode, then the duty the firmware was running written straight back: no jump (§6.1).
    /// Manual first: several drivers refuse a duty outside manual mode (`EBUSY`, `EROFS`, §3.5), and a ThinkPad ignores one. A
    /// duty that can't be read starts at full speed, and the step rate brings it down. A watchdog is armed last, once the fan
    /// runs what Penna wrote.
    fn take(&mut self, key: &Key) -> Result<f64> {
        let p = self.pwm(key)?;
        if p.kind == ControlKind::Modes {
            // Held in the mode it is in: the first write sets one.
            return Ok(if read_number(&p.enable)? == 0 { 100.0 } else { 0.0 });
        }
        let v = read_number(&p.value).map_or(p.scale() as i64, |v| v.clamp(0, p.scale() as i64));
        let raw = p.raw_of(v);
        write_number(&p.enable, 1)?;
        write_number(&p.value, p.file_of(raw))?;
        if let Some((path, seconds)) = &p.watchdog {
            write_number(path, *seconds as i64)?;
        }
        Ok(p.percent(raw))
    }

    fn write(&mut self, key: &Key, raw: u32) -> Result<Readback> {
        let p = self.pwm(key)?;
        if p.kind == ControlKind::Modes {
            write_number(&p.enable, if raw >= 1 { 0 } else { AUTOMATIC })?;
            return readback(p);
        }
        write_number(&p.value, p.file_of(raw))?;
        readback(p)
    }

    fn reassert(&mut self, key: &Key, raw: u32) -> Result<Readback> {
        let p = self.pwm(key)?;
        if p.kind == ControlKind::Modes {
            return self.write(key, raw);
        }
        write_number(&p.enable, 1)?;
        write_number(&p.value, p.file_of(raw))?;
        readback(p)
    }

    fn relocate(&self, entry: &HeldEntry) -> Option<HeldEntry> {
        let HeldEntry::Hwmon { key, original_enable, original_pwm, also, .. } = entry else { return None };
        let p = self.chips.iter().flat_map(|c| &c.pwms).find(|p| &p.key == key)?;
        Some(HeldEntry::Hwmon {
            key: key.clone(),
            enable: p.enable.clone(),
            original_enable: *original_enable,
            pwm: p.value.clone(),
            original_pwm: *original_pwm,
            also: also.clone(),
        })
    }
}

/// What a channel offers: its driver's kind (§3.5), or the generic hwmon ABI for a driver Penna doesn't know. A duty or stepped
/// control needs a writable `pwmN` and a `pwmN_enable`, or nobody can tell who is in charge of it: a ThinkPad's are read-only
/// without `fan_control=1`.
fn control_kind(entry: Option<&Driver>, value: &Path, enable: &Path) -> ControlKind {
    let settable = writable(value) && enable.exists();
    match entry.map_or(Offers::Duty { scale: 255 }, |d| d.offers) {
        Offers::Duty { scale } if settable => ControlKind::Duty { scale },
        Offers::Levels { steps } if settable && steps >= 2 => {
            ControlKind::Levels { steps, refresh: entry.is_some_and(|d| d.watchdog.is_some()) }
        }
        Offers::Modes if writable(enable) => ControlKind::Modes,
        Offers::Duty { .. } | Offers::Levels { .. } | Offers::Modes | Offers::ReadOnly => ControlKind::ReadOnly,
        Offers::FirmwareCurve { points } => ControlKind::FirmwareCurve { points },
    }
}

/// sysfs marks attributes a driver won't take writes for as read-only (0444), for root as well.
fn writable(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o222 != 0)
}

/// The `pwmN_enable` of a control with modes when it isn't at full speed: the firmware's automatic mode (`hp`, `asus`, §3.5).
const AUTOMATIC: i64 = 2;

fn readback(p: &Pwm) -> Result<Readback> {
    if p.kind == ControlKind::Modes {
        let v = read_number(&p.enable)?;
        return Ok(Readback { raw: Some(u32::from(v == 0)), mode: ControlMode::from_pwm_enable(v) });
    }
    let raw = read_number(&p.value).ok().map(|v| p.raw_of(v));
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
    enables: Vec<u32>,
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
        } else if let Some(n) = index("pwm", "_enable") {
            ch.enables.push(n);
        }
    }
    ch.temps.sort();
    ch.fans.sort();
    ch.pwms.sort();
    ch.enables.sort();
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
