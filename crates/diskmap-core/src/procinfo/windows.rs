//! Windows 映像和数据映射；加载器模块表只用于诊断。系统句柄表和 Restart Manager 尚未实现。
//!
//! 所有查询只读，不提权、不读取进程内存。刷新在公共层的限时工作线程中执行；
//! 每次系统调用之间检查截止时间，权限不足、进程退出或映射卸载时跳过，不重试。

use std::ffi::{OsString, c_void};
use std::io;
use std::mem::size_of;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;
use std::ptr;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{Value, json};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_INVALID_PARAMETER, ERROR_NO_MORE_FILES, HANDLE, HMODULE,
    INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{GetLogicalDrives, QueryDosDeviceW};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Memory::{
    MEM_COMMIT, MEM_IMAGE, MEM_MAPPED, MEM_RESERVE, MEMORY_BASIC_INFORMATION, VirtualQueryEx,
};
use windows_sys::Win32::System::ProcessStatus::{
    K32EnumProcessModulesEx, K32GetMappedFileNameW, K32GetModuleFileNameExW, LIST_MODULES_ALL,
};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_ACCESS_RIGHTS, PROCESS_QUERY_INFORMATION,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ, QueryFullProcessImageNameW,
};

use super::windows_rules::{
    self, Allocations, DevicePaths, MappedFiles, MappingKind, MappingStats, Region,
    path_difference, wide_bytes,
};
use super::{Collector, DEFAULT_BUDGET, Kind};
use crate::i18n::Text;

// 缓冲区按 UTF-16 码元计；不会把截断的路径登记成文件。
const PATH_CAPACITY: usize = 32_768;
const MAX_MODULES: usize = 65_536;

// 纯函数模块在 Linux 上使用自己的常量，Windows 编译时核对它们与 SDK 声明相同。
const _: () = {
    assert!(windows_rules::MEM_COMMIT == MEM_COMMIT);
    assert!(windows_rules::MEM_RESERVE == MEM_RESERVE);
    assert!(windows_rules::MEM_IMAGE == MEM_IMAGE);
    assert!(windows_rules::MEM_MAPPED == MEM_MAPPED);
};

struct Handle(HANDLE);

impl Handle {
    fn process(pid: u32, access: PROCESS_ACCESS_RIGHTS) -> io::Result<Self> {
        // SAFETY: 只按给定的查询权限打开 PID，不继承句柄；失败返回空指针。
        let raw = unsafe { OpenProcess(access, 0, pid) };
        if raw.is_null() {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self(raw))
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: Handle 只包装有效句柄，不可克隆，在这里恰好关闭一次。
        unsafe { CloseHandle(self.0) };
    }
}

pub(super) fn collect(out: &Collector) -> io::Result<()> {
    if out.expired() {
        return Ok(());
    }
    // SAFETY: 仅拍摄全系统进程列表，不请求堆、线程或模块，不继承句柄。
    let raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if raw == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let snapshot = Handle(raw);
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..PROCESSENTRY32W::default()
    };
    if out.expired() {
        return Ok(());
    }
    // SAFETY: 快照有效，entry 已初始化并设置正确大小。
    let mut found = unsafe { Process32FirstW(snapshot.0, &mut entry) } != 0;
    if !found {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_NO_MORE_FILES as i32) {
            return Err(error);
        }
    }
    let (devices, _) = device_paths(|| out.expired());
    // 所有进程复用一个路径缓冲区，不为每个映射分配 64 KiB。
    let mut path = vec![0_u16; PATH_CAPACITY];
    while found && !out.expired() {
        let pid = entry.th32ProcessID;
        if pid != 0 {
            out.process_seen();
            let end = entry
                .szExeFile
                .iter()
                .position(|c| *c == 0)
                .unwrap_or(entry.szExeFile.len());
            let mut comm = String::from_utf16_lossy(&entry.szExeFile[..end]);
            if comm.is_empty() {
                comm = pid.to_string();
            }
            collect_process(out, pid, &comm, &devices, &mut path);
        }
        if out.expired() {
            break;
        }
        // SAFETY: 快照仍有效，entry 与 Process32FirstW 使用的结构相同。
        found = unsafe { Process32NextW(snapshot.0, &mut entry) } != 0;
    }
    Ok(())
}

