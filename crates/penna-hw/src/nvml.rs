//! NVIDIA GPUs through NVML (DESIGN.md §3.4, §4.5). `libnvidia-ml` is loaded at runtime: a machine without it has no NVML
//! backend, not an error.
//!
//! Control (M4) goes through [`FanApi`], the handful of NVML calls fan control makes, so the way a fan is taken, written,
//! handed back and restored is tested against a fake GPU and runs unchanged against the real one ([`RealNvml`]):
//! * taking a fan records its policy (and, for a manual one, the speed it was holding) and changes nothing;
//! * writing is `nvmlDeviceSetFanSpeed_v2`, clamped to the range the card accepts, which also makes the policy manual;
//! * handing back below the floor is `nvmlDeviceSetDefaultFanSpeed_v2`, the driver's own curve;
//! * restoring puts back exactly what was found: the driver's policy, or manual at the recorded speed.
//!
//! Reading a fan's RPM or percent costs about a millisecond of kernel time a call, and only people look at them: read-back
//! checks the policy (§6.4), which is read every pass. `pennad` reads them every [`SPEED_EVERY`], and every pass while setup
//! measures ([`Pace`]).
use crate::{Backend, Readback};
use anyhow::{Result, anyhow, bail};
use nvml_wrapper::enum_wrappers::device::{TemperatureSensor, TemperatureThreshold};
use nvml_wrapper::enums::device::FanControlPolicy;
use nvml_wrapper::error::{NvmlError, nvml_sym, nvml_try};
use nvml_wrapper::{Device as NvmlDevice, Nvml};
use penna_core::held::HeldEntry;
use penna_core::{
    ControlInfo, ControlKind, ControlMode, ControlReading, Device, Key, Limit, LimitKind, Readings, Sample, SensorInfo, Source, TachInfo,
    sanity,
};
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fmt;
use std::time::{Duration, Instant};

pub const DRIVER: &str = "driver";
pub const MANUAL: &str = "manual";

/// How often `pennad` reads an NVIDIA fan's RPM and percent outside setup (§3.4).
pub const SPEED_EVERY: Duration = Duration::from_secs(5);

/// A pass that wakes a little early still counts as due: ticks wake late by a different amount each time.
const PACE_SLACK: Duration = Duration::from_millis(200);

/// When the fans' speeds are read again.
#[derive(Debug, Default)]
struct Pace {
    /// `None`: every pass.
    every: Option<Duration>,
    last: Option<Instant>,
}

impl Pace {
    fn due(&mut self, now: Instant) -> bool {
        let due = match (self.every, self.last) {
            (Some(every), Some(last)) => now.saturating_duration_since(last) + PACE_SLACK >= every,
            _ => true,
        };
        if due {
            self.last = Some(now);
        }
        due
    }
}

/// What a fan's last speed read found.
struct Speed {
    rpm: Sample<u32>,
    percent: Result<f64, String>,
}

/// Who sets a fan's speed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
    /// The driver's temperature curve (`NVML_FAN_POLICY_TEMPERATURE_CONTINOUS_SW`).
    Driver,
    /// Whatever was last set (`NVML_FAN_POLICY_MANUAL`).
    Manual,
}

impl Policy {
    pub fn name(self) -> &'static str {
        match self {
            Policy::Driver => DRIVER,
            Policy::Manual => MANUAL,
        }
    }

    fn mode(self) -> ControlMode {
        match self {
            Policy::Driver => ControlMode::Driver,
            Policy::Manual => ControlMode::Manual,
        }
    }
}

/// An NVML call that failed, kept as what it means rather than as a code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    NoPermission,
    GpuLost,
    NotSupported,
    InvalidArgument,
    NotFound,
    DriverNotLoaded,
    /// The driver's NVML lacks a function fan control needs.
    Missing(String),
    Other(String),
}

