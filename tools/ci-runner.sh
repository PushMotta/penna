#!/usr/bin/env bash
# Installs Penna's self-hosted GitHub Actions runner on this machine, the way push-crm's and three-primaries' are: the
# runner in ~/actions-runner-penna, registered to PushMotta/penna with the label `penna`, run by a systemd --user service.
# It runs as you, never as root, and it takes pushes to main only: its job-started hook, tools/ci-runner-guard.sh copied into
# its directory, refuses every other job before a step of it runs, whatever the workflow that sent it says.
#
#   tools/ci-runner.sh             install, register and start it
#   tools/ci-runner.sh remove      stop it, unregister it and delete it
#
# It copies the runner from another of your runners rather than downloading one; the runner updates itself.
set -euo pipefail

repo_slug=PushMotta/penna
dir="$HOME/actions-runner-penna"
unit=actions-runner-penna.service
unit_file="$HOME/.config/systemd/user/$unit"

remove() {
  systemctl --user disable --now "$unit" 2>/dev/null || true
  rm -f "$unit_file"
  systemctl --user daemon-reload
  if [ -x "$dir/config.sh" ] && [ -f "$dir/.runner" ]; then
    token=$(gh api -X POST "repos/$repo_slug/actions/runners/remove-token" --jq .token)
    (cd "$dir" && ./config.sh remove --token "$token")
  fi
  rm -rf "$dir"
  echo "Removed the penna runner."
}

install() {
  [ ! -e "$dir" ] || { echo "$dir already exists: tools/ci-runner.sh remove first"; exit 1; }
  src=""
  for d in "$HOME"/actions-runner-push-crm "$HOME"/actions-runner-three-primaries "$HOME"/actions-runner; do
    [ -x "$d/config.sh" ] && [ -d "$d/bin" ] && { src=$d; break; }
  done
  [ -n "$src" ] || { echo "No runner to copy from. Download one from https://github.com/$repo_slug/settings/actions/runners/new"; exit 1; }
  echo "== Copying the runner from $src"
  mkdir -p "$dir"
  (cd "$src" && cp -a bin externals config.sh env.sh run.sh run-helper.sh run-helper.sh.template run-helper.cmd.template \
    safe_sleep.sh svc.sh "$dir/")

  echo "== Registering it to $repo_slug, label penna"
  token=$(gh api -X POST "repos/$repo_slug/actions/runners/registration-token" --jq .token)
  (cd "$dir" && ./config.sh --unattended --url "https://github.com/$repo_slug" --token "$token" \
    --name "$(hostname)-penna" --labels penna --work _work --replace)

  echo "== Its job-started hook: pushes to main only"
  cp "$(dirname "$0")/ci-runner-guard.sh" "$dir/job-started.sh"
  chmod 755 "$dir/job-started.sh"
  echo "ACTIONS_RUNNER_HOOK_JOB_STARTED=$dir/job-started.sh" >>"$dir/.env"

  echo "== Starting it as a systemd --user service"
  mkdir -p "$(dirname "$unit_file")"
  cat > "$unit_file" <<EOF
[Unit]
Description=GitHub Actions runner for $repo_slug (label penna)
After=network-online.target

[Service]
WorkingDirectory=%h/actions-runner-penna
ExecStart=%h/actions-runner-penna/run.sh
Restart=always
RestartSec=10
KillSignal=SIGINT
TimeoutStopSec=5min
Nice=10

[Install]
WantedBy=default.target
EOF
  systemctl --user daemon-reload
  systemctl --user enable --now "$unit"
  systemctl --user --no-pager --lines=5 status "$unit" || true
  echo
  echo "The penna runner is up. Every push to main runs tools/ci.sh on it, and it refuses any other job."
}

case "${1:-install}" in
  install) install ;;
  remove) remove ;;
  *) sed -n '2,8p' "$0" | sed 's/^# \{0,1\}//'; exit 2 ;;
esac
