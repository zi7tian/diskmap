//! 索引的二进制编码。多字节数值均为小端，与结构体的内存布局无关。

use std::io::{self, Read, Write};

use super::*;

const COUNTS_BYTES: usize = 12;
const DIR_BYTES: usize = 48 + 8 * NCLS;
const FILE_BYTES: usize = 16;
const BUFFER_BYTES: usize = 64 * 1024;

impl Index {
    pub(crate) const MIN_ENCODED_LEN: u64 = (COUNTS_BYTES + DIR_BYTES) as u64;
    pub(crate) const MAX_ENCODED_LEN: u64 =
        COUNTS_BYTES as u64 + u32::MAX as u64 * (DIR_BYTES + FILE_BYTES + 1) as u64;

    pub(crate) fn encoded_len(&self) -> u64 {
        encoded_len(
            self.dirs.len() as u64,
            self.files.len() as u64,
            self.names.len() as u64,
        )
    }

    /// 三个 u32 长度（目录数、文件数、名字区字节数），接目录、文件和原样的名字区。
    pub(crate) fn write_encoded(&self, mut out: impl Write) -> io::Result<()> {
        for len in [self.dirs.len(), self.files.len(), self.names.len()] {
            let len = u32::try_from(len).map_err(|_| invalid_index())?;
            out.write_all(&len.to_le_bytes())?;
        }
        write_records(&mut out, &self.dirs, encode_dir)?;
        write_records(&mut out, &self.files, encode_file)?;
        out.write_all(&self.names)
    }

    pub(crate) fn read_encoded(
        mut input: impl Read,
        length: u64,
        os: Os,
        root_path: PathBuf,
    ) -> io::Result<Index> {
        let mut counts = [0; COUNTS_BYTES];
        input.read_exact(&mut counts)?;
        let [ndirs, nfiles, nnames] = [u32_at(&counts, 0), u32_at(&counts, 4), u32_at(&counts, 8)];
        if ndirs == 0 || length != encoded_len(ndirs.into(), nfiles.into(), nnames.into()) {
            return Err(invalid_index());
        }
        let dirs = read_records(&mut input, ndirs as usize, decode_dir)?;
        let files = read_records(&mut input, nfiles as usize, decode_file)?;
        let mut names = Vec::new();
        input.take(nnames.into()).read_to_end(&mut names)?;
        if names.len() != nnames as usize {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        let index = Index {
            os,
            root_path,
            dirs,
            files,
            names,
        };
        index.validate_decoded()?;
        Ok(index)
    }

    /// 打开存盘结果时，把各目录的原因编号改为恢复后的分类器中的编号。
    pub(crate) fn remap_reasons(&mut self, mapping: &[ReasonId]) -> io::Result<()> {
        for dir in &mut self.dirs {
            dir.reason = mapping
                .get(dir.reason as usize)
                .ok_or_else(invalid_index)?
                .0;
        }
        Ok(())
    }

    fn validate_decoded(&self) -> io::Result<()> {
        let mut names = Names {
            offset: 0,
            rest: &self.names,
        };
        let root = &self.dirs[0];
        let path = self.root_path.as_os_str().as_encoded_bytes();
        let root_name = &path[..path.len().min(u16::MAX as usize)];
        if root.parent != NO_PARENT || names.take(root.name_off, root.name_len)? != root_name {
            return Err(invalid_index());
        }

        // 目录可能并发写入，编号顺序与填充顺序不同。填充一次必定同时追加连续的子目录、
        // 文件和对应名字。按两个起始编号排序可恢复这一顺序，再检查没有重叠、遗漏或越界。
        let mut filled = Vec::new();
        for (id, node) in self.dirs.iter().enumerate() {
            if (id != 0 && node.parent as usize >= id)
                || node.category as usize >= NCLS
                || node.dirs_start as usize > self.dirs.len()
                || node.files_start as usize > self.files.len()
            {
                return Err(invalid_index());
            }
            let empty = node.dirs_len == 0 && node.files_len == 0;
            if node.flags & LISTED == 0 && (!empty || node.dirs_start != 0 || node.files_start != 0)
            {
                return Err(invalid_index());
            }
            if empty {
                check_totals(node, 0, 0, [0; NCLS])?;
            } else {
                filled.try_reserve(1).map_err(allocation_error)?;
                filled.push(id);
            }
        }
        filled.sort_unstable_by_key(|&id| (self.dirs[id].dirs_start, self.dirs[id].files_start));
        let (mut next_dir, mut next_file) = (1, 0);
        for id in filled {
            let node = &self.dirs[id];
            if (node.dirs_start, node.files_start) != (next_dir, next_file) {
                return Err(invalid_index());
            }
            let children = entries(&self.dirs, node.dirs_start, node.dirs_len)?;
            let files = entries(&self.files, node.files_start, node.files_len)?;
            check_children(self.os, id as u32, node, children, files, &mut names)?;
            next_dir += node.dirs_len;
            next_file += node.files_len;
        }
        if next_dir as usize != self.dirs.len()
            || next_file as usize != self.files.len()
            || !names.rest.is_empty()
        {
            return Err(invalid_index());
        }
        Ok(())
    }
}

fn entries<T>(all: &[T], start: u32, len: u32) -> io::Result<&[T]> {
    let end = start.checked_add(len).ok_or_else(invalid_index)?;
    all.get(start as usize..end as usize)
        .ok_or_else(invalid_index)
}

fn check_children(
    os: Os,
    id: u32,
    node: &DirNode,
    children: &[DirNode],
    files: &[FileEntry],
    names: &mut Names<'_>,
) -> io::Result<()> {
    let (mut nfiles, mut ndirs) = (0u64, u64::from(node.dirs_len));
    let mut csz = [0u64; NCLS];
    let mut previous = None;
    for child in children {
        if child.parent != id {
            return Err(invalid_index());
        }
        names.check_order(os, &mut previous, child.name_off, child.name_len)?;
        nfiles += u64::from(child.nfiles);
        ndirs += u64::from(child.ndirs);
        for (total, size) in csz.iter_mut().zip(child.csz) {
            *total = total.checked_add(size).ok_or_else(invalid_index)?;
        }
    }
    previous = None;
    for file in files {
        let category = file.category as usize;
        if category >= NCLS || file.flags & !UNCOUNTED != 0 {
            return Err(invalid_index());
        }
        names.check_order(os, &mut previous, file.name_off, file.name_len)?;
        if file.flags & UNCOUNTED == 0 {
            nfiles += 1;
            csz[category] = csz[category]
                .checked_add(file.size)
                .ok_or_else(invalid_index)?;
        }
    }
    check_totals(node, nfiles, ndirs, csz)
}

fn check_totals(node: &DirNode, nfiles: u64, ndirs: u64, csz: [u64; NCLS]) -> io::Result<()> {
    let size = csz.into_iter().try_fold(0u64, u64::checked_add);
    if nfiles != u64::from(node.nfiles)
        || ndirs != u64::from(node.ndirs)
        || csz != node.csz
        || size != Some(node.size)
    {
        return Err(invalid_index());
    }
    Ok(())
}

struct Names<'a> {
    offset: u32,
    rest: &'a [u8],
}