impl From<NvmlError> for Failure {
    fn from(e: NvmlError) -> Failure {
        match e {
            NvmlError::NoPermission => Failure::NoPermission,
            NvmlError::GpuLost => Failure::GpuLost,
            NvmlError::NotSupported => Failure::NotSupported,
            NvmlError::InvalidArg => Failure::InvalidArgument,
            NvmlError::NotFound => Failure::NotFound,
            NvmlError::DriverNotLoaded | NvmlError::Uninitialized => Failure::DriverNotLoaded,
            NvmlError::FailedToLoadSymbol(s) => Failure::Missing(s),
            other => Failure::Other(other.to_string()),
        }
    }
}

impl fmt::Display for Failure {
    /// Sentences a person can act on. A lost GPU keeps the words "GPU is lost", which the control loop reports as a fault.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Failure::NoPermission => write!(f, "NVML refused: changing GPU fans needs root (run pennad as root)"),
            Failure::GpuLost => write!(f, "GPU is lost: it fell off the bus, or the driver lost it"),
            Failure::NotSupported => {
                write!(f, "this GPU or driver doesn't allow fan control through NVML (it needs a Maxwell or newer card)")
            }
            Failure::InvalidArgument => write!(f, "NVML refused the value as out of range for this fan"),
            Failure::NotFound => write!(f, "NVML doesn't know this GPU any more"),
            Failure::DriverNotLoaded => write!(f, "the NVIDIA driver is not loaded"),
            Failure::Missing(s) => write!(f, "this driver's NVML has no {s}; fan control needs a newer NVIDIA driver"),
            Failure::Other(s) => write!(f, "NVML: {s}"),
        }
    }
}

impl std::error::Error for Failure {}

/// The NVML calls fan control makes, addressed by GPU UUID and fan index.
pub trait FanApi {
    fn policy(&mut self, uuid: &str, fan: u32) -> Result<Policy, Failure>;
    /// The speed the fan is being driven to, in % (`nvmlDeviceGetTargetFanSpeed`).
    fn target(&mut self, uuid: &str, fan: u32) -> Result<u32, Failure>;
    /// The speed it runs at, in % of its maximum noise tolerance speed (`nvmlDeviceGetFanSpeed_v2`): not duty (§0).
    fn percent(&mut self, uuid: &str, fan: u32) -> Result<u32, Failure>;
    /// The manual speeds the card accepts.
    fn range(&mut self, uuid: &str) -> Result<(u32, u32), Failure>;
    fn set_speed(&mut self, uuid: &str, fan: u32, percent: u32) -> Result<(), Failure>;
    fn set_default(&mut self, uuid: &str, fan: u32) -> Result<(), Failure>;
    fn set_policy(&mut self, uuid: &str, fan: u32, policy: Policy) -> Result<(), Failure>;
}

/// `nvml/<uuid>/fan<i>` → (uuid, i).
pub fn parse_fan(key: &Key) -> Result<(String, u32)> {
    let uuid = key.device().strip_prefix("nvml/").ok_or_else(|| anyhow!("{key} is not an NVIDIA control"))?;
    let fan = key.channel().strip_prefix("fan").and_then(|n| n.parse().ok()).ok_or_else(|| anyhow!("{key} is not an NVIDIA fan"))?;
    Ok((uuid.to_owned(), fan))
}

/// The way back, read before Penna takes the fan (§6.1). Nothing on the card changes.
pub fn original(api: &mut impl FanApi, key: &Key) -> Result<HeldEntry> {
    let (uuid, fan) = parse_fan(key)?;
    let policy = api.policy(&uuid, fan).map_err(|e| anyhow!("{key}: reading its fan policy: {e}"))?;
    let original_speed = match policy {
        // Manual: the speed it holds is part of what it was. Without it there is no exact way back, so refuse to take it.
        Policy::Manual => Some(api.target(&uuid, fan).map_err(|e| anyhow!("{key}: reading the speed it holds: {e}"))?),
        Policy::Driver => None,
    };
    Ok(HeldEntry::Nvml { key: key.clone(), uuid, fan, original_policy: policy.name().to_owned(), original_speed })
}

/// Taking an NVIDIA fan changes nothing: it stays on its current policy until the first write (§6.1). Returns its speed.
pub fn take(api: &mut impl FanApi, key: &Key) -> Result<f64> {
    let (uuid, fan) = parse_fan(key)?;
    api.percent(&uuid, fan).map(f64::from).map_err(|e| anyhow!("{key}: {e}"))
}

