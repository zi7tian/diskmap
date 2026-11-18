//! 顺序收集 MFT 扩展记录，随后组装；卷外元数据按位置合并读取。

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use crate::classify::Classifier;

use super::compact::Store;
use super::metadata::Cache;
use super::parse::{self, Attribute, Boot, ListEntry, RawRecord, Run, Value};
use super::pipeline::{self, ReadRange, Window};
use super::source::{ReadAt, ReadKind};
use super::tree::{EntryMeta, Name, Table};
use super::{Anomalies, ScanResult, Timings, check_cancel, invalid};

const ID_MASK: u64 = 0x0000_ffff_ffff_ffff;
const BLOCK: usize = 4 * 1024 * 1024;
/// 属性列表和位图有单独的大小限制，避免损坏的长度字段触发巨量分配。
const MAX_METADATA: u64 = 64 * 1024 * 1024;
const MAX_ATTRIBUTE_LIST: u64 = 256 * 1024;
const MAX_EXTENSIONS: usize = 8192;
const MAX_RESOLVED_BYTES: usize = 16 * 1024 * 1024;
/// 每一批只保留最多 4 MiB 的外置元数据，不随全卷记录数积累原始内容。
const METADATA_BATCH: usize = 4 * 1024 * 1024;

/// 保留真正的底层 I/O 失败来源，不能和单条记录的格式错误混淆。
#[derive(Debug)]
struct ReadFailure(io::Error);
impl std::fmt::Display for ReadFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
impl std::error::Error for ReadFailure {}
fn read_failure(error: io::Error) -> io::Error {
    io::Error::new(error.kind(), ReadFailure(error))
}
fn fatal(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::Interrupted | io::ErrorKind::OutOfMemory
    ) || error
        .get_ref()
        .is_some_and(|cause| cause.is::<ReadFailure>())
}

/// 只描述流的映射，不读取普通文件的内容。
struct Stream {
    size: u64,
    initialized: u64,
    allocated: u64,
    flags: u16,
    runs: Vec<Run>,
    resident: Option<Vec<u8>>,
}

impl Stream {
    fn from_attrs(
        attrs: &[&Attribute],
        boot: &Boot,
        complete: bool,
        bad_clusters: bool,
    ) -> io::Result<Self> {
        Self::assemble(attrs, boot, complete, bad_clusters, true)
    }

    /// 计算文件大小只验证映射，不复制驻留内容或运行列表；元数据读取才保留映射。
    fn assemble(
        attrs: &[&Attribute],
        boot: &Boot,
        complete: bool,
        bad_clusters: bool,
        keep_mapping: bool,
    ) -> io::Result<Self> {
        let Some(first) = attrs.first() else {
            return Err(invalid("missing NTFS stream"));
        };
        if let Value::Resident(value) = &first.value {
            if attrs.len() != 1 {
                return Err(invalid("duplicate resident NTFS stream"));
            }
            return Ok(Self {
                size: value.len() as u64,
                initialized: value.len() as u64,
                // 与 Windows 目录信息的 AllocationSize 一致；不是额外分配簇的大小。
                allocated: (value.len() as u64).div_ceil(8) * 8,
                flags: first.flags,
                runs: Vec::new(),
                resident: keep_mapping.then(|| value.clone()),
            });
        }
        let flags = attrs.iter().fold(0, |flags, attr| flags | attr.flags);
        let single;
        let mut sorted = Vec::new();
        let pieces = if attrs.len() == 1 {
            let Value::NonResident(value) = &first.value else {
                return Err(invalid("invalid NTFS stream form"));
            };
            single = [value];
            &single[..]
        } else {
            sorted.reserve(attrs.len());
            for attr in attrs {
                let Value::NonResident(value) = &attr.value else {
                    return Err(invalid("mixed resident and nonresident NTFS stream"));
                };
                sorted.push(value);
            }
            sorted.sort_unstable_by_key(|p| p.lowest_vcn);
            &sorted
        };
        let head = pieces[0];
        if head.lowest_vcn != 0 || head.data_size > i64::MAX as u64 {
            return Err(invalid("missing or invalid first NTFS stream extent"));
        }
        let mut runs = Vec::new();
        let mut next = 0u64;
        let mut physical = 0u64;
        for p in pieces {
            if p.lowest_vcn < next || (complete && p.lowest_vcn != next) {
                return Err(invalid("overlapping or missing NTFS stream extent"));
            }
            for run in &p.runs {
                if let Some(lcn) = run.lcn {
                    let end = lcn
                        .checked_add(run.len)
                        .and_then(|v| v.checked_mul(boot.cluster_size as u64))
                        .ok_or_else(|| invalid("NTFS physical extent overflows"))?;
                    if end > boot.volume_size {
                        return Err(invalid("NTFS extent lies outside volume"));
                    }
                    physical = physical
                        .checked_add(run.len)
                        .ok_or_else(|| invalid("NTFS allocated clusters overflow"))?;
                } else if flags & 0x8001 == 0 && !bad_clusters {
                    return Err(invalid("sparse run in ordinary NTFS stream"));
                }
                if keep_mapping {
                    runs.push(*run);
                }
            }
            next = if p.runs.is_empty() {
                0
            } else {
                p.highest_vcn
                    .checked_add(1)
                    .ok_or_else(|| invalid("NTFS VCN overflows"))?
            };
        }
        let covered = next
            .checked_mul(boot.cluster_size as u64)
            .ok_or_else(|| invalid("NTFS stream size overflows"))?;
        let mut allocated = head.compressed_size.unwrap_or(head.allocated);
        let physical = physical
            .checked_mul(boot.cluster_size as u64)
            .ok_or_else(|| invalid("NTFS allocation overflows"))?;
        if flags & 0x8001 != 0 && head.compressed_size.is_none() {
            allocated = physical;
        }
        if bad_clusters {
            // $BadClus:$Bad 是整个卷的坏簇映射，历史 NTFS 可没有 SPARSE 标志。
            // 它的 AllocatedLength 仍是全卷逻辑大小，实际只计非空洞的坏簇。
            let volume_clusters = boot.volume_size / boot.cluster_size as u64;
            if covered != volume_clusters * boot.cluster_size as u64
                || head.data_size != covered
                || head.allocated != covered
            {
                return Err(invalid("invalid NTFS bad-cluster stream size"));
            }
            allocated = physical;
        }
        if complete && (head.data_size > covered || allocated != physical) {
            return Err(invalid(
                "NTFS stream size or allocation disagrees with runlist",
            ));
        }
        Ok(Self {
            size: head.data_size,
            initialized: head.initialized,
            allocated,
            flags,
            runs,
            resident: None,
        })
    }

