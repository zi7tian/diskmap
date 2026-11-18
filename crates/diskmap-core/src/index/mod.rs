//! 完整索引（T01）：扫描结果的目录树存储与查询。对应 legacy/backend/scanner.py 的 `Dir` 和 `find`。
//!
//! 存储方式：
//! - 所有名字放在一个连续的字节区里，节点只记录位置和长度。名字按系统原样保存
//!   （`OsStr::as_encoded_bytes`），Linux 上不是 UTF-8 的名字也能保存和查找。
//! - 目录节点放在 `dirs`，文件放在 `files`。一个目录的子目录、文件在各自的数组里是连续的一段，
//!   在读完这个目录时用 [`Index::fill_dir`] 一次写入，并且按名字排好序，查找时二分。
//! - 每个目录记录整棵子树的总大小、文件数、子目录数和各类别大小，写入时沿上级目录逐级累加。
//!   文件条目 16 字节，目录节点 104 字节，另加名字本身。
//!
//! 与旧版相同：大小只统计文件，不含目录本身占用的空间；扫描起点自己不计入目录数；
//! 一个文件有多个硬链接时只计入一个（见 [`NewFile::linked`]、[`Index::count_file`]），其余的不计入、不显示。
//!
//! 扫描期间索引放在读写锁里（见 `scan`）：扫描线程每读完一个目录写入一次，界面随时可以读取。
//! NTFS MFT 扫描（T03）同样用 [`Index::fill_dir`] 写入：按父目录分组后，从根目录开始逐层写入。

mod storage;

use std::cmp::Ordering;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::{Path, PathBuf};

use crate::classify::{DirClass, ReasonId};
use crate::model::{Category, NCLS};
use crate::platform::Os;

/// 目录标志：没有权限读取。数值与旧版相同，界面 JSON 的 `fl` 字段直接使用。
pub const DENIED: u8 = 1;
/// 目录标志：另一个文件系统的挂载点，扫描时进入了它。数值与旧版相同。
pub const MOUNT: u8 = 2;
/// 内部标志：目录内容已经写入。
const LISTED: u8 = 0x80;
/// 内部文件标志：有多个硬链接、（还）没有计入统计的文件。
const UNCOUNTED: u8 = 1;

/// 目录编号。根目录是 [`DirId::ROOT`]。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DirId(pub u32);

impl DirId {
    /// 扫描起点。
    pub const ROOT: DirId = DirId(0);

    fn idx(self) -> usize {
        self.0 as usize
    }
}

/// 文件编号（全部文件按写入顺序编号）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FileId(pub u32);

impl FileId {
    fn idx(self) -> usize {
        self.0 as usize
    }
}

/// 要写入的子目录。
#[derive(Clone, Copy, Debug)]
pub struct NewDir<'a> {
    pub name: &'a OsStr,
    pub class: DirClass,
    /// [`MOUNT`] 等标志。
    pub flags: u8,
}

/// 要写入的文件。
#[derive(Clone, Copy, Debug)]
pub struct NewFile<'a> {
    pub name: &'a OsStr,
    pub size: u64,
    pub category: Category,
    /// 有多个硬链接的文件。写入时先不计入大小和数量，也不出现在 [`Dir::files`] 和查找结果里；
    /// 全部写入后，对每组链接调用一次 [`Index::count_file`]，只计入其中一个。
    pub linked: bool,
}

/// [`Index::fill_dir`] 写入的子目录、文件的编号范围，顺序与传入的相同。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Filled {
    pub dirs: std::ops::Range<u32>,
    pub files: std::ops::Range<u32>,
}

/// 写入失败。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndexError {
    /// 这个目录的内容已经写入过。
    AlreadyListed,
    /// 名字太长（超过 65535 字节），或者索引超出了容量（约 40 亿个条目、4 GB 名字）。
    Full,
}

impl fmt::Display for IndexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IndexError::AlreadyListed => f.write_str("directory contents already written"),
            IndexError::Full => f.write_str("index capacity exceeded"),
        }
    }
}

impl std::error::Error for IndexError {}

/// 按路径查找的结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Found {
    Dir(DirId),
    File { parent: DirId, file: FileId },
}

#[derive(Clone, Debug)]
struct DirNode {
    name_off: u32,
    name_len: u16,
    flags: u8,
    category: u8,
    reason: u32,
    parent: u32,
    dirs_start: u32,
    dirs_len: u32,
    files_start: u32,
    files_len: u32,
    nfiles: u32,
    ndirs: u32,
    size: u64,
    csz: [u64; NCLS],
}

