# Penna — design specification

Draft 3 · 21 October 2026 · status: decisions approved (§11), UI quality bar added (§8.6), M0–M6 done, design review 01 approved (milestone D, §11.1), design review 03 approved (milestone D2, §11.3). Penna 0.8.1 runs as a service on this desk, and since setup's generated curves were saved there it runs every fan on Balanced (M8). Draft 3 widens Penna from this desk to any workstation or laptop, with automatic setup, generated curves and a Defaults profile: design review 02 (§11.2) was approved the same day. M5 was done on 22 Oct 2026: Penna drives every fan on this desk, and it87 is Verified.

Companion: design review 01 — mockups in real theme tokens, live motion prototypes, and eight calls before QML — is published as an artifact (https://claude.ai/artifact/DmFftUzWVBd11NNBv4e8aK). Its source is `design/penna-design-review.html`.

Draft 1 called it `fanctl`; that name is taken in the AUR (§11, decision 1).

---

## 0. Verified environment

Everything below was checked on this machine on 21 Oct 2026, not taken from the brief. Read-only probes only: nothing was written to hardware and no driver was loaded.

| Item | Value |
|---|---|
| Machine | Gigabyte X570 AORUS PRO, AMD Ryzen 9 5950X (16 cores, 32 threads). |
| OS | Omarchy 4.0.5, kernel 7.2.8-9-omarchy with `linux-omarchy-headers` and `dkms` 3.4.4 installed, systemd 262. Hibernation is configured (`resume=` on the kernel command line). |
| Shell | Quickshell 0.3.2, Qt 6.12.0. `Quickshell.Io` exports `Socket`, `SocketServer`, `Process`, `SplitParser`, `DataStreamParser`, `FileView`, `JsonAdapter`. A polkit agent ships as `omarchy.polkit`. No temperature or fan surface exists in the shell (`hwmon` appears nowhere under `/usr/share/omarchy/shell`). |
| Toolchain | rustc and cargo 1.99.1. Crates current today: `nvml-wrapper` 0.13.0, `tokio` 1.54.0, `sd-notify` 0.5.0, `libmedium` 0.13.4. |
| hwmon | `hwmon0` acpitz, `hwmon2` nvme, `hwmon3` k10temp. There is no `hwmon1` this boot: indices are neither stable nor contiguous. |
| CPU | `k10temp` at `0000:00:18.3`: `Tctl` 78.5 °C, `Tccd1` 80.8 °C, `Tccd2` 50.5 °C at the moment of reading (1-minute load 2.85). No `temp_crit`. |
| NVMe | `Composite` 37.9 °C, `temp1_max` 81.85, `temp1_crit` 84.85. `Sensor 1` and `Sensor 2` report `temp_max` = 65261.85 °C, a firmware placeholder. |
| ACPI | `acpitz` reads 16.8 °C on both channels, below room temperature. Not a usable sensor. |
| Motherboard fans | **None visible.** No Super I/O driver is bound and `it87` does not autoload. Boot log: `ACPI: OSL: Resource conflict; ACPI support missing from driver?`. The stock `it87` in 7.2 knows IT8689E, IT8792E/IT8795E and IT87952E, but not IT8688E. Which chips this board carries is **not verified** (it needs root, §13); forum reports for X570 AORUS boards name IT8688E + IT8792E. |
| GPU 0 | ASUS RTX 3090 (GA102, subsystem `1043:8886`), `nvidia-open` 615.78.08, NVML 13.615.78.08. 2 fans, settable range **30–100 %**, policy 0 (temperature-controlled) on both. Thresholds: slowdown 95 °C, shutdown 98 °C, GPU max 93 °C, acoustic 65–91 °C (current 83). All of it is readable without root. |
| GPU 1 | RTX 2060 SUPER is bound to `vfio-pci` for a VM. NVML sees one GPU. The 2060's fans belong to the guest and cannot be reached from the host. |
| USB | No AIO cooler or fan hub. ITE IT8297 is Gigabyte's RGB controller (lighting only). |
| Displays | DP-1 and DP-2, 2560×1440 at 74.92 Hz, scale 1: one frame every 13.3 ms. |
| Shell look | `Style.fontFamily` is monospace unless a theme overrides it. The installed JetBrainsMono Nerd Font carries U+F0210, Material Design "fan". Shipped widgets draw Nerd Font glyphs (`iconText`), using `OpticalGlyph` for optical centring. The shipped `Ui` and panels animate with `Easing.OutCubic` at 120–260 ms. Candela reads Hyprland's `animations:enabled` as the reduced-motion preference. The four review themes (miasma, tokyo-night, catppuccin-latte, vantablack) are installed. |

Findings from the probes (see §9.3 for what they change):

- **The 3090 stops its fans by itself, and manual control cannot.** Sampled once a second at 36–39 °C: fans at 53 % / ~1000 RPM, then 0 % / 0 RPM a second later, all under the default policy. NVML refuses manual speeds below 30 %, so taking manual control of this card loses zero-RPM mode. *(M7, 22 Oct 2026: setup measured both fans in manual. GPU fan 1 stands still below 40 % and GPU fan 2 below 35 %, so manual 30 % stops them too, but only as a stall. Penna still hands a fan back to the driver below its floor, and the floor is now the measured stop, §4.5.)*
- **NVML's "target" is not what the driver is doing.** `nvmlDeviceGetTargetFanSpeed` read 30 % throughout, both while the fans ran at 53 % and while they were stopped. `GetFanSpeed_v2` (percent) and `GetFanSpeedRPM` tracked reality.
- **NVML percent is not duty.** It is a "percentage of the product's maximum noise tolerance fan speed" and "may exceed 100 %" (`nvml.h`).

---

## 1. Thesis

Linux can control fans, but it does not explain them. The kernel shows speed controls and RPM readings as unrelated numbered files. The numbering changes between boots, and the chip behind them may also be driven by the firmware. The existing tools either expose that raw (`fancontrol`, fan2go's YAML) or put a web dashboard in front of a root daemon (CoolerControl). Fan Control on Windows earned its following with one idea: each fan is a card, each curve is a card, and you wire them together in a window you can understand in a minute.

The bar of quality is not "has the knobs". A user opens it and sees every fan named for what it cools, with its RPM next to what it was asked for. They drag a point on a curve, hear the change, and never edit a config file. When something goes wrong — the daemon dies, a sensor disappears, the BIOS takes a fan back — the fans go somewhere safe, and the UI says which fan and why.

It has to do that on whatever Linux runs on: a workstation with two Super I/O chips, a ThinkPad whose fan has eight steps, a gaming laptop whose firmware runs its own curve. A new user should not need to know which. They press *Set up fans*, Penna measures every fan, proposes curves, and the machine is quieter a few minutes later. *Defaults* hands everything back to the BIOS and drivers at any time.

## 2. Principles

1. **Safe by construction.** No fan ever runs slower than the firmware would have run it unattended, except by a choice the user made with the consequence in view. Every path out of the daemon, including `kill -9`, hands control back (§6).
2. **The daemon owns the fans; surfaces only ask.** A shell crash, a closed window or a logout never changes a fan's speed. An unsaved edit is discarded when the client that made it goes away (§7.4).
3. **Physical truth.** Temperatures in °C, speeds in RPM, duty in % of PWM. Show what was asked for next to what was measured. When firmware overrides a control, say so; never show a value we wrote as if it were the fan's speed.
4. **Stable identity.** Things are named by hardware path and channel, never by `hwmonN` (§3.2). A device that is absent keeps its configuration.
5. **Theme native, keyboard first.** Candela's rules carry over: only `Color.*` and `Style.*` tokens, the shell's cursor model (j/k, h/l, Enter, Esc), and the pointer never produces a second highlight.
6. **Progressive disclosure.** The bar shows health. The popup has the daily controls. The studio has the curves. "Advanced" inside the studio has the last 10 %.
7. **Testable without hardware.** Control logic is pure. Every backend has a mock. The real sysfs backend runs unmodified against a fake tree (§9).
8. **Smooth, and measured.** Every surface keeps up with the display's refresh rate. Live values glide instead of stepping once a second, and nothing animates while nothing changes. The budgets in §8.6 are tested like the safety paths, not hoped for.
9. **Any machine, honestly.** Penna drives every fan the kernel lets it drive, in whatever way that hardware allows (§3.5), and says plainly what a machine can't do. What Penna knows about particular machines is data with a source (§3.6), never a code path for one board.
10. **Defaults are one step away.** Every surface can hand every fan back to its BIOS or driver, and that holds across restarts and reboots (§6.7).

## 3. Hardware model

### 3.1 Three kinds of thing

As in Fan Control, these are separate objects, related only by explicit pairing:

| Kind | What it is | hwmon | NVML |
|---|---|---|---|
| **Sensor** | A temperature in °C | `tempN_input` (millidegrees), `tempN_label`, `tempN_crit` | `nvmlDeviceGetTemperatureV` (the older `GetTemperature` is deprecated in NVML 13) |
| **Control** | Something we can set | `pwmN` (0–255) and `pwmN_enable` | fan index *i*: `SetFanSpeed_v2`, `SetFanControlPolicy`, `SetDefaultFanSpeed_v2` |
| **Tach** | An RPM reading | `fanN_input` | `GetFanSpeedRPM` for fan *i* |

hwmon does not say which tach a control drives: `pwm2` may well spin the fan on `fan4`, and one control can drive several fans through a splitter, or a fan with no tach (§5.2). NVML pairs control *i* with tach *i*.

### 3.2 Identity

| Source | Key | Example |
|---|---|---|
| hwmon | `hwmon/<name>/<device>/<channel>`, where `<device>` is the final component of the `device` link's real path | `hwmon/k10temp/0000:00:18.3/temp1` |
| NVML | `nvml/<gpu uuid>/<fan i \| temp>` | `nvml/GPU-168e2769-d673-f36a-ea7b-64df57f8d090/fan0` |
| Virtual | `virt/<user id>` | `virt/cpu` |
| Mock | `mock/<scenario>/<channel>` | `mock/render-burst/pwm2` |

When that final component is itself an enumerated class device, `<device>` is the bus device above it. NVMe is the case on this desk: its link ends in `…/0000:04:00.0/nvme/nvme1`, and `nvme1` is an enumeration number, so the key is `hwmon/nvme/0000:04:00.0/tempN`. *(Found building M0.)*

The label the user sees is separate, editable, and stored with the key. Default labels come from `tempN_label` when the driver gives one (`Tctl`, `Composite`). A configured key that is not present at startup — driver not loaded, GPU passed through to a VM — is kept, shown as absent with the reason, and never deleted. PCI display devices bound to `vfio-pci` are listed as "passed through to a VM", so the 2060 is visible but not offered.

### 3.3 Sanity

- A reading outside −40…150 °C is a failed read, not a temperature.
- A limit outside 40…130 °C is ignored (NVMe `temp_max` 65261.85 °C).
- `acpitz` and other sensors that read below 20 °C at idle are listed, but never chosen as a default source.

### 3.4 Backends

- **hwmon** — plain file reads and writes under a sysfs root that can be swapped (`/sys` in production, a temp directory in tests). Standard attributes are world-readable; writes need root.
- **NVML** — through `nvml-wrapper`, loaded at runtime so the daemon starts on machines without NVIDIA. The wrapper loads the unversioned `libnvidia-ml.so` by default; Penna asks for `libnvidia-ml.so.1` first and falls back to the unversioned name. `nvml-wrapper` 0.13 does not wrap `GetTargetFanSpeed`; we do not need it (§0).
- **Mock** — fake sysfs tree plus simulator (§9).
- **Platform profile** *(draft 3)* — the laptop-wide power profile, through power-profiles-daemon when it runs (§3.5).
- amdgpu is an hwmon driver like any other (`pwm1`, and `gpu_od/fan_ctrl/` on RDNA3 and later), so it arrives with the laptop drivers in M9, not in v2. *v2:* USB coolers and hubs through `hidapi`. None of these exists on this desk.

### 3.5 Kinds of control *(draft 3)*

Not every fan takes a duty. What a control accepts decides what Penna can do with it, and every surface says which kind a fan is (§8.3).

| Kind | The hardware takes | Curves | Penna writes |
|---|---|---|---|
| `duty` | Any duty, 0–100 % | Every curve type and every limit | When the value changes |
| `nvml` | Duty above a floor; the driver's curve below it | All, with hand-back below the floor (§4.5) | When the value changes |
| `speed` | A target RPM between the driver's minimum and maximum | All. % maps linearly onto that RPM range, and the card says what it means ("50 % = 3,000 RPM"). | When the value changes |
| `levels` | A few fixed steps | All, with the output rounded to a step and 3 °C of hysteresis between steps, so it never hunts | When the step changes, and every tick where a watchdog needs it |
| `firmware-curve` | N points the firmware runs by itself | One Graph curve per fan, on the firmware's own sensor, fitted to its points. No Mix and no virtual source. | Only when the curve changes. The firmware runs it, so it keeps running if Penna stops. |
| `modes` | Automatic, full speed, or a vendor's named modes | None: Full speed, a mode per profile, and Defaults | On a switch |
| `platform` | A machine-wide power profile | None: each Penna profile names one (§4.7) | On a profile switch |
| read-only | RPM, or nothing | None | Nothing |

**Drivers, as of Linux 7.3.** Checked against the kernel source and documentation on 21 Oct 2026, and against the READMEs of the out-of-tree drivers. Unless a row says otherwise, `pwmN_enable` is 0 = full speed, 1 = manual, 2 = automatic.

| Driver (hwmon `name`) | Kind | What matters |
|---|---|---|
| `it87`, and the frankcrawford fork | `duty` | Writing `pwmN` in automatic returns `EBUSY`. On the fork's Gigabyte H2RAM channels, writing 1 snapshots the firmware's fan settings and writing 2 restores them. |
| `nct6106` … `nct6799` (nct6775) | `duty` | `pwmN_enable` 2 = Thermal Cruise, 3 = Speed Cruise, 5 = Smart Fan IV. On the ASUS boards it lists (since 5.16), the chip is reached through ASUS's ACPI method, so there is no resource conflict. |
| `nct6683`, `nct6686`, `nct6687` | read-only | `pwmN` is writable only on a Mitac EC. The out-of-tree `nct6687d` makes MSI boards `duty` (enable 1/2), with `nct6683` blacklisted. |
| `f71808e` … (f71882fg) | `duty` | There is no enable 0. Writing `pwmN` outside manual returns `EROFS`. |
| `w83627ehf` family | `duty` | Enable 1–4; 5 only if it was the boot value. |
| `amdgpu` | `duty` up to RDNA3; `firmware-curve` with OverDrive | `gpu_od/fan_ctrl/fan_curve`: 5 points, speed in %, needs `amdgpu.ppfeaturemask` bit `0x4000`. Write `c` to commit and `r` to reset to the boot table. `pwm1` is not writable on RDNA4. |
| `nouveau` | `duty` | `pwm1` is 0–100 %, not 0–255. |
| `thinkpad` | `levels` (8) | Writes need `thinkpad_acpi fan_control=1`. `pwm1` maps to level `val >> 5` and reads back `level × 255 / 7`. Writes to `pwm1` are silently ignored unless enable is 1. `fan_watchdog` (0–120 s) returns the fan to automatic. A second fan can't be set separately. |
| `dell_smm` | `levels` (usually 3) | `pwmN` takes 0, 128 or 255. Reading it in automatic returns `ENODATA`. Enable is per fan (since 6.18), or one global write-only `pwm1_enable` on whitelisted models. |
| `asus` (asus-wmi) | `modes`; `duty` on its AGFN CPU fan | `pwm1_enable` reads the driver's cached state, not the hardware. GPU and middle fans take only 0 or 2. |
| `asus_custom_fan_curve` | `firmware-curve` (8 points per fan) | Points in °C, with `_pwm` stored as a percent. Enable 1 applies the curve, 2 switches it off, 3 loads the factory curve. Any point write switches the curve off until 1 is written again, and a platform-profile change switches it off too. |
| `hp` | `modes`; `duty` on Victus S and Omen (since 7.0) | 0 = full, 2 = automatic. In manual, reads come from the RPM, not from what was written. The driver renews manual mode every 90 s itself. |
| `acer` | `duty` on three models (since 6.19) | 0 = full, 1 = manual, 2 = automatic; stored as a percent. |
| `cros_ec` (Framework, Chromebooks) | `duty` when the EC supports it (since 6.18) | Enable 1 or 2 only. Writing `pwmN` in automatic returns `EOPNOTSUPP`. The EC goes automatic during suspend, and the driver restores it on resume. |
| `gpdfan` | `duty` | Manual first sets 255. |
| `lenovo_wmi_other` (since 7.0) | `speed` | `fanN_target` in RPM, rounded down to 100. 0 = automatic. |
| `applesmc` | `speed` | Up to 7.2: `fanN_manual` and `fanN_output`. From 7.3: `pwmN_enable` and `fanN_target`. No `pwmN` either way. |
| `alienware_wmi` | `modes` | `fanN_boost` (0–255) adds to the firmware's own speed. It is not a duty. |
| `msi-ec` (out of tree) | `modes` | `fan_mode` and `cooler_boost` on the platform device, not hwmon. |
| `legion_hwmon` (out of tree) | read-only, plus `platform` | Penna never writes its curve files. One written in the wrong unit hard-powered-off a Legion Pro 5, and some firmware keeps the table across reboots. |
| `system76_acpi`, `surface_fan`, `i915`, `xe`, `msi_wmi_platform` | read-only | |
| any `platform_profile` handler | `platform` | Below. |

**What the backends handle.**
- **Full speed reads back as "no control".** it87 and nct6775 report a manual channel at 255 as `pwm_enable` 0. After a 100 % write, read-back counts 0 as manual, not as `contested`.
- **Enable values differ by driver.** f71882fg and w83627ehf have no 0, thinkpad refuses 3, and cros_ec refuses 0. Restore writes back exactly what it read, as before. *Full speed* is always manual at 100 %, never enable 0.
- **Writing duty outside manual fails, or is ignored.** The failures are `EBUSY`, `EROFS`, `EINVAL` and `EOPNOTSUPP`; thinkpad ignores the write silently. Taking a control always sets manual first, and the write test (§5.1) catches the silent case.
- **Read-back maps.** thinkpad (levels), hp (from RPM), asus and acer (percent) and older it87 chips (odd values) read back other than what was written. The write test records the map.
- **Originals that can't be read.** dell_smm returns `ENODATA` in automatic, and its global enable is write-only; asus reports a cached state. For these, the driver table names the mode to restore (automatic), with its source. This is the one place Penna assumes a mode (§6.1).
- **Watchdogs.** On a ThinkPad, Penna sets `fan_watchdog` to 5 s and rewrites the level every tick, so a stalled or dead Penna hands the fan back to the EC with nothing else running. The EC's own fallback is automatic, at level 4 or above.
- **Firmware state that persists.** amdgpu's OverDrive table resets at boot, while ASUS curves and some Lenovo tables may not; nobody has verified it yet. `firmware.json` (§6.7) covers both cases.
- **Platform profiles.** When power-profiles-daemon or tuned-ppd owns `org.freedesktop.UPower.PowerProfiles`, Penna sets `ActiveProfile` (`power-saver`, `balanced`, `performance`) over D-Bus. Both daemons treat a direct write to `/sys/firmware/acpi/platform_profile` as the user's own choice, and would fight it. Penna writes sysfs only when neither runs. It is a power setting as much as a fan setting, and the card says so.
- **Module options the doctor names:**
  - `thinkpad_acpi fan_control=1`;
  - `amdgpu.ppfeaturemask` with `0x4000`;
  - `nct6683 force=1`;
  - the out-of-tree drivers;
  - Secure Boot signing for any DKMS module (§3.6).

**A driver Penna doesn't know** that has `pwmN` and `pwmN_enable` is treated as `duty` under the generic sysfs ABI. Its tier is at best Documented, and it is offered only after its write test passes.

**Penna never writes** a Super I/O chip's own curve registers (`pwmN_auto_point*`, Smart Fan IV), the legion curve files, or anything outside this table.

**In code.**
- `ControlKind` grows from `Pwm` and `Nvml` to these kinds, each with its range: the duty scale (255 or 100), RPM bounds, level count, or point count and sensor.
- The pure tick still computes a % per control. A per-kind stage after the limits turns it into an `Output`: `Duty`, `Level`, `Rpm`, `Curve`, `Mode` or `Profile`. Firmware curves are compiled when the configuration changes, not every tick.
- `penna-core/src/drivers.rs` holds a table keyed by hwmon `name`. Each entry gives the kind, what the hardware is for a person, the module options writes need, the source the entry was written from, and its tier. It is pure data, so it sits in the core beside the protocol that reports tiers. *(As built in M5; draft 3 placed it in `penna-hw`.)* The restore mode for originals that can't be read, and per-driver read-back rules, arrive with the drivers that need them (M9).

### 3.6 Machines Penna has not seen *(draft 3)*

Penna will run on machines nobody working on it has touched. Four things make that safe and understandable.

**Discovery is the kernel's.** Penna sees whatever hwmon drivers are bound, plus NVML, and never touches a chip itself (§11.2, call 1). The set of hwmon devices is re-listed every tick, which costs one `readdir`. When it changes — a driver is loaded, a dock or eGPU arrives, a GPU goes to a VM — the inventory is rebuilt without a restart. Controls that disappear become `absent`; controls that appear are listed under `firmware`.

**The doctor finds what is missing,** and names one action with its risk:
- **Board identity** from the world-readable DMI fields (`/sys/class/dmi/id/board_vendor`, `board_name`, `bios_version`, `product_name`), looked up in the board database.
- **Drivers:** which fan drivers are loaded, why one isn't (the kernel log's ACPI resource conflict, an unknown chip), and module parameters a driver needs for writes (`thinkpad_acpi fan_control=1`).
- **Secure Boot and lockdown.** An unsigned DKMS module, such as the it87 fork, does not load under Secure Boot unless it is signed with an enrolled key. Lockdown blocks nothing Penna does day to day (sysfs and NVML); it does block `--probe` below.
- **Other fan daemons,** as today.
- **When the database doesn't know the board,** `penna doctor --probe` (root) reads the Super I/O chip ID the way `sensors-detect` does: the vendor's entry sequence at `0x2E`/`0x4E`, then the ID registers. It is the only time Penna touches a port, and only when asked.

**The board database is data.** It lives in `/usr/share/penna/boards/*.toml`, one file per board, keyed by DMI vendor and board name. Each file holds:
- the driver and options known to work, and whether the driver loads at boot by itself;
- each header's name per channel, with a role hint (CPU cooler, pump, chipset fan, case fan);
- quirks;
- where every fact came from.

No entry is written from memory. Entries come from captures and setup runs on real machines, or name a public source. This desk's first entry names its headers from the it87 fork's sensors configuration for the GA-X570-AORUS-PRO, and setup (§5) confirms them:

| Chip | fan1 | fan2 | fan3 | fan4 | fan5 |
|---|---|---|---|---|---|
| IT8688E | `CPU_FAN` | `SYS_FAN1` | `SYS_FAN2` | `PCH_FAN` (chipset) | `CPU_OPT` |
| IT8792E | `SYS_FAN5_PUMP` | `SYS_FAN6_PUMP` | `SYS_FAN4` | | |

*(M5, 22 Oct 2026: pairing by hand confirmed the table. On both chips `pwmN` drives `fanN` and nothing else, and nothing is plugged into `SYS_FAN6_PUMP`.)*

The same file names `VRM MOS`, `PCH`, `PCIEX16` and `PCIEX8` temperatures. It also explains the cold `acpitz`: the firmware returns a fixed 290 K.

**Support tiers,** shown by the doctor and on each fan card:

| Tier | Means |
|---|---|
| **Verified** | The exit-path hardware test (§9.4) passed on real hardware with this driver. |
| **Captured** | A real machine's capture with this driver runs in the simulator. |
| **Documented** | Built from the driver's kernel documentation only. |

A fan on a Documented driver works, and Penna says that nobody has verified it yet.

**Captures** are how the tiers grow. `penna capture` needs no root. It records:
- the DMI board fields;
- the kernel version;
- the hwmon tree;
- the platform profile;
- the fan modules and their parameters.

It records no serial numbers. A capture, plus the results of a setup run, is what a new board entry is made from. Penna never uploads anything; sending a capture is the user's act.

## 4. Control model

### 4.1 Pipeline

Each control, each tick:

```
source  ─► curve ─► response ─► limits ─► output map ─► write if changed ─► read back
(sensor     (°C→%)   (hysteresis,  (min, max,   (% → 0–255 PWM,
 or virtual)          smoothing,    offset,       or NVML %)
                      step rate)    start/stop)
```

**% always means PWM duty** (NVML: NVML percent). Calibration (§5) never rescales it. It only feeds the limits and the RPM axis the editor draws. Duty converts to the 0–255 PWM value rounding up, so a floor is never read back below itself. *(M1: a property test caught rounding to nearest reading a 15 % floor back as 14.9 %.)*

### 4.2 Sources

- A **sensor**.
- A **virtual sensor**: `max`, `min` or `average` of sensors, or a `time-average` over N seconds of one sensor.

### 4.3 Curve types (v1)

| Type | Parameters | Notes |
|---|---|---|
| **Flat** | % | |
| **Graph** | points (°C → %), source | Linear between points, clamped beyond the ends, at least 2 points. The editor keeps % non-decreasing with temperature (§11). "Linear" is the two-point preset of Graph, not a separate type. |
| **Mix** | curves, `max` · `min` · `average` | Combines curve *outputs*: "case fans = max(CPU curve, GPU curve)". |
| **Trigger** | idle %, load %, on °C, off °C (off < on), source | Two-state, with built-in hysteresis. |

*v2:* Sync (follow another control with an offset), Target (PID to a temperature), RPM avoid zones.

### 4.4 Response (on the curve)

- **Response time** — exponential smoothing of the source with time constant τ. Zen's `Tctl` jumps several degrees on every boost; τ is how a CPU curve stops revving. The default is 5 s for CPU sources and 0 s for the rest.
- **Hysteresis** — the output rises at once and falls only after the temperature drops H °C below the point where it last rose. Then it follows the curve H °C behind the temperature, so it eases down instead of dropping H °C's worth at once. Default 3 °C, on Graph curves only. Option: both directions, where it moves in steps: only after the temperature has moved H °C either way, and then to the curve. A Trigger's hysteresis is its on/off gap. Flat and Mix curves take neither setting; a configuration that gives them one is refused. *(M8, 22 Oct 2026: dropping to the curve at once made the load check on this desk hunt. Balanced held CPU_FAN at 57 % while Tctl fell from 70.6 to 65.4 °C, then dropped it to 45 %, and Tctl was back at 70.5 °C 8 s later. That repeated every 31 s for 3 minutes, with the case fans swinging between 16 and 40 %. A plant fitted to those readings hunts the same way in a test, 45–57 %, and settles at 54 % with the output trailing.)*

### 4.5 Limits (on the control)

- **Min / max %**, **offset %**.
- **Step up / step down** in %/s (defaults 20 and 5).
- **May stop** — off by default. When off, the floor is the calibrated stop duty plus 5 %, or 30 % until the fan is calibrated. When on, outputs below the stop duty become 0. *May stop* is refused on a motherboard fan with no calibrated stop duty. Leaving 0 uses the **start %** kick for 2 s and then settles. The kick is the calibrated start duty, otherwise the larger of the floor and 50 %. *(M7, 22 Oct 2026: no fan runs below the lowest duty setup took it to. A fan measured down to that duty without stopping, such as a pump at 50 % or a header that races below 5 %, gets that duty as its floor instead of 30 %.)*
- **Entering and leaving safety.** Safety states and overrides apply at once. Leaving them, the fan falls back at the step-down rate. Overrides are clamped to the floor. An NVML override below 30 % on a fan with *may stop* hands the fan to the driver.
- **NVML floor.** Manual control cannot go below 30 % on this card (§0). With *may stop* on, an output below the floor **hands the fan back** to the driver policy (`SetDefaultFanSpeed_v2`). The driver's own curve then stops the fan when cool and still protects the GPU. Manual control resumes when the curve rises above the floor plus 5 %. This is the only way to keep zero-RPM, and it is observed behaviour, not a guess. *(M7, 22 Oct 2026: once setup has measured a GPU fan, its floor is also the stop duty: the 3090's fans stand still below 35–40 % in manual. Without *may stop* the floor keeps the 5 % margin above the stop.)*

