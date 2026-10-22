#!/usr/bin/env bash
# Renders setup's view offscreen at each step of a run on the mock desk (DESIGN.md §5, design review 03), as PNGs in $1.
# Needs the Omarchy shell's Commons/Ui and a built pennad. Shows nothing on screen.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
out=${1:-$(mktemp -d)}
mkdir -p "$out"
work=$(mktemp -d)
"$repo/target/debug/pennad" --mock x570-desk --forever --quiet --speed "${SPEED:-20}" --tree "$work/tree" \
  --boards "$repo/data/boards" --socket "$work/penna.sock" 2>"$work/pennad.log" &
daemon=$!
trap 'kill $daemon 2>/dev/null; wait $daemon 2>/dev/null; rm -rf "$work"' EXIT
sleep 1
PENNA_SOCKET="$work/penna.sock" PREVIEW_OUT="$out" PREVIEW_SPEED="${SPEED:-20}" QT_QPA_PLATFORM=offscreen timeout 120 quickshell -p "$here" --no-color 2>&1 \
  | grep -vE "^\s*$|WAYLAND_DISPLAY|--- WARNING|If you are actually|window masks|Configuration Loaded" || true
ls "$out"
