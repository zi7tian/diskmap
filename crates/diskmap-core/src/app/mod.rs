//! 应用状态：异步初始化、扫描生命周期和界面状态；不依赖 HTTP 或窗口。

#[cfg(feature = "server")]
mod catalog;
mod metadata;
mod reveal;
pub(crate) mod saved;
mod tree;

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::classify::Classifier;
use crate::i18n::Text;
use crate::pkgdb::PkgDb;
use crate::platform::{self, Os};
use crate::procinfo::{self, ProcInfo};
use crate::saved::Store;
use crate::scan::{Progress, Scan, ScanMethod, ScanOptions, ScanState};
use crate::view::{self, ViewCache};

/// 可克隆的应用句柄。所有克隆共享当前扫描和界面状态。
#[derive(Clone)]
pub struct App {
    inner: Arc<Inner>,
}

struct Inner {
    components: RwLock<Arc<Components>>,
    ready: Mutex<bool>,
    initialized: Condvar,
    scan: Mutex<Option<Arc<Session>>>,
    closed: AtomicBool,
    procs: ProcInfo,
    refresh_on_scan: bool,
    ui: Mutex<Ui>,
    admin: bool,
    settings_version: AtomicU64,
    saved: Mutex<saved::Location>,
}

struct Components {
    pkgdb: Arc<PkgDb>,
    classifier: Arc<Classifier>,
}

struct Session {
    scan: Scan,
    view: Mutex<ViewCache>,
    retained_processes: Arc<procinfo::Snapshot>,
    responses: Mutex<tree::TreeResponses>,
}

impl Session {
    fn new(scan: Scan, initial_processes: Arc<procinfo::Snapshot>) -> Self {
        Self {
            scan,
            view: Mutex::new(ViewCache::with_snapshot(initial_processes.clone())),
            retained_processes: initial_processes,
            responses: Mutex::new(tree::TreeResponses::default()),
        }
    }
}

#[derive(Default)]
struct Ui {
    state: Option<Value>,
    command: Option<Value>,
    last_seen: f64,
    bye_at: f64,
    agent_seen: f64,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    /// 立即返回；在后台读取软件包数据库和进程信息。
    pub fn new() -> Self {
        let pkgdb = Arc::new(PkgDb::default());
        let classifier = Arc::new(Classifier::new(Some(pkgdb.clone()), &[]));
        let app = Self::build(
            pkgdb,
            classifier,
            ProcInfo::new(),
            false,
            true,
            saved::Location::Default(Store::default()),
        );
        let worker = app.clone();
        if std::thread::Builder::new()
            .name("diskmap-init".into())
            .spawn(move || {
                let loaded = std::panic::catch_unwind(|| {
                    let pkgdb = Arc::new(PkgDb::load());
                    let classifier = Arc::new(Classifier::new(
                        Some(pkgdb.clone()),
                        &procinfo::read_swapfiles(),
                    ));
                    Arc::new(Components { pkgdb, classifier })
                });
                if let Ok(components) = loaded {
                    *worker
                        .inner
                        .components
                        .write()
                        .unwrap_or_else(PoisonError::into_inner) = components;
                }
                *lock(&worker.inner.ready) = true;
                worker.inner.initialized.notify_all();
                worker.inner.procs.refresh();
            })
            .is_err()
        {
            // 线程创建失败仍允许使用空数据库扫描，不让 HTTP 永久等待。
            *lock(&app.inner.ready) = true;
            app.inner.initialized.notify_all();
        }
        app
    }

    /// 注入固定组件供离线调用和确定性测试。不自动刷新进程或写入扫描缓存。
    pub fn with_components(
        pkgdb: Arc<PkgDb>,
        classifier: Arc<Classifier>,
        procs: ProcInfo,
    ) -> Self {
        Self::build(
            pkgdb,
            classifier,
            procs,
            true,
            false,
            saved::Location::Disabled,
        )
    }