    fn select(
        attrs: &[Attribute],
        ty: u32,
        name: &[u16],
        boot: &Boot,
        complete: bool,
    ) -> io::Result<Self> {
        let group: Vec<_> = attrs
            .iter()
            .filter(|a| a.ty == ty && a.name == name)
            .collect();
        Self::from_attrs(&group, boot, complete, false)
    }

    /// 元数据禁止压缩/加密/空洞；不尝试对损坏或不支持的布局做猜测。
    fn read(
        &self,
        source: &mut dyn ReadAt,
        boot: &Boot,
        offset: u64,
        out: &mut [u8],
        kind: ReadKind,
    ) -> io::Result<()> {
        if self.flags & 0xc001 != 0 {
            return Err(invalid(
                "compressed, sparse or encrypted NTFS metadata stream",
            ));
        }
        let end = offset
            .checked_add(out.len() as u64)
            .filter(|end| *end <= self.size && *end <= self.initialized)
            .ok_or_else(|| invalid("NTFS metadata read exceeds initialized stream"))?;
        if let Some(data) = &self.resident {
            out.copy_from_slice(&data[offset as usize..end as usize]);
            return Ok(());
        }
        let cluster = boot.cluster_size as u64;
        let mut position = offset;
        let mut remaining = out;
        while !remaining.is_empty() {
            let vcn = position / cluster;
            let i = self.runs.partition_point(|r| r.vcn <= vcn);
            let run = i
                .checked_sub(1)
                .and_then(|i| self.runs.get(i))
                .filter(|r| vcn < r.vcn + r.len)
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::NotFound, "unmapped NTFS metadata extent")
                })?;
            let lcn = run
                .lcn
                .ok_or_else(|| invalid("sparse NTFS metadata extent"))?;
            let within = position - run.vcn * cluster;
            let length = ((run.len * cluster - within).min(remaining.len() as u64)) as usize;
            source
                .read_kind(kind, lcn * cluster + within, &mut remaining[..length])
                .map_err(read_failure)?;
            position += length as u64;
            remaining = &mut remaining[length..];
        }
        Ok(())
    }

    fn metadata(
        &self,
        source: &mut dyn ReadAt,
        boot: &Boot,
        cancel: &AtomicBool,
        kind: ReadKind,
    ) -> io::Result<Vec<u8>> {
        if self.size > MAX_METADATA {
            return Err(invalid("NTFS metadata stream exceeds 64 MiB limit"));
        }
        let mut data = vec![0; self.size as usize];
        for (i, block) in data.chunks_mut(BLOCK).enumerate() {
            check_cancel(cancel)?;
            self.read(source, boot, (i * BLOCK) as u64, block, kind)?;
        }
        Ok(data)
    }

    /// 与 read 相同的映射验证，但只生成物理读取计划，不访问设备。
    fn ranges(&self, boot: &Boot, offset: u64, length: usize) -> io::Result<Vec<ReadRange>> {
        if self.flags & 0xc001 != 0 {
            return Err(invalid(
                "compressed, sparse or encrypted NTFS metadata stream",
            ));
        }
        offset
            .checked_add(length as u64)
            .filter(|end| *end <= self.size && *end <= self.initialized)
            .ok_or_else(|| invalid("NTFS metadata read exceeds initialized stream"))?;
        if self.resident.is_some() {
            return Ok(Vec::new());
        }
        let cluster = boot.cluster_size as u64;
        let mut position = offset;
        let mut remaining = length;
        let mut ranges = Vec::new();
        while remaining != 0 {
            let vcn = position / cluster;
            let i = self.runs.partition_point(|r| r.vcn <= vcn);
            let run = i
                .checked_sub(1)
                .and_then(|i| self.runs.get(i))
                .filter(|r| vcn < r.vcn + r.len)
                .ok_or_else(|| invalid("unmapped NTFS metadata extent"))?;
            let lcn = run
                .lcn
                .ok_or_else(|| invalid("sparse NTFS metadata extent"))?;
            let within = position - run.vcn * cluster;
            let len = (run.len * cluster - within).min(remaining as u64) as usize;
            ranges.push(ReadRange {
                offset: lcn * cluster + within,
                len,
            });
            position += len as u64;
            remaining -= len;
        }
        Ok(ranges)
    }

    fn cached(&self, cache: &Cache, boot: &Boot, cancel: &AtomicBool) -> io::Result<Vec<u8>> {
        if self.size > MAX_METADATA {
            return Err(invalid("NTFS metadata stream exceeds 64 MiB limit"));
        }
        let ranges = self.ranges(boot, 0, self.size as usize)?;
        if let Some(data) = &self.resident {
            return Ok(data.clone());
        }
        let mut out = vec![0; self.size as usize];
        let mut at = 0;
        for range in ranges {
            check_cancel(cancel)?;
            cache.copy(range.offset, &mut out[at..at + range.len])?;
            at += range.len;
        }
        Ok(out)
    }
}

