//! 延后组装的记录只保存已解析属性；不保留 FILE 原始扇区或未使用的驻留文件内容。
//!
//! 属性匹配需要的类型、实例号、名字、flags、VCN 和运行列表全部保留。DATA 和未知驻留
//! 属性只保存原长度；恢复时用同长度零字节代替未被扫描器解释的内容，大小计算、INDEX_ROOT
//! 长度检查和聚合上限仍与原属性相同。真正解释的 SI、FILE_NAME、LIST、REPARSE 保留原内容。

use std::io;
use std::mem::size_of;

use super::invalid;
use super::parse::{Attribute, NonResident, RawRecord, Run, Value};

const MAX_RESIDENT: usize = 65536;
const RECORD_CHUNK: usize = 1024;
const ARENA_CHUNK: usize = 64 * 1024;

#[derive(Clone, Copy)]
struct Record {
    reference: u64,
    base_reference: u64,
    start: u32,
    length: u32,
    flags: u16,
    attributes: u32,
}

/// 两个连续区分别保存定长目录和变长属性，没有逐记录/逐属性的长期堆分配。
#[derive(Default)]
pub(super) struct Store {
    records: Vec<Record>,
    arena: Vec<u8>,
    sorted: bool,
}

impl Store {
    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn reference_at(&self, index: usize) -> Option<u64> {
        self.records.get(index).map(|record| record.reference)
    }

    pub fn heap_bytes(&self) -> usize {
        self.records.capacity() * size_of::<Record>() + self.arena.capacity()
    }

    pub fn push(&mut self, reference: u64, record: &RawRecord) -> io::Result<()> {
        if (reference >> 48) as u16 != record.sequence {
            return Err(invalid(
                "NTFS compact record sequence disagrees with reference",
            ));
        }
        let attributes = u32::try_from(record.attrs.len())
            .map_err(|_| invalid("too many NTFS compact record attributes"))?;
        let mut length = 0usize;
        for attribute in &record.attrs {
            length = length
                .checked_add(encoded_len(attribute)?)
                .ok_or_else(|| invalid("NTFS compact record length overflows"))?;
        }
        let start = u32::try_from(self.arena.len())
            .map_err(|_| capacity_error("NTFS compact attribute arena exceeds 4 GiB"))?;
        let stored_length = u32::try_from(length)
            .map_err(|_| capacity_error("NTFS compact record exceeds 4 GiB"))?;
        self.arena
            .len()
            .checked_add(length)
            .filter(|end| *end <= u32::MAX as usize)
            .ok_or_else(|| capacity_error("NTFS compact attribute arena exceeds 4 GiB"))?;
        reserve_chunked(&mut self.records, 1, RECORD_CHUNK)?;
        reserve_chunked(&mut self.arena, length, ARENA_CHUNK)?;
        for attribute in &record.attrs {
            encode(attribute, &mut self.arena);
        }
        self.records.push(Record {
            reference,
            base_reference: record.base_reference,
            start,
            length: stored_length,
            flags: record.flags,
            attributes,
        });
        self.sorted = false;
        Ok(())
    }

    /// 收集完成后排序。重复的完整引用不会被任意挑选为有效记录。
    pub fn sort(&mut self) -> io::Result<()> {
        self.records.sort_unstable_by_key(|record| record.reference);
        if self
            .records
            .windows(2)
            .any(|pair| pair[0].reference == pair[1].reference)
        {
            return Err(invalid("duplicate NTFS compact record reference"));
        }
        self.sorted = true;
        Ok(())
    }

    /// 完整引用包括序号；相同槽位的旧序号绝不命中新记录。
    pub fn get(&self, reference: u64) -> io::Result<Option<RawRecord>> {
        if !self.sorted {
            return Err(invalid("NTFS compact record lookup before sorting"));
        }
        let Ok(index) = self
            .records
            .binary_search_by_key(&reference, |record| record.reference)
        else {
            return Ok(None);
        };
        let record = self.records[index];
        let start = record.start as usize;
        let end = start
            .checked_add(record.length as usize)
            .ok_or_else(|| invalid("NTFS compact record range overflows"))?;
        let data = self
            .arena
            .get(start..end)
            .ok_or_else(|| invalid("truncated NTFS compact record"))?;
        let mut cursor = Cursor { data, offset: 0 };
        if record.attributes as usize > data.len() / 16 {
            return Err(invalid("truncated NTFS compact attribute headers"));
        }
        let mut attrs = Vec::new();
        attrs
            .try_reserve_exact(record.attributes as usize)
            .map_err(|_| capacity_error("cannot allocate NTFS compact attributes"))?;
        for _ in 0..record.attributes {
            attrs.push(decode(&mut cursor)?);
        }
        if cursor.offset != data.len() {
            return Err(invalid("trailing NTFS compact record bytes"));
        }
        Ok(Some(RawRecord {
            sequence: (reference >> 48) as u16,
            base_reference: record.base_reference,
            flags: record.flags,
            attrs,
        }))
    }
}

