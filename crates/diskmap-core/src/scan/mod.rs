//! 扫描（T01）：通用的并行目录遍历，对应 legacy/backend/scanner.py 的 `Scan._run`。
//! NTFS MFT 快速扫描在子模块 [`ntfs`]（T03 负责）。
//!
//! ```no_run
//! use std::sync::Arc;
//! use diskmap_core::classify::Classifier;
//! use diskmap_core::scan::{Scan, ScanOptions};
//!
//! let classifier = Arc::new(Classifier::new(None, &[]));
//! let scan = Scan::start(ScanOptions::new("/usr"), classifier);
//! println!("已读到 {} 个文件", scan.progress().files); // 随时可以查询进度
//! scan.wait();
//! let index = scan.index(); // 扫描进行中也能读取，读到的是已经扫描的部分
//! println!("共 {} 字节", index.root().size());
//! ```
//!
//! 做法：
//! - 多个工作线程共用一个后进先出的待读目录栈（子目录按名字逆序压入，整体接近按名字的深度优先）。
//!   读目录、取文件信息、分类都在锁外进行；读完一个目录后短暂地加写锁，用 [`Index::fill_dir`] 一次写入。
//! - 硬链接（Unix 上链接数大于 1 的文件）：同一个（设备号, inode）只计入一个，其余不计入、不显示，与旧版相同。
//!   扫描时先写入索引但不计入；全部读完后，每组选按名字排序的深度优先遍历中最先遇到的那个计入
//!   （[`Index::cmp_file_order`]），这样不论线程怎样交错，结果都一样。
//! - 挂载点：扫描前算好起点下每个挂载点进入还是跳过（见 `plan`）。
//! - 不跟随符号链接，按文件计；Windows 的目录联接整个跳过（见 `read`）。
//!
//! 与旧版的不同：
//! - 保留全部文件。旧版每个目录只保留最大的 150 个文件，其余合并成一项；新版在生成界面数据时再合并（T05）。
//! - 同一个文件系统挂载多次、同一个文件有多个硬链接时，旧版选按读取目录的顺序先遇到的那个；
//!   新版选按名字排序的深度优先遍历中先遇到的那个。总数相同，选中的位置可能不同，但每次扫描都一样。
//! - Windows 的"实际占用"直接用系统给出的分配大小；旧版按簇大小估算。Windows 上不识别硬链接（与旧版相同，
//!   MFT 扫描 T03 会处理）。
//! - 出错时 `error` 是双语文字；旧版是 Python 异常的 repr。

pub mod ntfs;
mod plan;
mod read;

use std::ffi::OsString;
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard};
use std::time::{Duration, Instant, SystemTime};

use crate::classify::{Classifier, DirClass};
use crate::i18n::Text;
use crate::index::{
    self, DENIED, DirId, FileId, Filled, Index, IndexError, MOUNT, NewDir, NewFile,
};
use crate::model::Category;
use crate::platform::{self, Os, mounts};

use plan::{Decision, MountPlan, Visit};
use read::Kind;

/// 最多记录多少个没有权限读取的目录（与旧版相同）。
pub const DENIED_SAMPLES: usize = 50;

/// 工作线程数的上限。
pub const MAX_THREADS: usize = 64;

/// 选择扫描方式；强制 MFT 时不回退，便于验证和测量。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ScanMethod {
    #[default]
    Auto,
    Generic,
    Mft,
}

impl ScanMethod {
    pub const fn key(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Generic => "generic",
            Self::Mft => "mft",
        }
    }
}

/// 扫描选项。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanOptions {
    /// 扫描起点。开始时会用 `platform::norm_path` 规范化。
    pub root: PathBuf,
    /// `true`：按文件长度统计；`false`（默认）：按实际占用的磁盘空间统计。
    pub apparent: bool,
    /// 是否进入起点下其他磁盘分区的挂载点（默认 `true`，与旧版相同）。
    pub cross_fs: bool,
    /// 工作线程数；0（默认）表示用 [`default_threads`]。
    pub threads: usize,
    /// 默认自动选择；MFT 只用于 Windows 管理员扫描整个 NTFS 卷。
    pub method: ScanMethod,
}

impl ScanOptions {
    pub fn new(root: impl Into<PathBuf>) -> ScanOptions {
        ScanOptions {
            root: root.into(),
            apparent: false,
            cross_fs: true,
            threads: 0,
            method: ScanMethod::Auto,
        }
    }
}

