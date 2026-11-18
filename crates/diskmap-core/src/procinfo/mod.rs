//! 正在使用的文件，对应旧版 `procinfo.py`。句柄可共享，查询使用一次发布的不可变快照。
//! 默认最多等待 12 秒；超时发布部分数据，未返回的系统调用不持有快照锁，也不启动重复工作线程。

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
mod path;
#[cfg(test)]
mod tests;
#[cfg(windows)]
mod windows;
#[cfg(any(windows, test))]
mod windows_rules;
#[cfg(windows)]
pub use windows::diagnose_windows_process;

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, RwLock, mpsc};
use std::time::{Duration, Instant};

use crate::i18n::{Lang, Text};
use crate::platform::Os;

/// 默认也是最长的采集等待时间。
pub const DEFAULT_BUDGET: Duration = Duration::from_secs(12);

/// 同一 PID 对同一文件有多种用途时保留优先级最高的一种。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Kind {
    Exe,
    Lib,
    Open,
    Swap,
}
impl Kind {
    pub const fn key(self) -> &'static str {
        match self {
            Self::Exe => "exe",
            Self::Lib => "lib",
            Self::Open => "open",
            Self::Swap => "swap",
        }
    }
    pub const fn label(self) -> Text {
        match self {
            Self::Exe => Text::new("正在执行", "executing"),
            Self::Lib => Text::new("已加载到内存", "loaded into memory"),
            Self::Open => Text::new("已打开", "open"),
            Self::Swap => Text::new("交换文件", "swap file"),
        }
    }
    const fn rank(self) -> u8 {
        match self {
            Self::Exe | Self::Swap => 0,
            Self::Lib => 1,
            Self::Open => 2,
        }
    }
}

/// 普通程序名保持原样；交换文件使用双语的“系统 / system”。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Usage {
    pub comm: Text,
    pub kind: Kind,
}
impl Usage {
    pub fn new(comm: impl Into<String>, kind: Kind) -> Self {
        let comm = comm.into();
        Self {
            comm: Text::owned(comm.clone(), comm),
            kind,
        }
    }
}

/// PID → 用途，同时保留首次登记顺序（旧版摘要同计数时按此顺序排列）。
#[derive(Clone, Debug, Default)]
pub struct ProcessMap {
    entries: Vec<(u32, Usage)>,
    positions: HashMap<u32, usize>,
}
impl ProcessMap {
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn get(&self, pid: u32) -> Option<&Usage> {
        self.positions.get(&pid).map(|&i| &self.entries[i].1)
    }
    pub fn iter(&self) -> impl Iterator<Item = (u32, &Usage)> {
        self.entries.iter().map(|(pid, u)| (*pid, u))
    }
    /// 同 PID 同优先级保留先登记的一项，不改变摘要顺序。
    pub fn insert(&mut self, pid: u32, usage: Usage) {
        if let Some(&i) = self.positions.get(&pid) {
            if usage.kind.rank() < self.entries[i].1.kind.rank() {
                self.entries[i].1 = usage;
            }
        } else {
            self.positions.insert(pid, self.entries.len());
            self.entries.push((pid, usage));
        }
    }
}

/// 详情行的 JSON 字段和翻译与旧版相同。
#[derive(Clone, Debug, serde::Serialize)]
pub struct Detail {
    pub pid: u32,
    pub comm: Text,
    pub kind: Text,
}

#[derive(Clone, Debug)]
struct FileUsage {
    path: PathBuf,
    processes: ProcessMap,
}
#[derive(Debug, Default)]
struct Data {
    files: HashMap<Vec<u8>, FileUsage>,
    dirs: HashMap<Vec<u8>, usize>,
    nprocs: usize,
    visible: usize,
}
impl Data {
    fn entry(&mut self, os: Os, path: &Path) -> &mut ProcessMap {
        let key = path::key(os, path);
        let entry = self.files.entry(key.clone()).or_insert_with(|| {
            // 每个规范路径只累计一次，不按 PID 数累计；全程不访问磁盘。
            for parent in path::parents(os, &key) {
                *self.dirs.entry(parent.to_vec()).or_default() += 1;
            }
            FileUsage {
                path: path.to_owned(),
                processes: ProcessMap::default(),
            }
        });
        &mut entry.processes
    }
    fn insert(&mut self, os: Os, path: &Path, pid: u32, usage: Usage) {
        self.entry(os, path).insert(pid, usage);
    }
}