/// Sets the speed, clamped to what the card accepts, and reads the policy back (§6.4).
pub fn write(api: &mut impl FanApi, key: &Key, percent: u32) -> Result<Readback> {
    let (uuid, fan) = parse_fan(key)?;
    let (min, max) = api.range(&uuid).map_err(|e| anyhow!("{key}: reading its speed range: {e}"))?;
    let speed = percent.clamp(min, max);
    api.set_speed(&uuid, fan, speed).map_err(|e| anyhow!("{key}: setting {speed} %: {e}"))?;
    let mode = api.policy(&uuid, fan).map_or(ControlMode::Unknown, Policy::mode);
    Ok(Readback { raw: None, mode })
}

/// Below the floor: the driver's curve runs the fan, which may stop it, and Penna still holds it (§4.5).
pub fn handback(api: &mut impl FanApi, key: &Key) -> Result<()> {
    let (uuid, fan) = parse_fan(key)?;
    api.set_default(&uuid, fan).map_err(|e| anyhow!("{key}: handing it to the driver's curve: {e}"))
}

/// Exactly what was found: the driver's curve, or manual at the speed it held.
pub fn restore(api: &mut impl FanApi, key: &Key, uuid: &str, fan: u32, policy: &str, speed: Option<u32>) -> Result<String> {
    match policy {
        DRIVER => {
            api.set_default(uuid, fan).map_err(|e| anyhow!("{key}: back to the driver's curve: {e}"))?;
            Ok(format!("{key}: the driver's curve"))
        }
        MANUAL => {
            let speed = speed.ok_or_else(|| anyhow!("{key}: held.json says manual but records no speed"))?;
            api.set_policy(uuid, fan, Policy::Manual).map_err(|e| anyhow!("{key}: back to manual: {e}"))?;
            let (min, max) = api.range(uuid).map_err(|e| anyhow!("{key}: reading its speed range: {e}"))?;
            api.set_speed(uuid, fan, speed.clamp(min, max)).map_err(|e| anyhow!("{key}: back to {speed} %: {e}"))?;
            Ok(format!("{key}: manual at {speed} %"))
        }
        other => bail!("{key}: held.json names a policy {other:?} Penna doesn't know"),
    }
}

/// Restores a held.json entry for a real NVIDIA fan, opening NVML for the purpose: what `penna restore`, the panic hook and
/// a starting daemon run.
pub fn restore_entry(entry: &HeldEntry) -> Result<String> {
    let HeldEntry::Nvml { key, uuid, fan, original_policy, original_speed } = entry else {
        bail!("{} is not an NVIDIA fan", entry.key());
    };
    let mut api = RealNvml::open().map_err(|e| anyhow!("{key}: {e}"))?;
    restore(&mut api, key, uuid, *fan, original_policy, *original_speed)
}

/// NVML itself.
pub struct RealNvml {
    nvml: Nvml,
}

impl RealNvml {
    /// `Err` carries why there is no NVML on this machine, in words.
    pub fn open() -> std::result::Result<RealNvml, String> {
        let mut last = String::new();
        for lib in ["libnvidia-ml.so.1", "libnvidia-ml.so"] {
            match Nvml::builder().lib_path(OsStr::new(lib)).init() {
                Ok(nvml) => return Ok(RealNvml { nvml }),
                Err(NvmlError::LibloadingError(_)) => last = "NVML: libnvidia-ml is not installed".to_owned(),
                Err(NvmlError::DriverNotLoaded) => return Err("NVML: the NVIDIA driver is not loaded".to_owned()),
                Err(e) => return Err(format!("NVML: {e}")),
            }
        }
        Err(last)
    }

    fn device(&self, uuid: &str) -> Result<NvmlDevice<'_>, Failure> {
        self.nvml.device_by_uuid(uuid).map_err(Failure::from)
    }
}

