//! 读取一个目录的全部目录项，各系统分别实现。
//!
//! 与旧版相同的规则：
//! - 不跟随符号链接：指向目录的符号链接也按文件计，大小是链接本身；
//! - Windows 的目录联接（junction，包括卷挂载点）整个跳过，不计数；
//! - "实际占用"：Unix 为 `st_blocks × 512`；Windows 为目录信息里的分配大小（旧版按簇大小估算），
//!   非云端文件的目录分配大小为 0、长度大于 0 时另查压缩/稀疏大小；OneDrive 等"仅在线"文件计为 0。
//!   "文件长度"直接用文件大小。

use std::ffi::OsString;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

/// 目录项的种类。
pub(super) enum Kind {
    /// 要进入的子目录。
    Dir,
    /// 文件（包括符号链接、设备文件等所有不进入的项）。`link` 是硬链接数大于 1 时的（设备号, inode）。
    File { size: u64, link: Option<(u64, u64)> },
}

pub(super) struct Entry {
    pub name: OsString,
    pub kind: Kind,
}

/// 每读这么多项检查一次是否已取消。
const CANCEL_CHECK: usize = 4096;

/// 读取目录 `path`，对每一项调用 `f`。打不开目录时返回错误（没有权限时错误类型为
/// `PermissionDenied`）；读到一半的错误会跳过对应的项，与旧版相同。
pub(super) fn read_dir(
    path: &Path,
    apparent: bool,
    cancel: &AtomicBool,
    mut f: impl FnMut(Entry),
) -> io::Result<()> {
    #[cfg(windows)]
    {
        match win::read(path, apparent, cancel, &mut f) {
            Ok(()) => Ok(()),
            Err(win::Failure::Io(e)) => Err(e),
            // 个别文件系统不支持按句柄读取目录信息，改用标准库
            Err(win::Failure::Unsupported) => read_std(path, apparent, cancel, &mut f),
        }
    }
    #[cfg(not(windows))]
    {
        read_std(path, apparent, cancel, &mut f)
    }
}

/// 用标准库读取。Linux 上 `DirEntry::metadata` 相对目录句柄取信息（fstatat / statx），不再解析完整路径。
#[cfg(unix)]
fn read_std(
    path: &Path,
    apparent: bool,
    cancel: &AtomicBool,
    f: &mut dyn FnMut(Entry),
) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;

    for (i, entry) in std::fs::read_dir(path)?.enumerate() {
        if i % CANCEL_CHECK == CANCEL_CHECK - 1 && cancel.load(Ordering::Relaxed) {
            break;
        }
        let Ok(entry) = entry else { continue };
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() {
            f(Entry {
                name: entry.file_name(),
                kind: Kind::Dir,
            });
            continue;
        }
        let Ok(md) = entry.metadata() else { continue };
        let size = if apparent {
            md.len()
        } else {
            md.blocks() * 512
        };
        let link = (md.nlink() > 1).then(|| (md.dev(), md.ino()));
        f(Entry {
            name: entry.file_name(),
            kind: Kind::File { size, link },
        });
    }
    Ok(())
}

