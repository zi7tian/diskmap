//! macOS 的只读 libproc 查询。与旧版 psutil 一样，不枚举内存映射。
//!
//! 所有系统调用均在公共层的限时工作线程内执行；本模块还在每次查询前检查截止时间。
//! ABI 参照 Apple 的 `bsd/sys/proc_info.h` 和 `libsyscall/wrappers/libproc/libproc.c`。

use std::ffi::{OsStr, OsString};
use std::io;
use std::mem::{offset_of, size_of};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;

use super::{Collector, Kind};

// libc 已提供全部函数声明和 vnode_info_path；这里只补它尚未提供的两层结构和常量。
// https://github.com/apple-oss-distributions/xnu/blob/main/bsd/sys/proc_info.h
const PROC_PIDFDVNODEPATHINFO: libc::c_int = 2;

#[repr(C)]
#[derive(Default)]
struct ProcFileInfo {
    openflags: u32,
    status: u32,
    offset: libc::off_t,
    kind: i32,
    guardflags: u32,
}

#[repr(C)]
#[derive(Default)]
struct VnodeFdInfoWithPath {
    file: ProcFileInfo,
    vnode: libc::vnode_info_path,
}

// 同时约束 Intel 和 Apple Silicon 的 C ABI，避免补充结构与内核返回布局错位。
const _: () = assert!(size_of::<ProcFileInfo>() == 24);
const _: () = assert!(offset_of!(VnodeFdInfoWithPath, vnode) == 24);
const _: () = assert!(size_of::<VnodeFdInfoWithPath>() == 1200);

pub(super) fn collect(out: &Collector) -> io::Result<()> {
    for pid in pids(out)? {
        if out.expired() {
            break;
        }
        if pid <= 0 {
            continue;
        }
        out.process_seen();
        let comm = process_name(pid, out).unwrap_or_else(|| pid.to_string());
        if out.expired() {
            break;
        }
        if let Some(path) = executable(pid) {
            out.add(path, pid as u32, &comm, Kind::Exe);
        }
        if out.expired() {
            break;
        }
        let Some(fds) = file_descriptors(pid, out) else {
            continue;
        };
        out.process_visible();
        for fd in fds {
            if out.expired() {
                break;
            }
            if fd.proc_fd < 0 || fd.proc_fdtype != libc::PROX_FDTYPE_VNODE as u32 {
                continue;
            }
            if let Some(path) = open_file(pid, fd.proc_fd) {
                out.add(path, pid as u32, &comm, Kind::Open);
            }
        }
    }
    Ok(())
}

fn pids(out: &Collector) -> io::Result<Vec<libc::pid_t>> {
    let mut pids = vec![0; 128];
    while !out.expired() {
        let bytes = byte_count::<libc::pid_t>(pids.len())?;
        // SAFETY: 缓冲区具有 pid_t 的正确对齐和 bytes 字节可写空间，系统不会保留其指针。
        let count = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
        if count <= 0 {
            return Err(io::Error::last_os_error());
        }
        let count = count as usize;
        if count < pids.len() {
            pids.truncate(count);
            return Ok(pids);
        }
        // 列表恰好填满时重新读取，容忍两次查询间新进程出现。
        grow(&mut pids)?;
    }
    Ok(Vec::new())
}

fn process_name(pid: libc::pid_t, out: &Collector) -> Option<String> {
    // proc_name 至少需要 2 * MAXCOMLEN 字节；额外的零保证边界处仍有终止符。
    let mut name = [0_u8; 2 * libc::MAXCOMLEN + 1];
    // SAFETY: name 已初始化且足够容纳 proc_name 的输出，只读查询不会修改目标进程。
    let len = unsafe { libc::proc_name(pid, name.as_mut_ptr().cast(), name.len() as u32) };
    if len > 0 && (len as usize) < name.len() {
        return Some(String::from_utf8_lossy(&name[..len as usize]).into_owned());
    }
    if out.expired() {
        return None;
    }
    // 较新系统对 proc_name 使用的 TBSDINFO 要求同一用户，SHORTBSDINFO 可用于其他用户。
    let mut info = libc::proc_bsdshortinfo::default();
    let size = size_of::<libc::proc_bsdshortinfo>() as libc::c_int;
    // SAFETY: 使用 libc 的原生结构和真实缓冲区大小，只读查询不会保留指针。
    let len = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDT_SHORTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdshortinfo).cast(),
            size,
        )
    };
    if len != size {
        return None;
    }
    let bytes: Vec<_> = info
        .pbsi_comm
        .iter()
        .take_while(|&&ch| ch != 0)
        .map(|&ch| ch as u8)
        .collect();
    if bytes.is_empty() {
        None
    } else {
        Some(String::from_utf8_lossy(&bytes).into_owned())
    }
}

