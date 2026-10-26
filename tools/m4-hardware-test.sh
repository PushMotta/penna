#!/usr/bin/env bash
# M4 hardware test (DESIGN.md §6, §10): Penna drives the RTX 3090's fans for real and must hand them back to the driver on
# every way out. It runs pennad as a transient systemd service (Type=notify, WatchdogSec=5, ExecStopPost=penna restore)
# with test/fixtures/desk-3090.config.toml, copied to a scratch directory so root never writes into the repo.
#
#   sudo tools/m4-hardware-test.sh
#
# Takes about three minutes. The GPU fans are held at 45 % for a few seconds at a time, so you will hear them. Whatever
# happens, the cleanup hands both fans back to the driver (penna restore, then NVML directly as a last resort).
set -uo pipefail

here=$(cd "$(dirname "$0")" && pwd)
repo=$(dirname "$here")
bin="$repo/target/debug"
unit=penna-m4
work=/run/penna-m4
cfg=$work/config.toml
sock=/run/penna/penna.sock
held=/run/penna/held.json
export PENNA_SOCKET=$sock
fails=0

say() { printf '\n== %s\n' "$*"; }
state() { python3 -I "$here/nvml-state.py" "$@"; }
policies() { state --json | python3 -I -c 'import json,sys; print(" ".join(f["policy"] for g in json.load(sys.stdin) for f in g["fans"]))'; }

# expect <driver|manual> <what>: both fans must be on that policy.
expect() {
  local got
  got=$(policies)
  if [ "$got" = "$1 $1" ]; then
    echo "PASS  $2 ($got)"
  else
    echo "FAIL  $2: wanted $1 on both fans, got $got"
    fails=$((fails + 1))
  fi
  state
}

# start [pennad flags...]: a fresh transient unit, waiting until pennad answers. Extra unit properties go in $props.
# The unit's own sandboxing (packaging/pennad.service), so pennad runs here as the package runs it. ProtectHome is left out:
# this pennad is the checkout's, under /home. strict leaves $work read-only, and the test's configuration lives there.
hardening=(-p "ReadWritePaths=$work" -p StateDirectory=penna)
while IFS= read -r line; do
  case "$line" in ProtectHome=*) ;; *) hardening+=(-p "$line") ;; esac
done < <(sed -n '/^# Hardening/,/^\[Install\]/{/^[A-Za-z]/p}' "$repo/packaging/pennad.service")

props=()
start() {
  systemctl reset-failed "$unit" 2>/dev/null
  systemd-run --quiet --unit="$unit" --service-type=notify \
    -p WatchdogSec=5 -p WatchdogSignal=SIGKILL \
    -p RuntimeDirectory=penna -p RuntimeDirectoryPreserve=yes \
    -p "ExecStopPost=$bin/penna restore --from $held" "${hardening[@]}" "${props[@]}" \
    -- "$bin/pennad" --config "$cfg" --quiet "$@"
  for _ in $(seq 1 40); do
    "$bin/penna" state >/dev/null 2>&1 && return 0
    sleep 0.25
  done
  echo "FAIL  pennad did not answer on $sock"
  journalctl -u "$unit" -n 20 --no-pager
  fails=$((fails + 1))
  return 1
}

# hold <percent> <seconds>: both fans, through the daemon, the way a person would.
hold() {
  "$bin/penna" set "GPU fan 1" "$1" --for "${2}s" >/dev/null && "$bin/penna" set "GPU fan 2" "$1" --for "${2}s" >/dev/null
}

# wait_inactive <seconds>: until the unit has fully stopped, ExecStopPost included ("deactivating" is not stopped).
wait_inactive() {
  for _ in $(seq 1 "$1"); do
    case "$(systemctl show -p ActiveState --value "$unit" 2>/dev/null)" in
      inactive | failed | "") return 0 ;;
    esac
    sleep 1
  done
  echo "FAIL  pennad was still running after $1 s"
  fails=$((fails + 1))
  return 1
}