    fn build(
        pkgdb: Arc<PkgDb>,
        classifier: Arc<Classifier>,
        procs: ProcInfo,
        ready: bool,
        refresh_on_scan: bool,
        saved: saved::Location,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                components: RwLock::new(Arc::new(Components { pkgdb, classifier })),
                ready: Mutex::new(ready),
                initialized: Condvar::new(),
                scan: Mutex::new(None),
                closed: AtomicBool::new(false),
                procs,
                refresh_on_scan,
                ui: Mutex::new(Ui::default()),
                admin: platform::is_admin(),
                settings_version: AtomicU64::new(0),
                saved: Mutex::new(saved),
            }),
        }
    }

    fn components(&self) -> Arc<Components> {
        self.inner
            .components
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn ready(&self) -> bool {
        *lock(&self.inner.ready)
    }

    pub fn wait_ready(&self) {
        drop(
            self.inner
                .initialized
                .wait_while(lock(&self.inner.ready), |ready| !*ready)
                .unwrap_or_else(PoisonError::into_inner),
        );
    }

    pub fn current_scan(&self) -> Option<Scan> {
        lock(&self.inner.scan)
            .as_ref()
            .map(|session| session.scan.clone())
    }

    pub fn procs(&self) -> &ProcInfo {
        &self.inner.procs
    }
    pub fn pkgdb(&self) -> Arc<PkgDb> {
        self.components().pkgdb.clone()
    }
    pub fn admin(&self) -> bool {
        self.inner.admin
    }

    /// 校验目录后替换扫描；旧任务取消但不等待其系统调用结束。
    pub fn start_scan(&self, path: &str, apparent: bool, cross_fs: bool) -> Result<Value, Text> {
        let root = if path.is_empty() {
            platform::system_root()
        } else {
            platform::norm_path(path)
        };
        if !root.is_dir() {
            return Err(Text::owned(
                format!("文件夹不存在：{}", root.display()),
                format!("Folder does not exist: {}", root.display()),
            ));
        }
        self.wait_ready();
        let components = self.components();
        let mut options = ScanOptions::new(root);
        options.apparent = apparent;
        options.cross_fs = cross_fs;
        {
            let mut slot = lock(&self.inner.scan);
            if self.inner.closed.load(Ordering::Acquire) {
                return Err(Text::new("服务已关闭", "The service is closed"));
            }
            if let Some(previous) = slot.as_ref() {
                previous.scan.cancel();
            }
            if self.inner.refresh_on_scan {
                self.inner.procs.refresh_async();
            }
            let initial_processes = self.inner.procs.snapshot();
            let scan = self.start_managed_scan(options, components.classifier.clone());
            *slot = Some(Arc::new(Session::new(scan, initial_processes)));
        }
        Ok(self.status())
    }

    pub fn cancel(&self) -> Value {
        if let Some(scan) = self.current_scan() {
            scan.cancel();
        }
        self.status()
    }

    /// AI 请求优先复用覆盖目标且统计方式相容的结果，不打断正在查看的扫描。
    /// 与旧版一样，完成后的文件查找只承认前 150 项和扫描开始时的在用文件。
    pub fn agent_scan(
        &self,
        path: &str,
        rescan: bool,
        apparent: bool,
        cross_fs: bool,
    ) -> Result<Value, Text> {
        let root = if path.is_empty() {
            platform::system_root()
        } else {
            platform::norm_path(path)
        };
        let reused = {
            let slot = lock(&self.inner.scan);
            if self.inner.closed.load(Ordering::Acquire) {
                return Err(Text::new("服务已关闭", "The service is closed"));
            }
            !rescan
                && slot.as_ref().is_some_and(|session| {
                    let scan = &session.scan;
                    let state = scan.state();
                    !scan.is_saved()
                        && matches!(state, ScanState::Scanning | ScanState::Done)
                        && scan.apparent() == apparent
                        && (scan.cross_fs() || !cross_fs)
                        && path_is_covered(Os::current(), scan.root(), &root)
                        && (state == ScanState::Scanning || retained_path(session, &root))
                })
        };
        let mut status = if reused {
            self.status()
        } else {
            self.start_scan(&root.to_string_lossy(), apparent, cross_fs)?
        };
        // MFT 记录数只供界面使用，AI 扫描响应沿用原有字段。
        status.as_object_mut().unwrap().remove("mft_records");
        status["reused"] = json!(reused);
        Ok(status)
    }

    /// 取消工作；由 HTTP 句柄、CLI 或桌面窗口的退出流程调用。
    pub fn shutdown(&self) {
        let slot = lock(&self.inner.scan);
        self.inner.closed.store(true, Ordering::Release);
        if let Some(session) = slot.as_ref() {
            session.scan.cancel();
        }
    }

    pub fn refresh_procs(&self) -> Value {
        self.inner.procs.refresh();
        self.invalidate_tree_responses();
        self.status()
    }

    /// 设置发生变化后调用；同时支持由 HTTP 之外的调用方更新设置。
    pub fn invalidate_tree_responses(&self) {
        self.inner.settings_version.fetch_add(1, Ordering::AcqRel);
    }

    pub fn status(&self) -> Value {
        let components = self.components();
        let procs = self.inner.procs.snapshot();
        let (command, agent_seen) = {
            let ui = lock(&self.inner.ui);
            (ui.command.clone(), ui.agent_seen)
        };
        let mut out = json!({
            "ready": self.ready(), "os": Os::current().key(), "admin": self.inner.admin,
            "pkg": components.pkgdb.status(),
            "procs": {"total": procs.nprocs, "visible": procs.visible, "files": procs.file_count(),
                "refreshing": self.inner.procs.refreshing(), "error": procs.error},
            "command": command, "agent_seen_ago": if agent_seen > 0.0 { Some(round(now() - agent_seen, 10.0)) } else { None },
            "state": "idle",
        });
        if let Some(scan) = self.current_scan() {
            let progress = scan_status(&scan.progress());
            out.as_object_mut()
                .unwrap()
                .extend(progress.as_object().unwrap().clone());
            if let Some(saved) = saved::status_marker(&scan) {
                out["saved"] = saved;
            }
        }
        out
    }

    pub fn tree(&self, path: &str, frac: f64) -> Option<Value> {
        let session = lock(&self.inner.scan).clone()?;
        let scan = &session.scan;
        let path = if path.is_empty() {
            scan.root().to_owned()
        } else {
            platform::norm_path(path)
        };
        let snapshot = self.inner.procs.snapshot();
        let components = self.components();
        let state = scan.state();
        self.render_tree(&session, snapshot, &components, &path, frac, state)
    }

    fn render_tree(
        &self,
        session: &Session,
        snapshot: Arc<procinfo::Snapshot>,
        components: &Components,
        path: &Path,
        frac: f64,
        state: ScanState,
    ) -> Option<Value> {
        let scan = &session.scan;
        let mut view = lock(&session.view);
        let index = scan.index();
        let mut out = view.tree(
            &index,
            snapshot,
            path,
            frac,
            Some(&components.pkgdb),
            state.is_finished(),
        )?;
        out["root"] = json!(scan.root().to_string_lossy());
        out["state"] = json!(state.key());
        Some(out)
    }

    pub fn info(&self, path: &str) -> Option<Value> {
        let scan = self.current_scan()?;
        let path = platform::norm_path(path);
        let snapshot = self.inner.procs.snapshot();
        let components = self.components();
        let mut out = view::info(
            &scan.index(),
            &snapshot,
            scan.classifier(),
            Some(&components.pkgdb),
            &path,
        )?;
        // 在索引读锁外做 lstat、用户信息等系统调用。
        let target = out["path"].as_str()?.to_owned();
        let is_mount = {
            let index = scan.index();
            matches!(index.find(&path), Some(crate::index::Found::Dir(id))
                if index.dir(id).flags() & crate::index::MOUNT != 0)
        };
        if is_mount
            && let Some(mount) = platform::mounts::read_mounts().into_iter().find(|mount| {
                platform::path::cmp_paths(
                    Os::current(),
                    mount.path.as_os_str().as_encoded_bytes(),
                    Path::new(&target).as_os_str().as_encoded_bytes(),
                )
                .is_eq()
            })
        {
            out["mount"] = json!({"fstype": mount.fstype, "source": mount.source});
        }
        metadata::append(&mut out, Path::new(&target));
        Some(out)
    }

    pub fn reveal(&self, path: &str) -> Result<(), Text> {
        reveal::reveal(self, path)
    }

    pub fn set_ui_state(&self, body: Value) {
        lock(&self.inner.ui).state = Some(
            json!({"view": body.get("view"), "selected": body.get("selected"), "pinned": body.get("pinned"), "time": now()}),
        );
    }

    pub fn ui_state(&self) -> Option<Value> {
        lock(&self.inner.ui).state.clone()
    }
    pub fn set_command(&self, command: Value) {
        lock(&self.inner.ui).command = Some(command);
    }

    /// HTTP 兼容旧版允许数组 note 的行为；编号和内容须在同一锁内发布。
    pub(crate) fn request_show_value(&self, path: &Path, note: Value, time: f64) {
        let mut ui = lock(&self.inner.ui);
        let id = ui
            .command
            .as_ref()
            .and_then(|command| command["id"].as_u64())
            .unwrap_or(0)
            .saturating_add(1);
        ui.command = Some(
            json!({"id": id, "action": "show", "path": path.to_string_lossy(), "note": note, "time": time}),
        );
    }
    pub fn touch_ui(&self) {
        lock(&self.inner.ui).last_seen = now();
    }
    pub fn touch_agent(&self) {
        lock(&self.inner.ui).agent_seen = now();
    }
    pub fn say_bye(&self) {
        lock(&self.inner.ui).bye_at = now();
    }

    /// 浏览器模式下的退出判断，与旧版的 8 秒告别宽限和 180 秒心跳超时一致。
    pub fn ui_closed(&self, since_start: Duration) -> bool {
        let ui = lock(&self.inner.ui);
        if ui.last_seen == 0.0 {
            return since_start > Duration::from_secs(120);
        }
        let now = now();
        (ui.bye_at > ui.last_seen && now - ui.bye_at > 8.0) || now - ui.last_seen > 180.0
    }
}

