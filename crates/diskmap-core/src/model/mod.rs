//! 公共基础类型（T01 负责）。
//!
//! 这里的类型所有模块都会用到。

/// 重要程度类别。
///
/// 编号与旧版完全一致：界面用数字表示类别（JSON 里的 `c` 字段），AI 接口用 [`Category::key`]
/// 的英文名。不要改动顺序或编号。中英文名称和说明由 `classify` 模块提供（T02）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum Category {
    /// 系统核心：引导、内核与驱动、虚拟内存文件。删除可能导致无法开机。
    Core = 0,
    /// 正在使用：被运行中的程序执行、加载或打开的文件。由进程信息在运行时叠加，静态规则不会给出这个类别。
    Running = 1,
    /// 系统文件：操作系统和已安装程序的文件，应卸载而不是手动删除。
    System = 2,
    /// 配置文件：系统和用户设置、注册表、密钥。
    Config = 3,
    /// 用户数据：文档、下载、项目、媒体等个人文件。
    User = 4,
    /// 缓存/临时：通常可以安全清理。
    Cache = 5,
    /// 其他/应用数据。
    Other = 6,
}

/// 类别数量，用于"各类别大小"这类固定长度的数组。
pub const NCLS: usize = 7;

impl Category {
    /// 按编号排列的全部类别。
    pub const ALL: [Category; NCLS] = [
        Category::Core,
        Category::Running,
        Category::System,
        Category::Config,
        Category::User,
        Category::Cache,
        Category::Other,
    ];

    /// 编号（0–6），即界面 JSON 里的数字。
    pub const fn index(self) -> usize {
        self as usize
    }

    /// 由编号得到类别。
    pub const fn from_index(i: usize) -> Option<Category> {
        if i < NCLS { Some(Self::ALL[i]) } else { None }
    }

    /// 英文键名，AI 接口、报告和 JSON 里使用。
    pub const fn key(self) -> &'static str {
        match self {
            Category::Core => "core",
            Category::Running => "running",
            Category::System => "system",
            Category::Config => "config",
            Category::User => "user",
            Category::Cache => "cache",
            Category::Other => "other",
        }
    }

    /// 由英文键名得到类别。
    pub fn from_key(key: &str) -> Option<Category> {
        Self::ALL.into_iter().find(|c| c.key() == key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbering_matches_legacy() {
        // legacy/backend/classify.py：CORE, RUNNING, SYSTEM, CONFIG, USER, CACHE, OTHER = range(7)
        let keys: Vec<_> = Category::ALL.iter().map(|c| c.key()).collect();
        assert_eq!(
            keys,
            [
                "core", "running", "system", "config", "user", "cache", "other"
            ]
        );
        for (i, c) in Category::ALL.into_iter().enumerate() {
            assert_eq!(c.index(), i);
            assert_eq!(Category::from_index(i), Some(c));
            assert_eq!(Category::from_key(c.key()), Some(c));
        }
        assert_eq!(Category::from_index(NCLS), None);
        assert_eq!(Category::from_key("nope"), None);
        // 旧版分类规则里有 dir_cls <= CONFIG 这样的比较，顺序要保持
        assert!(Category::Core < Category::System && Category::System < Category::Config);
    }
}
