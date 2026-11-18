//! 查询逻辑和旧版分析投影；完整索引不受 150 文件显示上限影响。

use super::{AnalysisError, Result, ScanSnapshot, Snapshot, human, metadata};
use crate::classify::{CLASSES, Classifier};
use crate::i18n::Text;
use crate::index::{DENIED, DirId, FileId, Found, Index, MOUNT};
use crate::model::{Category, NCLS};
use crate::platform;
use crate::scan::{Progress, ScanState};
use serde_json::{Value, json};
use std::cell::RefCell;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

const KEEP_FILES: usize = 150;

fn translated(zh: &str, en: &str) -> String {
    Text::owned(zh.to_owned(), en.to_owned()).text().to_owned()
}
fn err(zh: &str, en: &str) -> AnalysisError {
    AnalysisError(Text::owned(zh.to_owned(), en.to_owned()))
}
fn no_scan() -> AnalysisError {
    err(
        "还没有扫描结果。请先调用 diskmap_scan 扫描一个文件夹或磁盘。",
        "Nothing has been scanned yet. Call diskmap_scan on a folder or drive first.",
    )
}
fn round1(value: f64) -> f64 {
    format!("{value:.1}").parse().unwrap_or(0.0)
}
fn pct(a: u64, b: u64) -> f64 {
    if b == 0 {
        0.0
    } else {
        round1(a as f64 / b as f64 * 100.0)
    }
}
fn display(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}
fn connected(s: &Snapshot) -> bool {
    s.last_seen != 0.0 && s.now - s.last_seen < 15.0
}
fn category(out: &mut Value, cat: Category) {
    out["category"] = json!(cat.key());
    out["category_label"] = json!(CLASSES[cat.index()].label);
}
fn safety(out: &mut Value, cat: Category, in_use: usize) {
    let (level, advice) = match cat {
        Category::Core => (
            "danger",
            Text::new(
                "系统核心：不要删除或修改，否则可能无法开机。",
                "System core: do not delete or modify, or the system may not boot.",
            ),
        ),
        Category::Running => (
            "in_use",
            Text::new(
                "正被程序使用：如需删除，先关闭相关程序。",
                "In use by a program: close that program before deleting.",
            ),
        ),
        Category::System => (
            "uninstall_only",
            Text::new(
                "系统或已安装程序的文件：应通过包管理器/系统设置卸载对应软件，不要手动删除。",
                "Belongs to the system or an installed program: uninstall the software (package manager / system settings) instead of deleting files by hand.",
            ),
        ),
        Category::Config => (
            "caution",
            Text::new(
                "配置文件：删除会丢失相应程序的设置，确认不再需要再删。",
                "Settings: deleting loses that program's settings. Only delete if no longer needed.",
            ),
        ),
        Category::User => (
            "ask_user",
            Text::new(
                "用户的个人文件：是否删除只能由用户决定，请先询问。",
                "Personal files: only the user can decide whether to delete them. Ask first.",
            ),
        ),
        Category::Cache => (
            "safe_to_clean",
            Text::new(
                "缓存/临时文件：通常可以安全清理，程序会按需重新生成。",
                "Cache / temporary files: usually safe to clean; programs recreate them as needed.",
            ),
        ),
        Category::Other => (
            "caution",
            Text::new(
                "应用数据：删除前确认对应程序已不再使用。",
                "Application data: make sure the program that owns it is no longer used before deleting.",
            ),
        ),
    };
    let mut advice = advice.text().to_owned();
    if in_use != 0 && cat != Category::Running {
        advice.push_str(&translated(
            &format!("其中有 {in_use} 个文件正被程序使用，清理前先关闭相关程序。"),
            &format!(" {in_use} files inside are in use; close those programs before cleaning."),
        ));
    }
    out["safety"] = json!(level);
    out["advice"] = json!(advice);
}
fn note(progress: &Progress) -> Option<String> {
    match progress.state {
        ScanState::Scanning => Some(translated(
            "扫描仍在进行中，以下数据还不完整。",
            "The scan is still running; these numbers are incomplete.",
        )),
        ScanState::Cancelled => Some(translated(
            "扫描被中途停止，以下数据不完整。",
            "The scan was stopped; these numbers are incomplete.",
        )),
        _ => None,
    }
}
fn add_note(out: &mut Value, progress: &Progress) {
    if let Some(n) = note(progress) {
        out["note"] = json!(n);
    }
}