### 4.6 Control states

Each control is in exactly one of these, and every surface renders the same set:

| State | Meaning |
|---|---|
| `firmware` | Not adopted. BIOS or driver in control. The default for every control on first run, and every control while the Defaults profile is active (§4.7). |
| `curve` | Following its assigned curve. |
| `measuring` | Setup is measuring it (§5). Emergencies and failed sources still win, as they do over `curve`. *(Draft 3.)* |
| `handback` | NVML below its floor, temporarily on the driver policy (§4.5). |
| `override` | A temporary fixed value (`penna set … --for`, or "Full speed" in the popup). |
| `failsafe` | A source failed or went stale. Running at the control's failsafe value. |
| `emergency` | A sensor crossed its emergency limit. 100 %. |
| `contested` | Something else keeps changing the control (§6.4). We have stopped writing to it, except in an emergency (§6.3). |
| `stalled` | Its paired tach reads 0 RPM while the duty should spin it (§6.3). Lands with pairing in M7; M1 only warns. |
| `absent` / `fault` | Not present, or a write failed and was handed back. |

### 4.7 Profiles

A profile assigns a curve (or `firmware`) to every control. Curves are shared between profiles, and one profile is active. Penna ships three: **Quiet**, **Balanced**, **Render**. Names are editable. Up to five show as pills in the popup; more go into a dropdown. Switching is instant through the popup or the CLI; automatic switching is v2 (§10). *(D2, §11.3: a profile shows as a pill once it runs at least one fan, so a new install shows Defaults alone. Defaults counts toward the five.)*

**Defaults** *(draft 3)* is a fourth, built-in profile. It assigns `firmware` to every control, can't be edited, renamed or deleted, and always comes first. Choosing it hands every fan back to its BIOS or driver and keeps it there across restarts and reboots (§6.7). Penna goes on showing every temperature and RPM. A new install starts in Defaults, and setup (§5) is what fills the other three.

On a machine whose only fan lever is the platform profile (§3.5), each Penna profile also names a platform profile. Through power-profiles-daemon: Quiet → `power-saver`, Balanced → `balanced`, Render → `performance`. Written directly, Quiet takes the handler's `quiet` or `low-power`. Defaults → whatever was set before Penna changed it.

## 5. Setup: measuring every fan *(draft 3)*

Setup is one automatic run that takes a machine from Defaults to proposed curves. It starts with *Set up fans…* in the studio, or `penna setup`. Every step is shown live, and *Stop* hands every fan back at once. Draft 2 had *Pair…* and *Calibrate…* as two separate tools; they are now steps of setup, and each can still be rerun for one fan from its card.

| Step | What happens | Time on this desk |
|---|---|---|
| 1. Idle sample | Watch every source for 30 s before any fan is taken. A source's idle is the lowest fifth of its readings, so something that starts during the 30 s doesn't count. | 30 s |
| 2. Write test | Take each control, write two values, read back (§5.1). | 5 s per control, all at once |
| 3. Pairing | Find which tachs each control turns (§5.2). | 6 s to settle, then 12 s per control |
| 4. Roles | Suggest what each fan does (§5.3). | — |
| 5. Calibration | Measure each fan's duty → RPM (§5.4). | about 3 min, all fans at once |
| 6. Load check | Optional: run the CPU flat out for 3 minutes on the proposed curve (§5.5). | 3 min |
| 7. Review | The user confirms roles and reviews the generated curves, which arrive as a draft (§5.5). | — |

Without the load check, a desk with eight controls takes about five minutes. *(Four until 0.7.3, which reads each step for longer, §5.4.)*

*(D2, §11.3.)* Setup opens on a start screen that says what will happen, how long it takes and what the guards are. The load check is its only option, on by default on a desktop and off on a laptop running on battery. While setup runs, its controls are `measuring` (§4.6) and the bar shows `fan-clock` (§8.1).

**Guards, for the whole run.** If a CPU source passes 85 °C, a GPU passes 80 °C, or any read fails, setup stops and every fan goes back to its original mode. The emergency rule (§6.3) is in force throughout. Setup wants an idle machine. If the 1-minute load is above a quarter of the CPU's threads, it says so and waits.

**A setup belongs to its connection,** like a draft (§7.4): if the studio closes, setup stops and hands everything back. `penna setup` holds its own connection, and Ctrl-C does the same.

What setup measures goes to `/var/lib/penna/measured.toml` (§7.5): measurements are facts about the hardware, not choices. What it proposes is a draft, and nothing is saved until the user saves it.

### 5.1 Write test

