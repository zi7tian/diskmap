//! 给界面的完整索引查询。只给返回的条目构造路径并查询进程快照。

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::path::Path;
use std::sync::Arc;

use serde::Serialize;

use crate::index::{Dir, DirId, File, FileId, Found, Index};
use crate::model::Category;
use crate::platform::{Os, path};
use crate::procinfo::Snapshot;

use super::{Aggregate, RetainedFiles, TreeOptions, ViewCache, area_threshold, join, place_rest};

const LARGEST_LIMIT: usize = 10;
const SEARCH_LIMIT: usize = 200;
const GROUP_PAGE: usize = 100;

#[derive(Serialize)]
pub(crate) struct Item {
    #[serde(rename = "type")]
    kind: &'static str,
    name: String,
    path: String,
    parent: String,
    size: u64,
    cls: u8,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Entry {
    Dir(DirId),
    File(DirId, FileId),
}

// 同大小时用索引编号固定当前索引中的次序，翻页排序使用同一个键。
type Ranked = (Reverse<u64>, Entry);

fn ranked_file(parent: DirId, file: File<'_>) -> Ranked {
    (Reverse(file.size()), Entry::File(parent, file.id()))
}

fn ranked_dir(dir: Dir<'_>) -> Ranked {
    (Reverse(dir.size()), Entry::Dir(dir.id()))
}

fn item(index: &Index, snapshot: &Snapshot, (Reverse(size), entry): Ranked) -> Item {
    let (kind, parent, name, category) = match entry {
        Entry::Dir(id) => {
            let dir = index.dir(id);
            (
                "dir",
                dir.parent().expect("child directory"),
                dir.name(),
                dir.category(),
            )
        }
        Entry::File(parent, id) => {
            let file = index.file(id);
            ("file", parent, file.name(), file.category())
        }
    };
    let parent = index.dir_path(parent);
    let path = join(index, &parent, name);
    let category = if kind == "file" && snapshot.lookup(&path).is_some() {
        Category::Running
    } else {
        category
    };
    Item {
        kind,
        name: name.to_string_lossy().into_owned(),
        path: path.to_string_lossy().into_owned(),
        parent: parent.to_string_lossy().into_owned(),
        size,
        cls: category as u8,
    }
}

fn directory(index: &Index, path: &Path) -> Option<DirId> {
    match index.find(path)? {
        Found::Dir(dir) => Some(dir),
        Found::File { .. } => None,
    }
}

fn keep_best(heap: &mut BinaryHeap<Ranked>, limit: usize, candidate: Ranked) {
    if heap.len() < limit {
        heap.push(candidate);
    } else if heap.peek().is_some_and(|worst| candidate < *worst) {
        *heap.peek_mut().expect("nonempty heap") = candidate;
    }
}

pub(crate) fn largest(index: &Index, snapshot: &Snapshot, path: &Path) -> Option<Vec<Item>> {
    let root = directory(index, path)?;
    let mut heap: BinaryHeap<Ranked> = BinaryHeap::with_capacity(LARGEST_LIMIT);
    let mut stack = vec![index.dir(root)];
    while let Some(dir) = stack.pop() {
        if heap.len() == LARGEST_LIMIT
            && heap
                .peek()
                .is_some_and(|&(Reverse(size), _)| dir.size() < size)
        {
            continue;
        }
        // Dir::files 排除未计入的硬链接，和目录的字节统计口径相同。
        for file in dir.files() {
            keep_best(&mut heap, LARGEST_LIMIT, ranked_file(dir.id(), file));
        }
        stack.extend(dir.subdirs());
    }
    Some(
        heap.into_sorted_vec()
            .into_iter()
            .map(|entry| item(index, snapshot, entry))
            .collect(),
    )
}

#[derive(Serialize)]
pub(crate) struct SearchResults {
    items: Vec<Item>,
    total: usize,
}

pub(crate) fn search(
    index: &Index,
    snapshot: &Snapshot,
    path: &Path,
    query: &str,
) -> Option<SearchResults> {
    let root = directory(index, path)?;
    if query.is_empty() {
        return Some(SearchResults {
            items: Vec::new(),
            total: 0,
        });
    }
    // 搜索总是不区分大小写，复用索引在 Windows 上的一对一简单大写映射。
    let key = path::name_key(Os::Windows, query.as_bytes());
    let mut folded = Vec::new();
    let mut matches = |name: &std::ffi::OsStr| {
        folded.clear();
        path::extend_name_key(Os::Windows, name.as_encoded_bytes(), &mut folded);
        match key.as_slice() {
            [byte] => folded.contains(byte),
            _ => folded.windows(key.len()).any(|part| part == key),
        }
    };
    let mut heap = BinaryHeap::with_capacity(SEARCH_LIMIT);
    let mut total = 0;
    let mut add = |entry| {
        total += 1;
        keep_best(&mut heap, SEARCH_LIMIT, entry);
    };
    let mut check = |dir: Dir<'_>| {
        if dir.id() != root && matches(dir.name()) {
            add(ranked_dir(dir));
        }
        for file in dir.files() {
            if matches(file.name()) {
                add(ranked_file(dir.id(), file));
            }
        }
    };
    if root == DirId::ROOT {
        for dir in index.dirs() {
            check(dir);
        }
    } else {
        let mut stack = vec![index.dir(root)];
        while let Some(dir) = stack.pop() {
            check(dir);
            stack.extend(dir.subdirs());
        }
    }
    Some(SearchResults {
        items: heap
            .into_sorted_vec()
            .into_iter()
            .map(|entry| item(index, snapshot, entry))
            .collect(),
        total,
    })
}