/// 一次刷新的一致快照。time 为秒，stamp 每次安装递增，与旧版含义相同。
#[derive(Clone, Debug)]
pub struct Snapshot {
    data: Arc<Data>,
    os: Os,
    pub nprocs: usize,
    pub visible: usize,
    pub time: f64,
    pub stamp: u64,
    pub error: Option<Text>,
    /// 到期发布的结果可能不完整；权限不足与旧版一样安静跳过。
    pub timed_out: bool,
}
impl Snapshot {
    fn empty(os: Os) -> Self {
        Self {
            data: Arc::default(),
            os,
            nprocs: 0,
            visible: 0,
            time: 0.0,
            stamp: 0,
            error: None,
            timed_out: false,
        }
    }
    pub fn file_count(&self) -> usize {
        self.data.files.len()
    }
    /// 路径保留第一次出现时的拼写，内部查找键按平台规范化。
    pub fn files(&self) -> impl Iterator<Item = (&Path, &ProcessMap)> {
        self.data
            .files
            .values()
            .map(|f| (f.path.as_path(), &f.processes))
    }
    pub fn lookup(&self, path: impl AsRef<Path>) -> Option<&ProcessMap> {
        self.data
            .files
            .get(&path::key(self.os, path.as_ref()))
            .map(|f| &f.processes)
    }
    pub fn dir_count(&self, path: impl AsRef<Path>) -> usize {
        self.data
            .dirs
            .get(&path::key(self.os, path.as_ref()))
            .copied()
            .unwrap_or(0)
    }
    pub fn summary(&self, path: impl AsRef<Path>) -> Option<Text> {
        self.summary_with_limit(path, 4)
    }
    pub fn summary_with_limit(&self, path: impl AsRef<Path>, limit: usize) -> Option<Text> {
        let slot = self.lookup(path)?;
        if slot.is_empty() {
            return None;
        }
        let mut counts: Vec<(&Text, usize)> = Vec::new();
        let mut positions = HashMap::new();
        for (_, usage) in slot.iter() {
            // 旧版 T 以中文为底层 str；普通程序恰好也叫“系统”时，应与交换文件合并。
            let i = *positions.entry(&usage.comm.zh).or_insert_with(|| {
                counts.push((&usage.comm, 0));
                counts.len() - 1
            });
            counts[i].1 += 1;
        }
        counts.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
        let render = |lang| {
            let sep = if lang == Lang::Zh { "、" } else { ", " };
            let mut result = counts
                .iter()
                .take(limit)
                .map(|(comm, n)| {
                    if *n > 1 {
                        format!("{} ×{n}", comm.get(lang))
                    } else {
                        comm.get(lang).to_owned()
                    }
                })
                .collect::<Vec<_>>()
                .join(sep);
            if counts.len() > limit {
                result.push_str(&if lang == Lang::Zh {
                    format!(" 等 {} 个程序", counts.len())
                } else {
                    format!(" and others ({} programs)", counts.len())
                });
            }
            result
        };
        Some(Text::owned(render(Lang::Zh), render(Lang::En)))
    }
    pub fn details(&self, path: impl AsRef<Path>) -> (Vec<Detail>, usize) {
        self.details_with_limit(path, 40)
    }
    pub fn details_with_limit(&self, path: impl AsRef<Path>, limit: usize) -> (Vec<Detail>, usize) {
        let Some(slot) = self.lookup(path) else {
            return (Vec::new(), 0);
        };
        let mut rows: Vec<_> = slot.iter().collect();
        // 旧版 T 的底层是中文 str；排序不随请求语言改变。
        rows.sort_by(|(a_pid, a), (b_pid, b)| {
            (a.kind.rank(), &a.comm.zh, a_pid).cmp(&(b.kind.rank(), &b.comm.zh, b_pid))
        });
        let total = rows.len();
        (
            rows.into_iter()
                .take(limit)
                .map(|(pid, u)| Detail {
                    pid,
                    comm: u.comm.clone(),
                    kind: u.kind.label(),
                })
                .collect(),
            total,
        )
    }
}