/// 默认的工作线程数：处理器数，最多 8 个。
pub fn default_threads() -> usize {
    std::thread::available_parallelism().map_or(4, |n| n.get().min(8))
}

/// 扫描状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScanState {
    Scanning,
    Done,
    Cancelled,
    Error,
}

impl ScanState {
    /// `"scanning"`、`"done"`、`"cancelled"`、`"error"`，与旧版 `/api/status` 的 `state` 字段一致。
    pub const fn key(self) -> &'static str {
        match self {
            ScanState::Scanning => "scanning",
            ScanState::Done => "done",
            ScanState::Cancelled => "cancelled",
            ScanState::Error => "error",
        }
    }

    /// 是否已经结束（完成、取消或出错）。
    pub const fn is_finished(self) -> bool {
        !matches!(self, ScanState::Scanning)
    }
}

/// 扫描时遇到的挂载点（旧版 `included_mounts`、`skipped_mounts` 的一项）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MountEntry {
    pub path: PathBuf,
    pub fstype: String,
    /// 来源设备。只有进入的挂载点有（旧版 `skipped` 里也没有这个字段）。
    pub source: Option<String>,
}

/// 扫描进度，对应旧版 `/api/status` 里与扫描有关的字段。
#[derive(Clone, Debug)]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub struct Progress {
    pub state: ScanState,
    /// 出错原因（`state` 为 `Error` 时）。
    pub error: Option<Text>,
    /// 规范化后的扫描起点。
    pub root: PathBuf,
    /// 已经计入的文件数、目录数（不含起点）、字节数。
    pub files: u64,
    pub dirs: u64,
    pub bytes: u64,
    /// 没有权限读取的目录数，以及其中前 [`DENIED_SAMPLES`] 个的路径。
    pub denied: u64,
    pub denied_samples: Vec<PathBuf>,
    /// 正在读取的目录（扫描进行中时）。多个线程同时在读，这里是最近开始读的一个。
    pub current: Option<PathBuf>,
    /// 用时：扫描进行中为到现在为止，结束后为总用时。
    pub elapsed: Duration,
    /// 结束的时间。
    pub finished_at: Option<SystemTime>,
    pub apparent: bool,
    pub cross_fs: bool,
    /// 起点所在分区和进入的分区的已用空间之和（与旧版相同，扫描正常完成后才有）。
    pub fs_used: u64,
    /// 进入的挂载点、跳过的挂载点（按遇到的先后）。
    pub mounts: Vec<MountEntry>,
    pub skipped: Vec<MountEntry>,
    pub threads: usize,
    /// 实际使用的扫描方式；尚未确定时为 Auto。
    pub method: ScanMethod,
    /// 自动选择或 MFT 失败后改用通用扫描的双语原因。
    pub fallback_reason: Option<Text>,
    /// MFT 已处理的记录数（通用扫描为 0；回退时保留尝试进度）。
    pub records: u64,
    /// MFT 跳过或保守处理的异常记录数；同一记录只计一次。
    pub anomalies: u64,
}

/// 一次扫描。可以克隆，克隆的句柄指向同一次扫描；最后一个句柄释放时自动取消。
#[derive(Clone)]
pub struct Scan {
    handle: Arc<Handle>,
}

struct Handle(Arc<Inner>);

impl Drop for Handle {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

impl Scan {
    /// 在后台线程开始扫描，立即返回。
    pub fn start(options: ScanOptions, classifier: Arc<Classifier>) -> Scan {
        Self::spawn(options, classifier, None)
    }

    /// 发布 Done 后在同一后台线程执行回调。回调不延长实时扫描句柄的生命周期。
    pub(crate) fn start_with_done(
        options: ScanOptions,
        classifier: Arc<Classifier>,
        done: impl FnOnce(Scan) + Send + 'static,
    ) -> Scan {
        Self::spawn(options, classifier, Some(Box::new(done)))
    }

    fn spawn(
        options: ScanOptions,
        classifier: Arc<Classifier>,
        done: Option<Box<dyn FnOnce(Scan) + Send>>,
    ) -> Scan {
        let inner = Arc::new(Inner::new(options, classifier));
        let worker = Arc::clone(&inner);
        let spawned = std::thread::Builder::new()
            .name("diskmap-scan".to_owned())
            .spawn(move || Self::run_with_done(worker, done));
        if let Err(e) = spawned {
            inner.finish(Some(Text::owned(
                format!("无法启动扫描线程：{e}"),
                format!("Could not start the scan thread: {e}"),
            )));
        }
        Self::from_inner(inner)
    }