impl FanApi for RealNvml {
    fn policy(&mut self, uuid: &str, fan: u32) -> Result<Policy, Failure> {
        match self.device(uuid)?.fan_control_policy(fan)? {
            FanControlPolicy::TemperatureContinousSw => Ok(Policy::Driver),
            FanControlPolicy::Manual => Ok(Policy::Manual),
        }
    }

    fn target(&mut self, uuid: &str, fan: u32) -> Result<u32, Failure> {
        let device = self.device(uuid)?;
        // Not wrapped by nvml-wrapper 0.13: called through the library's own symbol table.
        let sym = nvml_sym(self.nvml.lib().nvmlDeviceGetTargetFanSpeed.as_ref())?;
        let mut speed: std::os::raw::c_uint = 0;
        // SAFETY: the handle comes from a live `Device` of this `Nvml`, and `speed` outlives the call.
        unsafe { nvml_try(sym(device.handle(), fan, &mut speed))? };
        Ok(speed)
    }

    fn percent(&mut self, uuid: &str, fan: u32) -> Result<u32, Failure> {
        Ok(self.device(uuid)?.fan_speed(fan)?)
    }

    fn range(&mut self, uuid: &str) -> Result<(u32, u32), Failure> {
        Ok(self.device(uuid)?.min_max_fan_speed()?)
    }

    fn set_speed(&mut self, uuid: &str, fan: u32, percent: u32) -> Result<(), Failure> {
        Ok(self.device(uuid)?.set_fan_speed(fan, percent)?)
    }

    fn set_default(&mut self, uuid: &str, fan: u32) -> Result<(), Failure> {
        Ok(self.device(uuid)?.set_default_fan_speed(fan)?)
    }

    fn set_policy(&mut self, uuid: &str, fan: u32, policy: Policy) -> Result<(), Failure> {
        let p = match policy {
            Policy::Driver => FanControlPolicy::TemperatureContinousSw,
            Policy::Manual => FanControlPolicy::Manual,
        };
        Ok(self.device(uuid)?.set_fan_control_policy(fan, p)?)
    }
}

pub struct NvmlBackend {
    api: RealNvml,
    gpus: Vec<Gpu>,
    pace: Pace,
    speeds: BTreeMap<Key, Speed>,
}

struct Gpu {
    index: u32,
    uuid: String,
    fans: u32,
}

impl NvmlBackend {
    /// `Err` carries why there is no NVML on this machine, in words for a note.
    pub fn open() -> std::result::Result<NvmlBackend, String> {
        Ok(NvmlBackend { api: RealNvml::open()?, gpus: Vec::new(), pace: Pace::default(), speeds: BTreeMap::new() })
    }
}

