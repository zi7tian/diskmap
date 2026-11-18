//! 与 Python 版共用的设置文件；只有显式保存语言时才写入。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::{Map, Value};

use super::{Lang, UnsupportedLanguage, set_default};

// 与旧版使用相同的 .tmp 路径，因此将同一进程里的读改写串行化。
static SAVE_LOCK: Mutex<()> = Mutex::new(());

/// 设置文件位置：`platform::config_dir()/settings.json`。
pub fn settings_path() -> PathBuf {
    crate::platform::config_dir().join("settings.json")
}

/// 读取用户设置；不存在、不可读、损坏或顶层不是对象时返回空对象。
pub fn load_settings() -> Map<String, Value> {
    load_settings_at(&settings_path())
}

/// 从指定文件读取设置，方便测试或便携使用。
pub fn load_settings_at(path: &Path) -> Map<String, Value> {
    fs::read(path)
        .ok()
        .and_then(|data| serde_json::from_slice(&data).ok())
        .and_then(|value| match value {
            Value::Object(settings) => Some(settings),
            _ => None,
        })
        .unwrap_or_default()
}

/// 保存语言并更新进程默认值。无效名称返回错误；写盘失败仍更新本次使用的语言，与旧版相同。
pub fn save_lang(lang: &str) -> Result<Lang, UnsupportedLanguage> {
    save_lang_at(&settings_path(), lang)
}

/// 将语言保存到指定文件；保留其他字段，先写 `.tmp` 再原子替换目标文件。
pub fn save_lang_at(path: &Path, lang: &str) -> Result<Lang, UnsupportedLanguage> {
    let lang = lang.parse::<Lang>()?;
    let _lock = SAVE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut settings = load_settings_at(path);
    settings.insert("lang".to_owned(), Value::String(lang.code().to_owned()));
    let _ = write_settings(path, &settings);
    set_default(lang);
    Ok(lang)
}

fn write_settings(path: &Path, settings: &Map<String, Value>) -> io::Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".tmp");
    let temporary = PathBuf::from(temporary);
    let file = fs::File::create(&temporary)?;
    serde_json::to_writer_pretty(file, settings)?;
    fs::rename(&temporary, path)
}