fn collect_process(out: &Collector, pid: u32, comm: &str, devices: &DevicePaths, path: &mut [u16]) {
    if out.expired() {
        return;
    }
    // VirtualQueryEx 和 GetMappedFileNameW 都只要求 QUERY_INFORMATION，不再请求 VM_READ。
    // 成功时同一只读句柄也能查询 exe；拒绝此权限时仍尝试较弱的 exe 查询。
    let process = Handle::process(pid, PROCESS_QUERY_INFORMATION).ok();
    let limited = if process.is_none() && !out.expired() {
        Handle::process(pid, PROCESS_QUERY_LIMITED_INFORMATION).ok()
    } else {
        None
    };
    let exe = process.as_ref().or(limited.as_ref()).and_then(|handle| {
        (!out.expired())
            .then(|| executable_path(handle, path))
            .flatten()
    });
    if let Some(exe) = &exe {
        out.add(
            PathBuf::from(OsString::from_wide(exe)),
            pid,
            comm,
            Kind::Exe,
        );
    }
    let Some(process) = process else { return };
    let mut files = MappedFiles::new(exe.as_deref());
    let stats = walk_mappings(
        &process,
        devices,
        path,
        || out.expired(),
        |_, _, _, mapped| {
            if let Some(mapped) = mapped
                && let Some(kind) = files.observe(mapped)
            {
                out.add(PathBuf::from(OsString::from_wide(mapped)), pid, comm, kind);
            }
        },
    );
    // 与旧版一样按映射表是否可读计数，单个区域取名或盘符转换失败只进入诊断统计。
    if stats.visible() {
        out.process_visible();
    }
}

fn executable_path(process: &Handle, path: &mut [u16]) -> Option<Vec<u16>> {
    let mut count = path.len() as u32;
    // SAFETY: 只读查询句柄有效；缓冲区和长度指针有效，flags=0 请求 Win32 路径。
    let ok = unsafe { QueryFullProcessImageNameW(process.0, 0, path.as_mut_ptr(), &mut count) };
    if ok == 0 || count == 0 || count as usize >= path.len() {
        return None;
    }
    Some(path[..count as usize].to_vec())
}

#[derive(Default, Serialize)]
struct DeviceStats {
    logical_drives_calls: usize,
    query_dos_device_calls: usize,
    errors: Vec<Text>,
}

fn device_paths(expired: impl Fn() -> bool) -> (DevicePaths, DeviceStats) {
    let mut devices = DevicePaths::default();
    let mut stats = DeviceStats::default();
    if expired() {
        return (devices, stats);
    }
    stats.logical_drives_calls += 1;
    // SAFETY: GetLogicalDrives 不取指针，只读取当前进程可见的盘符位图。
    let drives = unsafe { GetLogicalDrives() };
    if drives == 0 {
        stats
            .errors
            .push(query_error("GetLogicalDrives", &io::Error::last_os_error()));
    }
    let mut target = vec![0_u16; PATH_CAPACITY];
    for index in 0..26 {
        if expired() {
            break;
        }
        if drives & (1 << index) == 0 {
            continue;
        }
        let name = [u16::from(b'A') + index, u16::from(b':'), 0];
        stats.query_dos_device_calls += 1;
        // SAFETY: name 以 NUL 结尾且不含尾分隔符，target 可写，长度按 UTF-16 码元计。
        let copied =
            unsafe { QueryDosDeviceW(name.as_ptr(), target.as_mut_ptr(), target.len() as u32) }
                as usize;
        if copied > 0 && copied <= target.len() {
            devices.insert(&name[..2], &target[..copied]);
        } else {
            stats.errors.push(query_error(
                &format!("QueryDosDeviceW({}:)", char::from(b'A' + index as u8)),
                &io::Error::last_os_error(),
            ));
        }
    }
    (devices, stats)
}

