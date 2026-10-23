#!/usr/bin/env bash
# Publishes a Penna release (DESIGN.md §10, M10). Run from a clean main that is in step with origin.
#
#   tools/release.sh check      everything that must hold before a release; changes nothing
#   tools/release.sh publish    tag v<version> and push it, regenerate omarchy-penna from plugin/, create the GitHub release
#                               with the version's CHANGELOG section, then fill the AUR PKGBUILD's checksum from the
#                               published tarball and push that commit
#   tools/release.sh aur        push packaging/aur to the AUR; needs the maintainer's AUR SSH key
#
# The version is Cargo.toml's; both PKGBUILDs must say the same. The plugin's repository is tagged with manifest.json's.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
repo=$(dirname "$here")
cd "$repo"

owner=PushMotta
plugin_repo="https://github.com/$owner/omarchy-penna.git"
aur_repo="ssh://aur@aur.archlinux.org/penna.git"
version=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1)
plugin_version=$(jq -r .version plugin/manifest.json)
tag="v$version"

fail() { echo "release: $*" >&2; exit 1; }
step() { echo "== $*"; }

check() {
  step "Penna $version, plugin $plugin_version"
  [ "$(git branch --show-current)" = main ] || fail "not on main"
  [ -z "$(git status --porcelain)" ] || fail "the working tree has changes"
  git fetch -q origin
  [ "$(git rev-parse HEAD)" = "$(git rev-parse origin/main)" ] || fail "main is not in step with origin/main"
  ! git rev-parse -q --verify "refs/tags/$tag" >/dev/null || fail "$tag already exists"
  for f in packaging/PKGBUILD packaging/aur/PKGBUILD; do
    grep -qx "pkgver=$version" "$f" || fail "$f doesn't say pkgver=$version"
  done
  diff <(sed -n '/^prepare()/,$p' packaging/PKGBUILD) <(sed -n '/^prepare()/,$p' packaging/aur/PKGBUILD) >/dev/null \
    || fail "the two PKGBUILDs build differently: their prepare(), build(), check() and package() must match"
  (cd packaging/aur && makepkg --printsrcinfo | diff -q - .SRCINFO >/dev/null) || fail "packaging/aur/.SRCINFO is stale"
  grep -q "^## $version " CHANGELOG.md || fail "CHANGELOG.md has no section for $version"
  for f in README.md plugin/README.md; do
    grep -q "v$version/penna-$version-1-x86_64.pkg.tar.zst" "$f" || fail "$f doesn't install penna-$version"
  done
  if command -v omarchy >/dev/null; then omarchy plugin validate plugin >/dev/null || fail "the plugin doesn't validate"; fi
  step "Tests"
  cargo fmt --check
  cargo clippy -q --workspace --all-targets --locked -- -D warnings
  # As on a machine without Penna, where the AUR package is first built: nothing installed is visible to the tests.
  if command -v bwrap >/dev/null; then
    bwrap --dev-bind / / --tmpfs /usr/share/penna --tmpfs /var/lib/penna --tmpfs /etc/penna --tmpfs /run/penna \
      --unsetenv PENNA_BOARDS --unsetenv PENNA_SOCKET -- cargo test -q --workspace --locked >/dev/null
  else
    cargo test -q --workspace --locked >/dev/null
  fi
  (cd plugin && node --test test/model.test.js test/studio.test.js >/dev/null)
  step "Ready to publish $tag"
}

notes() {
  awk -v v="$version" '$0 ~ "^## " v " " { on = 1; next } on && /^## / { exit } on' CHANGELOG.md
}

publish() {
  check
  step "Tagging $tag"
  git tag -a "$tag" -m "Penna $version"
  git push origin main "$tag"

  step "omarchy-penna from plugin/"
  split=$(git subtree split --prefix=plugin)
  git push "$plugin_repo" "$split:refs/heads/main"
  git push "$plugin_repo" "$split:refs/tags/v$plugin_version"

  step "GitHub release"
  gh release create "$tag" --repo "$owner/penna" --title "Penna $version" --notes "$(notes)"

  step "AUR checksum"
  url="https://github.com/$owner/penna/archive/refs/tags/$tag.tar.gz"
  sum=$(curl -fsSL "$url" | sha256sum | cut -d' ' -f1)
  [ ${#sum} = 64 ] || fail "couldn't fetch $url"
  sed -i "s/^sha256sums=.*/sha256sums=('$sum')/" packaging/aur/PKGBUILD
  (cd packaging/aur && makepkg --printsrcinfo > .SRCINFO)

  step "The package, built from the published tarball as a new machine would, on the release"
  build=$(mktemp -d)
  cp packaging/aur/PKGBUILD packaging/aur/.SRCINFO packaging/aur/penna.install "$build/"
  (cd "$build" && bwrap --dev-bind / / --tmpfs /usr/share/penna --tmpfs /var/lib/penna --tmpfs /etc/penna --tmpfs /run/penna \
    --unsetenv PENNA_BOARDS --unsetenv PENNA_SOCKET -- makepkg -f --nodeps >/dev/null)
  pkg="penna-$version-1-x86_64.pkg.tar.zst"
  (cd "$build" && sha256sum "$pkg" >SHA256SUMS && gh release upload "$tag" "$pkg" SHA256SUMS --repo "$owner/penna" --clobber)
  rm -rf "$build"
  git commit -q -m "AUR: $tag's checksum" packaging/aur/PKGBUILD packaging/aur/.SRCINFO
  git push origin main
  step "Published $tag. The AUR is next: tools/release.sh aur"
}

aur() {
  grep -q "^sha256sums=('[0-9a-f]\{64\}')" packaging/aur/PKGBUILD || fail "the AUR PKGBUILD has no checksum yet: publish first"
  work=$(mktemp -d)
  trap 'rm -rf "$work"' EXIT
  git clone -q "$aur_repo" "$work/penna"
  cp packaging/aur/PKGBUILD packaging/aur/.SRCINFO packaging/aur/penna.install "$work/penna/"
  cd "$work/penna"
  git add PKGBUILD .SRCINFO penna.install
  git commit -q -m "penna $version"
  git push origin HEAD:master
  step "penna $version is on the AUR"
}

case "${1:-}" in
  check) check ;;
  publish) publish ;;
  aur) aur ;;
  *) sed -n '2,10p' "$0" | sed 's/^# \{0,1\}//'; exit 2 ;;
esac