struct Inner {
    os: Os,
    snapshot: RwLock<Arc<Snapshot>>,
    refreshing: AtomicBool,
    collecting: Arc<AtomicBool>,
}
/// 可共享的刷新句柄；读取通过 snapshot，不在查询期间持有后台采集锁。
#[derive(Clone)]
pub struct ProcInfo {
    inner: Arc<Inner>,
}
impl Default for ProcInfo {
    fn default() -> Self {
        Self::new()
    }
}
impl ProcInfo {
    pub fn new() -> Self {
        Self::with_os(Os::current())
    }
    /// 注入路径规则用于离线数据或测试；实际采集仍使用宿主系统接口。
    pub fn with_os(os: Os) -> Self {
        Self {
            inner: Arc::new(Inner {
                os,
                snapshot: RwLock::new(Arc::new(Snapshot::empty(os))),
                refreshing: AtomicBool::new(false),
                collecting: Arc::new(AtomicBool::new(false)),
            }),
        }
    }
    pub fn snapshot(&self) -> Arc<Snapshot> {
        Arc::clone(
            &self
                .inner
                .snapshot
                .read()
                .unwrap_or_else(|e| e.into_inner()),
        )
    }
    pub fn refreshing(&self) -> bool {
        self.inner.refreshing.load(Ordering::Acquire)
    }
    /// 安装离线记录；迭代顺序决定摘要中同计数项的顺序。
    pub fn install(&self, raw: impl IntoIterator<Item = (PathBuf, ProcessMap)>) {
        let mut data = Data::default();
        for (path, slot) in raw {
            let entry = data.entry(self.inner.os, &path);
            for (pid, usage) in slot.entries {
                entry.insert(pid, usage);
            }
        }
        // 和旧版 install 一样，仅替换索引和 stamp，不改变上次刷新统计、耗时或错误。
        let mut slot = self
            .inner
            .snapshot
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let mut next = (**slot).clone();
        next.data = Arc::new(data);
        next.stamp += 1;
        let old = std::mem::replace(&mut *slot, Arc::new(next));
        drop(slot);
        drop(old);
    }
    pub fn refresh(&self) -> bool {
        self.refresh_with_budget(DEFAULT_BUDGET)
    }
    /// false 表示已有刷新，或上次超时的系统调用仍未返回。预算最多 12 秒。
    pub fn refresh_with_budget(&self, budget: Duration) -> bool {
        self.refresh_using(budget, collect_native)
    }
    pub fn refresh_async(&self) -> bool {
        self.refresh_async_with_budget(DEFAULT_BUDGET)
    }
    pub fn refresh_async_with_budget(&self, budget: Duration) -> bool {
        if !self.begin() {
            return false;
        }
        let this = self.clone();
        let start = Instant::now();
        let spawn = std::thread::Builder::new()
            .name("diskmap-procs-refresh".into())
            .spawn(move || {
                this.run(budget.min(DEFAULT_BUDGET), start, collect_native);
            });
        if let Err(error) = spawn {
            self.finish_spawn_error(start, error);
            return false;
        }
        true
    }
    fn begin(&self) -> bool {
        if self
            .inner
            .refreshing
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return false;
        }
        if self
            .inner
            .collecting
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            self.inner.refreshing.store(false, Ordering::Release);
            return false;
        }
        true
    }
    fn refresh_using(
        &self,
        budget: Duration,
        collect: impl FnOnce(&Collector) -> io::Result<()> + Send + 'static,
    ) -> bool {
        if !self.begin() {
            return false;
        }
        self.run(budget.min(DEFAULT_BUDGET), Instant::now(), collect);
        true
    }
    fn finish_spawn_error(&self, start: Instant, error: io::Error) {
        self.publish(
            None,
            start.elapsed(),
            Some(error_text(&error.to_string())),
            false,
        );
        self.inner.collecting.store(false, Ordering::Release);
        self.inner.refreshing.store(false, Ordering::Release);
    }
    fn run(
        &self,
        budget: Duration,
        start: Instant,
        collect: impl FnOnce(&Collector) -> io::Result<()> + Send + 'static,
    ) {
        let _refresh_guard = FlagGuard(&self.inner.refreshing);
        let out = Collector {
            data: Arc::default(),
            cancelled: Arc::default(),
            deadline: start + budget,
            os: self.inner.os,
        };
        let worker_out = out.clone();
        let collecting = Arc::clone(&self.inner.collecting);
        let (tx, rx) = mpsc::sync_channel(1);
        let spawn = std::thread::Builder::new()
            .name("diskmap-procs".into())
            .spawn(move || {
                let worker_guard = FlagGuard(&collecting);
                let result = collect(&worker_out);
                drop(worker_guard);
                let _ = tx.send(result);
            });
        if let Err(error) = spawn {
            self.publish(
                None,
                start.elapsed(),
                Some(error_text(&error.to_string())),
                false,
            );
            self.inner.collecting.store(false, Ordering::Release);
            return;
        }
        let result = rx.recv_timeout(out.deadline.saturating_duration_since(Instant::now()));
        out.cancelled.store(true, Ordering::Release);
        let (error, timed_out) = match result {
            Ok(Ok(())) => (None, Instant::now() >= out.deadline),
            Ok(Err(error)) => (Some(error_text(&error.to_string())), false),
            Err(mpsc::RecvTimeoutError::Timeout) => (None, true),
            Err(mpsc::RecvTimeoutError::Disconnected) => (
                Some(Text::new(
                    "读取进程信息的线程意外退出",
                    "process information worker exited unexpectedly",
                )),
                false,
            ),
        };
        let data = if error.is_none() {
            Some(std::mem::take(
                &mut *out.data.lock().unwrap_or_else(|e| e.into_inner()),
            ))
        } else {
            None
        };
        self.publish(data, start.elapsed(), error, timed_out);
    }
    fn publish(&self, data: Option<Data>, elapsed: Duration, error: Option<Text>, timed_out: bool) {
        let mut slot = self
            .inner
            .snapshot
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let mut next = (**slot).clone();
        if let Some(data) = data {
            next.nprocs = data.nprocs;
            next.visible = data.visible;
            next.data = Arc::new(data);
            next.stamp += 1;
        }
        next.time = elapsed.as_secs_f64();
        next.error = error;
        next.timed_out = timed_out;
        let old = std::mem::replace(&mut *slot, Arc::new(next));
        drop(slot);
        // 旧索引的释放可能较慢，不能阻塞查询线程取得快照。
        drop(old);
    }
}
struct FlagGuard<'a>(&'a AtomicBool);
impl Drop for FlagGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
fn error_text(error: &str) -> Text {
    Text::owned(
        format!("无法读取进程信息：{error}"),
        format!("could not read process information: {error}"),
    )
}

