//! GUI 子系统的命令行输出：接到父控制台，同时保留文件/管道重定向。

use std::ffi::OsString;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use windows_sys::Win32::Foundation::{
    DUPLICATE_SAME_ACCESS, DuplicateHandle, GetHandleInformation, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::System::Console::{
    ATTACH_PARENT_PROCESS, AttachConsole, GetConsoleMode, GetStdHandle, STD_ERROR_HANDLE,
    STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle,
};
use windows_sys::Win32::System::Threading::GetCurrentProcess;

/// 保持恢复后的句柄有效，直到入口完成输出。MCP 和无参数的双击启动不创建此类句柄。
#[derive(Default)]
pub struct ParentConsole {
    _handles: Vec<OwnedHandle>,
}

pub fn attach_parent_console(args: &[OsString]) -> ParentConsole {
    if !super::should_attach(args) {
        return ParentConsole::default();
    }
    let ids = [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE];
    // AttachConsole 会更改进程的标准句柄表，先复制有效的继承句柄，避免覆盖重定向。
    let saved = ids.map(|id| {
        // SAFETY: id 是三个合法的标准句柄编号之一；读取不转移句柄所有权。
        duplicate(unsafe { GetStdHandle(id) })
    });
    // SAFETY: 仅尝试连接父控制台，不分配新控制台；无父控制台或已经连接时正常返回失败。
    if unsafe { AttachConsole(ATTACH_PARENT_PROCESS) } == 0 {
        return ParentConsole::default();
    }
    let mut handles = Vec::new();
    for (id, inherited) in ids.into_iter().zip(saved) {
        let handle = inherited.or_else(|| {
            // SAFETY: 同上；连接成功后读取当前标准句柄，检查是否仍需补上输出通道。
            let current = unsafe { GetStdHandle(id) };
            if valid(current) {
                return None;
            }
            let input = id == STD_INPUT_HANDLE;
            std::fs::OpenOptions::new()
                .read(input)
                .write(!input)
                .open(if input { "CONIN$" } else { "CONOUT$" })
                .ok()
                .map(OwnedHandle::from)
        });
        if let Some(handle) = handle {
            // SAFETY: 句柄属于本进程并由返回的 ParentConsole 持有；编号合法。
            // Rust 的控制台输出使用 WriteConsoleW，不依赖当前 OEM/ANSI 中文代码页。
            if unsafe { SetStdHandle(id, handle.as_raw_handle()) } != 0 {
                handles.push(handle);
            }
        }
    }
    let mut mode = 0;
    // SAFETY: 标准输出编号合法，mode 指向有效存储；GetConsoleMode 会拒绝文件、管道或无效句柄。
    let is_console = unsafe { GetConsoleMode(GetStdHandle(STD_OUTPUT_HANDLE), &mut mode) != 0 };
    // 父控制台的提示符可能已经出现，先换行。检查恢复后的 stdout，保留重定向的原始字节。
    let _ = super::write_console_separator(&mut std::io::stdout().lock(), is_console);
    ParentConsole { _handles: handles }
}

fn valid(handle: HANDLE) -> bool {
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return false;
    }
    let mut flags = 0;
    // SAFETY: API 校验不可信的句柄值；输出指向本地有效的 DWORD。
    unsafe { GetHandleInformation(handle, &mut flags) != 0 }
}

fn duplicate(handle: HANDLE) -> Option<OwnedHandle> {
    if !valid(handle) {
        return None;
    }
    let mut copy = std::ptr::null_mut();
    // SAFETY: 获取的是当前进程的伪句柄，无需关闭。
    let process = unsafe { GetCurrentProcess() };
    // SAFETY: 源句柄已校验；源和目标都是当前进程，copy 指向有效存储；复制不改变源句柄。
    let copied = unsafe {
        DuplicateHandle(
            process,
            handle,
            process,
            &mut copy,
            0,
            0,
            DUPLICATE_SAME_ACCESS,
        )
    };
    if copied == 0 {
        None
    } else {
        // SAFETY: DuplicateHandle 成功返回独占的新句柄，交给 OwnedHandle 恰好关闭一次。
        Some(unsafe { OwnedHandle::from_raw_handle(copy) })
    }
}
