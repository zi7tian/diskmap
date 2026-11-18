#!/usr/bin/env bash
# 从只读挂载的 dmg 运行，退出时始终卸载镜像。
set -euo pipefail
root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)"
version="${1:?请指定版本 / Supply the version}"
platform="${2:?请指定平台 / Supply the platform}"
out="$root/target/packaging/out"
scripts="$root/crates/diskmap-desktop/packaging"
work="$(mktemp -d "$root/target/packaging/smoke-macos.XXXXXX")"
mounted=false
cleanup() {
  if "$mounted"; then hdiutil detach "$work/mount"; fi
}
trap cleanup EXIT
export DISKMAP_CACHE_DIR="$work/cache" DISKMAP_CONFIG_DIR="$work/config"
mkdir -p "$work/mount" "$work/cli unpack"
hdiutil attach "$out/DiskMap-$version-$platform.dmg" -readonly -nobrowse -mountpoint "$work/mount"
mounted=true
app="$work/mount/DiskMap.app"
test "$(readlink "$work/mount/Applications")" = /Applications
codesign --verify --deep --strict --verbose=2 "$app"
codesign --verify --strict --verbose=2 "$app/Contents/MacOS/diskmap-cli"
python3 -B "$scripts/smoke.py" --desktop "$app/Contents/MacOS/DiskMap" --cli "$app/Contents/MacOS/diskmap-cli" --version "$version" --work "$work/dmg-test"
hdiutil detach "$work/mount"
mounted=false
tar -xzf "$out/diskmap-cli-$version-$platform.tar.gz" -C "$work/cli unpack"
codesign --verify --strict --verbose=2 "$work/cli unpack/diskmap-cli"
python3 -B "$scripts/smoke.py" --cli "$work/cli unpack/diskmap-cli" --version "$version" --work "$work/cli-test"
