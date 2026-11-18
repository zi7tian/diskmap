//! NTFS 磁盘结构解析。只解释字节，不读取设备，也不执行任何写入。
//!
//! 结构依据 Microsoft 的 NTFS 3.x 开发者说明；fixup 的步长固定为 512 字节，
//! 与卷的逻辑扇区大小无关。所有偏移、长度和运行列表增量先验证，再访问缓冲区。
//! 参见 <https://learn.microsoft.com/en-us/windows/win32/devnotes/attribute-record-header>、
//! <https://learn.microsoft.com/en-us/windows/win32/devnotes/multi-sector-header>；
//! 压缩单元字段参照 <https://github.com/torvalds/linux/blob/master/fs/ntfs3/ntfs.h>。

use std::io;

const FIXUP_STRIDE: usize = 512;
const MAX_RECORD_SIZE: usize = 65536;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Boot {
    pub sector_size: u32,
    pub cluster_size: u32,
    pub record_size: u32,
    pub mft_offset: u64,
    pub volume_size: u64,
}

impl Boot {
    pub fn parse(data: &[u8]) -> io::Result<Self> {
        if data.len() < 512
            || bytes(data, 3, 8)? != b"NTFS    "
            || bytes(data, 510, 2)? != [0x55, 0xaa]
        {
            return Err(invalid("invalid NTFS boot signature"));
        }
        let sector_size = u32::from(u16_at(data, 11)?);
        if !(512..=4096).contains(&sector_size) || !sector_size.is_power_of_two() {
            return Err(invalid("invalid NTFS sector size"));
        }
        // 新格式允许用负的二进制指数表示每簇扇区数，最大簇为 2 MiB。
        let encoded = data[13];
        let sectors = match encoded {
            1..=128 => u32::from(encoded),
            0xf4..=0xff => 1u32 << (256 - u32::from(encoded)),
            _ => return Err(invalid("invalid NTFS sectors per cluster")),
        };
        let cluster_size = sector_size
            .checked_mul(sectors)
            .filter(|n| n.is_power_of_two() && *n <= 2 * 1024 * 1024)
            .ok_or_else(|| invalid("invalid NTFS cluster size"))?;
        let encoded = i8::from_ne_bytes([data[64]]);
        let record_size = if encoded > 0 {
            cluster_size.checked_mul(encoded as u32)
        } else {
            1u32.checked_shl(u32::from(encoded.unsigned_abs()))
        }
        .filter(|n| n.is_power_of_two() && (512..=MAX_RECORD_SIZE as u32).contains(n))
        .ok_or_else(|| invalid("invalid NTFS file record size"))?;
        let volume_size = u64_at(data, 40)?
            .checked_mul(u64::from(sector_size))
            .filter(|n| *n >= u64::from(cluster_size))
            .ok_or_else(|| invalid("invalid NTFS volume size"))?;
        let mft_offset = u64_at(data, 48)?
            .checked_mul(u64::from(cluster_size))
            .ok_or_else(|| invalid("NTFS MFT offset overflows"))?;
        if mft_offset
            .checked_add(u64::from(record_size))
            .is_none_or(|n| n > volume_size)
        {
            return Err(invalid("NTFS MFT is outside the volume"));
        }
        Ok(Self {
            sector_size,
            cluster_size,
            record_size,
            mft_offset,
            volume_size,
        })
    }
}

/// 一个运行段；稀疏段没有物理簇号。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Run {
    pub vcn: u64,
    pub len: u64,
    pub lcn: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub(super) struct NonResident {
    pub lowest_vcn: u64,
    pub highest_vcn: u64,
    pub allocated: u64,
    pub data_size: u64,
    pub initialized: u64,
    pub compressed_size: Option<u64>,
    pub runs: Vec<Run>,
}

#[derive(Clone, Debug)]
pub(super) enum Value {
    Resident(Vec<u8>),
    NonResident(NonResident),
}

impl Default for Value {
    fn default() -> Self {
        Self::Resident(Vec::new())
    }
}

