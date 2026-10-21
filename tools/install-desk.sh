#!/usr/bin/env bash
# Installs the Penna package built from this checkout on this desk, with the RTX 3090 config the M4 hardware test used,
# and starts pennad. Build the package first, as your user:
#
#   cd packaging && makepkg -f
#   pkexec tools/install-desk.sh
#
# Remove with `pkexec pacman -R penna`: stopping pennad hands every fan back first.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
repo=$(dirname "$here")

[ "$(id -u)" = 0 ] || { echo "This installs a system service and needs root: pkexec $0"; exit 2; }
user=$(getent passwd "${PKEXEC_UID:-${SUDO_UID:-}}" | cut -d: -f1 || true)
[ -n "$user" ] || { echo "Can't tell who ran this. Run it through pkexec or sudo."; exit 2; }
pkg=$(ls -t "$repo"/packaging/penna-[0-9]*-x86_64.pkg.tar.zst 2>/dev/null | head -1 || true)
[ -n "$pkg" ] || { echo "Build the package first, as $user: cd $repo/packaging && makepkg -f"; exit 2; }

echo "== Installing $(basename "$pkg")"
pacman -U --noconfirm "$pkg"

if [ -f /etc/penna/config.toml ]; then
  echo "== Keeping the existing /etc/penna/config.toml"
else
  install -Dm644 "$repo/test/fixtures/desk-3090.config.toml" /etc/penna/config.toml
  echo "== Installed this desk's config (both 3090 fans on the balanced GPU curve) as /etc/penna/config.toml"
fi

echo "== Adding $user to the penna group"
usermod -aG penna "$user"

echo "== Starting pennad"
systemctl daemon-reload
systemctl enable --now pennad.service
for _ in $(seq 1 40); do
  PENNA_SOCKET=/run/penna/penna.sock penna state >/dev/null 2>&1 && break
  sleep 0.25
done
systemctl --no-pager --lines=0 status pennad.service || true
echo
PENNA_SOCKET=/run/penna/penna.sock penna state || { echo "pennad did not answer:"; journalctl -u pennad -n 30 --no-pager; exit 1; }
echo
python3 -I "$here/nvml-state.py"
echo
echo "Penna is running. The bar widget now talks to it; $user can change profiles and curves from the popup and the studio."