#[derive(Clone, Copy, Debug)]
struct FileEntry {
    size: u64,
    name_off: u32,
    name_len: u16,
    category: u8,
    flags: u8,
}

const NO_PARENT: u32 = u32::MAX;

/// 扫描结果的完整目录树。
#[derive(Clone, Debug)]
pub struct Index {
    os: Os,
    root_path: PathBuf,
    names: Vec<u8>,
    dirs: Vec<DirNode>,
    files: Vec<FileEntry>,
}

impl Index {
    /// 新建只有扫描起点的索引。`root_path` 应是规范化后的绝对路径，`class` 来自 `Classifier::root_class`。
    pub fn new(os: Os, root_path: &Path, class: DirClass) -> Index {
        let mut index = Index {
            os,
            root_path: root_path.to_path_buf(),
            names: Vec::new(),
            dirs: Vec::new(),
            files: Vec::new(),
        };
        // 根目录的名字就是它的完整路径（与旧版相同）；路径长度超过 u16 时截断，只影响显示
        let bytes = root_path.as_os_str().as_encoded_bytes();
        let len = bytes.len().min(u16::MAX as usize);
        index.names.extend_from_slice(&bytes[..len]);
        index.dirs.push(DirNode {
            name_off: 0,
            name_len: len as u16,
            flags: 0,
            category: class.0 as u8,
            reason: class.1.0,
            parent: NO_PARENT,
            dirs_start: 0,
            dirs_len: 0,
            files_start: 0,
            files_len: 0,
            nfiles: 0,
            ndirs: 0,
            size: 0,
            csz: [0; NCLS],
        });
        index
    }

    /// 路径规则所属的系统（决定是否区分大小写、路径分隔符）。
    pub fn os(&self) -> Os {
        self.os
    }

    /// 扫描起点。
    pub fn root_path(&self) -> &Path {
        &self.root_path
    }

    /// 目录总数，包括扫描起点。
    pub fn dir_count(&self) -> usize {
        self.dirs.len()
    }

    /// 文件条目总数，包括没有计入的硬链接。
    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    /// 索引占用的堆内存（字节），按已分配的容量计算。
    pub fn heap_bytes(&self) -> usize {
        self.names.capacity()
            + self.dirs.capacity() * std::mem::size_of::<DirNode>()
            + self.files.capacity() * std::mem::size_of::<FileEntry>()
    }

    /// 释放多余的容量。扫描结束后调用一次。
    pub fn shrink_to_fit(&mut self) {
        self.names.shrink_to_fit();
        self.dirs.shrink_to_fit();
        self.files.shrink_to_fit();
    }

    /// 比较两个名字，顺序与索引内部的排序一致：Windows 不区分大小写，其他系统按字节比较。
    /// 写入 [`Index::fill_dir`] 之前，子目录和文件都要先按它排好序。
    pub fn cmp_names(&self, a: &OsStr, b: &OsStr) -> Ordering {
        cmp_names(self.os, a.as_encoded_bytes(), b.as_encoded_bytes())
    }

    // ------------------------------------------------------------------ 写入

