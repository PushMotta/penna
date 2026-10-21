#!/usr/bin/env bash
# Frame-timing test for DESIGN.md §8.6. Needs `pennad --mock … --forever` running and the Omarchy shell's Commons/Ui.
# For about 50 s it shows the popup's rows and then the studio on the second screen (keyboard untouched), and prints
# frame pacing per phase plus the render thread's own timings. Exits non-zero when a budget is missed.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
hz=$(hyprctl monitors -j | python3 -I -c 'import json,sys; print(max(m["refreshRate"] for m in json.load(sys.stdin)))')
log=$(mktemp --suffix=.penna-frames.log)

PENNA_REFRESH_HZ="$hz" QSG_INFO=1 QSG_RENDER_TIMING=1 \
  timeout 120 quickshell -p "$here" --no-color \
    --log-rules "qt.scenegraph.time.renderloop=true;qt.scenegraph.general=true" >"$log" 2>&1 || true

python3 -I - "$log" "$hz" <<'EOF'
import json, re, sys
log, hz = sys.argv[1], float(sys.argv[2])
text = open(log, errors="replace").read()
text = re.sub(r"\x1b\[[0-9;]*m", "", text)
budget = 1000 / hz
result = None
for line in text.splitlines():
    if "PENNA_RESULT " in line:
        result = json.loads(line.split("PENNA_RESULT ", 1)[1])
loop = re.search(r"qt\.scenegraph\.general: (threaded|basic|windows) render loop", text)
# Each frame's own cost on the render thread is sync + render; swap includes the wait for vsync, so it is left out.
renders = [float(s) + float(r) for s, r in re.findall(r"syncAndRender: frame rendered in \d+ms, sync=(\d+), render=(\d+), swap=\d+", text)]
renders.sort()
def pct(xs, p): return xs[min(len(xs) - 1, int(p * len(xs)))] if xs else 0.0
print(f"refresh {hz:.2f} Hz, budget {budget:.1f} ms; render loop: {loop.group(1) if loop else 'not reported'}")
if not result:
    print("no result: the harness did not finish (log: %s)" % log); sys.exit(2)
if result["error"]:
    print("harness error:", result["error"]); sys.exit(2)
ok = True
for phase in ("popup", "studio", "drag"):
    s = result[phase]
    line = (f"{phase:7} {s['frames']:5d} frames  interval p50 {s['p50']:5.1f}  p95 {s['p95']:5.1f}  p99 {s['p99']:5.1f}"
            f"  max {s['max']:5.1f} ms  dropped {s['dropped']}  first 0.5 s max {s['first']:5.1f} ms")
    print(line)
    if s.get("slowAfterTick"):
        print(f"        slow frames came this many ms after a tick: {sorted(s['slowAfterTick'])}")
    if s["dropped"] > 0: ok = False
if renders:
    p99 = pct(renders, 0.99)
    print(f"render thread (sync + render, whole-ms resolution): {len(renders)} frames, p50 {pct(renders, 0.5):.0f} ms, "
          f"p99 {p99:.0f} ms, max {renders[-1]:.0f} ms (half the budget is {budget / 2:.1f} ms)")
    if p99 > budget / 2: ok = False
else:
    print("render thread: no QSG_RENDER_TIMING lines in the log")
print("PASS" if ok else "FAIL", f"(log: {log})")
sys.exit(0 if ok else 1)
EOF
