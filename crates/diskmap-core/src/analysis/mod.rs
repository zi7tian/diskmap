//! 给 AI 和命令行报告使用的分析接口，对应旧版 `backend.analysis`。
//!
//! 不依赖 app、server 或 view。每次请求取得一次 [`Source::snapshot`]，嵌套查询共用
//! 同一份索引读锁、进度和进程快照。JSON 已按当前 i18n::scope 翻译；应在执行线程设置语言。

mod metadata;
mod query;
mod report;

use crate::classify::Classifier;
use crate::i18n::Text;
use crate::index::Index;
use crate::pkgdb::PkgDb;
use crate::platform::{self, Os};
use crate::procinfo;
use crate::scan::{Progress, Scan};
use serde_json::Value;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// 当前扫描。Live 只克隆句柄，不复制索引；Frozen 供离线报告和假实现使用。
/// 分类器必须是建立该索引时的分类器，不能换用新扫描的原因表。
#[derive(Clone)]
pub enum ScanSnapshot {
    Live(Scan),
    Frozen {
        index: Arc<Index>,
        progress: Box<Progress>,
        classifier: Arc<Classifier>,
    },
}
impl From<Scan> for ScanSnapshot {
    fn from(scan: Scan) -> Self {
        Self::Live(scan)
    }
}

/// 页面状态；time 为 Unix 秒，空路径表示没有对应视图或选择。
#[derive(Clone, Debug, Default)]
pub struct UiState {
    pub view: Option<PathBuf>,
    pub selected: Option<PathBuf>,
    /// 页面发送类别键名或 null；保留旧版对上报 JSON 原样回传的语义。
    pub pinned: Value,
    pub time: f64,
}

/// 一次轻量快照。Source 在短暂的应用锁内取得，再释放锁后执行分析。
/// pkgdb 必须与扫描分类器匹配；processes 在整个查询中不变。
#[derive(Clone)]
pub struct Snapshot {
    pub scan: Option<ScanSnapshot>,
    pub processes: Arc<procinfo::Snapshot>,
    /// 扫描开始时的在用名单，固定 150 个文件以外额外保留哪些项；刷新进程不能替换它。
    pub retained_processes: Arc<procinfo::Snapshot>,
    pub pkgdb: Arc<PkgDb>,
    pub version: String,
    pub os: Os,
    pub admin: bool,
    pub ready: bool,
    /// 最近界面请求的 Unix 秒；0 表示从未连接。AI 请求不能更新它。
    pub last_seen: f64,
    pub ui_state: Option<UiState>,
    /// 请求开始的 Unix 秒；可注入以测试连接边界。
    pub now: f64,
}
impl Snapshot {
    /// 未扫描、未连接的默认状态；不加载软件包、采集进程或写配置。
    pub fn new(processes: Arc<procinfo::Snapshot>, pkgdb: Arc<PkgDb>) -> Self {
        Self {
            scan: None,
            retained_processes: Arc::clone(&processes),
            processes,
            pkgdb,
            version: env!("CARGO_PKG_VERSION").into(),
            os: Os::current(),
            admin: platform::is_admin(),
            ready: false,
            last_seen: 0.0,
            ui_state: None,
            now: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64(),
        }
    }
}

/// 应用接口；不依赖 HTTP 或窗口，不负责开始扫描。
///
/// snapshot 应在同一应用锁内克隆扫描、对应软件包库和界面状态，然后立即释放锁；
/// 分析会持有索引读锁，调用者不能带着应用锁进入本模块。
/// request_show 只更新内存中的命令，不打开文件、不写磁盘、不刷新 last_seen。
/// 实现者须在应用锁内原子执行：id = 上次 id + 1（初次为 1），action = "show"，
/// path/note/time 使用传入值。note 已截到 200 个 Unicode 字符，path 已验证并还原索引拼写。
/// 无窗口时仍记录命令。HTTP 层自行记录 agent_seen，并把 AnalysisError 映射为旧版错误响应。
pub trait Source {
    fn snapshot(&self) -> Snapshot;
    fn request_show(&self, path: &Path, note: &str, now: f64);
}

