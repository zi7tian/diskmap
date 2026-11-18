//! DiskMap 核心库：扫描、完整索引、重要程度分类、分析，以及给界面和 AI 用的本地 HTTP 接口。
//!
//! 行为以 Python 1.3.0 旧版（仓库里的 `legacy/`）为准。

pub mod analysis; // T06 分析与报告
pub mod app; // T05 应用状态
pub mod classify; // T02 重要程度规则
pub mod i18n; // T02 中英双语文本与语言设置
pub mod index; // T01 完整索引：目录树的存储与查询
pub mod model; // T01 公共类型：类别等
pub mod pkgdb; // T02 软件包数据库
pub mod platform; // T01 平台差异：路径、管理员、挂载点、磁盘列表
pub mod procinfo; // T04 正在使用的文件
pub mod saved; // T12 扫描结果存盘
pub mod scan; // T01 通用扫描；scan::ntfs 由 T03 负责
// T05 本地 HTTP 接口与实例登记。只在启用 server 功能时编译（cli 和桌面版都启用）
#[cfg(feature = "server")]
pub mod server;
pub mod view; // T05 给界面的树数据

pub use model::{Category, NCLS};

/// 版本号（来自 Cargo.toml）。
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
