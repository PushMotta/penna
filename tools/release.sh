#!/usr/bin/env bash
# Publishes a Penna release (DESIGN.md §10, M10). Run from a clean main that is in step with origin.
#
#   tools/release.sh check      everything that must hold before a release; changes nothing
#   tools/release.sh publish    tag v<version> and push it, create the GitHub release with the version's CHANGELOG section,
#                               then fill the AUR PKGBUILD's checksum from the published tarball, attach the package built
#                               from it, and point README.md's install at the package's digest and the tag's commit
#   tools/release.sh aur        push packaging/aur to the AUR; needs the maintainer's AUR SSH key
#   tools/release.sh plugin     point plugin/README.md at the release README.md installs, by commit and digest, and
#                               regenerate omarchy-penna from plugin/, tagged with manifest.json's version
#
# The version is Cargo.toml's; both PKGBUILDs must say the same. omarchy-penna moves only with `plugin`, when we choose: the
# Omarchy marketplace reviews each of its commits, and its README names the one release of Penna that commit was reviewed with.
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

# The release a README's Install section installs: its version, the commit it was built from and its package's digest.
pinned() {
  local file=$1
  pinned_version=$(sed -n '/^## Install/,/^## [^I]/s|.*releases/download/v\([0-9.]*\)/penna-.*|\1|p' "$file" | head -1)
  pinned_commit=$(sed -n '/^## Install/,/^## [^I]/s|.*--detach \([0-9a-f]\{40\}\).*|\1|p' "$file" | head -1)
  pinned_sum=$(sed -n '/^## Install/,/^## [^I]/s|^echo "\([0-9a-f]\{64\}\)  penna-.*|\1|p' "$file" | head -1)
  [ -n "$pinned_version" ] && [ -n "$pinned_commit" ] && [ -n "$pinned_sum" ] || fail "$file's Install section names no release"
}

# Points a README's Install section at another release: every mention of the old version, commit and digest there.
pin() {
  local file=$1 to_version=$2 to_commit=$3 to_sum=$4 old
  pinned "$file"
  old=${pinned_version//./\\.}
  sed -i -E "/^## Install/,/^## [^I]/{
    s/$pinned_commit/$to_commit/g
    s/$pinned_sum/$to_sum/g
    s/(^|[^0-9.])$old([^0-9]|\$)/\1$to_version\2/g
    s/(^|[^0-9.])$old([^0-9]|\$)/\1$to_version\2/g
  }" "$file"
  pinned "$file"
  [ "$pinned_version $pinned_commit $pinned_sum" = "$to_version $to_commit $to_sum" ] \
    || fail "couldn't point $file at $to_version"
}

publish() {
  check
  step "Tagging $tag"
  git tag -a "$tag" -m "Penna $version"
  git push origin main "$tag"

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
  pin README.md "$version" "$(git rev-parse "$tag^{commit}")" "$(cut -d' ' -f1 "$build/SHA256SUMS")"
  rm -rf "$build"
  git commit -q -m "$tag: the AUR checksum, and the package's digest in the README" \
    packaging/aur/PKGBUILD packaging/aur/.SRCINFO README.md
  git push origin main
  step "Published $tag. The AUR is next: tools/release.sh aur. The plugin stays on its release until tools/release.sh plugin"
}

plugin() {
  [ "$(git branch --show-current)" = main ] || fail "not on main"
  [ -z "$(git status --porcelain)" ] || fail "the working tree has changes"
  git fetch -q origin
  [ "$(git rev-parse HEAD)" = "$(git rev-parse origin/main)" ] || fail "main is not in step with origin/main"
  # The release README.md installs, as publish left it, checked against what the release serves.
  pinned README.md
  local v=$pinned_version sum=$pinned_sum commit pkg got
  commit=$(git rev-parse "v$v^{commit}")
  [ "$commit" = "$pinned_commit" ] || fail "README.md builds $pinned_commit, but v$v is $commit"
  pkg="penna-$v-1-x86_64.pkg.tar.zst"
  got=$(curl -fsSL "https://github.com/$owner/penna/releases/download/v$v/$pkg" | sha256sum | cut -d' ' -f1)
  [ "$got" = "$sum" ] || fail "the release's $pkg has the digest $got, where README.md records $sum"
  step "Plugin $plugin_version, for Penna $v"
  pin plugin/README.md "$v" "$commit" "$sum"
  if command -v omarchy >/dev/null; then omarchy plugin validate plugin >/dev/null || fail "the plugin doesn't validate"; fi
  (cd plugin && node --test test/model.test.js test/studio.test.js >/dev/null)
  if ! git diff --quiet plugin/README.md; then
    git commit -q -m "Plugin $plugin_version: Penna $v, by commit and digest" plugin/README.md
    git push origin main
  fi
  split=$(git subtree split --prefix=plugin)
  if [ "$(git ls-remote "$plugin_repo" refs/heads/main | cut -f1)" = "$split" ]; then
    step "omarchy-penna is already at $split"
    return
  fi
  ! git ls-remote --exit-code --tags "$plugin_repo" "refs/tags/v$plugin_version" >/dev/null \
    || fail "omarchy-penna already has v$plugin_version: raise plugin/manifest.json's version first"
  step "omarchy-penna from plugin/"
  git push "$plugin_repo" "$split:refs/heads/main"
  git push "$plugin_repo" "$split:refs/tags/v$plugin_version"
  step "omarchy-penna is at $split. The marketplace reviews exactly that commit"
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
  plugin) plugin ;;
  *) sed -n '2,/^set /{/^#/p}' "$0" | sed 's/^# \{0,1\}//'; exit 2 ;;
esac