fn walk_mappings(
    process: &Handle,
    devices: &DevicePaths,
    path: &mut [u16],
    expired: impl Fn() -> bool,
    mut visit: impl FnMut(Region, MappingKind, &[u16], Option<&[u16]>),
) -> MappingStats {
    let mut stats = MappingStats::default();
    let mut allocations = Allocations::default();
    let mut address = 0;
    loop {
        if expired() {
            stats.timed_out = true;
            break;
        }
        let mut info = MEMORY_BASIC_INFORMATION::default();
        stats.virtual_query_calls += 1;
        // SAFETY: 只读查询句柄有效，info 使用 windows-sys 的 SDK 结构，长度与可写缓冲区相等；
        // address 只作为远程地址传入，不在本进程解引用，也不访问目标进程的内容。
        let copied = unsafe {
            VirtualQueryEx(
                process.0,
                address as *const c_void,
                &mut info,
                size_of::<MEMORY_BASIC_INFORMATION>(),
            )
        };
        if copied == 0 {
            let error = io::Error::last_os_error();
            // 按文档，超过目标最高地址时返回 INVALID_PARAMETER；首个查询失败不能算可见。
            if address != 0 && error.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32) {
                stats.address_walk_complete = true;
            } else {
                stats.query_winerror = error.raw_os_error();
            }
            break;
        }
        if copied != size_of::<MEMORY_BASIC_INFORMATION>() {
            stats.invalid_region = true;
            break;
        }
        let region = Region {
            base: info.BaseAddress as usize,
            allocation_base: info.AllocationBase as usize,
            size: info.RegionSize,
            state: info.State,
            region_type: info.Type,
        };
        let Some(next) = region.next_address(address) else {
            stats.invalid_region = true;
            break;
        };
        if let Some(kind) = allocations.observe(region) {
            match kind {
                MappingKind::Image => stats.image_allocations += 1,
                MappingKind::Data => stats.data_allocations += 1,
            }
            if expired() {
                stats.timed_out = true;
                break;
            }
            stats.mapped_filename_calls += 1;
            // SAFETY: 远程区域来自本轮 VirtualQueryEx，仅作为查询地址传递；句柄、缓冲区
            // 和长度有效。每个分配只查询第一个遇到的映像/数据映射区域。
            let copied = unsafe {
                K32GetMappedFileNameW(
                    process.0,
                    region.base as *const c_void,
                    path.as_mut_ptr(),
                    path.len() as u32,
                )
            } as usize;
            if copied > 0 && copied < path.len() {
                let raw = &path[..copied];
                let mapped = devices.translate(raw);
                if mapped.is_none() {
                    stats.unconverted_paths += 1;
                }
                visit(region, kind, raw, mapped.as_deref());
            } else {
                // 匿名的页文件支持映射也可能没有文件名；计入诊断，不重试其他保护区域。
                stats.filename_failures += 1;
                if let Some(code) = io::Error::last_os_error().raw_os_error() {
                    *stats.filename_winerrors.entry(code).or_default() += 1;
                }
            }
        }
        address = next;
    }
    stats
}

#[derive(Default, Serialize)]
struct ModuleStats {
    enum_calls: usize,
    filename_calls: usize,
    complete: bool,
    error: Option<Text>,
}

fn module_paths(
    process: &Handle,
    path: &mut [u16],
    expired: impl Fn() -> bool,
) -> (Vec<Vec<u16>>, ModuleStats) {
    let mut stats = ModuleStats::default();
    let mut result = Vec::new();
    let mut modules = vec![ptr::null_mut(); 256];
    let mut files = MappedFiles::new(None);
    while !expired() {
        let mut needed = 0;
        stats.enum_calls += 1;
        // SAFETY: 诊断句柄具备 QUERY_INFORMATION | VM_READ；modules 已初始化且大小正确，
        // needed 指针有效；返回的远程 HMODULE 仅传回系统查询，不解引用或关闭。
        let ok = unsafe {
            K32EnumProcessModulesEx(
                process.0,
                modules.as_mut_ptr(),
                (modules.len() * size_of::<HMODULE>()) as u32,
                &mut needed,
                LIST_MODULES_ALL,
            )
        };
        if ok == 0 {
            stats.error = Some(query_error(
                "EnumProcessModulesEx",
                &io::Error::last_os_error(),
            ));
            break;
        }
        let Some(count) = module_count(needed) else {
            stats.error = Some(Text::new(
                "模块表长度无效或超出上限",
                "invalid or oversized module table",
            ));
            break;
        };
        if count > modules.len() {
            modules.resize(
                count.max(modules.len() * 2).min(MAX_MODULES),
                ptr::null_mut(),
            );
            continue;
        }
        let mut complete = true;
        for &module in &modules[..count] {
            if expired() {
                return (result, stats);
            }
            if module.is_null() {
                continue;
            }
            stats.filename_calls += 1;
            // SAFETY: process 仍有效，module 来自该进程模块表，path 可写且长度正确。
            let copied = unsafe {
                K32GetModuleFileNameExW(process.0, module, path.as_mut_ptr(), path.len() as u32)
            } as usize;
            if copied > 0 && copied < path.len() - 1 {
                if files.observe(&path[..copied]).is_some() {
                    result.push(path[..copied].to_vec());
                }
            } else {
                complete = false;
                stats.error = Some(query_error(
                    "GetModuleFileNameExW",
                    &io::Error::last_os_error(),
                ));
            }
        }
        stats.complete = complete;
        break;
    }
    (result, stats)
}

fn module_count(bytes: u32) -> Option<usize> {
    let bytes = bytes as usize;
    if !bytes.is_multiple_of(size_of::<HMODULE>()) {
        return None;
    }
    let count = bytes / size_of::<HMODULE>();
    (count <= MAX_MODULES).then_some(count)
}

