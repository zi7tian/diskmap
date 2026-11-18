//! MFT 的紧凑暂存表和最后一次建树；不读取文件内容，不写入卷。
//!
//! 暂存只保存记录、父引用和集中存放的 UTF-16 名字，不为每个文件保存完整路径。
//! 建树先验证所有父引用，再在本地构造 `Index`；调用方成功后一次替换共享索引。

use std::borrow::Cow;
use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::time::Instant;

use crate::classify::Classifier;
use crate::index::{DirId, FileId, Index, NewDir, NewFile};
use crate::model::Category;
use crate::platform::Os;

use super::{Anomalies, Timings};

const ID_MASK: u64 = 0x0000_ffff_ffff_ffff;
const ROOT_RECORD: u64 = 5;
const EXTEND_RECORD: u64 = 11;
const ATTR_REPARSE: u32 = 0x400;
// SI 中 0x40000 是 FILE_ATTRIBUTE_EA；只有目录枚举输出才把同一位解释为 RECALL_ON_OPEN。
// 直接沿用目录 API 的掩码会把本地 EA 文件（包括大量硬链接）误记为零大小。
// https://learn.microsoft.com/en-us/windows/win32/fileio/file-attribute-constants
const CLOUD_ONLY: u32 = 0x40_0000 | 0x1000;
const TAG_MOUNT_POINT: u32 = 0xa000_0003;
const TAG_SYMLINK: u32 = 0xa000_000c;
const VISITING: u8 = 1;
const DONE: u8 = 2;
const EXTEND: u8 = 4;
const HIDDEN: u8 = 8;
const INVALID: u8 = 16;
const VISIBLE: u8 = 32;
const PUBLISHED: u8 = 64;
const COUNTED: u8 = 128;
const NO_PARENT: u64 = u64::MAX;
/// 防止损坏的极深目录树导致索引祖先累加和路径构造退化为无界的平方工作量。
const MAX_DEPTH: u16 = 1024;

/// 已合并扩展记录、备用数据流后的基础记录。
pub(super) struct EntryMeta {
    pub reference: u64,
    pub directory: bool,
    pub attributes: u32,
    pub reparse_tag: u32,
    pub apparent_size: u64,
    pub allocated_size: u64,
}

/// 一个 `$FILE_NAME`；纯 DOS 名字在进入暂存表之前过滤。
pub(super) struct Name<'a> {
    pub parent_reference: u64,
    pub namespace: u8,
    pub name: &'a [u16],
}

struct Record {
    reference: u64,
    apparent: u64,
    allocated: u64,
    first_name: u32,
    attrs: u32,
    tag: u32,
    name_count: u16,
    directory: bool,
}

impl Record {
    fn id(&self) -> u64 {
        self.reference & ID_MASK
    }

    fn tag(&self) -> u32 {
        if self.attrs & ATTR_REPARSE == 0 {
            0
        } else {
            self.tag
        }
    }

    fn traversable(&self) -> bool {
        self.directory && !matches!(self.tag(), TAG_MOUNT_POINT | TAG_SYMLINK)
    }

    fn size(&self, apparent: bool) -> u64 {
        if apparent {
            self.apparent
        } else if self.attrs & CLOUD_ONLY != 0 {
            0
        } else {
            self.allocated
        }
    }
}

struct Link {
    /// 校验之前是完整的父记录引用；校验之后是暂存表内的父记录编号。
    parent: u64,
    record: u32,
    name_offset: u32,
    name_len: u16,
}

#[derive(Default)]
pub(super) struct Table {
    records: Vec<Record>,
    links: Vec<Link>,
    names: Vec<u16>,
    deferred: bool,
}

#[derive(Clone, Default)]
struct Resolved {
    parent: u32,
    depth: u16,
    path_units: u16,
    state: u8,
}

impl Table {
    pub(super) fn new() -> Self {
        Self::default()
    }

    /// 位图给出有效记录上界时精确预留，避免跨过容量门槛后把整个记录表翻倍。
    /// 名字和硬链接数量无法从位图推断，仍由各自的实际数据驱动增长。
    pub(super) fn reserve_records(&mut self, records: usize) -> io::Result<()> {
        if records > u32::MAX as usize {
            return Err(capacity());
        }
        self.records
            .try_reserve_exact(records.saturating_sub(self.records.len()))
            .map_err(|_| capacity())
    }