impl App {
    /// HTTP 同时取得类型化分析快照和原始界面字段，避免两次取锁混入不同上报。
    pub(crate) fn analysis_snapshot_with_ui_state(
        &self,
    ) -> (crate::analysis::Snapshot, Option<Value>) {
        // 只克隆句柄和少量界面状态；分析拿到快照后才获取索引读锁。
        let scan = lock(&self.inner.scan);
        let components = self.components();
        let ui = lock(&self.inner.ui);
        let ui_state = ui.state.as_ref().map(|state| crate::analysis::UiState {
            view: state.get("view").and_then(Value::as_str).map(Into::into),
            selected: state
                .get("selected")
                .and_then(Value::as_str)
                .map(Into::into),
            pinned: state.get("pinned").cloned().unwrap_or(Value::Null),
            time: state.get("time").and_then(Value::as_f64).unwrap_or(0.0),
        });
        let processes = self.inner.procs.snapshot();
        let snapshot = crate::analysis::Snapshot {
            scan: scan.as_ref().map(|session| session.scan.clone().into()),
            retained_processes: scan.as_ref().map_or_else(
                || processes.clone(),
                |session| session.retained_processes.clone(),
            ),
            processes,
            pkgdb: components.pkgdb.clone(),
            version: crate::VERSION.into(),
            os: Os::current(),
            admin: self.inner.admin,
            ready: self.ready(),
            last_seen: ui.last_seen,
            ui_state,
            now: now(),
        };
        (snapshot, ui.state.clone())
    }
}