#[derive(Clone, Debug, Default)]
pub(super) struct Attribute {
    pub ty: u32,
    pub id: u16,
    pub name: Vec<u16>,
    pub flags: u16,
    pub value: Value,
}

#[derive(Clone, Debug, Default)]
pub(super) struct RawRecord {
    pub sequence: u16,
    pub base_reference: u64,
    pub flags: u16,
    pub attrs: Vec<Attribute>,
}

impl RawRecord {
    /// 修正调用方的内存副本；从不修改磁盘或镜像。不能对同一缓冲区重复修正。
    pub fn parse(data: &mut [u8], sector_size: u32) -> io::Result<Self> {
        let mut record = Self::default();
        record.parse_reuse(data, sector_size)?;
        Ok(record)
    }

    /// 复用同位置属性的名字、驻留值和运行列表容量。调用方处理完记录后可将 attrs 放回以复用。
    /// 失败时头字段清零、属性只保留本次已成功解析的前缀，绝不暴露上一条记录的尾部属性。
    pub fn parse_reuse(&mut self, data: &mut [u8], sector_size: u32) -> io::Result<()> {
        self.sequence = 0;
        self.base_reference = 0;
        self.flags = 0;
        let mut parsed = 0;
        let result = self.parse_into(data, sector_size, &mut parsed);
        self.attrs.truncate(parsed);
        result
    }

    fn parse_into(
        &mut self,
        data: &mut [u8],
        sector_size: u32,
        parsed: &mut usize,
    ) -> io::Result<()> {
        if !(512..=4096).contains(&sector_size)
            || !sector_size.is_power_of_two()
            || !(FIXUP_STRIDE..=MAX_RECORD_SIZE).contains(&data.len())
            || !data.len().is_power_of_two()
            || bytes(data, 0, 4)? != b"FILE"
        {
            return Err(invalid("invalid NTFS file record header"));
        }
        let usa = usize::from(u16_at(data, 4)?);
        let count = usize::from(u16_at(data, 6)?);
        let usa_end = usa
            .checked_add(count * 2)
            .ok_or_else(|| invalid("NTFS fixup offset overflows"))?;
        if usa < 42
            || !usa.is_multiple_of(2)
            || count != data.len() / FIXUP_STRIDE + 1
            || usa_end > FIXUP_STRIDE - 2
        {
            return Err(invalid("invalid NTFS fixup array"));
        }
        let sequence = u16_at(data, usa)?;
        // 全部检查通过之后才修正，避免错误时留下部分修正过的记录。
        for i in 1..count {
            if u16_at(data, i * FIXUP_STRIDE - 2)? != sequence {
                return Err(invalid("NTFS fixup sequence mismatch"));
            }
        }
        for i in 1..count {
            let replacement = u16_at(data, usa + i * 2)?.to_le_bytes();
            data[i * FIXUP_STRIDE - 2..i * FIXUP_STRIDE].copy_from_slice(&replacement);
        }

        let first = usize::from(u16_at(data, 20)?);
        let used = u32_at(data, 24)? as usize;
        let allocated = u32_at(data, 28)? as usize;
        if allocated != data.len()
            || used > allocated
            || first < usa_end.max(48)
            || !first.is_multiple_of(8)
            || first.checked_add(4).is_none_or(|end| end > used)
        {
            return Err(invalid("invalid NTFS attribute region"));
        }
        let mut offset = first;
        let region = &data[..used];
        loop {
            let ty = u32_at(region, offset)?;
            if ty == u32::MAX {
                break;
            }
            if ty == 0 || !ty.is_multiple_of(16) {
                return Err(invalid("invalid NTFS attribute type"));
            }
            let len = u32_at(region, offset + 4)? as usize;
            if len < 24 || !len.is_multiple_of(8) {
                return Err(invalid("invalid NTFS attribute length"));
            }
            let attribute = bytes(region, offset, len)?;
            if *parsed == self.attrs.len() {
                self.attrs.push(Attribute::default());
            }
            parse_attribute_into(attribute, ty, &mut self.attrs[*parsed])?;
            *parsed += 1;
            // len 至少为 24，且 bytes 已验证相加不会溢出；循环一定向前推进。
            offset += len;
        }
        self.sequence = u16_at(data, 16)?;
        self.base_reference = u64_at(data, 32)?;
        self.flags = u16_at(data, 22)?;
        Ok(())
    }
}

