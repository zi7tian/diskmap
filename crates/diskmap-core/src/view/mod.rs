//! 给界面的树数据和详情；字段、合并阈值与旧版 `scanner.py`、`server.py` 一致。
//!
//! 完整索引仍保存所有文件。这里只为旧界面复现每目录保留最大的 150 个文件，另保留扫描开始时的在用文件，
//! 再把小于显示阈值的项目合并。正在使用的字节数按进程快照缓存，并随目录发布增量更新。

#[cfg(any(feature = "server", test))]
pub(crate) mod catalog;
mod projection;
#[cfg(test)]
mod reference;
mod running;
mod typed;

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Value, json};

use crate::classify::Classifier;
use crate::index::{DENIED, Dir, DirId, Found, Index};
use crate::model::{Category, NCLS};
use crate::pkgdb::PkgDb;
use crate::procinfo::Snapshot;
use projection::{ProjectionCache, RetainedFiles};
use running::RunningCache;
use typed::{HttpTree, TreeData, TreeNode};

const KEEP_FILES: usize = 150;

/// 与旧版 `Scan.view` 一样，预算只在展开每个目录之前检查，可以超出一个目录的直接子项数。
#[derive(Clone, Copy, Debug)]
pub struct TreeOptions {
    pub frac: f64,
    pub max_nodes: usize,
    pub max_depth: usize,
}

impl Default for TreeOptions {
    fn default() -> Self {
        Self {
            frac: 1.0 / 40_000.0,
            max_nodes: 60_000,
            max_depth: 48,
        }
    }
}

/// 一次扫描的界面缓存。开始新的扫描时必须新建，不能在不同索引之间复用。
/// 应用应使用 [`Self::with_snapshot`]；`new` / `default` 在首次请求时固定额外保留名单。
#[derive(Default)]
pub struct ViewCache {
    running: RunningCache,
    retained_snapshot: Option<Arc<Snapshot>>,
    retained_files: Option<RetainedFiles>,
    projections: ProjectionCache,
    #[cfg(test)]
    rendered_file_paths: usize,
}

impl ViewCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// 固定扫描开始时的在用文件名单；与旧版一样，刷新进程只改变着色，不改变 150 个文件以外的保留名单。
    pub fn with_snapshot(snapshot: Arc<Snapshot>) -> Self {
        Self {
            retained_snapshot: Some(snapshot),
            ..Self::default()
        }
    }

    /// 返回 `path`、`totals` 和 `tree`；应用层补上扫描的 `root` 和 `state`。
    /// `finished` 表示扫描已结束（包括取消和失败），此时重建一次在用命中，补入延迟计数的硬链接。
    pub fn tree(
        &mut self,
        index: &Index,
        snapshot: Arc<Snapshot>,
        path: &Path,
        frac: f64,
        pkgdb: Option<&PkgDb>,
        finished: bool,
    ) -> Option<Value> {
        self.tree_with_options(
            index,
            snapshot,
            path,
            TreeOptions {
                frac,
                ..TreeOptions::default()
            },
            pkgdb,
            finished,
        )
    }

    pub fn tree_with_options(
        &mut self,
        index: &Index,
        snapshot: Arc<Snapshot>,
        path: &Path,
        options: TreeOptions,
        pkgdb: Option<&PkgDb>,
        finished: bool,
    ) -> Option<Value> {
        let data = self.build(index, snapshot, path, options, pkgdb, finished)?;
        Some(serde_json::to_value(data).expect("tree serialization"))
    }

    /// 完整 HTTP 树响应直接转为字节；不经过每节点 Value，也不分配静态字段名。
    pub fn tree_bytes(
        &mut self,
        index: &Index,
        snapshot: Arc<Snapshot>,
        path: &Path,
        frac: f64,
        pkgdb: Option<&PkgDb>,
        state: &str,
    ) -> Option<Vec<u8>> {
        let data = self.build(
            index,
            snapshot,
            path,
            TreeOptions {
                frac,
                ..TreeOptions::default()
            },
            pkgdb,
            state != "scanning",
        )?;
        Some(
            serde_json::to_vec(&HttpTree {
                path: &data.path,
                root: index.root_path().to_string_lossy(),
                state,
                totals: &data.totals,
                tree: &data.tree,
            })
            .expect("tree serialization"),
        )
    }

    fn build<'a>(
        &mut self,
        index: &'a Index,
        snapshot: Arc<Snapshot>,
        path: &Path,
        options: TreeOptions,
        pkgdb: Option<&'a PkgDb>,
        finished: bool,
    ) -> Option<TreeData<'a>> {
        let Found::Dir(id) = index.find(path)? else {
            return None;
        };
        let retained = self
            .retained_snapshot
            .get_or_insert_with(|| Arc::clone(&snapshot));
        self.running
            .update(index, Arc::clone(&snapshot), retained, finished);
        let retained_files = self
            .retained_files
            .get_or_insert_with(|| RetainedFiles::new(retained, index.os()));
        #[cfg(test)]
        {
            self.rendered_file_paths = 0;
        }
        let dir = index.dir(id);
        let path = dir.path();
        let mut totals = dir.category_sizes();
        for (category, size) in self.running.sizes(id).into_iter().enumerate() {
            totals[category] -= size;
            totals[Category::Running.index()] += size;
        }
        Some(TreeData {
            path: path.to_string_lossy().into_owned(),
            totals,
            tree: Render {
                index,
                snapshot: &snapshot,
                retained: retained_files,
                projections: &mut self.projections,
                pkgdb,
                finished,
                #[cfg(test)]
                rendered_file_paths: &mut self.rendered_file_paths,
            }
            .tree(id, path, options),
        })
    }
}

