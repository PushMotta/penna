#!/usr/bin/env bash
# M5 hardware test (DESIGN.md §6, §9.4, §10): Penna drives the motherboard's fans through it87 and must hand every one back
# on every way out. Like the M4 test, it runs pennad as a transient systemd service (Type=notify, WatchdogSec=5,
# ExecStopPost=penna restore), here with a configuration it writes that holds every motherboard fan at 60 %. It also watches
# whether each channel keeps what Penna wrote (the IT8792E question, §6.4), checks full speed, and spins one fan at a time to
# see which header each control drives (pairing, done by hand, §5.2). "As it was found" covers each channel's pwm_enable and
# every pwmN_auto_* setting the driver shows: on the IT8688E the manual duty register is also the curve's start PWM (§6.1).
#
#   pkexec tools/m5-hardware-test.sh            everything, about eight minutes
#   pkexec tools/m5-hardware-test.sh pairing    pairing alone, about five
#
# You will hear the fans. The installed pennad is stopped first (which hands the GPU's fans back) and started again at the end.
# Whatever happens, the cleanup puts every channel's pwm_enable back as it was found, first through penna restore and then by
# writing the recorded value directly. Everything printed also goes to target/m5-hardware-test.log (target/m5-pairing.log for
# pairing alone).
set -uo pipefail

here=$(cd "$(dirname "$0")" && pwd)
repo=$(dirname "$here")
bin="$repo/target/debug"
unit=penna-m5
work=/run/penna-m5
cfg=$work/config.toml
sock=/run/penna/penna.sock
held=/run/penna/held.json
hold_pct=60
hold_raw=$(( (hold_pct * 255 + 99) / 100 ))
mode=${1:-all}
case "$mode" in
  all) log=$repo/target/m5-hardware-test.log ;;
  pairing) log=$repo/target/m5-pairing.log ;;
  *) echo "usage: pkexec $0 [pairing]"; exit 2 ;;
esac
export PENNA_SOCKET=$sock
fails=0
service_was_active=0

say() { printf '\n== %s\n' "$*"; }