fn executable(pid: libc::pid_t) -> Option<PathBuf> {
    let mut path = [0_u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: 缓冲区尺寸恰为接口允许的最大值，生命周期覆盖调用；仅查询目标进程路径。
    let len = unsafe { libc::proc_pidpath(pid, path.as_mut_ptr().cast(), path.len() as u32) };
    if len <= 0 || len as usize >= path.len() {
        return None;
    }
    Some(PathBuf::from(OsStr::from_bytes(&path[..len as usize])))
}

fn file_descriptors(pid: libc::pid_t, out: &Collector) -> Option<Vec<libc::proc_fdinfo>> {
    let mut fds = vec![libc::proc_fdinfo::default(); 128];
    while !out.expired() {
        let bytes = byte_count::<libc::proc_fdinfo>(fds.len()).ok()?;
        // SAFETY: __error 返回当前线程有效的 errno 地址；缓冲区已初始化且大小、对齐正确。
        // 先清除本线程 errno，才能区分空列表和 libproc 同样返回 0 的权限/进程退出错误。
        let (len, errno) = unsafe {
            *libc::__error() = 0;
            let len = libc::proc_pidinfo(
                pid,
                libc::PROC_PIDLISTFDS,
                0,
                fds.as_mut_ptr().cast(),
                bytes,
            );
            (len, *libc::__error())
        };
        if len < 0 || (len == 0 && errno != 0) || len > bytes {
            return None;
        }
        if len % size_of::<libc::proc_fdinfo>() as libc::c_int != 0 {
            return None;
        }
        if len < bytes {
            fds.truncate(len as usize / size_of::<libc::proc_fdinfo>());
            return Some(fds);
        }
        grow(&mut fds).ok()?;
    }
    None
}

fn open_file(pid: libc::pid_t, fd: libc::c_int) -> Option<PathBuf> {
    let mut info = VnodeFdInfoWithPath::default();
    let size = size_of::<VnodeFdInfoWithPath>() as libc::c_int;
    // SAFETY: info 的 C ABI 布局已在上方断言；传入真实缓冲区大小，系统仅写本地结果。
    let len = unsafe {
        libc::proc_pidfdinfo(
            pid,
            fd,
            PROC_PIDFDVNODEPATHINFO,
            (&mut info as *mut VnodeFdInfoWithPath).cast(),
            size,
        )
    };
    if len != size {
        // 包括权限不足、进程退出、描述符已关闭或被复用等正常竞态。
        return None;
    }
    if info.vnode.vip_vi.vi_stat.vst_mode & libc::S_IFMT != libc::S_IFREG {
        return None;
    }
    path_from_chars(info.vnode.vip_path.as_flattened())
}

fn path_from_chars(chars: &[libc::c_char]) -> Option<PathBuf> {
    let len = chars.iter().position(|&ch| ch == 0)?;
    if len == 0 {
        return None;
    }
    let bytes = chars[..len].iter().map(|&ch| ch as u8).collect();
    Some(PathBuf::from(OsString::from_vec(bytes)))
}

fn byte_count<T>(count: usize) -> io::Result<libc::c_int> {
    count
        .checked_mul(size_of::<T>())
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| io::Error::from(io::ErrorKind::OutOfMemory))
}

fn grow<T: Default + Clone>(items: &mut Vec<T>) -> io::Result<()> {
    let new_len = items
        .len()
        .checked_mul(2)
        .ok_or_else(|| io::Error::from(io::ErrorKind::OutOfMemory))?;
    byte_count::<T>(new_len)?;
    items
        .try_reserve_exact(new_len - items.len())
        .map_err(|_| io::Error::from(io::ErrorKind::OutOfMemory))?;
    items.resize(new_len, T::default());
    Ok(())
}

pub(super) fn swapfiles() -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir("/private/var/vm") else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().as_bytes().starts_with(b"swapfile"))
        .map(|entry| entry.path())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_decoding_preserves_non_utf8_and_requires_terminator() {
        let chars = [b'/' as libc::c_char, -1, 0, b'x' as libc::c_char];
        let path = path_from_chars(&chars).unwrap();
        assert_eq!(path.as_os_str().as_bytes(), b"/\xff");
        assert!(path_from_chars(&chars[..2]).is_none());
        assert!(path_from_chars(&[0]).is_none());
    }

    #[test]
    fn list_buffer_size_stays_within_c_integer() {
        assert_eq!(byte_count::<libc::pid_t>(128).unwrap(), 512);
        assert!(byte_count::<libc::proc_fdinfo>(usize::MAX).is_err());
        assert!(byte_count::<libc::pid_t>(libc::c_int::MAX as usize).is_err());
        let mut entries = vec![libc::proc_fdinfo::default(); 2];
        grow(&mut entries).unwrap();
        assert_eq!(entries.len(), 4);
    }
}