    fn from_inner(inner: Arc<Inner>) -> Scan {
        Scan {
            handle: Arc::new(Handle(inner)),
        }
    }

    fn run_with_done(worker: Arc<Inner>, done: Option<Box<dyn FnOnce(Scan) + Send>>) {
        let state = worker.run();
        if state == ScanState::Done
            && let Some(done) = done
        {
            done(Self::from_inner(worker));
        }
    }

    /// 由存盘的索引和进度构造已完成的扫描。不启动线程，也不重新访问扫描起点。
    /// 调用方须提供与索引的分类原因编号相匹配的分类器。
    pub fn from_saved(
        index: Index,
        progress: Progress,
        classifier: Arc<Classifier>,
    ) -> io::Result<Scan> {
        let root = index.root();
        let finished_at = progress.finished_at.filter(|_| {
            progress.state == ScanState::Done
                && progress.error.is_none()
                && progress.current.is_none()
                && (1..=MAX_THREADS).contains(&progress.threads)
                && progress.denied_samples.len() <= DENIED_SAMPLES
                && progress.denied_samples.len() as u64 <= progress.denied
                && progress.root.as_os_str() == index.root_path().as_os_str()
                && (progress.files, progress.dirs, progress.bytes)
                    == (root.nfiles(), root.ndirs(), root.size())
        });
        let Some(finished_at) = finished_at else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "保存的扫描进度与索引不符 / Saved scan progress disagrees with the index",
            ));
        };
        let inner = Inner {
            root: progress.root,
            os: index.os(),
            apparent: progress.apparent,
            cross_fs: progress.cross_fs,
            threads: progress.threads,
            method: progress.method,
            records: AtomicU64::new(progress.records),
            anomalies: AtomicU64::new(progress.anomalies),
            classifier,
            index: RwLock::new(index),
            saved: true,
            cancelled: AtomicBool::new(false),
            files: AtomicU64::new(progress.files),
            dirs: AtomicU64::new(progress.dirs),
            bytes: AtomicU64::new(progress.bytes),
            denied: AtomicU64::new(progress.denied),
            current: Mutex::new(PathBuf::new()),
            status: Mutex::new(Status {
                method: progress.method,
                fallback_reason: progress.fallback_reason,
                state: ScanState::Done,
                error: None,
                finished: Some((progress.elapsed, finished_at)),
                denied_samples: progress.denied_samples,
                mounts: progress.mounts,
                skipped: progress.skipped,
                fs_used: progress.fs_used,
            }),
            done: Condvar::new(),
            queue: Mutex::new(Queue::default()),
            work: Condvar::new(),
            links: Mutex::new(Vec::new()),
            started: Instant::now(),
        };
        Ok(Self::from_inner(Arc::new(inner)))
    }

    fn inner(&self) -> &Inner {
        &self.handle.0
    }

    /// 这次扫描是否由存盘结果恢复。
    pub fn is_saved(&self) -> bool {
        self.inner().saved
    }

    pub(crate) fn saved_finished_at(&self) -> Option<SystemTime> {
        if self.is_saved() {
            lock(&self.inner().status).finished.map(|(_, time)| time)
        } else {
            None
        }
    }

    /// 请求取消。工作线程读完手上的目录后停止；已经扫描的部分保留。
    pub fn cancel(&self) {
        self.inner().cancel();
    }

    pub fn state(&self) -> ScanState {
        lock(&self.inner().status).state
    }

    /// 等待扫描结束，返回最终状态。
    pub fn wait(&self) -> ScanState {
        let inner = self.inner();
        let status = inner
            .done
            .wait_while(lock(&inner.status), |s| s.state == ScanState::Scanning)
            .unwrap_or_else(PoisonError::into_inner);
        status.state
    }

    /// 最多等待 `timeout`，返回当时的状态。
    pub fn wait_timeout(&self, timeout: Duration) -> ScanState {
        let inner = self.inner();
        let (status, _) = inner
            .done
            .wait_timeout_while(lock(&inner.status), timeout, |s| {
                s.state == ScanState::Scanning
            })
            .unwrap_or_else(PoisonError::into_inner);
        status.state
    }

    /// 当前进度。
    pub fn progress(&self) -> Progress {
        let inner = self.inner();
        let status = lock(&inner.status);
        let current = if status.state == ScanState::Scanning {
            let cur = lock(&inner.current);
            (!cur.as_os_str().is_empty()).then(|| cur.clone())
        } else {
            None
        };
        Progress {
            state: status.state,
            error: status.error.clone(),
            root: inner.root.clone(),
            files: inner.files.load(Relaxed),
            dirs: inner.dirs.load(Relaxed),
            bytes: inner.bytes.load(Relaxed),
            denied: inner.denied.load(Relaxed),
            denied_samples: status.denied_samples.clone(),
            current,
            elapsed: status
                .finished
                .map_or_else(|| inner.started.elapsed(), |(elapsed, _)| elapsed),
            finished_at: status.finished.map(|(_, at)| at),
            apparent: inner.apparent,
            cross_fs: inner.cross_fs,
            fs_used: status.fs_used,
            mounts: status.mounts.clone(),
            skipped: status.skipped.clone(),
            threads: inner.threads,
            method: status.method,
            fallback_reason: status.fallback_reason.clone(),
            records: inner.records.load(Relaxed),
            anomalies: inner.anomalies.load(Relaxed),
        }
    }

    /// 读取目录树。扫描进行中也可以读取（读到的是已经扫描的部分，还没读到的目录 `is_listed()` 为假），
    /// 但持有期间扫描线程无法写入，用完尽快释放。
    pub fn index(&self) -> RwLockReadGuard<'_, Index> {
        self.inner()
            .index
            .read()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// 规范化后的扫描起点。
    pub fn root(&self) -> &Path {
        &self.inner().root
    }

    pub fn apparent(&self) -> bool {
        self.inner().apparent
    }

    pub fn cross_fs(&self) -> bool {
        self.inner().cross_fs
    }

    /// 这次扫描用的分类器（解释原因编号时要用同一个）。
    pub fn classifier(&self) -> &Arc<Classifier> {
        &self.inner().classifier
    }
}

