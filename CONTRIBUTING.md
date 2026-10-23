# Contributing to Penna

## A machine report

The most useful thing you can send is a machine report from a machine Penna hasn't seen. Every fan driver Penna knows has a
support tier (DESIGN.md §3.6), and only reports from real machines move it up:

| Tier | Means |
|---|---|
| **Verified** | Penna's exit-path hardware test passed on real hardware with this driver. |
| **Captured** | A real machine's capture with this driver runs in Penna's simulator. |
| **Documented** | Built from the driver's kernel source and documentation only. |

Today only `it87` and NVIDIA's NVML are Verified, both on the desk Penna was built on. Every other driver is Documented: it
works in the simulator, and nobody has run it on the hardware yet.

### Making one

```
penna capture
```

It needs no root, changes nothing, and writes `penna-capture-<machine>-<date>.tar.gz` in the current directory. Inside:

- the hwmon tree, `/sys/class/hwmon` and the devices behind it, with each file's mode (a `pwm1` that is read-only is a fact
  about the driver);
- the PCI display devices: vendor, device and driver;
- the DMI fields that name the machine and its firmware (`sys_vendor`, `product_name`, `board_name`, `bios_version`, …);
- the platform profile and its choices;
- the parameters of each fan driver's module (`thinkpad_acpi fan_control`, for one);
- `doctor.json`, the findings `penna doctor` prints;
- `measured.toml`, what setup measured, when you've run it, with each GPU UUID replaced by `GPU-0`, `GPU-1`, …;
- `capture.toml`: Penna's version, the date, the kernel.

It leaves out serial numbers, `product_uuid` and asset tags. Unpack it and read it before you send it. Penna never uploads
anything: sending a capture is your act.

Then open a [machine report](https://github.com/PushMotta/penna/issues/new?template=machine.yml) and attach the tarball.
Running setup first (`penna setup`, or *Set up fans…* in the bar popup) makes the report much more useful: it shows which
header drives which fan, where each fan stops and starts, and what Penna's curves would have been.

### What happens to it

A capture becomes a fixture under `test/fixtures/drivers/`, and the simulator runs Penna's whole lifecycle on it: discovery,
setup, saving, Defaults, every way out. A board's header names and roles go into `data/boards/`, one file per board, naming
your report as the source. No entry is written from memory.

## Code

The design comes first: [DESIGN.md](DESIGN.md) is the spec, and a change that alters behaviour changes it in the same commit.
Two rules are not up for negotiation:

- **Kernel drivers only.** Penna drives fans through hwmon and NVML and never touches a chip from userspace (§11.2).
- **Every fan goes back.** A new kind of control says how it is put back before it is changed, and every exit path is tested
  (§6).

Before sending a pull request:

```
cargo fmt --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cd plugin && node --test test/model.test.js test/studio.test.js
```

The tests never touch real fans: they run against the mock desk, the simulator and captured fixtures. `pennad` refuses the
real `/sys` unless it runs as root with a configuration, and the tests never run it that way.

The hardware tests in `tools/` (`m4-hardware-test.sh`, `m5-hardware-test.sh`) are different. They drive this desk's real
fans through every way out, run as root, and are written for its hardware. Read one before running it anywhere else.

### The plugin

`plugin/` is the Omarchy shell plugin, published on its own as
[omarchy-penna](https://github.com/PushMotta/omarchy-penna) because `omarchy plugin add` wants `manifest.json` at the root.
Develop it here, against the mock desk:

```
ln -s "$PWD/plugin" ~/.config/omarchy/plugins/io.github.pushmotta.penna
cargo run -p pennad -- --mock render-burst --forever
omarchy-restart-shell
```

Change it in this repository, not in omarchy-penna: that repository is regenerated from `plugin/` at each release.
