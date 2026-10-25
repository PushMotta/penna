# Changelog

## 0.10.5 (25 Oct 2026)

- **`pennad` costs about half the CPU on a machine with an NVIDIA GPU.** Each NVML call costs about a millisecond of kernel
  time, and a 3090's two fans took 7–9 ms of each one-second tick. An NVIDIA fan's RPM and percent are now read every 5 s,
  and every tick while setup measures. Who controls the fan, which read-back checks, and the GPU's temperature are still
  read every tick. The popup's and studio's GPU fan speeds now update every 5 s.
- **Sleep.** `pennad` now holds a logind delay lock. When the machine goes to sleep, it leaves the fans alone until the machine
  wakes, then writes every fan it drives again. Before, its loop ran on into the suspend. On the desk's first suspend a fan
  write hung there, and on waking the watchdog killed `pennad`. Every fan was handed back and taken again a second later, but
  only because the last way out worked.

## 0.10.4 (23 Oct 2026)

From a second machine, an Intel N100 mini PC whose firmware runs its fan through ACPI:
- **`penna doctor`** names a machine past its firmware's placeholders ("To be filled by O.E.M."). It also says plainly when a
  fan is run by the firmware through ACPI, on or off with no speed to read, rather than sending you to look for a fan chip.
- **`penna capture`** keeps the thermal class, so ACPI fans and the zones that drive them show in a machine report. It also
  finds a sensor's module when its device has no driver link, as `coretemp`'s doesn't.
- **The power-profile lever** works only where the firmware has a platform profile. Without one, power-profiles-daemon
  changes only the CPU's power, and that is no fan lever.
- An ACPI fan that reports its speed is read as `acpi_fan`, and a Wi-Fi card's sensor is called Wi-Fi.

## 0.10.3 (23 Oct 2026)

- **Plugin 0.7.2, for the Omarchy marketplace:** its README links to Penna's for installing and removing the daemon, and the
  popup points to `penna doctor` when `pennad` isn't running or the user can't change fans. The plugin's own tree now holds
  no `sudo`, `systemctl` or package-manager commands, which the marketplace's security baseline asks a maintainer to review.

## 0.10.2 (23 Oct 2026)

- **Install from the release.** New AUR accounts are paused, so each release now carries a package built from its own source
  tarball, with its checksum. The README says how to install it with pacman.
- **The README shows every part of Penna:** the popup, the studio, setup, a fresh install, a laptop, the themes and the command
  line, in screenshots and GIFs of the real plugin. `tools/screenshots` remakes them.
- **Plugin 0.7.1:** the curve editor no longer throws when a curve's live reading goes away.

## 0.10.1 (23 Oct 2026)

- **The package builds on a machine without Penna.** Four daemon tests read the installed board database, so 0.10.0's
  `check()` failed wherever Penna wasn't installed yet, as in CI or a first AUR build. The tests now use the checkout's.
- **pennad answers when its loop falls behind.** Requests were taken only while the loop was ahead of its schedule, so a tick
  slower than its interval left every change from the popup, the studio or the CLI to time out after 10 s. They are now taken
  every tick.

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