// ---------------------------------------------------------------------- 内部

struct Inner {
    root: PathBuf,
    os: Os,
    apparent: bool,
    cross_fs: bool,
    threads: usize,
    method: ScanMethod,
    records: AtomicU64,
    anomalies: AtomicU64,
    classifier: Arc<Classifier>,
    index: RwLock<Index>,
    saved: bool,
    cancelled: AtomicBool,
    files: AtomicU64,
    dirs: AtomicU64,
    bytes: AtomicU64,
    denied: AtomicU64,
    /// 最近开始读的目录。工作线程用 try_lock 更新，抢不到就不更新。
    current: Mutex<PathBuf>,
    status: Mutex<Status>,
    /// 扫描结束时通知。
    done: Condvar,
    queue: Mutex<Queue>,
    /// 有新的待读目录、全部完成或取消时通知工作线程。
    work: Condvar,
    /// 有多个硬链接的文件，扫描结束后决定计入哪一个。
    links: Mutex<Vec<PendingLink>>,
    started: Instant,
}

struct Status {
    method: ScanMethod,
    fallback_reason: Option<Text>,
    state: ScanState,
    error: Option<Text>,
    /// 总用时和结束时间。
    finished: Option<(Duration, SystemTime)>,
    denied_samples: Vec<PathBuf>,
    mounts: Vec<MountEntry>,
    skipped: Vec<MountEntry>,
    fs_used: u64,
}

/// 待读的目录。
struct Task {
    dir: DirId,
    path: PathBuf,
    class: DirClass,
}

#[derive(Default)]
struct Queue {
    stack: Vec<Task>,
    /// 正在读目录的线程数。
    busy: usize,
    /// 在等待新目录的线程数。
    waiting: usize,
}

/// 每个工作线程复用的缓冲区。
#[derive(Default)]
struct Buffers {
    dirs: Vec<SubDir>,
    files: Vec<FileItem>,
}

struct SubDir {
    name: OsString,
    path: PathBuf,
    class: DirClass,
    flags: u8,
}

struct FileItem {
    name: OsString,
    size: u64,
    category: Category,
    /// 硬链接数大于 1 时的（设备号, inode）。
    link: Option<(u64, u64)>,
}