struct Mft<'a> {
    source: &'a mut dyn ReadAt,
    boot: Boot,
    data: Stream,
    bitmap: Vec<u8>,
    cancel: &'a AtomicBool,
}

impl Mft<'_> {
    fn record(&mut self, reference: u64, collected: Option<&Store>) -> io::Result<RawRecord> {
        check_cancel(self.cancel)?;
        let id = reference & ID_MASK;
        if !self.bitmap.is_empty() && !active(&self.bitmap, id) {
            return Err(invalid("NTFS attribute list references a free record"));
        }
        let record = if let Some(collected) = collected {
            collected
                .get(reference)?
                .ok_or_else(|| invalid("NTFS extension was not collected or has stale sequence"))?
        } else {
            // 只有 MFT 引导/位图准备允许定位读取：映射未齐之前尚不能开始全表顺序读。
            let offset = id
                .checked_mul(self.boot.record_size as u64)
                .ok_or_else(|| invalid("NTFS record offset overflows"))?;
            let mut data = vec![0; self.boot.record_size as usize];
            self.data.read(
                self.source,
                &self.boot,
                offset,
                &mut data,
                ReadKind::Extension,
            )?;
            RawRecord::parse(&mut data, self.boot.sector_size)?
        };
        if record.sequence != (reference >> 48) as u16 || record.flags & 1 == 0 {
            return Err(invalid("stale NTFS extension record reference"));
        }
        Ok(record)
    }

    fn attribute_list(
        &mut self,
        attrs: &[Attribute],
        cache: Option<&Cache>,
    ) -> io::Result<Vec<ListEntry>> {
        if !attrs.iter().any(|a| a.ty == 0x20) {
            return Ok(Vec::new());
        }
        let stream = Stream::select(attrs, 0x20, &[], &self.boot, true)?;
        if stream.size > MAX_ATTRIBUTE_LIST {
            return Err(invalid("NTFS attribute list exceeds 256 KiB limit"));
        }
        let data = if let Some(cache) = cache {
            stream.cached(cache, &self.boot, self.cancel)?
        } else {
            stream.metadata(
                self.source,
                &self.boot,
                self.cancel,
                ReadKind::AttributeList,
            )?
        };
        parse::parse_attribute_list(&data)
    }

    /// 验证属性列表中的每个引用。MFT 自身分片时，已知映射逐步扩大，不能凭物理连续猜位置。
    fn resolve(
        &mut self,
        reference: u64,
        mut attrs: Vec<Attribute>,
        bootstrap: bool,
        collected: Option<&Store>,
        cache: Option<&Cache>,
    ) -> io::Result<Vec<Attribute>> {
        // 基础记录已含完整映射时，无须依赖 ATTRIBUTE_LIST 才能读取 MFT。
        // 其余属性损坏由后面的普通记录处理计数，不能提前阻止整卷扫描。
        if bootstrap && Stream::select(&attrs, 0x80, &[], &self.boot, true).is_ok() {
            return Ok(attrs);
        }
        let mut list = self.attribute_list(&attrs, cache)?;
        if list.is_empty() {
            if attrs.iter().any(|a| a.ty == 0x20) {
                return Err(invalid("empty NTFS attribute list"));
            }
            return Ok(attrs);
        }
        if bootstrap {
            // 引导只需要 unnamed DATA。BITMAP 等其他流的失效引用不影响数据映射。
            list.retain(|entry| entry.ty == 0x80 && entry.name.is_empty());
        }
        let mut resolved_bytes = attribute_bytes(&attrs);
        let mut pending = BTreeMap::<u64, Vec<&ListEntry>>::new();
        let mut keys = BTreeSet::new();
        for entry in &list {
            if !keys.insert((entry.reference, entry.id)) {
                return Err(invalid("duplicate NTFS attribute list reference"));
            }
            if entry.reference == reference {
                find_attribute(&attrs, entry)?;
            } else {
                if entry.reference & ID_MASK == reference & ID_MASK {
                    return Err(invalid("stale NTFS base record reference"));
                }
                pending.entry(entry.reference).or_default().push(entry);
            }
        }
        validate_list_completeness(&attrs, reference, &list, bootstrap)?;
        if pending.len() > MAX_EXTENSIONS {
            return Err(invalid(
                "NTFS attribute list has too many extension records",
            ));
        }
        while !pending.is_empty() {
            check_cancel(self.cancel)?;
            let references: Vec<_> = pending.keys().copied().collect();
            let mut advanced = false;
            for extension in references {
                let record = match self.record(extension, collected) {
                    Err(e) if bootstrap && e.kind() == io::ErrorKind::NotFound && !fatal(&e) => {
                        continue;
                    }
                    result => result?,
                };
                if record.base_reference != reference {
                    return Err(invalid("NTFS extension belongs to a different base record"));
                }
                validate_list_completeness(&record.attrs, extension, &list, bootstrap)?;
                resolved_bytes = resolved_bytes
                    .checked_add(attribute_bytes(&record.attrs))
                    .filter(|n| *n <= MAX_RESOLVED_BYTES)
                    .ok_or_else(|| invalid("NTFS resolved attributes exceed 16 MiB limit"))?;
                // pending 由上面这份键列表得到，不可能在其他线程改变。
                if let Some(entries) = pending.remove(&extension) {
                    for entry in entries {
                        if entry.ty == 0x20 {
                            return Err(invalid("recursive NTFS attribute list"));
                        }
                        attrs.push(find_attribute(&record.attrs, entry)?.clone());
                    }
                }
                if bootstrap {
                    self.data = Stream::select(&attrs, 0x80, &[], &self.boot, false)?;
                    if Stream::select(&attrs, 0x80, &[], &self.boot, true).is_ok() {
                        return Ok(attrs);
                    }
                }
                advanced = true;
            }
            if !advanced {
                return Err(invalid("NTFS MFT bootstrap cannot reach extension records"));
            }
        }
        Ok(attrs)
    }
}

