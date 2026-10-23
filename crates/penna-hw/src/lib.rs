//! Penna's hardware backends (DESIGN.md §3.4, §7.2).
//!
//! * [`hwmon::Hwmon`] reads sysfs under an injectable root: `/sys` in production, a [`capture`] in tests.
//! * [`nvml::NvmlBackend`] reads NVIDIA GPUs; it is simply absent where `libnvidia-ml` is.
//! * [`pci::unreachable`] lists display devices passed through to a VM.
//! * [`mock_nvml::MockNvml`] is the simulator's file-backed NVML, and [`sim`] the simulator itself (§9.1).
//! * [`held`] writes `held.json` and restores from it (§6.1, §6.2).
//!
//! Discovery is re-run when the set of hwmon devices changes (§3.6), so a driver loaded while `pennad` runs adds its fans.
pub mod capture;
pub mod held;
pub mod hwmon;
pub mod mock_nvml;
pub mod nvml;
pub mod pci;
pub mod platform;
pub mod sim;

use anyhow::{Result, anyhow, bail};
use penna_core::boards::Board;
use penna_core::drivers::Class;
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
    /// A firmware curve, °C → %, written whole for the firmware to run (§3.5).
    fn write_curve(&mut self, key: &Key, _points: &[[f64; 2]]) -> Result<()> {
        bail!("{}: {key} runs no firmware curve", self.name())
    }
    /// Exactly what `held.json` recorded, with this backend's own handle where it has one.
    fn release(&mut self, entry: &HeldEntry) -> Result<String> {
        held::restore(entry)
    }
    /// Whether what `discover` found may have changed since: a driver loaded or unloaded, a device added or gone (§3.6).
    fn changed(&mut self) -> bool {
        false
    }
    /// A held entry pointed at this control's files as they are now, keeping what the control was found as. `None` when this
    /// backend doesn't have the control, or its entries name no files.
    fn relocate(&self, _entry: &HeldEntry) -> Option<HeldEntry> {
        None
    }
}

pub struct Options {
    pub sysfs_root: PathBuf,
    pub nvml: bool,
    /// The simulator's NVML, instead of the real one.
    pub mock_nvml: Option<PathBuf>,
    /// Open hwmon controls for writing.
    pub control: bool,
    /// The board database (§3.6); `None` leaves boards unnamed.
    pub boards: Option<PathBuf>,
}

/// Where the package installs the board database.
pub const BOARDS: &str = "/usr/share/penna/boards";

impl Default for Options {
    /// `PENNA_BOARDS` names another board database, such as a checkout's `data/boards`.
    fn default() -> Options {
        let boards = std::env::var_os("PENNA_BOARDS").map_or_else(|| PathBuf::from(BOARDS), PathBuf::from);
        Options { sysfs_root: PathBuf::from("/sys"), nvml: true, mock_nvml: None, control: false, boards: Some(boards) }
    }
}

/// The DMI board vendor and name under a sysfs root, which are world-readable.
pub fn dmi_board(sysfs_root: &Path) -> Option<(String, String)> {
    let read = |f: &str| std::fs::read_to_string(sysfs_root.join("class/dmi/id").join(f)).ok().map(|s| s.trim().to_owned());
    Some((read("board_vendor")?, read("board_name")?))
}

/// Every board file in `dir`, and why any could not be read.
pub fn load_boards(dir: &Path) -> (Vec<Board>, Vec<String>) {
    let mut boards = Vec::new();
    let mut problems = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return (boards, problems) };
    let mut paths: Vec<PathBuf> =
        entries.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "toml")).collect();
    paths.sort();
    for p in paths {
        match std::fs::read_to_string(&p).map_err(|e| e.to_string()).and_then(|t| Board::parse(&t)) {
            Ok(b) => boards.push(b),
            Err(e) => problems.push(format!("{}: {e}", p.display())),
        }
    }
    (boards, problems)
}

/// Whether a path is under the real `/sys`, rather than a test tree.
pub fn is_real_sysfs(path: &Path) -> bool {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()).starts_with("/sys")
}

/// Every backend this machine has, and what they found.
pub struct Machine {
    backends: Vec<Box<dyn Backend>>,
    /// What each backend found, in the order of `backends`.
    found: Vec<Vec<Device>>,
    /// Findings from opening the backends (NVML missing), kept across rediscovery.
    opened: Vec<String>,
    sysfs_root: PathBuf,
    /// Control key → the backend that owns it.
    owners: BTreeMap<Key, usize>,
    /// What the board database knows about this machine's board.
    pub board: Option<Board>,
    /// Control → the tachs pairing found it turns (§5.2), which name it.
    pairing: BTreeMap<Key, Vec<Key>>,
    pub inventory: Inventory,
}

