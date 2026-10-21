# Penna — design specification

Draft 2 · 21 October 2026 · status: decisions approved (§11), UI quality bar added (§8.6), M0, M1 and M2 done, design review 01 approved (milestone D, §11.1). Next: M3, the studio.

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

- **The 3090 stops its fans by itself, and manual control cannot.** Sampled once a second at 36–39 °C: fans at 53 % / ~1000 RPM, then 0 % / 0 RPM a second later, all under the default policy. NVML refuses manual speeds below 30 %, so taking manual control of this card loses zero-RPM mode.
- **NVML's "target" is not what the driver is doing.** `nvmlDeviceGetTargetFanSpeed` read 30 % throughout, both while the fans ran at 53 % and while they were stopped. `GetFanSpeed_v2` (percent) and `GetFanSpeedRPM` tracked reality.
- **NVML percent is not duty.** It is a "percentage of the product's maximum noise tolerance fan speed" and "may exceed 100 %" (`nvml.h`).

---

## 1. Thesis

Linux can control fans, but it does not explain them. The kernel shows speed controls and RPM readings as unrelated numbered files. The numbering changes between boots, and the chip behind them may also be driven by the firmware. The existing tools either expose that raw (`fancontrol`, fan2go's YAML) or put a web dashboard in front of a root daemon (CoolerControl). Fan Control on Windows earned its following with one idea: each fan is a card, each curve is a card, and you wire them together in a window you can understand in a minute.

The bar of quality is not "has the knobs". A user opens it and sees every fan named for what it cools, with its RPM next to what it was asked for. They drag a point on a curve, hear the change, and never edit a config file. When something goes wrong — the daemon dies, a sensor disappears, the BIOS takes a fan back — the fans go somewhere safe, and the UI says which fan and why.

## 2. Principles

1. **Safe by construction.** No fan ever runs slower than the firmware would have run it unattended, except by a choice the user made with the consequence in view. Every path out of the daemon, including `kill -9`, hands control back (§6).
2. **The daemon owns the fans; surfaces only ask.** A shell crash, a closed window or a logout never changes a fan's speed. An unsaved edit is discarded when the client that made it goes away (§7.4).
3. **Physical truth.** Temperatures in °C, speeds in RPM, duty in % of PWM. Show what was asked for next to what was measured. When firmware overrides a control, say so; never show a value we wrote as if it were the fan's speed.
4. **Stable identity.** Things are named by hardware path and channel, never by `hwmonN` (§3.2). A device that is absent keeps its configuration.
5. **Theme native, keyboard first.** Candela's rules carry over: only `Color.*` and `Style.*` tokens, the shell's cursor model (j/k, h/l, Enter, Esc), and the pointer never produces a second highlight.
6. **Progressive disclosure.** The bar shows health. The popup has the daily controls. The studio has the curves. "Advanced" inside the studio has the last 10 %.
7. **Testable without hardware.** Control logic is pure. Every backend has a mock. The real sysfs backend runs unmodified against a fake tree (§9).
8. **Smooth, and measured.** Every surface keeps up with the display's refresh rate. Live values glide instead of stepping once a second, and nothing animates while nothing changes. The budgets in §8.6 are tested like the safety paths, not hoped for.

## 3. Hardware model

### 3.1 Three kinds of thing

As in Fan Control, these are separate objects, related only by explicit pairing:

| Kind | What it is | hwmon | NVML |
|---|---|---|---|
| **Sensor** | A temperature in °C | `tempN_input` (millidegrees), `tempN_label`, `tempN_crit` | `nvmlDeviceGetTemperatureV` (the older `GetTemperature` is deprecated in NVML 13) |
| **Control** | Something we can set | `pwmN` (0–255) and `pwmN_enable` | fan index *i*: `SetFanSpeed_v2`, `SetFanControlPolicy`, `SetDefaultFanSpeed_v2` |
| **Tach** | An RPM reading | `fanN_input` | `GetFanSpeedRPM` for fan *i* |

hwmon does not say which tach a control drives: `pwm2` may well spin the fan on `fan4`, and one control can drive several fans through a splitter, or a fan with no tach (§5.1). NVML pairs control *i* with tach *i*.

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
- *v2:* amdgpu (`pwm1`, and `gpu_od/fan_ctrl/` on RDNA3 and later), USB coolers and hubs through `hidapi`. Neither exists on this desk.

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
- **Hysteresis** — the output rises at once and falls only after the temperature drops H °C below the point where it last rose. Default 3 °C, on Graph curves only. Option: both directions. A Trigger's hysteresis is its on/off gap. Flat and Mix curves take neither setting; a configuration that gives them one is refused.

### 4.5 Limits (on the control)

- **Min / max %**, **offset %**.
- **Step up / step down** in %/s (defaults 20 and 5).
- **May stop** — off by default. When off, the floor is the calibrated stop duty plus 5 %, or 30 % until the fan is calibrated. When on, outputs below the stop duty become 0. *May stop* is refused on a motherboard fan with no calibrated stop duty. Leaving 0 uses the **start %** kick for 2 s and then settles. The kick is the calibrated start duty, otherwise the larger of the floor and 50 %.
- **Entering and leaving safety.** Safety states and overrides apply at once. Leaving them, the fan falls back at the step-down rate. Overrides are clamped to the floor. An NVML override below 30 % on a fan with *may stop* hands the fan to the driver.
- **NVML floor.** Manual control cannot go below 30 % on this card (§0). With *may stop* on, an output below the floor **hands the fan back** to the driver policy (`SetDefaultFanSpeed_v2`). The driver's own curve then stops the fan when cool and still protects the GPU. Manual control resumes when the curve rises above the floor plus 5 %. This is the only way to keep zero-RPM, and it is observed behaviour, not a guess.

### 4.6 Control states

Each control is in exactly one of these, and every surface renders the same set:

| State | Meaning |
|---|---|
| `firmware` | Not adopted. BIOS or driver in control. The default for every control on first run. |
| `curve` | Following its assigned curve. |
| `handback` | NVML below its floor, temporarily on the driver policy (§4.5). |
| `override` | A temporary fixed value (`penna set … --for`, or "Full speed" in the popup). |
| `failsafe` | A source failed or went stale. Running at the control's failsafe value. |
| `emergency` | A sensor crossed its emergency limit. 100 %. |
| `contested` | Something else keeps changing the control (§6.4). We have stopped writing to it, except in an emergency (§6.3). |
| `stalled` | Its paired tach reads 0 RPM while the duty should spin it (§6.3). Lands with pairing in M5; M1 only warns. |
| `absent` / `fault` | Not present, or a write failed and was handed back. |

### 4.7 Profiles

A profile assigns a curve (or `firmware`) to every control. Curves are shared between profiles, and one profile is active. Penna ships three: **Quiet**, **Balanced**, **Render**. Names are editable. Up to five show as pills in the popup; more go into a dropdown. Switching is instant through the popup or the CLI; automatic switching is v2 (§10).

## 5. Pairing and calibration

### 5.1 Pairing (hwmon only)

All adopted controls are held at their current duty. One control at a time is raised to 100 % for 6 s, then dropped to 40 % for 6 s. Any tach that follows both moves by more than 25 % is paired with it. A control can end up with several tachs, or none: a header with no tach, or a dead fan. The result is shown for the user to confirm. The run aborts and hands everything back if any temperature passes its guard (CPU 85 °C, GPU 80 °C by default) or any read fails. About 12 s per control.

### 5.2 Calibration

Per control, with its tach paired:

1. Step down from 100 % in 5 % steps. At each step, wait until RPM is stable within 3 % over 2 s (timeout 10 s) and record it. The lowest duty that still turns is the **stop duty**.
2. From 0, step up in 2 % steps. The first duty that spins a stopped fan is the **start duty**, usually above the stop duty because of stiction.
3. Store the duty→RPM table and make it monotonic.

Uses: the floor in §4.5, the RPM axis in the editor ("40 % ≈ 820 RPM"), dead-zone warnings, and later RPM-mode control. NVML fans skip steps 1–2: we record % → RPM over 30–100 % for the axis only.

Measurements live apart from configuration (§7.5): they are facts about the hardware, not choices.

## 6. Safety

The guarantees, in order of what they protect against.

### 6.1 Write-ahead hand-back

Before a control leaves `firmware`, the daemon writes its key and **original mode** to `/run/penna/held.json` (temporary file, `fsync`, atomic rename). Original mode is the exact `pwmN_enable` value — "automatic" varies by chip, so it is never assumed to be 2 — and, for NVML, the policy. Only then does it switch the control to manual. Adoption is bumpless: read the duty the firmware was running and start the curve's ramp from there. Taking an NVML fan records its policy and changes nothing else: manual mode starts with the first write, so a fan the driver has stopped is not spun up for a tick. Restoring a control whose original `pwm_enable` was 1 (manual) also writes back its original duty. The order is the guarantee, as in Candela: **a control is never manual without its way back already on disk.**

### 6.2 Every exit path restores

| Exit | What runs |
|---|---|
| SIGTERM, SIGINT, `systemctl stop` | The loop's shutdown path restores every held control, then removes `held.json`. |
| Panic | A panic hook restores from an immutable snapshot of `held.json` taken at adoption. No locks, no shared state. The hook runs under both `panic = "unwind"` and `"abort"`. |
| `kill -9`, OOM, a crash inside NVML | Nothing in the process runs. The unit's `ExecStopPost=penna restore` reads `held.json` and restores. `RuntimeDirectoryPreserve=yes` keeps the file past the process. |
| Hang | `Type=notify`, `WatchdogSec=5`. The **control loop itself** pings once per tick, not a helper thread, so a stuck loop stops pinging. systemd kills the daemon, runs `ExecStopPost`, and `Restart=on-failure` brings it back. |
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
- **A stalled fan.** A paired tach that reads 0 RPM for 3 s while the duty is at or above the stop duty plus 5 % gets one start kick at 100 % for 2 s. If it still reads 0, the control is `stalled`: urgent, the `fan-alert` glyph, and "isn't turning at 35 %". Penna keeps commanding the curve, and the emergency rule still protects. *(M1 only warns; the state needs pairing, M5.)*
- A write fails → the control is handed back to its original mode, enters `fault`, and is reported.
- Writes happen only when the value changes, at most once per control per tick.

### 6.4 Read-back and contested controls

After every write, read back `pwmN` and `pwmN_enable` (NVML: the policy). If the mode is no longer manual, or the duty is off by more than 2 %, for two ticks running, something else is driving the fan. That could be the BIOS, a chip "helper", a resume reset, or another tool. The daemon re-asserts up to three times in 30 s. After that it marks the control `contested` and stops writing. The UI then says "The BIOS (or another program) is controlling this fan", with what we wrote and what we read. This is the case the it87 community reports on Gigabyte X570 boards, where IT8792E PWM writes reverted within a second.

At start, `penna doctor` and the daemon refuse to adopt while `coolercontrold`, `fan2go` or `fancontrol` is active, and name the unit.

### 6.5 Resume

The daemon subscribes to logind `PrepareForSleep(false)`. On resume it re-reads every mode and re-asserts manual control where it held it. Read-back (§6.4) catches whatever this misses. Both suspend and hibernate have to be tested here (§13).

### 6.6 What we do to the chip

With `ignore_resource_conflict` the it87 README warns that "both ACPI and this driver may access the chip at the same time", which can cause "unexpected system reboots". Our part of that risk is the number of accesses: a 1 s tick for hwmon, no write when nothing changed, and one read pass per tick. Loading the driver that way stays the user's decision, explained by `penna doctor`.

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
1. `$PENNA_SOCKET`
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

`draft.*`, `pair` and `calibrate` arrive with the studio (M3) and pairing (M5).

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

- The socket is world-connectable. Each request is checked against the peer's credentials: reads are open to any local user; writes need uid 0 or membership of the `penna` group, which is created by `sysusers.d`. polkit (the shell already ships an agent) is the v2 route if group membership proves awkward. Under `--mock`, the daemon's own uid may also write, so the whole stack runs unprivileged.
- **A draft belongs to its connection.** The studio's edits apply live to the fans for preview, and `draft.save` persists them. If the connection closes with a draft open — Esc, a closed overlay, a shell crash — the daemon reverts to the saved configuration. This is Candela's "the revert runs outside the shell", without a countdown: a bad fan curve is audible and caught by §6, not invisible like a dark display.

### 7.5 Persistence

| Path | Holds | Written by |
|---|---|---|
| `/etc/penna/config.toml` | Labels, virtual sensors, curves, control limits, profiles, active profile | Daemon only, atomically, on `draft.save` and `profile.set`. Hand edits while the daemon is stopped are fine; `penna check` validates them. |
| `/var/lib/penna/measured.toml` | Pairings, calibration tables, keyed by control and tach | Daemon, after a confirmed pair or calibrate |
| `/run/penna/held.json` | Write-ahead hand-back list (§6.1) | Daemon; read by `penna restore` |

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
| Nothing adopted | `fan-auto` U+F171D | text |

Wheel: none, because fans are not a volume.

### 8.2 Popup

Same chrome as the audio and bluetooth popups, 380 logical px wide.

- **Hero.** Active profile and one line: `Balanced · 4 fans · CPU 62 °C · GPU 41 °C`. When something is wrong, the line says what instead: `GPU fan 1 contested — the driver is controlling it`.
- **Profile.** ButtonGroup `Quiet · Balanced · Render`.
- **Temperatures.** One row per source a curve uses: value plus a 60 s sparkline from the service's ring buffer.
- **Fans.** One row per adopted control: label, RPM, duty, curve name, and a state badge when not `curve`. Unadopted controls fold under `Firmware (n)`; absent and passed-through devices fold under `Not reachable (n)`, each with its reason.
- **Actions.** `Full speed` (an override until pressed again), `Curves…` (opens the studio).

### 8.3 Studio (kind: overlay)

Summoned from the popup's `Curves…` and with `penna open`. A centred card about 1100 logical px wide, with its height set by its content. It is laid out like Fan Control: controls on the left, the curve editor in the middle, sensors on the right, action bar along the bottom. Narrower than 1100 px, the sensors column becomes a tab beside the controls.

- **Controls.** A card per control: label, live RPM and duty, state, assigned curve (dropdown), and Advanced (limits, may stop, failsafe, start %). `Adopt` and `Release` live here.
- **Curve editor.** X is °C, Y is duty %, with the control's calibrated RPM as a secondary axis on the right. Draggable points, the hysteresis band shaded, the stop and min lines drawn, and a live marker where the smoothed source and the output are now. The hand gets Candela's drag rules: position = press position + pointer travel, a 4 px threshold, and snapping measured in screen pixels. Keys: `[` / `]` select point, h/l move by 1 °C, j/k by 1 %, Shift ×5, `a` add, `x` delete.
- **Sensors.** Every sensor with a sparkline, and the virtual-sensor builder.
- **Action bar.** Profile dropdown, `Pair…`, `Calibrate…`, `Discard`, `Save`. Hints at the far left in caption size. Pairing and calibration open a step-by-step view: what is happening ("Raising PWM 3 to 100 % — fan 4 rose 610 → 1790 RPM"), the live guard temperatures, and `Stop`, which hands everything back at once.

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
penna pair [<control>] | calibrate <control>
penna check [<file>]               # validate a config without applying it
penna restore [--from <held.json>] # root; ExecStopPost
penna doctor                       # driver bound? ACPI conflict? it87 chip and version? other fan daemons? vfio GPUs? contested controls?
penna capture <dir>                # read-only snapshot of /sys/class/hwmon for test fixtures
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

## 10. Scope and milestones

| v1 | v2 |
|---|---|
| hwmon and NVML backends, mock and simulator | amdgpu; USB coolers and hubs via `hidapi` |
| Flat, Graph, Mix, Trigger; virtual sensors; response and limits | Sync, Target (PID), RPM avoid zones, RPM-mode control |
| Pairing, calibration, contested detection, every safety path in §6 | Automatic profile switching (a process running, e.g. a Blender render or Brush training → Render; AC power; time of day) |
| Profiles switched by hand | Stress-test runner to check curves under load |
| CLI, Omarchy plugin (bar, popup, studio), PKGBUILD | Standalone egui/wgpu app; shell notifications on `contested` / `failsafe` |

Milestones, each ending in something visible:

- **M0** — `penna watch` and `penna capture` on this desk, read-only, no root: k10temp, NVMe, and the 3090's temperature, two fans, percent and RPM. **Done 21 Oct 2026**: 28 tests, clippy clean, fixture in `test/fixtures/desk-2026-10-21`.
- **M1** — `penna-core`, mock, simulator and property tests. `pennad --mock` runs unprivileged. **Done 21 Oct 2026**: 86 tests, including 4 property tests at 400 cases each and exit-path tests for `kill -9`, SIGTERM, panic and a leftover `held.json`. The simulator has `render-burst` and `faults` scenarios. Mock mode runs on virtual time (`--speed`). `pennad` and `penna restore` refuse the real `/sys` until M4/M5.
- **D** — Visual design review: mockups in real theme tokens and motion prototypes (§8.6), sourced in `design/`. No QML is written before it is approved. **Approved 21 Oct 2026** (§11.1).
- **M2** — Protocol and CLI against the mock. The plugin's service, bar widget and popup against the mock daemon. **Done 21 Oct 2026**:
  - protocol v2 over the daemon's socket, with peer-credential authorisation and slow clients dropped;
  - the daemon-backed CLI and `penna waybar`;
  - 107 Rust tests and 8 plugin logic tests;
  - the popup running live in this shell against `pennad --mock`.
- **M3** — Studio and curve editor against the mock, with the §8.6 frame-timing test passing.
- **M4** — Real NVML control of the 3090 as root: hand-back, exit paths, `kill -9` on real hardware.
- **M5** — Motherboard fans: bind `it87` (stock first, the fork if the IT8688E needs it), `doctor`, pairing, calibration, contested detection on the IT8792E.
- **M6** — Packaging, README, first release.

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

## 12. Prior art, read but not copied

- **Fan Control** (Rem0o, Windows). Cards for controls, speeds and curves. Mix with Max, Min and Average. Response time, hysteresis and hysteresis direction, step up and step down, RPM avoid zones, start/stop logic for 0 RPM, calibration. Built on LibreHardwareMonitor, with .NET plugin DLLs. Taken: the card model and the curve vocabulary. Not taken: in-process plugins, because a root daemon must not load third-party code.
- **CoolerControl.** A headless daemon that serves its own web UI, REST and gRPC APIs, Prometheus metrics and the `cctv` CLI. Profiles (fixed, graph, mix, overlay), functions (hysteresis, thresholds, response time), modes, calibration, tags, stress tests and an auto-create wizard. Credited: overlay profiles (our offset), stress tests (v2). Different here: no HTTP server in a root process, only a local socket, and the UI belongs to the shell.
- **fan2go** (Go). One loop per fan. Adaptive measurement of start, max and interior points, kept in a database. Curves `linear`, `staircase`, `pid` and `function` (min, max, average, delta, sum, difference). The `pwmValueChangedByThirdParty` and `fanModeChangedByThirdParty` sanity checks; `controlMode.onExit: restore`; `setPwmToGetPwmMap` for fans that read back a different value than was written. Its documentation warns that the hwmon index "is not stable" and that NVIDIA RPM needs driver 565 or newer. Taken: third-party change detection (our `contested`) and the read-back map idea. Different: GUI first, and calibration shown in the editor.
- **lm_sensors `fancontrol` / `pwmconfig`.** The baseline. Its configuration names `hwmonN` paths, which break when the indices move.
- **frankcrawford/it87.** Supports IT8688E and IT8792E. MMIO is on by default and "necessary for fan control with newer Gigabyte motherboards". Full manual mode with workarounds for IT8688, IT8689, IT8790, IT8792/IT8795 and IT87952 since August 2026. Calls `ignore_resource_conflict` "inherently risky".

## 13. To verify on the desk

These need root or a shell restart, and were not run for this draft.

1. **Which chips.** `sudo modprobe it87 ignore_resource_conflict=1` with the stock driver, then read the kernel log. If only the IT8792E binds, the IT8688E needs the fork.
2. **Which headers each chip drives**, through pairing, and whether IT8792E writes hold or come back `contested`.
3. **BIOS Smart Fan settings** that manual control may need. The fork notes some Gigabyte boards need a BIOS curve workaround.
4. **NVML on the 3090 as root.** Set both fans to 40 %, read back percent and RPM, then `SetDefaultFanSpeed_v2` and confirm zero RPM returns at idle.
5. **`pwm_enable` after suspend and after hibernate.**
6. **Cost of a tick** (`strace -c` on the daemon). Goal: under 1 ms.
7. **The shell's render loop on this NVIDIA stack** (`QSG_INFO=1`), and a baseline frame-timing run of the shipped popups for comparison. A `threaded` loop keeps animation off the GUI thread. If it is `basic`, every glide competes with QML logic, and §8.6's budgets need re-checking against that.
