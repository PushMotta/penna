# Penna

Fan control for Linux, made to be understood. The design is in [DESIGN.md](DESIGN.md). This is milestone M2: the control loop,
its safety paths, a simulated desk, and the socket surfaces talk to (protocol v2, DESIGN.md §7.3). Nothing here writes to real
hardware; real control arrives in M4 (NVIDIA) and M5 (motherboard).

```
cargo run -p pennad -- --mock render-burst --forever   # the mock desk, listening on $XDG_RUNTIME_DIR/penna/penna.sock
cargo run -p pennad -- --mock faults --speed 10        # every fault Penna must survive, ten times faster

cargo run -p penna -- watch                            # live table: from pennad when it answers, else the real machine, read-only
cargo run -p penna -- state [--json]                   # what pennad is doing now
cargo run -p penna -- profile quiet                    # switch profile, and keep it
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
omarchy-shell shell toggle io.github.pushmotta.penna '{}'   # open the popup
node --test plugin/test/model.test.js                        # the popup's logic
```

With the plugin symlinked, the shell doesn't notice edits inside it. It also keeps a plugin's scripts in memory across its own
plugin reload. So after editing, restart the shell with `omarchy-restart-shell`. `omarchy plugin disable
io.github.pushmotta.penna` takes the widget off the bar.

## Tests

`cargo test --workspace` runs the control loop on simulated time (including property tests), the real hwmon backend against
`test/fixtures/desk-2026-10-21` and against the simulator's tree, `pennad` itself through `kill -9`, SIGTERM and a panic, and a
client against the daemon's socket, including one that stops reading.