/// hwmon controls are named for their chip and channel until pairing names them for a header (§5.2): "IT8688E pwm1", then
/// "CPU_FAN". Two chips both have a `pwm1`, and a name that two controls share can't be used to pick one.
fn label_controls(inventory: &mut Inventory, pairing: &BTreeMap<Key, Vec<Key>>) {
    // A tach named only by its channel (`fan6`) says less than the chip does: only a header's name names a control.
    let tachs: BTreeMap<Key, String> = inventory
        .devices
        .iter()
        .flat_map(|d| &d.tachs)
        .filter(|t| t.label != t.key.channel())
        .map(|t| (t.key.clone(), t.label.clone()))
        .collect();
    for d in inventory.devices.iter_mut().filter(|d| d.source == penna_core::Source::Hwmon) {
        let chip = d.model.clone().unwrap_or_else(|| d.driver.clone());
        for c in &mut d.controls {
            let headers: Option<Vec<&str>> =
                pairing.get(&c.key).map(|ts| ts.iter().filter_map(|t| tachs.get(t).map(String::as_str)).collect());
            c.label = match headers {
                Some(h) if !h.is_empty() => h.join(" + "),
                _ => format!("{chip} {}", c.key.channel()),
            };
        }
    }
}

impl Machine {
    pub fn open(opts: &Options) -> Result<Machine> {
        let mut opened = Vec::new();
        let hwmon = if opts.control { hwmon::Hwmon::controlling(&opts.sysfs_root)? } else { hwmon::Hwmon::new(&opts.sysfs_root) };
        let mut backends: Vec<Box<dyn Backend>> = vec![Box::new(hwmon)];
        if let Some(root) = &opts.mock_nvml {
            backends.push(Box::new(mock_nvml::MockNvml::new(root)));
        } else if opts.nvml {
            match nvml::NvmlBackend::open() {
                Ok(b) => backends.push(Box::new(b)),
                Err(reason) => opened.push(reason),
            }
        }
        let found = vec![Vec::new(); backends.len()];
        let board = match (&opts.boards, dmi_board(&opts.sysfs_root)) {
            (Some(dir), Some((vendor, name))) => {
                let (boards, problems) = load_boards(dir);
                opened.extend(problems);
                penna_core::boards::find(&boards, &vendor, &name).cloned()
            }
            _ => None,
        };
        let mut m = Machine {
            backends,
            found,
            opened,
            sysfs_root: opts.sysfs_root.clone(),
            owners: BTreeMap::new(),
            board,
            pairing: BTreeMap::new(),
            inventory: Inventory::default(),
        };
        let mut errors = Vec::new();
        for i in 0..m.backends.len() {
            if let Err(e) = m.discover(i) {
                errors.push(e);
            }
        }
        m.assemble(errors);
        Ok(m)
    }

    /// Discovers again when a backend says its devices changed (§3.6), and says whether it did. The devices may come out the
    /// same while their files moved, as when a driver is reloaded: held entries then need [`Machine::relocate`].
    pub fn rediscover(&mut self) -> bool {
        let changed: Vec<usize> = (0..self.backends.len()).filter(|&i| self.backends[i].changed()).collect();
        if changed.is_empty() {
            return false;
        }
        let mut errors = Vec::new();
        for i in changed {
            if let Err(e) = self.discover(i) {
                errors.push(e);
            }
        }
        self.assemble(errors);
        true
    }

    fn discover(&mut self, i: usize) -> Result<(), String> {
        match self.backends[i].discover() {
            Ok(found) => {
                self.found[i] = found;
                Ok(())
            }
            Err(e) => {
                self.found[i].clear();
                Err(format!("{}: {e:#}", self.backends[i].name()))
            }
        }
    }

    fn assemble(&mut self, errors: Vec<String>) {
        self.owners.clear();
        for (i, found) in self.found.iter().enumerate() {
            for c in found.iter().flat_map(|d| &d.controls) {
                self.owners.insert(c.key.clone(), i);
            }
        }
        let devices: Vec<Device> = self.found.iter().flatten().cloned().collect();
        let mut notes = self.opened.clone();
        notes.extend(errors);
        let motherboard = devices.iter().any(|d| penna_core::drivers::lookup(&d.driver).is_some_and(|e| e.class == Class::Motherboard));
        if !motherboard {
            notes.push("Motherboard fans: none visible. No Super I/O driver (it87, nct6775, …) is bound.".to_owned());
        }
        let unreachable = pci::unreachable(&self.sysfs_root);
        self.inventory = Inventory { devices, unreachable, notes };
        if let Some(b) = &self.board {
            b.apply(&mut self.inventory);
        }
        label_controls(&mut self.inventory, &self.pairing);
    }

    /// What pairing found (§5.2): each paired control takes its header's name.
    pub fn set_pairing(&mut self, measured: &penna_core::config::Measured) {
        self.pairing = measured.controls.iter().filter(|m| !m.tachs.is_empty()).map(|m| (m.key.clone(), m.tachs.clone())).collect();
        label_controls(&mut self.inventory, &self.pairing);
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
    pub fn write_curve(&mut self, key: &Key, points: &[[f64; 2]]) -> Result<()> {
        self.owner(key)?.write_curve(key, points)
    }
    pub fn relocate(&self, entry: &HeldEntry) -> Option<HeldEntry> {
        let i = *self.owners.get(entry.key())?;
        self.backends[i].relocate(entry)
    }
    /// Puts a held control back, through the backend that owns it when there is one.
    pub fn release(&mut self, entry: &HeldEntry) -> Result<String> {
        match self.owners.get(entry.key()).copied() {
            Some(i) => self.backends[i].release(entry),
            None => held::restore(entry),
        }
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