/// Windows 的后备读法：大小按 4096 字节的簇向上取整估算（与旧版相同的做法）。
/// 区分不了目录联接和指向目录的符号链接，两者都跳过。
#[cfg(windows)]
fn read_std(
    path: &Path,
    apparent: bool,
    cancel: &AtomicBool,
    f: &mut dyn FnMut(Entry),
) -> io::Result<()> {
    use std::os::windows::fs::MetadataExt;

    for (i, entry) in std::fs::read_dir(path)?.enumerate() {
        if i % CANCEL_CHECK == CANCEL_CHECK - 1 && cancel.load(Ordering::Relaxed) {
            break;
        }
        let Ok(entry) = entry else { continue };
        // Windows 上 DirEntry::metadata 直接来自目录信息，不需要额外的系统调用
        let Ok(md) = entry.metadata() else { continue };
        let attrs = md.file_attributes();
        if attrs & win::ATTR_DIRECTORY != 0 {
            if md.file_type().is_symlink() {
                continue;
            }
            f(Entry {
                name: entry.file_name(),
                kind: Kind::Dir,
            });
            continue;
        }
        let len = md.len();
        let size = if apparent {
            len
        } else if attrs & win::CLOUD_ONLY != 0 {
            0
        } else {
            len.div_ceil(4096) * 4096
        };
        f(Entry {
            name: entry.file_name(),
            kind: Kind::File { size, link: None },
        });
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn read_std(
    path: &Path,
    _apparent: bool,
    cancel: &AtomicBool,
    f: &mut dyn FnMut(Entry),
) -> io::Result<()> {
    for (i, entry) in std::fs::read_dir(path)?.enumerate() {
        if i % CANCEL_CHECK == CANCEL_CHECK - 1 && cancel.load(Ordering::Relaxed) {
            break;
        }
        let Ok(entry) = entry else { continue };
        let Ok(ft) = entry.file_type() else { continue };
        let kind = if ft.is_dir() {
            Kind::Dir
        } else {
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            Kind::File { size, link: None }
        };
        f(Entry {
            name: entry.file_name(),
            kind,
        });
    }
    Ok(())
}

#[cfg(windows)]
mod win {
    use std::cell::RefCell;
    use std::ffi::OsString;
    use std::io;
    use std::mem::offset_of;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, Ordering};

    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_INVALID_FUNCTION, ERROR_INVALID_PARAMETER, ERROR_NO_MORE_FILES,
        ERROR_NOT_SUPPORTED, GetLastError, HANDLE, INVALID_HANDLE_VALUE, SetLastError,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FULL_DIR_INFO, FILE_LIST_DIRECTORY,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FileFullDirectoryInfo,
        GetCompressedFileSizeW, GetFileInformationByHandleEx, OPEN_EXISTING,
    };

    use super::{CANCEL_CHECK, Entry, Kind, compressed_size_result, windows_file_size};

    pub(super) const ATTR_DIRECTORY: u32 = 0x10;
    const ATTR_REPARSE_POINT: u32 = 0x400;
    /// FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS | RECALL_ON_OPEN | OFFLINE：OneDrive 等"仅在线"文件，不占本地空间
    pub(super) const CLOUD_ONLY: u32 = 0x40_0000 | 0x4_0000 | 0x1000;
    const TAG_MOUNT_POINT: u32 = 0xA000_0003;
    const TAG_SYMLINK: u32 = 0xA000_000C;

    pub(super) enum Failure {
        /// 这个文件系统不支持按句柄读取目录信息（第一次读取就失败）。
        Unsupported,
        Io(io::Error),
    }

    struct Handle(HANDLE);

    impl Drop for Handle {
        fn drop(&mut self) {
            // SAFETY: 句柄由 CreateFileW 成功返回，只在这里关闭一次。
            unsafe { CloseHandle(self.0) };
        }
    }

    thread_local! {
        /// 每个扫描线程一个 64 KiB 的缓冲区，避免每个目录重新分配和清零。用 u64 保证 8 字节对齐（记录里有 i64 字段）。
        static BUF: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
    }

    pub(super) fn read(
        path: &Path,
        apparent: bool,
        cancel: &AtomicBool,
        f: &mut dyn FnMut(Entry),
    ) -> Result<(), Failure> {
        let wide = wide_path(path);
        // SAFETY: wide 是以 0 结尾的路径；打开目录必须带 FILE_FLAG_BACKUP_SEMANTICS。
        let raw = unsafe {
            CreateFileW(
                wide.as_ptr(),
                FILE_LIST_DIRECTORY,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                std::ptr::null_mut(),
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            return Err(Failure::Io(io::Error::last_os_error()));
        }
        let handle = Handle(raw);
        BUF.with_borrow_mut(|buf| {
            if buf.is_empty() {
                buf.resize(8192, 0);
            }
            let mut first = true;
            let mut count = 0usize;
            loop {
                // SAFETY: 缓冲区可写，长度如实传入；句柄有效。
                let ok = unsafe {
                    GetFileInformationByHandleEx(
                        handle.0,
                        FileFullDirectoryInfo,
                        buf.as_mut_ptr().cast(),
                        (buf.len() * 8) as u32,
                    )
                };
                if ok == 0 {
                    // SAFETY: 紧跟在失败的系统调用之后读取错误码。
                    let err = unsafe { GetLastError() };
                    if err == ERROR_NO_MORE_FILES {
                        return Ok(());
                    }
                    if !first {
                        // 读到一半出错（例如网络盘断开）：保留已经读到的项
                        return Ok(());
                    }
                    if matches!(
                        err,
                        ERROR_INVALID_PARAMETER | ERROR_NOT_SUPPORTED | ERROR_INVALID_FUNCTION
                    ) {
                        return Err(Failure::Unsupported);
                    }
                    return Err(Failure::Io(io::Error::from_raw_os_error(err as i32)));
                }
                first = false;
                let base = buf.as_ptr().cast::<u8>();
                let mut off = 0usize;
                loop {
                    // SAFETY: 系统保证每条记录完整地位于缓冲区内并按 8 字节对齐，
                    // NextEntryOffset 指向下一条记录或为 0；文件名紧跟在固定字段之后，长度为 FileNameLength 字节。
                    let (info, name) = unsafe {
                        let rec = base.add(off);
                        let info = &*rec.cast::<FILE_FULL_DIR_INFO>();
                        let name = std::slice::from_raw_parts(
                            rec.add(offset_of!(FILE_FULL_DIR_INFO, FileName))
                                .cast::<u16>(),
                            info.FileNameLength as usize / 2,
                        );
                        (info, name)
                    };
                    if name != [0x2e] && name != [0x2e, 0x2e] {
                        emit(path, info, name, apparent, f);
                    }
                    count += 1;
                    if count.is_multiple_of(CANCEL_CHECK) && cancel.load(Ordering::Relaxed) {
                        return Ok(());
                    }
                    if info.NextEntryOffset == 0 {
                        break;
                    }
                    off += info.NextEntryOffset as usize;
                }
            }
        })
    }

    /// 以 0 结尾的 UTF-16 路径。长路径加上 `\\?\` 前缀，否则超过 MAX_PATH 会打不开（标准库也这样处理）。
    fn wide_path(path: &Path) -> Vec<u16> {
        const BS: u16 = b'\\' as u16;
        let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
        let prefixed = wide.starts_with(&[BS, BS, b'?' as u16, BS])
            || wide.starts_with(&[BS, BS, b'.' as u16, BS]);
        if wide.len() >= 248 && !prefixed {
            let (head, rest) = if wide.starts_with(&[BS, BS]) {
                // \\server\share → \\?\UNC\server\share
                (r"\\?\UNC", &wide[1..])
            } else {
                (r"\\?\", &wide[..])
            };
            let mut long: Vec<u16> = head.encode_utf16().collect();
            long.extend_from_slice(rest);
            wide = long;
        }
        wide.push(0);
        wide
    }

    /// 只查询实际占用，不读取内容。失败时交给调用方保留目录枚举的 0，不能把错误码当字节数。
    fn compressed_size(path: &Path) -> Option<u64> {
        let wide = wide_path(path);
        let mut high = 0;
        // SAFETY: wide 是以 0 结尾的有效 UTF-16 路径，high 是独立可写的 u32。
        // 清除当前线程的错误码，区分合法的低 32 位全 1 与失败；该 API 只查询文件大小。
        let low = unsafe {
            SetLastError(0);
            GetCompressedFileSizeW(wide.as_ptr(), &mut high)
        };
        let error = if low == u32::MAX {
            // SAFETY: 紧跟 GetCompressedFileSizeW，期间没有其他 Windows 系统调用。
            unsafe { GetLastError() }
        } else {
            0
        };
        compressed_size_result(low, high, error)
    }

    fn emit(
        path: &Path,
        info: &FILE_FULL_DIR_INFO,
        name: &[u16],
        apparent: bool,
        f: &mut dyn FnMut(Entry),
    ) {
        let attrs = info.FileAttributes;
        // 重解析点的标记放在 EaSize 字段里（MS-FSCC 2.4.14）
        let tag = if attrs & ATTR_REPARSE_POINT != 0 {
            info.EaSize
        } else {
            0
        };
        let name = OsString::from_wide(name);
        if attrs & ATTR_DIRECTORY != 0 && tag != TAG_SYMLINK {
            if tag == TAG_MOUNT_POINT {
                return; // 目录联接、卷挂载点：指向别处，跟进去会重复计算甚至死循环
            }
            f(Entry {
                name,
                kind: Kind::Dir,
            });
            return;
        }
        let size = windows_file_size(
            apparent,
            info.AllocationSize,
            info.EndOfFile,
            attrs & CLOUD_ONLY != 0,
            tag == TAG_SYMLINK,
            || compressed_size(&path.join(&name)),
        );
        f(Entry {
            name,
            kind: Kind::File { size, link: None },
        });
    }
}

/// 单独保留判断逻辑，Linux 上也能验证额外 API 调用只发生在需要补查的条目。
#[cfg(any(windows, test))]
fn windows_file_size(
    apparent: bool,
    allocated: i64,
    logical: i64,
    cloud_only: bool,
    symlink: bool,
    compressed: impl FnOnce() -> Option<u64>,
) -> u64 {
    if apparent {
        logical.max(0) as u64
    } else if cloud_only {
        0
    } else if allocated == 0 && logical > 0 && !symlink {
        // 不要求 COMPRESSED 标记：Windows 的目录枚举也可能对 WOF/稀疏文件返回 0。
        // GetCompressedFileSizeW 会跟随符号链接，必须保留链接自身大小的原有口径。
        compressed().unwrap_or(0)
    } else {
        allocated.max(0) as u64
    }
}

#[cfg(any(windows, test))]
fn compressed_size_result(low: u32, high: u32, error: u32) -> Option<u64> {
    (low != u32::MAX || error == 0).then_some((u64::from(high) << 32) | u64::from(low))
}

#[cfg(test)]
mod compressed_size_tests {
    use super::{compressed_size_result, windows_file_size};

    #[test]
    fn fallback_only_queries_zero_allocated_nonempty_local_files() {
        for (apparent, allocated, logical, cloud, symlink, expected) in [
            (true, 0, 100, false, false, 100),
            (false, 0, 100, true, false, 0),
            (false, 4096, 100, false, false, 4096),
            (false, 0, 0, false, false, 0),
            (false, 0, -1, false, false, 0),
            (false, -1, 100, false, false, 0),
            (false, 0, 100, false, true, 0),
        ] {
            assert_eq!(
                windows_file_size(apparent, allocated, logical, cloud, symlink, || {
                    panic!("不应查询文件 / File must not be queried")
                }),
                expected
            );
        }
        for result in [Some(520_192), Some(0), None] {
            let mut calls = 0;
            assert_eq!(
                windows_file_size(false, 0, 2_926_798, false, false, || {
                    calls += 1;
                    result
                }),
                result.unwrap_or(0)
            );
            assert_eq!(calls, 1);
        }
    }

    #[test]
    fn size_query_keeps_high_word_and_disambiguates_error_sentinel() {
        assert_eq!(compressed_size_result(0, 0, 0), Some(0));
        assert_eq!(compressed_size_result(7, 2, 0), Some((2u64 << 32) + 7));
        assert_eq!(compressed_size_result(u32::MAX, 1, 0), Some(0x1_ffff_ffff));
        assert_eq!(compressed_size_result(u32::MAX, 0, 5), None);
    }
}
