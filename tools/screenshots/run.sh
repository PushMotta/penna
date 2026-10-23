#!/usr/bin/env bash
# README screenshots and GIFs. Two sessions of the real popup and studio, against mock desks, on one screen covered by a plain
# backdrop, so nothing else on that screen is captured:
#   1. a tour: the x570 desk from a fresh install through setup, then the popup and the studio on Balanced. The screen is
#      recorded throughout, to session.mp4.
#   2. a ThinkPad, set up beforehand, for its popup and its studio.
#   3. the x570 desk, set up beforehand, its studio in the current theme and each of $THEMES.
# Stills are PNGs of the whole screen; gif.sh cuts and crops. The run takes about four minutes.
#
#   tools/screenshots/run.sh [OUT] [SCREEN]     OUT defaults to a new temporary directory; SCREEN to a screen not focused
#   PARTS="themes" tools/screenshots/run.sh …    only some of tour, laptop and themes
#
# Nothing takes the keyboard: the harness runs on a copy of the shell's Ui whose KeyboardPanel never asks for focus, and the
# studio opens without its grab. Needs a built pennad (cargo build), grim and wf-recorder.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
out=${1:-$(mktemp -d)}
mkdir -p "$out"
out=$(cd "$out" && pwd)
screen=${2:-$(hyprctl monitors -j | jq -r '[.[] | select(.focused | not)][0].name // .[0].name')}
themes=${THEMES:-"nord catppuccin-latte gruvbox rose-pine tokyo-night"}
work=$(mktemp -d)
daemon=""
recorder=""
cleanup() {
  [ -n "$recorder" ] && kill -INT "$recorder" 2>/dev/null && wait "$recorder" 2>/dev/null
  [ -n "$daemon" ] && kill "$daemon" 2>/dev/null && wait "$daemon" 2>/dev/null
  rm -rf "$work"
}
trap cleanup EXIT

echo "== Themes: $themes"
for t in $themes; do
  next="$work/home/.local/state/omarchy/current/next-theme"
  rm -rf "$work/home" && mkdir -p "$next"
  cp "/usr/share/omarchy/themes/$t/colors.toml" "$next/"
  # The shell's own renderer, in a home of its own: the user's theme state is never touched.
  HOME="$work/home" OMARCHY_PATH=/usr/share/omarchy /usr/bin/omarchy-theme-set-templates >/dev/null 2>&1
  mkdir -p "$work/themes/$t"
  cp "$next/colors.toml" "$next/shell.toml" "$work/themes/$t/"
done
mkdir -p "$work/themes/current"
cp "$HOME/.local/state/omarchy/current/theme/colors.toml" "$HOME/.local/state/omarchy/current/theme/shell.toml" "$work/themes/current/"

echo "== The harness"
h="$work/harness"
mkdir -p "$h"
cp "$here/shell.qml" "$h/"
# Every theme's files, read once here: a FileView whose path changes can hand back the previous file's text.
python3 -I - "$work/themes" $themes current >"$h/themes.js" <<'EOF'
import json, pathlib, sys
root = pathlib.Path(sys.argv[1])
themes = {n: {"colors": (root / n / "colors.toml").read_text(), "shell": (root / n / "shell.toml").read_text()} for n in sys.argv[2:]}
print(".pragma library\nvar themes = " + json.dumps(themes) + ";")
EOF
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

shell() {
  PENNA_SOCKET="$1" SHOT_OUT="$out" SHOT_SCREEN="$screen" SHOT_MODE="$2" \
    timeout 400 quickshell -p "$h" --no-color >"$out/$2.log" 2>&1 || true
  grep -E "SHOT|MARK" "$out/$2.log" || true
}

mock() {
  "$repo/target/debug/pennad" --mock "$1" --forever --quiet --speed "$2" --tree "$work/tree-$3" \
    --boards "$repo/data/boards" --socket "$work/$3.sock" 2>"$out/pennad-$3.log" &
  daemon=$!
  for _ in $(seq 1 100); do [ -S "$work/$3.sock" ] && return; sleep 0.1; done
  echo "pennad --mock $1 never listened" >&2
  exit 1
}

# A desk set up from outside the shell: setup run to its review over the socket, and its curves saved.
set_up() {
  python3 -I - "$1" <<'EOF'
import json, socket, sys, time
s = socket.socket(socket.AF_UNIX); s.connect(sys.argv[1])
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
assert call("setup.start")["ok"]
deadline = time.time() + 240
while (call("state")["result"].get("setup") or {}).get("step") != "review":
    if time.time() > deadline: sys.exit("setup never reached its review")
    time.sleep(0.5)
time.sleep(1)
assert call("draft.save")["ok"]
time.sleep(16)
EOF
}

stop_mock() {
  kill "$daemon" && wait "$daemon" 2>/dev/null || true
  daemon=""
}

for part in ${PARTS:-tour laptop themes}; do
  case $part in
    tour)
      echo "== The tour, on $screen, recorded"
      mock x570-desk 20 tour
      wf-recorder -o "$screen" -r 30 -f "$out/session.mp4" >"$out/recorder.log" 2>&1 &
      recorder=$!
      echo "RECORDING $(date +%s%3N)" >"$out/marks.txt"
      sleep 1
      shell "$work/tour.sock" tour | tee -a "$out/marks.txt"
      kill -INT "$recorder" && wait "$recorder" 2>/dev/null || true
      recorder=""
      stop_mock
      ;;
    laptop)
      echo "== A ThinkPad, set up"
      mock "$repo/test/fixtures/drivers/thinkpad.toml" 20 laptop
      set_up "$work/laptop.sock"
      shell "$work/laptop.sock" laptop
      stop_mock
      ;;
    themes)
      echo "== The studio in each theme"
      mock x570-desk 20 themes
      set_up "$work/themes.sock"
      shell "$work/themes.sock" themes
      stop_mock
      ;;
  esac
done
ls -1 "$out"
