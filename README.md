# Penna

Fan control for Linux, made to be understood. The design is in [DESIGN.md](DESIGN.md). This is milestone M1: the control loop,
its safety paths and a simulated desk. Nothing here writes to real hardware; real control arrives in M4 (NVIDIA) and M5 (motherboard).

```
cargo run -p pennad -- --mock render-burst             # a render on the mock desk, one line a tick
cargo run -p pennad -- --mock faults --speed 10        # every fault Penna must survive, ten times faster
cargo run -p penna -- check test/fixtures/scenarios/mock-desk.config.toml
cargo run -p penna -- restore --from <held.json>       # put every fan back without the daemon (ExecStopPost)
cargo run -p penna -- watch                            # M0: the real machine, read-only
cargo run -p penna -- capture <dir>                    # M0: read-only snapshot of /sys/class/hwmon for fixtures
```

`cargo test --workspace` runs the control loop on simulated time (including property tests), the real hwmon backend against
`test/fixtures/desk-2026-10-21` and against the simulator's tree, and `pennad` itself through `kill -9`, SIGTERM and a panic.