    /// 顺序扫描阶段要求记录号递增；延迟组装阶段显式开启后允许追加更小的基础记录号。
    pub(super) fn begin_deferred(&mut self) {
        self.deferred = true;
    }

    /// 正常记录按 MFT 顺序解析；带属性列表的记录可在 begin_deferred 后完成。
    pub(super) fn push<'a>(
        &mut self,
        meta: EntryMeta,
        names: impl IntoIterator<Item = Name<'a>>,
    ) -> io::Result<()> {
        let names_len = self.names.len();
        let links_len = self.links.len();
        let result = self.push_record(meta, names);
        if result.is_err() {
            // 局部记录失败不影响后续记录；必须撤销已经暂存的名字，不能留下悬空 record 编号。
            self.names.truncate(names_len);
            self.links.truncate(links_len);
        }
        result
    }

    fn push_record<'a>(
        &mut self,
        meta: EntryMeta,
        names: impl IntoIterator<Item = Name<'a>>,
    ) -> io::Result<()> {
        let id = meta.reference & ID_MASK;
        if !self.deferred && self.records.last().is_some_and(|last| last.id() >= id) {
            return Err(invalid(
                "MFT 记录号不递增 / MFT record numbers are not increasing",
            ));
        }
        let record = u32::try_from(self.records.len()).map_err(|_| capacity())?;
        let first_name = u32::try_from(self.links.len()).map_err(|_| capacity())?;
        let mut count = 0u16;
        for name in names {
            if name.namespace > 3 {
                return Err(invalid("文件名命名空间无效 / Invalid filename namespace"));
            }
            if name.namespace == 2 {
                continue;
            }
            let root_dot = id == ROOT_RECORD && name.name == [b'.' as u16];
            if name.name.is_empty()
                || name.name.len() > 255
                || (!root_dot && matches!(name.name, [46] | [46, 46]))
                || name.name.iter().any(|&c| matches!(c, 0 | 47 | 92))
            {
                return Err(invalid("MFT 文件名无效 / Invalid MFT filename"));
            }
            // NTFS 的多个命名空间可能给出相同的长名；同一父目录内只存一次。
            if self.links[first_name as usize..].iter().any(|link| {
                link.parent == name.parent_reference && self.name_units(link) == name.name
            }) {
                continue;
            }
            if count == 1024 {
                return Err(invalid("硬链接超过 NTFS 上限 / Too many NTFS hard links"));
            }
            let name_offset = u32::try_from(self.names.len()).map_err(|_| capacity())?;
            let end = self
                .names
                .len()
                .checked_add(name.name.len())
                .ok_or_else(capacity)?;
            if end > u32::MAX as usize || self.links.len() == u32::MAX as usize {
                return Err(capacity());
            }
            self.names
                .try_reserve(name.name.len())
                .map_err(|_| capacity())?;
            self.links.try_reserve(1).map_err(|_| capacity())?;
            self.names.extend_from_slice(name.name);
            self.links.push(Link {
                parent: name.parent_reference,
                record,
                name_offset,
                name_len: name.name.len() as u16,
            });
            count += 1;
        }
        self.records.try_reserve(1).map_err(|_| capacity())?;
        self.records.push(Record {
            reference: meta.reference,
            apparent: meta.apparent_size,
            allocated: meta.allocated_size,
            first_name,
            attrs: meta.attributes,
            tag: meta.reparse_tag,
            name_count: count,
            directory: meta.directory,
        });
        Ok(())
    }

    pub(super) fn heap_bytes(&self) -> usize {
        self.records.capacity() * std::mem::size_of::<Record>()
            + self.links.capacity() * std::mem::size_of::<Link>()
            + self.names.capacity() * std::mem::size_of::<u16>()
    }

    #[cfg(test)]
    pub(super) fn record_count(&self) -> usize {
        self.records.len()
    }

    fn name_units(&self, link: &Link) -> &[u16] {
        let start = link.name_offset as usize;
        &self.names[start..start + link.name_len as usize]
    }

    fn record_index(&self, id: u64) -> io::Result<usize> {
        self.records
            .binary_search_by_key(&id, Record::id)
            .map_err(|_| invalid("父目录记录不存在 / Parent directory record is missing"))
    }

    /// 仅调整记录表；名字区和链接区保持原位，利用每条记录自己的范围更新链接反向编号。
    /// 原地排序不分配全表映射，且恢复顺序扫描时同样的冲突选择及硬链接顺序。
    fn order_deferred(&mut self, cancel: &AtomicBool) -> io::Result<()> {
        if !self.deferred {
            return Ok(());
        }
        check_cancel(cancel)?;
        self.records.sort_unstable_by_key(Record::id);
        check_cancel(cancel)?;
        let mut previous = None;
        for (index, record) in self.records.iter().enumerate() {
            if index.is_multiple_of(4096) {
                check_cancel(cancel)?;
            }
            if previous == Some(record.id()) {
                return Err(invalid(
                    "延迟组装重复发布 MFT 记录 / Deferred assembly published a duplicate MFT record",
                ));
            }
            previous = Some(record.id());
            let index = u32::try_from(index).map_err(|_| capacity())?;
            let start = record.first_name as usize;
            let end = start
                .checked_add(record.name_count as usize)
                .ok_or_else(|| invalid("MFT link range overflows"))?;
            let links = self
                .links
                .get_mut(start..end)
                .ok_or_else(|| invalid("MFT deferred record link range is invalid"))?;
            for link in links {
                link.record = index;
            }
        }
        self.deferred = false;
        Ok(())
    }

    /// 根记录损坏也只影响根记录；索引本来就有虚拟根，其他合法根子项仍可展示。
    fn ensure_root(&mut self, anomalies: &mut Anomalies) -> io::Result<(usize, bool)> {
        match self.records.binary_search_by_key(&ROOT_RECORD, Record::id) {
            Ok(root) => {
                if !self.records[root].traversable() {
                    anomalies.note(ROOT_RECORD);
                    self.records[root].directory = true;
                    self.records[root].attrs &= !ATTR_REPARSE;
                }
                Ok((root, false))
            }
            Err(root) => {
                anomalies.note(ROOT_RECORD);
                if self.records.len() == u32::MAX as usize {
                    return Err(capacity());
                }
                self.records.try_reserve(1).map_err(|_| capacity())?;
                self.records.insert(
                    root,
                    Record {
                        reference: ROOT_RECORD,
                        apparent: 0,
                        allocated: 0,
                        first_name: 0,
                        attrs: 0x10,
                        tag: 0,
                        name_count: 0,
                        directory: true,
                    },
                );
                for link in &mut self.links {
                    if link.record as usize >= root {
                        link.record += 1;
                    }
                }
                Ok((root, true))
            }
        }
    }

    fn resolve(
        &mut self,
        cancel: &AtomicBool,
        anomalies: &mut Anomalies,
    ) -> io::Result<(usize, Vec<Resolved>)> {
        check_cancel(cancel)?;
        let (root, virtual_root) = self.ensure_root(anomalies)?;
        let mut resolved = Vec::new();
        resolved
            .try_reserve_exact(self.records.len())
            .map_err(|_| capacity())?;
        resolved.resize(self.records.len(), Resolved::default());
        // 同目录的文件常在 MFT 中相邻；缓存完整引用，不能把已复用记录的序列号混为一谈。
        let mut parent_cache = None;
        for i in 0..self.links.len() {
            if i.is_multiple_of(4096) {
                check_cancel(cancel)?;
            }
            let link = &self.links[i];
            let child = link.record as usize;
            let parent = match parent_cache {
                Some((reference, parent)) if reference == link.parent => parent,
                _ => {
                    let parent = self.record_index(link.parent & ID_MASK).ok();
                    parent_cache = Some((link.parent, parent));
                    parent
                }
            };
            let valid = parent.filter(|&p| {
                self.records[p].directory
                    && ((virtual_root && p == root) || self.records[p].reference == link.parent)
            });
            if let Some(parent) = valid {
                self.links[i].parent = parent as u64;
            } else {
                anomalies.note(self.records[child].id());
                self.links[i].parent = NO_PARENT;
            }
        }
        for (i, record) in self.records.iter_mut().enumerate() {
            if i.is_multiple_of(4096) {
                check_cancel(cancel)?;
            }
            resolved[i].parent = root as u32;
            if i == root {
                let start = record.first_name as usize;
                let end = start + record.name_count as usize;
                if record.name_count > 1
                    || self.links[start..end]
                        .iter()
                        .any(|l| l.parent != root as u64)
                {
                    anomalies.note(ROOT_RECORD);
                }
                resolved[i].state = DONE;
                continue;
            }
            let start = record.first_name as usize;
            let end = start + record.name_count as usize;
            let mut chosen = None;
            for (offset, link) in self.links[start..end].iter_mut().enumerate() {
                if link.parent == NO_PARENT {
                    continue;
                }
                if record.directory && chosen.is_some() {
                    // 目录不能有多个父路径，保留第一个合法引用，其他名字不扩散成重复子树。
                    anomalies.note(record.id());
                    link.parent = NO_PARENT;
                } else if chosen.is_none() {
                    chosen = Some(start + offset);
                }
            }
            if let Some(first) = chosen {
                resolved[i].parent = self.links[first].parent as u32;
                if record.directory {
                    record.first_name = first as u32;
                }
            } else {
                if record.id() >= 16
                    || record.directory
                    || record.apparent != 0
                    || record.allocated != 0
                {
                    anomalies.note(record.id());
                }
                resolved[i].state = INVALID | DONE;
            }
        }
        let mut chain = Vec::<usize>::new();
        for start in 0..self.records.len() {
            if start.is_multiple_of(4096) {
                check_cancel(cancel)?;
            }
            if !self.records[start].directory || resolved[start].state & DONE != 0 {
                continue;
            }
            let mut current = start;
            while resolved[current].state & DONE == 0 {
                if chain.len().is_multiple_of(4096) {
                    check_cancel(cancel)?;
                }
                if resolved[current].state & VISITING != 0 {
                    // 链中这些项都依赖此环；其他分支不受影响。下一轮遇到已隔离的父项也能立即停止。
                    for &i in &chain {
                        anomalies.note(self.records[i].id());
                        resolved[i].state = INVALID | DONE;
                    }
                    chain.clear();
                    break;
                }
                resolved[current].state |= VISITING;
                chain.try_reserve(1).map_err(|_| capacity())?;
                chain.push(current);
                current = resolved[current].parent as usize;
            }
            while let Some(i) = chain.pop() {
                let parent = resolved[i].parent as usize;
                if resolved[parent].state & INVALID != 0 {
                    anomalies.note(self.records[i].id());
                    resolved[i].state = INVALID | DONE;
                    continue;
                }
                let depth = u32::from(resolved[parent].depth) + 1;
                let units = u32::from(resolved[parent].path_units)
                    + u32::from(self.links[self.records[i].first_name as usize].name_len)
                    + 1;
                if depth > u32::from(MAX_DEPTH) || units > 32767 {
                    anomalies.note(self.records[i].id());
                    resolved[i].state = INVALID | DONE;
                    continue;
                }
                let extend =
                    self.records[i].id() == EXTEND_RECORD || resolved[parent].state & EXTEND != 0;
                let hidden = self.records[i].tag() == TAG_MOUNT_POINT
                    || resolved[parent].state & HIDDEN != 0
                    || !self.records[parent].traversable();
                resolved[i].depth = depth as u16;
                resolved[i].path_units = units as u16;
                resolved[i].state =
                    DONE | if extend { EXTEND } else { 0 } | if hidden { HIDDEN } else { 0 };
            }
        }
        Ok((root, resolved))
    }

    /// 先隔离异常记录，再用批量插入接口建树；取消时不发布结果。
    pub(super) fn build(
        mut self,
        root_path: &Path,
        classifier: &Classifier,
        apparent: bool,
        cancel: &AtomicBool,
        anomalies: &mut Anomalies,
        timings: &mut Timings,
    ) -> io::Result<Index> {
        let started = Instant::now();
        let initial_classify = timings.classify;
        let initial_index = timings.index;
        let initial_hardlinks = timings.hardlinks;
        self.order_deferred(cancel)?;
        let (root, mut resolved) = self.resolve(cancel, anomalies)?;
        // 一个物理文件只要有名字位于 $Extend 下，就作为元文件处理所有硬链接。
        for link in &self.links {
            if link.parent != NO_PARENT
                && !self.records[link.record as usize].traversable()
                && resolved[link.parent as usize].state & EXTEND != 0
            {
                resolved[link.record as usize].state |= EXTEND;
            }
        }
        for link in &mut self.links {
            if link.parent == NO_PARENT {
                continue;
            }
            let child = link.record as usize;
            let record = &self.records[child];
            let parent = link.parent as usize;
            if resolved[child].state & INVALID != 0 || resolved[parent].state & INVALID != 0 {
                anomalies.note(record.id());
                link.parent = NO_PARENT;
                continue;
            }
            let hidden =
                resolved[parent].state & HIDDEN != 0 || !self.records[parent].traversable();
            if link.record as usize == root
                || hidden
                || (record.directory && record.tag() == TAG_MOUNT_POINT)
            {
                link.parent = NO_PARENT;
            } else if !record.traversable()
                && (record.id() < 16 || resolved[link.record as usize].state & EXTEND != 0)
            {
                link.parent = root as u64;
            }
        }
        let mut total = 0u64;
        // 只检查会显示的唯一文件；异常大小只隔离当前记录，不能清空健康子树。
        for link in &mut self.links {
            if link.parent == NO_PARENT {
                continue;
            }
            let i = link.record as usize;
            let record = &self.records[i];
            if resolved[i].state & INVALID != 0 {
                link.parent = NO_PARENT;
                continue;
            }
            if resolved[i].state & VISIBLE == 0 && !record.traversable() {
                let size = record.size(apparent);
                if size > i64::MAX as u64 || total.checked_add(size).is_none() {
                    anomalies.note(record.id());
                    resolved[i].state |= INVALID;
                    link.parent = NO_PARENT;
                    continue;
                }
                total += size;
            }
            resolved[i].state |= VISIBLE;
        }
        // 只按父编号排序；每个目录中的名字随后转换一次、按索引的同一规则排序。
        self.links.sort_unstable_by_key(|link| link.parent);
        let stage = Instant::now();
        let root_class = classifier.root_class(root_path);
        timings.classify += stage.elapsed();
        let mut index = Index::new(Os::Windows, root_path, root_class);
        let mut stack = vec![(root, DirId::ROOT, u32::MAX)];
        let mut children = Vec::<Child>::new();
        let mut child_names = ChildNames::default();
        // 普通路径复用 UTF-8 缓冲区，栈只记父路径的完整字符边界；原生特殊名字走原有构造路径。
        let mut path_text = root_path.to_str().unwrap_or_default().to_owned();
        let mut classify_path = String::new();
        while let Some((record_id, dir_id, parent_len)) = stack.pop() {
            check_cancel(cancel)?;
            resolved[record_id].state |= PUBLISHED;
            let path: Cow<'_, Path> = if dir_id == DirId::ROOT && root_path.to_str().is_some() {
                Cow::Borrowed(Path::new(&path_text))
            } else if let (true, Some(name)) =
                (parent_len != u32::MAX, index.dir(dir_id).name().to_str())
            {
                path_text.truncate(parent_len as usize);
                push_text_component(&mut path_text, name);
                Cow::Borrowed(Path::new(&path_text))
            } else {
                Cow::Owned(index.dir_path(dir_id))
            };
            let child_parent_len = path
                .to_str()
                .and_then(|path| u32::try_from(path.len()).ok())
                .unwrap_or(u32::MAX);
            classify_path.clear();
            if let Some(path) = path.to_str() {
                classify_path.push_str(path);
            }
            let classify_parent_len = classify_path.len();
            let stage = Instant::now();
            let ctx = classifier.enter_dir(index.dir(dir_id).class(), &path);
            timings.classify += stage.elapsed();
            let start = self
                .links
                .partition_point(|link| link.parent < record_id as u64);
            let end = self
                .links
                .partition_point(|link| link.parent <= record_id as u64);
            children.clear();
            child_names.clear();
            for (i, link) in self.links[start..end].iter().enumerate() {
                if i.is_multiple_of(4096) {
                    check_cancel(cancel)?;
                }
                let child = match child_names.push(link.record, self.name_units(link)) {
                    Ok(child) => child,
                    Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                        anomalies.note(self.records[link.record as usize].id());
                        continue;
                    }
                    Err(error) => return Err(error),
                };
                children.push(child);
            }
            children.sort_unstable_by(|a, b| {
                child_names
                    .sort_key(a)
                    .cmp(child_names.sort_key(b))
                    .then_with(|| {
                        child_names
                            .name(a)
                            .as_encoded_bytes()
                            .cmp(child_names.name(b).as_encoded_bytes())
                    })
                    .then_with(|| a.record.cmp(&b.record))
            });
            children.dedup_by(|a, b| {
                if child_names.sort_key(a) != child_names.sort_key(b) {
                    return false;
                }
                let (left, right) = (child_names.name(a), child_names.name(b));
                if a.record != b.record || left != right {
                    anomalies.note(self.records[a.record as usize].id());
                    anomalies.note(self.records[b.record as usize].id());
                }
                true
            });
            let core = |r: u32| {
                let r = r as usize;
                self.records[r].id() < 16
                    || resolved[r].state & EXTEND != 0
                    || resolved[resolved[r].parent as usize].state & EXTEND != 0
            };
            let mut dirs = Vec::new();
            let mut files = Vec::new();
            let mut dir_records = Vec::new();
            let mut file_records = Vec::new();
            // 按目录批量计时，避免每个文件读取时钟；包含分类结果所需的临时目录项。
            let stage = Instant::now();
            for child in &children {
                let record = &self.records[child.record as usize];
                let name = child_names.name(child);
                if record.traversable() {
                    let child_path: Cow<'_, Path> =
                        if let (true, Some(name)) = (child_parent_len != u32::MAX, name.to_str()) {
                            classify_path.truncate(classify_parent_len);
                            push_text_component(&mut classify_path, name);
                            Cow::Borrowed(Path::new(&classify_path))
                        } else {
                            let mut child_path = path.as_os_str().to_owned();
                            if !child_path
                                .as_encoded_bytes()
                                .last()
                                .is_some_and(|&b| matches!(b, b'/' | b'\\'))
                            {
                                child_path.push("\\");
                            }
                            child_path.push(name);
                            Cow::Owned(PathBuf::from(child_path))
                        };
                    let mut class = classifier.dir_class(&ctx, name, &child_path);
                    if core(child.record) {
                        class.0 = Category::Core;
                    }
                    dirs.push(NewDir {
                        name,
                        class,
                        flags: 0,
                    });
                    dir_records.push(child.record);
                } else {
                    files.push(NewFile {
                        name,
                        size: record.size(apparent),
                        category: if core(child.record) {
                            Category::Core
                        } else {
                            classifier.file_class(&ctx, name)
                        },
                        linked: record.name_count > 1,
                    });
                    file_records.push(child.record);
                }
            }
            timings.classify += stage.elapsed();
            let stage = Instant::now();
            let filled = index
                .fill_dir(dir_id, &dirs, &files)
                .map_err(|e| io::Error::new(io::ErrorKind::OutOfMemory, e))?;
            timings.index += stage.elapsed();
            for &record in &file_records {
                resolved[record as usize].state |= PUBLISHED;
            }
            for (record, id) in dir_records.into_iter().zip(filled.dirs).rev() {
                stack.push((record as usize, DirId(id), child_parent_len));
            }
            let stage = Instant::now();
            for (i, (record, id)) in file_records.into_iter().zip(filled.files).enumerate() {
                if i.is_multiple_of(4096) {
                    check_cancel(cancel)?;
                }
                let r = record as usize;
                if self.records[r].name_count > 1 && resolved[r].state & COUNTED == 0 {
                    // 当前目录先处理文件，再按名字递归子目录，正是 cmp_file_order 的顺序。
                    // 首次出现的合法名字即可计入，无须暂存全部硬链接再建路径链比较。
                    index.count_file(dir_id, FileId(id));
                    resolved[r].state |= COUNTED;
                }
            }
            timings.hardlinks += stage.elapsed();
        }
        for (i, record) in self.records.iter().enumerate() {
            if i.is_multiple_of(4096) {
                check_cancel(cancel)?;
            }
            if resolved[i].state & VISIBLE != 0 && resolved[i].state & PUBLISHED == 0 {
                // 名字冲突或不能表示的目录名会使其子树不可达；报告受影响项而不冒用其他路径。
                anomalies.note(record.id());
            }
        }
        check_cancel(cancel)?;
        let stage = Instant::now();
        index.shrink_to_fit();
        timings.index += stage.elapsed();
        timings.tree += started.elapsed().saturating_sub(
            (timings.classify - initial_classify)
                + (timings.index - initial_index)
                + (timings.hardlinks - initial_hardlinks),
        );
        Ok(index)
    }
}