/// 写入了索引、还没有计入的硬链接。
struct PendingLink {
    key: (u64, u64),
    dir: DirId,
    file: FileId,
}

impl Inner {
    fn new(options: ScanOptions, classifier: Arc<Classifier>) -> Inner {
        let root = normalize_root(&options.root);
        let os = Os::current();
        let index = Index::new(os, &root, classifier.root_class(&root));
        let threads = match options.threads {
            0 => default_threads(),
            n => n.min(MAX_THREADS),
        };
        Inner {
            root,
            os,
            apparent: options.apparent,
            cross_fs: options.cross_fs,
            threads,
            method: options.method,
            records: AtomicU64::new(0),
            anomalies: AtomicU64::new(0),
            classifier,
            index: RwLock::new(index),
            saved: false,
            cancelled: AtomicBool::new(false),
            files: AtomicU64::new(0),
            dirs: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            denied: AtomicU64::new(0),
            current: Mutex::new(PathBuf::new()),
            status: Mutex::new(Status {
                method: options.method,
                fallback_reason: None,
                state: ScanState::Scanning,
                error: None,
                finished: None,
                denied_samples: Vec::new(),
                mounts: Vec::new(),
                skipped: Vec::new(),
                fs_used: 0,
            }),
            done: Condvar::new(),
            queue: Mutex::new(Queue::default()),
            work: Condvar::new(),
            links: Mutex::new(Vec::new()),
            started: Instant::now(),
        }
    }

    /// 扫描线程的入口：扫描，然后记录结果。
    fn run(&self) -> ScanState {
        let error = match catch_unwind(AssertUnwindSafe(|| self.scan_all())) {
            Ok(Ok(())) => None,
            Ok(Err(e)) => Some(e),
            Err(_) => Some(internal_error()),
        };
        self.finish(error)
    }

    fn scan_all(&self) -> Result<(), Text> {
        if self.cancelled.load(Relaxed) || ntfs::selected(self)? {
            return Ok(());
        }
        if !self.root.is_dir() {
            let p = self.root.display();
            return Err(Text::owned(
                format!("不是文件夹：{p}"),
                format!("Not a folder: {p}"),
            ));
        }
        let plan = MountPlan::new(
            self.os,
            &self.root,
            &mounts::read_mounts(),
            self.cross_fs,
            &mounts::skip_paths(),
        );
        self.run_workers(&plan);
        // 取消时也处理已经读到的部分，让目录树的统计前后一致
        self.count_links();
        if !self.cancelled.load(Relaxed) {
            write(&self.index).shrink_to_fit();
            let used = self.fs_used(&plan);
            lock(&self.status).fs_used = used;
        }
        Ok(())
    }

    fn run_workers(&self, plan: &MountPlan) {
        let class = self
            .index
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .root()
            .class();
        lock(&self.queue).stack.push(Task {
            dir: DirId::ROOT,
            path: self.root.clone(),
            class,
        });
        std::thread::scope(|s| {
            for i in 1..self.threads {
                // 线程创建失败时少一个线程，结果不受影响
                let _ = std::thread::Builder::new()
                    .name(format!("diskmap-scan-{i}"))
                    .spawn_scoped(s, || self.work(plan));
            }
            self.work(plan);
        });
    }

    /// 每组硬链接（相同的设备号和 inode）计入一个：按名字排序的深度优先遍历中最先遇到的那个。
    fn count_links(&self) {
        let mut pending = std::mem::take(&mut *lock(&self.links));
        if pending.is_empty() {
            return;
        }
        pending.sort_unstable_by_key(|p| p.key);
        let mut index = write(&self.index);
        let (mut files, mut bytes) = (0u64, 0u64);
        for group in pending.chunk_by(|a, b| a.key == b.key) {
            let first = group
                .iter()
                .min_by(|a, b| index.cmp_file_order((a.dir, a.file), (b.dir, b.file)));
            if let Some(p) = first
                && index.count_file(p.dir, p.file)
            {
                files += 1;
                bytes += index.file(p.file).size();
            }
        }
        drop(index);
        self.files.fetch_add(files, Relaxed);
        self.bytes.fetch_add(bytes, Relaxed);
    }

