//! Penna's model, with no I/O in it (DESIGN.md §3, §7.2).
//!
//! * [`Key`] names a sensor, control or tach by hardware path and channel, never by `hwmonN` (§3.2).
//! * [`Inventory`] is what discovery found; [`Readings`] is one pass of values over it.
//! * [`sanity`] holds the rules that turn raw numbers into readings or reject them (§3.3).
//! * [`config::Config`] is what a person chose; [`plan::Plan`] is that resolved against a machine.
//! * [`control::tick`] is the control loop's pure heart (§4, §6): readings in, commands out.
//! * [`held::HeldEntry`] is the way back for one control, written before it leaves firmware (§6.1).
//! * [`drivers`] is what Penna knows about each fan driver (§3.5, §3.6): data, not code paths.
//! * [`boards::Board`] is what the board database knows about one motherboard (§3.6).
//! * [`setup::Setup`] is setup's run (§5): readings in, the duty each measured control runs at out.
pub mod boards;
pub mod config;
pub mod control;
pub mod drivers;
pub mod held;
pub mod key;
pub mod model;
pub mod plan;
pub mod sanity;
pub mod setup;
pub mod starter;

pub use control::{Command, LoopState, Outcome, Request, Scope, State, tick};
pub use key::Key;
pub use model::{
    ControlInfo, ControlKind, ControlMode, ControlReading, Device, Inventory, Limit, LimitKind, Readings, Sample, SensorInfo, Source,
    TachInfo, Unreachable,
};