    /// 写入一个目录读到的全部内容。每个目录只能写入一次。
    ///
    /// `subdirs` 和 `files` 必须已经按 [`Index::cmp_names`] 排好序。返回新子目录、新文件的编号，
    /// 顺序与传入的相同。文件大小、数量和各类别大小会累加到这个目录和它的所有上级目录
    /// （[`NewFile::linked`] 的文件除外）。
    pub fn fill_dir(
        &mut self,
        dir: DirId,
        subdirs: &[NewDir<'_>],
        files: &[NewFile<'_>],
    ) -> Result<Filled, IndexError> {
        let node = &self.dirs[dir.idx()];
        if node.flags & LISTED != 0 {
            return Err(IndexError::AlreadyListed);
        }
        debug_assert!(
            subdirs
                .windows(2)
                .all(|w| self.cmp_names(w[0].name, w[1].name) != Ordering::Greater)
        );
        debug_assert!(
            files
                .windows(2)
                .all(|w| self.cmp_names(w[0].name, w[1].name) != Ordering::Greater)
        );

        // 先检查容量，确保要么全部写入，要么什么都不改
        let mut name_bytes = 0usize;
        for name in subdirs
            .iter()
            .map(|d| d.name)
            .chain(files.iter().map(|f| f.name))
        {
            let len = name.as_encoded_bytes().len();
            if len > u16::MAX as usize {
                return Err(IndexError::Full);
            }
            name_bytes += len;
        }
        let fits = |current: usize, extra: usize| {
            current
                .checked_add(extra)
                .is_some_and(|n| n <= u32::MAX as usize)
        };
        if !fits(self.names.len(), name_bytes)
            || !fits(self.dirs.len(), subdirs.len())
            || !fits(self.files.len(), files.len())
        {
            return Err(IndexError::Full);
        }

        let dirs_start = self.dirs.len() as u32;
        let files_start = self.files.len() as u32;
        self.names.reserve(name_bytes);
        self.dirs.reserve(subdirs.len());
        self.files.reserve(files.len());

        for d in subdirs {
            let (name_off, name_len) = self.push_name(d.name);
            self.dirs.push(DirNode {
                name_off,
                name_len,
                flags: d.flags & !LISTED,
                category: d.class.0 as u8,
                reason: d.class.1.0,
                parent: dir.0,
                dirs_start: 0,
                dirs_len: 0,
                files_start: 0,
                files_len: 0,
                nfiles: 0,
                ndirs: 0,
                size: 0,
                csz: [0; NCLS],
            });
        }
        let mut total = 0u64;
        let mut counted = 0u32;
        let mut csz = [0u64; NCLS];
        for f in files {
            let (name_off, name_len) = self.push_name(f.name);
            self.files.push(FileEntry {
                size: f.size,
                name_off,
                name_len,
                category: f.category as u8,
                flags: if f.linked { UNCOUNTED } else { 0 },
            });
            if !f.linked {
                total += f.size;
                counted += 1;
                csz[f.category.index()] += f.size;
            }
        }

        let node = &mut self.dirs[dir.idx()];
        node.flags |= LISTED;
        node.dirs_start = dirs_start;
        node.dirs_len = subdirs.len() as u32;
        node.files_start = files_start;
        node.files_len = files.len() as u32;

        let ndirs = subdirs.len() as u32;
        let mut cur = dir.0;
        while cur != NO_PARENT {
            let n = &mut self.dirs[cur as usize];
            n.size += total;
            n.nfiles += counted;
            n.ndirs += ndirs;
            for (sum, add) in n.csz.iter_mut().zip(csz) {
                *sum += add;
            }
            cur = n.parent;
        }
        Ok(Filled {
            dirs: dirs_start..dirs_start + ndirs,
            files: files_start..files_start + files.len() as u32,
        })
    }

    /// 计入一个写入时标为 [`NewFile::linked`] 的文件：大小和数量累加到所在目录 `parent` 和所有上级目录，
    /// 之后它出现在 [`Dir::files`] 和查找结果里。已经计入的文件返回 `false`，不做改动。
    pub fn count_file(&mut self, parent: DirId, file: FileId) -> bool {
        let p = &self.dirs[parent.idx()];
        debug_assert!((p.files_start..p.files_start + p.files_len).contains(&file.0));
        let entry = &mut self.files[file.idx()];
        if entry.flags & UNCOUNTED == 0 {
            return false;
        }
        entry.flags &= !UNCOUNTED;
        let (size, category) = (entry.size, entry.category as usize);
        let mut cur = parent.0;
        while cur != NO_PARENT {
            let n = &mut self.dirs[cur as usize];
            n.size += size;
            n.nfiles += 1;
            n.csz[category] += size;
            cur = n.parent;
        }
        true
    }

    /// 两个文件在"按名字排序的深度优先遍历"中的先后：同一个目录里按名字；一个目录里的文件先于它的
    /// 子目录里的内容。参数是（所在目录, 文件）。同一组硬链接按这个顺序选出计入的那一个，结果固定。
    pub fn cmp_file_order(&self, a: (DirId, FileId), b: (DirId, FileId)) -> Ordering {
        let chain = |mut d: DirId| {
            let mut v = Vec::new();
            while d != DirId::ROOT {
                v.push(d);
                d = DirId(self.dirs[d.idx()].parent);
            }
            v.reverse();
            v
        };
        let (ca, cb) = (chain(a.0), chain(b.0));
        for (&x, &y) in ca.iter().zip(&cb) {
            if x != y {
                // 前面的部分相同，x、y 是同一个目录下的两个子目录
                return self.cmp_entry_names(self.dir(x).name(), self.dir(y).name());
            }
        }
        match ca.len().cmp(&cb.len()) {
            Ordering::Equal => self.cmp_entry_names(self.file(a.1).name(), self.file(b.1).name()),
            // a 所在的目录是 b 所在目录的上级：a 先遇到
            other => other,
        }
    }

    /// 按索引的顺序比较名字；不区分大小写时相同的名字再按原样的字节比较，保证结果固定。
    fn cmp_entry_names(&self, a: &OsStr, b: &OsStr) -> Ordering {
        self.cmp_names(a, b)
            .then_with(|| a.as_encoded_bytes().cmp(b.as_encoded_bytes()))
    }

    /// 给目录加上标志（例如 [`DENIED`]）。
    pub fn add_flags(&mut self, dir: DirId, flags: u8) {
        self.dirs[dir.idx()].flags |= flags & !LISTED;
    }

    fn push_name(&mut self, name: &OsStr) -> (u32, u16) {
        let bytes = name.as_encoded_bytes();
        let off = self.names.len() as u32;
        self.names.extend_from_slice(bytes);
        (off, bytes.len() as u16)
    }

    // ------------------------------------------------------------------ 读取

    /// 目录。
    pub fn dir(&self, id: DirId) -> Dir<'_> {
        Dir {
            index: self,
            id,
            node: &self.dirs[id.idx()],
        }
    }

    /// 扫描起点。
    pub fn root(&self) -> Dir<'_> {
        self.dir(DirId::ROOT)
    }