/// 列表必须完整覆盖所有记录的属性，不能把损坏或读取中变化的列表静默当成完整结果。
fn validate_list_completeness(
    attrs: &[Attribute],
    reference: u64,
    list: &[ListEntry],
    data_only: bool,
) -> io::Result<()> {
    for attr in attrs
        .iter()
        .filter(|a| a.ty != 0x20 && (!data_only || (a.ty == 0x80 && a.name.is_empty())))
    {
        if !list
            .iter()
            .any(|entry| entry.reference == reference && entry.id == attr.id)
        {
            return Err(invalid("NTFS attribute list omits a record attribute"));
        }
    }
    Ok(())
}

fn attribute_bytes(attrs: &[Attribute]) -> usize {
    attrs
        .iter()
        .map(|a| {
            std::mem::size_of::<Attribute>()
                + a.name.len() * 2
                + match &a.value {
                    Value::Resident(v) => v.len(),
                    Value::NonResident(n) => n.runs.len() * std::mem::size_of::<Run>(),
                }
        })
        .sum()
}

fn find_attribute<'a>(attrs: &'a [Attribute], entry: &ListEntry) -> io::Result<&'a Attribute> {
    let mut matches = attrs.iter().filter(|a| {
        a.ty == entry.ty
            && a.id == entry.id
            && a.name == entry.name
            && match &a.value {
                Value::Resident(_) => entry.lowest_vcn == 0,
                Value::NonResident(n) => n.lowest_vcn == entry.lowest_vcn,
            }
    });
    let attr = matches
        .next()
        .ok_or_else(|| invalid("NTFS attribute list target is missing"))?;
    if matches.next().is_some() {
        return Err(invalid("NTFS attribute instance is not unique"));
    }
    Ok(attr)
}

fn active(bitmap: &[u8], id: u64) -> bool {
    bitmap
        .get((id / 8) as usize)
        .is_some_and(|v| v & (1 << (id % 8)) != 0)
}