#[cfg(test)]
fn parse_attribute(data: &[u8], ty: u32) -> io::Result<Attribute> {
    let mut attribute = Attribute::default();
    parse_attribute_into(data, ty, &mut attribute)?;
    Ok(attribute)
}

fn parse_attribute_into(data: &[u8], ty: u32, attribute: &mut Attribute) -> io::Result<()> {
    bytes(data, 0, 24)?;
    let form = data[8];
    let name_len = usize::from(data[9]);
    let name_offset = usize::from(u16_at(data, 10)?);
    let flags = u16_at(data, 12)?;
    // 仅实现已定义的压缩格式 1、加密和稀疏标志。不能把未知压缩格式当作普通流。
    if flags & !0xc001 != 0 {
        return Err(invalid("unsupported NTFS attribute flags"));
    }
    let id = u16_at(data, 14)?;
    let (header, value_offset) = match form {
        0 => {
            let value_len = u32_at(data, 16)? as usize;
            let offset = usize::from(u16_at(data, 20)?);
            if offset < 24 {
                return Err(invalid("invalid NTFS resident value offset"));
            }
            let content = bytes(data, offset, value_len)?;
            if let Value::Resident(value) = &mut attribute.value {
                value.clear();
                value.extend_from_slice(content);
            } else {
                attribute.value = Value::Resident(content.to_vec());
            }
            (24, offset)
        }
        1 => {
            bytes(data, 0, 64)?;
            let lowest_vcn = u64_at(data, 16)?;
            let highest_vcn = u64_at(data, 24)?;
            let offset = usize::from(u16_at(data, 32)?);
            let compression_unit = u16_at(data, 34)?;
            if compression_unit > 16 || (flags & 0x8001 == 0 && compression_unit != 0) {
                return Err(invalid("invalid NTFS compression unit"));
            }
            let extended = lowest_vcn == 0 && flags & 0x8001 != 0;
            let header = if extended { 72 } else { 64 };
            if offset < header || offset >= data.len() {
                return Err(invalid("invalid NTFS mapping pairs offset"));
            }
            let allocated = u64_at(data, 40)?;
            let data_size = u64_at(data, 48)?;
            let initialized = u64_at(data, 56)?;
            if lowest_vcn == 0 && initialized > data_size {
                return Err(invalid("invalid NTFS initialized data size"));
            }
            let compressed_size = if extended {
                Some(u64_at(data, 64)?)
            } else {
                None
            };
            let mut runs = match std::mem::take(&mut attribute.value) {
                Value::NonResident(value) => value.runs,
                Value::Resident(_) => Vec::new(),
            };
            parse_runs_into(&data[offset..], lowest_vcn, highest_vcn, &mut runs)?;
            if runs.is_empty() && lowest_vcn == 0 && (allocated != 0 || data_size != 0) {
                return Err(invalid("nonempty NTFS attribute has no runs"));
            }
            attribute.value = Value::NonResident(NonResident {
                lowest_vcn,
                highest_vcn,
                allocated,
                data_size,
                initialized,
                compressed_size,
                runs,
            });
            (header, offset)
        }
        _ => return Err(invalid("invalid NTFS attribute form")),
    };
    attribute.name.clear();
    if name_len != 0 {
        if name_offset < header
            || !name_offset.is_multiple_of(2)
            || name_offset
                .checked_add(name_len * 2)
                .is_none_or(|end| end > value_offset)
        {
            return Err(invalid("invalid NTFS attribute name offset"));
        }
        wide_into(data, name_offset, name_len, &mut attribute.name)?;
    }
    attribute.ty = ty;
    attribute.id = id;
    attribute.flags = flags;
    Ok(())
}

