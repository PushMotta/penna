//! NVIDIA GPUs through NVML (DESIGN.md §3.4). `libnvidia-ml` is loaded at runtime: a machine without it has no NVML backend, not an error.
use crate::Backend;
use anyhow::Result;
use nvml_wrapper::enum_wrappers::device::{TemperatureSensor, TemperatureThreshold};
use nvml_wrapper::enums::device::FanControlPolicy;
use nvml_wrapper::error::NvmlError;
use nvml_wrapper::{Device as NvmlDevice, Nvml};
use penna_core::{
    ControlInfo, ControlKind, ControlMode, ControlReading, Device, Key, Limit, LimitKind, Readings, Sample, SensorInfo, Source, TachInfo,
    sanity,
};
use std::ffi::OsStr;

pub struct NvmlBackend {
    nvml: Nvml,
    gpus: Vec<Gpu>,
}

struct Gpu {
    index: u32,
    uuid: String,
    fans: u32,
}

impl NvmlBackend {
    /// `Err` carries why there is no NVML on this machine, in words for a note.
    pub fn open() -> std::result::Result<NvmlBackend, String> {
        let mut last = String::new();
        for lib in ["libnvidia-ml.so.1", "libnvidia-ml.so"] {
            match Nvml::builder().lib_path(OsStr::new(lib)).init() {
                Ok(nvml) => return Ok(NvmlBackend { nvml, gpus: Vec::new() }),
                Err(NvmlError::LibloadingError(_)) => last = "NVML: libnvidia-ml is not installed".to_owned(),
                Err(NvmlError::DriverNotLoaded) => return Err("NVML: the NVIDIA driver is not loaded".to_owned()),
                Err(e) => return Err(format!("NVML: {e}")),
            }
        }
        Err(last)
    }
}

impl Backend for NvmlBackend {
    fn name(&self) -> &'static str {
        "nvml"
    }

    fn discover(&mut self) -> Result<Vec<Device>> {
        self.gpus.clear();
        let mut devices = Vec::new();
        for index in 0..self.nvml.device_count()? {
            let gpu = self.nvml.device_by_index(index)?;
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

    fn original(&mut self, key: &Key) -> Result<penna_core::held::HeldEntry> {
        anyhow::bail!("{key}: {}", NvmlBackend::NOT_YET)
    }

    fn read(&mut self, out: &mut Readings) -> Result<()> {
        for gpu in &self.gpus {
            let device = match self.nvml.device_by_index(gpu.index) {
                Ok(d) => d,
                Err(e) => {
                    // The GPU fell off the bus or the driver went away: every value it owns is a failed read.
                    let reason = e.to_string();
                    out.temps.insert(Key::nvml_temp(&gpu.uuid), Sample::Error(reason.clone()));
                    for fan in 0..gpu.fans {
                        out.rpms.insert(Key::nvml_fan(&gpu.uuid, fan), Sample::Error(reason.clone()));
                        out.controls.insert(Key::nvml_fan(&gpu.uuid, fan), Sample::Error(reason.clone()));
                    }
                    continue;
                }
            };
            out.temps.insert(
                Key::nvml_temp(&gpu.uuid),
                device.temperature(TemperatureSensor::Gpu).map_or_else(|e| Sample::Error(e.to_string()), |c| sanity::temperature(c as f64)),
            );
            for fan in 0..gpu.fans {
                let key = Key::nvml_fan(&gpu.uuid, fan);
                out.rpms.insert(key.clone(), device.fan_speed_rpm(fan).map_or_else(|e| Sample::Error(e.to_string()), Sample::Value));
                out.controls.insert(key, fan_control(&device, fan));
            }
        }
        Ok(())
    }
}

impl NvmlBackend {
    /// Real NVIDIA control arrives in M4 (DESIGN.md §10); until then every control call says so.
    pub const NOT_YET: &'static str = "real NVIDIA fan control arrives in M4";
}

fn fan_control(device: &NvmlDevice<'_>, fan: u32) -> Sample<ControlReading> {
    let percent = match device.fan_speed(fan) {
        Ok(p) => p,
        Err(e) => return Sample::Error(e.to_string()),
    };
    let mode = match device.fan_control_policy(fan) {
        Ok(FanControlPolicy::TemperatureContinousSw) => ControlMode::Driver,
        Ok(FanControlPolicy::Manual) => ControlMode::Manual,
        Err(_) => ControlMode::Unknown,
    };
    Sample::Value(ControlReading { percent: percent as f64, raw: None, mode })
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

    #[test]
    fn bus_ids_match_sysfs() {
        assert_eq!(short_bus_id("00000000:09:00.0"), "0000:09:00.0");
        assert_eq!(short_bus_id("0000:0A:00.0"), "0000:0a:00.0");
    }
}
