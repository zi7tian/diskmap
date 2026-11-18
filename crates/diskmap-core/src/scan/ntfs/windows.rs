//! Windows 卷句柄。只以 GENERIC_READ 打开；不请求锁卷、卸载或写权限。

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom};
use std::os::windows::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::atomic::AtomicBool;

use windows_sys::Win32::Foundation::GENERIC_READ;
use windows_sys::Win32::Storage::FileSystem::{
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, GetDiskFreeSpaceW, GetVolumeInformationW,
};

use crate::classify::Classifier;
use crate::i18n::Text;
use crate::platform;

use super::source::ReadAt;
use super::{ScanResult, reader};

struct Volume {
    file: File,
    sector: u64,
    buffer: Vec<u8>,
}

impl ReadAt for Volume {
    fn read_exact_at(&mut self, offset: u64, output: &mut [u8]) -> io::Result<()> {
        // 卷按非缓存设备处理：偏移和长度按逻辑扇区对齐，缓冲区地址按64KiB对齐。
        // 多留出的字节用于寻找已对齐的子切片，无裸指针解引用或自定义分配器。
        const CHUNK: usize = 4 * 1024 * 1024;
        const ALIGN: usize = 65536;
        if self.buffer.is_empty() {
            self.buffer.resize(CHUNK + ALIGN + 8192, 0);
        }
        let pad = (ALIGN - self.buffer.as_ptr() as usize % ALIGN) % ALIGN;
        let mut position = offset;
        for out in output.chunks_mut(CHUNK) {
            let start = position / self.sector * self.sector;
            let skip = (position - start) as usize;
            let bytes = (skip as u64 + out.len() as u64).div_ceil(self.sector) * self.sector;
            let buffer = &mut self.buffer[pad..pad + bytes as usize];
            self.file.seek(SeekFrom::Start(start))?;
            self.file.read_exact(buffer)?;
            out.copy_from_slice(&buffer[skip..skip + out.len()]);
            position = position
                .checked_add(out.len() as u64)
                .ok_or_else(|| super::invalid("卷偏移溢出 / Volume offset overflow"))?;
        }
        Ok(())
    }
}

