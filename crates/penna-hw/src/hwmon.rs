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
    /// The device's `power/runtime_status`: a read that fails while it says `suspended` is a device asleep, not a fault.
    runtime: Option<PathBuf>,
    temps: Vec<(Key, PathBuf)>,
    fans: Vec<(Key, PathBuf)>,
    pwms: Vec<Pwm>,
    targets: Vec<Target>,
    curves: Vec<Curve>,
}

impl Chip {
    fn asleep(&self) -> bool {
        self.runtime.as_deref().and_then(read_trimmed).is_some_and(|s| s == "suspended")
    }
}

/// A control set through `pwmN` and `pwmN_enable`: a duty, a level, or a mode (§3.5).
struct Pwm {
    key: Key,
    kind: ControlKind,
    value: PathBuf,
    enable: PathBuf,
    /// The mode to put back whatever `pwmN_enable` reads: the driver can't report it (`dell_smm`'s write-only enable) or
    /// reports a cache (`asus`) (§6.1).
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

/// A target speed in RPM (§3.5): `fanN_target`, or applesmc's `fanN_output` up to 7.2.
struct Target {
    key: Key,
    min: u32,
    max: u32,
    target: PathBuf,
    /// The file that switches it to manual, and the values for manual and automatic: applesmc's `pwmN_enable` (1, 2) or
    /// `fanN_manual` (1, 0). `lenovo_wmi_other` has none: a target of 0 is automatic.
    switch: Option<(PathBuf, i64, i64)>,
    /// Its tach, so taking it starts from the speed it turns.
    tach: PathBuf,
}

impl Target {
    fn percent(&self, rpm: u32) -> f64 {
        let span = self.max.saturating_sub(self.min).max(1) as f64;
        ((rpm as f64 - self.min as f64) / span * 100.0).clamp(0.0, 100.0)
    }