The control is taken as in §6.1, with its way back on disk first. Penna writes 60 % and then 80 %, reads back after 1 s each time, and watches for 3 s more. *(22 Oct 2026: each reading is checked against the duty the loop shows it wrote on the tick before, not against the time since the take. The daemon's clock is the wall clock, each tick wakes a little late by a different amount, and `--tick` runs from 0.25 to 5 s: counting whole seconds compared a reading with a duty not yet written, about one run in two.)* The result is one of:

| Result | Means | Then |
|---|---|---|
| `ok` | Reads back what was written, and stays manual. | Offered for curves. |
| `maps` | Reads back a different value, but the same one every time (fan2go's `setPwmToGetPwmMap`). | Offered; read-back compares against the map. *(M7: recorded, but not offered until the loop's read-back follows a map, with the drivers that need one, M9.)* |
| `steps` | Only some values stick: the hardware has levels. | Offered as `levels` (§3.5). |
| `reverts` | Back to the old value or mode within 3 s: firmware is driving it. This is the IT8792E report on Gigabyte X570 boards (§6.4). | Not offered. The card says what wrote over it, and the doctor says what can be done. |
| `refused` | The write fails. | Not offered, with the error. |

*(M5, 22 Oct 2026.)* After handing back, the test also reads every `pwmN_auto_*` file the driver shows and compares it with how it was found, 2 s later because it87 caches the chip for 1.5 s. It87's shared start-PWM register is covered by writing the duty back (§6.1). Any other difference means a manual write changes the firmware's own curve. The result is `ok` with a warning on the card, and Penna does not offer a generated curve for that control until the cause is in the driver table.

### 5.2 Pairing (hwmon only)

Every taken control is held at 60 % for 6 s, so each rise is measured from a steady speed; from the write test's 80 % the rises would be smaller. One control at a time is then raised to 100 % for 6 s and dropped to 40 % for 6 s. A tach pairs with the control when its highest reading in the 6 s at 100 % is at least 15 % and 150 RPM above what it read just before the raise. The drop to 40 % confirms: a tach that rose and then didn't fall is reported, not paired, and so is one that two controls move. *(22 Oct 2026, before the first run on this desk, whose CPU_OPT carries a liquid cooler's pump: the low step is 50 %, a pump's floor, since pairing runs before any role is known; and a tach that alone rose pairs on its rise, since a pump barely slows between 60 and 50 %. The fall decides only between tachs that both rose.)* A control can end up with several tachs, or none: a header with no fan, or a dead one. NVML and most laptop drivers pair control *i* with tach *i* and skip this step. *(D2, §11.3, call 5; draft 3 paired a tach that moved more than 25 % both ways from the controls' current duty.)*

*(M5, 22 Oct 2026: pairing by hand on this desk.)* From a 60 % hold, 100 % raised each fan by 31–59 %; the smallest rise was the PCH fan's, close to the 25 % above. The fans were still speeding up 4 s in. The first version of the by-hand test read each tach once, 5 s in, against RPMs recorded before the fans had come down from full speed, and it paired one control of eight. The version that worked waits for every fan to settle, measures each control from what the tachs read just before it, and keeps each tach's highest reading over 11 s. Setup's pairing (M7) needs the same three things, and the rule above has them.

### 5.3 Roles

Each fan gets a role, which decides its generated curve and how it is calibrated:

| Role | Suggested when | Calibration | Never |
|---|---|---|---|
| CPU cooler | The board database names the header `CPU_FAN` or `CPU_OPT`. | Full; stopped for at most 10 s at a time. | — |
| GPU | NVML or amdgpu. | % → RPM only. | — |
| Pump | The header is a pump header, **and** the fan turns above 2,000 RPM at 100 %. Never assumed from the header alone: a case fan on a pump header is common. *(22 Oct 2026: also a fan above 2,000 RPM on a CPU header, as a suggestion to confirm: liquid coolers put their pump on CPU_FAN or CPU_OPT.)* | No stop test. | Stops; goes below 50 %. |
| Chipset | The board database names it (`PCH_FAN`). | No stop test, unless the idle sample saw its firmware stop it: then full. | Stops, unless the idle sample saw its firmware stop it. |
| Laptop | The machine is a laptop and the fan is its system fan. | By level. | — |
| Case | Everything else. | Full. | — |

Until the user confirms the roles, a fan on a pump or chipset header is treated as one that must never stop. *(22 Oct 2026: so is any fan on CPU_OPT, the board database's `cpu-opt` hint, and any fan on a CPU header that turns above 2,000 RPM: each may be a pump, and is held at 50 % or more until confirmed.)* *Identify* on any fan card spins that fan to 100 % for 3 s and back, so it can be found by ear or by eye.

*(D2, §11.3.)* Every suggested role carries its reason, in words: "The board calls this header CPU_OPT", "On a pump header, but 1,730 RPM at 100 %: a case fan". *(M8: the speed a reason quotes is calibration's at 100 % once there is one. Pairing's 6 s at 100 % can end before a fan is up to speed: SYS_FAN5_PUMP read 1,450 RPM there and 1,819 calibrated.)* A fan on a pump header that is suggested as a case fan is held at 50 % or more, and measured no lower, until the user confirms it; confirming measures it below 50 % (about 30 s). One exception to the rule above: a chipset fan the idle sample saw its firmware stop may stop, because the firmware already does it. On this desk the BIOS stops PCH_FAN with the PCH at 45 °C; a never-stop curve would run it near 2,700 RPM. The review also asks for a name per fan, prefilled with its header.

### 5.4 Calibration

Per control, with its tach paired. Fans are calibrated all at once, each against its own tach. Air from one fan barely moves another's RPM, and running them together halves the time.

1. Step down from 100 % in 5 % steps. At each step, wait until RPM is stable within 3 % over 2 s (timeout 10 s) and record it. The lowest duty that still turns is the **stop duty**. Pumps and chipset fans stop the descent at 50 % and 30 %, and so does an unconfirmed fan on a pump header (50 %), except a chipset fan its firmware was seen stopping (§5.3).
2. From 0, step up in 2 % steps. The first duty that spins a stopped fan is the **start duty**, usually above the stop duty because of stiction. The time from that write to a steady RPM is the **spin-up time**. *(M7, as built: the climb starts at the stop duty, since a stopped fan never starts below it; from 0 it kept a CPU fan stopped past its 10 s. Steps are 2 s apart. "Stable" also needs the last two readings within 1.5 %: a fan still closing on its speed passed the 3 % reading low.)*
3. Store the duty→RPM table and make it monotonic, with the **maximum RPM**.

*(M7, after the first run on this desk, 22 Oct 2026.)* Three changes:
- **A duty counts from the first tick the loop shows it written.** The fan is taken on one tick and written on the next, and it87 caches the tach for 1.5 s. Counting from the request, a re-measure read three ticks of the firmware's own speed as steady and recorded 938 RPM at 100 %.
- **A step that turns the fan faster than the step above ends the descent there**, by as much as pairing calls a rise (§5.2). That step above becomes the fan's lowest duty, and the review says why. Written 0 %, this desk's SYS_FAN1 runs at full speed.
- **Calibration after a confirmed role carries on from where the run stopped.** It keeps what the run measured above that point.

*(M7, after the third run on this desk, 22 Oct 2026.)* "Stable" is judged on three readings 2 s apart. it87 reads the chip at most every 1.5 s, so on a 1 s tick every other reading repeats the last, and a repeat always passed "the last two within 1.5 %": a fan still slowing was read as steady. On the test rig this put CPU_FAN's points up to 4 % high. And a fan whose three readings all move one way at 10 s gets up to 30 s. This desk's SYS_FAN4 overshoots a raise: at 100 % it read 2,149 RPM, then 2,045, 1,934 and 1,854, still falling when the 10 s cap took it. A point now takes about 8 s, not 5, and calibration on this desk about 3 minutes.

NVML fans skip steps 1–2: Penna records % → RPM over 30–100 % for the axis only. `levels` controls record the RPM at each level.

Uses:
- the floor in §4.5;
- the RPM axis in the editor ("40 % ≈ 820 RPM");
- dead-zone warnings;
- generated curves (§5.5);
- drift (§5.6);
- later, RPM-mode control.

### 5.5 Generated curves

Setup proposes a curve for each role in each profile. Fans that share a role share its curve, so the studio stays readable: "CPU · Quiet", "Case · Quiet", "GPU · Quiet". Each fan's floor and maximum come from its own calibration.

**Anchors.** Each source has three temperatures:
- **idle**: from step 1;
- **target**: the temperature a profile tries to keep it under;
- **limit**: TjMax from the CPU table, the NVML GPU maximum, or a plausible `temp_crit`.

On this desk: Tctl idle about 45 °C and limit 90 °C. The 3090's target is NVML's acoustic threshold (83 °C) and its limit 93 °C.

**Points.** Points are placed in *airflow*, as a share of the fan's range from its floor to its maximum RPM. They are then turned into duty through the role's calibration. A curve therefore means the same thing on a fan whose RPM barely moves below 40 % as on one that is linear. % still means duty (§11, decision 4): the generator only chooses which duty.

| Source | | Quiet | Balanced | Render |
|---|---|---|---|---|
| CPU (τ 5 s, hysteresis 3 °C) | floor until | idle + 15 | idle + 10 | idle + 5 |
| | 50 % airflow at | limit − 15 | limit − 20 | limit − 30 |
| | 100 % at | limit − 3 | limit − 8 | limit − 15 |
| GPU (NVIDIA: *may stop*, so below the floor is the driver's curve and its zero-RPM) | driver's curve below | target − 10 | target − 15 | target − 20 |
| | 50 % at | target | target − 5 | target − 10 |
| | 100 % at | limit − 3 | limit − 5 | target |

On this desk, Balanced CPU comes out as: floor to 55 °C, 50 % airflow at 70 °C, 100 % at 82 °C.

- **Case fans** follow a Mix (max) of a CPU curve and a GPU curve. They stay at their floor until either source is 15 °C above idle, and their top is capped at 50 % airflow in Quiet, 75 % in Balanced and 100 % in Render.
- **Pumps** are Flat at 70 %, 85 % and 100 % duty.
- **Chipset fans** follow the chipset temperature when the board has one: floor to 65 °C, 100 % at 80 °C. Without that sensor they stay with their firmware. A chipset fan its firmware was seen stopping (§5.3) gets *may stop* and 0 % at 60 °C, so it rests below about 64 °C as it did before.
- **A laptop's fan** follows the larger of its CPU and GPU curves, rounded to its levels.
- **Firmware curves** are fitted to the firmware's own points, on its own sensor.

**Load check (optional, CPU only).** Penna loads every CPU thread with a busy loop at `nice 19` for 3 minutes, with the draft's Balanced curve live on the fans. If the CPU source passes limit − 5, the 100 % point moves 5 °C earlier and the check runs once more. The review shows the plateau: "Under 3 minutes of full load the CPU levelled at 78 °C, with its fans at 64 %." It is on by default on a desktop and off on a laptop running on battery. A GPU check would need a GPU workload Penna does not ship; the driver's own limits and NVML's thresholds stand in.

**Generated, then yours.** Once saved, generated curves are ordinary curves. Running setup again proposes a new draft. It never overwrites a saved edit unless the user saves the draft. The table above is the starting point to be tuned on real machines, not a promise; the load check is how it is held to account.

*(M8, as built, 22 Oct 2026.)*
- **A role's fans share its curve through their mean airflow.** The curve's duty gives them, on average, the airflow it was placed at. Its floor is the lowest of theirs, and the plan lifts each fan to its own (§4.5). Each fan's maximum is the lowest duty at which its monotonic table reaches 98 % of its top speed: this desk's SYS_FAN4 gets 70 %.
- **Five points per graph:** the floor, then 25, 50, 75 and 100 % airflow at temperatures spread evenly between the anchors. A GPU curve adds a 0 % point 1 °C below its floor, so below it the fan goes back to the driver.
- **Case fans** ramp from 15 °C above each source's idle to their cap, which they reach where that source's own curve of the same profile reaches 100 %.
- **The chipset curve is one curve for all three profiles.** Its only anchor is idle: its temperatures are fixed.
- **The GPU's target is NVML's acoustic threshold:** 83 °C on the 3090, which `nvidia-smi` calls its "Target Temperature Specification". Without one, the target is the limit less 10 °C.
- **Ids and labels:** `cpu-quiet`, "CPU · Quiet"; `case-balanced` mixes `case-balanced-cpu` and `case-balanced-gpu`. Generating replaces curves with these ids, the starter's `cpu-quiet` included, and leaves every other curve alone.
- **The draft keeps the profile that was on,** so it runs nothing until it is saved, and remembers what it was generated from. Run again, setup finds a profile it writes already on, Balanced as a rule: then the fans run the draft's curves while it is reviewed, and `penna setup` and the journal say so. Not saving puts back what was saved. Confirmed roles generate it again from there, so a role given up leaves no curve behind. `draft.generate` makes the same draft from measurements already saved (§7.3).
- **The load check** saves what setup measured before it starts. During it the Balanced profile runs on the draft; the review switches back to the profile that was on, and stopping setup takes the draft back. Passing the limit less 5 °C ends a pass in place of setup's 85 °C guard: the emergency rule (§6.3) stands behind it. A second pass waits until the CPU is within 10 °C of idle, or 2 minutes. Only Balanced's CPU curve moves. The plateau is the mean of the last 30 s. pennad loads its own threads, each at nice 19; the mock desk loads its simulated CPU. It is off for a run that measures only some fans again.
- Not yet: firmware curves fitted to a firmware's own points, and laptop fans (M9).

### 5.6 Drift

Once a fan is calibrated, Penna compares its RPM at the current duty with the table as it runs. A fan that turns more than 25 % slower than calibrated, steadily for 10 minutes, gets a `warn` event and a line on its card: "Front intake turns 30 % slower than when it was set up: dust, or a tired bearing." *Recalibrate* on the card measures it again.

## 6. Safety

The guarantees, in order of what they protect against.

### 6.1 Write-ahead hand-back

Before a control leaves `firmware`, the daemon writes its key and **original mode** to `/run/penna/held.json` (temporary file, `fsync`, atomic rename). Original mode is the exact `pwmN_enable` value — "automatic" varies by chip, so it is never assumed to be 2 — and, for NVML, the policy. *(Draft 3: the one exception is a driver that can't report its mode, such as `dell_smm`; its driver-table entry names the mode to restore, with a source, §3.5.)* Only then does it switch the control to manual. Adoption is bumpless: read the duty the firmware was running and start the curve's ramp from there. Taking an NVML fan records its policy and changes nothing else: manual mode starts with the first write, so a fan the driver has stopped is not spun up for a tick. Restoring a control writes back the duty it was found at, in manual mode, and only then its original mode. *(M5, 22 Oct 2026: draft 3 wrote the duty back only for a control found manual. On it87's newer chips, the IT8688E here, the manual duty register is also the automatic curve's start PWM, `pwmN_auto_start`, so the first hardware test left the BIOS curves starting at Penna's last duty, 60 % where the BIOS had 27 %. Had Penna last held a fan lower than the BIOS start, the curve would have started lower too. On every other driver an automatic mode takes over at once and the duty is a moment's value.)* The order is the guarantee, as in Candela: **a control is never manual without its way back already on disk.**

*(22 Oct 2026.)* The loop only steps the controls its plan names, so a new plan keeps every control Penna still holds, with nothing assigned, until it has been given back. Before this, a fan that only a studio's draft had adopted stayed manual at its last duty when the draft was discarded or its studio closed, until pennad stopped.

### 6.2 Every exit path restores

| Exit | What runs |
|---|---|
| SIGTERM, SIGINT, `systemctl stop` | The loop's shutdown path restores every held control, then removes `held.json`. |
| Panic | A panic hook restores from an immutable snapshot of `held.json` taken at adoption. No locks, no shared state. The hook runs under both `panic = "unwind"` and `"abort"`. |
| `kill -9`, OOM, a crash inside NVML | Nothing in the process runs. The unit's `ExecStopPost=penna restore` reads `held.json` and restores. `RuntimeDirectoryPreserve=yes` keeps the file past the process. |
| Hang | `Type=notify`, `WatchdogSec=5`. The **control loop itself** pings once per tick, not a helper thread, so a stuck loop stops pinging. systemd kills the daemon, runs `ExecStopPost`, and `Restart=on-failure` brings it back. |
| `systemctl kill` of the whole unit | `systemctl kill` signals every process in the unit's cgroup by default, and that includes the `ExecStopPost` systemd starts at the same moment. On this desk the journal said "Control process exited, code=killed", and the fans stayed manual. `Restart=on-failure` brings `pennad` back, and its first act is to restore the `held.json` left behind: the fans were back on the driver's curve about 2 s after the kill (§9.4). |
| Reboot | The firmware resets the chips. `/run` is tmpfs, so no stale state survives. |

### 6.3 Inside a tick

- **A failed source.** A source read fails, or returns something that is not a plausible temperature → that control goes to `failsafe` (default 100 %) on the same tick. Every source is read every tick, so there is no older value to fall back on. *(Draft 2 said "older than 3 s", which could never happen.)*
- **An emergency.** A watched sensor at or above its **emergency limit** → every adopted control goes to `emergency` (100 %) until that sensor falls 5 °C below the limit. The watched sensors are:
  - sensors with a plausible limit of their own: hwmon `temp_crit`, or the NVML GPU max (93 °C here);
  - every CPU sensor, at TjMax from a small table (90 °C for the 5950X; k10temp gives no limit);
  - any other sensor a curve reads, at 95 °C.

  Not every sensor is watched: a Super I/O chip's unconnected inputs commonly read 127 °C and would hold every fan at 100 % for ever.
- **An emergency outranks a contest.** A `contested` control is taken back and written at 100 % every tick while the emergency lasts, then returns to `contested`.
- **Adopted mid-emergency.** A control adopted during an emergency or a failed source gets 100 %, or its failsafe value, on the same tick.
- **A stalled fan.** A paired tach that reads 0 RPM for 3 s while the duty is at or above the stop duty plus 5 % gets one start kick at 100 % for 2 s. If it still reads 0, the control is `stalled`: urgent, the `fan-alert` glyph, and "isn't turning at 35 %". Penna keeps commanding the curve, and the emergency rule still protects. *(M1 only warns; the state needs pairing, M7.)*
- A write fails → the control is handed back to its original mode, enters `fault`, and is reported.
- Writes happen only when the value changes, at most once per control per tick.

### 6.4 Read-back and contested controls

After every write, read back `pwmN` and `pwmN_enable` (NVML: the policy). If the mode is no longer manual, or the duty is off by more than 2 %, for two ticks running, something else is driving the fan. That could be the BIOS, a chip "helper", a resume reset, or another tool. The daemon re-asserts up to three times in 30 s. After that it marks the control `contested` and stops writing. The UI then says "The BIOS (or another program) is controlling this fan", with what we wrote and what we read. This is the case the it87 community reports on Gigabyte X570 boards, where IT8792E PWM writes reverted within a second.

At start, `penna doctor` and the daemon refuse to adopt while `coolercontrold`, `fan2go` or `fancontrol` is active, and name the unit.

### 6.5 Resume

The daemon subscribes to logind `PrepareForSleep(false)`. On resume it re-reads every mode and re-asserts manual control where it held it. Read-back (§6.4) catches whatever this misses. Both suspend and hibernate have to be tested here (§13).

### 6.6 What we do to the chip

With `ignore_resource_conflict` the it87 README warns that "both ACPI and this driver may access the chip at the same time", which can cause "unexpected system reboots". Our part of that risk is the number of accesses: a 1 s tick for hwmon, no write when nothing changed, and one read pass per tick. Loading the driver that way stays the user's decision, explained by `penna doctor`. *(M5, 22 Oct 2026: on boards in the fork's DMI table, this desk's among them, the fork ignores the conflict by itself and loads at every boot. There, installing `it87-dkms-git` is the decision, and the doctor says so.)*

Windows has no equivalent of this check. LibreHardwareMonitor, and so Fan Control, reaches the chip through its own kernel driver whether ACPI uses it or not. On Gigabyte boards it also switches off Gigabyte's fan firmware on the second ITE chip before driving those fans: it writes 0 to a byte at offset `0x947` of that controller's memory, and restores the byte on exit. That firmware is why IT8792E writes revert on Linux (§6.4). Penna does not copy the trick into userspace (§11.2, call 1). If the it87 fork's own Gigabyte workarounds don't cover a board, the fix belongs in the driver.

### 6.7 Back to defaults *(draft 3)*

The Defaults profile (§4.7) is the one action. It is the popup's first pill, and it is also in the studio, `penna profile defaults` and `penna reset`. For each kind of control (§3.5), "default" means:

| Kind | Back to defaults |
|---|---|
| `duty` | The original duty, then the exact original `pwmN_enable` (§6.1). Penna never writes a chip's curve files (`pwmN_auto_*`). Where the duty register doubles as the curve's start, as on it87's newer chips, writing the duty back is what leaves the BIOS curve as the BIOS set it. |
| `nvml` | The driver's policy (`SetDefaultFanSpeed_v2`). |
| `speed` | Automatic: the original enable value, or a target of 0 where 0 means automatic (`lenovo_wmi_other`). |
| `levels` | The embedded controller's automatic mode. |
| `firmware-curve` | The curve Penna found, from a snapshot taken before its first write, or the driver's own factory reset where it has one. |
| `modes` | Automatic. |
| `platform` | The profile that was set before Penna changed it. |

**State that outlives a reboot.** Some laptop firmware curves and the platform profile survive a reboot. Before Penna first changes one, it snapshots it to `/var/lib/penna/firmware.json`, written ahead like `held.json`. Desktop chips and NVML reset at power-on, so `held.json` in `/run` is enough for them.

**Without the daemon.** `penna reset` (root) restores everything in `held.json` and `firmware.json`. It also sets the configuration's profile to Defaults, so a restarted `pennad` doesn't take the fans again.

**Without Penna.** Removing the package runs `penna reset` before stopping the service. A reboot resets desktop chips and NVML regardless.

## 7. Architecture

### 7.1 Processes

```
hwmon · NVML ─► pennad (root, systemd) ─► /run/penna/penna.sock (JSON lines)
                control thread + IPC      ├── penna            CLI, any user
                                          ├── Omarchy plugin   QML, in omarchy-shell
                                          └── standalone app   egui/wgpu (v2, other desktops)
```

**pennad** has two halves that never wait on each other:

- The **control thread** is a plain thread on a fixed schedule (1 s by default, 250 ms–5 s allowed). Each tick: drain pending commands from a bounded channel, read all sources, run the pure `tick`, write, read back, publish a snapshot, ping the watchdog. No async runtime.
- The **IPC side** is a `tokio` current-thread runtime that serves the socket. It reads snapshots from a `watch` channel and sends commands into the control thread's queue. A slow or hostile client can stall only itself.

The heart of `penna-core` is a pure function, tested with simulated time:

```rust
fn tick(cfg: &Config, prev: &LoopState, r: &Readings, now: Instant) -> (LoopState, Vec<Command>)
```

### 7.2 Workspace

```
penna/
  Cargo.toml          workspace
  crates/
    penna-core/       model, curves, response, limits, profiles, safety decisions; no I/O
    penna-hw/         Backend trait; hwmon, nvml, mock + simulator
    penna-proto/      IPC message types (serde); shared by daemon, CLI, egui app
    pennad/           daemon
    penna/            CLI
  plugin/             Omarchy shell plugin (manifest.json, QML, Model.js)
  design/             mockups and motion prototypes for the review (§8.6)
  packaging/          pennad.service, sysusers.d, tmpfiles.d, PKGBUILD
  test/fixtures/      captured sysfs trees (this desk), simulator scenarios
```

```rust
trait Backend {
    fn discover(&mut self) -> Result<Inventory>;                     // sensors, controls, tachs, with stable keys
    fn read(&mut self, out: &mut Readings) -> Result<()>;            // one pass per tick
    fn take(&mut self, c: &ControlKey) -> Result<Original>;          // enter manual; what to restore
    fn write(&mut self, c: &ControlKey, duty: Duty) -> Result<Readback>;
    fn release(&mut self, c: &ControlKey, to: &Original) -> Result<()>;
}
```

Dependencies: `tokio`, `serde`, `serde_json`, `toml`, `nvml-wrapper`, `sd-notify`, `zbus` (logind only), `tracing` + `tracing-journald`, `clap`. Peer credentials come from `tokio::net::UnixStream::peer_cred()`. hwmon is hand-rolled rather than `libmedium`: a few hundred lines with an injectable root, and the mock is then a plain directory.

Goals, to be measured at M4: under 5 MB RSS and under 0.1 % of one core at a 1 s tick.

### 7.3 Protocol (version 2, fixed for M2)

**Transport.** Newline-delimited JSON over a Unix socket: one UTF-8 object per line, at most 1 MiB per line. Clients look for the socket in this order:
1. `$PENNA_SOCKET`, and nothing else when it is set *(M8: pointed at a mock that isn't up, a client fell back to this machine's daemon)*
2. `/run/penna/penna.sock` (production)
3. `$XDG_RUNTIME_DIR/penna/penna.sock` (where `pennad --mock` listens by default)

`pennad --socket <path>` overrides where the daemon listens.

**Hello.** The daemon speaks first, once per connection:

```
← {"ev":"hello","v":2,"daemon":"0.2.0","mock":true,"tick":1.0,"can_write":true}
```

`can_write` is the peer-credential decision from §7.4. Surfaces disable the controls that change fans when it is false and say why.

**Requests and replies.** A request carries a client-chosen `id`; the reply echoes it. Events carry no `id`.

```
→ {"id":7,"op":"profile.set","name":"render"}
← {"id":7,"ok":true}
← {"id":8,"ok":false,"error":"There is no profile \"turbo\". Profiles: quiet, balanced, render."}
```

Errors are sentences a person can act on, in the voice of §8.

**Operations in M2.** A ✎ marks writes, which need `can_write`:

| op | Arguments | Result |
|---|---|---|
| `state` | — | One `tick` body (below), without the `ev` field |
| `subscribe` | — | `ok`, then one `history` event, then a `tick` every tick and an `event` for every transition |
| `unsubscribe` | — | `ok` |
| `config` | — | The active configuration as JSON, keys as in §7.5 |
| ✎ `profile.set` | `name` | Persisted to the configuration |
| ✎ `full_speed` | `on` (bool) | Runtime only |
| ✎ `override` | `control` (key, omitted = every control), `duty` (number, or `null` to clear), `seconds` (omitted = until cleared) | Runtime only |
| ✎ `retry` | `control` | A contested or faulted control tries again (§6.4) |
| ✎ `adopt` | `control`, `curve` | Assigns the curve to the control in the active profile; persisted |
| ✎ `release` | `control` | Unassigns it in the active profile, handing it back to firmware; persisted |
| ✎ `label` | `key`, `label` | Renames a sensor or control; persisted |

`pair` and `calibrate` arrive with pairing (M7).

**Operations added in M3 (the studio).**

| op | Arguments | Result |
|---|---|---|
| `studio` | — | The studio body (below) for this connection |
| ✎ `draft.put_curve` | `curve` (a curve object as in the studio body; a new `id` creates a curve) | The updated studio body |
| ✎ `draft.delete_curve` | `curve` (its id; `id` is the request's own field) | The updated studio body. Refused while a control in any profile uses the curve. |
| ✎ `draft.assign` | `control`, `curve` (id, or `"firmware"`) | The updated studio body. Applies to the active profile. |
| ✎ `draft.limits` | `control`, then any of `min`, `max`, `offset`, `step_up`, `step_down`, `may_stop`, `failsafe` | The updated studio body |
| ✎ `draft.save` | — | `ok` with `result.text`. Persists the whole draft (§7.5). |
| ✎ `draft.discard` | — | The studio body at the saved configuration |

A draft is validated like `penna check`; a refused change leaves the draft as it was and says why, e.g. "A curve can't fall as the temperature rises: 72 °C → 58 % is below 65 °C → 60 %." A valid draft applies to the fans at once (preview, §7.4). Only one connection may hold a draft at a time; a second one is refused with "Another studio has unsaved changes." When the connection holding a draft closes without `draft.save`, the daemon returns to the saved configuration and sends an `event` saying so.

**Studio body.** Everything static the editor needs, with the draft applied:

```
{"profile":"balanced",
 "profiles":[{"id":"balanced","label":"Balanced","assign":{"<control key>":"cpu-quiet","<key>":"firmware"}}],
 "curves":[{"id":"cpu-quiet","label":"CPU quiet","kind":"graph","source":"<sensor key>","source_label":"AMD CPU · Tctl",
            "points":[[45,30],[65,40],[78,65],[88,100]],"response":5.0,"hysteresis":3.0,"used_by":["<control key>"]},
           {"id":"case","label":"Case","kind":"mix","mix":{"op":"max","of":["cpu-quiet","gpu"]},"used_by":[…]},
           {"id":"idle","label":"Idle","kind":"flat","duty":35.0,"used_by":[]},
           {"id":"pump","label":"Pump","kind":"trigger","source":"<key>","trigger":{"idle":40,"load":80,"on":60,"off":52},"used_by":[]}],
 "controls":[{"key":"<key>","label":"CPU cooler","kind":"pwm","min":25.0,"max":100.0,"offset":0.0,"step_up":20.0,"step_down":5.0,
              "may_stop":false,"stop":20.0,"start":30.0,"floor":30.0,"failsafe":100.0,"nvml":null,
              "calibration":[[20,420],[30,610],…,[100,1800]]}],
 "sensors":[{"key":"<key>","label":"Tctl","device":"AMD CPU","virtual":false}],
 "draft":{"mine":true,"dirty":true,"curves":["cpu-quiet"],"controls":[]}}
```

- **`nvml`** is `{"min":30,"max":100}` for an NVIDIA fan, else `null`.
- **`calibration`** is the measured duty → RPM table (§5.4), or `null` before calibration. The editor draws no RPM axis without one.
- **`draft.mine`** says whether this connection holds the draft; `dirty` whether it differs from the saved configuration.

**Additions to `tick`.**
- **`curves[].hold_until`** is the temperature below which a held output may fall (§4.4), or `null` when the curve is not held.
- **`draft`** at the top level is `null`, or `{"curves":[…],"controls":[…]}` while some connection is previewing changes. The popup shows "Previewing unsaved changes" when it is set.

**As built in M3.**
- **A draft exists only while it differs from the saved configuration.** An edit back to the saved values drops it. So `dirty` is true whenever a draft exists; with none, the body shows `{mine:false, dirty:false, curves:[], controls:[]}` and the tick's `draft` is `null`.
- **`used_by`** lists the active profile's controls. Deletion is still refused if any profile uses the curve, and the refusal names each control with its profile.
- **Writes to the saved configuration during a draft.** `profile.set`, `adopt`, `release` and `label` apply to both the saved configuration and the draft. If one would conflict with the draft, it is refused: "That conflicts with a studio's unsaved changes: …".
- **A changed curve's hysteresis starts over,** on preview and on revert, so the change shows on the fans at once instead of being held.
- **`draft.put_curve` merges.** A field left out keeps what the curve had, so echoing the studio body back changes nothing. The source must be a sensor on this machine or a virtual one.
- **Studio `controls` lists every control on the machine,** including those no profile names, with their default limits.
- **`hold_until` is the anchor minus the hysteresis.**
- **`config` returns the running configuration,** so it includes a draft while one is previewing.
- **Events, all `info`:** previewing started, saved, discarded, and "The studio closed without saving; back to the saved configuration (…)".
- **Re-sent ticks.** Surfaces treat a tick that repeats the last `seq` as new state but not as a new second: no history, no sparkline step. Before this, the re-sent ticks during a drag (ten a second) cost 63 dropped frames in 10 s.

**Operations proposed in draft 3** (settled as they are built, M5–M9):

| op | Arguments | Result |
|---|---|---|
| `doctor` | — | The doctor's findings (§3.6) as JSON, so surfaces show them inline |
| ✎ `profile.set` | `name` may be `defaults` | As before; Defaults is built in (§4.7) |
| ✎ `identify` | `control` | 100 % for 3 s, then back |
| ✎ `setup.start` | `controls` (omitted = all), `load_check` (bool) | `ok`; then `setup` events. Bound to this connection (§5) |
| ✎ `setup.stop` | — | Every fan back to its original mode at once |
| ✎ `setup.roles` | `roles` (control → role) | Confirms roles; the generated draft follows them |
| ✎ `draft.generate` | — | The studio body, with generated curves as this connection's draft (§5.5). *(M8: always all three profiles.)* |

A `setup` event carries the step, the control being measured, live readings ("Raising PWM 3 to 100 % — fan 4 rose 610 → 1,790 RPM"), the guard temperatures and, at the end, the results. Ticks gain `controls[].kind` with the kinds of §3.5, `controls[].tier` (§3.6), and `controls[].drift`.

*(M8, as built.)* Setup's progress in the tick carries `load` while the load check runs (its pass, °C, the CPU fans' duty, and its result) and `proposed` once measuring is over: the curves setup generated, the anchors of their sources, the fans left alone and why, or why there is no draft. The studio's `draft` gains `generated`, the curves still as setup made them. Saving a generated draft switches to Balanced (§11.3 call 9). `$PENNA_SOCKET`, when set, is the only socket a client tries.

**`tick` event.** UI-shaped: everything a surface draws, with no second request.

```
← {"ev":"tick","v":2,"seq":812,"t":1792595449.2,
   "profile":"balanced",
   "profiles":[{"id":"quiet","label":"Quiet"},{"id":"balanced","label":"Balanced"},{"id":"render","label":"Render"}],
   "full_speed":false,
   "emergency":[{"sensor":"hwmon/k10temp/0000:00:18.3/temp1","label":"CPU · Tctl","celsius":94.5,"limit":90,"release":85}],
   "sensors":[{"key":"hwmon/k10temp/0000:00:18.3/temp1","label":"Tctl","device":"CPU","celsius":62.1,
               "scale":[20,95],"limits":[…],"virtual":false,"below_ambient":false,"error":null}],
   "controls":[{"key":"hwmon/it87/it87.2608/pwm1","label":"CPU cooler","kind":"pwm","fans":1,"rpm":1140,"rpms":[1140],
                "state":"curve","reason":null,"duty":51.0,"target":52.0,"trend":"rising",
                "curve":"cpu-quiet","curve_label":"CPU quiet","floor":30.0,"may_stop":false,"override":null}],
   "curves":[{"id":"cpu-quiet","label":"CPU quiet","kind":"graph","source":"hwmon/k10temp/0000:00:18.3/temp1",
              "smoothed":63.4,"out":47.0,"held":false,"error":null}],
   "unreachable":[{"id":"pci/0000:0a:00.0","label":"NVIDIA TU106 [GeForce RTX 2060 SUPER]","reason":"passed through to a VM (vfio-pci)"}],
   "notes":["Motherboard fans: none visible. No Super I/O driver (it87, nct6775, …) is bound."]}
```

- **`controls`** lists only controls the configuration knows. Every other control is listed under `firmware`, as `[{"key","label"}]`, so the popup can fold it.
- **`scale`** is the sensor's fixed sparkline range (§11.1, call 5): 20 °C up to its emergency limit, or its plausible limit, plus 5 °C, rounded to 5. Without either, 20–100 °C.
- **`rpm`** is the mean of the control's paired tachs, or `null` when it has none.
- **`state`** is one of §4.6. **`override`** is `{"duty":…, "until":…}`, or `null`.

**`history` event.** Sent once after `subscribe`, so a shell that has just restarted draws full sparklines on its first frame (§8.6, no blanks). It holds the last 300 ticks, oldest first, with `null` for a failed read:

```
← {"ev":"history","seq":812,"tick":1.0,
   "sensors":{"<key>":[61.2,61.4,…]},"controls":{"<key>":{"duty":[…],"rpm":[…]}}}
```

**`event` event.** The transitions the mock trace prints (§9), for notifications (v2) and the CLI:

```
← {"ev":"event","seq":107,"t":…,"level":"alert","control":"<key>","text":"Front intake: contested → emergency: …"}
```

`level` is `info`, `warn` or `alert`.

**As built in M2.** The shape above holds; these details were settled while building it.
- **Extra fields.** `hello` adds `why`, a sentence, when `can_write` is false. Write replies carry `result: {"text": …}`, the daemon's own sentence, which the CLI prints.
- **A repeated `seq`.** After a write, the daemon republishes the current `tick` with the same `seq`, so `state` shows the change at once. The hardware follows on the next tick.
- **An early `history` can be empty.** The socket opens before the first tick, so a bind failure changes nothing on the machine.
- **Labels.**
  - `sensors[].device` is the full device label ("AMD CPU", "NVIDIA GeForce RTX 3090"; "Virtual" for virtual sensors), and `emergency[].label` is "AMD CPU · Tctl". Surfaces shorten it.
  - Sensors arrive ordered CPU, GPU, motherboard, storage, other, virtual, with below-room-temperature last.
- **Controls.**
  - `fans` counts paired tachs: an unpaired control has `fans` 0 and `rpm` `null`.
  - `kind` is `null` for a configured control that is not present.
  - A control a profile assigns to `"firmware"` is listed under `controls` with state `firmware`, not under `firmware`.
- **Profiles and curves.** Profile labels come from the id in words. Profiles are ordered by known names (silent, quiet, balanced, normal, performance, render, full), then alphabetically. `[[curve]]` takes an optional `label`, defaulting to the id in words ("cpu-quiet" becomes "CPU quiet"). Physical sensor and tach names live in a `[labels]` table in the config (§7.5), which `label` writes to.
- **Persistence.** Writes re-serialise the TOML: comments are lost and a generated header is written. Under `--mock`, the scenario's config is copied fresh on every start.
- **Overrides.** `until` is Unix time and `seconds` is wall-clock time, even on a sped-up mock desk. `override` with neither `control` nor `duty` also clears the per-fan overrides.
- **Naming things.** Controls are matched by key, exact label (any case), or a unique label prefix; profiles and curves by id or label.
- **Without a daemon.** `penna watch --json` with no daemon emits M0's hardware snapshot, tagged `"ev":"hardware"`.

### 7.4 Authorisation and drafts

- The socket is world-connectable. Each request is checked against the peer's credentials: reads are open to any local user; writes need uid 0 or membership of the `penna` group, which is created by `sysusers.d`. polkit (the shell already ships an agent) is the v2 route if group membership proves awkward. Under `--mock`, the daemon's own uid may also write, so the whole stack runs unprivileged. Membership counts from `/etc/group` as well as from the connecting process's own groups, so `usermod -aG penna` applies at once, with no new login *(M6)*.
- **A draft belongs to its connection.** The studio's edits apply live to the fans for preview, and `draft.save` persists them. If the connection closes with a draft open — Esc, a closed overlay, a shell crash — the daemon reverts to the saved configuration. This is Candela's "the revert runs outside the shell", without a countdown: a bad fan curve is audible and caught by §6, not invisible like a dark display.

### 7.5 Persistence

| Path | Holds | Written by |
|---|---|---|
| `/etc/penna/config.toml` | Labels, virtual sensors, curves, control limits, profiles, active profile | Daemon only, atomically, on `draft.save` and `profile.set`. Hand edits while the daemon is stopped are fine; `penna check` validates them. |
| `/var/lib/penna/measured.toml` | Pairings, calibration tables, keyed by control and tach. *Draft 3:* write-test results, confirmed roles, idle temperatures, the load check's plateau | Daemon, after a confirmed pair or calibrate, or a finished setup |
| `/run/penna/held.json` | Write-ahead hand-back list (§6.1) | Daemon; read by `penna restore` |
| `/var/lib/penna/firmware.json` | Snapshots of firmware state that outlives a reboot: firmware curves, the platform profile (§6.7) *(draft 3)* | Daemon, ahead of the first change; read by `penna reset` |
| `/usr/share/penna/boards/*.toml` | The board database (§3.6) *(draft 3)* | The package |

```toml
version = 1
profile = "balanced"

[[sensor]]
key = "virt/case"
label = "Case"
max = ["hwmon/k10temp/0000:00:18.3/temp1", "nvml/GPU-168e2769-d673-f36a-ea7b-64df57f8d090/temp"]

[[curve]]
id = "cpu-quiet"
graph = { source = "hwmon/k10temp/0000:00:18.3/temp1", points = [[45, 30], [65, 40], [78, 65], [88, 100]] }
response = "5s"
hysteresis = 3.0

[[control]]
key = "nvml/GPU-168e2769-d673-f36a-ea7b-64df57f8d090/fan0"
label = "GPU fan 1"
may_stop = true        # below 30 %: hand back to the driver (§4.5)
failsafe = 100

[profiles.balanced]
"nvml/GPU-168e2769-d673-f36a-ea7b-64df57f8d090/fan0" = "gpu-balanced"
```

### 7.6 Packaging

`pennad.service` (`Type=notify`, `WatchdogSec=5`, `Restart=on-failure`, `ExecStopPost=/usr/bin/penna restore`, `RuntimeDirectory=penna`, `RuntimeDirectoryPreserve=yes`, `StateDirectory=penna`, `ConfigurationDirectory=penna`, plus the hardening that still allows `/sys` writes). Also `sysusers.d` for the group and a PKGBUILD. The it87 fork is an optional dependency that `doctor` recommends, never a hard one.

**As built in M6** (`packaging/`):
- **The unit.**
  - `RestartSec=1`, because the whole-unit kill in §6.2 relies on a quick restart.
  - `WatchdogSignal=SIGKILL`: the watchdog exists to get the fans back, not to leave a core dump.
  - `ExecStart=/usr/bin/pennad --quiet`: the journal gets changes (states, faults, emergencies) rather than a line every second.
  - Hardening: `ProtectSystem=strict`, `ProtectHome`, `PrivateTmp`, `NoNewPrivileges`, `ProtectKernelModules`, `ProtectControlGroups`, the `Restrict*` family, and native syscalls only. `/sys` and `/dev` stay reachable for hwmon and NVML.
- **The package.**
  - The PKGBUILD builds from the checkout and runs the whole test suite in `check()`.
  - It installs the binaries, the unit, the group, the docs, the MIT licence and the plugin (`/usr/share/penna/plugin`).
  - Its install script restarts `pennad` on upgrade and stops it before removal, which hands every fan back.
- **First start without a configuration.** `pennad` writes a starter to `/etc/penna/config.toml`: curves for what the machine has (each NVIDIA GPU, and the CPU's Tctl or Package sensor), `[[control]]` entries for NVIDIA fans with *may stop* on so zero-RPM survives adoption, and three profiles that assign nothing. Every fan stays with its firmware until the user adopts it in the studio.
- **`penna doctor`.** Read-only findings, each with what to do:
  - the daemon and whether this user can write;
  - motherboard fan drivers, naming the board and the kernel log's ACPI conflict;
  - other fan daemons;
  - NVML, and GPUs passed through to a VM;
  - contested or faulted fans.

  It exits 1 when something needs attention.
- **Installed on this desk on 21 Oct 2026** with `tools/install-desk.sh`. `pennad` runs as an enabled service using about 22 MB. Both 3090 fans are on the driver's curve at idle, as M4 showed. The desk's user changes profiles without root.

## 8. Surfaces

### 8.1 Bar widget (kind: bar-widget)

The Material Design fan glyph (U+F0210), drawn through `OpticalGlyph` like the shipped widgets. By default the widget shows the glyph alone; a widget setting adds one chosen temperature, e.g. `62°`. The glyph never spins. It changes shape with the state, and colour only reinforces the shape (§11.1, call 1):

| State | Glyph | Colour |
|---|---|---|
| Normal | `fan` U+F0210 | text |
| Full speed | `fan-chevron-up` U+F146E | accent |
| Timed override | `fan-clock` U+F1A3A | accent |
| `failsafe`, `emergency`, `contested`, `fault` | `fan-alert` U+F146C | urgent |
| Daemon unreachable | `fan-off` U+F081D | dim |
| Nothing adopted, or Defaults | `fan-auto` U+F171D | text |
| Setup running *(D2)* | `fan-clock` U+F1A3A | accent |

Wheel: none, because fans are not a volume.

### 8.2 Popup

Same chrome as the audio and bluetooth popups, 380 logical px wide.

- **Hero.** Active profile and one line: `Balanced · 4 fans · CPU 62 °C · GPU 41 °C`. When something is wrong, the line says what instead: `GPU fan 1 contested — the driver is controlling it`.
- **Profile.** ButtonGroup `Defaults · Quiet · Balanced · Render` *(draft 3: Defaults first, §4.7)*. In Defaults, the hero says so in words: `Defaults · the BIOS and drivers run every fan`. On a new install, which starts in Defaults, it adds `Set up fans…`. *(D2, §11.3: Defaults is set apart from the profiles by a wider gap, and choosing it needs no confirmation. A profile shows only once it runs at least one fan, so a new install has no pills, only the hero and `Set up fans…`.)*
- **Temperatures.** One row per source a curve uses: value plus a 60 s sparkline from the service's ring buffer.
- **Fans.** One row per adopted control: label, RPM, duty, curve name, and a state badge when not `curve`. Unadopted controls fold under `Firmware (n)`; absent and passed-through devices fold under `Not reachable (n)`, each with its reason. *(D2, §11.3.)* In Defaults every fan stays listed, with its RPM and who runs it (`BIOS` or `Driver`) in place of the curve, and no duty: on the IT8688E, while the BIOS runs a fan, `pwmN` reads the BIOS curve's start value, not the duty (M5). Before pairing the rows are tachs, named for their headers by the board database; the tick carries them (`tachs`, an additive protocol field). A control that turned no fan in pairing folds under `Empty header (n)`, by header name.
- **Actions.** `Full speed` (an override until pressed again), `Curves…` (opens the studio).

### 8.3 Studio (kind: overlay)

Summoned from the popup's `Curves…` and with `penna open`. A centred card about 1100 logical px wide, with its height set by its content. It is laid out like Fan Control: controls on the left, the curve editor in the middle, sensors on the right, action bar along the bottom. Narrower than 1100 px, the sensors column becomes a tab beside the controls.

- **Controls.** A card per control: label, live RPM and duty, state, assigned curve (dropdown), and Advanced (limits, may stop, failsafe, start %). `Adopt` and `Release` live here.
- **Curve editor.** X is °C, Y is duty %, with the control's calibrated RPM as a secondary axis on the right. Draggable points, the hysteresis band shaded, the stop and min lines drawn, and a live marker where the smoothed source and the output are now. The hand gets Candela's drag rules: position = press position + pointer travel, a 4 px threshold, and snapping measured in screen pixels. Keys: `[` / `]` select point, h/l move by 1 °C, j/k by 1 %, Shift ×5, `a` add, `x` delete.
- **Sensors.** Every sensor with a sparkline, and the virtual-sensor builder.
- **Action bar.** Profile dropdown, `Set up fans…` *(draft 3; it replaces `Pair…` and `Calibrate…`)*, `Discard`, `Save`. Hints at the far left in caption size.
- **Setup view** *(draft 3)*. It takes the editor's place while setup runs:
  - one row per fan, with its live RPM and the step it is on;
  - its duty → RPM table drawing itself point by point as calibration measures it;
  - the guard temperatures, and what is happening in words ("Raising PWM 3 to 100 % — fan 4 rose 610 → 1,790 RPM");
  - `Stop`, which hands everything back at once.

  It ends on the review: role chips per fan, with `Identify`, then the generated curves in the editor, marked *Generated*, ready to save or discard.

  *(As approved in D2, §11.3.)* The setup view takes the studio over: the editor's place shows a rail of the seven steps and one panel per step, the controls column one row per fan with its live RPM and its step in words, and the sensors column becomes the guards. Pairing shows every tach on one RPM scale with the one that moves in the accent. Calibration draws each fan's duty → RPM table in small multiples, each on its own scale. *Stop and hand back* sits where *Save* does. The review's roles are chips with their reason and a name field per fan; a dashed chip still needs the user's yes. The draft holds all three profiles, and each curve's anchors (idle, target, limit) are drawn on its temperature axis. *Save* keeps the whole draft and switches to Balanced; *Discard* leaves Defaults on and keeps the measurements.
- **Each card shows its kind and tier** *(draft 3)*. "8-point firmware curve, on the CPU sensor", "3 steps", "Documented: nobody has verified this driver yet". A fan whose write test came back `reverts` says what writes over it. *(D2, §11.3: only where they say something. Cards are grouped under a heading per device, which names the tier once: "IT8688E · motherboard · Verified", with one sentence under it when the tier is below Verified. A card adds a kind line unless it is plain duty or NVML. Drift is a line on the card with* Recalibrate*, not a badge; `stalled` is urgent.)*

The setup view, the Defaults pill, and kind and tier on the cards are new UI. They went through a visual review in real theme tokens before any QML (milestone D2, §10, §11.3), as milestone D did.

### 8.4 Plugin service (kind: service)

One `Socket` connection with a `SplitParser` per shell, shared by the widget, popup and studio. It holds the latest snapshot and a 5-minute ring buffer, and reconnects with backoff. The studio opens its own connection, so its draft is bound to it (§7.4).

**Found building M2** (shell from Omarchy 4.0.5, Quickshell 0.3.2):
- **No built-ins at a plugin script's top level.** In the shell, built-ins such as `String` are not reachable while a plugin script initialises. `String.fromCodePoint` at the top of `Model.js` left every constant after it undefined, so every function using one threw — about 110 warnings a second. Glyphs are therefore surrogate-pair literals, and nothing at a script's top level calls anything. The same call works in plain Qt 6.12, so this is the shell's script context, not Qt.
- **Script changes need a shell restart.** The shell reloads a plugin when its entry under `~/.config/omarchy/plugins` changes, and it doesn't see edits inside a symlinked checkout. It also keeps a plugin's scripts in memory across that reload, even after `Qt.clearComponentCache()`. After editing a script, run `omarchy-restart-shell`.
- **Cost.** With the popup open against the mock desk, the whole shell process used about 2 % of one core; closed, 1–2 %, the same as before Penna. Frame timing proper is M3's test.

### 8.5 CLI

```
penna state                        # JSON snapshot
penna watch [--json]               # live table, or the event stream
penna profile <name>
penna set <control> <percent|auto> [--for 60s]
penna adopt <control> | release <control>
penna setup [<control>…] [--no-load-check]   # draft 3: §5, live in the terminal; Ctrl-C hands back
penna identify <control>           # 100 % for 3 s
penna pair [<control>] | calibrate <control>   # single steps of setup, for one fan
penna profile defaults             # every fan back to BIOS or driver, and kept there (§6.7)
penna check [<file>]               # validate a config without applying it
penna restore [--from <held.json>] # root; ExecStopPost
penna reset                        # root; Defaults without the daemon, firmware snapshots included (§6.7)
penna doctor [--probe] [--json]    # driver bound? ACPI conflict? Secure Boot? board known? other fan daemons? vfio GPUs? contested controls?
penna capture <dir>                # read-only snapshot for fixtures: hwmon, DMI board fields, kernel, fan modules (§3.6)
penna waybar                       # one JSON line for a Waybar custom module (non-Omarchy desktops)
```

### 8.6 Quality bar

"Elite and smooth" is defined here as things that can be checked. The rules apply to every surface, the standalone app included.

**Frame budget.** Every surface keeps up with the display's refresh rate: 13.3 ms at this desk's 74.92 Hz, and the design must still hold at 144 Hz (6.9 ms). The test is a scripted session against `pennad --mock` with Qt's scene-graph timing logs on: open the popup, open the studio, 30 s of live data, a 10 s point drag. It passes when no frame is dropped and the 99th-percentile frame stays under half the budget.

**The tick is invisible.** The daemon decides once a second (§11, decision 8); the UI must not look like it.

- **Geometry glides.** Bars, the curve marker and sparklines move linearly from the shown value to the new one over one tick interval, driven by `FrameAnimation`. When the next tick lands they retarget from wherever they are, so consecutive ticks join into continuous motion instead of a stutter of separate eases.
- **Text eases and rounds.** Text eases over 140 ms and is rounded to what a person reads: whole °C and RPM to the nearest 10 in the popup, 0.1 °C in the studio inspector. A value sitting on a rounding boundary does not flicker: the shown number changes only when the value moves 0.6 of a step past it.
- **Sparklines slide.** A sparkline appends one point per tick. Between ticks its path stays fixed and slides by a transform under a clipping parent, so no frame rebuilds any geometry.
- **The marker rides the curve.** The UI interpolates the temperature and evaluates the curve at it, instead of drawing a straight line between two output points.

**Nothing moves when nothing changes.** No spinning fans, breathing glows or idle shimmer. The bar never animates continuously: its text changes at most once per tick, and only when the shown integer changes. With no surface open, a desk at steady state costs the shell no frames beyond those once-a-tick changes.

**Input is local.**

- A dragged point follows the pointer on the next frame, from local state.
- Drafts go to the daemon at most 10 times a second, plus the final value on release. The daemon's echo never moves a point under the hand.
- A profile switch shows at once, and is confirmed or rolled back, with the reason, on the next tick.

**Rendering rules.**

- **Shape, not Canvas.** Plots are `Shape` items with `preferredRendererType: Shape.CurveRenderer`. `Canvas` draws on the CPU and repaints the whole item on every change. That suits Candela's still gamut plot, not a plot that moves every frame.
- **No live effects.** No `layer.enabled`, `MultiEffect`, blur or shadow on anything that updates live.
- **Crisp lines.** Hairlines sit on device pixels (`Style.spacing.hairline`) and positions are rounded, so grid lines stay crisp.
- **Numbers never shift sideways.** The shell font is monospace by default; if a theme sets a proportional one, `font.features: { "tnum": 1 }` keeps figures tabular. Value fields are fixed-width, so going from 999 to 1000 RPM never reflows a row.
- **Nothing jumps.** Badges and state lines reserve their space, or grow with `Behavior on height` (140 ms).

**Motion vocabulary.** The shell's own, so Penna moves like the popups around it:

- `Easing.OutCubic` for everything.
- 120–140 ms for state and colour changes, 180 ms for reveals, 260 ms for a surface opening.
- State colours (urgent, accent) cross-fade over 140 ms.
- Reduced motion follows Hyprland's `animations:enabled`, as Candela does: values snap, sparklines step, nothing glides.

**No waiting, no blanks.**

- **The popup is never empty.** It has content on its first frame, because the service already holds the snapshot and five minutes of history. No spinner, no empty sparkline.
- **The studio is fast.** Its first frame lands within 100 ms of the summon. Pairing and calibration load behind `Loader { asynchronous: true }`.
- **Old data is labelled, never zeroed.** When the daemon is unreachable, surfaces show the last known values dimmed, with their age (`12 s ago`).
- **Empty states are designed.** With no motherboard fans visible, the studio says why and shows `penna doctor`'s finding inline (driver not bound, ACPI conflict, what to install), not a blank column.

**The editor explains itself.**

- Hovering a point shows `72 °C → 58 % ≈ 1240 RPM` beside it, placed so it never covers the point.
- The marker leaves a 10 s trail. While hysteresis holds the output, the marker sits inside the shaded band, visibly off the curve: that is the answer to "why isn't it slowing down?".
- When the step rate limits the output, the inspector says so: `Rising 5 %/s → 64 %`.

**Measured in M3.** `tools/frame-timing/run.sh` runs the session above against `pennad --mock` on the second screen. It records frame intervals per phase with a `FrameAnimation`, and the render thread's own `sync + render` time with `QSG_RENDER_TIMING`. On this desk (threaded render loop, NVIDIA 615, 74.92 Hz, two runs):

| Phase | Frames | Interval p50 / p99 / max | Dropped |
|---|---|---|---|
| Popup, 5 s | 381 | 13.3 / 13.8–13.9 / 15.0 ms | 0 |
| Studio, 30 s live | 2,308 | 13.1–13.2 / 13.7–13.8 / 16.6 ms | 0 |
| Drag, 10 s | 771 | 13.1 / 13.8–13.9 / 15.1 ms | 0 |

The render thread took 1 ms at p99 and 6 ms at most, against the 6.7 ms half-budget. The studio's first frame came 77–82 ms after the summon (budget: 100 ms).

The first run failed, with 0.3–1 % of frames taking two refresh cycles, and every slow frame came 2–30 ms after a tick. Three causes, all fixed:
1. **A Repeater given a fresh array every tick rebuilds every delegate.** That meant every card, sparkline and number, each tick, which also silently reset every glide. Repeaters are now keyed by lists of ids that change only when the set of rows does, and each delegate looks up its row by key.
2. **Re-sent ticks after writes.** The daemon re-sends the current tick after each write, and the surfaces were treating each one as a new second (§7.3, as built in M3).
3. **Nine five-minute sparklines rebuilt in one frame.** The studio now rebuilds one per frame after each tick. Each glides from its own moment, so the stagger is invisible.

**Review before code.** As with Candela, the design is reviewed before any QML is written (milestone D, §10). The review covers mockups in real theme tokens across miasma, tokyo-night, catppuccin-latte and vantablack. It also covers motion prototypes of the three live behaviours: value glide, sparkline slide and marker ride.

## 9. Building without hardware

### 9.1 Mock tree and simulator

A scenario generates a fake sysfs tree (`hwmonN/{name, tempN_input, pwmN, pwmN_enable, fanN_input}`), and a simulator updates it in place:

- **Fans.** RPM follows duty through a model with stop duty, start duty, maximum RPM and a first-order lag (τ ≈ 2 s).
- **Heat.** `C·dT/dt = P(t) − k·airflow·(T − T_ambient)`, with a load script (idle → render burst → idle).
- **Faults, on a timeline.** A sensor disappears; a read returns `EIO`; `pwm_enable` flips to 2 (firmware takes back); a tach stays at 0 at 60 % (dead fan); a resume resets every chip; the NVML GPU is lost.

The **real hwmon backend runs against this tree unmodified**, so the code that writes `/sys` is the code under test. The NVML mock implements the same trait and reproduces §0: the 30 % floor, zero RPM under the driver policy, and percent that is not duty.

### 9.2 Tests

- **Property tests** on `tick`: the output is always within limits or in a declared safety state; it never goes below the stop duty unless *may stop* is set; any failed read reaches `failsafe` within one tick; the set of restored controls equals the set adopted, on every exit path.
- **Exit-path tests.** Run `pennad --sysfs-root <tmp>` as an ordinary user, `kill -9` it, run `penna restore`, and assert every `pwmN_enable` is back to its original value. The same for panic and watchdog timeout.
- **Fixtures** from this desk via `penna capture`: the missing `hwmon1`, the bogus NVMe limits and the cold `acpitz` are all part of it.
- **Frame timing** (§8.6): the scripted session against `pennad --mock`, run on the desk, with the budgets asserted from the timing log. Run it before every release and after any change to a live surface.

### 9.3 What §0 changed

The NVML floor and hand-back (§4.5), "percent is not duty" (§3.1, §4.1), absent-but-kept devices and the vfio listing (§3.2), the sanity rules (§3.3), never addressing anything by `hwmonN` (§3.2), and resume after hibernate (§6.5).

### 9.4 The M4 hardware test (21 Oct 2026)

`tools/m4-hardware-test.sh`, run as root through `pkexec`, drives the real 3090 with `test/fixtures/desk-3090.config.toml`. `pennad` runs as a transient systemd service with `Type=notify`, `WatchdogSec=5` and `ExecStopPost=penna restore`. Each case holds both fans at 45 %, ends the daemon one way, then reads both fans' policy back through NVML. All six cases pass on the second run:

| Case | The fans went back to the driver through |
|---|---|
| `systemctl stop` (SIGTERM) | `pennad`'s own shutdown path |
| `kill -9` of `pennad` alone | `ExecStopPost`, in under a second ("restored … the driver's curve", for each fan) |
| `kill -9` of the whole unit | the restarted `pennad` restoring the leftover `held.json`, about 2 s after the kill |
| Panic | the panic hook |
| Hang | systemd's watchdog after 5 s, then `ExecStopPost` |
| Idle | the curve below 30 %: the driver keeps the fans and stops them; a 6 s hold took them and gave them back |

The first run failed the `kill -9` case. It used `systemctl kill`, which killed `ExecStopPost` too, and that is what led to the whole-unit row in §6.2. The test now kills only the main process for that case and covers the whole-unit kill separately, with `Restart=on-failure`.

Also seen on the card:
- **NVML percent is not duty.** At a 45 % target the fans reported 46–64 %, at 703–1,464 RPM, and the two fans differed.
- **The driver takes a few seconds to stop the fans after a hand-back.** It ran them at about 50 % for a few seconds after each one, then stopped them at idle.
- **The first write takes over without a jump.** Taking does nothing on the card, so a fan stopped by the driver stays stopped until the first write.

### 9.5 Many machines *(draft 3)*

There is one desk to test on, and Penna has to run on many machines. Coverage comes in the three tiers of §3.6:
- **Documented.** For every driver Penna knows, a fixture tree is written from that driver's kernel documentation, with its quirks: levels, read-back maps, enable values, firmware curves and their sensor. The simulator models each kind of §3.5, so setup, generated curves and every exit path run against each driver's tree in `cargo test`.
- **Captured.** Real captures go to `test/fixtures/machines/<board>/`, with the board entry they produced. Each one runs the same tests.
- **Verified.** A driver is Verified when the §9.4 hardware test has passed on real hardware with it. This desk verifies `nvidia` (M4) and `it87` (M5: the fork, on the IT8688E and IT8792E).

## 10. Scope and milestones

| v1 | v2 |
|---|---|
| hwmon and NVML backends, mock and simulator; every kind of control in §3.5, amdgpu and the laptop drivers included *(draft 3)* | USB coolers and hubs via `hidapi` |
| Flat, Graph, Mix, Trigger; virtual sensors; response and limits | Sync, Target (PID), RPM avoid zones, RPM-mode control |
| Setup: write test, pairing, roles, calibration, generated curves, CPU load check, drift *(draft 3)*; contested detection, every safety path in §6 | Automatic profile switching (a process running, e.g. a Blender render or Brush training → Render; AC power; time of day) |
| Profiles switched by hand, and Defaults *(draft 3)* | GPU load check (needs a GPU workload) |
| Doctor, board database, support tiers, captures *(draft 3)* | Standalone egui/wgpu app; shell notifications on `contested` / `failsafe` |
| CLI, Omarchy plugin (bar, popup, studio), PKGBUILD | |

Milestones, each ending in something visible:

- **M0** — `penna watch` and `penna capture` on this desk, read-only, no root: k10temp, NVMe, and the 3090's temperature, two fans, percent and RPM. **Done 21 Oct 2026**: 28 tests, clippy clean, fixture in `test/fixtures/desk-2026-10-21`.
- **M1** — `penna-core`, mock, simulator and property tests. `pennad --mock` runs unprivileged. **Done 21 Oct 2026**: 86 tests, including 4 property tests at 400 cases each and exit-path tests for `kill -9`, SIGTERM, panic and a leftover `held.json`. The simulator has `render-burst` and `faults` scenarios. Mock mode runs on virtual time (`--speed`). `pennad` and `penna restore` refuse the real `/sys` until M4/M5.
- **D** — Visual design review: mockups in real theme tokens and motion prototypes (§8.6), sourced in `design/`. No QML is written before it is approved. **Approved 21 Oct 2026** (§11.1).
- **M2** — Protocol and CLI against the mock. The plugin's service, bar widget and popup against the mock daemon. **Done 21 Oct 2026**:
  - protocol v2 over the daemon's socket, with peer-credential authorisation and slow clients dropped;
  - the daemon-backed CLI and `penna waybar`;
  - 107 Rust tests and 8 plugin logic tests;
  - the popup running live in this shell against `pennad --mock`.
- **M3** — Studio and curve editor against the mock, with the §8.6 frame-timing test passing. **Done 21 Oct 2026**:
  - drafts bound to their connection, previewing live and reverting on close;
  - a studio with control cards, an editor for Graph, Trigger and Flat curves, a Mix view, and sensors over 5 minutes;
  - frame timing passing twice in a row (table in §8.6);
  - 114 Rust tests and 16 plugin logic tests.
- **M4** — Real NVML control of the 3090 as root: hand-back, exit paths, `kill -9` on real hardware. **Done 21 Oct 2026**:
  - production `pennad` requires root and refuses motherboard fans until M5;
  - real NVML take, write, hand-back and restore, with `penna restore` for real NVIDIA entries;
  - the hardware test passes all six cases (§9.4);
  - 124 Rust tests.
- **M5** — *Redefined in draft 3:* **every fan on this desk, and the kinds of control.**
  - The kinds of §3.5 in `penna-core`, the backends and the protocol.
  - Real `duty` control of the motherboard fans through the it87 fork, with the §9.4 hardware test extended to them.
  - The write test (§5.1) and contested detection on the IT8792E.
  - Rediscovery when hwmon devices change (§3.6).
  - The Defaults profile, `firmware.json` and `penna reset` (§6.7).
  - The doctor with the board database (this desk's entry first) and support tiers.
  - Draft 2's M5 named pairing and calibration as well; they move to M7 as steps of setup.

  **Done 22 Oct 2026.** Built and tested against the simulator (147 Rust tests, 18 plugin logic tests), then on this desk in two hardware runs:
  - **The kinds.** `ControlKind` is `Duty { scale }`, `Nvml`, `Levels`, `Speed`, `FirmwareCurve`, `Modes` or `ReadOnly`, and the protocol reports it with a tier for every control. hwmon gives each channel its driver's kind (§3.5), and a duty channel needs a `pwmN` whose mode bits allow writes plus a `pwmN_enable`. Undrivable kinds are listed with the reason and never written; adopting one is refused with that reason.
  - **Real control.** Production `pennad` opens hwmon for writing; the M4 guards are gone. Full speed read back as `pwm_enable` 0 is not a contest. Read-back tolerance is 2 % of the control's own scale.
  - **`held.json` names real paths.** Entries name the device's real path, not `class/hwmon/hwmonN`, so a renumbered device can't receive another's restore.
  - **Rediscovery.** Each tick lists `class/hwmon` with its link targets; any change rediscovers the hwmon backend and plans again. Held entries follow their control to its new files by key. A held control that goes away raises an alert. A control taken again keeps the originals it was first found with.
  - **A device that is gone is not a failed restore.** Its entry waits in `held.json`, across a clean stop and a restart. The next `pennad` keeps it, and puts the control back as it was found when the device returns. Before this, an unloaded driver would have made the next start refuse to run.
  - **Defaults** is built in (§4.7). It is first in every profile list, a new install starts there, adopting or releasing in it is refused, and the popup says "Defaults · the BIOS and drivers run every fan".
  - **`penna reset`** goes through `pennad` when it runs. Without it, as root, `penna reset` restores `held.json` and saves Defaults. Removing the package runs it first.
  - **The board database** loads from `/usr/share/penna/boards` by DMI (`PENNA_BOARDS` or `pennad --boards` for another directory). This desk's entry names its tachs and temperatures, drops the unconnected `EC_TEMP` inputs and the fixed-290 K `acpitz`, and gives the doctor the driver to load and its risk. Until pairing names them, hwmon controls are called by chip and channel ("IT8688E pwm1"), because two chips both have a `pwm1`.
  - **The doctor** reports each fan driver's tier, says why a driver's fans can't be driven, checks Secure Boot when a DKMS module is the answer, and doesn't ask a laptop for a Super I/O chip.
  - `tools/m5-hardware-test.sh` holds every it87 channel at 60 %. It ends `pennad` in the five ways of §9.4 and checks every `pwm_enable` comes back exactly. It also watches 40 s for channels that revert (the IT8792E question), checks full speed, and spins one control at a time to see which header it drives (`pairing` runs that step alone).

  **The hardware test, first run (22 Oct 2026).** `it87-dkms-git` 358.bd7a566 loaded with `ignore_resource_conflict=1`: the IT8688E at 0xa40 and the IT8792E at 0xa60, the second through Gigabyte's EC (H2RAM, SmartFan G2). Rebuilt from the journal, because the run's output stayed in another terminal:
  - **All five exit paths handed every channel back:** stop, `kill -9`, a whole-unit kill (`ExecStopPost` was killed too, and the restarted `pennad` restored what was left), panic, and the watchdog.
  - **Every channel held for 40 s, the IT8792E's three included.** None was marked contested, so with this fork the second chip's firmware does not write over Penna (the §6.4 report predates its Gigabyte workarounds).
  - **Full speed raised no false contest.**
  - **A hand-back bug: the mode went back, but not the duty.** On the IT8688E that duty is the BIOS curve's start PWM. Fixed: the duty goes back first (§6.1). The test now checks every `pwmN_auto_*` and the start PWM read as `pwm`, and writes `target/m5-hardware-test.log`.
  - **The fork's hwmon names carry Gigabyte's SIV id** (`it8688_4008090b`, since c8d2038 in Sep 2026), so the board database didn't match. Fixed: an entry's chip matches its name with or without the id. Keys carry the full name, as sysfs does.

  **The second run, after a reboot (22 Oct 2026).** The reboot put the BIOS's start PWMs back (70 of 255, 27 %). From `target/m5-hardware-test.log` and `target/m5-pairing.log`:
  - **The fork now loads at every boot by itself.** Its DMI table is the module's alias list, and it gives this board `it87_acpi_ignore`, which has the same effect as `ignore_resource_conflict=1`. The kernel logged "Ignoring expected ACPI resource conflict". The board entry no longer names the option, and the doctor says that installing the package is the decision, and that it holds at every boot (§6.6).
  - **All five exit paths handed back every `pwm_enable` and all 33 automatic-curve settings**, the start PWMs among them, so the duty fix holds.
  - **Every channel held for 40 s again**, the IT8792E's included, with no contest, and full speed raised no false contest.
  - **Pairing confirms the fork's labels (§3.6).** The first version of the step measured too early and paired one control (§5.2). With that fixed, from the 60 % hold to 100 %:

    | Control | Tach | Header | RPM |
    |---|---|---|---|
    | IT8688E `pwm1` | `fan1` | `CPU_FAN` | 1048 → 1662 |
    | IT8688E `pwm2` | `fan2` | `SYS_FAN1` | 1562 → 2191 |
    | IT8688E `pwm3` | `fan3` | `SYS_FAN2` | 1044 → 1658 |
    | IT8688E `pwm4` | `fan4` | `PCH_FAN` | 4299 → 5625 |
    | IT8688E `pwm5` | `fan5` | `CPU_OPT` | 1374 → 2184 |
    | IT8792E `pwm1` | `fan1` | `SYS_FAN5_PUMP` | 1106 → 1726 |
    | IT8792E `pwm2` | — | `SYS_FAN6_PUMP` | nothing plugged in |
    | IT8792E `pwm3` | `fan3` | `SYS_FAN4` | 1544 → 2142 |

  - **it87 is Verified** (§3.6, §9.6): the doctor and the protocol say so. The cards show it once tiers reach the UI (D2).

  Moved on: the write test (§5.1) to M7, where setup's procedures run in the loop, with pairing written to `measured.toml`. `firmware.json` arrives with the first kind whose state outlives a reboot (M9); `penna reset` already restores everything there is today. The installed service is still 0.6.0, which leaves the motherboard's fans to the BIOS.
- **M6** — Packaging, README, first release. **Done 21 Oct 2026**, ahead of M5 (motherboard fans need the it87 decision, §13): 0.6.0 is packaged, installed and running as a service on this desk, with 136 Rust tests and 17 plugin logic tests (§7.6, as built). Still to do for a public release: publish the repository and the plugin as its own repo (§11, decision 9), and switch the PKGBUILD to a tagged tarball (now M10).
- **D2** *(draft 3)* — Visual review of the new UI: the setup view, the Defaults pill, and kind, tier and drift on fan cards. Mockups in real theme tokens across the four review themes, before their QML. **Approved 22 Oct 2026** (§11.3): design review 03, published as an artifact (https://claude.ai/artifact/5Vq9APfnNHVnuNiY9mKevm), source `design/penna-design-review-03.html`. It mocks the three on this desk's measured fans, with a live simulated setup run, and asked for ten calls (its section G).
- **M7** — **Setup** (§5.1–5.4, §5.6): write test, pairing, roles with *Identify*, calibration and drift, in the studio and `penna setup`. Run on this desk and on every simulated driver. With D2 (§11.3): the start screen, the setup view and `fan-clock`, roles with reasons and names, the chipset and pump-header exceptions, the Defaults popup with tachs in the tick, and device headings with tiers on the cards.

  **In progress since 22 Oct 2026.** Built and tested against a mock of this desk (161 Rust tests, 24 plugin logic tests):
  - **Setup is pure, like the loop** (`penna-core/src/setup.rs`): one tick's readings in, the duty each measured control runs at out. The daemon puts that in `measuring`, a new state (§4.6) the loop runs exactly, with no floor, step rate or kick, and that only an emergency outranks. `tests/setup.rs` runs it through tick, carry-out and record against a modelled desk: pairing, roles, stop and start duties, every fan back as it was found, the guards, a busy machine, Stop, and an emergency.
  - **Controls the configuration doesn't name** come into the plan for setup (`Plan::carry`) and stay until they are given back. A new plan keeps every control Penna still holds until it is released (§6.1): before this, a fan only a studio's draft had adopted stayed manual when the draft was discarded.
  - **As built:** the write test runs from each control's own first write, since the loop takes on one tick and writes on the next; NVML pairs control *i* with its own tach; the climb back from a stop starts at the stop duty (§5.4); a tach an earlier control turns, still recovering from its 40 %, isn't reported; a confirmed case fan on a pump header is measured below 50 % at once, on the confirming connection (`Setup::calibrate`).
  - **The daemon** runs setup on the connection that started it, saves `measured.toml` when measuring is over, names each paired control for its header, and plans again with the new floors. `setup.start`, `setup.stop`, `setup.roles` and `identify` are in the protocol (§7.3), with `tachs`, `setup`, `profiles[].fans`, `firmware[].rpm` and `controls[].drift` in the tick. A change is published before its reply goes out.
  - **`penna setup`** and **`penna identify`** (§8.5). On the mock desk a run takes 3:41 for ten controls.
  - **Drift** (`penna-core/src/drift.rs`): 25 % slower than calibrated, steadily for ten minutes, on its curve at a steady duty; a warn event, the card's line and *Recalibrate*.
  - **The simulator** has `x570-desk`: this desk as M5 measured it, from a fresh install, with its board's DMI fields so the board database applies.
  - **The plugin** (§11.3 calls 1–10): Defaults set apart and pills only for profiles that run a fan, *Set up fans…* on a new install, every fan listed in Defaults, `fan-clock` while setup runs, the setup view with its review, device headings with tiers, kind lines and drift on the cards. `tools/setup-preview` renders the setup view offscreen at each step of a run on the mock desk.

  - **Before the first run on this desk** (22 Oct 2026): this desk's CPU_OPT carries a liquid cooler's pump. Pairing's low step is now 50 %, a lone rising tach pairs on its rise (a pump barely slows), and a fast fan on a CPU header, or any fan on CPU_OPT, is held at 50 % until confirmed (§5.2, §5.3). `tests/setup.rs` adds a pump on CPU_OPT and checks that setup runs no fan below 50 % before calibration.
  - The plugin is merged; the running shell picks it up at its next start. `tools/setup-preview/live.sh` runs the studio's setup on a second screen against the mock desk.

  **The first run on this desk (22 Oct 2026).** 0.7.0 was installed as the service, and `penna setup` ran in a terminal: 4:09 for ten controls. The idle sample took 30 s, the write test 7 s, pairing 1:42 and calibration 1:50.
  - **Pairing agrees with M5's by-hand pairing** on every header. Each `pwmN` drives `fanN`, SYS_FAN6_PUMP is empty, and every fan's RPM at 60 % and at 100 % is within 3 % of M5's table. CPU_OPT was suggested as a pump and confirmed, so the pump was never run below 50 %. SYS_FAN5_PUMP was suggested as a case fan, confirmed, and then measured below 50 %.

    | Header | Stops below | Starts at | At 100 % | |
    |---|---|---|---|---|
    | CPU_FAN | 10 % | 10 % | 1,662 RPM | about 350 RPM from 10 to 20 % |
    | SYS_FAN1 | never | — | 2,170 RPM | about 835 RPM from 5 to 20 %, full speed at 0 % |
    | SYS_FAN2 | 10 % | 10 % | 1,687 RPM | about 345 RPM from 10 to 20 % |
    | PCH_FAN | 10 % | 18 % | 5,578 RPM | its BIOS stops it when cool |
    | CPU_OPT (pump) | — | — | 2,142 RPM | measured down to 50 % only |
    | SYS_FAN5_PUMP | 10 % | 10 % | 1,691 RPM at 90 % | its 100 % was recorded wrong, see below |
    | SYS_FAN4 | 25 % | 25 % | 2,096 RPM | flat between 60 and 90 % |
    | GPU fan 1 | 40 % | 40 % | 2,817 RPM | NVML percent |
    | GPU fan 2 | 35 % | 35 % | 2,855 RPM | NVML percent |

    Spin-up times were 8–11 s. Most fans reached the 10 s cap on "stable", so these are upper bounds.
  - **Four things were wrong in what it saved; all four were fixed after the run:**
    1. **SYS_FAN5_PUMP: 938 RPM at 100 %**, which is its firmware's speed. The re-measure started at 100 % on a fan not yet taken, and counted three readings from before its write. A duty now counts from the first tick the loop shows it written (§5.4). The rig in `tests/setup.rs` now lags its tachs by a tick, as it87's cache does, and without the fix it reproduces the bad reading.
    2. **The re-measure ran the whole descent, 1:48**, where the CLI said about 30 s. It now carries on from 50 % and keeps the points above, in about a minute.
    3. **SYS_FAN6_PUMP was saved as `paired = true`, with no tachs.** `paired` meant that pairing ran. It now means a fan was found, and a header with none is saved as `empty = true`.
    4. **SYS_FAN1 was saved with 2,170 RPM at 0 % and a stop of 0 %.** Written 0 %, this header runs its fan flat out. Calibration now ends the descent at a step that runs faster than the one above, and records the step above as the fan's lowest duty (§5.4). The plan never runs a fan below its lowest duty (§4.5).
  - **The 3090's fans stall in manual**: GPU fan 1 below 40 %, GPU fan 2 below 35 % (§0). The balanced GPU curve asks 30–40 % between 51 and 57 °C, and gpu-render asks 30 % at 40 °C. In those ranges a fan stood still while Penna showed a duty. The NVML floor now includes the measured stop (§4.5), so below it the fan goes back to the driver.
  - `x570-desk` now has both quirks, and the IPC test checks them end to end. 162 Rust tests, 24 plugin logic tests.

  **The second run (22 Oct 2026, 0.7.1)** measured only the GPU fans. Every motherboard header came out of the write test as `maps`, "204 → 153": 80 % asked, 60 % read. The write test counted whole seconds from the take on the wall clock, and the tick after the 60 % write woke a fraction of a millisecond earlier in its second than the write's own tick: 0.9998 s after it, so the 60 % reading was skipped, and on the next tick the 60 % still showing was taken for the 80 %. The eight headers share a take tick, so one late tick fails them all. The GPU passed, since its read-back checks only who is in charge. The mock never saw it: its clock steps in exact seconds. Pairing offers only `ok` headers, so pairing and calibration measured nothing on the motherboard, and `measured.toml` lost the first run's pairing.
  - **Fixed in 0.7.2:** each reading is checked against the duty the loop shows it wrote on the tick before (§5.1), and a header the loop finds contested is `reverts`. The rig in `tests/setup.rs` can wake each tick up to 2 ms late and run a 2 s tick: on the old write test 12 of 24 late clocks and the 2 s tick fail as this desk did, and a whole run on 12 late clocks now measures the same as on an exact one. 164 Rust tests.
  - The GPU fans measured again: GPU fan 1 stops below 40 % as before, GPU fan 2 below 40 % where the first run found 35 %.

  **The third run (22 Oct 2026, 0.7.2)** measured every fan. Every header passed the write test, pairing agreed with M5 again, and the four fixes held on the hardware: SYS_FAN1 is measured down to 5 % with no 0 % point, SYS_FAN5_PUMP reads 1,726 RPM at 100 % (M5: 1,726), and its re-measure took 63 s. SYS_FAN6_PUMP is saved as empty. The GPU fans stop below 40 %, both of them this time, and GPU fan 1 turned only 294 RPM at 40 %. Every point matched the first run's within about 3 %, except SYS_FAN4 at 100 %: 1,834 RPM, where the first run read 2,096 and M5 2,142.
  - **SYS_FAN4, measured alone with `penna setup SYS_FAN4`** while its tach was recorded from the tick stream every second: raised to 100 %, it reads 1,607, 2,149, 2,045, 1,934 and 1,854 RPM, each twice. It overshoots, and comes back over some 20 s. The write test shows the same at 80 %: 1,890 RPM, where it holds about 1,610. So M5's 2,142 and the first run's 2,096 were the overshoot. The descent followed the fall, which is part of why SYS_FAN4 looks flat from 65 to 90 %.
  - **Every reading twice** is it87's 1.5 s cache on a 1 s tick, on both chips. "Stable" compared the last two readings, and a repeat always passes.
  - **Fixed in 0.7.3** (§5.4): "stable" is judged on readings 2 s apart, and a fan still moving one way at 10 s gets up to 30 s. The rig's tachs now repeat as it87's do, and it has a SYS_FAN4 that overshoots: every point of five fans must be within 3 % of the fan's steady speed, which the old rule failed by 4 % on CPU_FAN. `x570-desk` has the cache and the overshoot too. Calibration takes 177 s on the rig where it took 99, and `penna setup` and the plugin say about five minutes. 165 Rust tests.
  - The journal and the studio said "below its 30 % floor" for the GPU fans, whose floor is now 40 %. Both name the real floor.

  **The fourth run (22 Oct 2026, 0.7.3)** took 5:30 for ten controls, calibration 3:12, and SYS_FAN5_PUMP's re-measure 1:37. Every fan's duty and RPM were recorded each second from the tick stream (`target/m7-setup-run4.jsonl`; SYS_FAN4 alone in `target/m7-sysfan4-alone.log`), and every point is where its fan settled:
  - **CPU_FAN and SYS_FAN2 are slow fans:** 8–10 s a step, and 16 s to reach 100 % from the hold. They read 1,730 and 1,726 RPM at 100 %, where M5 and the earlier runs read 1,662–1,687 while they were still speeding up and had 95 % above 100 %. Their tables now rise all the way.
  - **SYS_FAN5_PUMP:** 1,819 RPM at 100 % after 18 s (M5 and the third run: 1,726, still rising), and a smooth table down to its stop at 10 %.
  - **SYS_FAN4** overshot to 2,096 RPM and was read at 1,726, 20 s later. Above 65 % it holds about 1,700 RPM whatever the duty (1,642–1,726), its own regulation hunting: at 80 % and at 70 % it dipped and came back above the step before. Running it above 65 % buys nothing.
  - **The 3090's fans stop below 45 % in manual**, both: given longer at 40 %, they kept slowing to a stall. Their floor is 45 %, and the journal says so. 2,962 and 2,974 RPM at 100 %.
  - The other headers match the earlier runs. SYS_FAN1 is measured down to 5 % with no 0 % point, CPU_OPT is a pump measured down to 50 %, PCH_FAN stops below 10 % and starts at 20 %, and SYS_FAN6_PUMP is empty.

  **On this desk, M7 is done.** Two things move on:
  - **To M8:** step 3 of §5.4, a monotonic table, isn't built. SYS_FAN4's top shows why it matters, and M8 is the first to read a table backwards, from RPM to duty.
  - **A role's reason quotes pairing's RPM:** the highest in 6 s at 100 %, before calibration. A slow fan reads low there: SYS_FAN5_PUMP's says 1,460 RPM, where it turns 1,819. A slow pump could be suggested as a case fan. That is safe, since a fan on a pump header stays at 50 % or more until confirmed (§5.3), but the suggestion should use the calibrated top once there is one.

  Still to do: the simulated drivers when M9 adds them.
- **M8** — **Generated curves** (§5.5): per role and profile from setup's measurements, the CPU load check, and the review step.

  **In progress since 22 Oct 2026.** Built and tested on the mock desk and against this desk's own measurements, now a fixture (`test/fixtures/measured/x570-desk.toml`).
  - **The generator** (`penna-core/src/generate.rs`): every curve of §5.5 but firmware and laptop curves, every fan's limits, all three profiles. Calibration tables are made monotonic here (§5.4, step 3, moved from M7).
  - **The daemon** proposes the draft when measuring is over and again for confirmed roles. It runs the load check, and saving switches to Balanced. `draft.generate` is in the protocol.
  - **`penna setup`** keeps one connection from start to save. It shows the load check's result, describes the proposed curves per group of fans and profile, and asks to save them. `--no-load-check` skips the check.
  - **Two fixes on the way:** a client given `$PENNA_SOCKET` no longer falls back to this machine's daemon, and the GPU's limits line and inspector name its measured floor.

  **The fifth run on this desk, the first of M8 (22 Oct 2026, 0.8.0).** `penna setup` took 8:30 for ten controls with the load check: idle 30 s, write test 6 s, pairing 1:31, calibration 3:11, load check 3:00. Confirming the roles re-measured SYS_FAN5_PUMP below 50 % in 1:31. The proposal was saved: 19 curves, every fan on Balanced.
  - **The load check passed in one pass:** "Under 3 minutes of full load the CPU levelled at 68 °C, with its fans at 56 %." The peak was 70.9 °C, the limit 90.
  - **But the fans hunted under the load** (§4.4): CPU_FAN between 45 and 57 % every 31 s, the case fans between 16 and 40 %.
  - **The idle sample wasn't idle.** Tctl climbed from 40 to 61 °C during its 30 s, and the mean, 50.3 °C, became the CPU's idle. Over the 75 minutes of review Tctl's median was 40.8 °C. Every CPU curve came out about 9 °C late: Balanced holds CPU_FAN at its floor to 60 °C. A steeper curve from 60 to 70 °C is half of why the fans hunted.
  - **Balanced was already on,** from the M5 test's GPU-only configuration. So for the whole review all nine fans ran the proposal, while `penna setup` said nothing would change until it was saved. At idle every fan sat at its floor: CPU_FAN 15 %, SYS_FAN1 5 % (840 RPM, its lowest), SYS_FAN2 and SYS_FAN5_PUMP 15 %, SYS_FAN4 30 %, PCH_FAN stopped, both GPU fans on the driver's curve at 0 RPM, the pump at 85 %. Tctl stayed between 39 and 43 °C (10th to 90th percentile).
  - **GPU fan 1's floor came out at 35 %**, where the fourth run found 45 % for both fans. The M5 test's `gpu` curve stays in the file, unused.

  **Fixed in 0.8.1:**
  - Hysteresis trails the curve on the way down instead of dropping to it (§4.4). It is tested on a plant fitted to the load check's readings.
  - The idle sample takes the lowest fifth of its readings (§5).
  - `penna setup` and the journal say when the fans run the proposal during the review.
  - A role's reason quotes the calibrated speed at 100 % (§5.3).
  - "1 fans" now reads "1 fan".

  178 Rust tests, 24 plugin logic tests.

  Still to do:
  - the plugin's review of the proposed curves, the *Generated* marks and the anchors on the axis (§8.3, §11.3 call 9);
  - a run on this desk with 0.8.1, for an idle sample that is idle and a load check without hunting.
- **M9** — **Laptops and other drivers:**
  - `levels`, `firmware-curve`, `modes` and `platform` controls, amdgpu included;
  - a Documented fixture per driver;
  - captures from real machines, wherever they can be had;
  - tiers shown in the UI.
- **M10** — **Public release:**
  - the repositories, the plugin as its own repo, and a tagged tarball for the PKGBUILD and the AUR;
  - how to contribute a capture.

## 11. Decisions (approved 21 Oct 2026)

1. **Name: Penna**, provisional. Plugin id `io.github.pushmotta.penna`, daemon `pennad`, CLI `penna`, repository `penna`. No AUR package uses the name. Draft 1's `fanctl` was dropped because `fanctl` and `fanctl-git` already exist in the AUR, and the proposed "Nortada" was not taken up. **Chosen 21 Oct 2026.**
2. **Two kinds of mixing.** Virtual sensors combine temperatures, and Mix curves combine curve outputs. Both, because they answer different questions. **Approved.**
3. **Graph curves are non-decreasing.** The editor pushes neighbouring points instead of allowing a dip. A fan curve that falls as the temperature rises is almost always a mistake. **Approved.**
4. **% means duty.** It is never rescaled to the calibrated range; the RPM axis carries what calibration knows (principle 3). **Approved.**
5. **A draft belongs to its connection** (§7.4). **Approved.**
6. **Failsafe.** A failed source means 100 %; an exiting daemon means the original mode. Handing back to firmware on a sensor failure would rely on the BIOS curve, which may be the thing the user replaced. **Approved.**
7. **Authorisation.** Peer credentials plus a `penna` group, with polkit later if needed. **Approved.**
8. **Tick.** 1 s by default, because Super I/O accesses are the risk in §6.6, and fans and heatsinks respond in seconds. The UI hides the tick (§8.6). **Approved.**
9. **Repository.** One workspace with the plugin under `plugin/`, symlinked into `~/.config/omarchy/plugins/` while developing. At release the plugin is published as its own repo, because `omarchy plugin add` expects `manifest.json` at the root. **Approved.**
10. **The UI quality bar is a requirement.** The budgets in §8.6 are tested, and the visual design review (milestone D) comes before any QML. **Asked for on 21 Oct 2026.**

### 11.1 Design review 01 (approved 21 Oct 2026)

The review (companion artifact, section G) asked for eight calls, and the user approved all eight recommendations the same day:

1. **The glyph carries state; colour reinforces.** No review theme separates urgent from accent by lightness (1.1–1.7:1), and miasma's urgent is 2.3:1 on its background. There are six glyphs, all in the installed font (§8.1).
2. **The bar widget shows the glyph only by default.** A temperature is a widget setting.
3. **Fan rows lead with RPM; duty is second.**
4. **Full speed is a toggle until pressed again.** `fan-clock` is reserved for timed overrides from the CLI.
5. **Sparkline windows:** 60 s in the popup, 5 minutes in the studio, on fixed per-sensor scales.
6. **The studio has three columns at 1100 px.** Narrower than that, the sensors column becomes a tab beside the controls.
7. **Three profiles ship: Quiet, Balanced, Render.** Names are editable. Up to five show as pills; more go into a dropdown.
8. **Motion constants:** linear glide over one tick, a text dead band of 0.6 of a step, a 140 ms text ease, and `OutCubic` for everything else.

### 11.2 Design review 02: any machine (approved 21 Oct 2026)

Asked for on 21 Oct 2026, in the user's words: "control all the systems fans with this app, same level of control as the GPU fans", "auto calibration, or auto curve profile creation", "always have the ability to reset to bios or driver defaults", and "this has to work on a myriad of workstations and laptops". Calls:

1. **Kernel drivers only.** Penna drives fans through hwmon and NVML and never touches a chip from userspace: no port I/O, no copied Windows tricks (§6.6). Userspace chip access is blocked under Secure Boot lockdown, has no lock against ACPI, and would fight the kernel's own drivers. Gaps are fixed in the drivers and offered upstream. **Approved 21 Oct 2026** ("move ahead with this").
2. **Seven kinds of control, plus read-only** (§3.5), each shown for what it is. Same-as-GPU control everywhere the hardware allows it, and a plain sentence where it doesn't. Penna never writes a chip's own curve registers, or a driver whose mistakes have powered machines off (`legion_hwmon`).
3. **Defaults is a built-in profile,** first in every list (§4.7). A new install starts there, and choosing it is the reset (§6.7).
4. **Firmware state is snapshotted before the first change.** `penna reset` restores everything without the daemon, and removing the package runs it (§6.7).
5. **Setup is one automatic run** (§5). Pumps and chipset fans are never stopped; CPU fans stop for at most 10 s; the guards hand everything back.
6. **Generated curves** per role and profile, anchored on measured idle and each sensor's limit, placed in airflow and written in duty, delivered as a draft (§5.5). The table is a starting point, checked under load on the CPU.
7. **The board database is data with sources,** and every fan shows its support tier (§3.6). No entry is written from memory.
8. **Platform profiles** are switched by Penna's profiles only where they are a machine's only fan lever, and through power-profiles-daemon when it runs (§3.5).
9. **Rediscovery without a restart** when hwmon devices come and go (§3.6).
10. **Order:** M5 (this desk, the kinds) → D2 (visual review) → M7 (setup) → M8 (generated curves) → M9 (laptops and other drivers) → M10 (public release).

The user approved all ten recommendations on 21 Oct 2026 ("go ahead with your reccos").

### 11.3 Design review 03: setup, Defaults and fan cards (approved 22 Oct 2026)

Milestone D2. The review (companion artifact, section G) asked for ten calls. Three come from measurements on this desk (calls 3, 7 and 8), and two change draft 3's rules (calls 5 and 8):

1. **Defaults is the first pill, set apart by a wider gap.** It counts toward the five pills. Choosing it needs no confirmation (§8.2).
2. **A profile shows as a pill once it runs at least one fan.** A new install shows Defaults and `Set up fans…` only (§4.7, §8.2).
3. **In Defaults, every fan stays listed** with its RPM and who runs it, named for its header, and its duty left blank: the IT8688E's PWM register reads the BIOS curve's start value while the BIOS runs a fan. Before pairing the rows are tachs, which the tick carries as an additive protocol field (§8.2).
4. **Setup takes the studio over,** opens on a start screen with the load check as its only option, and puts *Stop and hand back* where *Save* sits. The bar shows `fan-clock` while it runs (§5, §8.1, §8.3).
5. **Pairing settles every fan at 60 % first, and pairs on the rise:** 15 % and 150 RPM above the tach's reading just before, at its highest in the 6 s at 100 %. The drop to 40 % confirms. This paired all seven fans by hand in M5 (§5.2).
6. **Calibration draws each fan's table on its own scale,** in small multiples, as it measures (§8.3).
7. **Roles are chips with their reason, and each fan can be named in the review.** A fan on a pump header that turns out to be a case fan is held at 50 % or more until confirmed, then measured below (§5.3).
8. **A chipset fan may stop when the idle sample saw its firmware stop it.** Calibration then measures it to a stop, and its curve rests below about 64 °C. Otherwise chipset fans never stop (§5.3–5.5).
9. **Generated curves arrive as one draft** holding all three profiles, each curve marked *Generated*, with its anchors drawn on the axis. *Save* keeps everything and switches to Balanced; *Discard* leaves Defaults on and keeps the measurements (§8.3).
10. **Cards carry kind and tier only when they say something.** The tier goes on the device heading; a card adds a kind line unless it is plain duty or NVML. Drift is a line with *Recalibrate*; `stalled` is urgent; an empty header folds away (§8.2, §8.3).

The user approved all ten recommendations on 22 Oct 2026 ("go ahead with your reccos").

## 12. Prior art, read but not copied

- **Fan Control** (Rem0o, Windows). Cards for controls, speeds and curves. Mix with Max, Min and Average. Response time, hysteresis and hysteresis direction, step up and step down, RPM avoid zones, start/stop logic for 0 RPM, calibration. Built on LibreHardwareMonitor, with .NET plugin DLLs. Taken: the card model and the curve vocabulary. Not taken: in-process plugins, because a root daemon must not load third-party code.
- **CoolerControl.** A headless daemon that serves its own web UI, REST and gRPC APIs, Prometheus metrics and the `cctv` CLI. Profiles (fixed, graph, mix, overlay), functions (hysteresis, thresholds, response time), modes, calibration, tags, stress tests and an auto-create wizard. Credited: overlay profiles (our offset), stress tests (v2). Different here: no HTTP server in a root process, only a local socket, and the UI belongs to the shell.
- **fan2go** (Go). One loop per fan. Adaptive measurement of start, max and interior points, kept in a database. Curves `linear`, `staircase`, `pid` and `function` (min, max, average, delta, sum, difference). The `pwmValueChangedByThirdParty` and `fanModeChangedByThirdParty` sanity checks; `controlMode.onExit: restore`; `setPwmToGetPwmMap` for fans that read back a different value than was written. Its documentation warns that the hwmon index "is not stable" and that NVIDIA RPM needs driver 565 or newer. Taken: third-party change detection (our `contested`) and the read-back map idea. Different: GUI first, and calibration shown in the editor.
- **LibreHardwareMonitor** (the library under Fan Control; MPL-2.0) *(draft 3)*.
  - **On Windows** it reaches hardware through its own kernel driver, WinRing0 and since 2025 PawnIO, and does the chip work itself. It probes `0x2E`/`0x4E` with each vendor's entry sequence, reads the chip ID at `0x20` and the base address at `0x60`, and reads registers with its own code per chip family.
  - **Board names** come from SMBIOS and a per-board table in code.
  - **On Gigabyte boards** it switches off the fan firmware on the second ITE chip (§6.6).
  - **On Linux** it only reads `/sys/class/hwmon` and controls nothing.
  - Taken: a board database, as data rather than code (§3.6). Not taken: userspace chip access (§11.2, call 1), and none of its code or tables, which are MPL-2.0.
- **lm_sensors `fancontrol` / `pwmconfig`.** The baseline. Its configuration names `hwmonN` paths, which break when the indices move.
- **frankcrawford/it87.** Supports IT8688E and IT8792E. MMIO is on by default and "necessary for fan control with newer Gigabyte motherboards". Full manual mode with workarounds for IT8688, IT8689, IT8790, IT8792/IT8795 and IT87952 since August 2026. Calls `ignore_resource_conflict` "inherently risky".

## 13. To verify on the desk

These need root or a shell restart, and were not run for this draft.

1. ~~**Which chips.**~~ *Loaded 22 Oct 2026:* `it87-dkms-git` 358.bd7a566, first by hand with `ignore_resource_conflict=1`. It now loads at every boot by itself: the fork's DMI table names this board and ignores the conflict for it, as the option would (M5 status, §10). The IT8688E is at 0xa40 (`it8688_4008090b`, five controls) and the IT8792E at 0xa60 (`it8792_4008090b`, three, through Gigabyte's EC). The kernel logged the expected ACPI resource conflict and nothing else.
2. ~~**Which headers each chip drives.**~~ Paired by hand in M5: each `pwmN` drives `fanN`, as the fork's labels say (§3.6). ~~Whether IT8792E writes hold~~: they held for 40 s in both M5 hardware runs, with no contest (M5 status, §10).
3. ~~**BIOS Smart Fan settings** that manual control may need.~~ None on this desk: all eight channels held in both M5 runs with the BIOS settings as they are (F36d). The fork notes some Gigabyte boards need a BIOS curve workaround.
4. ~~**NVML on the 3090 as root.**~~ Verified in M4 (§9.4): manual speeds hold and read back as manual, `SetDefaultFanSpeed_v2` hands the fans back, and zero RPM returns at idle.
5. **`pwm_enable` after suspend and after hibernate.**
6. **Cost of a tick** (`strace -c` on the daemon). Goal: under 1 ms.
7. ~~**The shell's render loop on this NVIDIA stack.**~~ Verified in M3: `threaded` (Quickshell 0.3.2, NVIDIA 615.78.08). A baseline run of the shipped popups, for comparison, is still to do.