fn capacity_error(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::OutOfMemory, message)
}

/// 固定小步增长，避免 Vec 倍增导致紧凑表在交付前突然多占近一倍内存。
fn reserve_chunked<T>(values: &mut Vec<T>, additional: usize, chunk: usize) -> io::Result<()> {
    let end = values
        .len()
        .checked_add(additional)
        .ok_or_else(|| capacity_error("NTFS compact capacity overflows"))?;
    if end > values.capacity() {
        let capacity = end
            .checked_add(chunk - 1)
            .map(|n| n / chunk * chunk)
            .ok_or_else(|| capacity_error("NTFS compact capacity overflows"))?;
        values
            .try_reserve_exact(capacity - values.len())
            .map_err(|_| capacity_error("cannot allocate NTFS compact record storage"))?;
    }
    Ok(())
}

fn keep_resident(ty: u32) -> bool {
    matches!(ty, 0x10 | 0x20 | 0x30 | 0xc0)
}

fn encoded_len(attribute: &Attribute) -> io::Result<usize> {
    if attribute.name.len() > 255 {
        return Err(invalid(
            "NTFS compact attribute name exceeds 255 UTF-16 units",
        ));
    }
    let payload = match &attribute.value {
        Value::Resident(value) => {
            if value.len() > MAX_RESIDENT {
                return Err(invalid(
                    "NTFS compact resident attribute exceeds record limit",
                ));
            }
            if keep_resident(attribute.ty) {
                value.len()
            } else {
                0
            }
        }
        Value::NonResident(value) => {
            // 解析后的 LCN 来自非负 i64；u64::MAX 可无歧义表示空洞。
            if value.runs.iter().any(|run| run.lcn == Some(u64::MAX)) {
                return Err(invalid("invalid NTFS compact run location"));
            }
            u32::try_from(value.runs.len()).map_err(|_| invalid("too many NTFS compact runs"))?;
            value
                .runs
                .len()
                .checked_mul(24)
                .and_then(|n| n.checked_add(48))
                .ok_or_else(|| invalid("NTFS compact run storage overflows"))?
        }
    };
    payload
        .checked_add(16 + attribute.name.len() * 2)
        .ok_or_else(|| invalid("NTFS compact attribute length overflows"))
}

fn encode(attribute: &Attribute, output: &mut Vec<u8>) {
    output.extend_from_slice(&attribute.ty.to_le_bytes());
    output.extend_from_slice(&attribute.id.to_le_bytes());
    output.extend_from_slice(&attribute.flags.to_le_bytes());
    output.extend_from_slice(&(attribute.name.len() as u16).to_le_bytes());
    let (form, length) = match &attribute.value {
        Value::Resident(value) => (u16::from(!keep_resident(attribute.ty)), value.len()),
        Value::NonResident(value) => (
            2 + u16::from(value.compressed_size.is_some()),
            value.runs.len(),
        ),
    };
    output.extend_from_slice(&form.to_le_bytes());
    output.extend_from_slice(&(length as u32).to_le_bytes());
    for unit in &attribute.name {
        output.extend_from_slice(&unit.to_le_bytes());
    }
    match &attribute.value {
        Value::Resident(value) => {
            if keep_resident(attribute.ty) {
                output.extend_from_slice(value);
            }
        }
        Value::NonResident(value) => {
            for number in [
                value.lowest_vcn,
                value.highest_vcn,
                value.allocated,
                value.data_size,
                value.initialized,
                value.compressed_size.unwrap_or(0),
            ] {
                output.extend_from_slice(&number.to_le_bytes());
            }
            for run in &value.runs {
                for number in [run.vcn, run.len, run.lcn.unwrap_or(u64::MAX)] {
                    output.extend_from_slice(&number.to_le_bytes());
                }
            }
        }
    }
}

struct Cursor<'a> {
    data: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, count: usize) -> io::Result<&'a [u8]> {
        let end = self
            .offset
            .checked_add(count)
            .ok_or_else(|| invalid("NTFS compact cursor overflows"))?;
        let bytes = self
            .data
            .get(self.offset..end)
            .ok_or_else(|| invalid("truncated NTFS compact attribute"))?;
        self.offset = end;
        Ok(bytes)
    }
    fn u16(&mut self) -> io::Result<u16> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> io::Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn u64(&mut self) -> io::Result<u64> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }
}