    /// 全部目录，按编号顺序（父目录总在子目录前面）。
    pub fn dirs(&self) -> impl ExactSizeIterator<Item = Dir<'_>> + '_ {
        (0..self.dirs.len() as u32).map(|i| self.dir(DirId(i)))
    }

    /// 文件。
    pub fn file(&self, id: FileId) -> File<'_> {
        File {
            index: self,
            id,
            entry: &self.files[id.idx()],
        }
    }

    fn name_bytes(&self, off: u32, len: u16) -> &[u8] {
        &self.names[off as usize..off as usize + len as usize]
    }

    /// 在目录 `parent` 里按名字找子目录。
    pub fn find_subdir(&self, parent: DirId, name: &OsStr) -> Option<DirId> {
        self.find_subdir_bytes(parent, name.as_encoded_bytes())
    }

    /// 在目录 `parent` 里按名字找文件。
    pub fn find_file(&self, parent: DirId, name: &OsStr) -> Option<FileId> {
        self.find_file_bytes(parent, name.as_encoded_bytes())
    }

    fn find_subdir_bytes(&self, parent: DirId, name: &[u8]) -> Option<DirId> {
        let p = &self.dirs[parent.idx()];
        let range = p.dirs_start as usize..(p.dirs_start + p.dirs_len) as usize;
        self.dirs[range]
            .binary_search_by(|d| cmp_names(self.os, self.name_bytes(d.name_off, d.name_len), name))
            .ok()
            .map(|i| DirId(p.dirs_start + i as u32))
    }

    fn find_file_bytes(&self, parent: DirId, name: &[u8]) -> Option<FileId> {
        let p = &self.dirs[parent.idx()];
        let range = p.files_start as usize..(p.files_start + p.files_len) as usize;
        let slice = &self.files[range];
        slice
            .binary_search_by(|f| cmp_names(self.os, self.name_bytes(f.name_off, f.name_len), name))
            .ok()
            .filter(|&i| slice[i].flags & UNCOUNTED == 0)
            .map(|i| FileId(p.files_start + i as u32))
    }

    /// 按完整路径查找（对应旧版 `Scan.find`）。路径应是规范化后的绝对路径；不在扫描范围内或
    /// 没有扫描到时返回 `None`。Windows 不区分大小写，`/` 和 `\` 都算分隔符。
    pub fn find(&self, path: &Path) -> Option<Found> {
        let os = self.os;
        let path = path.as_os_str().as_encoded_bytes();
        let root = self.root_path.as_os_str().as_encoded_bytes();
        // 开头的分隔符个数（"/"、UNC 路径的 "\\"）必须相同，其余逐个部分比较
        if leading_seps(os, path) != leading_seps(os, root) {
            return None;
        }
        let mut parts = components(os, path);
        for want in components(os, root) {
            let got = parts.next()?;
            if cmp_names(os, got, want) != Ordering::Equal {
                return None;
            }
        }
        let mut cur = DirId::ROOT;
        let mut parts = parts.peekable();
        while let Some(part) = parts.next() {
            if parts.peek().is_none() {
                if let Some(d) = self.find_subdir_bytes(cur, part) {
                    return Some(Found::Dir(d));
                }
                return self
                    .find_file_bytes(cur, part)
                    .map(|file| Found::File { parent: cur, file });
            }
            cur = self.find_subdir_bytes(cur, part)?;
        }
        Some(Found::Dir(cur))
    }

    /// 目录的完整路径。
    pub fn dir_path(&self, id: DirId) -> PathBuf {
        let mut chain = Vec::new();
        let mut cur = id.0;
        while cur != DirId::ROOT.0 {
            chain.push(cur);
            cur = self.dirs[cur as usize].parent;
        }
        let mut out = OsString::from(self.root_path.as_os_str());
        for &i in chain.iter().rev() {
            self.push_component(&mut out, self.dir(DirId(i)).name());
        }
        PathBuf::from(out)
    }

    /// 文件的完整路径。`parent` 是文件所在的目录。
    pub fn file_path(&self, parent: DirId, file: FileId) -> PathBuf {
        let mut out = self.dir_path(parent).into_os_string();
        self.push_component(&mut out, self.file(file).name());
        PathBuf::from(out)
    }

    fn push_component(&self, out: &mut OsString, name: &OsStr) {
        let sep = self.os.sep();
        if !out
            .as_encoded_bytes()
            .last()
            .is_some_and(|&b| is_sep(self.os, b))
        {
            let mut buf = [0u8; 4];
            out.push(&*sep.encode_utf8(&mut buf));
        }
        out.push(name);
    }
}

