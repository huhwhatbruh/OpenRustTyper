#!/usr/bin/env bash
# Build an Arch package from the commit that is checked out, rather than from a
# published release tag. The PKGBUILD in this directory downloads the tag; this
# swaps that one line for a tarball of HEAD and leaves everything else alone, so
# what gets built is exactly what the release will build.
#
#   packaging/build-local.sh [output-dir]      # default: ./dist
#
# Commit first: `git archive` packages what is committed, not what is edited.
set -euo pipefail

root=$(git rev-parse --show-toplevel)
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -n 1)
out=$(realpath -m "${1:-$root/dist}")
mkdir -p "$out"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

git -C "$root" archive --prefix="OpenAutoTyper-$version/" HEAD | gzip > "$work/openautotyper-$version.tar.gz"
sed -e 's|^source=.*|source=("$pkgname-$pkgver.tar.gz")|' \
    -e "s|^sha256sums=.*|sha256sums=('SKIP')|" \
    "$root/packaging/PKGBUILD" > "$work/PKGBUILD"
cp "$root/packaging/openautotyper.install" "$work/"

(cd "$work" && PKGDEST="$out" makepkg --force --noconfirm)
echo
echo "built:"
ls -1 "$out"/openautotyper-*.pkg.tar.* 2>/dev/null
