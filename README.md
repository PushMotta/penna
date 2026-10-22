# Penna

Fan control for Linux, made to be understood. The design is in [DESIGN.md](DESIGN.md). Penna drives NVIDIA fans (M4) and every
hwmon fan of a kind it knows (M5: DESIGN.md §3.5), runs as a service (M6), and is meant for any workstation or
laptop: it builds on the kernel's own drivers and says plainly what a machine can't do.

```
cargo run -p pennad -- --mock render-burst --forever   # the mock desk, listening on $XDG_RUNTIME_DIR/penna/penna.sock
cargo run -p pennad -- --mock faults --speed 10        # every fault Penna must survive, ten times faster

cargo run -p penna -- watch                            # live table: from pennad when it answers, else the real machine, read-only
cargo run -p penna -- state [--json]                   # what pennad is doing now
cargo run -p penna -- profile quiet                    # switch profile, and keep it
cargo run -p penna -- profile defaults                 # every fan back to its BIOS or driver, and kept there
cargo run -p penna -- reset                            # the same; without pennad (as root) it restores held.json itself
cargo run -p penna -- set "CPU cooler" 70 --for 30s    # hold a fan (or `all`); `auto` puts it back on its curve
cargo run -p penna -- full-speed on|off
cargo run -p penna -- events                           # takes, hand-backs, failsafes, emergencies, as they happen
cargo run -p penna -- waybar [--follow]                # one JSON line for a Waybar custom module

cargo run -p penna -- check test/fixtures/scenarios/mock-desk.config.toml
cargo run -p penna -- restore --from <held.json>       # put every fan back without the daemon (ExecStopPost)
cargo run -p penna -- capture <dir>                    # read-only snapshot of /sys/class/hwmon for fixtures
```

`socat - UNIX-CONNECT:$XDG_RUNTIME_DIR/penna/penna.sock` speaks the protocol by hand: type `{"id":1,"op":"subscribe"}`.

## The Omarchy plugin

`plugin/` is the shell plugin: the bar glyph, the popup, and the service that holds the one socket to `pennad`. To run it
against the mock desk:

```
ln -s ~/Projects/penna/plugin ~/.config/omarchy/plugins/io.github.pushmotta.penna
omarchy plugin enable io.github.pushmotta.penna
cargo run -p pennad -- --mock render-burst --forever
penna open                                                   # open the studio (or click the glyph, then Curves…)
node --test plugin/test/model.test.js plugin/test/studio.test.js
```

`tools/frame-timing/run.sh` is the §8.6 frame-timing test. With `pennad --mock … --forever` running, it shows the popup's
rows and then the studio on the second screen for about a minute: 5 s of popup, 30 s of studio, a 10 s scripted drag. It
doesn't take the keyboard. It prints frame pacing per phase and the render thread's own timings, and exits non-zero when a
budget is missed.

With the plugin symlinked, the shell doesn't notice edits inside it. It also keeps a plugin's scripts in memory across its own
plugin reload. So after editing, restart the shell with `omarchy-restart-shell`. `omarchy plugin disable
io.github.pushmotta.penna` takes the widget off the bar.

## Installing (M6)

```
cd packaging && makepkg -f --nodeps     # --nodeps when cargo comes from rustup rather than pacman
pkexec pacman -U penna-0.6.0-*-x86_64.pkg.tar.zst
sudo systemctl enable --now pennad      # the first start writes /etc/penna/config.toml, adopting nothing
sudo usermod -aG penna "$USER"          # change fans from the popup and the studio; applies at once
penna doctor                            # what Penna can and can't reach on this machine
```

On this desk, `pkexec tools/install-desk.sh` does all of that, and installs the RTX 3090 config the M4 hardware test used.
`pkexec pacman -R penna` stops `pennad` first, which hands every fan back.

The package holds `/usr/bin/pennad`, `/usr/bin/penna`, the systemd unit (`packaging/pennad.service`: `Type=notify`,
watchdog, `ExecStopPost=penna restore`, `Restart=on-failure`, hardened but allowed `/sys` and `/dev/nvidia*`), the `penna`
group (`sysusers.d`), and the plugin under `/usr/share/penna/plugin`.

## Real hardware (M4: NVIDIA fans, M5: motherboard fans)

Production `pennad` runs as root and controls NVIDIA fans and every hwmon fan of a kind it drives. New hwmon devices are found
while it runs, so a driver loaded later adds its fans without a restart.

```
sudo ./target/debug/pennad --config test/fixtures/desk-3090.config.toml   # this desk's RTX 3090
python3 -I tools/nvml-state.py                                            # what NVML says about every fan, no root
pkexec tools/m4-hardware-test.sh                                          # every way out hands the fans back (§9.4)
pkexec tools/m5-hardware-test.sh                                          # the same for every it87 channel, plus contests and pairing
pkexec tools/m5-hardware-test.sh pairing                                  # which header each control drives, alone
PENNA_BOARDS=data/boards cargo run -p penna -- doctor                     # with this checkout's board database
```

The hardware test runs `pennad` as a transient systemd service, holds the fans at 45 %, and ends the daemon six ways:
stop, `kill -9`, a whole-unit kill with restart, panic, a watchdog-caught hang, and idle. After each, it checks through NVML
that both fans are back on the driver's curve. `pkexec` asks for the password through the shell's polkit dialog, which also
works from tools that have no terminal.

## Tests

`cargo test --workspace` runs the control loop on simulated time (including property tests), the real hwmon backend against
`test/fixtures/desk-2026-10-21` and against the simulator's tree, `pennad` itself through `kill -9`, SIGTERM and a panic, and a
client against the daemon's socket, including one that stops reading.