fn decode(cursor: &mut Cursor<'_>) -> io::Result<Attribute> {
    let ty = cursor.u32()?;
    let id = cursor.u16()?;
    let flags = cursor.u16()?;
    let name_len = usize::from(cursor.u16()?);
    let form = cursor.u16()?;
    let length = cursor.u32()? as usize;
    if name_len > 255 {
        return Err(invalid("invalid NTFS compact attribute name length"));
    }
    let name = cursor
        .take(name_len * 2)?
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    let value = match form {
        0 | 1 => {
            if length > MAX_RESIDENT || (form == 0) != keep_resident(ty) {
                return Err(invalid("invalid NTFS compact resident value"));
            }
            let mut value = Vec::new();
            value
                .try_reserve_exact(length)
                .map_err(|_| capacity_error("cannot restore NTFS compact resident value"))?;
            if form == 0 {
                value.extend_from_slice(cursor.take(length)?);
            } else {
                value.resize(length, 0);
            }
            Value::Resident(value)
        }
        2 | 3 => {
            let lowest_vcn = cursor.u64()?;
            let highest_vcn = cursor.u64()?;
            let allocated = cursor.u64()?;
            let data_size = cursor.u64()?;
            let initialized = cursor.u64()?;
            let compressed = cursor.u64()?;
            let bytes = length
                .checked_mul(24)
                .ok_or_else(|| invalid("NTFS compact run length overflows"))?;
            let mut data = Cursor {
                data: cursor.take(bytes)?,
                offset: 0,
            };
            let mut runs = Vec::new();
            runs.try_reserve_exact(length)
                .map_err(|_| capacity_error("cannot restore NTFS compact runs"))?;
            for _ in 0..length {
                let vcn = data.u64()?;
                let len = data.u64()?;
                let lcn = data.u64()?;
                runs.push(Run {
                    vcn,
                    len,
                    lcn: (lcn != u64::MAX).then_some(lcn),
                });
            }
            Value::NonResident(NonResident {
                lowest_vcn,
                highest_vcn,
                allocated,
                data_size,
                initialized,
                compressed_size: (form == 3).then_some(compressed),
                runs,
            })
        }
        _ => return Err(invalid("invalid NTFS compact attribute form")),
    };
    Ok(Attribute {
        ty,
        id,
        name,
        flags,
        value,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resident(ty: u32, id: u16, bytes: &[u8]) -> Attribute {
        Attribute {
            ty,
            id,
            flags: 0,
            name: Vec::new(),
            value: Value::Resident(bytes.to_vec()),
        }
    }

    fn nonresident(ty: u32, id: u16) -> Attribute {
        Attribute {
            ty,
            id,
            flags: 0x8001,
            name: vec![0xd800, 0x20, 0x4e2d],
            value: Value::NonResident(NonResident {
                lowest_vcn: 7,
                highest_vcn: 11,
                allocated: 8192,
                data_size: 10000,
                initialized: 9999,
                compressed_size: Some(4096),
                runs: vec![
                    Run {
                        vcn: 7,
                        len: 3,
                        lcn: None,
                    },
                    Run {
                        vcn: 10,
                        len: 2,
                        lcn: Some(40),
                    },
                ],
            }),
        }
    }

    #[test]
    fn preserves_headers_payloads_runs_and_full_references() {
        let reference = 63 | (8u64 << 48);
        let original = RawRecord {
            sequence: 8,
            base_reference: 29 | (11u64 << 48),
            flags: 3,
            attrs: vec![
                resident(0x10, 0, &[7; 48]),
                resident(0x20, 1, &[8; 64]),
                resident(0x30, 2, &[9; 66]),
                resident(0xc0, 3, &[10; 8]),
                resident(0x80, 4, b"file contents are not scanned"),
                resident(0x90, 5, &[11; 32]),
                nonresident(0x80, 6),
                nonresident(0xe0, 7),
            ],
        };
        let mut store = Store::default();
        store.push(reference, &original).unwrap();
        assert!(store.get(reference).is_err());
        store.sort().unwrap();
        let restored = store.get(reference).unwrap().unwrap();
        assert_eq!(restored.sequence, original.sequence);
        assert_eq!(restored.base_reference, original.base_reference);
        assert_eq!(restored.flags, original.flags);
        assert_eq!(restored.attrs.len(), original.attrs.len());
        for (before, after) in original.attrs.iter().zip(&restored.attrs) {
            let keep_content = keep_resident(before.ty);
            assert_eq!(
                (before.ty, before.id, before.flags),
                (after.ty, after.id, after.flags)
            );
            assert_eq!(before.name, after.name);
            match (&before.value, &after.value) {
                (Value::Resident(before), Value::Resident(after)) => {
                    assert_eq!(before.len(), after.len());
                    if keep_content {
                        assert_eq!(before, after);
                    } else {
                        assert!(after.iter().all(|byte| *byte == 0));
                    }
                }
                (Value::NonResident(before), Value::NonResident(after)) => {
                    assert_eq!(before.lowest_vcn, after.lowest_vcn);
                    assert_eq!(before.highest_vcn, after.highest_vcn);
                    assert_eq!(before.allocated, after.allocated);
                    assert_eq!(before.data_size, after.data_size);
                    assert_eq!(before.initialized, after.initialized);
                    assert_eq!(before.compressed_size, after.compressed_size);
                    assert_eq!(before.runs, after.runs);
                }
                _ => panic!("属性驻留形式变化"),
            }
        }
        assert!(store.get(63 | (7u64 << 48)).unwrap().is_none());
        assert!(store.get(64 | (8u64 << 48)).unwrap().is_none());
    }

    #[test]
    fn data_contents_do_not_increase_storage_or_change_lengths() {
        let make = |length| RawRecord {
            sequence: 1,
            flags: 1,
            attrs: vec![resident(0x80, 0, &vec![0xff; length])],
            ..Default::default()
        };
        let mut small = Store::default();
        let mut large = Store::default();
        small.push(1u64 << 48, &make(1)).unwrap();
        large.push(1u64 << 48, &make(60_000)).unwrap();
        assert_eq!(small.arena.len(), 16);
        assert_eq!(small.arena.len(), large.arena.len());
        large.sort().unwrap();
        let result = large.get(1u64 << 48).unwrap().unwrap();
        let Value::Resident(value) = &result.attrs[0].value else {
            panic!()
        };
        assert_eq!(value.len(), 60_000);
        assert!(value.iter().all(|byte| *byte == 0));
    }

    #[test]
    fn sorts_references_rejects_duplicates_and_keeps_failed_push_atomic() {
        let record = RawRecord {
            sequence: 2,
            flags: 1,
            ..Default::default()
        };
        let mut store = Store::default();
        store.push(9 | (2u64 << 48), &record).unwrap();
        store.push(3 | (2u64 << 48), &record).unwrap();
        store.sort().unwrap();
        assert_eq!(store.len(), 2);
        assert_eq!(store.reference_at(0), Some(3 | (2u64 << 48)));
        assert_eq!(store.reference_at(1), Some(9 | (2u64 << 48)));
        assert_eq!(store.reference_at(2), None);
        assert!(store.push(5 | (3u64 << 48), &record).is_err());
        assert_eq!(store.len(), 2);
        assert!(store.get(9 | (2u64 << 48)).unwrap().is_some());
        store.push(9 | (2u64 << 48), &record).unwrap();
        assert!(store.sort().is_err());
    }

    #[test]
    fn truncated_and_mutated_compact_bytes_never_escape_bounds() {
        let reference = 1u64 << 48;
        let record = RawRecord {
            sequence: 1,
            flags: 1,
            attrs: vec![resident(0x10, 0, &[0; 48]), nonresident(0x80, 1)],
            ..Default::default()
        };
        let mut store = Store::default();
        store.push(reference, &record).unwrap();
        store.sort().unwrap();
        let original = store.arena.clone();
        for end in 0..original.len() {
            store.arena = original[..end].to_vec();
            assert!(store.get(reference).is_err());
        }
        for position in 0..original.len() {
            store.arena.clone_from(&original);
            store.arena[position] ^= 0xff;
            let _ = store.get(reference);
        }
        store.arena = original;
        assert!(store.get(reference).unwrap().is_some());
    }

    #[test]
    fn directory_is_32_bytes_and_capacity_slack_is_bounded() {
        assert_eq!(size_of::<Record>(), 32);
        let mut store = Store::default();
        let record = RawRecord {
            sequence: 1,
            flags: 1,
            attrs: vec![resident(0x80, 0, &[0; 700])],
            ..Default::default()
        };
        for id in 0..3000 {
            store.push(id | (1u64 << 48), &record).unwrap();
        }
        assert!(store.records.capacity() - store.records.len() < RECORD_CHUNK);
        assert!(store.arena.capacity() - store.arena.len() < ARENA_CHUNK);
        assert!(store.heap_bytes() < 3000 * 48 + RECORD_CHUNK * 32 + ARENA_CHUNK);
    }

    #[test]
    fn resolved_groups_are_not_limited_to_u16_attribute_counts() {
        let record = RawRecord {
            sequence: 1,
            flags: 1,
            attrs: vec![resident(0x80, 0, &[]); 65536],
            ..Default::default()
        };
        let mut store = Store::default();
        store.push(1u64 << 48, &record).unwrap();
        store.sort().unwrap();
        assert_eq!(store.get(1u64 << 48).unwrap().unwrap().attrs.len(), 65536);
        assert_eq!(size_of::<Record>(), 32);
    }
}
