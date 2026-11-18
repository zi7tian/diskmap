//! 内核路径不能使用 norm_path（它会裁剪合法空白）；只按平台折叠大小写和分隔符。

use std::borrow::Cow;
use std::path::Path;

use crate::platform::Os;
use crate::platform::path::path_key;

pub(super) fn key(os: Os, path: &Path) -> Vec<u8> {
    let mut key = path_key(os, path.as_os_str().as_encoded_bytes());
    let root = root_len(os, &key);
    while key.len() > root && key.last() == Some(&(os.sep() as u8)) {
        key.pop();
    }
    key
}

fn root_len(os: Os, path: &[u8]) -> usize {
    if os != Os::Windows {
        return usize::from(path.starts_with(b"/"));
    }
    if path.get(1..3) == Some(b":\\") {
        return 3;
    }
    if path.starts_with(b"\\\\") {
        let mut separators = path
            .iter()
            .enumerate()
            .skip(2)
            .filter(|(_, c)| **c == b'\\');
        if separators.next().is_some() {
            return separators.next().map_or(path.len(), |(i, _)| i);
        }
        return path.len();
    }
    usize::from(path.starts_with(b"\\"))
}

pub(super) fn parents(os: Os, path: &[u8]) -> impl Iterator<Item = &[u8]> {
    let root = root_len(os, path);
    let parent = move |p: &[u8]| {
        p.iter()
            .rposition(|&b| b == os.sep() as u8)
            .unwrap_or(0)
            .max(root)
    };
    std::iter::successors(Some(&path[..parent(path)]), move |p| {
        if p.len() <= root {
            None
        } else {
            Some(&p[..parent(p)])
        }
    })
}

pub(super) fn usable(os: Os, path: &Path) -> bool {
    let p = path.as_os_str().as_encoded_bytes();
    if os == Os::Windows {
        return p.len() > 3 && p[1] == b':' && matches!(p[2], b'\\' | b'/');
    }
    p.starts_with(b"/")
        && ![
            b"/proc/".as_slice(),
            b"/sys/",
            b"/dev/",
            b"/memfd:",
            b"/run/",
            b"/SYSV",
        ]
        .iter()
        .any(|prefix| p.starts_with(prefix))
}

pub(super) fn without_deleted(path: &Path) -> Cow<'_, Path> {
    let Some(bytes) = path
        .as_os_str()
        .as_encoded_bytes()
        .strip_suffix(b" (deleted)")
    else {
        return Cow::Borrowed(path);
    };
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Cow::Owned(Path::new(std::ffi::OsStr::from_bytes(bytes)).to_owned())
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        let _ = bytes;
        let mut wide: Vec<_> = path.as_os_str().encode_wide().collect();
        wide.truncate(wide.len() - " (deleted)".len());
        Cow::Owned(std::ffi::OsString::from_wide(&wide).into())
    }
    #[cfg(not(any(unix, windows)))]
    {
        Cow::Owned(String::from_utf8_lossy(bytes).into_owned().into())
    }
}
