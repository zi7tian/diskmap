#!/usr/bin/env bash
# CI 默认安装 deb；本地用 --extract-deb --skip-appimage，不调用 sudo 或运行 AppImage。
set -euo pipefail
root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)"
version="${1:?请指定版本 / Supply the version}"
shift
extract_deb=false
skip_appimage=false
for option in "$@"; do
  case "$option" in
    --extract-deb) extract_deb=true ;;
    --skip-appimage) skip_appimage=true ;;
    *) printf '未知选项 / Unknown option %s\n' "$option" >&2; exit 1 ;;
  esac
done
out="$root/target/packaging/out"
scripts="$root/crates/diskmap-desktop/packaging"
work="$(mktemp -d "$root/target/packaging/smoke-linux.XXXXXX")"
installed=false
cleanup() {
  if "$installed"; then sudo dpkg --remove disk-map; fi
}
trap cleanup EXIT
export DISKMAP_CACHE_DIR="$work/cache" DISKMAP_CONFIG_DIR="$work/config"

# 无论是否安装，都检查最终发布的 deb 控制文件、程序位置和桌面入口。
mkdir -p "$work/control" "$work/deb"
ar p "$out/DiskMap-$version-linux-x64.deb" control.tar.gz | tar -xz -C "$work/control"
cat "$work/control/control"
grep -Fx 'Package: disk-map' "$work/control/control"
grep -Fx "Version: ${version/-/\~}" "$work/control/control"
grep -E '^Depends:.*libwebkit2gtk-4\.1-0' "$work/control/control"
grep -E '^Depends:.*libgtk-3-0' "$work/control/control"
ar p "$out/DiskMap-$version-linux-x64.deb" data.tar.gz | tar -xz -C "$work/deb"
test -x "$work/deb/usr/bin/DiskMap"
test -x "$work/deb/usr/bin/diskmap-cli"
test -f "$work/deb/usr/share/applications/DiskMap.desktop"
grep -Fx 'StartupWMClass=DiskMap' "$work/deb/usr/share/applications/DiskMap.desktop"
if "$extract_deb"; then
  bin="$work/deb/usr/bin"
else
  # 安装失败也尝试卸载，便于在 CI 中收回半安装状态。
  installed=true
  sudo dpkg --install "$out/DiskMap-$version-linux-x64.deb"
  bin=/usr/bin
fi
python3 -B "$scripts/smoke.py" --desktop "$bin/DiskMap" --cli "$bin/diskmap-cli" --version "$version" --work "$work/deb-test"
if "$installed"; then
  sudo dpkg --remove disk-map
  installed=false
  test ! -e /usr/bin/DiskMap
  test ! -e /usr/bin/diskmap-cli
fi

if ! "$skip_appimage"; then
  image="$out/DiskMap-$version-linux-x64.AppImage"
  chmod +x "$image"
  # 显式解包到工作目录，核对 sidecar，并从该 AppDir 的 AppRun 执行自检。
  (cd "$work" && "$image" --appimage-extract > appimage-files.txt)
  test -x "$work/squashfs-root/usr/bin/diskmap-cli"
  python3 -B "$scripts/smoke.py" --desktop "$work/squashfs-root/AppRun" --cli "$work/squashfs-root/usr/bin/diskmap-cli" --version "$version" --work "$work/appdir-test"
  # 再经过最终 AppImage 的入口执行，避免只检查解出的 ELF。
  python3 -B "$scripts/smoke.py" --appimage --desktop "$image" --cli "$work/squashfs-root/usr/bin/diskmap-cli" --version "$version" --work "$work/appimage-test"
fi

mkdir -p "$work/tar unpack" "$work/cli unpack"
tar -xzf "$out/DiskMap-$version-linux-x64.tar.gz" -C "$work/tar unpack"
python3 -B "$scripts/smoke.py" --desktop "$work/tar unpack/DiskMap" --cli "$work/tar unpack/diskmap-cli" --version "$version" --work "$work/tar-test"
XDG_DATA_HOME="$work/xdg" sh "$work/tar unpack/install-desktop.sh"
grep -Fx "Exec=/usr/bin/env \"$work/tar unpack/DiskMap\"" "$work/xdg/applications/DiskMap.desktop"
grep -Fx 'StartupWMClass=DiskMap' "$work/xdg/applications/DiskMap.desktop"
tar -xzf "$out/diskmap-cli-$version-linux-x64.tar.gz" -C "$work/cli unpack"
python3 -B "$scripts/smoke.py" --cli "$work/cli unpack/diskmap-cli" --version "$version" --work "$work/cli-test"