    /// 起点所在分区和进入的分区的已用空间之和（旧版 `fs_mps`）。
    fn fs_used(&self, plan: &MountPlan) -> u64 {
        let included: Vec<PathBuf> = lock(&self.status)
            .mounts
            .iter()
            .map(|m| m.path.clone())
            .collect();
        let root = plan.root_mount().unwrap_or(&self.root);
        std::iter::once(root)
            .chain(included.iter().map(PathBuf::as_path))
            .filter_map(mounts::fs_usage)
            .map(|u| u.used)
            .sum()
    }

    /// 记录结束状态并通知等待的线程。
    fn finish(&self, error: Option<Text>) -> ScanState {
        let mut status = lock(&self.status);
        if status.error.is_none() {
            status.error = error;
        }
        status.state = if status.error.is_some() {
            ScanState::Error
        } else if self.cancelled.load(Relaxed) {
            ScanState::Cancelled
        } else {
            ScanState::Done
        };
        status.finished = Some((self.started.elapsed(), SystemTime::now()));
        let state = status.state;
        drop(status);
        self.done.notify_all();
        state
    }

    fn cancel(&self) {
        if self.saved {
            return;
        }
        self.cancelled.store(true, Relaxed);
        let _queue = lock(&self.queue);
        self.work.notify_all();
    }

    /// 无法继续的错误：记录原因并停止。
    fn fail(&self, error: Text) {
        {
            let mut status = lock(&self.status);
            if status.error.is_none() {
                status.error = Some(error);
            }
        }
        self.cancel();
    }

    // ------------------------------------------------------------------ 工作线程

    fn work(&self, plan: &MountPlan) {
        let mut buf = Buffers::default();
        while let Some(task) = self.next_task() {
            let children = catch_unwind(AssertUnwindSafe(|| self.read_one(task, plan, &mut buf)))
                .unwrap_or_else(|_| {
                    self.fail(internal_error());
                    Vec::new()
                });
            self.task_done(children);
        }
    }

    /// 取下一个待读目录；全部完成或已取消时返回 `None`。
    fn next_task(&self) -> Option<Task> {
        let mut queue = lock(&self.queue);
        loop {
            if self.cancelled.load(Relaxed) {
                return None;
            }
            if let Some(task) = queue.stack.pop() {
                queue.busy += 1;
                return Some(task);
            }
            if queue.busy == 0 {
                return None;
            }
            queue.waiting += 1;
            queue = self
                .work
                .wait(queue)
                .unwrap_or_else(PoisonError::into_inner);
            queue.waiting -= 1;
        }
    }

    fn task_done(&self, children: Vec<Task>) {
        let mut queue = lock(&self.queue);
        queue.busy -= 1;
        let n = children.len();
        // 逆序压入，按名字排在前面的先读
        queue.stack.extend(children.into_iter().rev());
        if queue.stack.is_empty() && queue.busy == 0 {
            self.work.notify_all();
        } else {
            // 自己会接着取一个，其余的叫醒等待的线程
            for _ in 0..n.saturating_sub(1).min(queue.waiting) {
                self.work.notify_one();
            }
        }
    }