/// 保留中英两份文字；Display 使用当前请求语言。
#[derive(Clone, Debug)]
pub struct AnalysisError(pub Text);
impl fmt::Display for AnalysisError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0.text())
    }
}
impl std::error::Error for AnalysisError {}
pub type Result<T> = std::result::Result<T, AnalysisError>;

/// 与旧版 human 相同的 1024 进制大小显示。
pub fn human(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut n = bytes as f64;
    for unit in ["KB", "MB", "GB", "TB", "PB"] {
        n /= 1024.0;
        if n < 1024.0 || unit == "PB" {
            return if n >= 100.0 {
                format!("{n:.0} {unit}")
            } else if n >= 10.0 {
                format!("{n:.1} {unit}")
            } else {
                format!("{n:.2} {unit}")
            };
        }
    }
    unreachable!()
}

/// 未扫描也成功，scan 为 null。
pub fn status(source: &(impl Source + ?Sized)) -> Value {
    query::status(&source.snapshot(), None)
}
/// top 限制到 1..=50，通常传 10；None 或空路径使用扫描根。
pub fn summary(source: &(impl Source + ?Sized), path: Option<&Path>, top: i64) -> Result<Value> {
    query::with_context(&source.snapshot(), |c| c.summary(path, top))
}
/// limit 限制到 1..=500，通常传 50。
pub fn list_dir(source: &(impl Source + ?Sized), path: Option<&Path>, limit: i64) -> Result<Value> {
    query::with_context(&source.snapshot(), |c| c.list_dir(path, limit))
}
/// 只有此接口读取目标当前的 lstat 元数据，不读取文件内容。
pub fn explain(source: &(impl Source + ?Sized), path: &Path) -> Result<Value> {
    query::with_context(&source.snapshot(), |c| c.explain(path))
}
/// limit 限制到 1..=200，通常传 30；默认阈值为 1 << 20。
/// reclaimable_bytes 在筛选和截断之前求和，也包含在用项。
pub fn cleanup(
    source: &(impl Source + ?Sized),
    path: Option<&Path>,
    limit: i64,
    min_bytes: u64,
) -> Result<Value> {
    query::with_context(&source.snapshot(), |c| c.cleanup(path, limit, min_bytes))
}
/// limit 限制到 1..=200，通常传 20；类别过滤使用静态类别，running 例外。
pub fn largest_files(
    source: &(impl Source + ?Sized),
    path: Option<&Path>,
    limit: i64,
    category: Option<&str>,
) -> Result<Value> {
    query::with_context(&source.snapshot(), |c| {
        c.largest_files(path, limit, category)
    })
}
/// 超过 30 秒无界面请求时失败；嵌套查询失败仅省略对应字段。
pub fn user_view(source: &(impl Source + ?Sized)) -> Result<Value> {
    query::user_view(&source.snapshot())
}
/// 验证路径并登记页面跳转命令；无窗口也成功，不打开文件管理器。
pub fn show(source: &(impl Source + ?Sized), path: &Path, note: Option<&str>) -> Result<Value> {
    let snapshot = source.snapshot();
    let (target, result) = query::with_context(&snapshot, |c| c.show(path))?;
    // 此时索引读锁已释放，避免应用锁与索引锁顺序反转。
    let note: String = note.unwrap_or_default().chars().take(200).collect();
    source.request_show(&target, &note, snapshot.now);
    Ok(result)
}
/// 字段为 status、summary、cleanup，通常 top=15。
pub fn report_json(
    source: &(impl Source + ?Sized),
    path: Option<&Path>,
    top: i64,
) -> Result<Value> {
    query::with_context(&source.snapshot(), |c| c.report_json(path, top))
}
/// 中英文字、空行和末尾换行与旧版相同。
pub fn report_markdown(
    source: &(impl Source + ?Sized),
    path: Option<&Path>,
    top: i64,
) -> Result<String> {
    query::with_context(&source.snapshot(), |c| {
        Ok(report::markdown(
            &c.report_json(path, top)?,
            c.progress.apparent,
        ))
    })
}