    fn mode(&self, target: i64) -> ControlMode {
        match &self.switch {
            Some((path, manual, _)) => match read_number(path) {
                Ok(v) if v == *manual => ControlMode::Manual,
                Ok(v) => u32::try_from(v).map_or(ControlMode::Unknown, ControlMode::Automatic),
                Err(_) => ControlMode::Unknown,
            },
            None if target == 0 => ControlMode::Automatic(0),
            None => ControlMode::Manual,
        }
    }
}

/// A curve the firmware runs itself (§3.5).
struct Curve {
    key: Key,
    files: CurveFiles,
    /// What Penna last wrote, as the firmware took it: an OverDrive curve that reads otherwise was put back by someone else.
    written: Option<Vec<[i64; 2]>>,
}

enum CurveFiles {
    /// amdgpu's `gpu_od/fan_ctrl/fan_curve`: points written as `index °C %`, then `c` to commit.
    Od(PathBuf),
    /// asus-wmi's `pwmN_auto_pointM_temp` (°C) and `_pwm` (0–255, kept as a percent), applied by writing 1 to `pwmN_enable`.
    Points { enable: PathBuf, temps: Vec<PathBuf>, pwms: Vec<PathBuf> },
}

/// One of a chip's controls.
enum Ctl<'a> {
    Pwm(&'a Pwm),
    Target(&'a Target),
    Curve(&'a Curve),
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
    fn ctl(&self, key: &Key) -> Result<Ctl<'_>> {
        if !self.control {
            bail!("hwmon was opened read-only");
        }
        for chip in &self.chips {
            if let Some(p) = chip.pwms.iter().find(|p| &p.key == key) {
                if let Some(why) = p.kind.undrivable_reason() {
                    bail!("{key}: {why}");
                }
                return Ok(Ctl::Pwm(p));
            }
            if let Some(t) = chip.targets.iter().find(|t| &t.key == key) {
                return Ok(Ctl::Target(t));
            }
            if let Some(c) = chip.curves.iter().find(|c| &c.key == key) {
                return Ok(Ctl::Curve(c));
            }
        }
        bail!("{key} is not a control here")
    }

    fn curve_mut(&mut self, key: &Key) -> Option<&mut Curve> {
        self.chips.iter_mut().flat_map(|c| &mut c.curves).find(|c| &c.key == key)
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

    /// Whether a module parameter allows writes (§3.5): a ThinkPad's `fan_control`.
    fn allowed(&self, entry: Option<&Driver>) -> bool {
        match entry.and_then(|d| d.allowed) {
            Some(p) => read_trimmed(&self.root.join(p.path)).is_some_and(|v| v == p.value),
            None => true,
        }
    }
}

fn read_number(path: &Path) -> Result<i64> {
    let s = fs::read_to_string(path).map_err(|e| anyhow!("reading {}: {}", path.display(), io_reason(&e)))?;
    s.trim().parse().map_err(|_| anyhow!("{} holds {:?}, not a number", path.display(), s.trim()))
}

pub(crate) fn write_number(path: &Path, v: i64) -> Result<()> {
    fs::write(path, format!("{v}\n")).map_err(|e| anyhow!("writing {v} to {}: {}", path.display(), io_reason(&e)))
}

/// An OverDrive fan curve as amdgpu prints it: `OD_FAN_CURVE:`, then `i: <°C>C <%>%` per point, then `OD_RANGE:` with
/// `FAN_CURVE(hotspot temp): <min>C <max>C` and `FAN_CURVE(fan speed): <min>% <max>%` (smu_v13_0_0_emit_clk_levels).
pub(crate) struct OdTable {
    pub points: Vec<[i64; 2]>,
    pub temp: Option<(i64, i64)>,
    pub speed: Option<(i64, i64)>,
}

pub(crate) fn od_read(path: &Path) -> Result<OdTable> {
    let text = fs::read_to_string(path).map_err(|e| anyhow!("reading {}: {}", path.display(), io_reason(&e)))?;
    let number = |s: &str| s.trim().trim_end_matches(['C', '%']).trim().parse::<i64>().ok();
    let pair = |rest: &str| {
        let mut it = rest.split_whitespace();
        Some((number(it.next()?)?, number(it.next()?)?))
    };
    let mut t = OdTable { points: Vec::new(), temp: None, speed: None };
    for line in text.lines() {
        if let Some((i, rest)) = line.split_once(':')
            && i.trim().parse::<usize>().is_ok()
            && let Some((c, p)) = pair(rest)
        {
            t.points.push([c, p]);
        } else if let Some(rest) = line.strip_prefix("FAN_CURVE(hotspot temp):") {
            t.temp = pair(rest);
        } else if let Some(rest) = line.strip_prefix("FAN_CURVE(fan speed):") {
            t.speed = pair(rest);
        }
    }
    if t.points.is_empty() {
        bail!("{} holds no fan curve", path.display());
    }
    Ok(t)
}

/// Writes an OverDrive curve point by point, `index °C %`, then commits it with `c` (amdgpu_pm.c, `amdgpu_od_set`). Each write
/// is one command; the file is opened to append, which sysfs ignores, so a test tree keeps every command for its simulator.
pub(crate) fn od_write(path: &Path, points: &[[i64; 2]]) -> Result<()> {
    use std::io::Write;
    let mut f = fs::OpenOptions::new().append(true).open(path).map_err(|e| anyhow!("opening {}: {}", path.display(), io_reason(&e)))?;
    for (i, [c, p]) in points.iter().enumerate() {
        f.write_all(format!("{i} {c} {p}\n").as_bytes())
            .map_err(|e| anyhow!("writing point {i} to {}: {}", path.display(), io_reason(&e)))?;
    }
    f.write_all(b"c\n").map_err(|e| anyhow!("committing {}: {}", path.display(), io_reason(&e)))
}

/// Puts an OverDrive curve back: the points as found when they are all within its range and the driver takes them; otherwise
/// `r`, which puts the boot table back and commits it. Returns whether it was the points.
pub(crate) fn od_restore(path: &Path, points: &[[i64; 2]]) -> Result<bool> {
    let t = od_read(path)?;
    let within = |v: i64, r: Option<(i64, i64)>| r.is_none_or(|(a, b)| (a..=b).contains(&v));
    if points.iter().all(|[c, p]| within(*c, t.temp) && within(*p, t.speed)) && od_write(path, points).is_ok() {
        return Ok(true);
    }
    use std::io::Write;
    let mut f = fs::OpenOptions::new().append(true).open(path).map_err(|e| anyhow!("opening {}: {}", path.display(), io_reason(&e)))?;
    f.write_all(b"r\n").map_err(|e| anyhow!("resetting {}: {}", path.display(), io_reason(&e)))?;
    Ok(false)
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
            let hwmon_dir = fs::canonicalize(&link).unwrap_or(link);
            let Some(name) = read_trimmed(&hwmon_dir.join("name")) else { continue };
            let entry = drivers::lookup(&name);
            let device_dir = fs::canonicalize(hwmon_dir.join("device")).ok();
            let mut location = device_dir.as_deref().map_or_else(|| "virtual".to_owned(), device_component);
            let base = location.clone();
            let mut n = 2;
            while devices.iter().any(|d| d.id == format!("hwmon/{name}/{location}")) {
                location = format!("{base}#{n}");
                n += 1;
            }

            // A driver registered the old way keeps its attributes on the device, not the hwmon directory: applesmc up to 7.2.
            let mut dir = hwmon_dir.clone();
            let mut channels = list_channels(&dir);
            if channels.is_empty()
                && let Some(d) = &device_dir
                && !list_channels(d).is_empty()
            {
                dir = d.clone();
                channels = list_channels(d);
            }
            let mut chip = Chip {
                runtime: device_dir.as_ref().map(|d| d.join("power/runtime_status")).filter(|p| p.exists()),
                temps: Vec::new(),
                fans: Vec::new(),
                pwms: Vec::new(),
                targets: Vec::new(),
                curves: Vec::new(),
            };
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

            for n in &channels.temps {
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
            for n in &channels.fans {
                let key = Key::hwmon(&name, &location, &format!("fan{n}"));
                let label = read_trimmed(&dir.join(format!("fan{n}_label"))).unwrap_or_else(|| format!("fan{n}"));
                device.tachs.push(TachInfo { key: key.clone(), label });
                chip.fans.push((key, dir.join(format!("fan{n}_input"))));
            }
            let allowed = self.allowed(entry);
            match entry.map(|d| d.offers) {
                // A target speed: the control is the fan itself, as an NVIDIA fan is.
                Some(Offers::Speed) => {
                    for n in &channels.fans {
                        if let Some(t) = target(&dir, &name, &location, *n).filter(|_| allowed) {
                            device.controls.push(ControlInfo {
                                key: t.key.clone(),
                                label: format!("fan{n}"),
                                kind: ControlKind::Speed { min: t.min, max: t.max },
                            });
                            chip.targets.push(t);
                        }
                    }
                }
                // A firmware curve per `pwmN_enable` with all its point files.
                Some(Offers::FirmwareCurve { points }) => {
                    for n in &channels.enables {
                        let enable = dir.join(format!("pwm{n}_enable"));
                        let temps: Vec<PathBuf> = (1..=points).map(|m| dir.join(format!("pwm{n}_auto_point{m}_temp"))).collect();
                        let pwms: Vec<PathBuf> = (1..=points).map(|m| dir.join(format!("pwm{n}_auto_point{m}_pwm"))).collect();
                        let whole = writable(&enable) && temps.iter().chain(&pwms).all(|p| writable(p)) && allowed;
                        let key = Key::hwmon(&name, &location, &format!("pwm{n}"));
                        let kind = if whole { ControlKind::FirmwareCurve { points } } else { ControlKind::ReadOnly };
                        device.controls.push(ControlInfo { key: key.clone(), label: format!("pwm{n}"), kind });
                        if whole {
                            chip.curves.push(Curve { key, files: CurveFiles::Points { enable, temps, pwms }, written: None });
                        }
                    }
                }
                _ => {
                    // A driver with modes may offer `pwmN_enable` alone: the mode is all there is to set (§3.5).
                    let mut pwms = channels.pwms.clone();
                    if entry.is_some_and(|d| matches!(d.offers, Offers::Modes { .. })) {
                        pwms.extend(channels.enables.iter().filter(|n| !channels.pwms.contains(n)));
                        pwms.sort();
                    }
                    for n in pwms {
                        let key = Key::hwmon(&name, &location, &format!("pwm{n}"));
                        let (value, enable) = (dir.join(format!("pwm{n}")), dir.join(format!("pwm{n}_enable")));
                        let mut kind = if allowed { control_kind(entry, &value, &enable) } else { ControlKind::ReadOnly };
                        // amdgpu's OverDrive curve, where pwm1 takes no duty (RDNA4) and the curve file takes writes.
                        let od = entry.and_then(|d| d.curve).map(|c| hwmon_dir.join(c));
                        if kind == ControlKind::ReadOnly
                            && n == 1
                            && let Some(od) = od.filter(|p| writable(p))
                            && let Ok(table) = od_read(&od)
                        {
                            let points = table.points.len() as u32;
                            kind = ControlKind::FirmwareCurve { points };
                            chip.curves.push(Curve { key: key.clone(), files: CurveFiles::Od(od), written: None });
                            device.controls.push(ControlInfo { key, label: format!("pwm{n}"), kind });
                            continue;
                        }
                        device.controls.push(ControlInfo { key: key.clone(), label: format!("pwm{n}"), kind });
                        let watchdog =
                            entry.and_then(|d| d.watchdog).map(|w| (self.root.join(w.path), w.seconds)).filter(|(p, _)| p.exists());
                        chip.pwms.push(Pwm { key, kind, value, enable, restore: entry.and_then(|d| d.restore), watchdog });
                    }
                }
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
                    // amdgpu refuses every read of a runtime-suspended GPU (EPERM) rather than wake it (§4.2).
                    Sample::Error(_) if chip.asleep() => Sample::Asleep,
                    Sample::Error(e) => Sample::Error(e),
                    Sample::Asleep => Sample::Asleep,
                };
                out.temps.insert(key.clone(), sample);
            }
            for (key, path) in &chip.fans {
                let sample = match read_int_sample(path) {
                    Sample::Value(rpm) => u32::try_from(rpm).map_or(Sample::Implausible(rpm as f64), Sample::Value),
                    Sample::Implausible(v) => Sample::Implausible(v),
                    Sample::Error(e) => Sample::Error(e),
                    Sample::Asleep => Sample::Asleep,
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
                        Sample::Asleep => Sample::Asleep,
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
                    Sample::Asleep => Sample::Asleep,
                };
                out.controls.insert(pwm.key.clone(), sample);
            }
            for t in &chip.targets {
                let sample = match read_int_sample(&t.target) {
                    Sample::Value(v) if (0..=u32::MAX as i64).contains(&v) => {
                        Sample::Value(ControlReading { percent: t.percent(v as u32), raw: Some(v as u32), mode: t.mode(v) })
                    }
                    Sample::Value(v) => Sample::Implausible(v as f64),
                    Sample::Implausible(v) => Sample::Implausible(v),
                    Sample::Error(e) => Sample::Error(e),
                    Sample::Asleep => Sample::Asleep,
                };
                out.controls.insert(t.key.clone(), sample);
            }
            for c in &chip.curves {
                out.controls.insert(c.key.clone(), curve_reading(c));
            }
        }
        Ok(())
    }

    /// The way back, as found. A driver that can't say which mode it is in (`dell_smm`'s write-only enable), or reports a cache
    /// (`asus`), gets the mode its table entry names. A stepped control's duty isn't put back: in automatic it reads the level a
    /// ThinkPad would take in manual, and `dell_smm` none at all (§3.5).
    fn original(&mut self, key: &Key) -> Result<HeldEntry> {
        match self.ctl(key)? {
            Ctl::Pwm(p) => {
                let original_enable = match (p.restore, read_number(&p.enable)) {
                    (Some(mode), _) => mode,
                    (None, Ok(v)) => v,
                    (None, Err(e)) => return Err(e),
                };
                let original_pwm = match p.kind {
                    ControlKind::Modes | ControlKind::Levels { .. } => None,
                    _ => Some(read_number(&p.value)?.clamp(0, p.scale() as i64) as u32),
                };
                let mut also = Vec::new();
                if let Some((path, _)) = &p.watchdog {
                    let value = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?.trim().to_owned();
                    also.push(Also { path: path.clone(), value });
                }
                Ok(HeldEntry::Hwmon {
                    key: key.clone(),
                    enable: p.enable.clone(),
                    original_enable,
                    pwm: p.value.clone(),
                    original_pwm,
                    also,
                })
            }
            Ctl::Target(t) => {
                let (switch, original_switch) = match &t.switch {
                    Some((path, ..)) => (Some(path.clone()), Some(read_number(path)?)),
                    None => (None, None),
                };
                Ok(HeldEntry::Target {
                    key: key.clone(),
                    target: t.target.clone(),
                    original: read_number(&t.target)?,
                    switch,
                    original_switch,
                })
            }
            Ctl::Curve(c) => match &c.files {
                CurveFiles::Od(path) => Ok(HeldEntry::OdCurve { key: key.clone(), path: path.clone(), original: od_read(path)?.points }),
                CurveFiles::Points { enable, temps, pwms } => {
                    let mut points = Vec::new();
                    for path in temps.iter().chain(pwms) {
                        let value = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?.trim().to_owned();
                        points.push(Also { path: path.clone(), value });
                    }
                    Ok(HeldEntry::PointCurve { key: key.clone(), enable: enable.clone(), original_enable: read_number(enable)?, points })
                }
            },
        }
    }

    /// Manual mode, then the duty the firmware was running written straight back: no jump (§6.1).
    /// Manual first: several drivers refuse a duty outside manual mode (`EBUSY`, `EROFS`, §3.5), and a ThinkPad ignores one. A
    /// duty that can't be read starts at full speed, and the step rate brings it down. A watchdog is armed last, once the fan
    /// runs what Penna wrote. A target speed starts at the speed the fan turns; a firmware curve and a mode are taken as they are,
    /// and the first write sets them.
    fn take(&mut self, key: &Key) -> Result<f64> {
        match self.ctl(key)? {
            Ctl::Pwm(p) => {
                if p.kind == ControlKind::Modes {
                    // A mode that reads a cache is taken as automatic, which is what its driver left it in.
                    let full = p.restore.is_none() && read_number(&p.enable)? == 0;
                    return Ok(if full { 100.0 } else { 0.0 });
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
            Ctl::Target(t) => {
                let rpm = read_number(&t.tach).unwrap_or(t.max as i64).clamp(t.min as i64, t.max as i64);
                if let Some((path, manual, _)) = &t.switch {
                    write_number(path, *manual)?;
                }
                write_number(&t.target, rpm)?;
                Ok(t.percent(rpm as u32))
            }
            Ctl::Curve(_) => Ok(0.0),
        }
    }

    fn write(&mut self, key: &Key, raw: u32) -> Result<Readback> {
        match self.ctl(key)? {
            Ctl::Pwm(p) => {
                if p.kind == ControlKind::Modes {
                    write_number(&p.enable, if raw >= 1 { 0 } else { AUTOMATIC })?;
                } else {
                    write_number(&p.value, p.file_of(raw))?;
                }
                readback(p)
            }
            Ctl::Target(t) => {
                write_number(&t.target, raw as i64)?;
                let got = read_number(&t.target).ok();
                Ok(Readback { raw: got.map(|v| v.max(0) as u32), mode: t.mode(got.unwrap_or(0)) })
            }
            Ctl::Curve(_) => bail!("{key}: its firmware runs a curve, written whole"),
        }
    }

    fn reassert(&mut self, key: &Key, raw: u32) -> Result<Readback> {
        match self.ctl(key)? {
            Ctl::Pwm(p) if p.kind != ControlKind::Modes => {
                write_number(&p.enable, 1)?;
                write_number(&p.value, p.file_of(raw))?;
                readback(p)
            }
            Ctl::Target(t) => {
                if let Some((path, manual, _)) = &t.switch {
                    write_number(path, *manual)?;
                }
                self.write(key, raw)
            }
            _ => self.write(key, raw),
        }
    }

    /// A firmware curve, written whole (§3.5): OverDrive's points within its range, then a commit; ASUS's points as °C and
    /// 0–255, then 1 to its enable, which applies them.
    fn write_curve(&mut self, key: &Key, points: &[[f64; 2]]) -> Result<()> {
        let written = match self.ctl(key)? {
            Ctl::Curve(c) => match &c.files {
                CurveFiles::Od(path) => {
                    let t = od_read(path)?;
                    let (t0, t1) = t.temp.unwrap_or((0, 255));
                    let (s0, s1) = t.speed.unwrap_or((0, 100));
                    let pts: Vec<[i64; 2]> =
                        points.iter().map(|p| [(p[0].round() as i64).clamp(t0, t1), (p[1].round() as i64).clamp(s0, s1)]).collect();
                    od_write(path, &pts)?;
                    pts
                }
                CurveFiles::Points { enable, temps, pwms } => {
                    let pts: Vec<[i64; 2]> = points.iter().map(|p| [(p[0].round() as i64).clamp(0, 255), p[1].round() as i64]).collect();
                    for (i, [c, pct]) in pts.iter().enumerate() {
                        write_number(&temps[i], *c)?;
                        write_number(&pwms[i], (*pct as f64 * 255.0 / 100.0).round().clamp(0.0, 255.0) as i64)?;
                    }
                    write_number(enable, 1)?;
                    pts
                }
            },
            _ => bail!("{key} runs no firmware curve"),
        };
        if let Some(c) = self.curve_mut(key) {
            c.written = Some(written);
        }
        Ok(())
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

/// A speed control's files, when they are all there: a writable target, its minimum and maximum, and the switch to manual
/// where the driver has one. applesmc from 7.3 has `fanN_target` and `pwmN_enable`; up to 7.2, `fanN_output` and `fanN_manual`.
fn target(dir: &Path, name: &str, location: &str, n: u32) -> Option<Target> {
    let file = |f: &str| dir.join(format!("fan{n}_{f}"));
    let (target, switch) = if writable(&file("target")) {
        let enable = dir.join(format!("pwm{n}_enable"));
        (file("target"), writable(&enable).then_some((enable, 1, 2)))
    } else if writable(&file("output")) {
        (file("output"), writable(&file("manual")).then(|| (file("manual"), 1, 0)))
    } else {
        return None;
    };
    let (min, max) = (read_int(&file("min"))?, read_int(&file("max"))?);
    if min < 0 || max <= min {
        return None;
    }
    Some(Target {
        key: Key::hwmon(name, location, &format!("fan{n}")),
        min: min as u32,
        max: max as u32,
        target,
        switch,
        tach: file("input"),
    })
}

/// A firmware curve's reading: whether the firmware runs Penna's curve, as its mode (§3.5).
fn curve_reading(c: &Curve) -> Sample<ControlReading> {
    match &c.files {
        CurveFiles::Points { enable, .. } => match read_int_sample(enable) {
            // 1 runs the custom curve; 2 is the firmware's own, as a platform profile change leaves it.
            Sample::Value(v) => {
                let mode = if v == 1 { ControlMode::Manual } else { u32::try_from(v).map_or(ControlMode::Unknown, ControlMode::Automatic) };
                Sample::Value(ControlReading { percent: 0.0, raw: None, mode })
            }
            Sample::Implausible(v) => Sample::Implausible(v),
            Sample::Error(e) => Sample::Error(e),
            Sample::Asleep => Sample::Asleep,
        },
        CurveFiles::Od(path) => match od_read(path) {
            Ok(t) => {
                let mine = c.written.as_ref().is_none_or(|w| *w == t.points);
                Sample::Value(ControlReading {
                    percent: 0.0,
                    raw: None,
                    mode: if mine { ControlMode::Manual } else { ControlMode::Automatic(2) },
                })
            }
            Err(e) => Sample::Error(format!("{e:#}")),
        },
    }
}

/// What a channel offers: its driver's kind (§3.5), or the generic hwmon ABI for a driver Penna doesn't know. A duty or stepped
/// control needs a writable `pwmN` and a `pwmN_enable`, or nobody can tell who is in charge of it. A driver with modes may give
/// a channel with a `pwmN` of its own a duty instead (asus-wmi's AGFN CPU fan).
fn control_kind(entry: Option<&Driver>, value: &Path, enable: &Path) -> ControlKind {
    let settable = writable(value) && enable.exists();
    match entry.map_or(Offers::Duty { scale: 255 }, |d| d.offers) {
        Offers::Duty { scale } if settable => ControlKind::Duty { scale },
        Offers::Levels { steps } if settable && steps >= 2 => {
            ControlKind::Levels { steps, refresh: entry.is_some_and(|d| d.watchdog.is_some()) }
        }
        Offers::Modes { duty: true } if settable => ControlKind::Duty { scale: 255 },
        Offers::Modes { .. } if writable(enable) => ControlKind::Modes,
        Offers::Duty { .. } | Offers::Levels { .. } | Offers::Modes { .. } | Offers::Speed | Offers::ReadOnly => ControlKind::ReadOnly,
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

impl Channels {
    fn is_empty(&self) -> bool {
        self.temps.is_empty() && self.fans.is_empty() && self.pwms.is_empty() && self.enables.is_empty()
    }
}

fn list_channels(dir: &Path) -> Channels {
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