    /// 读一个目录，写入索引，返回要接着读的子目录。
    fn read_one(&self, task: Task, plan: &MountPlan, buf: &mut Buffers) -> Vec<Task> {
        if let Ok(mut cur) = self.current.try_lock() {
            cur.clone_from(&task.path);
        }
        let classifier = &*self.classifier;
        let ctx = classifier.enter_dir(task.class, &task.path);
        let Buffers { dirs, files } = buf;
        dirs.clear();
        files.clear();

        let read = read::read_dir(
            &task.path,
            self.apparent,
            &self.cancelled,
            |entry| match entry.kind {
                Kind::Dir => {
                    let path = task.path.join(&entry.name);
                    let flags = match plan.visit(&path) {
                        Visit::Normal => 0,
                        Visit::Ignored => return,
                        Visit::Mount(Decision::Skip { fstype }) => {
                            self.note_mount(&path, fstype, None);
                            return;
                        }
                        Visit::Mount(Decision::Include { fstype, source }) => {
                            self.note_mount(&path, fstype, Some(source));
                            MOUNT
                        }
                    };
                    let class = classifier.dir_class(&ctx, &entry.name, &path);
                    dirs.push(SubDir {
                        name: entry.name,
                        path,
                        class,
                        flags,
                    });
                }
                Kind::File { size, link } => {
                    let category = classifier.file_class(&ctx, &entry.name);
                    files.push(FileItem {
                        name: entry.name,
                        size,
                        category,
                        link,
                    });
                }
            },
        );

        if let Err(err) = read {
            // 读不了的目录记为内容为空；没有权限时加上标志（旧版对其他错误不做标记）
            let denied = err.kind() == io::ErrorKind::PermissionDenied;
            if denied {
                self.note_denied(&task.path);
            }
            let mut index = write(&self.index);
            if denied {
                index.add_flags(task.dir, DENIED);
            }
            let _ = index.fill_dir(task.dir, &[], &[]);
            return Vec::new();
        }
        if self.cancelled.load(Relaxed) {
            // 可能只读了一部分，不写入
            return Vec::new();
        }

        let os = self.os;
        dirs.sort_unstable_by(|a, b| {
            index::cmp_names(os, a.name.as_encoded_bytes(), b.name.as_encoded_bytes())
        });
        files.sort_unstable_by(|a, b| {
            index::cmp_names(os, a.name.as_encoded_bytes(), b.name.as_encoded_bytes())
        });
        let new_dirs: Vec<NewDir<'_>> = dirs
            .iter()
            .map(|d| NewDir {
                name: &d.name,
                class: d.class,
                flags: d.flags,
            })
            .collect();
        let new_files: Vec<NewFile<'_>> = files
            .iter()
            .map(|f| NewFile {
                name: &f.name,
                size: f.size,
                category: f.category,
                linked: f.link.is_some(),
            })
            .collect();
        let filled = write(&self.index).fill_dir(task.dir, &new_dirs, &new_files);
        let Filled {
            dirs: dir_ids,
            files: file_ids,
        } = match filled {
            Ok(ids) => ids,
            Err(e) => {
                self.fail(index_error(e));
                return Vec::new();
            }
        };

        // 硬链接留到扫描结束后再计入，其余文件现在就计入进度
        let (mut nfiles, mut nbytes) = (0u64, 0u64);
        let mut pending = None;
        for (f, id) in files.iter().zip(file_ids) {
            match f.link {
                Some(key) => pending
                    .get_or_insert_with(|| lock(&self.links))
                    .push(PendingLink {
                        key,
                        dir: task.dir,
                        file: FileId(id),
                    }),
                None => {
                    nfiles += 1;
                    nbytes += f.size;
                }
            }
        }
        drop(pending);
        self.files.fetch_add(nfiles, Relaxed);
        self.dirs.fetch_add(dirs.len() as u64, Relaxed);
        self.bytes.fetch_add(nbytes, Relaxed);
        files.clear();
        dirs.drain(..)
            .zip(dir_ids)
            .map(|(d, id)| Task {
                dir: DirId(id),
                path: d.path,
                class: d.class,
            })
            .collect()
    }

    fn note_mount(&self, path: &Path, fstype: &str, source: Option<&str>) {
        let entry = MountEntry {
            path: path.to_path_buf(),
            fstype: fstype.to_owned(),
            source: source.map(str::to_owned),
        };
        let mut status = lock(&self.status);
        if entry.source.is_some() {
            status.mounts.push(entry);
        } else {
            status.skipped.push(entry);
        }
    }

    fn note_denied(&self, path: &Path) {
        self.denied.fetch_add(1, Relaxed);
        let mut status = lock(&self.status);
        if status.denied_samples.len() < DENIED_SAMPLES {
            status.denied_samples.push(path.to_path_buf());
        }
    }
}

/// 起点的规范化：能转成文字的路径用 `platform::norm_path`（与旧版相同），否则只转成绝对路径。
fn normalize_root(root: &Path) -> PathBuf {
    match root.to_str() {
        Some(s) => platform::norm_path(s),
        None => std::path::absolute(root).unwrap_or_else(|_| root.to_path_buf()),
    }
}

fn internal_error() -> Text {
    Text::new("扫描时出现内部错误", "Internal error while scanning")
}

fn index_error(e: IndexError) -> Text {
    match e {
        IndexError::Full => Text::new(
            "文件太多，超出了索引的容量（约 40 亿项）",
            "Too many files: the index is full (about 4 billion entries)",
        ),
        IndexError::AlreadyListed => internal_error(),
    }
}

/// 加锁；其他线程在持有锁时出错不影响这里的数据（都是计数和列表），忽略中毒。
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn write(l: &RwLock<Index>) -> std::sync::RwLockWriteGuard<'_, Index> {
    l.write().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests;
