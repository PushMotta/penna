# Changelog

## 0.10.1 (23 Oct 2026)

- **The package builds on a machine without Penna.** Four daemon tests read the installed board database, so 0.10.0's
  `check()` failed wherever Penna wasn't installed yet, as in CI or a first AUR build. The tests now use the checkout's.

## 0.10.0 (23 Oct 2026)

The first public release. Penna runs every day on the desk it was built on, where every way out has been tested on the
hardware; every other driver is built from the kernel's source and tested in a simulator.

- **Fans:** motherboard Super I/O chips (`it87`, `nct6775`, `nct6683`, `f71882fg`, `w83627ehf`), NVIDIA GPUs through NVML
  and `nouveau`, AMD GPUs through `amdgpu` (RDNA4 through its OverDrive curve), and laptops: ThinkPad, Dell, ASUS, HP,
  Lenovo, Framework, GPD, Acer and Intel Macs. Each fan is driven as what it is: a duty, fixed levels, a target speed, a
  curve the firmware runs, or only automatic and full speed. Alienware and Legion fans are read only. On a laptop with no fan
  Penna can drive, its profiles switch the power profile.
- **Setup:** one automatic run measures every fan, pairs it to its tach, suggests its role, calibrates it, and proposes Quiet,
  Balanced and Render curves for review. A CPU load check tests Balanced under load.
- **Safety:** every fan goes back to its BIOS or driver on every way out, including a crash, `kill -9`, a hung loop and
  removing the package. *Defaults* does it on demand.
- **Surfaces:** the Omarchy plugin (bar glyph, popup, studio), the `penna` command line, and a Waybar module.
- **Laptops with a sleeping GPU:** a hybrid laptop's dGPU, runtime-suspended, reads as asleep rather than failed. A curve on it
  rests at its lowest point instead of running the fans at 100 %.
- **Machine reports:** `penna capture` packs what Penna needs to learn a machine, without root and without serial numbers.
- **The plugin and the daemon** say which one to update when they don't fit together, since one comes from git and the other
  from a package.

Support tiers: `it87` and NVML are Verified; every other driver is Documented. See the README.
