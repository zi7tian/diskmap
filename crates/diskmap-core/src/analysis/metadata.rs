//! 报告所需的只读元数据；不跟随符号链接，字段与旧版 `App.info` / `explain` 一致。

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::i18n::Text;

/// 按系统本地时区格式化时间。系统无法表示该时间时返回 `None`。
pub(super) fn local_time(time: SystemTime, seconds: bool) -> Option<String> {
    imp::local_time(time, seconds)
}

/// 为解释结果补充当前磁盘上的元数据，不读取文件内容。
///
/// `is_file` 指扫描结果中的类型；项目扫描后变化时，仍与旧版一样报告当前 `lstat` 的长度。
/// 调用方应在本函数之后设置 denied 提示，因为旧版中 denied 会覆盖 gone 提示。
pub(super) fn enrich(out: &mut Value, path: &Path, is_file: bool) {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        out["note"] = serde_json::json!(Text::new(
            "该项目在扫描后已被删除或移动。",
            "This item was deleted or moved after the scan.",
        ));
        return;
    };
    if let Some(modified) = metadata.modified().ok().and_then(|t| local_time(t, false)) {
        out["modified"] = modified.into();
    }
    out["mode"] = imp::mode(&metadata, path).into();
    imp::owner_group(out, &metadata);
    if is_file {
        out["file_length_bytes"] = metadata.len().into();
    }
}

/// POSIX 的类型及权限位（Windows 先按 Python 的规则合成同一组位）。
fn mode_string(mode: u32) -> String {
    let kind = match mode & 0o170000 {
        0o010000 => 'p',
        0o020000 => 'c',
        0o040000 => 'd',
        0o060000 => 'b',
        0o100000 => '-',
        0o120000 => 'l',
        0o140000 => 's',
        0o160000 if cfg!(target_os = "macos") => 'w',
        _ => '?',
    };
    let mut out = String::with_capacity(10);
    out.push(kind);
    for (shift, special, lower, upper) in [
        (6, 0o4000, 's', 'S'),
        (3, 0o2000, 's', 'S'),
        (0, 0o1000, 't', 'T'),
    ] {
        let bits = mode >> shift;
        out.push(if bits & 4 != 0 { 'r' } else { '-' });
        out.push(if bits & 2 != 0 { 'w' } else { '-' });
        out.push(match (bits & 1 != 0, mode & special != 0) {
            (true, true) => lower,
            (false, true) => upper,
            (true, false) => 'x',
            (false, false) => '-',
        });
    }
    out
}

/// Python Windows `lstat` 以文件属性合成权限；可执行扩展名另加执行位。
#[cfg(any(windows, test))]
fn windows_mode(attributes: u32, symlink: bool, path: &Path) -> String {
    let directory = attributes & 0x10 != 0;
    let readonly = attributes & 0x1 != 0;
    let mut bits = if symlink {
        0o120000
    } else if directory {
        0o040000
    } else {
        0o100000
    };
    bits |= if readonly { 0o444 } else { 0o666 };
    let mut bytes = path.as_os_str().as_encoded_bytes();
    // 非扩展路径的 Win32 规则会忽略尾部空格和点，Python 也先去掉它们再判断扩展名。
    if !bytes.starts_with(br"\\?\") {
        while bytes.last().is_some_and(|b| matches!(b, b' ' | b'.')) {
            bytes = &bytes[..bytes.len() - 1];
        }
    }
    let executable = bytes
        .get(bytes.len().saturating_sub(4)..)
        .is_some_and(|ext| {
            [b".exe", b".bat", b".cmd", b".com"].iter().any(|known| {
                crate::platform::path::cmp_names(crate::platform::Os::Windows, ext, *known).is_eq()
            })
        });
    if directory || executable {
        bits |= 0o111;
    }
    mode_string(bits)
}

#[cfg(unix)]
mod imp {
    use super::*;
    use std::ffi::CStr;
    use std::fs::Metadata;
    use std::mem::MaybeUninit;
    use std::os::unix::fs::MetadataExt;

