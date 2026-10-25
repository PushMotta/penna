#!/usr/bin/env bash
# Penna's CI gate, the same anywhere: format, clippy, every Rust test, and the plugin's model tests. The workflow
# (.github/workflows/test.yml) runs it on a self-hosted runner; anyone can run it before pushing.
#
#   tools/ci.sh                in a fresh clone of HEAD: what a new contributor, CI or the AUR's tarball gets, with no
#                              untracked or ignored files and no empty directories (git keeps none)
#   tools/ci.sh here           in this checkout, uncommitted changes included
#   tools/ci.sh rust|plugin    one half, in the current directory
#
# Nothing here touches real fans: the tests run against the mock desk, the simulator and fixtures (CONTRIBUTING.md).
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
repo=$(dirname "$here")

step() { echo "== $*"; }

# As on a machine without Penna, as the release check runs them: an installed board database, its state and a running
# pennad's socket are hidden from the tests. A test that passes only where Penna is installed fails here, not after a release.
without_penna() {
  local hide=() d
  for d in /usr/share/penna /var/lib/penna /etc/penna /run/penna; do
    [ -e "$d" ] && hide+=(--tmpfs "$d")
  done
  if [ ${#hide[@]} -eq 0 ]; then
    env -u PENNA_BOARDS -u PENNA_SOCKET "$@"
  elif command -v bwrap >/dev/null; then
    bwrap --dev-bind / / "${hide[@]}" --unsetenv PENNA_BOARDS --unsetenv PENNA_SOCKET -- "$@"
  else
    echo "Penna is installed here, and without bwrap (bubblewrap) the tests would see it"
    exit 1
  fi
}

rust() {
  step "Format"
  cargo fmt --check
  step "Clippy"
  cargo clippy --workspace --all-targets --locked -- -D warnings
  step "Tests"
  without_penna cargo test --workspace --locked
}

plugin() {
  step "Plugin model tests"
  (cd plugin && node --test test/model.test.js test/studio.test.js)
}

case "${1:-fresh}" in
  rust) rust ;;
  plugin) plugin ;;
  here)
    cd "$repo"
    rust
    plugin
    ;;
  fresh)
    work=$(mktemp -d)
    trap 'rm -rf "$work"' EXIT
    step "A fresh clone of $(git -C "$repo" rev-parse --short HEAD)"
    git clone -q --no-hardlinks "$repo" "$work/penna"
    git -C "$work/penna" checkout -q "$(git -C "$repo" rev-parse HEAD)"
    cd "$work/penna"
    # The build itself is cached across runs; the sources are the clone's.
    export CARGO_TARGET_DIR="$repo/target/ci"
    rust
    plugin
    ;;
  *)
    sed -n '2,10p' "$0" | sed 's/^# \{0,1\}//'
    exit 2
    ;;
esac
step "CI passed"