/// 一个目录的只读视图。
#[derive(Clone, Copy)]
pub struct Dir<'a> {
    index: &'a Index,
    id: DirId,
    node: &'a DirNode,
}

impl<'a> Dir<'a> {
    pub fn id(&self) -> DirId {
        self.id
    }

    /// 名字。扫描起点的名字是它的完整路径。
    pub fn name(&self) -> &'a OsStr {
        os_str(
            self.index
                .name_bytes(self.node.name_off, self.node.name_len),
        )
    }

    pub fn parent(&self) -> Option<DirId> {
        (self.node.parent != NO_PARENT).then_some(DirId(self.node.parent))
    }

    /// 分类结果：类别和原因编号。
    pub fn class(&self) -> DirClass {
        (self.category(), ReasonId(self.node.reason))
    }

    pub fn category(&self) -> Category {
        Category::from_index(self.node.category as usize).unwrap_or(Category::Other)
    }

    /// [`DENIED`]、[`MOUNT`] 等标志。
    pub fn flags(&self) -> u8 {
        self.node.flags & !LISTED
    }

    /// 内容是否已经读到（扫描进行中时，还没读到的目录是空的）。
    pub fn is_listed(&self) -> bool {
        self.node.flags & LISTED != 0
    }

    /// 子树里全部文件的总大小。
    pub fn size(&self) -> u64 {
        self.node.size
    }

    /// 子树里的文件数。
    pub fn nfiles(&self) -> u64 {
        self.node.nfiles as u64
    }

    /// 子树里的目录数（不含自己）。
    pub fn ndirs(&self) -> u64 {
        self.node.ndirs as u64
    }

    /// 子树里各类别（按 [`Category`] 编号）的文件总大小。
    pub fn category_sizes(&self) -> [u64; NCLS] {
        self.node.csz
    }

    /// 直接子目录，按名字排序。
    pub fn subdirs(&self) -> impl ExactSizeIterator<Item = Dir<'a>> + 'a {
        let index = self.index;
        let start = self.node.dirs_start;
        (start..start + self.node.dirs_len).map(move |i| index.dir(DirId(i)))
    }

    /// 直接包含的文件，按名字排序。不含没有计入的硬链接。
    pub fn files(&self) -> impl Iterator<Item = File<'a>> + 'a {
        let index = self.index;
        let start = self.node.files_start;
        (start..start + self.node.files_len)
            .map(move |i| index.file(FileId(i)))
            .filter(|f| f.entry.flags & UNCOUNTED == 0)
    }

    /// 完整路径。
    pub fn path(&self) -> PathBuf {
        self.index.dir_path(self.id)
    }
}

