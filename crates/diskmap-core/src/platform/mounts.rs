//! 挂载点与磁盘信息（T01）：决定扫描时进入哪些文件系统，以及启动页显示哪些磁盘。
//! 对应 legacy/backend/mounts.py。

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::{Os, system_root};

/// 真正占用磁盘的文件系统（小写），与旧版 `DISK_FS` 相同。扫描时只进入这些文件系统的挂载点。
pub const DISK_FS: &[&str] = &[
    "ext2", "ext3", "ext4", "btrfs", "xfs", "vfat", "msdos", "exfat", "ntfs", "ntfs3", "fuseblk",
    "f2fs", "zfs", "jfs", "reiserfs", "bcachefs", "hfsplus", "nilfs2", "udf", "apfs", "hfs", "fat",
    "fat16", "fat32", "refs",
];

/// 是否是真正占用磁盘的文件系统。`fstype` 应为小写。
pub fn is_disk_fs(fstype: &str) -> bool {
    DISK_FS.contains(&fstype)
}

/// 一个挂载点。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mount {
    pub path: PathBuf,
    /// 识别同一个文件系统被挂载多次（bind mount）：Linux 为"设备号:子树根"，其他系统为设备名。
    pub dev: String,
    /// 文件系统类型，小写。
    pub fstype: String,
    /// 来源设备，例如 `/dev/nvme0n1p2`、`C:`。
    pub source: String,
    /// macOS 的系统内部卷（nobrowse）和 Time Machine 本地快照。
    pub hidden: bool,
    /// 光驱。
    pub removable: bool,
}

/// 当前的全部挂载点。同一路径挂载多次时只保留最后（最上层）的一个。读取失败时返回保守的默认值。
pub fn read_mounts() -> Vec<Mount> {
    os_mounts().unwrap_or_else(fallback_mounts)
}

/// 扫描时总是跳过的文件夹。macOS 的 `/System/Volumes` 下是系统卷、数据卷等，
/// 其内容已经通过"固定链接"出现在 `/` 下，再扫一遍会重复计算。
pub fn skip_paths() -> Vec<PathBuf> {
    if Os::current() == Os::Macos {
        vec![PathBuf::from("/System/Volumes")]
    } else {
        Vec::new()
    }
}

/// 磁盘空间，单位字节。计算方式与 Python 的 `shutil.disk_usage` 相同。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Usage {
    pub total: u64,
    pub used: u64,
    pub free: u64,
}

/// 路径所在文件系统的空间。
pub fn fs_usage(path: &Path) -> Option<Usage> {
    imp::fs_usage(path)
}

/// 启动页显示的一个磁盘。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Disk {
    pub path: PathBuf,
    pub fstype: String,
    pub source: String,
    /// 是否是系统盘。
    pub system: bool,
    pub usage: Usage,
}

/// 启动页的磁盘列表（旧版 `disk_mounts`）：只列真实磁盘，同一设备只列一次，系统盘排在最前。
/// macOS 只列 `/` 和 `/Volumes` 下的磁盘（系统还会挂载很多内部卷）。
pub fn disk_mounts() -> Vec<Disk> {
    let os = Os::current();
    let sysroot = system_root();
    let same = |a: &Path, b: &Path| {
        let (a, b) = (
            a.as_os_str().as_encoded_bytes(),
            b.as_os_str().as_encoded_bytes(),
        );
        super::path::cmp_paths(os, a, b).is_eq()
    };
    let mut mounts = read_mounts();
    mounts.sort_by(|a, b| super::path::cmp_paths(os, bytes(&a.path), bytes(&b.path)));
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for m in mounts {
        if !is_disk_fs(&m.fstype) || m.hidden || m.removable {
            continue;
        }
        if os == Os::Macos && m.path != Path::new("/") && !bytes(&m.path).starts_with(b"/Volumes/")
        {
            continue;
        }
        let source = super::path::path_key(os, m.source.as_bytes());
        if seen.contains(&source) && !same(&m.path, &sysroot) {
            continue;
        }
        seen.insert(source);
        if let Some(usage) = fs_usage(&m.path).filter(|u| u.total > 0) {
            let system = same(&m.path, &sysroot);
            let source = disk_source(os, &m.path, m.source);
            out.push(Disk {
                path: m.path,
                fstype: m.fstype,
                source,
                system,
                usage,
            });
        }
    }
    out.sort_by(|a, b| {
        (!a.system)
            .cmp(&(!b.system))
            .then_with(|| super::path::cmp_paths(os, bytes(&a.path), bytes(&b.path)))
    });
    out
}