pub(super) fn status(s: &Snapshot, progress: Option<&Progress>) -> Value {
    let mut out = json!({"version":s.version,"os":s.os.key(),"admin":s.admin,"ready":s.ready,
        "window_connected":connected(s),"processes":{"total":s.processes.nprocs,
        "in_use_files":s.processes.file_count(),"visible_processes":s.processes.visible}});
    let owned;
    let progress = if let Some(p) = progress {
        p
    } else {
        match &s.scan {
            None => {
                out["scan"] = Value::Null;
                out["hint"] = json!(Text::new(
                    "还没有扫描。调用 diskmap_scan 开始，例如扫描整个系统盘。",
                    "Nothing scanned yet. Call diskmap_scan to start, for example on the system drive."
                ));
                return out;
            }
            Some(ScanSnapshot::Live(scan)) => {
                owned = scan.progress();
                &owned
            }
            Some(ScanSnapshot::Frozen { progress, .. }) => progress,
        }
    };
    out["scan"] = json!({"root":display(&progress.root),"state":progress.state.key(),
        "files":progress.files,"dirs":progress.dirs,"bytes":progress.bytes,"size":human(progress.bytes),
        "elapsed_seconds":round1(progress.elapsed.as_secs_f64()),
        "finished_at":progress.finished_at.and_then(|t| metadata::local_time(t,true)),
        "current":if progress.state==ScanState::Scanning {Some(progress.current.as_deref().map(display).unwrap_or_default())} else {None},
        "denied_dirs":progress.denied,"disk_used_bytes":if progress.fs_used==0 {None} else {Some(progress.fs_used)},
        "size_mode":if progress.apparent {"apparent"} else {"allocated"},"includes_other_partitions":progress.cross_fs});
    if progress.denied != 0 {
        let n = progress.denied;
        out["scan"]["denied_note"] = json!(Text::owned(
            format!(
                "有 {n} 个文件夹因权限不足没有统计；以管理员/root 身份运行 DiskMap 可以看到全部。"
            ),
            format!(
                "{n} folders could not be read (no permission) and are not counted; run DiskMap as administrator/root to see everything."
            )
        ));
    }
    if let Some(ui) = &s.ui_state {
        out["user_view"] = json!({"view":ui.view.as_deref().map(display),"selected":ui.selected.as_deref().map(display)});
    }
    out
}

