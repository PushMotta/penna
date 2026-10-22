//! The simulator's NVML (DESIGN.md §9.1): one directory per GPU, files the simulator updates and this backend reads and writes.
//! It behaves as the 3090 on the development desk does (§0): manual speeds below its 30 % floor are refused, the driver's own
//! policy stops the fans when cool, and the percent it reports is of a "maximum noise tolerance" speed, not duty.
use crate::{Backend, Readback};
use anyhow::{Context, Result, anyhow, bail};
use penna_core::held::HeldEntry;
use penna_core::{
    ControlInfo, ControlKind, ControlMode, ControlReading, Device, Key, Limit, LimitKind, Readings, Sample, SensorInfo, Source, TachInfo,
    sanity,
};
use std::fs;
use std::path::{Path, PathBuf};

pub const DRIVER: &str = "driver";
pub const MANUAL: &str = "manual";

pub struct MockNvml {
    root: PathBuf,
    gpus: Vec<Gpu>,
}

struct Gpu {
    uuid: String,
    dir: PathBuf,
    fans: u32,
    min: u32,
    max: u32,
}

impl MockNvml {
    pub fn new(root: impl Into<PathBuf>) -> MockNvml {
        MockNvml { root: root.into(), gpus: Vec::new() }
    }

    fn fan(&self, key: &Key) -> Result<(&Gpu, u32)> {
        let uuid = key.device().strip_prefix("nvml/").ok_or_else(|| anyhow!("{key} is not an NVML key"))?;
        let fan: u32 = key.channel().strip_prefix("fan").and_then(|n| n.parse().ok()).ok_or_else(|| anyhow!("{key} is not a fan"))?;
        let gpu = self.gpus.iter().find(|g| g.uuid == uuid).ok_or_else(|| anyhow!("no GPU {uuid}"))?;
        if fan >= gpu.fans {
            bail!("{key}: the GPU has {} fans", gpu.fans);
        }
        if gpu.dir.join("lost").exists() {
            bail!("GPU is lost");
        }
        Ok((gpu, fan))
    }
}

fn read(dir: &Path, name: &str) -> Result<String> {
    let p = dir.join(name);
    Ok(fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?.trim().to_owned())
}

fn read_f(dir: &Path, name: &str) -> Result<f64> {
    let s = read(dir, name)?;
    s.parse().map_err(|_| anyhow!("{name} holds {s:?}"))
}

fn write(dir: &Path, name: &str, v: impl std::fmt::Display) -> Result<()> {
    let p = dir.join(name);
    fs::write(&p, format!("{v}\n")).with_context(|| format!("writing {}", p.display()))
}

fn mode(policy: &str) -> ControlMode {
    match policy {
        MANUAL => ControlMode::Manual,
        DRIVER => ControlMode::Driver,
        _ => ControlMode::Unknown,
    }
}