/// Windows 启动页的 source 与旧版 psutil.device 一样保留盘符根路径末尾的反斜杠。
/// 只转换磁盘列表字段；扫描使用的 Mount.source 和设备去重规则保持原样。
fn disk_source(os: Os, root: &Path, source: String) -> String {
    if os == Os::Windows {
        root.to_string_lossy().into_owned()
    } else {
        source
    }
}

fn bytes(p: &Path) -> &[u8] {
    p.as_os_str().as_encoded_bytes()
}

#[cfg(test)]
mod disk_fields_tests {
    use super::*;

    #[test]
    fn windows_disk_source_preserves_drive_root_separator() {
        for (root, source) in [(r"C:\", "C:"), (r"D:\", "D:"), (r"e:\", "e:")] {
            assert_eq!(
                disk_source(Os::Windows, Path::new(root), source.into()),
                root
            );
        }
    }

    #[test]
    fn unix_disk_source_keeps_device_name() {
        for (os, root, source) in [
            (Os::Linux, "/", "/dev/nvme0n1p2"),
            (Os::Linux, "/mnt/data", "server:/export/data"),
            (Os::Macos, "/", "/dev/disk1s1"),
        ] {
            assert_eq!(disk_source(os, Path::new(root), source.into()), source);
        }
    }
}

/// 系统接口读不到时的默认值（旧版 `_fallback_mounts`）。
fn fallback_mounts() -> Vec<Mount> {
    let root = system_root();
    let name = root.to_string_lossy().into_owned();
    let (fstype, source) = match Os::current() {
        Os::Windows => ("ntfs", name.trim_end_matches('\\').to_string()),
        Os::Macos => ("apfs", name.clone()),
        Os::Linux => ("ext4", name.clone()),
    };
    vec![Mount {
        path: root,
        dev: name,
        fstype: fstype.to_string(),
        source,
        hidden: false,
        removable: false,
    }]
}

/// 解析 Linux 的 `/proc/self/mountinfo`（旧版 `_linux_mounts`）。
#[cfg(target_os = "linux")]
pub fn parse_mountinfo(text: &[u8]) -> Vec<Mount> {
    use std::collections::HashMap;
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let mut out: Vec<Mount> = Vec::new();
    let mut at: HashMap<PathBuf, usize> = HashMap::new();
    for line in text.split(|&b| b == b'\n') {
        // "36 35 98:0 /mnt1 /mnt2 rw,noatime master:1 - ext3 /dev/root rw"：" - " 前后两部分
        let Some(sep) = line.windows(3).position(|w| w == b" - ") else {
            continue;
        };
        let left: Vec<&[u8]> = line[..sep]
            .split(|&b| b == b' ')
            .filter(|s| !s.is_empty())
            .collect();
        let right: Vec<&[u8]> = line[sep + 3..]
            .split(|&b| b == b' ')
            .filter(|s| !s.is_empty())
            .collect();
        if left.len() < 5 || right.len() < 2 {
            continue;
        }
        let path = PathBuf::from(OsString::from_vec(unescape(left[4])));
        let mount = Mount {
            path: path.clone(),
            // 设备号 + 子树根，用于识别 bind mount
            dev: format!(
                "{}:{}",
                String::from_utf8_lossy(left[2]),
                String::from_utf8_lossy(&unescape(left[3]))
            ),
            fstype: String::from_utf8_lossy(right[0]).to_lowercase(),
            source: String::from_utf8_lossy(&unescape(right[1])).into_owned(),
            hidden: false,
            removable: false,
        };
        match at.get(&path) {
            Some(&i) => out[i] = mount,
            None => {
                at.insert(path, out.len());
                out.push(mount);
            }
        }
    }
    out
}

/// mountinfo 里空格等字符写成 `\040` 这样的八进制转义。
#[cfg(target_os = "linux")]
fn unescape(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i] == b'\\'
            && i + 3 < s.len()
            && s[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c))
        {
            let v = (s[i + 1] - b'0') as u32 * 64
                + (s[i + 2] - b'0') as u32 * 8
                + (s[i + 3] - b'0') as u32;
            if let Ok(b) = u8::try_from(v) {
                out.push(b);
                i += 4;
                continue;
            }
        }
        out.push(s[i]);
        i += 1;
    }
    out
}