/// 一个文件的只读视图。
#[derive(Clone, Copy)]
pub struct File<'a> {
    index: &'a Index,
    id: FileId,
    entry: &'a FileEntry,
}

impl<'a> File<'a> {
    pub fn id(&self) -> FileId {
        self.id
    }

    pub fn name(&self) -> &'a OsStr {
        os_str(
            self.index
                .name_bytes(self.entry.name_off, self.entry.name_len),
        )
    }

    /// 大小（实际占用或文件长度，取决于扫描方式）。
    pub fn size(&self) -> u64 {
        self.entry.size
    }

    pub fn category(&self) -> Category {
        Category::from_index(self.entry.category as usize).unwrap_or(Category::Other)
    }
}

/// 名字的比较规则统一由 `platform::path` 提供。
#[inline]
pub fn cmp_names(os: Os, a: &[u8], b: &[u8]) -> Ordering {
    crate::platform::path::cmp_names(os, a, b)
}

fn is_sep(os: Os, b: u8) -> bool {
    b == b'/' || (os == Os::Windows && b == b'\\')
}

fn leading_seps(os: Os, path: &[u8]) -> usize {
    path.iter().take_while(|&&b| is_sep(os, b)).count()
}

fn components(os: Os, path: &[u8]) -> impl Iterator<Item = &[u8]> {
    path.split(move |&b| is_sep(os, b))
        .filter(|c| !c.is_empty())
}