pub(super) fn with_context<T>(
    s: &Snapshot,
    f: impl FnOnce(&Context<'_>) -> Result<T>,
) -> Result<T> {
    match s.scan.as_ref().ok_or_else(no_scan)? {
        ScanSnapshot::Live(scan) => {
            let progress = scan.progress();
            let index = scan.index();
            f(&Context::new(s, &index, &progress, scan.classifier()))
        }
        ScanSnapshot::Frozen {
            index,
            progress,
            classifier,
        } => f(&Context::new(s, index, progress, classifier)),
    }
}

#[derive(Clone, Copy)]
enum Entry {
    File(FileId),
    Group {
        bytes: u64,
        count: usize,
        category: Category,
    },
}
#[derive(Clone, Copy)]
enum Item {
    Dir(DirId),
    File(DirId, Entry),
}
#[derive(Clone)]
struct Hit {
    path: PathBuf,
    parent: DirId,
    file: FileId,
}

struct Projection {
    entries: Vec<Entry>,
    // 小目录全部可见；大目录的成员集合避免大量在用文件逐个线性查找。
    retained: Option<HashSet<FileId>>,
}

pub(super) struct Context<'a> {
    source: &'a Snapshot,
    index: &'a Index,
    pub(super) progress: &'a Progress,
    classifier: &'a Classifier,
    projection: RefCell<HashMap<DirId, Rc<Projection>>>,
    hits: RefCell<Option<Vec<Hit>>>,
}
impl<'a> Context<'a> {
    fn new(
        source: &'a Snapshot,
        index: &'a Index,
        progress: &'a Progress,
        classifier: &'a Classifier,
    ) -> Self {
        Self {
            source,
            index,
            progress,
            classifier,
            projection: RefCell::default(),
            hits: RefCell::default(),
        }
    }
    fn normalize(&self, path: &Path) -> PathBuf {
        if self.index.os() == platform::Os::current()
            && let Some(p) = path.to_str()
        {
            platform::norm_path(p)
        } else {
            path.to_path_buf()
        }
    }
    fn find(&self, path: &Path) -> Option<Found> {
        let found = self.index.find(&self.normalize(path))?;
        if let Found::File { parent, file } = found
            && !self.visible(parent, file)
        {
            return None;
        }
        Some(found)
    }
    fn not_found(&self, path: &Path) -> AnalysisError {
        let p = display(path);
        let root = display(self.index.root_path());
        err(
            &format!("扫描结果中没有这个路径：{p}（当前扫描范围是 {root}）"),
            &format!("This path is not in the scan results: {p} (the scan covers {root})"),
        )
    }
    fn dir(&self, path: Option<&Path>) -> Result<DirId> {
        let path = path
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(self.index.root_path());
        match self.find(path) {
            Some(Found::Dir(id)) => Ok(id),
            Some(Found::File { .. }) => {
                let p = display(path);
                Err(err(
                    &format!("这是一个文件而不是文件夹：{p}。查看文件请用 diskmap_explain。"),
                    &format!("This is a file, not a folder: {p}. Use diskmap_explain for files."),
                ))
            }
            None => Err(self.not_found(path)),
        }
    }
    fn visible(&self, parent: DirId, file: FileId) -> bool {
        self.files(parent)
            .retained
            .as_ref()
            .is_none_or(|files| files.contains(&file))
    }
    fn files(&self, id: DirId) -> Rc<Projection> {
        if let Some(v) = self.projection.borrow().get(&id) {
            return v.clone();
        }
        let mut files: Vec<_> = self.index.dir(id).files().collect();
        let grouped = files.len() > KEEP_FILES;
        let entries: Vec<_> = if !grouped {
            files.iter().map(|f| Entry::File(f.id())).collect()
        } else {
            files.sort_by_key(|f| Reverse(f.size()));
            let mut out: Vec<_> = files[..KEEP_FILES]
                .iter()
                .map(|f| Entry::File(f.id()))
                .collect();
            let mut bytes = 0;
            let mut count = 0;
            let mut weights = [0u128; NCLS];
            for f in &files[KEEP_FILES..] {
                if self.source.retained_processes.file_count() != 0
                    && self
                        .source
                        .retained_processes
                        .lookup(self.index.file_path(id, f.id()))
                        .is_some()
                {
                    out.push(Entry::File(f.id()));
                } else {
                    bytes += f.size();
                    count += 1;
                    weights[f.category().index()] += u128::from(f.size()) + 1;
                }
            }
            if count > 0 {
                let cat = Category::ALL
                    .into_iter()
                    .max_by_key(|c| (weights[c.index()], Reverse(c.index())))
                    .unwrap();
                out.push(Entry::Group {
                    bytes,
                    count,
                    category: cat,
                });
            }
            out
        };
        let retained = grouped.then(|| {
            entries
                .iter()
                .filter_map(|e| match e {
                    Entry::File(file) => Some(*file),
                    Entry::Group { .. } => None,
                })
                .collect()
        });
        let result = Rc::new(Projection { entries, retained });
        self.projection.borrow_mut().insert(id, Rc::clone(&result));
        result
    }
    fn entry_size(&self, e: Entry) -> u64 {
        match e {
            Entry::File(f) => self.index.file(f).size(),
            Entry::Group { bytes, .. } => bytes,
        }
    }
    fn entry_cat(&self, e: Entry) -> Category {
        match e {
            Entry::File(f) => self.index.file(f).category(),
            Entry::Group { category, .. } => category,
        }
    }
    fn item_size(&self, i: Item) -> u64 {
        match i {
            Item::Dir(d) => self.index.dir(d).size(),
            Item::File(_, e) => self.entry_size(e),
        }
    }
    fn entry(&self, item: Item, parent_size: Option<u64>) -> Value {
        let mut out = match item {
            Item::Dir(id) => {
                let d = self.index.dir(id);
                let path = d.path();
                let in_use = self.source.processes.dir_count(&path);
                let mut out = json!({"path":display(&path),"type":"dir","bytes":d.size(),"size":human(d.size()),
                    "reason":self.classifier.reason(d.class().1),"files":d.nfiles(),"dirs":d.ndirs()});
                category(&mut out, d.category());
                safety(&mut out, d.category(), in_use);
                if in_use > 0 {
                    out["in_use_files"] = json!(in_use);
                }
                if d.flags() & DENIED != 0 {
                    out["denied"] = json!(true);
                }
                if d.flags() & MOUNT != 0 {
                    out["mount_point"] = json!(true);
                }
                out
            }
            Item::File(parent, Entry::File(id)) => {
                let d = self.index.dir(parent);
                let f = self.index.file(id);
                let path = self.index.file_path(parent, id);
                let used = self.source.processes.summary(&path);
                let static_cat = f.category();
                let cat = if used.is_some() {
                    Category::Running
                } else {
                    static_cat
                };
                let (_, reason) =
                    self.classifier
                        .explain_file(d.class(), &d.path(), f.name(), &path);
                let mut out = json!({"path":display(&path),"type":"file","bytes":f.size(),"size":human(f.size()),"reason":reason});
                category(&mut out, cat);
                safety(&mut out, cat, 0);
                if let Some(used) = used {
                    out["used_by"] = json!(used);
                    out["static_category"] = json!(static_cat.key());
                }
                if let Some(pkg) = self.source.pkgdb.owner(&path).filter(|p| !p.is_empty()) {
                    out["package"] = json!(pkg);
                }
                out
            }
            Item::File(
                parent,
                Entry::Group {
                    bytes,
                    count,
                    category: cat,
                },
            ) => {
                let path = display(&self.index.dir_path(parent));
                let mut out = json!({"path":path,"type":"group","bytes":bytes,"count":count,"size":human(bytes),
                    "note":Text::owned(format!("{path} 中 {count} 个较小的文件合并统计"),format!("{count} smaller files in {path}, counted together"))});
                category(&mut out, cat);
                out
            }
        };
        if let Some(p) = parent_size.filter(|&p| p > 0) {
            out["percent_of_parent"] = json!(pct(self.item_size(item), p));
        }
        out
    }
    fn children(&self, d: DirId) -> Vec<Item> {
        self.index
            .dir(d)
            .subdirs()
            .map(|c| Item::Dir(c.id()))
            .chain(
                self.files(d)
                    .entries
                    .iter()
                    .copied()
                    .map(|f| Item::File(d, f)),
            )
            .collect()
    }
    fn descendant(&self, mut parent: DirId, root: DirId) -> bool {
        loop {
            if parent == root {
                return true;
            }
            match self.index.dir(parent).parent() {
                Some(p) => parent = p,
                None => return false,
            }
        }
    }
    fn running(&self, root: DirId) -> Vec<Hit> {
        if self.hits.borrow().is_none() {
            let mut hits: Vec<_> = self
                .source
                .processes
                .files()
                // 进程提供的是系统原始路径，不能当用户输入 trim；否则尾空白文件会错配。
                .filter_map(|(path, _)| match self.index.find(path) {
                    Some(Found::File { parent, file }) if self.visible(parent, file) => Some(Hit {
                        path: path.to_path_buf(),
                        parent,
                        file,
                    }),
                    _ => None,
                })
                .collect();
            // Snapshot 不保留路径登记顺序，以路径打破平局，保证重复请求一致。
            hits.sort_by(|a, b| {
                self.index
                    .file(b.file)
                    .size()
                    .cmp(&self.index.file(a.file).size())
                    .then_with(|| a.path.cmp(&b.path))
            });
            // 分隔符等路径别名可能命中同一条目，同一索引文件最多叠加一次。
            let mut seen = HashSet::new();
            hits.retain(|h| seen.insert((h.parent, h.file)));
            *self.hits.borrow_mut() = Some(hits);
        }
        self.hits
            .borrow()
            .as_ref()
            .unwrap()
            .iter()
            .filter(|h| self.descendant(h.parent, root))
            .cloned()
            .collect()
    }
    pub(super) fn list_dir(&self, path: Option<&Path>, limit: i64) -> Result<Value> {
        let id = self.dir(path)?;
        let d = self.index.dir(id);
        let mut items = self.children(id);
        items.sort_by_key(|&i| Reverse(self.item_size(i)));
        let total = items.len();
        let out: Vec<_> = items
            .into_iter()
            .take(limit.clamp(1, 500) as usize)
            .map(|i| self.entry(i, Some(d.size())))
            .collect();
        let mut result = json!({"path":display(&d.path()),"bytes":d.size(),"size":human(d.size()),"total_items":total,"shown":out.len(),"items":out});
        add_note(&mut result, self.progress);
        Ok(result)
    }
    pub(super) fn largest_files(
        &self,
        path: Option<&Path>,
        limit: i64,
        cat: Option<&str>,
    ) -> Result<Value> {
        let root = self.dir(path)?;
        let limit = limit.clamp(1, 200) as usize;
        let filter = match cat {
            Some(c) => Some(Category::from_key(c).ok_or_else(|| {
                let keys = Category::ALL.map(Category::key).join(", ");
                err(
                    &format!("未知类别：{c}，可选 {keys}"),
                    &format!("Unknown category: {c}. Choose one of {keys}"),
                )
            })?),
            None => None,
        };
        let files: Vec<_> = if filter == Some(Category::Running) {
            self.running(root)
                .into_iter()
                .take(limit)
                .map(|h| self.entry(Item::File(h.parent, Entry::File(h.file)), None))
                .collect()
        } else {
            // 有界堆只保存 limit 个候选，保持旧版 (大小, 遍历序号) 的逆序规则。
            let mut heap = BinaryHeap::new();
            let mut seq = 0usize;
            let mut stack = vec![root];
            while let Some(d) = stack.pop() {
                stack.extend(self.index.dir(d).subdirs().map(|c| c.id()));
                for e in self.files(d).entries.iter().copied() {
                    if let Entry::File(id) = e {
                        let f = self.index.file(id);
                        if filter.is_none_or(|c| c == f.category()) {
                            heap.push(Reverse((f.size(), seq, d, id)));
                            if heap.len() > limit {
                                heap.pop();
                            }
                        }
                        seq += 1;
                    }
                }
            }
            let mut chosen = heap.into_iter().map(|Reverse(v)| v).collect::<Vec<_>>();
            chosen.sort_unstable_by(|a, b| b.cmp(a));
            chosen
                .into_iter()
                .map(|(_, _, d, f)| self.entry(Item::File(d, Entry::File(f)), None))
                .collect()
        };
        let mut result =
            json!({"path":display(&self.index.dir_path(root)),"category":cat,"files":files});
        add_note(&mut result, self.progress);
        Ok(result)
    }
    pub(super) fn cleanup(&self, path: Option<&Path>, limit: i64, min_bytes: u64) -> Result<Value> {
        let root = self.dir(path)?;
        let d = self.index.dir(root);
        let mut candidates = Vec::new();
        if d.category() == Category::Cache {
            candidates = self.children(root);
        } else {
            let mut stack = vec![root];
            while let Some(id) = stack.pop() {
                let x = self.index.dir(id);
                for c in x.subdirs() {
                    if c.category() == Category::Cache {
                        candidates.push(Item::Dir(c.id()));
                    } else {
                        stack.push(c.id());
                    }
                }
                if x.category() != Category::Cache {
                    candidates.extend(
                        self.files(id)
                            .entries
                            .iter()
                            .copied()
                            .filter(|&e| self.entry_cat(e) == Category::Cache)
                            .map(|e| Item::File(id, e)),
                    );
                }
            }
        }
        let total: u64 = candidates.iter().map(|&i| self.item_size(i)).sum();
        candidates.retain(|&i| self.item_size(i) >= min_bytes);
        candidates.sort_by_key(|&i| Reverse(self.item_size(i)));
        let items: Vec<_> = candidates
            .into_iter()
            .take(limit.clamp(1, 200) as usize)
            .map(|i| self.entry(i, None))
            .collect();
        let mut text = translated(
            "只是建议：DiskMap 不会删除任何文件。删除前请让用户确认；标记为 in_use 的项目需先关闭相关程序。",
            "Suggestions only: DiskMap never deletes files. Get the user's confirmation before deleting; close the programs using items marked in_use first.",
        );
        if let Some(n) = note(self.progress) {
            text = format!("{n} {text}");
        }
        Ok(
            json!({"path":display(&d.path()),"reclaimable_bytes":total,"reclaimable_size":human(total),"candidates_shown":items.len(),"candidates":items,"note":text}),
        )
    }
    pub(super) fn summary(&self, path: Option<&Path>, top: i64) -> Result<Value> {
        let id = self.dir(path)?;
        let d = self.index.dir(id);
        let path = d.path();
        let top = top.clamp(1, 50);
        let running = self.running(id);
        let mut totals = d.category_sizes();
        for h in &running {
            let f = self.index.file(h.file);
            totals[f.category().index()] -= f.size();
            totals[Category::Running.index()] += f.size();
        }
        let mut cats: Vec<_> = Category::ALL
            .into_iter()
            .filter(|c| totals[c.index()] > 0)
            .collect();
        cats.sort_by_key(|c| Reverse(totals[c.index()]));
        let by_cat: Vec<_> = cats
            .into_iter()
            .map(|c| {
                let n = totals[c.index()];
                let mut v = json!({"bytes":n,"size":human(n),"percent":pct(n,d.size().max(1))});
                category(&mut v, c);
                v
            })
            .collect();
        let clean = self.cleanup(Some(&path), top.min(10), 1 << 20)?;
        let running_bytes: u64 = running.iter().map(|h| self.index.file(h.file).size()).sum();
        let largest:Vec<_>=running.iter().take(5).map(|h|json!({"path":display(&h.path),"size":human(self.index.file(h.file).size()),"used_by":self.source.processes.summary(&h.path)})).collect();
        let mut result = json!({"path":display(&path),"bytes":d.size(),"size":human(d.size()),"files":d.nfiles(),"dirs":d.ndirs(),
            "reason":self.classifier.reason(d.class().1),"by_category":by_cat,
            "largest_children":self.list_dir(Some(&path),top)?["items"],"largest_files":self.largest_files(Some(&path),top,None)?["files"],
            "cleanup":{"reclaimable_bytes":clean["reclaimable_bytes"],"reclaimable_size":clean["reclaimable_size"],"top_candidates":clean["candidates"]},
            "in_use":{"files":running.len(),"bytes":running_bytes,"size":human(running_bytes),"largest":largest}});
        category(&mut result, d.category());
        add_note(&mut result, self.progress);
        Ok(result)
    }
    pub(super) fn explain(&self, path: &Path) -> Result<Value> {
        let found = self.find(path).ok_or_else(|| self.not_found(path))?;
        let (target, mut out, denied) = match found {
            Found::Dir(id) => {
                let d = self.index.dir(id);
                let target = d.path();
                let busy = self.source.processes.dir_count(&target);
                let mut out = json!({"path":display(&target),"type":"dir","bytes":d.size(),"size":human(d.size()),
                    "reason":self.classifier.reason(d.class().1),"files":d.nfiles(),"dirs":d.ndirs(),
                    "largest_children":self.list_dir(Some(&target),8)?["items"]});
                category(&mut out, d.category());
                safety(&mut out, d.category(), busy);
                if busy > 0 {
                    out["in_use_files"] = json!(busy);
                }
                if let Some(parent) = d.parent() {
                    out["percent_of_parent"] = json!(pct(d.size(), self.index.dir(parent).size()));
                }
                if d.flags() & MOUNT != 0
                    && let Some(m) = platform::mounts::read_mounts().into_iter().find(|m| {
                        platform::path::cmp_paths(
                            self.index.os(),
                            m.path.as_os_str().as_encoded_bytes(),
                            target.as_os_str().as_encoded_bytes(),
                        )
                        .is_eq()
                    })
                {
                    out["mount"] = json!({"fstype":m.fstype,"source":m.source});
                }
                (target, out, d.flags() & DENIED != 0)
            }
            Found::File { parent, file } => {
                let d = self.index.dir(parent);
                let f = self.index.file(file);
                let target = self.index.file_path(parent, file);
                let (static_cat, reason) =
                    self.classifier
                        .explain_file(d.class(), &d.path(), f.name(), &target);
                let (details, total) = self.source.processes.details(&target);
                let cat = if total > 0 {
                    Category::Running
                } else {
                    static_cat
                };
                let mut out = json!({"path":display(&target),"type":"file","bytes":f.size(),"size":human(f.size()),"reason":reason});
                category(&mut out, cat);
                safety(&mut out, cat, 0);
                if cat != static_cat {
                    out["static_category"] = json!(static_cat.key());
                }
                if let Some(pkg) = self.source.pkgdb.owner(&target).filter(|p| !p.is_empty()) {
                    out["package"] = json!(pkg);
                }
                if total > 0 {
                    out["used_by"] = json!(details.into_iter().take(10).collect::<Vec<_>>());
                    out["used_by_count"] = json!(total);
                }
                (target, out, false)
            }
        };
        metadata::enrich(&mut out, &target, matches!(found, Found::File { .. }));
        if denied {
            out["note"] = json!(Text::new(
                "没有权限读取此文件夹，大小未计入。",
                "No permission to read this folder; its size is not counted."
            ));
        }
        Ok(out)
    }
    pub(super) fn show(&self, path: &Path) -> Result<(PathBuf, Value)> {
        let target = match self.find(path) {
            Some(Found::Dir(d)) => self.index.dir_path(d),
            Some(Found::File { parent, file }) => self.index.file_path(parent, file),
            None => {
                let p = display(path);
                return Err(err(
                    &format!("扫描结果中没有这个路径：{p}"),
                    &format!("This path is not in the scan results: {p}"),
                ));
            }
        };
        let connected = connected(self.source);
        let message = if connected {
            Text::new(
                "已让 DiskMap 窗口跳转并高亮该项目。",
                "The DiskMap window now shows and highlights this item.",
            )
        } else {
            Text::new(
                "已记录，但 DiskMap 窗口当前似乎没有打开，用户打开后才能看到。",
                "Noted, but the DiskMap window does not seem to be open; the user will see it after opening it.",
            )
        };
        let out = json!({"ok":true,"path":display(&target),"window_connected":connected,"message":message});
        Ok((target, out))
    }
    pub(super) fn report_json(&self, path: Option<&Path>, top: i64) -> Result<Value> {
        Ok(
            json!({"status":status(self.source,Some(self.progress)),"summary":self.summary(path,top)?,"cleanup":self.cleanup(path,top,1<<20)?}),
        )
    }
}

