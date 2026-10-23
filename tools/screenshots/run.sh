#!/usr/bin/env bash
# README and marketplace screenshots (M10). A mock of this desk runs setup and saves its curves, then the real popup and studio
# show on one screen for about ten seconds and grim captures each. The PNGs are the whole screen: crop them to the card
# before using them, since whatever else is on that screen is in them too.
#
#   tools/screenshots/run.sh [OUT] [SCREEN]     OUT defaults to a new temporary directory; SCREEN to a screen not focused
#
# Nothing takes the keyboard: the harness runs on a copy of the shell's Ui whose KeyboardPanel never asks for focus, and the
# studio is opened without its keyboard grab. Needs a built pennad and penna (cargo build).
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
out=${1:-$(mktemp -d)}
mkdir -p "$out"
out=$(cd "$out" && pwd)
screen=${2:-$(hyprctl monitors -j | jq -r '[.[] | select(.focused | not)][0].name // .[0].name')}
work=$(mktemp -d)
trap 'kill $daemon 2>/dev/null; wait $daemon 2>/dev/null; rm -rf "$work"' EXIT

echo "== The mock desk, setup and its curves saved"
"$repo/target/debug/pennad" --mock x570-desk --forever --quiet --speed 20 --tree "$work/tree" \
  --boards "$repo/data/boards" --socket "$work/penna.sock" 2>"$work/pennad.log" &
daemon=$!
python3 -I - "$work/penna.sock" <<'EOF'
import json, socket, sys, time
deadline = time.time() + 20
while True:
    try:
        s = socket.socket(socket.AF_UNIX); s.connect(sys.argv[1]); break
    except OSError:
        if time.time() > deadline: sys.exit("pennad --mock never listened")
        time.sleep(0.1)
f = s.makefile("rw")
f.readline()
n = 0
def call(op, **args):
    global n
    n += 1
    f.write(json.dumps({"id": n, "op": op, **args}) + "\n"); f.flush()
    while True:
        v = json.loads(f.readline())
        if v.get("id") == n: return v
def setup_step():
    st = call("state")["result"]
    return (st.get("setup") or {}).get("step")
assert call("setup.start")["ok"], "setup didn't start"
deadline = time.time() + 240
while setup_step() != "review":
    if time.time() > deadline: sys.exit("setup never reached its review")
    time.sleep(0.5)
time.sleep(2)
reply = call("draft.save")
assert reply["ok"], reply
# A minute of the desk on Balanced, so the sparklines are full.
time.sleep(4)
print("   saved:", call("state")["result"]["profile"])
EOF

echo "== The popup and the studio on $screen"
h="$work/harness"
mkdir -p "$h"
cp "$here/shell.qml" "$h/"
ln -s "$repo/plugin" "$h/penna"
ln -s /usr/share/omarchy/shell/Commons "$h/Commons"
cp -r /usr/share/omarchy/shell/Ui "$h/Ui"
python3 -I - "$h/Ui/KeyboardPanel.qml" <<'EOF'
import sys
p = sys.argv[1]
s = open(p).read()
old = """  WlrLayershell.keyboardFocus: open
    ? (focusPrimed ? WlrKeyboardFocus.OnDemand : WlrKeyboardFocus.Exclusive)
    : WlrKeyboardFocus.None"""
assert old in s, "the shell's KeyboardPanel changed: update this patch"
open(p, "w").write(s.replace(old, "  WlrLayershell.keyboardFocus: WlrKeyboardFocus.None"))
EOF
PENNA_SOCKET="$work/penna.sock" SHOT_OUT="$out" SHOT_SCREEN="$screen" timeout 60 quickshell -p "$h" --no-color \
  >"$out/quickshell.log" 2>&1 || true
grep -E "SHOT" "$out/quickshell.log" || true
ls -1 "$out"