/// 锁只保护内存登记，任何系统调用均在锁外执行。
#[derive(Clone)]
struct Collector {
    data: Arc<Mutex<Data>>,
    cancelled: Arc<AtomicBool>,
    deadline: Instant,
    os: Os,
}
impl Collector {
    fn expired(&self) -> bool {
        self.cancelled.load(Ordering::Acquire) || Instant::now() >= self.deadline
    }
    fn add(&self, path: impl AsRef<Path>, pid: u32, comm: &str, kind: Kind) {
        let path = path.as_ref();
        if self.expired() || !path::usable(self.os, path) {
            return;
        }
        let path = path::without_deleted(path);
        let usage = if kind == Kind::Swap {
            Usage {
                comm: Text::new("系统", "system"),
                kind,
            }
        } else {
            Usage::new(comm, kind)
        };
        let mut data = self.data.lock().unwrap_or_else(|e| e.into_inner());
        if !self.expired() {
            data.insert(self.os, &path, pid, usage);
        }
    }
    fn process_seen(&self) {
        let mut data = self.data.lock().unwrap_or_else(|e| e.into_inner());
        if !self.expired() {
            data.nprocs += 1;
        }
    }
    fn process_visible(&self) {
        let mut data = self.data.lock().unwrap_or_else(|e| e.into_inner());
        if !self.expired() {
            data.visible += 1;
        }
    }
}

#[cfg(target_os = "linux")]
use linux as native;
#[cfg(target_os = "macos")]
use macos as native;
#[cfg(windows)]
use windows as native;
fn collect_native(out: &Collector) -> io::Result<()> {
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    {
        // 先登记交换文件，避免大量进程用尽预算后遗漏系统核心文件。
        if !out.expired() {
            for path in native::swapfiles() {
                out.add(path, 0, "", Kind::Swap);
            }
        }
        if !out.expired() {
            native::collect(out)?;
        }
        Ok(())
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = out;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "unsupported operating system",
        ))
    }
}

/// 旧版 read_swapfiles / osinfo.swapfiles 的接口，供分类器创建时使用；最多等待 12 秒。
/// 并发调用共享同一次查询；超时返回上次结果，未返回的查询不会导致线程累积。
pub fn read_swapfiles() -> Vec<PathBuf> {
    static SHARED: OnceLock<(Mutex<SwapState>, Condvar)> = OnceLock::new();
    let (mutex, changed) =
        SHARED.get_or_init(|| (Mutex::new(SwapState::default()), Condvar::new()));
    let deadline = Instant::now() + DEFAULT_BUDGET;
    let mut state = mutex.lock().unwrap_or_else(|e| e.into_inner());
    let generation = state.generation;
    if !state.running {
        state.running = true;
        let spawn = std::thread::Builder::new()
            .name("diskmap-swapfiles".into())
            .spawn(move || {
                let paths = std::panic::catch_unwind(|| {
                    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
                    {
                        native::swapfiles()
                    }
                    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
                    {
                        Vec::new()
                    }
                });
                let mut state = mutex.lock().unwrap_or_else(|e| e.into_inner());
                if let Ok(paths) = paths {
                    state.paths = paths;
                }
                state.running = false;
                state.generation += 1;
                changed.notify_all();
            });
        if spawn.is_err() {
            state.running = false;
            changed.notify_all();
        }
    }
    while state.running && state.generation == generation {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        let (next, _) = changed
            .wait_timeout(state, remaining)
            .unwrap_or_else(|e| e.into_inner());
        state = next;
    }
    state.paths.clone()
}
#[derive(Default)]
struct SwapState {
    running: bool,
    generation: u64,
    paths: Vec<PathBuf>,
}