#[cfg(target_os = "linux")]
fn os_mounts() -> Option<Vec<Mount>> {
    let text = std::fs::read("/proc/self/mountinfo").ok()?;
    let mounts = parse_mountinfo(&text);
    (!mounts.is_empty()).then_some(mounts)
}

#[cfg(target_os = "macos")]
fn os_mounts() -> Option<Vec<Mount>> {
    use std::ffi::CStr;
    use std::os::unix::ffi::OsStrExt;

    // 系统内部卷带 nobrowse 标志（<sys/mount.h> 的 MNT_DONTBROWSE）
    const MNT_DONTBROWSE: u32 = 0x0010_0000;
    let mut buf: *mut libc::statfs = std::ptr::null_mut();
    // SAFETY: getmntinfo 把一个由系统管理的数组地址写进 buf 并返回元素个数，数组在下次调用前有效；
    // 下面立即把需要的字段复制出来，不保留指针。
    let n = unsafe { libc::getmntinfo(&mut buf, libc::MNT_NOWAIT) };
    if n <= 0 || buf.is_null() {
        return None;
    }
    // SAFETY: 见上，buf 指向 n 个有效的 statfs。
    let entries = unsafe { std::slice::from_raw_parts(buf, n as usize) };
    let mut out = Vec::with_capacity(entries.len());
    for e in entries {
        // SAFETY: 这三个字段是系统填好的、以 0 结尾的定长字符数组。
        let (path, source, fstype) = unsafe {
            (
                CStr::from_ptr(e.f_mntonname.as_ptr()),
                CStr::from_ptr(e.f_mntfromname.as_ptr()),
                CStr::from_ptr(e.f_fstypename.as_ptr()),
            )
        };
        let source = source.to_string_lossy().into_owned();
        out.push(Mount {
            path: PathBuf::from(std::ffi::OsStr::from_bytes(path.to_bytes())),
            dev: if source.is_empty() {
                path.to_string_lossy().into_owned()
            } else {
                source.clone()
            },
            fstype: fstype.to_string_lossy().to_lowercase(),
            // 带 @ 的设备名是 Time Machine 本地快照
            hidden: (e.f_flags & MNT_DONTBROWSE) != 0 || source.contains('@'),
            source,
            removable: false,
        });
    }
    Some(out)
}