cleanup() {
  systemctl stop "$unit" 2>/dev/null
  systemctl reset-failed "$unit" 2>/dev/null
  [ -f "$held" ] && "$bin/penna" restore --from "$held"
  if [ "$(policies)" != "driver driver" ]; then
    echo "cleanup: a fan is still manual; handing both to the driver through NVML"
    state --default >/dev/null
  fi
  rm -f "$sock"
  rm -rf "$work"
  say "After the test"
  state
  if [ "$fails" -eq 0 ]; then echo "M4 hardware test: PASS"; else echo "M4 hardware test: $fails FAILED"; fi
}

# ------------------------------------------------------------------ preflight
[ "$(id -u)" = 0 ] || { echo "This drives the GPU's fans and needs root: sudo $0"; exit 2; }
[ -x "$bin/pennad" ] && [ -x "$bin/penna" ] || { echo "Build first, as your user: cargo build -p pennad -p penna"; exit 2; }
temp=$(state --json | python3 -I -c 'import json,sys; print(json.load(sys.stdin)[0]["temp"])')
[ "$temp" -lt 70 ] || { echo "The GPU is at $temp °C. Run this while it is idle."; exit 2; }
"$bin/penna" state >/dev/null 2>&1 && { echo "A pennad already answers on $sock. Stop it first."; exit 2; }
mkdir -p "$work"
cp "$repo/test/fixtures/desk-3090.config.toml" "$cfg"
"$bin/penna" check "$cfg" || { echo "penna check refused the desk config."; exit 2; }
trap cleanup EXIT

say "Before"
state

say "1 · systemctl stop (SIGTERM): pennad hands both fans back itself"
if start; then
  hold 45 120
  sleep 6
  expect manual "held at 45 % by Penna"
  systemctl stop "$unit"
  sleep 1
  expect driver "after systemctl stop"
  if [ -f "$held" ]; then echo "FAIL  held.json was left behind"; fails=$((fails + 1)); fi
fi

say "2 · kill -9 of pennad: nothing in it runs; ExecStopPost hands the fans back"
if start; then
  hold 45 120
  sleep 5
  expect manual "held at 45 %"
  # Only the daemon, as a crash or the OOM killer would: a whole-unit kill also kills ExecStopPost (case 2b).
  systemctl kill --kill-whom=main --signal=SIGKILL "$unit"
  wait_inactive 15
  expect driver "after kill -9 and ExecStopPost"
  journalctl -u "$unit" -n 40 --no-pager | grep -iE 'restored|nothing to restore' | tail -2
fi

say "2b · kill -9 of the whole unit: ExecStopPost dies too, so Restart= brings pennad back, and it restores first"
props=(-p Restart=on-failure -p RestartSec=1)
if start; then
  hold 45 120
  sleep 5
  expect manual "held at 45 %"
  systemctl kill --signal=SIGKILL "$unit"
  sleep 8
  expect driver "after the restart (the hold died with the old pennad; at idle the curve hands back)"
  journalctl -u "$unit" -n 60 --no-pager | grep -iE 'restored what a previous run left' | tail -1 | cut -c1-200
  systemctl stop "$unit"
  wait_inactive 15
fi
props=()

say "3 · panic: the panic hook hands the fans back"
if start --test-panic-after 12; then
  hold 45 120
  sleep 4
  expect manual "held at 45 % before the panic"
  wait_inactive 30
  sleep 1
  expect driver "after the panic"
fi

say "4 · hang: the loop stops pinging, systemd's watchdog kills pennad, ExecStopPost hands back"
if start --test-hang-after 10; then
  hold 45 120
  sleep 4
  expect manual "held at 45 % before the hang"
  wait_inactive 40
  sleep 2
  expect driver "after the watchdog"
  journalctl -u "$unit" -n 40 --no-pager | grep -iE 'watchdog' | tail -2
fi

say "5 · idle: below 30 % the driver keeps the fans (and may stop them); a hold takes them and gives them back"
if start; then
  sleep 8
  expect driver "at idle, on the driver's curve"
  "$bin/penna" state | sed -n '1,14p'
  hold 40 6
  sleep 3
  expect manual "a 6 s hold at 40 %"
  sleep 9
  expect driver "after the hold expired"
  systemctl stop "$unit"
  sleep 1
  expect driver "after stop"
fi
