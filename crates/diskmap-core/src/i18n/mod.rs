//! 中英双语文本、请求语言与持久化设置，对应 `legacy/backend/i18n.py`。
//!
//! HTTP 响应应在序列化的线程内创建 [`scope`] 守卫，随后调用 serde；[`Text`] 会直接序列化
//! 成所选语言的字符串。守卫不能跨线程传递，也不要跨异步任务的 `.await` 持有。

mod settings;
mod system;
#[cfg(windows)]
mod windows;

use std::borrow::Cow;
use std::cell::Cell;
use std::ffi::OsStr;
use std::fmt;
use std::marker::PhantomData;
use std::rc::Rc;
use std::str::FromStr;
use std::sync::atomic::{AtomicU8, Ordering};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

pub use settings::{load_settings, load_settings_at, save_lang, save_lang_at, settings_path};
pub use system::{system_lang, system_lang_with};

/// 所支持的语言代码，顺序与旧版相同。
pub const LANGS: [&str; 2] = ["zh", "en"];

/// 界面与输出语言。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Lang {
    /// 中文。
    Zh,
    /// 英文。
    En,
}

impl Lang {
    /// 语言代码，与旧版、设置文件和命令行参数一致。
    pub const fn code(self) -> &'static str {
        match self {
            Lang::Zh => "zh",
            Lang::En => "en",
        }
    }

    /// 宽松解析语言名称，与 [`norm`] 相同。
    pub fn parse(value: &str) -> Option<Self> {
        norm(value)
    }
}

impl fmt::Display for Lang {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

impl FromStr for Lang {
    type Err = UnsupportedLanguage;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        norm(value).ok_or_else(|| UnsupportedLanguage(value.to_owned()))
    }
}

impl Serialize for Lang {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.code())
    }
}

impl<'de> Deserialize<'de> for Lang {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

/// 不支持的语言名称；显示错误时使用当前语言。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnsupportedLanguage(pub String);

impl UnsupportedLanguage {
    /// 给界面或其他调用方保存的双语错误文字。
    pub fn text(&self) -> Text {
        Text::owned(
            format!("不支持的语言：{}", self.0),
            format!("unsupported language: {}", self.0),
        )
    }
}

impl fmt::Display for UnsupportedLanguage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.text().fmt(f)
    }
}

impl std::error::Error for UnsupportedLanguage {}

/// `'zh_CN.UTF-8'` → 中文，`'en-US'` → 英文，其他名称返回 `None`。
/// 与旧版一样，`zh.UTF-8` 不被接受，语言代码后只能跟下划线或连字符。
pub fn norm(value: &str) -> Option<Lang> {
    // Python 的 str.strip() 还将 U+001C..U+001F 视为空白。
    let value = value
        .trim_matches(|ch: char| ch.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&ch))
        .as_bytes();
    if value.len() < 2 || (value.len() > 2 && !matches!(value[2], b'_' | b'-')) {
        return None;
    }
    match [value[0].to_ascii_lowercase(), value[1].to_ascii_lowercase()] {
        [b'z', b'h'] => Some(Lang::Zh),
        [b'e', b'n'] => Some(Lang::En),
        _ => None,
    }
}

/// 中英双语文本，对应旧版的 `T(中文, 英文)`。
/// 固定文字不分配内存；运行时生成的文字用 [`Text::owned`]。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Text {
    pub zh: Cow<'static, str>,
    pub en: Cow<'static, str>,
}

impl Text {
    /// 固定文字。
    pub const fn new(zh: &'static str, en: &'static str) -> Text {
        Text {
            zh: Cow::Borrowed(zh),
            en: Cow::Borrowed(en),
        }
    }

    /// 运行时生成的文字。
    pub fn owned(zh: impl Into<String>, en: impl Into<String>) -> Text {
        Text {
            zh: Cow::Owned(zh.into()),
            en: Cow::Owned(en.into()),
        }
    }

    /// 指定语言的版本。
    pub fn get(&self, lang: Lang) -> &str {
        match lang {
            Lang::Zh => &self.zh,
            Lang::En => &self.en,
        }
    }

    /// 当前请求语言的版本。
    pub fn text(&self) -> &str {
        self.get(current())
    }
}

impl fmt::Display for Text {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(self.text())
    }
}

impl Serialize for Text {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.text())
    }
}

// 0 表示尚未读取默认语言；1、2 分别对应中文、英文。
static DEFAULT: AtomicU8 = AtomicU8::new(0);

thread_local! {
    static LOCAL: Cell<Option<Lang>> = const { Cell::new(None) };
}

fn encoded(lang: Lang) -> u8 {
    match lang {
        Lang::Zh => 1,
        Lang::En => 2,
    }
}

/// 当前线程的临时语言优先，否则返回进程默认语言。
pub fn current() -> Lang {
    if let Some(lang) = LOCAL.with(Cell::get) {
        return lang;
    }
    let mut value = DEFAULT.load(Ordering::Relaxed);
    if value == 0 {
        let found = encoded(resolve(None));
        // 如果此时另一线程显式设置了默认语言，不能用自动检测的结果覆盖它。
        value = match DEFAULT.compare_exchange(0, found, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => found,
            Err(existing) => existing,
        };
    }
    if value == 1 { Lang::Zh } else { Lang::En }
}

