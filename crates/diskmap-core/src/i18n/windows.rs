//! 仅 Windows 编译的最小系统调用。

use windows_sys::Win32::Globalization::GetUserDefaultUILanguage;

use super::Lang;

pub(super) fn language() -> Lang {
    // SAFETY: 无参数的系统查询函数，没有指针或调用方需要维护的资源。
    let id = unsafe { GetUserDefaultUILanguage() };
    if id & 0x03ff == 0x04 {
        Lang::Zh
    } else {
        Lang::En
    }
}
