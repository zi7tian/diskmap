#!/bin/sh
# 把 DiskMap 添加到 Linux 桌面的应用菜单（只写入当前用户的 ~/.local/share/applications）
# 同时适用于打包好的 DiskMap 可执行文件和源码版。
set -e
DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$DIR/.." && pwd)"   # 源码版：仓库根目录（图标在 assets/icons/）
APPS="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
mkdir -p "$APPS"

if [ -x "$DIR/DiskMap" ]; then
    EXEC="\"$DIR/DiskMap\""
else
    EXEC="python3 \"$DIR/diskmap.py\""
fi
if [ -f "$DIR/diskmap.png" ]; then
    ICON="$DIR/diskmap.png"
elif [ -f "$ROOT/assets/icons/diskmap-512.png" ]; then
    ICON="$ROOT/assets/icons/diskmap-512.png"
else
    ICON="drive-harddisk"
fi

cat > "$APPS/diskmap.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=DiskMap 磁盘地图
Comment=用嵌套方框查看磁盘占用，并按重要程度着色
Exec=$EXEC
Icon=$ICON
Terminal=false
Categories=System;Utility;
StartupWMClass=diskmap
EOF
echo "已添加到应用菜单：$APPS/diskmap.desktop"