impl Backend for NvmlBackend {
    fn name(&self) -> &'static str {
        "nvml"
    }

    fn discover(&mut self) -> Result<Vec<Device>> {
        self.gpus.clear();
        self.speeds.clear();
        let nvml = &self.api.nvml;
        let mut devices = Vec::new();
        for index in 0..nvml.device_count()? {
            let gpu = nvml.device_by_index(index)?;
            let uuid = gpu.uuid()?;
            let fans = gpu.num_fans().unwrap_or(0);
            let id = format!("nvml/{uuid}");

            let mut temp = SensorInfo {
                key: Key::nvml_temp(&uuid),
                label: "GPU".to_owned(),
                limits: Vec::new(),
                ignored_limits: Vec::new(),
                below_ambient: false,
            };
            for (kind, threshold) in [
                (LimitKind::GpuMax, TemperatureThreshold::GpuMax),
                (LimitKind::Slowdown, TemperatureThreshold::Slowdown),
                (LimitKind::Shutdown, TemperatureThreshold::Shutdown),
                (LimitKind::Acoustic, TemperatureThreshold::AcousticCurr),
            ] {
                if let Ok(c) = gpu.temperature_threshold(threshold) {
                    match sanity::limit(c as f64) {
                        Some(c) => temp.limits.push(Limit { kind, celsius: c }),
                        None => temp.ignored_limits.push(Limit { kind, celsius: c as f64 }),
                    }
                }
            }
            if let Ok(c) = gpu.temperature(TemperatureSensor::Gpu) {
                temp.below_ambient = sanity::below_ambient(c as f64);
            }

            let range = gpu.min_max_fan_speed().ok();
            let mut device = Device {
                id,
                source: Source::Nvml,
                driver: "nvml".to_owned(),
                label: gpu.name().unwrap_or_else(|_| "NVIDIA GPU".to_owned()),
                location: gpu.pci_info().map(|p| short_bus_id(&p.bus_id)).unwrap_or_else(|_| format!("index {index}")),
                model: None,
                sensors: vec![temp],
                tachs: Vec::new(),
                controls: Vec::new(),
            };
            for fan in 0..fans {
                let key = Key::nvml_fan(&uuid, fan);
                let label = format!("Fan {}", fan + 1);
                device.tachs.push(TachInfo { key: key.clone(), label: label.clone() });
                if let Some((min, max)) = range {
                    device.controls.push(ControlInfo { key, label, kind: ControlKind::Nvml { min, max } });
                }
            }
            devices.push(device);
            self.gpus.push(Gpu { index, uuid, fans });
        }
        Ok(devices)
    }

    fn read(&mut self, out: &mut Readings) -> Result<()> {
        let fresh = self.pace.due(Instant::now());
        let mut lost = false;
        for gpu in &self.gpus {
            let device = match self.api.nvml.device_by_index(gpu.index) {
                Ok(d) => d,
                Err(e) => {
                    // The GPU fell off the bus or the driver went away: every value it owns is a failed read.
                    let reason = Failure::from(e).to_string();
                    out.temps.insert(Key::nvml_temp(&gpu.uuid), Sample::Error(reason.clone()));
                    for fan in 0..gpu.fans {
                        out.rpms.insert(Key::nvml_fan(&gpu.uuid, fan), Sample::Error(reason.clone()));
                        out.controls.insert(Key::nvml_fan(&gpu.uuid, fan), Sample::Error(reason.clone()));
                    }
                    lost = true;
                    continue;
                }
            };
            out.temps.insert(
                Key::nvml_temp(&gpu.uuid),
                device
                    .temperature(TemperatureSensor::Gpu)
                    .map_or_else(|e| Sample::Error(Failure::from(e).to_string()), |c| sanity::temperature(c as f64)),
            );
            for fan in 0..gpu.fans {
                let key = Key::nvml_fan(&gpu.uuid, fan);
                if fresh || !self.speeds.contains_key(&key) {
                    let rpm = device.fan_speed_rpm(fan).map_or_else(|e| Sample::Error(Failure::from(e).to_string()), Sample::Value);
                    let percent = device.fan_speed(fan).map(f64::from).map_err(|e| Failure::from(e).to_string());
                    self.speeds.insert(key.clone(), Speed { rpm, percent });
                }
                let speed = &self.speeds[&key];
                out.rpms.insert(key.clone(), speed.rpm.clone());
                out.controls.insert(key, fan_control(&device, fan, &speed.percent));
            }
        }
        if lost {
            // Whatever comes back is read whole on the next pass.
            self.pace.last = None;
        }
        Ok(())
    }

    fn pace(&mut self, every: Option<Duration>) {
        self.pace.every = every;
    }

    fn original(&mut self, key: &Key) -> Result<HeldEntry> {
        original(&mut self.api, key)
    }

    fn take(&mut self, key: &Key) -> Result<f64> {
        take(&mut self.api, key)
    }

    fn write(&mut self, key: &Key, raw: u32) -> Result<Readback> {
        write(&mut self.api, key, raw)
    }

    fn handback(&mut self, key: &Key) -> Result<()> {
        handback(&mut self.api, key)
    }

    fn release(&mut self, entry: &HeldEntry) -> Result<String> {
        match entry {
            HeldEntry::Nvml { key, uuid, fan, original_policy, original_speed } => {
                restore(&mut self.api, key, uuid, *fan, original_policy, *original_speed)
            }
            other => crate::held::restore(other),
        }
    }
}

