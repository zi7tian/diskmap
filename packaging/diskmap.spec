# -*- mode: python ; coding: utf-8 -*-
# PyInstaller 打包配置。在项目根目录运行：
#     python -m PyInstaller --noconfirm --clean packaging/diskmap.spec
# 产物：
#     Windows  dist/DiskMap.exe          单个可执行文件
#     macOS    dist/DiskMap.app          应用程序包
#     Linux    dist/DiskMap              单个可执行文件
import os
import sys

ROOT = os.path.abspath(os.path.join(SPECPATH, ".."))
ICONS = os.path.join(SPECPATH, "icons")
sys.path.insert(0, ROOT)
from backend import __version__  # noqa: E402

IS_WIN = sys.platform == "win32"
IS_MAC = sys.platform == "darwin"

a = Analysis(
    [os.path.join(ROOT, "diskmap.py")],
    pathex=[ROOT],
    datas=[(os.path.join(ROOT, "web"), "web")],
    hiddenimports=[],
    # Linux 的窗口借用系统 Python 里的 GTK，不打包 gi；也不需要 Qt / Tk
    excludes=["tkinter", "gi", "PyQt5", "PyQt6", "PySide2", "PySide6"],
    noarchive=False,
)
pyz = PYZ(a.pure)

if IS_MAC:
    exe = EXE(
        pyz, a.scripts, [],
        exclude_binaries=True,
        name="DiskMap",
        console=False,
        upx=False,
        argv_emulation=False,
    )
    coll = COLLECT(exe, a.binaries, a.datas, name="DiskMap", upx=False)
    app = BUNDLE(
        coll,
        name="DiskMap.app",
        icon=os.path.join(ICONS, "diskmap.icns"),
        bundle_identifier="app.diskmap.DiskMap",
        version=__version__,
        info_plist={
            "CFBundleName": "DiskMap",
            "CFBundleDisplayName": "DiskMap 磁盘地图",
            "CFBundleShortVersionString": __version__,
            "CFBundleVersion": __version__,
            "LSMinimumSystemVersion": "11.0",
            "LSApplicationCategoryType": "public.app-category.utilities",
            "NSHighResolutionCapable": True,
            # 界面通过 http://127.0.0.1 访问本机的后台
            "NSAppTransportSecurity": {"NSAllowsLocalNetworking": True},
        },
    )
else:
    version_info = None
    if IS_WIN:
        from PyInstaller.utils.win32.versioninfo import (FixedFileInfo, StringFileInfo,
                                                         StringStruct, StringTable,
                                                         VarFileInfo, VarStruct, VSVersionInfo)
        nums = tuple(int(x) for x in (__version__.split(".") + ["0", "0", "0"])[:4])
        version_info = VSVersionInfo(
            ffi=FixedFileInfo(filevers=nums, prodvers=nums),
            kids=[
                StringFileInfo([StringTable("080404B0", [
                    StringStruct("FileDescription", "DiskMap 磁盘地图"),
                    StringStruct("ProductName", "DiskMap"),
                    StringStruct("FileVersion", __version__),
                    StringStruct("ProductVersion", __version__),
                    StringStruct("OriginalFilename", "DiskMap.exe"),
                ])]),
                VarFileInfo([VarStruct("Translation", [0x0804, 1200])]),
            ],
        )
    exe = EXE(
        pyz, a.scripts, a.binaries, a.datas, [],
        name="DiskMap",
        console=False,
        upx=False,
        icon=os.path.join(ICONS, "diskmap.ico") if IS_WIN else None,
        version=version_info,
    )