pub(super) fn scan(
    source: &mut dyn ReadAt,
    root: &Path,
    apparent: bool,
    classifier: &Classifier,
    cancel: &AtomicBool,
    mut progress: impl FnMut(u64, u64),
) -> io::Result<ScanResult> {
    check_cancel(cancel)?;
    let started = Instant::now();
    let mut measured = super::source::Measured {
        source,
        elapsed: Default::default(),
        bytes: 0,
        calls: 0,
        stats: Default::default(),
    };
    let source: &mut dyn ReadAt = &mut measured;
    let mut sector = [0u8; 512];
    source.read_exact_at(0, &mut sector)?;
    let boot = Boot::parse(&sector)?;
    let mut first = vec![0; boot.record_size as usize];
    source.read_exact_at(boot.mft_offset, &mut first)?;
    let first = RawRecord::parse(&mut first, boot.sector_size)?;
    if first.base_reference != 0 || first.flags & 3 != 1 {
        return Err(invalid("invalid NTFS MFT base record"));
    }
    let reference = (first.sequence as u64) << 48;
    let data = Stream::select(&first.attrs, 0x80, &[], &boot, false)?;
    let mut mft = Mft {
        source,
        boot,
        data,
        bitmap: Vec::new(),
        cancel,
    };
    let attrs = mft.resolve(reference, first.attrs.clone(), true, None, None)?;
    mft.data = Stream::select(&attrs, 0x80, &[], &boot, true)?;
    if mft.data.resident.is_some()
        || !mft.data.size.is_multiple_of(boot.record_size as u64)
        || mft.data.size > boot.volume_size
        || mft.data.initialized != mft.data.size
    {
        return Err(invalid("invalid NTFS MFT stream length"));
    }
    if mft.data.runs.first().is_none_or(|run| {
        run.vcn != 0
            || run
                .lcn
                .and_then(|lcn| lcn.checked_mul(boot.cluster_size as u64))
                != Some(boot.mft_offset)
    }) {
        return Err(invalid("NTFS MFT mapping disagrees with boot sector"));
    }
    let count = mft.data.size / boot.record_size as u64;
    if !(16..=u32::MAX as u64).contains(&count) {
        return Err(invalid("NTFS MFT record count outside supported range"));
    }
    let mut anomalies = Anomalies::default();
    // DATA 映射确定之后才解析其他 MFT 元数据。无关扩展损坏时退化为逐条读 FILE，
    // 不能因为读取 BITMAP 的准备工作失败而绕过下面的位图降级。
    let attrs = match mft.resolve(reference, first.attrs, false, None, None) {
        Ok(attrs) => Some(attrs),
        Err(error) if fatal(&error) => return Err(error),
        Err(_) => {
            anomalies.note(0);
            None
        }
    };
    // 位图损坏不妨碍读 MFT 数据本身：改为检查每条 FILE 的 in-use 标志。
    let bitmap = attrs
        .as_deref()
        .ok_or_else(|| invalid("NTFS MFT metadata cannot be resolved"))
        .and_then(|attrs| Stream::select(attrs, 0xb0, &[], &boot, true))
        .and_then(|bitmap| bitmap.metadata(mft.source, &boot, cancel, ReadKind::Other));
    match bitmap {
        Ok(bitmap)
            if bitmap.len() as u64 * 8 >= count && active(&bitmap, 0) && active(&bitmap, 5) =>
        {
            mft.bitmap = bitmap
        }
        Err(error) if fatal(&error) => return Err(error),
        _ => anomalies.note(0),
    }
    let mut table = Table::new();
    if !mft.bitmap.is_empty() {
        // 有效位图给出记录上界，避免 Vec 倍增把百万级暂存表扩成接近两倍。
        // 只数声明的 MFT 槽位；位图损坏降级时不按可能很大的空槽总量预分配。
        let whole = (count / 8) as usize;
        let mut allocated: usize = mft.bitmap[..whole]
            .iter()
            .map(|v| v.count_ones() as usize)
            .sum();
        if !count.is_multiple_of(8) {
            allocated += (mft.bitmap[whole] & ((1u8 << (count % 8)) - 1)).count_ones() as usize;
        }
        table.reserve_records(allocated)?;
    }
    let mut raw = vec![0; boot.record_size as usize];
    let mut record = RawRecord::default();
    let mut names = Vec::new();
    let mut extensions = Store::default();
    let mut deferred = Store::default();
    let records_per_block = BLOCK / boot.record_size as usize;
    let mut processed = 0u64;
    let empty_cache = Cache::default();
    let mut context = RecordContext {
        boot,
        cancel,
        cache: &empty_cache,
    };
    let bitmap = &mft.bitmap;
    let data = &mft.data;
    let windows = (0..count).step_by(records_per_block).filter_map(|start| {
        // 连续大量全空窗口也必须响应取消，不能等 filter_map 找到下一块活记录。
        if let Err(error) = check_cancel(cancel) {
            return Some(Err(error));
        }
        let n = (count - start).min(records_per_block as u64) as usize;
        if !(start..start + n as u64).any(|id| bitmap.is_empty() || active(bitmap, id)) {
            return None;
        }
        let offset = start * boot.record_size as u64;
        let len = n * boot.record_size as usize;
        Some(data.ranges(&boot, offset, len).map(|ranges| Window {
            offset,
            len,
            ranges,
        }))
    });
    let pipeline = pipeline::run(mft.source, windows, BLOCK, cancel, |window, bytes| {
        let start = window.offset / boot.record_size as u64;
        for (i, bytes) in bytes.chunks_exact(boot.record_size as usize).enumerate() {
            if i.is_multiple_of(256) {
                check_cancel(cancel)?;
            }
            let id = start + i as u64;
            if !bitmap.is_empty() && !active(bitmap, id) {
                continue;
            }
            raw.copy_from_slice(bytes);
            // 位图损坏降级时，空槽和已删除记录不解释过期属性。
            if bitmap.is_empty()
                && (raw.iter().all(|b| *b == 0)
                    || (raw.get(..4) == Some(b"FILE")
                        && raw.get(22).is_some_and(|flags| flags & 1 == 0)))
            {
                continue;
            }
            processed += 1;
            if record.parse_reuse(&mut raw, boot.sector_size).is_err() || record.flags & 1 == 0 {
                anomalies.note(id);
                continue;
            }
            let reference = id | ((record.sequence as u64) << 48);
            let result =
                if record.base_reference != 0 {
                    extensions.push(reference, &record)
                } else if record.attrs.iter().any(|a| {
                    a.ty == 0x20 || (a.ty == 0xc0 && matches!(a.value, Value::NonResident(_)))
                }) {
                    deferred.push(reference, &record)
                } else {
                    add_record(
                        &mut table,
                        &mut context,
                        reference,
                        record.flags & 2 != 0,
                        &record.attrs,
                        &mut names,
                    )
                };
            if let Err(error) = result {
                if fatal(&error) {
                    return Err(error);
                }
                anomalies.note(id);
            }
        }
        progress(processed, anomalies.count());
        Ok(())
    })?;
    // 缓冲是两个线程间转移所有权的两个 Vec；没有复制或缓存整份 MFT。
    let extension_records = extensions.len() as u64;
    let deferred_records = deferred.len() as u64;
    let collection_bytes = extensions.heap_bytes() + deferred.heap_bytes();
    let mut temporary_bytes = table.heap_bytes()
        + collection_bytes
        + 2 * BLOCK
        + mft.bitmap.capacity()
        + raw.capacity()
        + mft.data.runs.capacity() * std::mem::size_of::<Run>();
    drop(raw);
    drop(record);
    extensions.sort()?;
    deferred.sort()?;
    table.begin_deferred();
    let mut reparses = Store::default();
    let pending = plan_records(&deferred, 0x20, &boot, cancel, &mut anomalies)?;
    for batch in batches(&pending) {
        check_cancel(cancel)?;
        let cache = read_metadata_batch(mft.source, &deferred, batch, 0x20, &boot, cancel)?;
        for pending in batch {
            check_cancel(cancel)?;
            let base = get_record(&deferred, pending.reference)?;
            let result = mft
                .resolve(
                    pending.reference,
                    base.attrs,
                    false,
                    Some(&extensions),
                    Some(&cache),
                )
                .and_then(|attrs| {
                    if external_reparse(&attrs) {
                        // 与原路径一样，SI/名字/流映射等前置错误必须先局部拒绝；
                        // 不能为本应跳过的记录读盘并把它升级成整卷 I/O 失败。
                        match add_record(
                            &mut table,
                            &mut RecordContext {
                                boot,
                                cancel,
                                cache: &empty_cache,
                            },
                            pending.reference,
                            base.flags & 2 != 0,
                            &attrs,
                            &mut names,
                        ) {
                            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                                reparses.push(pending.reference, &RawRecord { attrs, ..base })
                            }
                            result => result,
                        }
                    } else {
                        add_record(
                            &mut table,
                            &mut RecordContext {
                                boot,
                                cancel,
                                cache: &cache,
                            },
                            pending.reference,
                            base.flags & 2 != 0,
                            &attrs,
                            &mut names,
                        )
                    }
                });
            if let Err(error) = result {
                if fatal(&error) {
                    return Err(error);
                }
                anomalies.note(pending.reference);
            }
        }
        temporary_bytes = temporary_bytes.max(
            table.heap_bytes()
                + collection_bytes
                + reparses.heap_bytes()
                + cache.heap_bytes()
                + mft.bitmap.capacity()
                + pending.capacity() * std::mem::size_of::<Pending>()
                + BLOCK,
        );
        progress(processed, anomalies.count());
    }
    // 已完成引用验证；外置重解析数据只需合并后的记录，及时释放基础/扩展收集表。
    drop(pending);
    drop(deferred);
    drop(extensions);
    reparses.sort()?;
    let pending = plan_records(&reparses, 0xc0, &boot, cancel, &mut anomalies)?;
    for batch in batches(&pending) {
        check_cancel(cancel)?;
        let cache = read_metadata_batch(mft.source, &reparses, batch, 0xc0, &boot, cancel)?;
        for pending in batch {
            let record = get_record(&reparses, pending.reference)?;
            let result = add_record(
                &mut table,
                &mut RecordContext {
                    boot,
                    cancel,
                    cache: &cache,
                },
                pending.reference,
                record.flags & 2 != 0,
                &record.attrs,
                &mut names,
            );
            if let Err(error) = result {
                if fatal(&error) {
                    return Err(error);
                }
                anomalies.note(pending.reference);
            }
        }
        temporary_bytes = temporary_bytes.max(
            table.heap_bytes()
                + reparses.heap_bytes()
                + cache.heap_bytes()
                + mft.bitmap.capacity()
                + pending.capacity() * std::mem::size_of::<Pending>()
                + BLOCK,
        );
        progress(processed, anomalies.count());
    }
    check_cancel(cancel)?;
    drop(pending);
    drop(reparses);
    drop(names);
    drop(mft);
    let parse_elapsed = started.elapsed();
    let io_overlap = measured
        .stats
        .sequential
        .elapsed
        .saturating_sub(pipeline.wait);
    let mut timings = Timings {
        io: measured.elapsed.saturating_sub(io_overlap),
        parse: parse_elapsed.saturating_sub(measured.elapsed.saturating_sub(io_overlap)),
        ..Default::default()
    };
    let build_started = Instant::now();
    let index = table.build(
        root,
        classifier,
        apparent,
        cancel,
        &mut anomalies,
        &mut timings,
    )?;
    let build_elapsed = build_started.elapsed();
    // build返回时已销毁暂存表与目录工作区，把清理耗时也归入建树，六阶段互斥且完整。
    timings.tree =
        build_elapsed.saturating_sub(timings.classify + timings.hardlinks + timings.index);
    progress(processed, anomalies.count());
    Ok(ScanResult {
        index,
        records: processed,
        anomalies: anomalies.count(),
        parse_elapsed,
        build_elapsed,
        timings,
        read_bytes: measured.bytes,
        read_calls: measured.calls,
        read_stats: measured.stats,
        io_overlap,
        collection_bytes,
        extension_records,
        deferred_records,
        temporary_bytes,
    })
}