/// 由保存的字节还原名字。
fn os_str(bytes: &[u8]) -> &OsStr {
    #[cfg(unix)]
    {
        std::os::unix::ffi::OsStrExt::from_bytes(bytes)
    }
    #[cfg(not(unix))]
    {
        // SAFETY: 名字由 push_name 原样写入，或在存盘读入时与当前平台重新编码的字节校对。
        // 每个名字均为有效 UTF-8 或与本目标的 OsStr::as_encoded_bytes 完全一致。
        unsafe { OsStr::from_encoded_bytes_unchecked(bytes) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OTHER: DirClass = (Category::Other, ReasonId(0));

    fn os(s: &str) -> &OsStr {
        OsStr::new(s)
    }

    fn sorted<'a>(index: &Index, mut names: Vec<&'a str>) -> Vec<&'a str> {
        names.sort_by(|a, b| index.cmp_names(os(a), os(b)));
        names
    }

    fn dirs<'a>(names: &[&'a str]) -> Vec<NewDir<'a>> {
        names
            .iter()
            .map(|&n| NewDir {
                name: OsStr::new(n),
                class: OTHER,
                flags: 0,
            })
            .collect()
    }

    /// /r: 子目录 a、b，文件 x(10, 缓存)、y(5, 用户)；/r/a: 文件 z(100, 系统)；/r/a/deep: 空
    fn sample() -> Index {
        let mut ix = Index::new(Os::Linux, Path::new("/r"), (Category::User, ReasonId(3)));
        let names = sorted(&ix, vec!["b", "a"]);
        let r = ix
            .fill_dir(
                DirId::ROOT,
                &dirs(&names),
                &[
                    NewFile {
                        name: os("x"),
                        size: 10,
                        category: Category::Cache,
                        linked: false,
                    },
                    NewFile {
                        name: os("y"),
                        size: 5,
                        category: Category::User,
                        linked: false,
                    },
                ],
            )
            .unwrap();
        assert_eq!(r.dirs, 1..3);
        assert_eq!(r.files, 0..2);
        let a = ix.find_subdir(DirId::ROOT, os("a")).unwrap();
        let r = ix
            .fill_dir(
                a,
                &dirs(&["deep"]),
                &[NewFile {
                    name: os("z"),
                    size: 100,
                    category: Category::System,
                    linked: false,
                }],
            )
            .unwrap();
        let deep = DirId(r.dirs.start);
        ix.fill_dir(deep, &[], &[]).unwrap();
        ix
    }

    #[test]
    fn totals_roll_up_to_every_ancestor() {
        let ix = sample();
        let root = ix.root();
        assert_eq!((root.size(), root.nfiles(), root.ndirs()), (115, 3, 3));
        let cs = root.category_sizes();
        assert_eq!(
            (
                cs[Category::Cache.index()],
                cs[Category::User.index()],
                cs[Category::System.index()]
            ),
            (10, 5, 100)
        );
        let a = ix.dir(ix.find_subdir(DirId::ROOT, os("a")).unwrap());
        assert_eq!((a.size(), a.nfiles(), a.ndirs()), (100, 1, 1));
        assert_eq!(root.class(), (Category::User, ReasonId(3)));
        assert_eq!(root.name(), os("/r"));
        assert!(root.is_listed());
    }

    #[test]
    fn children_are_sorted_and_found_by_path() {
        let ix = sample();
        let names: Vec<_> = ix.root().subdirs().map(|d| d.name().to_owned()).collect();
        assert_eq!(names, ["a", "b"]);
        assert_eq!(ix.find(Path::new("/r")), Some(Found::Dir(DirId::ROOT)));
        assert_eq!(ix.find(Path::new("/r/")), Some(Found::Dir(DirId::ROOT)));
        let a = ix.find_subdir(DirId::ROOT, os("a")).unwrap();
        assert_eq!(ix.find(Path::new("/r/a")), Some(Found::Dir(a)));
        match ix.find(Path::new("/r/a/z")) {
            Some(Found::File { parent, file }) => {
                assert_eq!(parent, a);
                assert_eq!(ix.file(file).size(), 100);
                assert_eq!(ix.file_path(parent, file), PathBuf::from("/r/a/z"));
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(ix.find(Path::new("/r/a/missing")), None);
        assert_eq!(ix.find(Path::new("/rx/a")), None);
        assert_eq!(ix.find(Path::new("/")), None);
        assert_eq!(ix.find(Path::new("r/a")), None);
        assert_eq!(
            ix.dir_path(ix.find_subdir(a, os("deep")).unwrap()),
            PathBuf::from("/r/a/deep")
        );
    }

    #[test]
    fn a_directory_is_written_once() {
        let mut ix = sample();
        assert_eq!(
            ix.fill_dir(DirId::ROOT, &[], &[]),
            Err(IndexError::AlreadyListed)
        );
        ix.add_flags(DirId::ROOT, DENIED);
        assert_eq!(ix.root().flags(), DENIED);
        assert!(ix.root().is_listed());
    }

    #[test]
    fn windows_lookup_ignores_case_and_accepts_both_separators() {
        let mut ix = Index::new(Os::Windows, Path::new("C:\\"), OTHER);
        let names = sorted(&ix, vec!["Windows", "Users", "ÜBER"]);
        assert_eq!(names, ["Users", "Windows", "ÜBER"]);
        let r = ix.fill_dir(
            DirId::ROOT,
            &dirs(&names),
            &[NewFile {
                name: os("PageFile.SYS"),
                size: 7,
                category: Category::Core,
                linked: false,
            }],
        );
        let win = DirId(r.unwrap().dirs.start + 1);
        ix.fill_dir(win, &dirs(&["System32"]), &[]).unwrap();
        assert_eq!(
            ix.find(Path::new("c:\\windows\\SYSTEM32"))
                .map(|f| matches!(f, Found::Dir(_))),
            Some(true)
        );
        assert_eq!(
            ix.find(Path::new("C:/Windows/System32"))
                .map(|f| matches!(f, Found::Dir(_))),
            Some(true)
        );
        assert!(matches!(
            ix.find(Path::new("c:\\pagefile.sys")),
            Some(Found::File { .. })
        ));
        assert!(matches!(
            ix.find(Path::new("C:\\über")),
            Some(Found::Dir(_))
        ));
        assert_eq!(ix.find(Path::new("D:\\Windows")), None);
        let sys32 = ix.find_subdir(win, os("system32")).unwrap();
        assert_eq!(ix.dir_path(sys32).as_os_str(), os("C:\\Windows\\System32"));
    }

    #[test]
    fn linux_lookup_is_case_sensitive() {
        let mut ix = Index::new(Os::Linux, Path::new("/"), OTHER);
        let names = sorted(&ix, vec!["usr", "Usr"]);
        ix.fill_dir(DirId::ROOT, &dirs(&names), &[]).unwrap();
        let a = ix.find(Path::new("/usr"));
        let b = ix.find(Path::new("/Usr"));
        assert!(a.is_some() && b.is_some() && a != b);
        assert_eq!(ix.find(Path::new("/USR")), None);
        assert_eq!(ix.dir_path(DirId(1)), PathBuf::from("/Usr"));
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_names_round_trip() {
        use std::os::unix::ffi::OsStrExt;
        let mut ix = Index::new(Os::Linux, Path::new("/t"), OTHER);
        let raw = OsStr::from_bytes(b"caf\xe9");
        ix.fill_dir(
            DirId::ROOT,
            &[],
            &[NewFile {
                name: raw,
                size: 1,
                category: Category::Other,
                linked: false,
            }],
        )
        .unwrap();
        let mut p = PathBuf::from("/t");
        p.push(raw);
        assert!(matches!(ix.find(&p), Some(Found::File { .. })));
        assert_eq!(ix.file(FileId(0)).name(), raw);
    }

    fn linked(name: &str, size: u64) -> NewFile<'_> {
        NewFile {
            name: os(name),
            size,
            category: Category::User,
            linked: true,
        }
    }

    fn file_names(ix: &Index, dir: DirId) -> Vec<OsString> {
        ix.dir(dir).files().map(|f| f.name().to_owned()).collect()
    }

    #[test]
    fn linked_files_count_only_when_chosen() {
        // /r: 子目录 a；文件 l1（硬链接）、n。/r/a: 文件 l2（同一个文件的另一个链接）
        let mut ix = Index::new(Os::Linux, Path::new("/r"), OTHER);
        let plain = NewFile {
            name: os("n"),
            size: 5,
            category: Category::Cache,
            linked: false,
        };
        let root = ix
            .fill_dir(DirId::ROOT, &dirs(&["a"]), &[linked("l1", 50), plain])
            .unwrap();
        let a = DirId(root.dirs.start);
        let sub = ix.fill_dir(a, &[], &[linked("l2", 50)]).unwrap();
        let (l1, l2) = (FileId(root.files.start), FileId(sub.files.start));
        // 写入后先不计入，也看不到
        assert_eq!((ix.root().size(), ix.root().nfiles()), (5, 1));
        assert_eq!(file_names(&ix, DirId::ROOT), ["n"]);
        assert_eq!(ix.find(Path::new("/r/l1")), None);
        assert_eq!(ix.file_count(), 3);
        // 根目录里的 l1 先于子目录里的 l2
        assert_eq!(
            ix.cmp_file_order((DirId::ROOT, l1), (a, l2)),
            Ordering::Less
        );
        assert_eq!(
            ix.cmp_file_order((a, l2), (DirId::ROOT, l1)),
            Ordering::Greater
        );
        assert!(ix.count_file(DirId::ROOT, l1));
        assert!(!ix.count_file(DirId::ROOT, l1));
        assert_eq!((ix.root().size(), ix.root().nfiles()), (55, 2));
        assert_eq!(ix.root().category_sizes()[Category::User.index()], 50);
        assert_eq!(ix.dir(a).size(), 0);
        assert_eq!(file_names(&ix, DirId::ROOT), ["l1", "n"]);
        assert!(matches!(
            ix.find(Path::new("/r/l1")),
            Some(Found::File { .. })
        ));
        assert_eq!(ix.find(Path::new("/r/a/l2")), None);
    }

    #[test]
    fn file_order_is_depth_first_by_name() {
        // /r: 子目录 a、b，文件 z；/r/a: 子目录 c，文件 y；/r/a/c: 文件 x；/r/b: 文件 w
        let mut ix = Index::new(Os::Linux, Path::new("/r"), OTHER);
        let r = ix
            .fill_dir(DirId::ROOT, &dirs(&["a", "b"]), &[linked("z", 1)])
            .unwrap();
        let (a, b) = (DirId(r.dirs.start), DirId(r.dirs.start + 1));
        let z = (DirId::ROOT, FileId(r.files.start));
        let ra = ix.fill_dir(a, &dirs(&["c"]), &[linked("y", 1)]).unwrap();
        let c = DirId(ra.dirs.start);
        let y = (a, FileId(ra.files.start));
        let x = (
            c,
            FileId(ix.fill_dir(c, &[], &[linked("x", 1)]).unwrap().files.start),
        );
        let w = (
            b,
            FileId(ix.fill_dir(b, &[], &[linked("w", 1)]).unwrap().files.start),
        );
        let mut all = vec![w, x, y, z];
        all.sort_by(|p, q| ix.cmp_file_order(*p, *q));
        assert_eq!(all, [z, y, x, w]);
    }

    #[test]
    fn entries_stay_compact() {
        assert_eq!(std::mem::size_of::<FileEntry>(), 16);
        assert_eq!(std::mem::size_of::<DirNode>(), 104);
    }
}