/// 返回详情的静态字段和进程信息。文件时间、权限和所有者由应用层在索引锁之外读取。
/// 与完整索引一致：即使文件在树中被合并，仍可按路径查询详情。
pub fn info(
    index: &Index,
    snapshot: &Snapshot,
    classifier: &Classifier,
    pkgdb: Option<&PkgDb>,
    path: &Path,
) -> Option<Value> {
    match index.find(path)? {
        Found::Dir(id) => {
            let dir = index.dir(id);
            let path = dir.path();
            Some(json!({
                "path": path.to_string_lossy(),
                "name": dir.name().to_string_lossy(),
                "type": "dir",
                "size": dir.size(),
                "nfiles": dir.nfiles(),
                "ndirs": dir.ndirs(),
                "cls": dir.category() as u8,
                "reason": classifier.reason(dir.class().1),
                "running": snapshot.dir_count(&path),
                "denied": dir.flags() & DENIED != 0,
            }))
        }
        Found::File { parent, file } => {
            let dir = index.dir(parent);
            let file = index.file(file);
            let path = index.file_path(parent, file.id());
            let (class, reason) =
                classifier.explain_file(dir.class(), &dir.path(), file.name(), &path);
            let (procs, nprocs) = snapshot.details(&path);
            Some(json!({
                "path": path.to_string_lossy(),
                "name": file.name().to_string_lossy(),
                "type": "file",
                "size": file.size(),
                "cls": (if nprocs == 0 { class } else { Category::Running }) as u8,
                "static_cls": class as u8,
                "reason": reason,
                "pkg": pkgdb.and_then(|db| db.owner(&path)),
                "procs": procs,
                "nprocs": nprocs,
            }))
        }
    }
}

struct Node<'a> {
    value: TreeNode<'a>,
    children: Option<Vec<usize>>,
}

struct Render<'a, 'work> {
    index: &'a Index,
    snapshot: &'work Snapshot,
    retained: &'work RetainedFiles,
    projections: &'work mut ProjectionCache,
    pkgdb: Option<&'a PkgDb>,
    finished: bool,
    #[cfg(test)]
    rendered_file_paths: &'work mut usize,
}

impl<'a> Render<'a, '_> {
    fn tree(&mut self, id: DirId, path: PathBuf, options: TreeOptions) -> TreeNode<'a> {
        let index = self.index;
        let snapshot = self.snapshot;
        let min_size = area_threshold(index.dir(id).size() as f64, options.frac);
        let mut budget = options.max_nodes;
        let mut nodes = vec![Node {
            value: directory(index.dir(id), &path, snapshot),
            children: None,
        }];
        let mut queue = VecDeque::from([(id, 0, 0, path)]);
        while let Some((id, output, depth, parent_path)) = queue.pop_front() {
            let dir = index.dir(id);
            if depth >= options.max_depth || budget == 0 {
                if dir.subdirs().len() > 0 || dir.files().next().is_some() {
                    nodes[output].value.more = Some(1);
                }
                continue;
            }
            let mut children = Vec::new();
            let mut small = Aggregate::default();
            for child in dir.subdirs() {
                if child.size() >= min_size {
                    let path = join(index, &parent_path, child.name());
                    let child_output = push_node(&mut nodes, directory(child, &path, snapshot));
                    children.push(child_output);
                    queue.push_back((child.id(), child_output, depth + 1, path));
                } else {
                    small.add(child.size(), 1, child.category());
                }
            }
            let projection =
                self.projections
                    .get(index, id, &parent_path, self.retained, self.finished);
            for &file_id in &projection.files {
                let file = index.file(file_id);
                if file.size() < min_size {
                    small.add(file.size(), 1, file.category());
                    continue;
                }
                // 只有单独输出的文件才构造路径并查询进程和软件包。
                #[cfg(test)]
                {
                    *self.rendered_file_paths += 1;
                }
                let path = join(index, &parent_path, file.name());
                let mut value = TreeNode {
                    name: file.name().to_string_lossy(),
                    size: file.size(),
                    category: file.category() as u8,
                    kind: "f",
                    ..TreeNode::default()
                };
                if snapshot.lookup(&path).is_some() {
                    value.static_category = Some(file.category() as u8);
                    value.category = Category::Running as u8;
                    value.processes = Some(snapshot.summary(&path));
                }
                value.package = self
                    .pkgdb
                    .and_then(|db| db.owner(&path))
                    .filter(|owner| !owner.is_empty());
                children.push(push_node(&mut nodes, value));
            }
            if place_rest(&projection.rest, &mut small, min_size) {
                children.push(push_node(&mut nodes, projection.rest.node()));
            }
            if small.size > 0 || small.count > 0 {
                children.push(push_node(&mut nodes, small.node()));
            }
            children.sort_by_key(|&child| std::cmp::Reverse(nodes[child].value.size));
            budget = budget.saturating_sub(children.len());
            nodes[output].children = Some(children);
        }
        // 广度优先控制预算，倒序组装 JSON；不在索引上递归，避免深目录导致栈溢出。
        for i in (0..nodes.len()).rev() {
            if let Some(children) = nodes[i].children.take() {
                let values = children
                    .into_iter()
                    .map(|child| std::mem::take(&mut nodes[child].value))
                    .collect();
                nodes[i].value.children = Some(values);
            }
        }
        std::mem::take(&mut nodes[0].value)
    }
}