/// 外置 reparse 属性只有在 SI 标记为重解析点时才需要内容；不能因无关属性触发额外 I/O。
fn external_reparse(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|a| a.ty == 0xc0 && matches!(a.value, Value::NonResident(_)))
        && attrs.iter().any(|a| a.ty == 0x10 && matches!(&a.value,
            Value::Resident(v) if parse::parse_standard_information(v).is_ok_and(|flags| flags & 0x400 != 0)))
}

fn get_record(store: &Store, reference: u64) -> io::Result<RawRecord> {
    store
        .get(reference)?
        .ok_or_else(|| invalid("missing deferred NTFS record"))
}

struct Pending {
    reference: u64,
    offset: u64,
    bytes: usize,
}

fn metadata_stream(attrs: &[Attribute], ty: u32, boot: &Boot) -> io::Result<Option<Stream>> {
    if !attrs.iter().any(|a| a.ty == ty) {
        return Ok(None);
    }
    let stream = Stream::select(attrs, ty, &[], boot, true)?;
    let limit = if ty == 0x20 {
        MAX_ATTRIBUTE_LIST
    } else {
        16 * 1024
    };
    if stream.size > limit {
        return Err(invalid("NTFS metadata exceeds per-record limit"));
    }
    Ok(Some(stream))
}

