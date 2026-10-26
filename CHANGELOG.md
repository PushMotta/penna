# Changelog

## 0.10.6 (26 Oct 2026)

Security, after the Omarchy marketplace's review found that one local account could reach another's desktop through a fan's
name. A review of everything one local account can do to another, or to the daemon they share, found more of the same kind
(DESIGN.md §7.4):
- **Any local user could take every descriptor `pennad` needs.** The socket took connections without limit and never closed
  an idle one, and with no descriptor left the daemon can't write or hand back a fan. Now: at most 32 connections per uid other
  than root and 256 in all, and one that asks nothing within 30 s of its hello is closed. A failing `accept` waits instead of
  spinning, and says so in the journal once every 10 s at most, not 7,800 times a second.
- **A member of the `penna` group could make every tick enormous.** Curve ids, curves, graph points and mixes had no limit, so
  one saved configuration of 28 MB made every subscriber's tick 56 MB and slowed the control loop four times. A configuration
  now holds at most 128 curves, 32 points a graph, 16 curves a mix, 64 virtual sensors, 32 profiles and 512 labels, and at
  every limit it is under half a protocol line.
- **Names are text.** A label, a curve's id or name, or a profile's name could hold escape sequences and newlines, which reached
  other users' terminals (a clipboard write, a cleared screen) and forged lines in the journal. They are now 1–48 characters
  with no control characters, newlines, NUL, bidirectional marks or overrides, line separators, or characters that draw
  nothing (a zero-width space, a byte-order mark), and no key holds one either. The CLI also replaces any such character it
  is sent, from an older daemon, with `�`, and passes a clean line on to `--json` exactly as the daemon sent it.
- **`penna doctor`** suggested `penna retry "<label>"`, so a `$(…)` in a label ran when the line was pasted. It names the fan
  by its key now. **`penna waybar`** escapes its tooltip, which Waybar reads as Pango markup.
- **Who changed something** is in the journal, no longer in the events every local user can subscribe to.
- **The penna group from the kernel:** a connecting process's groups come from `SO_PEERGROUPS`, not from `/proc/<pid>/status`,
  where the pid may belong to another process by the time it is read.
- **`sudo penna restore --from <file>`** writes only the fan files `pennad` records (`pwmN`, `pwmN_enable`, a target, a
  firmware curve's points, `platform_profile`…), under `/sys` and with no `..`: not another sysfs file, nor a new one
  elsewhere.
- **The unit** also has `ProtectKernelLogs`, Unix and netlink sockets only, no IP, and the `@system-service` syscall set. The
  hardware tests now run `pennad` with the unit's own sandboxing.
- **Plugin 0.7.4: a name is shown as text, never as markup.** Any member of the `penna` group can set a fan's or sensor's
  label, and the popup read a fan's name, a temperature's and a sensor's as Qt styled text. A label holding an `<img>` tag
  made another user's popup fetch that URL from their desktop (the marketplace's review found it). Every Text in the plugin
  now reads plain text, and the lines that dim a note after a name escape both parts. `test/markup.test.js` fails on any
  Text without an explicit format, and on markup written anywhere but `Model.dimmedLine`. The plugin also refuses a line
  over 4 MiB before parsing it, drops the history of sensors and fans that are gone, and its CI job has a timeout.
- **Plugin 0.7.3:** its README names the one release of Penna it was reviewed with, 0.10.5, by its full commit and its
  package's SHA-256. It installs exactly that package, checked against the digest written in the README, or builds that
  commit. The Omarchy marketplace's review asked for this. The plugin's repository now moves only when the plugin is pointed
  at a newer release (`tools/release.sh plugin`), not at every release.
- **The README's install** checks the package against a digest written in the README, not only against the release's own
  `SHA256SUMS`, and builds a release from the commit it was built from rather than from `main`.

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