impl Backend for MockNvml {
    fn name(&self) -> &'static str {
        "nvml (mock)"
    }

    fn discover(&mut self) -> Result<Vec<Device>> {
        self.gpus.clear();
        let mut dirs: Vec<PathBuf> = match fs::read_dir(&self.root) {
            Ok(e) => e.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.is_dir()).collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e.into()),
        };
        dirs.sort();
        let mut devices = Vec::new();
        for dir in dirs {
            let uuid = dir.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_owned();
            let fans = read_f(&dir, "fans")? as u32;
            let (min, max) = (read_f(&dir, "min")? as u32, read_f(&dir, "max")? as u32);
            let mut temp = SensorInfo {
                key: Key::nvml_temp(&uuid),
                label: "GPU".into(),
                limits: vec![],
                ignored_limits: vec![],
                below_ambient: false,
            };
            for (kind, file) in [
                (LimitKind::GpuMax, "gpu_max"),
                (LimitKind::Slowdown, "slowdown"),
                (LimitKind::Shutdown, "shutdown"),
                (LimitKind::Acoustic, "acoustic"),
            ] {
                if let Ok(c) = read_f(&dir, file)
                    && let Some(c) = sanity::limit(c)
                {
                    temp.limits.push(Limit { kind, celsius: c });
                }
            }
            let mut device = Device {
                id: format!("nvml/{uuid}"),
                source: Source::Nvml,
                driver: "nvml".into(),
                label: read(&dir, "name").unwrap_or_else(|_| "NVIDIA GPU (mock)".into()),
                location: "mock".into(),
                model: None,
                sensors: vec![temp],
                tachs: vec![],
                controls: vec![],
            };
            for fan in 0..fans {
                let key = Key::nvml_fan(&uuid, fan);
                let label = format!("Fan {}", fan + 1);
                device.tachs.push(TachInfo { key: key.clone(), label: label.clone() });
                device.controls.push(ControlInfo { key, label, kind: ControlKind::Nvml { min, max } });
            }
            devices.push(device);
            self.gpus.push(Gpu { uuid, dir, fans, min, max });
        }
        Ok(devices)
    }

    fn read(&mut self, out: &mut Readings) -> Result<()> {
        for g in &self.gpus {
            if g.dir.join("lost").exists() {
                out.temps.insert(Key::nvml_temp(&g.uuid), Sample::Error("GPU is lost".into()));
                for fan in 0..g.fans {
                    out.rpms.insert(Key::nvml_fan(&g.uuid, fan), Sample::Error("GPU is lost".into()));
                    out.controls.insert(Key::nvml_fan(&g.uuid, fan), Sample::Error("GPU is lost".into()));
                }
                continue;
            }
            out.temps.insert(
                Key::nvml_temp(&g.uuid),
                read_f(&g.dir, "temp").map_or_else(|e| Sample::Error(format!("{e:#}")), sanity::temperature),
            );
            for fan in 0..g.fans {
                let key = Key::nvml_fan(&g.uuid, fan);
                let rpm = read_f(&g.dir, &format!("fan{fan}_rpm"))
                    .map_or_else(|e| Sample::Error(format!("{e:#}")), |v| Sample::Value(v.max(0.0) as u32));
                out.rpms.insert(key.clone(), rpm);
                let control = match (read_f(&g.dir, &format!("fan{fan}_percent")), read(&g.dir, &format!("fan{fan}_policy"))) {
                    (Ok(percent), Ok(policy)) => Sample::Value(ControlReading { percent, raw: None, mode: mode(&policy) }),
                    (Err(e), _) | (_, Err(e)) => Sample::Error(format!("{e:#}")),
                };
                out.controls.insert(key, control);
            }
        }
        Ok(())
    }

    fn original(&mut self, key: &Key) -> Result<HeldEntry> {
        let (g, fan) = self.fan(key)?;
        let name = format!("fan{fan}_policy");
        Ok(HeldEntry::MockNvml { key: key.clone(), policy: g.dir.join(&name), original_policy: read(&g.dir, &name)? })
    }

    /// Taking an NVML fan changes nothing: it stays on the driver's policy until the first write.
    fn take(&mut self, key: &Key) -> Result<f64> {
        let (g, fan) = self.fan(key)?;
        read_f(&g.dir, &format!("fan{fan}_percent"))
    }

    fn write(&mut self, key: &Key, raw: u32) -> Result<Readback> {
        let (g, fan) = self.fan(key)?;
        if !(g.min..=g.max).contains(&raw) {
            bail!("invalid argument: {raw} % is outside {}–{} %", g.min, g.max);
        }
        write(&g.dir, &format!("fan{fan}_target"), raw)?;
        write(&g.dir, &format!("fan{fan}_policy"), MANUAL)?;
        Ok(Readback { raw: None, mode: ControlMode::Manual })
    }

    fn handback(&mut self, key: &Key) -> Result<()> {
        let (g, fan) = self.fan(key)?;
        write(&g.dir, &format!("fan{fan}_policy"), DRIVER)
    }
}