# ------------------------------------------------------------------ the channels
declare -a dirs=() keys=() names=()
declare -A orig_enable=() orig_pwm=() orig_auto=() auto_label=() has_curve=() moved=()
for h in /sys/class/hwmon/hwmon*; do
  name=$(cat "$h/name" 2>/dev/null) || continue
  case "$name" in it8*) ;; *) continue ;; esac
  dir=$(readlink -f "$h")
  loc=$(basename "$(readlink -f "$h/device")")
  for p in "$dir"/pwm[0-9]; do
    # Root may write anything, so look at the mode bits as Penna does: sysfs marks a read-only attribute 0444 (§3.5).
    [ $(( 8#$(stat -c %a "$p") & 8#222 )) -ne 0 ] && [ -e "${p}_enable" ] || continue
    n=${p##*/pwm}
    dirs+=("$dir/pwm$n")
    keys+=("hwmon/$name/$loc/pwm$n")
    names+=("$name pwm$n")
  done
done

table() {
  printf '  %-14s %-8s %-6s %s\n' channel enable pwm "fans on this chip (RPM)"
  for i in "${!dirs[@]}"; do
    local p=${dirs[$i]} fans=""
    for f in "$(dirname "$p")"/fan[0-9]_input; do fans+="$(basename "$f" _input)=$(cat "$f") "; done
    printf '  %-14s %-8s %-6s %s\n' "${names[$i]}" "$(cat "${p}_enable")" "$(cat "$p")" "$fans"
  done
}

# expect_held <what>: every channel manual at what Penna wrote. it87 reads a manual channel at 255 back as enable 0 (§3.5).
expect_held() {
  local bad=()
  for i in "${!dirs[@]}"; do
    local e v
    e=$(cat "${dirs[$i]}_enable") v=$(cat "${dirs[$i]}")
    if [ "$e" != 1 ] && ! { [ "$e" = 0 ] && [ "$v" = 255 ]; }; then bad+=("${names[$i]} enable=$e"); fi
  done
  if [ ${#bad[@]} -eq 0 ]; then echo "PASS  $1"; else echo "FAIL  $1: ${bad[*]}"; fails=$((fails + 1)); fi
}

# The automatic-curve settings of one channel that no longer read as they were found. On it87 a channel that shows its curve
# (pwmN_auto_*) reads its start PWM back as pwm while automatic, pwm4 and pwm5 of the IT8688E included, which hide
# pwmN_auto_start; the IT8792E's firmware shows no curve, and its pwm is the live duty.
auto_drift() {
  local f now
  if [ -n "${has_curve[$1]:-}" ]; then
    now=$(cat "$1")
    [ "$now" = "${orig_pwm[$1]}" ] || printf "%s (its start) %s, was %s; " "$(cat "$(dirname "$1")/name") ${1##*/}" "$now" "${orig_pwm[$1]}"
  fi
  for f in "${!orig_auto[@]}"; do
    [ "${f%_auto_*}" = "$1" ] || continue
    now=$(cat "$f" 2>/dev/null)
    [ "$now" = "${orig_auto[$f]}" ] || printf '%s %s, was %s; ' "${auto_label[$f]}" "$now" "${orig_auto[$f]}"
  done
}

# expect_original <what>: every channel's pwm_enable and automatic curve exactly as they were found.
expect_original() {
  local bad=()
  sleep 2 # it87 caches what it reads from the chip for 1.5 s
  for i in "${!dirs[@]}"; do
    local p=${dirs[$i]} e drift
    e=$(cat "${p}_enable")
    [ "$e" = "${orig_enable[$p]}" ] || bad+=("${names[$i]} enable=$e, was ${orig_enable[$p]};")
    drift=$(auto_drift "$p")
    [ -z "$drift" ] || bad+=("$drift")
  done
  if [ ${#bad[@]} -eq 0 ]; then echo "PASS  $1 (every pwm_enable and automatic curve as found)"
  else echo "FAIL  $1: ${bad[*]}"; fails=$((fails + 1)); fi
}

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
    -- "$bin/pennad" --config "$cfg" --boards "$repo/data/boards" --quiet "$@"
  for _ in $(seq 1 40); do
    "$bin/penna" state >/dev/null 2>&1 && return 0
    sleep 0.25
  done
  echo "FAIL  pennad did not answer on $sock"
  journalctl -u "$unit" -n 20 --no-pager
  fails=$((fails + 1))
  return 1
}

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

# Penna's own state for a control: curve, contested, …
penna_state() {
  "$bin/penna" state --json | python3 -I -c 'import json,sys; t=json.load(sys.stdin); print(next((c["state"] for c in t["controls"] if c["key"]==sys.argv[1]), "?"))' "$1"
}

# pair: which header does each control drive? One control at a time goes to 100 % while every other fan holds (pairing by
# hand, §5.2). Each rise is measured from what the tachs read just before it, once every fan has settled at the hold.
pair() {
  say "7 · which header does each control drive? One at a time to 100 % for 12 s (pairing by hand, §5.2)"
  # Fans come down to the hold at 5 %/s, and their RPM lags the duty by seconds.
  sleep 15
  local tachs=(/sys/class/hwmon/hwmon*/fan[0-9]_input) i f b pk now rose
  for i in "${!dirs[@]}"; do
    [ -n "${moved[${dirs[$i]}]:-}" ] && { echo "  ${names[$i]}: skipped, it reverts"; continue; }
    local -A base=() peak=()
    for f in "${tachs[@]}"; do base[$f]=$(cat "$f"); peak[$f]=0; done
    "$bin/penna" set "${keys[$i]}" 100 --for 12s >/dev/null
    # A fan takes seconds to spin up, and it87 reads the chip at most every 1.5 s: keep each tach's highest reading.
    sleep 3
    for _ in $(seq 1 8); do
      for f in "${tachs[@]}"; do now=$(cat "$f"); [ "$now" -gt "${peak[$f]}" ] && peak[$f]=$now; done
      sleep 1
    done
    rose=""
    for f in "${tachs[@]}"; do
      b=${base[$f]} pk=${peak[$f]}
      # 15 % and 150 RPM: well above a tach's jitter at the hold.
      if [ $((pk - b)) -ge 150 ] && [ $((pk * 100)) -ge $((b * 115)) ]; then
        rose+="$(cat "$(dirname "$f")/name") $(basename "$f" _input) $b→$pk RPM; "
      fi
    done
    echo "  ${names[$i]} → ${rose:-no tach moved (no fan on this header, or one without a tach)}"
    # Back down to the hold before the next one, so its rise is its own.
    sleep 16
  done
}

cleanup() {
  systemctl stop "$unit" 2>/dev/null
  systemctl reset-failed "$unit" 2>/dev/null
  [ -f "$held" ] && "$bin/penna" restore --from "$held"
  sleep 2
  for p in "${dirs[@]}"; do
    drift=$(auto_drift "$p")
    if [ "$(cat "${p}_enable")" != "${orig_enable[$p]}" ] || [ -n "$drift" ]; then
      echo "cleanup: $p was left at pwm_enable $(cat "${p}_enable") ${drift:+($drift)}; writing back pwm ${orig_pwm[$p]}, pwm_enable ${orig_enable[$p]}"
      echo 1 >"${p}_enable"
      echo "${orig_pwm[$p]}" >"$p"
      echo "${orig_enable[$p]}" >"${p}_enable"
    fi
  done
  rm -f "$sock"
  rm -rf "$work"
  if [ "$service_was_active" = 1 ]; then
    systemctl start pennad.service && echo "Started the installed pennad again."
  fi
  say "After the test"
  table
  if [ "$fails" -eq 0 ]; then echo "M5 hardware test: PASS"; else echo "M5 hardware test: $fails FAILED"; fi
  chown --reference="$repo" "$log" 2>/dev/null
}

# ------------------------------------------------------------------ preflight
[ "$(id -u)" = 0 ] || { echo "This drives the motherboard's fans and needs root: pkexec $0"; exit 2; }
[ -x "$bin/pennad" ] && [ -x "$bin/penna" ] || { echo "Build first, as your user: cargo build -p pennad -p penna"; exit 2; }
[ ${#dirs[@]} -gt 0 ] || { echo "No it87 fan controls under /sys/class/hwmon. Load the driver first: modprobe it87 ignore_resource_conflict=1"; exit 2; }
tctl=$(for h in /sys/class/hwmon/hwmon*; do [ "$(cat "$h/name")" = k10temp ] && cat "$h/temp1_input"; done | head -1)
[ -n "$tctl" ] && [ "$tctl" -lt 70000 ] || { echo "The CPU is at $((tctl / 1000)) °C. Run this while the machine is idle."; exit 2; }
exec > >(tee "$log") 2>&1
for i in "${!dirs[@]}"; do
  p=${dirs[$i]}
  orig_enable[$p]=$(cat "${p}_enable")
  orig_pwm[$p]=$(cat "$p")
  for f in "$p"_auto_*; do
    [ -r "$f" ] || continue
    orig_auto[$f]=$(cat "$f" 2>/dev/null)
    auto_label[$f]="${names[$i]%% *} ${f##*/}"
    has_curve[$p]=1
  done
done
trap cleanup EXIT
if systemctl is-active --quiet pennad.service; then
  service_was_active=1
  systemctl stop pennad.service
  echo "Stopped the installed pennad; it starts again at the end."
fi
"$bin/penna" state >/dev/null 2>&1 && { echo "A pennad still answers on $sock. Stop it first."; exit 2; }

mkdir -p "$work"
{
  echo 'version = 1'
  echo 'profile = "test"'
  echo
  echo '[[curve]]'
  echo 'id = "hold"'
  echo "flat = { percent = $hold_pct }"
  for k in "${keys[@]}"; do printf '\n[[control]]\nkey = "%s"\nfailsafe = 100\n' "$k"; done
  echo
  echo '[profiles.test]'
  for k in "${keys[@]}"; do printf '"%s" = "hold"\n' "$k"; done
} >"$cfg"
"$bin/penna" check "$cfg" || { echo "penna check refused the test config:"; cat "$cfg"; exit 2; }

say "Before: ${#dirs[@]} channels, as the firmware runs them"
table
starts=""
for f in "${!orig_auto[@]}"; do [[ $f == *_auto_start ]] && starts+="${auto_label[$f]}=${orig_auto[$f]} "; done
echo "  ${#orig_auto[@]} automatic-curve settings recorded. Start PWMs: ${starts:-none shown}"

if [ "$mode" = pairing ]; then
  if start; then
    pair
    systemctl stop "$unit"
    wait_inactive 15
    expect_original "after the last stop"
  fi
  exit
fi

say "1 · systemctl stop (SIGTERM): pennad hands every channel back itself"
if start; then
  sleep 6
  expect_held "held at $hold_pct % by Penna"
  systemctl stop "$unit"
  sleep 1
  expect_original "after systemctl stop"
  if [ -f "$held" ]; then echo "FAIL  held.json was left behind"; fails=$((fails + 1)); fi
fi

say "2 · kill -9 of pennad: ExecStopPost hands every channel back"
if start; then
  sleep 5
  expect_held "held at $hold_pct %"
  systemctl kill --kill-whom=main --signal=SIGKILL "$unit"
  wait_inactive 15
  expect_original "after kill -9 and ExecStopPost"
fi

say "2b · kill -9 of the whole unit: Restart= brings pennad back, and it restores what was left first"
props=(-p Restart=on-failure -p RestartSec=1)
if start; then
  sleep 5
  expect_held "held at $hold_pct %"
  systemctl kill --signal=SIGKILL "$unit"
  sleep 6
  journalctl -u "$unit" -n 60 --no-pager | grep -iE 'restored what a previous run left' | tail -1 | cut -c1-200
  expect_held "the restarted pennad took them again, from the original modes"
  systemctl stop "$unit"
  wait_inactive 15
  expect_original "after stopping the restarted pennad"
fi
props=()

say "3 · panic: the panic hook hands every channel back"
if start --test-panic-after 12; then
  sleep 4
  expect_held "held at $hold_pct % before the panic"
  wait_inactive 30
  sleep 1
  expect_original "after the panic"
fi

say "4 · hang: systemd's watchdog kills pennad, ExecStopPost hands back"
if start --test-hang-after 10; then
  sleep 4
  expect_held "held at $hold_pct % before the hang"
  wait_inactive 40
  sleep 2
  expect_original "after the watchdog"
fi

say "5 · does each channel keep what Penna writes? (the IT8792E question, §6.4) — 15 s to settle, then 40 s"
if start; then
  # A fan the firmware ran fast comes down at the step-down rate (5 %/s): let every channel reach the hold first.
  sleep 15
  for _ in $(seq 1 40); do
    for i in "${!dirs[@]}"; do
      p=${dirs[$i]} e=$(cat "${dirs[$i]}_enable") v=$(cat "${dirs[$i]}")
      if [ "$e" != 1 ] || [ $(( v > hold_raw ? v - hold_raw : hold_raw - v )) -gt 5 ]; then moved[$p]="enable=$e pwm=$v"; fi
    done
    sleep 1
  done
  for i in "${!dirs[@]}"; do
    p=${dirs[$i]} st=$(penna_state "${keys[$i]}")
    if [ -n "${moved[$p]:-}" ]; then
      # Something else writes this channel: Penna must have stopped and said so.
      if [ "$st" = contested ]; then echo "PASS  ${names[$i]} reverts (${moved[$p]}), and Penna marked it contested"
      else echo "FAIL  ${names[$i]} reverts (${moved[$p]}), but Penna says $st"; fails=$((fails + 1)); fi
    else
      if [ "$st" = curve ]; then echo "PASS  ${names[$i]} holds $hold_raw/255, and Penna says curve"
      else echo "FAIL  ${names[$i]} holds, but Penna says $st"; fails=$((fails + 1)); fi
    fi
  done

  say "6 · full speed: 100 % reads back as pwm_enable 0 on it87, which is not a contest (§3.5)"
  "$bin/penna" full-speed on >/dev/null
  sleep 6
  bad=()
  for i in "${!dirs[@]}"; do
    [ -n "${moved[${dirs[$i]}]:-}" ] && continue
    st=$(penna_state "${keys[$i]}")
    [ "$st" = override ] || [ "$st" = curve ] || bad+=("${names[$i]} is $st")
  done
  table
  if [ ${#bad[@]} -eq 0 ]; then echo "PASS  full speed, with no false contest"; else echo "FAIL  full speed: ${bad[*]}"; fails=$((fails + 1)); fi
  "$bin/penna" full-speed off >/dev/null

  pair
  systemctl stop "$unit"
  wait_inactive 15
  expect_original "after the last stop"
fi