#[cfg(windows)]
fn os_mounts() -> Option<Vec<Mount>> {
    use windows_sys::Win32::Storage::FileSystem::{
        GetDriveTypeW, GetLogicalDriveStringsW, GetVolumeInformationW,
    };

    const DRIVE_CDROM: u32 = 5;
    let mut buf = [0u16; 512];
    // SAFETY: 缓冲区长度如实传入；返回值是写入的字符数（不含最后的 0），超过长度表示缓冲区不够。
    let n = unsafe { GetLogicalDriveStringsW(buf.len() as u32, buf.as_mut_ptr()) } as usize;
    if n == 0 || n > buf.len() {
        return None;
    }
    let mut out = Vec::new();
    for root in buf[..n].split(|&c| c == 0).filter(|s| !s.is_empty()) {
        let mut wroot = root.to_vec();
        wroot.push(0);
        // SAFETY: wroot 是以 0 结尾的 UTF-16 字符串。
        let dtype = unsafe { GetDriveTypeW(wroot.as_ptr()) };
        let mut fsname = [0u16; 64];
        // SAFETY: 不需要的输出参数传空指针；文件系统名缓冲区长度如实传入。
        let ok = unsafe {
            GetVolumeInformationW(
                wroot.as_ptr(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                fsname.as_mut_ptr(),
                fsname.len() as u32,
            )
        };
        let fstype = if ok != 0 {
            let end = fsname.iter().position(|&c| c == 0).unwrap_or(fsname.len());
            String::from_utf16_lossy(&fsname[..end]).to_lowercase()
        } else {
            String::new()
        };
        let path = String::from_utf16_lossy(root);
        out.push(Mount {
            path: PathBuf::from(&path),
            dev: path.clone(),
            fstype,
            source: path.trim_end_matches('\\').to_string(),
            hidden: false,
            removable: dtype == DRIVE_CDROM,
        });
    }
    Some(out)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn os_mounts() -> Option<Vec<Mount>> {
    None
}

#[cfg(unix)]
mod imp {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    use super::Usage;

    #[allow(clippy::unnecessary_cast)] // statvfs 各字段的类型因系统而异
    pub fn fs_usage(path: &Path) -> Option<Usage> {
        let c = CString::new(path.as_os_str().as_bytes()).ok()?;
        // SAFETY: statvfs 是纯数据结构，全零是合法初值；c 是以 0 结尾的路径。
        let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
            return None;
        }
        let frsize = st.f_frsize as u64;
        let blocks = st.f_blocks as u64;
        Some(Usage {
            total: blocks * frsize,
            used: blocks.saturating_sub(st.f_bfree as u64) * frsize,
            free: st.f_bavail as u64 * frsize,
        })
    }
}

#[cfg(windows)]
mod imp {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    use super::Usage;

    pub fn fs_usage(path: &Path) -> Option<Usage> {
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let (mut avail, mut total, mut total_free) = (0u64, 0u64, 0u64);
        // SAFETY: wide 以 0 结尾；三个输出参数都指向有效的 u64。
        let ok =
            unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut avail, &mut total, &mut total_free) };
        if ok == 0 {
            return None;
        }
        // 与 Python 的 shutil.disk_usage 相同：free 是当前用户可用的空间，used = total - free
        Some(Usage {
            total,
            used: total.saturating_sub(avail),
            free: avail,
        })
    }
}

#[cfg(not(any(unix, windows)))]
mod imp {
    use std::path::Path;

    use super::Usage;

    pub fn fs_usage(_path: &Path) -> Option<Usage> {
        None
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    const SAMPLE: &[u8] = b"\
22 1 259:2 / / rw,relatime shared:1 - ext4 /dev/nvme0n1p2 rw
23 22 0:21 / /proc rw,nosuid shared:5 - proc proc rw
24 22 259:3 / /home/my\\040files rw shared:2 - btrfs /dev/nvme0n1p3 rw,subvol=/home
25 22 259:3 /data /srv/bind rw shared:2 - btrfs /dev/nvme0n1p3 rw
26 22 0:30 / /tmp rw shared:6 - tmpfs tmpfs rw
27 26 0:31 / /tmp rw shared:7 - tmpfs tmpfs2 rw
garbage line
";

    #[test]
    fn mountinfo_is_parsed_like_legacy() {
        let m = parse_mountinfo(SAMPLE);
        let get = |p: &str| m.iter().find(|x| x.path == Path::new(p)).cloned();
        let home = get("/home/my files").unwrap();
        assert_eq!(
            (home.dev.as_str(), home.fstype.as_str()),
            ("259:3:/", "btrfs")
        );
        assert_eq!(get("/srv/bind").unwrap().dev, "259:3:/data");
        assert_eq!(get("/").unwrap().source, "/dev/nvme0n1p2");
        // 同一路径挂载两次时保留后面的
        assert_eq!(get("/tmp").unwrap().source, "tmpfs2");
        assert_eq!(m.len(), 5);
        assert!(is_disk_fs("btrfs") && !is_disk_fs("tmpfs") && !is_disk_fs("proc"));
    }

    #[test]
    fn real_system_has_a_root_mount_and_usage() {
        let mounts = read_mounts();
        assert!(mounts.iter().any(|m| m.path == Path::new("/")));
        let u = fs_usage(Path::new("/")).unwrap();
        assert!(u.total > 0 && u.used <= u.total);
        assert!(disk_mounts().iter().all(|d| is_disk_fs(&d.fstype)));
    }
}