fn area_threshold(size: f64, frac: f64) -> u64 {
    ((size * frac) as u64).max(1)
}

/// 返回是否单独显示 150 项之外的文件；阈值以下时把它们作为一块并入 small。
fn place_rest(rest: &Aggregate, small: &mut Aggregate, min_size: u64) -> bool {
    if rest.count == 0 {
        return false;
    }
    if rest.size >= min_size {
        return true;
    }
    // 旧版先聚合，再参与面积阈值：类别票数是聚合块大小 + 1，而不是逐文件的票数。
    small.add(rest.size, rest.count, rest.category());
    false
}

fn push_node<'a>(nodes: &mut Vec<Node<'a>>, value: TreeNode<'a>) -> usize {
    let id = nodes.len();
    nodes.push(Node {
        value,
        children: None,
    });
    id
}

fn directory<'a>(dir: Dir<'a>, path: &Path, snapshot: &Snapshot) -> TreeNode<'a> {
    let count = snapshot.dir_count(path);
    TreeNode {
        name: dir.name().to_string_lossy(),
        size: dir.size(),
        category: dir.category() as u8,
        kind: "d",
        nfiles: Some(dir.nfiles()),
        ndirs: Some(dir.ndirs()),
        flags: (dir.flags() != 0).then_some(dir.flags()),
        running: (count != 0).then_some(count),
        ..TreeNode::default()
    }
}

fn join(index: &Index, parent: &Path, name: &std::ffi::OsStr) -> PathBuf {
    let mut path = std::ffi::OsString::with_capacity(parent.as_os_str().len() + name.len() + 1);
    path.push(parent);
    let bytes = path.as_encoded_bytes();
    if !bytes
        .last()
        .is_some_and(|&b| b == b'/' || (index.os() == crate::platform::Os::Windows && b == b'\\'))
    {
        path.push(if index.os() == crate::platform::Os::Windows {
            "\\"
        } else {
            "/"
        });
    }
    path.push(name);
    PathBuf::from(path)
}

#[derive(Clone, Default)]
struct Aggregate {
    size: u64,
    count: u64,
    votes: [u128; NCLS],
}

impl Aggregate {
    fn add(&mut self, size: u64, count: u64, category: Category) {
        self.size += size;
        self.count += count;
        self.votes[category.index()] += u128::from(size) + 1;
    }

    fn remove(&mut self, size: u64, category: Category) {
        self.size -= size;
        self.count -= 1;
        self.votes[category.index()] -= u128::from(size) + 1;
    }

    fn category(&self) -> Category {
        // Python 的 max 在相等时保留最前面的类别。
        let mut best = 0;
        for i in 1..NCLS {
            if self.votes[i] > self.votes[best] {
                best = i;
            }
        }
        Category::from_index(best).unwrap_or(Category::Other)
    }

    fn node(&self) -> TreeNode<'static> {
        TreeNode {
            name: "".into(),
            size: self.size,
            category: self.category() as u8,
            kind: "a",
            count: Some(self.count),
            ..TreeNode::default()
        }
    }

    #[cfg(test)]
    fn value(&self) -> Value {
        json!({"n": "", "s": self.size, "c": self.category() as u8, "t": "a", "k": self.count})
    }
}

#[cfg(test)]
mod tests;