impl<'a> Names<'a> {
    fn take(&mut self, offset: u32, len: u16) -> io::Result<&'a [u8]> {
        if offset != self.offset {
            return Err(invalid_index());
        }
        let (name, rest) = self
            .rest
            .split_at_checked(len as usize)
            .ok_or_else(invalid_index)?;
        #[cfg(windows)]
        validate_windows_name(name)?;
        self.offset = offset
            .checked_add(u32::from(len))
            .ok_or_else(invalid_index)?;
        self.rest = rest;
        Ok(name)
    }

    fn check_order(
        &mut self,
        os: Os,
        previous: &mut Option<&'a [u8]>,
        offset: u32,
        len: u16,
    ) -> io::Result<()> {
        let name = self.take(offset, len)?;
        if previous.is_some_and(|before| cmp_names(os, before, name).is_gt()) {
            return Err(invalid_index());
        }
        *previous = Some(name);
        Ok(())
    }
}

/// 从合法 UTF-8 和单独的代理码元还原 UTF-16，再用本工具链的安全接口重新编码。
/// 重新编码还会拒绝用两个孤立代理项编码的代理对（应使用四字节 UTF-8）。
#[cfg(windows)]
fn validate_windows_name(name: &[u8]) -> io::Result<()> {
    use std::os::windows::ffi::OsStringExt;

    let Err(mut error) = std::str::from_utf8(name) else {
        return Ok(());
    };
    let mut units = Vec::with_capacity(name.len());
    let mut rest = name;
    loop {
        let (valid, tail) = rest.split_at(error.valid_up_to());
        units.extend(
            std::str::from_utf8(valid)
                .map_err(|_| invalid_index())?
                .encode_utf16(),
        );
        let [0xed, second @ 0xa0..=0xbf, third @ 0x80..=0xbf, tail @ ..] = tail else {
            return Err(invalid_index());
        };
        units.push(0xd000 | (u16::from(second & 0x3f) << 6) | u16::from(third & 0x3f));
        match std::str::from_utf8(tail) {
            Ok(valid) => {
                units.extend(valid.encode_utf16());
                break;
            }
            Err(next) => {
                rest = tail;
                error = next;
            }
        }
    }
    let native = OsString::from_wide(&units);
    if name != native.as_encoded_bytes() {
        return Err(invalid_index());
    }
    Ok(())
}