    pub(super) fn local_time(time: SystemTime, seconds: bool) -> Option<String> {
        let seconds_since_epoch = match time.duration_since(UNIX_EPOCH) {
            Ok(duration) => i128::from(duration.as_secs()),
            Err(error) => {
                let duration = error.duration();
                -i128::from(duration.as_secs()) - i128::from(duration.subsec_nanos() != 0)
            }
        };
        let stamp: libc::time_t = seconds_since_epoch.try_into().ok()?;
        let mut result = MaybeUninit::<libc::tm>::uninit();
        // SAFETY: stamp 是有效 time_t；result 指向独占且足够大的 tm 输出空间。
        // localtime_r 成功时完整初始化 tm；失败时不读取它。
        let result = unsafe {
            if libc::localtime_r(&stamp, result.as_mut_ptr()).is_null() {
                return None;
            }
            result.assume_init()
        };
        let mut out = format!(
            "{:04}-{:02}-{:02} {:02}:{:02}",
            i64::from(result.tm_year) + 1900,
            result.tm_mon + 1,
            result.tm_mday,
            result.tm_hour,
            result.tm_min,
        );
        if seconds {
            out.push_str(&format!(":{:02}", result.tm_sec));
        }
        Some(out)
    }

    pub(super) fn mode(metadata: &Metadata, _path: &Path) -> String {
        mode_string(metadata.mode())
    }

    pub(super) fn owner_group(out: &mut Value, metadata: &Metadata) {
        out["owner"] = user_name(metadata.uid()).into();
        out["group"] = group_name(metadata.gid()).into();
    }

    fn user_name(uid: libc::uid_t) -> String {
        let mut buffer = vec![0u8; 16 * 1024];
        loop {
            let mut record = MaybeUninit::<libc::passwd>::uninit();
            let mut result = std::ptr::null_mut();
            // SAFETY: record、result、buffer 都是独占可写空间，长度正确。
            // 返回成功且 result 非空时，pw_name 指向仍存活的 buffer 内的 NUL 结尾字符串。
            let status = unsafe {
                let status = libc::getpwuid_r(
                    uid,
                    record.as_mut_ptr(),
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                    &mut result,
                );
                if status == 0 && !result.is_null() {
                    let name = (*result).pw_name;
                    if !name.is_null() {
                        return CStr::from_ptr(name).to_string_lossy().into_owned();
                    }
                }
                status
            };
            // 防止损坏的 NSS 数据要求无限增长；无法解析的名字与缺失用户一样回退到数字。
            if status != libc::ERANGE || buffer.len() >= 1024 * 1024 {
                return uid.to_string();
            }
            buffer.resize(buffer.len() * 2, 0);
        }
    }

    fn group_name(gid: libc::gid_t) -> String {
        let mut buffer = vec![0u8; 16 * 1024];
        loop {
            let mut record = MaybeUninit::<libc::group>::uninit();
            let mut result = std::ptr::null_mut();
            // SAFETY: 输出结构和缓冲区均独占、长度正确；只在调用成功且 result 非空时
            // 读取其 gr_name，并在 buffer 释放或增长之前复制 NUL 结尾的名字。
            let status = unsafe {
                let status = libc::getgrgid_r(
                    gid,
                    record.as_mut_ptr(),
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                    &mut result,
                );
                if status == 0 && !result.is_null() {
                    let name = (*result).gr_name;
                    if !name.is_null() {
                        return CStr::from_ptr(name).to_string_lossy().into_owned();
                    }
                }
                status
            };
            if status != libc::ERANGE || buffer.len() >= 1024 * 1024 {
                return gid.to_string();
            }
            buffer.resize(buffer.len() * 2, 0);
        }
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::fs::Metadata;
    use std::os::windows::fs::MetadataExt;

    use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows_sys::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};