/// The fan's percent as last read, and who sets its speed, read now.
fn fan_control(device: &NvmlDevice<'_>, fan: u32, percent: &Result<f64, String>) -> Sample<ControlReading> {
    let percent = match percent {
        Ok(p) => *p,
        Err(e) => return Sample::Error(e.clone()),
    };
    let mode = match device.fan_control_policy(fan) {
        Ok(FanControlPolicy::TemperatureContinousSw) => ControlMode::Driver,
        Ok(FanControlPolicy::Manual) => ControlMode::Manual,
        Err(_) => ControlMode::Unknown,
    };
    Sample::Value(ControlReading { percent, raw: None, mode })
}

/// NVML's `00000000:09:00.0` as sysfs writes it: `0000:09:00.0`.
fn short_bus_id(bus_id: &str) -> String {
    match bus_id.split_once(':') {
        Some((domain, rest)) if domain.len() > 4 => format!("{}:{rest}", &domain[domain.len() - 4..]).to_lowercase(),
        _ => bus_id.to_lowercase(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// A GPU with two fans, as the desk's 3090: 30–100 %, starting on the driver's curve. Every call is logged.
    struct Fake {
        policy: BTreeMap<u32, Policy>,
        target: BTreeMap<u32, u32>,
        lost: bool,
        root: bool,
        log: Vec<String>,
    }

    impl Fake {
        fn new() -> Fake {
            Fake {
                policy: BTreeMap::from([(0, Policy::Driver), (1, Policy::Driver)]),
                target: BTreeMap::from([(0, 30), (1, 30)]),
                lost: false,
                root: true,
                log: Vec::new(),
            }
        }

        fn check(&self, uuid: &str, fan: u32) -> Result<(), Failure> {
            if self.lost {
                return Err(Failure::GpuLost);
            }
            if uuid != "GPU-1" {
                return Err(Failure::NotFound);
            }
            if fan > 1 {
                return Err(Failure::InvalidArgument);
            }
            Ok(())
        }

        fn writes(&self) -> Result<(), Failure> {
            if self.root { Ok(()) } else { Err(Failure::NoPermission) }
        }
    }

    impl FanApi for Fake {
        fn policy(&mut self, uuid: &str, fan: u32) -> Result<Policy, Failure> {
            self.check(uuid, fan)?;
            Ok(self.policy[&fan])
        }
        fn target(&mut self, uuid: &str, fan: u32) -> Result<u32, Failure> {
            self.check(uuid, fan)?;
            Ok(self.target[&fan])
        }
        fn percent(&mut self, uuid: &str, fan: u32) -> Result<u32, Failure> {
            self.check(uuid, fan)?;
            Ok(53)
        }
        fn range(&mut self, uuid: &str) -> Result<(u32, u32), Failure> {
            self.check(uuid, 0)?;
            Ok((30, 100))
        }
        fn set_speed(&mut self, uuid: &str, fan: u32, percent: u32) -> Result<(), Failure> {
            self.check(uuid, fan)?;
            self.writes()?;
            if !(30..=100).contains(&percent) {
                return Err(Failure::InvalidArgument);
            }
            self.log.push(format!("speed {fan} {percent}"));
            self.policy.insert(fan, Policy::Manual);
            self.target.insert(fan, percent);
            Ok(())
        }
        fn set_default(&mut self, uuid: &str, fan: u32) -> Result<(), Failure> {
            self.check(uuid, fan)?;
            self.writes()?;
            self.log.push(format!("default {fan}"));
            self.policy.insert(fan, Policy::Driver);
            Ok(())
        }
        fn set_policy(&mut self, uuid: &str, fan: u32, policy: Policy) -> Result<(), Failure> {
            self.check(uuid, fan)?;
            self.writes()?;
            self.log.push(format!("policy {fan} {}", policy.name()));
            self.policy.insert(fan, policy);
            Ok(())
        }
    }

    fn fan(i: u32) -> Key {
        Key::nvml_fan("GPU-1", i)
    }

    fn back(fake: &mut Fake, entry: &HeldEntry) -> Result<String> {
        let HeldEntry::Nvml { key, uuid, fan, original_policy, original_speed } = entry else { unreachable!() };
        restore(fake, key, uuid, *fan, original_policy, *original_speed)
    }

    #[test]
    fn a_fan_speed_is_read_every_five_seconds_and_every_pass_while_setup_measures() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut pace = Pace::default();
        assert!(pace.due(at(0)) && pace.due(at(1000)), "unpaced, as `penna watch` reads: every pass");
        pace.every = Some(SPEED_EVERY);
        // Ticks a second apart, from 2 s to 12 s, the even ones 3 ms early and the odd ones 5 ms late.
        let tick = |s: u64| if s % 2 == 0 { at(s * 1000 - 3) } else { at(s * 1000 + 5) };
        let due: Vec<u64> = (2..=12).filter(|&s| pace.due(tick(s))).collect();
        assert_eq!(due, [6, 11]);
        pace.every = None;
        assert!(pace.due(at(12_500)) && pace.due(at(13_000)), "setup measuring: every pass");
    }

    #[test]
    fn taking_records_the_driver_policy_and_changes_nothing() {
        let mut g = Fake::new();
        let e = original(&mut g, &fan(0)).unwrap();
        assert_eq!(
            e,
            HeldEntry::Nvml { key: fan(0), uuid: "GPU-1".into(), fan: 0, original_policy: "driver".into(), original_speed: None }
        );
        assert_eq!(take(&mut g, &fan(0)).unwrap(), 53.0);
        assert!(g.log.is_empty(), "taking an NVIDIA fan writes nothing: {:?}", g.log);
    }

    #[test]
    fn writes_clamp_to_the_card_and_read_the_policy_back() {
        let mut g = Fake::new();
        assert_eq!(write(&mut g, &fan(1), 12).unwrap().mode, ControlMode::Manual);
        write(&mut g, &fan(1), 140).unwrap();
        assert_eq!(g.log, ["speed 1 30", "speed 1 100"]);
    }

    #[test]
    fn a_driver_original_goes_back_to_the_driver() {
        let mut g = Fake::new();
        let e = original(&mut g, &fan(0)).unwrap();
        write(&mut g, &fan(0), 70).unwrap();
        assert_eq!(back(&mut g, &e).unwrap(), "nvml/GPU-1/fan0: the driver's curve");
        assert_eq!(g.policy[&0], Policy::Driver);
    }

    #[test]
    fn a_manual_original_goes_back_to_manual_at_its_speed() {
        let mut g = Fake::new();
        g.policy.insert(1, Policy::Manual);
        g.target.insert(1, 64);
        let e = original(&mut g, &fan(1)).unwrap();
        assert!(matches!(&e, HeldEntry::Nvml { original_policy, original_speed: Some(64), .. } if original_policy == "manual"));
        write(&mut g, &fan(1), 90).unwrap();
        handback(&mut g, &fan(1)).unwrap();
        g.log.clear();
        assert_eq!(back(&mut g, &e).unwrap(), "nvml/GPU-1/fan1: manual at 64 %");
        assert_eq!(g.log, ["policy 1 manual", "speed 1 64"]);
        assert_eq!((g.policy[&1], g.target[&1]), (Policy::Manual, 64));
    }

    #[test]
    fn failures_are_sentences() {
        let mut g = Fake::new();
        g.root = false;
        let e = write(&mut g, &fan(0), 50).unwrap_err().to_string();
        assert!(e.contains("needs root"), "{e}");
        g.lost = true;
        let e = original(&mut g, &fan(0)).unwrap_err().to_string();
        assert!(e.contains("GPU is lost"), "{e}");
        let e = back(
            &mut Fake::new(),
            &HeldEntry::Nvml { key: fan(0), uuid: "GPU-1".into(), fan: 0, original_policy: "manual".into(), original_speed: None },
        )
        .unwrap_err()
        .to_string();
        assert!(e.contains("records no speed"), "{e}");
        assert!(parse_fan(&Key::parse("hwmon/it87/x/pwm1").unwrap()).is_err());
    }

    #[test]
    fn bus_ids_match_sysfs() {
        assert_eq!(short_bus_id("00000000:09:00.0"), "0000:09:00.0");
        assert_eq!(short_bus_id("0000:0A:00.0"), "0000:0a:00.0");
    }
}
