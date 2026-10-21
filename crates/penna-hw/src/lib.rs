//! Penna's hardware backends (DESIGN.md §3.4, §7.2).
//!
//! * [`hwmon::Hwmon`] reads sysfs under an injectable root: `/sys` in production, a [`capture`] in tests.
//! * [`nvml::NvmlBackend`] reads NVIDIA GPUs; it is simply absent where `libnvidia-ml` is.
//! * [`pci::unreachable`] lists display devices passed through to a VM.
//! * [`mock_nvml::MockNvml`] is the simulator's file-backed NVML, and [`sim`] the simulator itself (§9.1).
//! * [`held`] writes `held.json` and restores from it (§6.1, §6.2).
//!
//! Control (`take`, `write`, `handback`, `release`) works against an injectable root only: M1 never writes under the real `/sys`,
//! and real NVML control arrives in M4.
pub mod capture;
pub mod held;
pub mod hwmon;
pub mod mock_nvml;
pub mod nvml;
pub mod pci;
pub mod sim;

use anyhow::{Result, anyhow, bail};
use penna_core::held::HeldEntry;
use penna_core::{ControlMode, Device, Inventory, Key, Readings};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// What a control reads right after a write (§6.4).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Readback {
    pub raw: Option<u32>,
    pub mode: ControlMode,
}

pub trait Backend {
    fn name(&self) -> &'static str;
    /// Sensors, controls and tachs, with stable keys.
    fn discover(&mut self) -> Result<Vec<Device>>;
    /// One pass over everything `discover` found. A channel that fails to read is a [`penna_core::Sample::Error`], not an `Err`.
    fn read(&mut self, out: &mut Readings) -> Result<()>;
    /// The way back, read before the control leaves firmware. It goes into `held.json` before [`Backend::take`] (§6.1).
    fn original(&mut self, key: &Key) -> Result<HeldEntry> {
        bail!("{}: {key} cannot be controlled yet", self.name())
    }
    /// Leave firmware without a jump. Returns the duty the firmware was running.
    fn take(&mut self, key: &Key) -> Result<f64> {
        bail!("{}: {key} cannot be controlled yet", self.name())
    }
    fn write(&mut self, key: &Key, _raw: u32) -> Result<Readback> {
        bail!("{}: {key} cannot be controlled yet", self.name())
    }
    /// Manual again, then write: something changed the control under us.
    fn reassert(&mut self, key: &Key, raw: u32) -> Result<Readback> {
        self.write(key, raw)
    }
    /// NVML: back to the driver's curve while Penna still holds the fan.
    fn handback(&mut self, key: &Key) -> Result<()> {
        bail!("{}: {key} has no driver curve to hand back to", self.name())
    }
}

pub struct Options {
    pub sysfs_root: PathBuf,
    pub nvml: bool,
    /// The simulator's NVML, instead of the real one.
    pub mock_nvml: Option<PathBuf>,
    /// Open controls for writing. Refused under the real `/sys` in M1.
    pub control: bool,
}

impl Default for Options {
    fn default() -> Options {
        Options { sysfs_root: PathBuf::from("/sys"), nvml: true, mock_nvml: None, control: false }
    }
}

/// M1 drives simulated hardware only.
pub fn refuse_real_sysfs(path: &Path) -> Result<()> {
    let real = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if real.starts_with("/sys") {
        bail!(
            "{} is the real /sys. This Penna (M1) controls simulated hardware only; real control arrives in M4 (NVIDIA) and M5 (motherboard). Try: pennad --mock render-burst",
            path.display()
        );
    }
    Ok(())
}

/// Every backend this machine has, and what they found.
pub struct Machine {
    backends: Vec<Box<dyn Backend>>,
    /// Control key → the backend that owns it.
    owners: BTreeMap<Key, usize>,
    pub inventory: Inventory,
}

impl Machine {
    pub fn open(opts: &Options) -> Result<Machine> {
        let mut notes = Vec::new();
        let hwmon = if opts.control { hwmon::Hwmon::controlling(&opts.sysfs_root)? } else { hwmon::Hwmon::new(&opts.sysfs_root) };
        let mut backends: Vec<Box<dyn Backend>> = vec![Box::new(hwmon)];
        if let Some(root) = &opts.mock_nvml {
            backends.push(Box::new(mock_nvml::MockNvml::new(root)));
        } else if opts.nvml {
            match nvml::NvmlBackend::open() {
                Ok(b) => backends.push(Box::new(b)),
                Err(reason) => notes.push(reason),
            }
        }

        let mut devices = Vec::new();
        let mut owners = BTreeMap::new();
        for (i, backend) in backends.iter_mut().enumerate() {
            match backend.discover() {
                Ok(found) => {
                    for d in &found {
                        for c in &d.controls {
                            owners.insert(c.key.clone(), i);
                        }
                    }
                    devices.extend(found);
                }
                Err(e) => notes.push(format!("{}: {e:#}", backend.name())),
            }
        }
        let has_super_io = devices.iter().any(|d| hwmon::SUPER_IO_DRIVERS.contains(&d.driver.as_str()));
        if !has_super_io {
            notes.push("Motherboard fans: none visible. No Super I/O driver (it87, nct6775, …) is bound.".to_owned());
        }
        let unreachable = pci::unreachable(&opts.sysfs_root);
        Ok(Machine { backends, owners, inventory: Inventory { devices, unreachable, notes } })
    }

    fn owner(&mut self, key: &Key) -> Result<&mut Box<dyn Backend>> {
        let i = *self.owners.get(key).ok_or_else(|| anyhow!("{key} is not a control on this machine"))?;
        Ok(&mut self.backends[i])
    }

    pub fn original(&mut self, key: &Key) -> Result<HeldEntry> {
        self.owner(key)?.original(key)
    }
    pub fn take(&mut self, key: &Key) -> Result<f64> {
        self.owner(key)?.take(key)
    }
    pub fn write(&mut self, key: &Key, raw: u32) -> Result<Readback> {
        self.owner(key)?.write(key, raw)
    }
    pub fn reassert(&mut self, key: &Key, raw: u32) -> Result<Readback> {
        self.owner(key)?.reassert(key, raw)
    }
    pub fn handback(&mut self, key: &Key) -> Result<()> {
        self.owner(key)?.handback(key)
    }

    pub fn read(&mut self) -> Readings {
        let mut readings = Readings::default();
        for backend in &mut self.backends {
            if let Err(e) = backend.read(&mut readings) {
                // Backends report per-channel failures in the readings; an Err here means the backend itself is gone.
                let note = format!("{}: {e:#}", backend.name());
                if !self.inventory.notes.contains(&note) {
                    self.inventory.notes.push(note);
                }
            }
        }
        readings
    }
}
