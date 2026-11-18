//! 每目录的旧版文件投影：只选前 150 项，对被聚合的文件不构造完整路径或查询进程哈希表。

use std::borrow::Cow;
use std::cmp::Reverse;
use std::collections::{HashMap, hash_map::Entry};
use std::path::Path;

use crate::index::{DirId, FileId, Index};
use crate::platform::Os;
use crate::platform::path::cmp_path_key;
use crate::procinfo::Snapshot;

use super::{Aggregate, KEEP_FILES};

/// 初始进程名单按父目录整理；文件名排好序，每个目录只查询一次目录哈希表。
#[derive(Default)]
pub(super) struct RetainedFiles {
    directories: HashMap<Vec<u8>, Vec<Vec<u8>>>,
    #[cfg(test)]
    pub directory_lookups: std::cell::Cell<usize>,
}

impl RetainedFiles {
    pub fn new(snapshot: &Snapshot, os: Os) -> Self {
        let mut directories: HashMap<Vec<u8>, Vec<Vec<u8>>> = HashMap::new();
        for (path, _) in snapshot.files() {
            let key = path_key(os, path);
            let Some(last) = key.iter().rposition(|&byte| byte == os.sep() as u8) else {
                continue;
            };
            if last + 1 == key.len() {
                continue;
            }
            let parent = key[..last.max(root_len(os, &key))].to_vec();
            directories
                .entry(parent)
                .or_default()
                .push(key[last + 1..].to_vec());
        }
        for names in directories.values_mut() {
            names.sort_unstable();
            names.dedup();
        }
        Self {
            directories,
            #[cfg(test)]
            directory_lookups: std::cell::Cell::new(0),
        }
    }

    fn names(&self, os: Os, path: &Path) -> &[Vec<u8>] {
        #[cfg(test)]
        self.directory_lookups.set(self.directory_lookups.get() + 1);
        if self.directories.is_empty() {
            return &[];
        }
        self.directories
            .get(&path_key(os, path))
            .map_or(&[], Vec::as_slice)
    }
}

// 与 procinfo 的路径键一致：不裁剪文件名空白、不解析符号链接，Windows 只折叠大小写及分隔符。
// 此处只处理初始候选路径和每目录一次的路径，绝不在普通文件循环里构造路径键。
fn path_key(os: Os, path: &Path) -> Vec<u8> {
    let mut key = crate::platform::path::path_key(os, path.as_os_str().as_encoded_bytes());
    let root = root_len(os, &key);
    while key.len() > root && key.last() == Some(&(os.sep() as u8)) {
        key.pop();
    }
    key
}

fn root_len(os: Os, path: &[u8]) -> usize {
    if os != Os::Windows {
        return usize::from(path.starts_with(b"/"));
    }
    if path.get(1..3) == Some(b":\\") {
        return 3;
    }
    if path.starts_with(b"\\\\") {
        let mut separators = path
            .iter()
            .enumerate()
            .skip(2)
            .filter(|(_, byte)| **byte == b'\\');
        if separators.next().is_some() {
            return separators.next().map_or(path.len(), |(i, _)| i);
        }
        return path.len();
    }
    usize::from(path.starts_with(b"\\"))
}

#[derive(Clone)]
pub(super) struct Projection {
    pub files: Vec<FileId>,
    pub rest: Aggregate,
    grouped: bool,
}

impl Projection {
    fn build(index: &Index, id: DirId, path: &Path, retained: &RetainedFiles) -> Self {
        let mut rest = Aggregate::default();
        let mut files: Vec<_> = index
            .dir(id)
            .files()
            .map(|file| {
                rest.add(file.size(), 1, file.category());
                (Reverse(file.size()), file.id())
            })
            .collect();
        if files.len() <= KEEP_FILES {
            return Self {
                files: files.into_iter().map(|(_, id)| id).collect(),
                rest: Aggregate::default(),
                grouped: false,
            };
        }

        // FileId 在同一目录里就是原来的名字次序；完整排序键没有平局，局部选择与稳定全排序完全一致。
        files.select_nth_unstable(KEEP_FILES);
        let (largest, others) = files.split_at_mut(KEEP_FILES);
        largest.sort_unstable();
        let mut selected: Vec<_> = largest.iter().map(|&(_, id)| id).collect();
        let names = retained.names(index.os(), path);
        if !names.is_empty() {
            let mut extra: Vec<_> = others
                .iter()
                .copied()
                .filter(|&(_, id)| {
                    let name = index.file(id).name().as_encoded_bytes();
                    // 只对预先整理好的名字做无分配比较，不拼完整路径、不分配路径键、不查每文件哈希表。
                    names
                        .binary_search_by(|key| cmp_path_key(index.os(), key, name))
                        .is_ok()
                })
                .collect();
            extra.sort_unstable();
            selected.extend(extra.into_iter().map(|(_, id)| id));
        }
        for &id in &selected {
            let file = index.file(id);
            rest.remove(file.size(), file.category());
        }
        Self {
            files: selected,
            rest,
            grouped: true,
        }
    }
}

/// 只缓存完成扫描中真正发生 150 项聚合的目录，避免再保存一份全部文件编号。
#[derive(Default)]
pub(super) struct ProjectionCache {
    directories: HashMap<DirId, Projection>,
    #[cfg(test)]
    pub builds: usize,
}

impl ProjectionCache {
    pub fn get<'a>(
        &'a mut self,
        index: &Index,
        id: DirId,
        path: &Path,
        retained: &RetainedFiles,
        finished: bool,
    ) -> Cow<'a, Projection> {
        if !finished {
            // 进行中的目录发布和最后的硬链接计入都可能改变前 150 项；不缓存中间投影。
            self.directories.clear();
            #[cfg(test)]
            {
                self.builds += 1;
            }
            return Cow::Owned(Projection::build(index, id, path, retained));
        }
        match self.directories.entry(id) {
            Entry::Occupied(entry) => Cow::Borrowed(entry.into_mut()),
            Entry::Vacant(entry) => {
                #[cfg(test)]
                {
                    self.builds += 1;
                }
                let projection = Projection::build(index, id, path, retained);
                if projection.grouped {
                    Cow::Borrowed(entry.insert(projection))
                } else {
                    Cow::Owned(projection)
                }
            }
        }
    }
}
