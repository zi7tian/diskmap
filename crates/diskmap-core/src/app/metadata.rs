//! 详情面板的 lstat 字段；不跟随符号链接，不持有索引锁。

use std::fs;
use std::path::Path;

use serde_json::{Value, json};

pub(super) fn append(out: &mut Value, path: &Path) {
    let Ok(st) = fs::symlink_metadata(path) else {
        out["gone"] = json!(true);
        return;
    };
    if let Some(mtime) = modified(&st) {
        out["mtime"] = json!(mtime);
    }
    out["mode"] = json!(mode(&st, path));
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        out["owner"] = json!(owner(st.uid()));
        out["group"] = json!(group(st.gid()));
    }
    if out["type"] == "file" {
        out["apparent"] = json!(st.len());
    }
}

#[cfg(unix)]
fn modified(st: &fs::Metadata) -> Option<String> {
    use std::ffi::CStr;
    use std::os::unix::fs::MetadataExt;
    let stamp = st.mtime() as libc::time_t;
    // SAFETY: tm 和输出缓冲区均为有效的独占存储，localtime_r 不保留参数指针。
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&stamp, &mut tm).is_null() {
            return None;
        }
        let mut buffer = [0 as libc::c_char; 64];
        let n = libc::strftime(
            buffer.as_mut_ptr(),
            buffer.len(),
            c"%Y-%m-%d %H:%M".as_ptr(),
            &tm,
        );
        (n > 0).then(|| {
            CStr::from_ptr(buffer.as_ptr())
                .to_string_lossy()
                .into_owned()
        })
    }
}

#[cfg(windows)]
fn modified(st: &fs::Metadata) -> Option<String> {
    use std::os::windows::fs::MetadataExt;
    use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows_sys::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
    let time = st.last_write_time();
    let ft = FILETIME {
        dwLowDateTime: time as u32,
        dwHighDateTime: (time >> 32) as u32,
    };
    // SAFETY: 两个输出结构已初始化，空时区指针表示当前系统时区。
    unsafe {
        let mut utc: SYSTEMTIME = std::mem::zeroed();
        let mut local: SYSTEMTIME = std::mem::zeroed();
        if FileTimeToSystemTime(&ft, &mut utc) == 0
            || SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut local) == 0
        {
            return None;
        }
        Some(format!(
            "{:04}-{:02}-{:02} {:02}:{:02}",
            local.wYear, local.wMonth, local.wDay, local.wHour, local.wMinute
        ))
    }
}

#[cfg(not(any(unix, windows)))]
fn modified(_: &fs::Metadata) -> Option<String> {
    None
}

#[cfg(unix)]
fn mode(st: &fs::Metadata, _: &Path) -> String {
    use std::os::unix::fs::MetadataExt;
    mode_string(st.mode())
}

#[cfg(windows)]
fn mode(st: &fs::Metadata, path: &Path) -> String {
    use std::os::windows::fs::MetadataExt;
    // 与 Python 的 Windows stat.filemode 相同：权限来自只读属性，目录及可执行扩展名带 x。
    let directory = st.file_attributes() & 0x10 != 0;
    let mut bits = if st.permissions().readonly() {
        0o444
    } else {
        0o666
    };
    if directory
        || path.extension().is_some_and(|ext| {
            ["exe", "com", "bat", "cmd"].iter().any(|x| {
                crate::platform::path::cmp_names(
                    crate::platform::Os::Windows,
                    ext.as_encoded_bytes(),
                    x.as_bytes(),
                )
                .is_eq()
            })
        })
    {
        bits |= 0o111;
    }
    bits |= if st.file_type().is_symlink() {
        0o120000
    } else if directory {
        0o040000
    } else {
        0o100000
    };
    mode_string(bits)
}

#[cfg(not(any(unix, windows)))]
fn mode(_: &fs::Metadata, _: &Path) -> String {
    "----------".into()
}

fn mode_string(mode: u32) -> String {
    let mut out = String::with_capacity(10);
    out.push(match mode & 0o170000 {
        0o040000 => 'd',
        0o120000 => 'l',
        0o100000 => '-',
        0o020000 => 'c',
        0o060000 => 'b',
        0o010000 => 'p',
        0o140000 => 's',
        0o160000 if cfg!(target_os = "macos") => 'w',
        _ => '?',
    });
    for shift in [6, 3, 0] {
        let bits = mode >> shift;
        out.push(if bits & 4 != 0 { 'r' } else { '-' });
        out.push(if bits & 2 != 0 { 'w' } else { '-' });
        let special = match shift {
            6 => 0o4000,
            3 => 0o2000,
            _ => 0o1000,
        };
        out.push(match (mode & special != 0, bits & 1 != 0, shift) {
            (true, true, 0) => 't',
            (true, false, 0) => 'T',
            (true, true, _) => 's',
            (true, false, _) => 'S',
            (false, true, _) => 'x',
            _ => '-',
        });
    }
    out
}

#[cfg(unix)]
fn owner(uid: libc::uid_t) -> String {
    // SAFETY: getpwuid_r 只写入给定结构/缓冲区；复制名称后才释放缓冲区。
    unsafe {
        let mut size = 16384;
        loop {
            let mut entry: libc::passwd = std::mem::zeroed();
            let mut found = std::ptr::null_mut();
            let mut buffer = vec![0u8; size];
            let result = libc::getpwuid_r(
                uid,
                &mut entry,
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut found,
            );
            if result == libc::ERANGE && size < 1024 * 1024 {
                size *= 2;
                continue;
            }
            if result == 0 && !found.is_null() && !entry.pw_name.is_null() {
                return std::ffi::CStr::from_ptr(entry.pw_name)
                    .to_string_lossy()
                    .into_owned();
            }
            return uid.to_string();
        }
    }
}

#[cfg(unix)]
fn group(gid: libc::gid_t) -> String {
    // SAFETY: getgrgid_r 的输出结构/缓冲区有效，返回指针仅在缓冲区生命周期内读取。
    unsafe {
        let mut size = 16384;
        loop {
            let mut entry: libc::group = std::mem::zeroed();
            let mut found = std::ptr::null_mut();
            let mut buffer = vec![0u8; size];
            let result = libc::getgrgid_r(
                gid,
                &mut entry,
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut found,
            );
            if result == libc::ERANGE && size < 1024 * 1024 {
                size *= 2;
                continue;
            }
            if result == 0 && !found.is_null() && !entry.gr_name.is_null() {
                return std::ffi::CStr::from_ptr(entry.gr_name)
                    .to_string_lossy()
                    .into_owned();
            }
            return gid.to_string();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn permissions_include_special_bits() {
        assert_eq!(mode_string(0o104755), "-rwsr-xr-x");
        assert_eq!(mode_string(0o041770), "drwxrwx--T");
        assert_eq!(mode_string(0o120777), "lrwxrwxrwx");
    }
}