pub(super) fn user_view(s: &Snapshot) -> Result<Value> {
    let ui=s.ui_state.as_ref().filter(|_|s.last_seen!=0.0 && s.now-s.last_seen<=30.0).ok_or_else(||err(
        "用户没有打开 DiskMap 窗口（或窗口里还没有扫描结果），无法获取用户正在看的内容。",
        "The DiskMap window is not open (or has no scan yet), so what the user is looking at is unknown."))?;
    let mut out = json!({"view":ui.view.as_deref().map(display),"selected":ui.selected.as_deref().map(display),
        "highlighted_category":ui.pinned,"updated_seconds_ago":round1(s.now-ui.time)});
    // 无扫描或路径已失效时仍返回界面上报的基础字段。
    let _ = with_context(s, |c| {
        if let Some(path) = ui.view.as_deref().filter(|p| !p.as_os_str().is_empty())
            && let Ok(v) = c.summary(Some(path), 5)
        {
            out["view_summary"] = v;
        }
        if let Some(path) = ui.selected.as_deref().filter(|p| !p.as_os_str().is_empty())
            && let Ok(v) = c.explain(path)
        {
            out["selected_detail"] = v;
        }
        Ok(())
    });
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rounding_and_units_follow_python() {
        assert_eq!(round1(2.65), 2.6);
        assert_eq!(round1(2.55), 2.5);
        assert_eq!(pct(1, 16), 6.2);
        assert_eq!(human(1023), "1023 B");
        assert_eq!(human(1024), "1.00 KB");
        assert_eq!(human(10 * 1024), "10.0 KB");
        assert_eq!(human(100 * 1024), "100 KB");
        assert_eq!(human(u64::MAX), "16384 PB");
    }
}