/// 先按每条元数据的首个物理偏移排序，再按有效负载大小分成有界批次。
/// 每批内部所有分片再次按实际物理偏移排序，碎片化流也不会按引用顺序随机读取。
fn plan_records(
    store: &Store,
    ty: u32,
    boot: &Boot,
    cancel: &AtomicBool,
    anomalies: &mut Anomalies,
) -> io::Result<Vec<Pending>> {
    let mut pending = Vec::new();
    pending
        .try_reserve_exact(store.len())
        .map_err(|_| io::Error::from(io::ErrorKind::OutOfMemory))?;
    for i in 0..store.len() {
        check_cancel(cancel)?;
        let reference = store
            .reference_at(i)
            .ok_or_else(|| invalid("missing deferred NTFS reference"))?;
        let record = get_record(store, reference)?;
        let result = (|| -> io::Result<Pending> {
            let Some(stream) = metadata_stream(&record.attrs, ty, boot)? else {
                return Ok(Pending {
                    reference,
                    offset: 0,
                    bytes: 0,
                });
            };
            let ranges = stream.ranges(boot, 0, stream.size as usize)?;
            let bytes = if stream.resident.is_some() {
                0
            } else {
                stream.size as usize
            };
            Ok(Pending {
                reference,
                offset: ranges.iter().map(|r| r.offset).min().unwrap_or(0),
                bytes,
            })
        })();
        match result {
            Ok(item) => pending.push(item),
            Err(error) if fatal(&error) => return Err(error),
            Err(_) => anomalies.note(reference),
        }
    }
    pending.sort_unstable_by_key(|p| (p.offset, p.reference));
    Ok(pending)
}

fn batches(pending: &[Pending]) -> impl Iterator<Item = &[Pending]> {
    let mut remaining = pending;
    std::iter::from_fn(move || {
        if remaining.is_empty() {
            return None;
        }
        let mut count = 0;
        let mut bytes = 0usize;
        while count < remaining.len() && bytes + remaining[count].bytes <= METADATA_BATCH {
            bytes += remaining[count].bytes;
            count += 1;
        }
        // 单条列表最多256KiB，重解析最多16KiB；即使调用方出错也必须推进。
        count = count.max(1);
        let (batch, tail) = remaining.split_at(count);
        remaining = tail;
        Some(batch)
    })
}