/// 解析增量编码的运行列表；偏移是有符号数，长度是正数，稀疏段不改变上一个 LCN。
#[cfg(test)]
fn parse_runs(data: &[u8], lowest: u64, highest: u64) -> io::Result<Vec<Run>> {
    let mut runs = Vec::new();
    parse_runs_into(data, lowest, highest, &mut runs)?;
    Ok(runs)
}

fn parse_runs_into(data: &[u8], lowest: u64, highest: u64, runs: &mut Vec<Run>) -> io::Result<()> {
    runs.clear();
    let mut offset = 0;
    let mut vcn = lowest;
    let mut lcn = 0i64;
    loop {
        let header = *data
            .get(offset)
            .ok_or_else(|| invalid("unterminated NTFS mapping pairs"))?;
        offset += 1;
        if header == 0 {
            break;
        }
        let len_width = usize::from(header & 15);
        let delta_width = usize::from(header >> 4);
        if !(1..=8).contains(&len_width) || delta_width > 8 {
            return Err(invalid("invalid NTFS mapping pair widths"));
        }
        let raw_len = bytes(data, offset, len_width)?;
        let mut decoded = [0u8; 8];
        decoded[..len_width].copy_from_slice(raw_len);
        let len = u64::from_le_bytes(decoded);
        if len == 0 || len > i64::MAX as u64 {
            return Err(invalid("invalid NTFS run length"));
        }
        offset += len_width;
        let physical = if delta_width == 0 {
            None
        } else {
            let raw_delta = bytes(data, offset, delta_width)?;
            let fill = if raw_delta[delta_width - 1] & 0x80 == 0 {
                0
            } else {
                0xff
            };
            let mut decoded = [fill; 8];
            decoded[..delta_width].copy_from_slice(raw_delta);
            let delta = i64::from_le_bytes(decoded);
            lcn = lcn
                .checked_add(delta)
                .filter(|n| *n >= 0)
                .ok_or_else(|| invalid("invalid NTFS run LCN delta"))?;
            let physical = lcn as u64;
            physical
                .checked_add(len)
                .filter(|n| *n <= i64::MAX as u64)
                .ok_or_else(|| invalid("NTFS physical run overflows"))?;
            Some(physical)
        };
        offset += delta_width;
        runs.push(Run {
            vcn,
            len,
            lcn: physical,
        });
        vcn = vcn
            .checked_add(len)
            .filter(|n| *n <= i64::MAX as u64)
            .ok_or_else(|| invalid("NTFS virtual run overflows"))?;
    }
    let empty = runs.is_empty() && lowest == 0 && matches!(highest, 0 | u64::MAX);
    if !empty && (highest.checked_add(1) != Some(vcn) || highest < lowest) {
        return Err(invalid("NTFS runs do not cover the declared VCN range"));
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ListEntry {
    pub ty: u32,
    pub id: u16,
    pub name: Vec<u16>,
    pub lowest_vcn: u64,
    pub reference: u64,
}

/// 列表长度以属性的 data_size 为准，不能把簇尾未使用的字节传进来。
pub(super) fn parse_attribute_list(data: &[u8]) -> io::Result<Vec<ListEntry>> {
    let mut entries = Vec::new();
    let mut offset = 0;
    while offset < data.len() {
        let header = bytes(data, offset, 26)?;
        let ty = u32_at(header, 0)?;
        let len = usize::from(u16_at(header, 4)?);
        if ty == 0 || ty == u32::MAX || !ty.is_multiple_of(16) || len < 26 || !len.is_multiple_of(8)
        {
            return Err(invalid("invalid NTFS attribute list entry"));
        }
        let entry = bytes(data, offset, len)?;
        let name_len = usize::from(entry[6]);
        let name_offset = usize::from(entry[7]);
        let name = if name_len == 0 {
            Vec::new()
        } else {
            if name_offset < 26 || !name_offset.is_multiple_of(2) {
                return Err(invalid("invalid NTFS attribute list name"));
            }
            wide(entry, name_offset, name_len)?
        };
        entries.push(ListEntry {
            ty,
            id: u16_at(entry, 24)?,
            name,
            lowest_vcn: u64_at(entry, 8)?,
            reference: u64_at(entry, 16)?,
        });
        offset += len;
    }
    Ok(entries)
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct FileName {
    pub parent_reference: u64,
    pub name: Vec<u16>,
    pub namespace: u8,
}

#[cfg(test)]
pub(super) fn parse_file_name(data: &[u8]) -> io::Result<FileName> {
    let mut name = FileName::default();
    parse_file_name_into(data, &mut name)?;
    Ok(name)
}

/// 只覆写当前名字，保留 UTF-16 缓冲区；错误输入不会留下前一条文件名或父引用。
pub(super) fn parse_file_name_into(data: &[u8], name: &mut FileName) -> io::Result<()> {
    name.parent_reference = 0;
    name.namespace = 0;
    name.name.clear();
    bytes(data, 0, 66)?;
    let len = usize::from(data[64]);
    let namespace = data[65];
    wide_into(data, 66, len, &mut name.name)?;
    // "." 仅供根记录使用；由建树层依据记录编号检查。UTF-16 原样保存，包括孤立代理项。
    if name.name.is_empty()
        || name.name.iter().any(|c| matches!(*c, 0 | 0x2f | 0x5c))
        || name.name == [0x2e, 0x2e]
        || namespace > 3
    {
        name.name.clear();
        return Err(invalid("invalid NTFS file name"));
    }
    name.parent_reference = u64_at(data, 0)?;
    name.namespace = namespace;
    Ok(())
}

pub(super) fn parse_standard_information(data: &[u8]) -> io::Result<u32> {
    // NTFS 1.x 为 48 字节；NTFS 3.x 增添的所有者、安全和 USN 字段不影响扫描。
    bytes(data, 0, 48)?;
    u32_at(data, 32)
}

pub(super) fn parse_reparse_tag(data: &[u8]) -> io::Result<u32> {
    bytes(data, 0, 8)?;
    let payload = usize::from(u16_at(data, 4)?);
    bytes(data, 8, payload)?;
    u32_at(data, 0)
}

fn bytes(data: &[u8], offset: usize, len: usize) -> io::Result<&[u8]> {
    let end = offset
        .checked_add(len)
        .ok_or_else(|| invalid("NTFS offset overflows"))?;
    data.get(offset..end)
        .ok_or_else(|| invalid("truncated NTFS structure"))
}

fn u16_at(data: &[u8], offset: usize) -> io::Result<u16> {
    let raw = bytes(data, offset, 2)?;
    Ok(u16::from_le_bytes([raw[0], raw[1]]))
}

fn u32_at(data: &[u8], offset: usize) -> io::Result<u32> {
    let raw = bytes(data, offset, 4)?;
    Ok(u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]))
}

fn u64_at(data: &[u8], offset: usize) -> io::Result<u64> {
    let raw = bytes(data, offset, 8)?;
    Ok(u64::from_le_bytes([
        raw[0], raw[1], raw[2], raw[3], raw[4], raw[5], raw[6], raw[7],
    ]))
}

fn wide(data: &[u8], offset: usize, len: usize) -> io::Result<Vec<u16>> {
    let mut name = Vec::new();
    wide_into(data, offset, len, &mut name)?;
    Ok(name)
}

fn wide_into(data: &[u8], offset: usize, len: usize, name: &mut Vec<u16>) -> io::Result<()> {
    let raw = bytes(
        data,
        offset,
        len.checked_mul(2)
            .ok_or_else(|| invalid("NTFS name length overflows"))?,
    )?;
    name.clear();
    name.extend(
        raw.as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair)),
    );
    Ok(())
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
#[path = "parse_tests.rs"]
mod tests;
