# What Penna supports

Penna doesn't support motherboards or laptops one by one. It drives fans through the kernel's own drivers, so a machine works
when its fan chip has a driver Penna knows and that driver is loaded. The tables below are generated from Penna's own code and
data, so they say exactly what this version knows.

## Will it work on my machine?

```
penna doctor
```

It needs no root and changes nothing. It names:
- the board;
- each fan chip and its driver, and whether Penna can write to it;
- the driver's support tier;
- one thing to do about anything missing: a driver to install, a module option, another fan daemon in the way.

Before installing Penna, `sensors` (from lm_sensors) shows which drivers are bound. A fan chip's driver there, such as `it87` or
`nct6775`, is one Penna drives.

When there are no motherboard fans at all, it is usually one of two things:
- **The kernel's driver doesn't know the chip.** Many Gigabyte boards have an ITE IT8688E, which only the out-of-tree it87 fork
  knows.
- **The firmware claims the chip's ports.** The boot log says "ACPI resource conflict". The doctor names both, and the risk of
  the fix.
- **The firmware runs the fan through ACPI.** Many mini PCs, such as an Intel N100 box, show their fan only as ACPI fan devices
  that are on or off, with no speed to read. Penna can neither read nor drive those, and the doctor says so. An ACPI fan that
  reports its speed is read as `acpi_fan`.

## Systems