pub(crate) struct GroupOptions {
    pub frac: f64,
    pub view_size: f64,
    pub parent_files: u64,
    pub parent_dirs: u64,
    pub ordinal: usize,
    pub offset: usize,
    pub size: f64,
    pub count: u64,
    pub cls: u8,
}

pub(crate) struct GroupQuery<'a> {
    pub path: &'a Path,
    pub parent: &'a Path,
    pub options: GroupOptions,
}

#[derive(Serialize)]
pub(crate) struct GroupPage {
    kind: &'static str,
    total: u64,
    items: Vec<Item>,
    next_offset: Option<usize>,
}

impl ViewCache {
    pub(crate) fn group_page(
        &mut self,
        index: &Index,
        snapshot: &Arc<Snapshot>,
        query: GroupQuery<'_>,
        finished: bool,
    ) -> Option<GroupPage> {
        let root = directory(index, query.path)?;
        let parent = directory(index, query.parent)?;
        let options = query.options;
        let dir = index.dir(parent);
        // 索引单次扫描只追加。父目录计数不变时，块成员不会因扫描了别处而变化。
        if options.view_size > index.dir(root).size() as f64
            || options.parent_files != dir.nfiles()
            || options.parent_dirs != dir.ndirs()
        {
            return None;
        }
        let min_size = area_threshold(options.view_size, options.frac);
        // 确认父目录在该视图内，且从视图起点到父目录的每一层都通过了面积阈值。
        let mut ancestor = parent;
        let max_depth = TreeOptions::default().max_depth;
        for depth in 0..max_depth {
            if ancestor == root {
                break;
            }
            let dir = index.dir(ancestor);
            if dir.size() < min_size || depth + 1 == max_depth {
                return None;
            }
            ancestor = dir.parent()?;
        }
        let parent_path = dir.path();
        let retained = self
            .retained_snapshot
            .get_or_insert_with(|| Arc::clone(snapshot));
        let retained_files = self
            .retained_files
            .get_or_insert_with(|| RetainedFiles::new(retained, index.os()));
        let projection =
            self.projections
                .get(index, parent, &parent_path, retained_files, finished);

        let mut small = Aggregate::default();
        let mut items = Vec::new();
        for child in dir.subdirs().filter(|child| child.size() < min_size) {
            small.add(child.size(), 1, child.category());
            items.push(ranked_dir(child));
        }
        for file in projection
            .files
            .iter()
            .map(|&id| index.file(id))
            .filter(|file| file.size() < min_size)
        {
            small.add(file.size(), 1, file.category());
            items.push(ranked_file(parent, file));
        }
        let separate = place_rest(&projection.rest, &mut small, min_size);
        // /api/tree 先插入 rest，再插入 small，按大小稳定排序。即使两块的字段全相同也能区分。
        let mut groups = [
            ("rest", &projection.rest, separate),
            ("small", &small, small.count > 0),
        ];
        if small.size > projection.rest.size {
            groups.swap(0, 1);
        }
        let (kind, group, _) = groups
            .into_iter()
            .filter(|&(_, _, exists)| exists)
            .nth(options.ordinal)?;
        // 扫描中如果当前块与点击时的摘要不同，不把另一块的成员混到详情或下一页。
        if group.size as f64 != options.size
            || group.count != options.count
            || group.category() as u8 != options.cls
        {
            return None;
        }
        if options.offset as u64 >= group.count {
            return Some(GroupPage {
                kind,
                total: group.count,
                items: Vec::new(),
                next_offset: None,
            });
        }
        if kind == "rest" {
            items.clear();
        }
        if projection.rest.count > 0 && (kind == "rest" || !separate) {
            // 只缓存保留文件的编号。其余文件按索引顺序迭代，用归并跳过保留项。
            let mut retained_ids = projection.files.clone();
            retained_ids.sort_unstable();
            let mut retained_ids = retained_ids.into_iter().peekable();
            items.extend(
                dir.files()
                    .filter(|file| retained_ids.next_if_eq(&file.id()).is_none())
                    .map(|file| ranked_file(parent, file)),
            );
        }
        debug_assert_eq!(items.len() as u64, group.count);
        let end = options.offset.saturating_add(GROUP_PAGE).min(items.len());
        if end < items.len() {
            items.select_nth_unstable(end);
            items.truncate(end);
        }
        if options.offset != 0 {
            items.select_nth_unstable(options.offset - 1);
        }
        let page = &mut items[options.offset..];
        page.sort_unstable();
        Some(GroupPage {
            kind,
            total: group.count,
            items: page
                .iter()
                .map(|&entry| item(index, snapshot, entry))
                .collect(),
            next_offset: ((end as u64) < group.count).then_some(end),
        })
    }
}

#[cfg(test)]
mod tests;
