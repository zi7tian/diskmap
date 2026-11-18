#!/bin/sh
# 在解压后的目录运行，只向当前用户的应用菜单写入入口，程序留在原目录。
set -eu
DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
APPS="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
test -x "$DIR/DiskMap"
mkdir -p "$APPS"

# Exec 同时经过 desktop 字符串和命令行两层解码，保留空格及特殊字符。
# 由 env 启动，让含 % 的路径在参数展开后解析，避开 GLib 的文件名预检查。
EXEC=$(printf '%s' "$DIR/DiskMap" | sed -e 's/\\/\\\\\\\\/g' -e 's/"/\\\\"/g' -e 's/`/\\\\`/g' -e 's/\$/\\\\$/g' -e 's/%/%%/g')
ICON=$(printf '%s' "$DIR/diskmap.png" | sed 's/\\/\\\\/g')
cat > "$APPS/DiskMap.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=DiskMap
Name[zh_CN]=DiskMap 磁盘地图
Comment=Explore disk usage with nested rectangles colored by importance
Comment[zh_CN]=用嵌套方框查看磁盘占用，并按重要程度着色
Exec=/usr/bin/env "$EXEC"
Icon=$ICON
Terminal=false
Categories=Utility;
StartupWMClass=DiskMap
EOF
printf '已添加到应用菜单 / Added to the application menu\n%s\n' "$APPS/DiskMap.desktop"