/// 设置进程默认语言，不影响已经创建的线程语言守卫。
pub fn set_default(lang: Lang) {
    DEFAULT.store(encoded(lang), Ordering::Relaxed);
}

/// 接受外部语言名称；无效名称回退到英文，与旧版 `set_default` 相同。
pub fn set_default_name(lang: Option<&str>) {
    set_default(lang.and_then(norm).unwrap_or(Lang::En));
}

/// 当前线程的临时语言守卫。按创建的逆序释放；释放时恢复前一个临时语言。
///
/// 守卫不能跨线程转移：
/// ```compile_fail
/// use diskmap_core::i18n::{Lang, scope};
/// let guard = scope(Lang::En);
/// std::thread::spawn(move || drop(guard));
/// ```
#[derive(Debug)]
#[must_use = "请持有守卫直到响应序列化结束"]
pub struct LanguageScope {
    previous: Option<Lang>,
    // Rc 让守卫保持 !Send + !Sync，避免在另一线程恢复错误的线程局部状态。
    _thread_bound: PhantomData<Rc<()>>,
}

impl Drop for LanguageScope {
    fn drop(&mut self) {
        LOCAL.with(|local| local.set(self.previous));
    }
}

/// 在当前线程临时选择语言，可以嵌套；释放守卫后恢复。
pub fn scope(lang: Lang) -> LanguageScope {
    scope_optional(Some(lang))
}

/// 对应旧版的 `use(None)`：未指定语言时保留当前线程的选择。
pub fn scope_optional(lang: Option<Lang>) -> LanguageScope {
    let previous = LOCAL.with(|local| {
        let previous = local.get();
        local.set(lang.or(previous));
        previous
    });
    LanguageScope {
        previous,
        _thread_bound: PhantomData,
    }
}

/// 对应旧版 `L`，当场选择双语字符串，不分配内存。
pub fn text<'a>(zh: &'a str, en: &'a str) -> &'a str {
    match current() {
        Lang::Zh => zh,
        Lang::En => en,
    }
}

/// 对应旧版 `tx(T)`；普通值可直接传给 serde，无需转换。
pub fn tx(value: &Text) -> &str {
    value.text()
}

/// 英文单复数；未提供复数（或为空）时在单数后面加 `s`。
pub fn plural<'a>(n: u64, one: &'a str, many: Option<&'a str>) -> Cow<'a, str> {
    if n == 1 {
        Cow::Borrowed(one)
    } else if let Some(many) = many.filter(|many| !many.is_empty()) {
        Cow::Borrowed(many)
    } else {
        Cow::Owned(format!("{one}s"))
    }
}

/// 将可序列化结构里的所有 [`Text`] 转为所选语言的 JSON 字符串，对应旧版 `loc`。
pub fn loc<T: Serialize + ?Sized>(value: &T, lang: Option<Lang>) -> serde_json::Result<Value> {
    // 旧版 loc 在入口取一次语言，保证序列化过程中默认值变化也不会产生混合语言。
    let _guard = scope(lang.unwrap_or_else(current));
    serde_json::to_value(value)
}

/// 决定默认语言：命令行 > `DISKMAP_LANG` > 设置文件 > 系统语言。
pub fn resolve(cli: Option<&str>) -> Lang {
    if let Some(lang) = cli.and_then(norm) {
        return lang;
    }
    if let Some(lang) = std::env::var("DISKMAP_LANG").ok().as_deref().and_then(norm) {
        return lang;
    }
    if let Some(lang) = load_settings()
        .get("lang")
        .and_then(Value::as_str)
        .and_then(norm)
    {
        return lang;
    }
    system_lang()
}

/// 参数注入版本，不读取进程环境或真实设置文件，适用于测试和嵌入程序。
pub fn resolve_with(
    cli: Option<&str>,
    diskmap_lang: Option<&str>,
    settings: &Map<String, Value>,
    system: Lang,
) -> Lang {
    cli.and_then(norm)
        .or_else(|| diskmap_lang.and_then(norm))
        .or_else(|| settings.get("lang").and_then(Value::as_str).and_then(norm))
        .unwrap_or(system)
}

/// 创建命令行解析器之前提取 `--lang`，以便帮助文字也使用对应语言。
/// 返回原始参数的借用，支持 `args_os()`，不要求其他参数是 UTF-8。
pub fn pre_scan_argv<S: AsRef<OsStr>>(argv: &[S]) -> Option<&OsStr> {
    for (index, arg) in argv.iter().enumerate() {
        let arg = arg.as_ref();
        if arg == "--lang" && index + 1 < argv.len() {
            return Some(argv[index + 1].as_ref());
        }
        if let Some(value) = arg.to_str().and_then(|arg| arg.strip_prefix("--lang=")) {
            return Some(OsStr::new(value));
        }
    }
    None
}

#[cfg(test)]
mod tests;
