//! 在用字节增量缓存：每个进程快照构建一次路径树，目录发布后只匹配它自己的候选文件。

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use crate::index::{DirId, FileId, Found, Index};
use crate::model::NCLS;
use crate::platform::Os;
use crate::procinfo::Snapshot;

#[derive(Default)]
struct Candidates {
    dirs: HashMap<Vec<u8>, Candidates>,
    files: Vec<PathBuf>,
}

#[derive(Default)]
pub(super) struct RunningCache {
    snapshot: Option<Arc<Snapshot>>,
    pending: VecDeque<(DirId, Candidates)>,
    thresholds: HashMap<DirId, Option<(Reverse<u64>, FileId)>>,
    sizes: HashMap<DirId, [u64; NCLS]>,
    counted: HashSet<FileId>,
    finished: bool,
    #[cfg(test)]
    pub file_lookups: usize,
}

impl RunningCache {
    pub fn update(
        &mut self,
        index: &Index,
        snapshot: Arc<Snapshot>,
        retained: &Snapshot,
        finished: bool,
    ) {
        if !self
            .snapshot
            .as_ref()
            .is_some_and(|previous| Arc::ptr_eq(previous, &snapshot))
            || (finished && !self.finished)
        {
            // 硬链接到最后才计入，也可能挤出原先的第 150 个文件；结束转换时统一重建一次。
            #[cfg(test)]
            let lookups = self.file_lookups;
            *self = Self::default();
            #[cfg(test)]
            {
                self.file_lookups = lookups;
            }
            let os = index.os();
            let root = components(os, index.root_path().as_os_str().as_encoded_bytes());
            let mut candidates = Candidates::default();
            for (path, _) in snapshot.files() {
                let mut parts = components(os, path.as_os_str().as_encoded_bytes());
                if parts.len() <= root.len() || !parts.starts_with(&root) {
                    continue;
                }
                parts.pop();
                let mut dir = &mut candidates;
                for part in parts.into_iter().skip(root.len()) {
                    dir = dir.dirs.entry(part).or_default();
                }
                dir.files.push(path.to_owned());
            }
            self.pending.push_back((DirId::ROOT, candidates));
            self.snapshot = Some(snapshot);
        }
        let mut waiting = VecDeque::new();
        while let Some((id, mut candidates)) = self.pending.pop_front() {
            let dir = index.dir(id);
            if !dir.is_listed() {
                waiting.push_back((id, candidates));
                continue;
            }
            for path in candidates.files {
                #[cfg(test)]
                {
                    self.file_lookups += 1;
                }
                if let Some(Found::File { parent, file }) = index.find(&path) {
                    self.add(index, retained, parent, file);
                }
            }
            for child in dir.subdirs() {
                let key = component(index.os(), child.name().as_encoded_bytes());
                if let Some(candidates) = candidates.dirs.remove(&key) {
                    self.pending.push_back((child.id(), candidates));
                }
            }
        }
        self.pending = waiting;
        if finished {
            self.pending.clear();
        }
        self.finished = finished;
    }

    fn add(&mut self, index: &Index, retained: &Snapshot, parent: DirId, file: FileId) {
        let threshold = self.thresholds.entry(parent).or_insert_with(|| {
            let mut files: Vec<_> = index
                .dir(parent)
                .files()
                .map(|file| (Reverse(file.size()), file.id()))
                .collect();
            (files.len() > super::KEEP_FILES)
                .then(|| *files.select_nth_unstable(super::KEEP_FILES - 1).1)
        });
        let entry = index.file(file);
        if threshold.is_some_and(|threshold| (Reverse(entry.size()), file) > threshold)
            && retained.lookup(index.file_path(parent, file)).is_none()
        {
            return;
        }
        // 同一条目可能通过不同分隔符或路径拼写命中；按索引编号保证字节数只转移一次。
        if !self.counted.insert(file) {
            return;
        }
        let file = entry;
        let mut dir = Some(parent);
        while let Some(id) = dir {
            self.sizes.entry(id).or_default()[file.category().index()] += file.size();
            dir = index.dir(id).parent();
        }
    }

    pub fn sizes(&self, id: DirId) -> [u64; NCLS] {
        self.sizes.get(&id).copied().unwrap_or_default()
    }
}

fn components(os: Os, path: &[u8]) -> Vec<Vec<u8>> {
    path.split(|&b| b == b'/' || (os == Os::Windows && b == b'\\'))
        .filter(|part| !part.is_empty())
        .map(|part| component(os, part))
        .collect()
}

fn component(os: Os, name: &[u8]) -> Vec<u8> {
    crate::platform::path::name_key(os, name)
}