impl crate::analysis::Source for App {
    fn snapshot(&self) -> crate::analysis::Snapshot {
        self.analysis_snapshot_with_ui_state().0
    }

    fn request_show(&self, path: &Path, note: &str, time: f64) {
        self.request_show_value(path, json!(note), time);
    }
}

/// 从一次进度快照生成界面扫描字段，便于在各平台上验证 MFT 的状态切换。
fn scan_status(p: &Progress) -> Value {
    let mounts: Vec<_> = p
        .mounts
        .iter()
        .map(|m| json!({"path": m.path.to_string_lossy(), "fstype": m.fstype, "source": m.source}))
        .collect();
    let skipped: Vec<_> = p
        .skipped
        .iter()
        .take(30)
        .map(|m| json!({"path": m.path.to_string_lossy(), "fstype": m.fstype}))
        .collect();
    let samples: Vec<_> = p
        .denied_samples
        .iter()
        .take(8)
        .map(|path| path.to_string_lossy())
        .collect();
    let mut progress = json!({
        "state": p.state.key(), "error": p.error, "root": p.root.to_string_lossy(),
        "files": p.files, "dirs": p.dirs, "bytes": p.bytes, "denied": p.denied,
        "denied_samples": samples, "current": p.current.as_deref().unwrap_or(Path::new("")).to_string_lossy(),
        "elapsed": round(p.elapsed.as_secs_f64(), 100.0), "apparent": p.apparent,
        "cross_fs": p.cross_fs, "fs_used": p.fs_used, "mounts": mounts, "skipped": skipped,
    });
    if p.state == ScanState::Scanning && p.method == ScanMethod::Mft {
        progress["mft_records"] = json!(p.records);
    }
    progress
}

fn path_is_covered(os: Os, root: &Path, path: &Path) -> bool {
    let root = platform::path::path_key(os, root.as_os_str().as_encoded_bytes());
    let path = platform::path::path_key(os, path.as_os_str().as_encoded_bytes());
    path == root
        || (path.starts_with(&root)
            && (root.last() == Some(&(os.sep() as u8))
                || path.get(root.len()) == Some(&(os.sep() as u8))))
}

fn retained_path(session: &Session, path: &Path) -> bool {
    use crate::index::Found;
    use std::cmp::Reverse;
    let index = session.scan.index();
    match index.find(path) {
        Some(Found::Dir(_)) => true,
        Some(Found::File { parent, file }) => {
            if session
                .retained_processes
                .lookup(index.file_path(parent, file))
                .is_some()
            {
                return true;
            }
            let rank = (Reverse(index.file(file).size()), file);
            index
                .dir(parent)
                .files()
                .filter(|entry| (Reverse(entry.size()), entry.id()) < rank)
                .take(150)
                .count()
                < 150
        }
        None => false,
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}
fn round(value: f64, scale: f64) -> f64 {
    (value * scale).round_ties_even() / scale
}

#[cfg(test)]
mod tests;
