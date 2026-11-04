"""挂载点 / 磁盘信息：决定扫描时进入哪些文件系统，以及启动页显示哪些磁盘。"""
import os
import shutil
import string

from .osinfo import IS_LINUX, IS_MAC, IS_WIN, pkey, system_root

# 真正占用磁盘的文件系统（小写）
DISK_FS = frozenset({
    "ext2", "ext3", "ext4", "btrfs", "xfs", "vfat", "msdos", "exfat", "ntfs", "ntfs3",
    "fuseblk", "f2fs", "zfs", "jfs", "reiserfs", "bcachefs", "hfsplus", "nilfs2", "udf",
    "apfs", "hfs", "fat", "fat16", "fat32", "refs",
})

# macOS：/System/Volumes 下是系统卷、数据卷等，其内容已通过"固定链接"出现在 / 下，
# 再扫一遍会重复计算
MAC_SKIP = ("/System/Volumes",)


def _unescape(s):
    return (s.replace("\\040", " ").replace("\\011", "\t")
             .replace("\\012", "\n").replace("\\134", "\\"))


def _linux_mounts():
    mounts = {}
    with open("/proc/self/mountinfo", errors="surrogateescape") as f:
        for line in f:
            left, _, right = line.partition(" - ")
            lp = left.split()
            rp = right.split()
            if len(lp) < 5 or len(rp) < 2:
                continue
            mounts[_unescape(lp[4])] = {
                "dev": lp[2] + ":" + _unescape(lp[3]),  # 设备号 + 子树根，用于识别 bind mount
                "fstype": rp[0].lower(),
                "source": _unescape(rp[1]),
                "hidden": False,
            }
    return mounts


def _psutil_mounts():
    import psutil
    mounts = {}
    for p in psutil.disk_partitions(all=True):
        dev = p.device or p.mountpoint
        opts = p.opts or ""
        mounts[p.mountpoint] = {
            "dev": dev,
            "fstype": (p.fstype or "").lower(),
            "source": p.device or "",
            # macOS：nobrowse 的是系统内部卷；带 @ 的是 Time Machine 本地快照
            "hidden": IS_MAC and ("nobrowse" in opts or "@" in dev),
            "removable": "cdrom" in opts,
        }
    return mounts


def _fallback_mounts():
    if IS_WIN:
        drives = getattr(os, "listdrives", None)
        roots = drives() if drives else [f"{c}:\\" for c in string.ascii_uppercase
                                         if os.path.exists(f"{c}:\\")]
        return {r: {"dev": r, "fstype": "ntfs", "source": r[:2], "hidden": False} for r in roots}
    return {"/": {"dev": "/", "fstype": "apfs" if IS_MAC else "ext4", "source": "/", "hidden": False}}


def read_mounts():
    """返回 {挂载点: {"dev", "fstype", "source", "hidden"}}。"""
    try:
        if IS_LINUX and os.path.exists("/proc/self/mountinfo"):
            return _linux_mounts()
        return _psutil_mounts()
    except Exception:
        return _fallback_mounts()


def fs_usage(path):
    try:
        u = shutil.disk_usage(path)
    except OSError:
        return None
    return {"total": u.total, "used": u.used, "free": u.free}


def skip_paths():
    """扫描时总是跳过的文件夹（pkey 形式）。"""
    return {pkey(p) for p in MAC_SKIP} if IS_MAC else set()


def disk_mounts():
    """供启动页展示的"磁盘列表"。"""
    out = []
    seen = set()
    sysroot = system_root()
    for mp, info in sorted(read_mounts().items()):
        if info["fstype"] not in DISK_FS or info.get("hidden") or info.get("removable"):
            continue
        # macOS 还会挂载很多系统内部卷（/System/Volumes/*、/System/Library/AssetsV2/… 等），
        # 用户能看到的磁盘只有 / 和 /Volumes 下的外接磁盘
        if IS_MAC and mp != "/" and not mp.startswith("/Volumes/"):
            continue
        key = info["source"]
        if key in seen and mp != sysroot:
            continue
        seen.add(key)
        usage = fs_usage(mp)
        if usage and usage["total"] > 0:
            out.append({"path": mp, "fstype": info["fstype"], "source": info["source"],
                        "system": pkey(mp) == pkey(sysroot), **usage})
    out.sort(key=lambda d: (not d["system"], d["path"]))
    return out
