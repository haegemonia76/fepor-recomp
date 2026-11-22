#!/usr/bin/env bash
# Assemble one release archive (ADR 0010, 0023): the two executables, the RecompLauncher profile stamped with
# the release version, the licence. No game data. The archive has its files at the top level.
#
# Usage: tools/package-release.sh <version> <windows|linux|macos> <x64|arm64> <bin-dir> [out-dir]
#   bin-dir  holds the release build of fepor-recomp and fepor-game (target/release)
#   out-dir  where the archive goes (default: dist)
# Writes <out-dir>/fepor-recomp-<version>-<os>-<arch>.<zip|tar.xz> and prints its path.
set -euo pipefail

if [ $# -lt 4 ]; then
    sed -n '2,9p' "$0" | sed 's/^# \{0,1\}//' >&2
    exit 2
fi
version=$1 os=$2 arch=$3 bin_dir=$4 out_dir=${5:-dist}
repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)

case "$os" in
    windows) exe=".exe" ext="zip" ;;
    linux | macos) exe="" ext="tar.xz" ;;
    *) echo "unknown system: $os (windows, linux or macos)" >&2; exit 2 ;;
esac
case "$arch" in x64 | arm64) ;; *) echo "unknown architecture: $arch (x64 or arm64)" >&2; exit 2 ;; esac

name="fepor-recomp-$version-$os-$arch"
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT

for bin in fepor-recomp fepor-game; do
    [ -f "$bin_dir/$bin$exe" ] || { echo "missing $bin_dir/$bin$exe: build with cargo build --release" >&2; exit 1; }
    cp "$bin_dir/$bin$exe" "$stage/"
done
cp "$repo/LICENSE" "$stage/"
# the profile's setup step names the version RecompLauncher compares to decide a rebuild is needed
jq --arg v "$version" '.prepare.version = $v' "$repo/packaging/recomplauncher.json" > "$stage/recomplauncher.json"

mkdir -p "$out_dir"
out=$(cd "$out_dir" && pwd)/$name.$ext
rm -f "$out"
if [ "$ext" = zip ]; then
    (cd "$stage" && 7z a -tzip -bso0 -bsp0 "$out" .)
else
    tar -C "$stage" -cJf "$out" .
fi
# the path as the caller gave it: on Windows the absolute one is an MSYS path other tools cannot read
echo "$out_dir/$name.$ext"