fn push_text_component(path: &mut String, name: &str) {
    if !path
        .as_bytes()
        .last()
        .is_some_and(|&b| matches!(b, b'/' | b'\\'))
    {
        path.push('\\');
    }
    path.push_str(name);
}

struct Child {
    record: u32,
    offset: u32,
    /// 非零为集中 UTF-8 名字长度；零为 unusual 中的原生名字索引。
    len: u32,
    key_offset: u32,
    /// 零表示键与原始名字相同；否则指向 ChildNames::keys。
    key_len: u32,
}

/// 一个目录只分配连续名字区，避免数百万个短 OsString 堆分配。
/// Windows 的孤立 UTF-16 代理项仍单独保存为 OsString，不能有损替换。
#[derive(Default)]
struct ChildNames {
    text: String,
    /// 折叠键可含 WTF-8 孤立代理项，不能混入 UTF-8 String。
    keys: Vec<u8>,
    unusual: Vec<OsString>,
}

impl ChildNames {
    fn clear(&mut self) {
        self.text.clear();
        self.keys.clear();
        self.unusual.clear();
    }

    fn push(&mut self, record: u32, units: &[u16]) -> io::Result<Child> {
        if units.is_empty() {
            return Err(invalid("空的文件名 / Empty filename"));
        }
        let start = self.text.len();
        self.text
            .try_reserve(units.len() * 3)
            .map_err(|_| capacity())?;
        for character in char::decode_utf16(units.iter().copied()) {
            match character {
                Ok(character) => self.text.push(character),
                Err(_) => {
                    self.text.truncate(start);
                    let name = native_name(units)?;
                    let offset = u32::try_from(self.unusual.len()).map_err(|_| capacity())?;
                    let key_offset = self.keys.len();
                    self.keys
                        .try_reserve(units.len() * 3)
                        .map_err(|_| capacity())?;
                    crate::platform::path::extend_name_key(
                        Os::Windows,
                        name.as_encoded_bytes(),
                        &mut self.keys,
                    );
                    let key_len = self.keys.len() - key_offset;
                    if self.keys.len() > u32::MAX as usize {
                        return Err(capacity());
                    }
                    self.unusual.try_reserve(1).map_err(|_| capacity())?;
                    self.unusual.push(name);
                    return Ok(Child {
                        record,
                        offset,
                        len: 0,
                        key_offset: key_offset as u32,
                        key_len: key_len as u32,
                    });
                }
            }
        }
        let len = self.text.len() - start;
        let native = &self.text.as_bytes()[start..];
        let key_offset = self.keys.len();
        // ASCII 大写名字复用原文；其他名字只折叠一次，且不用逐文件的临时分配。
        let mut key_len = 0;
        if !native.is_ascii() || native.iter().any(u8::is_ascii_lowercase) {
            self.keys
                .try_reserve(units.len() * 3)
                .map_err(|_| capacity())?;
            crate::platform::path::extend_name_key(Os::Windows, native, &mut self.keys);
            if self.keys[key_offset..] == *native {
                self.keys.truncate(key_offset);
            } else {
                key_len = self.keys.len() - key_offset;
            }
        }
        if self.text.len() > u32::MAX as usize || self.keys.len() > u32::MAX as usize {
            return Err(capacity());
        }
        Ok(Child {
            record,
            offset: start as u32,
            len: len as u32,
            key_offset: key_offset as u32,
            key_len: key_len as u32,
        })
    }