    pub(super) fn local_time(time: SystemTime, seconds: bool) -> Option<String> {
        const EPOCH_TICKS: i128 = 116_444_736_000_000_000;
        let ticks = match time.duration_since(UNIX_EPOCH) {
            Ok(duration) => EPOCH_TICKS + i128::try_from(duration.as_nanos() / 100).ok()?,
            Err(error) => {
                EPOCH_TICKS - i128::try_from(error.duration().as_nanos().div_ceil(100)).ok()?
            }
        };
        let ticks = u64::try_from(ticks).ok()?;
        let file_time = FILETIME {
            dwLowDateTime: ticks as u32,
            dwHighDateTime: (ticks >> 32) as u32,
        };
        let mut utc = SYSTEMTIME::default();
        let mut local = SYSTEMTIME::default();
        // SAFETY: 输入 FILETIME 完整初始化，两个 SYSTEMTIME 输出均独占且大小正确；
        // 空时区指针要求使用系统当前时区；任一 API 失败时不使用输出。
        unsafe {
            if FileTimeToSystemTime(&file_time, &mut utc) == 0
                || SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut local) == 0
            {
                return None;
            }
        }
        let mut out = format!(
            "{:04}-{:02}-{:02} {:02}:{:02}",
            local.wYear, local.wMonth, local.wDay, local.wHour, local.wMinute
        );
        if seconds {
            out.push_str(&format!(":{:02}", local.wSecond));
        }
        Some(out)
    }

    pub(super) fn mode(metadata: &Metadata, path: &Path) -> String {
        windows_mode(
            metadata.file_attributes(),
            metadata.file_type().is_symlink(),
            path,
        )
    }

    pub(super) fn owner_group(_out: &mut Value, _metadata: &Metadata) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn permission_types_and_special_bits() {
        assert_eq!(mode_string(0o100644), "-rw-r--r--");
        assert_eq!(mode_string(0o041777), "drwxrwxrwt");
        assert_eq!(mode_string(0o106444), "-r-Sr-Sr--");
        assert_eq!(mode_string(0o106755), "-rwsr-sr-x");
        assert_eq!(mode_string(0o121777), "lrwxrwxrwt");
        assert_eq!(mode_string(0o141000), "s--------T");
        assert_eq!(mode_string(0o010600), "prw-------");
        assert_eq!(mode_string(0o020660), "crw-rw----");
        assert_eq!(mode_string(0o060660), "brw-rw----");
        assert_eq!(mode_string(0), "?---------");
    }

    #[test]
    fn windows_permissions_match_python_attribute_rules() {
        assert_eq!(windows_mode(0, false, Path::new("data.txt")), "-rw-rw-rw-");
        assert_eq!(windows_mode(1, false, Path::new("Run.EXE")), "-r-xr-xr-x");
        assert_eq!(windows_mode(0x11, false, Path::new("dir")), "dr-xr-xr-x");
        assert_eq!(windows_mode(0x10, true, Path::new("link")), "lrwxrwxrwx");
        assert_eq!(windows_mode(0, true, Path::new("x.cmd")), "lrwxrwxrwx");
        assert_eq!(windows_mode(0, false, Path::new("x.exe. ")), "-rwxrwxrwx");
        assert_eq!(
            windows_mode(0, false, Path::new(r"\\?\C:\x.exe. ")),
            "-rw-rw-rw-"
        );
    }

    #[test]
    fn local_time_formats_minute_and_second_consistently() {
        // 固定秒值，不依赖测试机的时区；不改变进程环境，允许与其它测试并行。
        let when = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let minute = local_time(when, false).unwrap();
        let second = local_time(when, true).unwrap();
        assert_eq!(minute.len(), 16);
        assert_eq!(second.len(), 19);
        assert!(second.starts_with(&minute));
        assert_eq!(&second[16..17], ":");
        assert_eq!(&second[17..], "20");
    }

    #[cfg(unix)]
    #[test]
    fn enrich_reads_link_itself_and_missing_path_is_bilingual() {
        use crate::i18n::{Lang, scope};
        use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "diskmap-t06-metadata-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir(&root).unwrap();
        let file = root.join("target");
        std::fs::write(&file, [0u8; 1024]).unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o640)).unwrap();
        let link = root.join("link");
        symlink("target", &link).unwrap();
        // macOS 的链接权限受 umask 影响，以链接自身的元数据推导期望值。
        let link_metadata = std::fs::symlink_metadata(&link).unwrap();
        assert!(link_metadata.file_type().is_symlink());
        let mut out = serde_json::json!({});
        enrich(&mut out, &link, true);
        assert_eq!(out["file_length_bytes"], 6);
        assert_eq!(out["mode"], mode_string(link_metadata.mode()));
        assert!(out["owner"].is_string() && out["group"].is_string());
        assert!(out["modified"].is_string());
        assert!(out.get("note").is_none());
        let mut out = serde_json::json!({});
        enrich(&mut out, &file, true);
        assert_eq!(out["file_length_bytes"], 1024);
        assert_eq!(out["mode"], "-rw-r-----");
        for (lang, expected) in [
            (Lang::Zh, "该项目在扫描后已被删除或移动。"),
            (Lang::En, "This item was deleted or moved after the scan."),
        ] {
            let _scope = scope(lang);
            let mut out = serde_json::json!({});
            enrich(&mut out, &root.join("missing"), false);
            assert_eq!(out, serde_json::json!({"note": expected}));
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