fn read_metadata_batch(
    source: &mut dyn ReadAt,
    store: &Store,
    batch: &[Pending],
    ty: u32,
    boot: &Boot,
    cancel: &AtomicBool,
) -> io::Result<Cache> {
    let mut ranges = Vec::new();
    for pending in batch.iter().filter(|p| p.bytes != 0) {
        check_cancel(cancel)?;
        let record = get_record(store, pending.reference)?;
        if let Some(stream) = metadata_stream(&record.attrs, ty, boot)? {
            ranges.extend(stream.ranges(boot, 0, stream.size as usize)?);
        }
    }
    let kind = if ty == 0x20 {
        ReadKind::AttributeList
    } else {
        ReadKind::Reparse
    };
    Cache::read(source, ranges, kind, cancel).map_err(|error| {
        if matches!(
            error.kind(),
            io::ErrorKind::Interrupted | io::ErrorKind::OutOfMemory
        ) {
            error
        } else {
            read_failure(error)
        }
    })
}

struct RecordContext<'a> {
    boot: Boot,
    cancel: &'a AtomicBool,
    cache: &'a Cache,
}

fn add_record(
    table: &mut Table,
    mft: &mut RecordContext<'_>,
    reference: u64,
    directory: bool,
    attrs: &[Attribute],
    names: &mut Vec<parse::FileName>,
) -> io::Result<()> {
    let mut name_count = 0usize;
    let mut attributes = None;
    let mut tag = 0;
    let mut first_data = None;
    let mut data_count = 0usize;
    for attr in attrs {
        match attr.ty {
            0x10 => {
                if attributes.is_some() || !attr.name.is_empty() {
                    return Err(invalid("duplicate or named NTFS standard information"));
                }
                let Value::Resident(value) = &attr.value else {
                    return Err(invalid("nonresident NTFS standard information"));
                };
                attributes = Some(parse::parse_standard_information(value)?);
            }
            0x30 => {
                let Value::Resident(value) = &attr.value else {
                    return Err(invalid("nonresident NTFS file name"));
                };
                if name_count == names.len() {
                    names.push(parse::FileName::default());
                }
                parse::parse_file_name_into(value, &mut names[name_count])?;
                name_count += 1;
            }
            0x80 => {
                first_data.get_or_insert(attr);
                data_count += 1;
            }
            _ => {}
        }
    }
    if name_count == 0 && (12..16).contains(&(reference & ID_MASK)) && !directory && data_count == 0
    {
        return Ok(()); // 无名字、无数据的预留记录不会贡献统计。
    }
    let attributes = attributes.ok_or_else(|| invalid("NTFS standard information is missing"))?;
    // $Extend 下的 $Quota/$ObjId/$Reparse 是只有索引的合法文件；重解析点也可以没有
    // DATA。索引块不计入文件大小，但保留这些零字节条目，不能误报为数据缺失。
    if !directory
        && reference & ID_MASK >= 16
        && data_count == 0
        && attributes & 0x400 == 0
        && !attrs.iter().any(|attr| {
            attr.ty == 0x90 && matches!(&attr.value, Value::Resident(value) if value.len() >= 32)
        })
    {
        return Err(invalid("NTFS file has no DATA attribute"));
    }
    if attributes & 0x400 != 0 {
        let stream = Stream::select(attrs, 0xc0, &[], &mft.boot, true)?;
        if stream.size > 16 * 1024 {
            return Err(invalid("NTFS reparse data exceeds 16 KiB"));
        }
        tag = parse::parse_reparse_tag(&stream.cached(mft.cache, &mft.boot, mft.cancel)?)?;
    }
    let (mut apparent_size, mut allocated_size) = (0u64, 0u64);
    let mut measure = |group: &[&Attribute]| -> io::Result<()> {
        let bad_clusters =
            reference & ID_MASK == 8 && group.first().is_some_and(|a| a.name == [36, 66, 97, 100]);
        let stream = Stream::assemble(group, &mft.boot, true, bad_clusters, false)?;
        apparent_size = apparent_size
            .checked_add(stream.size)
            .ok_or_else(|| invalid("NTFS apparent size overflows"))?;
        allocated_size = allocated_size
            .checked_add(stream.allocated)
            .ok_or_else(|| invalid("NTFS allocated size overflows"))?;
        Ok(())
    };
    if data_count == 1 {
        if let Some(attr) = first_data {
            measure(&[attr])?;
        }
    } else if data_count > 1 {
        let mut data = BTreeMap::<&[u16], Vec<&Attribute>>::new();
        for attr in attrs.iter().filter(|attr| attr.ty == 0x80) {
            data.entry(&attr.name).or_default().push(attr);
        }
        for group in data.values() {
            measure(group)?;
        }
    }
    table.push(
        EntryMeta {
            reference,
            directory,
            attributes,
            reparse_tag: tag,
            apparent_size,
            allocated_size,
        },
        names[..name_count].iter().map(|n| Name {
            parent_reference: n.parent_reference,
            namespace: n.namespace,
            name: &n.name,
        }),
    )
}