- **Linux with systemd.** `pennad` is a systemd service. The `penna` command line works anywhere.
- **Arch Linux and its derivatives:** the package on each [release](https://github.com/PushMotta/penna/releases), for x86_64.
  Elsewhere, build from source with Rust 1.85 or newer (`cargo build --release`) and install the unit, the group and the
  binaries as `packaging/PKGBUILD` does.
- **Omarchy, for the bar plugin.** Tested on Omarchy 4.0 with Quickshell 0.3.2. On other desktops, use `penna waybar` for
  Waybar, or the command line.
- **Kernel:** tested on 7.2. The drivers are documented against Linux 7.3. A driver newer than the running kernel just isn't
  there, and the doctor says so.
- **NVIDIA GPUs:** NVIDIA's own driver, proprietary or open, 520 or newer for fan control and 565 or newer for fan RPM. Those
  are the releases that added the NVML calls Penna uses, per NVML's changelog. Tested with 615.
- **AMD GPUs:** `amdgpu`. On RDNA4, writes need the module option the table below names.

## Fan drivers

<!-- generated: drivers -->
| Driver | Registers as | Hardware | Penna can | Before Penna can write | Tier |
|---|---|---|---|---|---|
| NVML | NVML | NVIDIA GPUs | set a duty | NVIDIA's driver 520 or newer; 565 for RPM | Verified |
| `it87` | `it87`, `it8*` | Motherboard | set a duty | — | Verified |
| `nct6775` | `nct61*`, `nct67*` | Motherboard | set a duty | — | Documented |
| `nct6683` | `nct6683`, `nct6686`, `nct6687` | Motherboard | set a duty | — | Documented |
| `f71882fg` | `f718*`, `f8000`, `f8186*` | Motherboard | set a duty | — | Documented |
| `w83627ehf` | `w83*` | Motherboard | set a duty | — | Documented |
| `amdgpu` | `amdgpu` | AMD GPU | set a duty, or write its OverDrive curve where `pwm1` is read-only (RDNA4) | `amdgpu ppfeaturemask=0xfff7ffff` | Documented |
| `nouveau` | `nouveau` | NVIDIA GPU (nouveau) | set a duty | — | Documented |
| `gpd-fan` | `gpdfan` | GPD handheld | set a duty | — | Documented |
| `cros_ec_hwmon` | `cros_ec` | Embedded controller | set a duty | — | Documented |
| `acer-wmi` | `acer` | Acer laptop | set a duty | — | Documented |
| `thinkpad_acpi` | `thinkpad` | ThinkPad | pick one of 8 levels, under the firmware's watchdog | `thinkpad_acpi fan_control=1` | Documented |
| `dell-smm-hwmon` | `dell_smm` | Dell laptop | pick one of 3 levels | — | Documented |
| `asus-wmi` | `asus` | ASUS laptop | automatic or full speed; a duty where the channel has one | — | Documented |
| `asus-wmi custom fan curves` | `asus_custom_fan_curve` | ASUS fan curves | write the firmware's 8-point curve | — | Documented |
| `hp-wmi` | `hp` | HP laptop | automatic or full speed | — | Documented |
| `alienware-wmi` | `alienware_wmi` | Alienware | read only | — | Documented |
| `lenovo-wmi-other` | `lenovo_wmi_other` | Lenovo laptop | set a target speed | — | Documented |
| `applesmc` | `applesmc` | Mac | set a target speed | — | Documented |
| `legion-laptop` | `legion_hwmon` | Lenovo Legion | read only | — | Documented |
| `acpi-fan` | `acpi_fan` | ACPI fan | read only | — | Documented |
| others | `system76_acpi`, `surface_fan`, `i915`, `xe`, `msi_wmi_platform` | System76, Surface, Intel GPUs (`i915`, `xe`), MSI laptops | read only | — | Documented |
<!-- end: drivers -->

The tiers:
- **Verified:** Penna's exit-path hardware test passed on real hardware with this driver.
- **Captured:** a real machine's capture with this driver runs in Penna's simulator.
- **Documented:** built from the driver's kernel source and documentation. It works in the simulator, and nobody has run
  Penna on that hardware yet.

A [machine report](../CONTRIBUTING.md#a-machine-report) moves a driver up.

## Motherboard fan chips

Each motherboard driver handles the chips its documentation lists. Penna matches a chip by the name its driver registers, so a
chip the driver learns later works too.

<!-- generated: chips -->
- **`it87`**: IT8603E/IT8623E, IT8620E, IT8628E, IT8689E, IT8705F, IT8712F, IT8716F/IT8726F, IT8718F, IT8720F, IT8721F/IT8758E, IT8728F, IT8732F, IT8771E, IT8772E, IT8781F, IT8782F, IT8783E/F, IT8786E, IT8790E, IT8792E/IT8795E, IT87952E, SiS950 [clone of IT8705F].
  <sub>From Linux 7.4 (734bef548), Documentation/hwmon/it87.rst.</sub>
- **`nct6775`**: Nuvoton NCT6102D/NCT6104D/NCT6106D, Nuvoton NCT5572D/NCT6771F/NCT6772F/NCT6775F/W83677HG-I, Nuvoton NCT5573D/NCT5577D/NCT6776D/NCT6776F, Nuvoton NCT5532D/NCT6779D, Nuvoton NCT6791D, Nuvoton NCT6792D, Nuvoton NCT6793D, Nuvoton NCT6795D, Nuvoton NCT6796D, Nuvoton NCT6797D, Nuvoton NCT6798D, Nuvoton NCT5585D (compatible to NCT6798D), Nuvoton NCT6796D-S/NCT6799D-R.
  <sub>From Linux 7.4 (734bef548), Documentation/hwmon/nct6775.rst.</sub>
- **`nct6683`**: Nuvoton NCT6683D/NCT6686D/NCT6687D.
  <sub>From Linux 7.4 (734bef548), Documentation/hwmon/nct6683.rst.</sub>
- **`f71882fg`**: Fintek F71808E, Fintek F71808A, Fintek F71858FG, Fintek F71862FG and F71863FG, Fintek F71869F and F71869E, Fintek F71869A, Fintek F71882FG and F71883FG, Fintek F71889FG, Fintek F71889ED, Fintek F71889A, Fintek F8000, Fintek F81801U, Fintek F81865F.
  <sub>From Linux 7.4 (734bef548), Documentation/hwmon/f71882fg.rst.</sub>
- **`w83627ehf`**: Winbond W83627EHF/EHG (ISA access ONLY), Winbond W83627DHG, Winbond W83627DHG-P, Winbond W83627UHG, Winbond W83667HG, Winbond W83667HG-B, Nuvoton NCT6775F/W83667HG-I, Nuvoton NCT6776F.
  <sub>From Linux 7.4 (734bef548), Documentation/hwmon/w83627ehf.rst.</sub>
- **`it87-dkms-git`**: IT8603E, IT8606E, IT8607E, IT8613E, IT8620E, IT8622E, IT8623E, IT8625E, IT8628E, IT8528E, IT8655E, IT8665E, IT8686E, IT8688E, IT8689E, IT8696E, IT8698E, IT8705F, IT8712F, IT8716F, IT8718F, IT8720F, IT8721F, IT8726F, IT8728F, IT8732F, IT8736F, IT8738E, IT8758E, IT8771E, IT8772E, IT8781F, IT8782F, IT8783E/F, IT8786E, IT8790E, IT8792E, IT87952E, Sis950.
  <sub>From github.com/frankcrawford/it87 at bd7a566, README.</sub>
<!-- end: chips -->

## Boards Penna knows by name

Any board with a supported chip works. On these, Penna also names each fan header as the board does, gives setup's roles a
head start, and says which driver to install. Each comes from a machine report or a public source, named in its file under
`data/boards`. [Add yours](../CONTRIBUTING.md#a-machine-report).

<!-- generated: boards -->
| Board | Chips | Headers, as the board names them | Driver |
|---|---|---|---|
| Gigabyte Technology Co., Ltd. X570 AORUS PRO | IT8688E, IT8792E | `CPU_FAN`, `SYS_FAN1`, `SYS_FAN2`, `PCH_FAN`, `CPU_OPT`, `SYS_FAN5_PUMP`, `SYS_FAN6_PUMP`, `SYS_FAN4` | `it87` from `it87-dkms-git` |
<!-- end: boards -->

## Temperatures

Penna reads every hwmon temperature, and each NVIDIA GPU's through NVML. A sensor of these drivers is shown by what it measures:

<!-- generated: sensors -->
| hwmon name | Shown as |
|---|---|
| `k10temp` | AMD CPU |
| `zenpower` | AMD CPU |
| `coretemp` | Intel CPU |
| `nvme` | NVMe |
| `drivetemp` | Drive |
| `acpitz` | ACPI thermal zone |
| `iwlwifi` | Wi-Fi |
| `asus_ec_sensors` | Motherboard |
| `gigabyte_wmi` | Motherboard |

A sensor that reports a critical or GPU-max limit of its own has that as its emergency limit: an NVIDIA GPU's "GPU max", an amdgpu or NVMe sensor's critical temperature. A CPU sensor without one uses its model's maximum, from AMD's published figures:

| CPU | Limit |
|---|---|
| Ryzen 9 5950X | 90 °C |
| Ryzen 9 5900X | 90 °C |
| Ryzen 7 5800X | 90 °C |
| Ryzen 5 5600X | 95 °C |
| Ryzen 9 7950X | 95 °C |

Any other CPU gets 95 °C. Any other sensor declares an emergency only when a curve reads it, at 95 °C: Super I/O chips report unconnected inputs as 127 °C. A reading outside -40…150 °C is a failed read, and a limit outside 40…130 °C is ignored as a firmware placeholder. A sensor that reads below 20 °C, such as a firmware's fixed ACPI zone, is marked as below room temperature and set apart.
<!-- end: sensors -->

## Not supported

- **Fan chips with no kernel driver.** Penna never touches hardware from userspace. A chip gets support when the kernel, or an
  out-of-tree driver, does.
- **USB coolers and fan hubs** (AIO pumps, Corsair, NZXT, Lian Li controllers): not yet.
- **A GPU passed through to a virtual machine:** its fans belong to the guest. The doctor names it.
- **Windows and macOS.**