    fn name(&self, child: &Child) -> &OsStr {
        if child.len == 0 {
            &self.unusual[child.offset as usize]
        } else {
            let start = child.offset as usize;
            OsStr::new(&self.text[start..start + child.len as usize])
        }
    }

    fn sort_key(&self, child: &Child) -> &[u8] {
        if child.key_len == 0 {
            return self.name(child).as_encoded_bytes();
        }
        let start = child.key_offset as usize;
        &self.keys[start..start + child.key_len as usize]
    }
}

fn native_name(units: &[u16]) -> io::Result<OsString> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        Ok(OsString::from_wide(units))
    }
    #[cfg(not(windows))]
    {
        // 非 Windows 的 OsString 无法无损表达孤立代理项；不能有损转换后合并不同文件。
        String::from_utf16(units).map(OsString::from).map_err(|_| {
            invalid("镜像含无法在此平台表示的 UTF-16 名字 / Image contains a UTF-16 name unsupported on this platform")
        })
    }
}

fn check_cancel(cancel: &AtomicBool) -> io::Result<()> {
    if cancel.load(Relaxed) {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "已取消 MFT 建树 / MFT tree construction cancelled",
        ))
    } else {
        Ok(())
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn capacity() -> io::Error {
    io::Error::new(
        io::ErrorKind::OutOfMemory,
        "MFT 暂存表超过容量 / MFT staging table capacity exceeded",
    )
}

#[cfg(test)]
#[path = "tree_tests.rs"]
mod tests;