fn encoded_len(ndirs: u64, nfiles: u64, nnames: u64) -> u64 {
    COUNTS_BYTES as u64 + ndirs * DIR_BYTES as u64 + nfiles * FILE_BYTES as u64 + nnames
}

fn write_records<T, const WIDTH: usize>(
    out: &mut impl Write,
    records: &[T],
    encode: fn(&T, &mut [u8; WIDTH]),
) -> io::Result<()> {
    let mut buffer = [0; BUFFER_BYTES];
    for group in records.chunks(BUFFER_BYTES / WIDTH) {
        let bytes = &mut buffer[..group.len() * WIDTH];
        for (record, slot) in group.iter().zip(bytes.as_chunks_mut::<WIDTH>().0) {
            encode(record, slot);
        }
        out.write_all(bytes)?;
    }
    Ok(())
}

fn read_records<T, const WIDTH: usize>(
    input: &mut impl Read,
    count: usize,
    decode: fn(&[u8; WIDTH]) -> T,
) -> io::Result<Vec<T>> {
    let mut records = Vec::new();
    let mut buffer = [0; BUFFER_BYTES];
    while records.len() < count {
        let len = (count - records.len()).min(BUFFER_BYTES / WIDTH);
        let bytes = &mut buffer[..len * WIDTH];
        input.read_exact(bytes)?;
        if records.is_empty() {
            // 先读到数据再预留；只有实际读到的记录才写入内存。
            records.try_reserve_exact(count).map_err(allocation_error)?;
        }
        records.extend(bytes.as_chunks::<WIDTH>().0.iter().map(decode));
    }
    Ok(records)
}

fn encode_dir(d: &DirNode, bytes: &mut [u8; DIR_BYTES]) {
    bytes[0..4].copy_from_slice(&d.name_off.to_le_bytes());
    bytes[4..6].copy_from_slice(&d.name_len.to_le_bytes());
    bytes[6] = d.flags;
    bytes[7] = d.category;
    let counts = [
        d.reason,
        d.parent,
        d.dirs_start,
        d.dirs_len,
        d.files_start,
        d.files_len,
        d.nfiles,
        d.ndirs,
    ];
    for (slot, value) in bytes[8..40].as_chunks_mut::<4>().0.iter_mut().zip(counts) {
        *slot = value.to_le_bytes();
    }
    bytes[40..48].copy_from_slice(&d.size.to_le_bytes());
    for (slot, value) in bytes[48..].as_chunks_mut::<8>().0.iter_mut().zip(d.csz) {
        *slot = value.to_le_bytes();
    }
}

fn decode_dir(bytes: &[u8; DIR_BYTES]) -> DirNode {
    DirNode {
        name_off: u32_at(bytes, 0),
        name_len: u16::from_le_bytes([bytes[4], bytes[5]]),
        flags: bytes[6],
        category: bytes[7],
        reason: u32_at(bytes, 8),
        parent: u32_at(bytes, 12),
        dirs_start: u32_at(bytes, 16),
        dirs_len: u32_at(bytes, 20),
        files_start: u32_at(bytes, 24),
        files_len: u32_at(bytes, 28),
        nfiles: u32_at(bytes, 32),
        ndirs: u32_at(bytes, 36),
        size: u64_at(bytes, 40),
        csz: std::array::from_fn(|i| u64_at(bytes, 48 + i * 8)),
    }
}

fn encode_file(f: &FileEntry, bytes: &mut [u8; FILE_BYTES]) {
    bytes[0..8].copy_from_slice(&f.size.to_le_bytes());
    bytes[8..12].copy_from_slice(&f.name_off.to_le_bytes());
    bytes[12..14].copy_from_slice(&f.name_len.to_le_bytes());
    bytes[14] = f.category;
    bytes[15] = f.flags;
}

fn decode_file(bytes: &[u8; FILE_BYTES]) -> FileEntry {
    FileEntry {
        size: u64_at(bytes, 0),
        name_off: u32_at(bytes, 8),
        name_len: u16::from_le_bytes([bytes[12], bytes[13]]),
        category: bytes[14],
        flags: bytes[15],
    }
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().expect("固定字段宽度"))
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().expect("固定字段宽度"))
}

fn invalid_index() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "保存的索引结构无效 / Invalid saved index structure",
    )
}

fn allocation_error(error: std::collections::TryReserveError) -> io::Error {
    io::Error::new(io::ErrorKind::OutOfMemory, error)
}

#[cfg(test)]
mod tests;
