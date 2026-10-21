//! hwmon through sysfs (DESIGN.md §3.4). The sysfs root is injectable, so the same code reads `/sys` and a captured fixture.
use crate::{Backend, Readback};
use anyhow::{Context, Result, anyhow, bail};
use penna_core::held::HeldEntry;
use penna_core::{
    ControlInfo, ControlKind, ControlMode, ControlReading, Device, Key, Limit, LimitKind, Readings, Sample, SensorInfo, Source, TachInfo,
    sanity,
};
use std::fs;
use std::path::{Path, PathBuf};

/// Drivers that put a motherboard's fan headers on hwmon.
pub const SUPER_IO_DRIVERS: &[&str] =
    &["it87", "nct6775", "nct6683", "nct6687", "f71882fg", "f71808e", "w83627ehf", "w83627hf", "w83795", "asus_ec_sensors", "dell_smm"];

pub struct Hwmon {
    root: PathBuf,
    chips: Vec<Chip>,
    /// Writes allowed: only through [`Hwmon::controlling`].
    control: bool,
}

struct Chip {
    temps: Vec<(Key, PathBuf)>,
    fans: Vec<(Key, PathBuf)>,
    pwms: Vec<Pwm>,
}

struct Pwm {
    key: Key,
    value: PathBuf,
    enable: PathBuf,
}

impl Hwmon {
    /// Read-only.
    pub fn new(sysfs_root: impl Into<PathBuf>) -> Hwmon {
        Hwmon { root: sysfs_root.into(), chips: Vec::new(), control: false }
    }

    /// Reads and writes. M1 refuses the real `/sys`; the root is made absolute so `held.json` names files anyone can find.
    pub fn controlling(sysfs_root: &Path) -> Result<Hwmon> {
        crate::refuse_real_sysfs(sysfs_root)?;
        let root = fs::canonicalize(sysfs_root).with_context(|| format!("opening {}", sysfs_root.display()))?;
        Ok(Hwmon { root, chips: Vec::new(), control: true })
    }

    fn pwm(&self, key: &Key) -> Result<&Pwm> {
        if !self.control {
            bail!("hwmon was opened read-only");
        }
        self.chips.iter().flat_map(|c| &c.pwms).find(|p| &p.key == key).ok_or_else(|| anyhow!("{key} is not a PWM control here"))
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

    fn discover(&mut self) -> Result<Vec<Device>> {
        self.chips.clear();
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
        for (_, dir) in dirs {
            let Some(name) = read_trimmed(&dir.join("name")) else { continue };
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
                device.controls.push(ControlInfo { key: key.clone(), label: format!("pwm{n}"), kind: ControlKind::Pwm });
                chip.pwms.push(Pwm { key, value: dir.join(format!("pwm{n}")), enable: dir.join(format!("pwm{n}_enable")) });
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
                let sample = match read_int_sample(&pwm.value) {
                    Sample::Value(raw @ 0..=255) => {
                        let mode = read_int(&pwm.enable).map_or(ControlMode::Unknown, ControlMode::from_pwm_enable);
                        Sample::Value(ControlReading { percent: raw as f64 * 100.0 / 255.0, raw: Some(raw as u32), mode })
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
        let original_pwm = read_number(&p.value)?.clamp(0, 255) as u32;
        Ok(HeldEntry::Hwmon { key: key.clone(), enable: p.enable.clone(), original_enable, pwm: p.value.clone(), original_pwm })
    }

    /// Manual mode, then the duty the firmware was running written straight back: no jump (§6.1).
    fn take(&mut self, key: &Key) -> Result<f64> {
        let p = self.pwm(key)?;
        let raw = read_number(&p.value)?.clamp(0, 255);
        write_number(&p.enable, 1)?;
        write_number(&p.value, raw)?;
        Ok(raw as f64 * 100.0 / 255.0)
    }

    fn write(&mut self, key: &Key, raw: u32) -> Result<Readback> {
        let p = self.pwm(key)?;
        write_number(&p.value, raw.min(255) as i64)?;
        readback(p)
    }

    fn reassert(&mut self, key: &Key, raw: u32) -> Result<Readback> {
        let p = self.pwm(key)?;
        write_number(&p.enable, 1)?;
        write_number(&p.value, raw.min(255) as i64)?;
        readback(p)
    }
}

fn readback(p: &Pwm) -> Result<Readback> {
    let raw = read_number(&p.value).ok().map(|v| v.clamp(0, 255) as u32);
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
        "amdgpu" => "AMD GPU",
        d if SUPER_IO_DRIVERS.contains(&d) => "Motherboard",
        d => d,
    }
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
        assert_eq!(friendly_label("mystery"), "mystery");
    }
}