fn query_error(operation: &str, error: &io::Error) -> Text {
    Text::owned(
        format!("{operation} 只读查询失败：{error}"),
        format!("{operation} read-only query failed: {error}"),
    )
}

fn path_json(path: &[u16]) -> Value {
    let hex: String = wide_bytes(path)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    json!({"path":OsString::from_wide(path).to_string_lossy(),"path_hex":hex})
}

/// 只读诊断一个 PID；输出加载器路径、真实映射路径、T15 集合差异和实际查询次数。
/// 预算最多 12 秒，在系统调用间检查；这与刷新一样不承诺单次系统调用的硬实时上限。
pub fn diagnose_windows_process(pid: u32, budget: Duration) -> Value {
    let start = Instant::now();
    let deadline = start + budget.min(DEFAULT_BUDGET);
    let expired = || Instant::now() >= deadline;
    let (devices, device_stats) = device_paths(expired);
    let mut path = vec![0_u16; PATH_CAPACITY];
    let mut errors = Vec::new();
    let process = if expired() {
        None
    } else {
        match Handle::process(pid, PROCESS_QUERY_INFORMATION) {
            Ok(handle) => Some(handle),
            Err(error) => {
                errors.push(query_error(
                    "OpenProcess(PROCESS_QUERY_INFORMATION)",
                    &error,
                ));
                None
            }
        }
    };
    let exe = process.as_ref().and_then(|p| {
        if expired() {
            None
        } else {
            executable_path(p, &mut path)
        }
    });
    let (modules, module_stats) = if expired() {
        (Vec::new(), ModuleStats::default())
    } else {
        match Handle::process(pid, PROCESS_QUERY_INFORMATION | PROCESS_VM_READ) {
            Ok(handle) => module_paths(&handle, &mut path, expired),
            Err(error) => {
                errors.push(query_error(
                    "OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ)",
                    &error,
                ));
                (Vec::new(), ModuleStats::default())
            }
        }
    };
    let mut mappings = Vec::new();
    let mut mapped_paths = Vec::new();
    let mapping_stats = process.as_ref().map(|process| {
        walk_mappings(
            process,
            &devices,
            &mut path,
            expired,
            |region, kind, raw, mapped| {
                if let Some(mapped) = mapped {
                    mapped_paths.push(mapped.to_vec());
                }
                mappings.push(json!({
                "allocation_base":format!("0x{:x}",region.allocation_base),
                "query_address":format!("0x{:x}",region.base),
                "region_type":kind.key(),"device_path":path_json(raw),"file":mapped.map(path_json),
            }));
            },
        )
    });
    json!({
        "pid":pid,"time":start.elapsed().as_secs_f64(),"timed_out":expired(),
        "executable":exe.as_deref().map(path_json),
        "modules":modules.iter().map(|p|path_json(p)).collect::<Vec<_>>(),
        "mappings":mappings,
        "module_only":path_difference(&modules,&mapped_paths).iter().map(|p|path_json(p)).collect::<Vec<_>>(),
        "mapping_only":path_difference(&mapped_paths,&modules).iter().map(|p|path_json(p)).collect::<Vec<_>>(),
        "device_stats":device_stats,"module_stats":module_stats,"mapping_stats":mapping_stats,"errors":errors,
        "note":Text::new(
            "模块表和映射顺序采样；模块装卸、进程退出与权限会影响差异。匿名映射取不到文件名，未转换的设备路径不会登记；保留 path_hex 供无损核对。",
            "Module and mapping samples are sequential; unloads, exits and permissions affect differences. Anonymous mappings have no filename; untranslated device paths are skipped. path_hex preserves lossless paths."
        ),
    })
}

/// 与旧版一致：仅登记系统盘中实际存在的 pagefile.sys 和 swapfile.sys。
pub(super) fn swapfiles() -> Vec<PathBuf> {
    let root = crate::platform::system_root();
    ["pagefile.sys", "swapfile.sys"]
        .into_iter()
        .map(|name| root.join(name))
        .filter(|path| path.exists())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_table_lengths_are_bounded_and_aligned() {
        let unit = size_of::<HMODULE>() as u32;
        assert_eq!(module_count(0), Some(0));
        assert_eq!(module_count(unit * 300), Some(300));
        assert_eq!(module_count(unit * MAX_MODULES as u32), Some(MAX_MODULES));
        assert_eq!(module_count(unit * (MAX_MODULES as u32 + 1)), None);
        assert_eq!(module_count(unit + 1), None);
    }
}