/// 仅接受驱动器根和卷 GUID 根，拒绝 UNC、子目录及任意设备路径。
fn volume_path(root: &Path) -> Option<(String, String)> {
    let input = root.to_str()?.replace('/', "\\");
    let stripped = input.strip_prefix(r"\\?\").unwrap_or(&input);
    let b = stripped.as_bytes();
    if b.len() == 3 && b[0].is_ascii_alphabetic() && b[1..] == *b":\\" {
        let drive = (b[0] as char).to_ascii_uppercase();
        return Some((format!("{drive}:\\"), format!(r"\\.\{drive}:")));
    }
    const VOLUME_PREFIX: &[u8] = br"\\?\Volume{";
    if input
        .as_bytes()
        .get(..VOLUME_PREFIX.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(VOLUME_PREFIX))
        && input.ends_with("}\\")
    {
        let id = &input[VOLUME_PREFIX.len()..input.len() - 2];
        if id.len() == 36
            && id.bytes().enumerate().all(|(i, c)| {
                if matches!(i, 8 | 13 | 18 | 23) {
                    c == b'-'
                } else {
                    c.is_ascii_hexdigit()
                }
            })
        {
            return Some((input.clone(), input.trim_end_matches('\\').to_owned()));
        }
    }
    None
}

pub(super) fn scan_volume(
    path: &Path,
    apparent: bool,
    classifier: &Classifier,
    cancel: &AtomicBool,
    mut progress: impl FnMut(u64, u64),
) -> Result<ScanResult, Text> {
    let (root, device) = volume_path(path).ok_or_else(|| Text::new(
        "MFT 只支持整个本地卷；子文件夹和网络路径使用通用扫描",
        "MFT requires an entire local volume; subfolders and network paths use generic scanning",
    ))?;
    if !platform::is_admin() {
        return Err(Text::new(
            "读取 NTFS MFT 需要管理员权限",
            "Reading NTFS MFT requires administrator privileges",
        ));
    }
    let wide: Vec<u16> = root.encode_utf16().chain(Some(0)).collect();
    let mut fsname = [0u16; 32];
    // SAFETY: 根路径有结尾0；fsname是有效可写数组，长度如实传入，其余可选输出为空。
    let ok = unsafe {
        GetVolumeInformationW(
            wide.as_ptr(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            fsname.as_mut_ptr(),
            fsname.len() as u32,
        )
    };
    if ok == 0 {
        return Err(failure(io::Error::last_os_error()));
    }
    let length = fsname.iter().position(|c| *c == 0).unwrap_or(fsname.len());
    if String::from_utf16_lossy(&fsname[..length]) != "NTFS" {
        return Err(Text::new(
            "卷的文件系统不是 NTFS",
            "The volume filesystem is not NTFS",
        ));
    }
    let (mut sectors, mut bytes, mut free, mut total) = (0, 0, 0, 0);
    // SAFETY: 路径有结尾0，四个输出均指向独立、有效的u32。
    let ok = unsafe {
        GetDiskFreeSpaceW(
            wide.as_ptr(),
            &mut sectors,
            &mut bytes,
            &mut free,
            &mut total,
        )
    };
    if ok == 0 {
        return Err(failure(io::Error::last_os_error()));
    }
    if !(512..=4096).contains(&bytes) || !bytes.is_power_of_two() {
        return Err(Text::new(
            "不支持的 NTFS 扇区大小",
            "Unsupported NTFS sector size",
        ));
    }
    let file = OpenOptions::new()
        .access_mode(GENERIC_READ)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .open(device)
        .map_err(failure)?;
    let mut source = Volume {
        file,
        sector: bytes as u64,
        buffer: Vec::new(),
    };
    progress(0, 0);
    reader::scan(&mut source, path, apparent, classifier, cancel, progress).map_err(failure)
}

fn failure(error: io::Error) -> Text {
    Text::owned(
        format!("无法读取 NTFS MFT：{error}"),
        format!("Could not read NTFS MFT: {error}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_paths_only() {
        assert_eq!(volume_path(Path::new("C:\\")).unwrap().1, r"\\.\C:");
        assert_eq!(volume_path(Path::new(r"\\?\D:\")).unwrap().1, r"\\.\D:");
        assert!(volume_path(Path::new(r"C:\Users")).is_none());
        assert!(volume_path(Path::new(r"\\server\share\")).is_none());
        assert!(volume_path(Path::new(r"\\.\PhysicalDrive0")).is_none());
        assert!(
            volume_path(Path::new(
                r"\\?\Volume{01234567-89ab-cdef-0123-456789abcdef}\"
            ))
            .is_some()
        );
    }

    #[test]
    fn volume_guid_prefix_accepts_ascii_case_and_separator_variants() {
        let guid = "01234567-89aB-cDeF-0123-456789aBCdef";
        for prefix in ["Volume", "volume", "VOLUME", "vOlUmE"] {
            for separator in ['\\', '/'] {
                let input =
                    format!("{separator}{separator}?{separator}{prefix}{{{guid}}}{separator}");
                let normalized = input.replace('/', "\\");
                assert_eq!(
                    volume_path(Path::new(&input)),
                    Some((
                        normalized.clone(),
                        normalized.trim_end_matches('\\').to_owned()
                    )),
                    "{input}"
                );
            }
        }
    }

    #[test]
    fn volume_guid_aliases_still_require_an_exact_guid_root() {
        for input in [
            r"\\?\volume{}\",
            r"\\?\volume{01234567-89ab-cdef-0123-456789abcdeg}\",
            r"\\?\volume{01234567_89ab-cdef-0123-456789abcdef}\",
            r"\\?\volume{01234567-89ab-cdef-0123-456789abcdef}",
            r"\\?\volume{01234567-89ab-cdef-0123-456789abcdef}\child",
            r"\\.\volume{01234567-89ab-cdef-0123-456789abcdef}\",
            r"\\?\volumｅ{01234567-89ab-cdef-0123-456789abcdef}\",
            r"\\?\卷{01234567-89ab-cdef-0123-456789abcdef}\",
            r"\\?\vol",
            "",
        ] {
            assert!(volume_path(Path::new(input)).is_none(), "{input}");
        }
    }
}
