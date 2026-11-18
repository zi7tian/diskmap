use std::fs;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Barrier, Mutex, MutexGuard};

use serde_json::json;

use super::*;
use crate::platform::Os;

const CACHE: Text = Text::new("缓存文件夹", "Cache folder");
static TEST_DEFAULT_LOCK: Mutex<()> = Mutex::new(());
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

type TestResult = Result<(), Box<dyn std::error::Error>>;

// 只有测试进程默认值的用例才持有此锁，其他用例一律使用线程局部语言。
struct DefaultGuard {
    old: u8,
    _lock: MutexGuard<'static, ()>,
}

impl DefaultGuard {
    fn new() -> Self {
        let lock = TEST_DEFAULT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let old = DEFAULT.load(Ordering::Relaxed);
        set_default(Lang::En);
        Self { old, _lock: lock }
    }
}

impl Drop for DefaultGuard {
    fn drop(&mut self) {
        DEFAULT.store(self.old, Ordering::Relaxed);
    }
}

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> std::io::Result<Self> {
        let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("diskmap-i18n-{}-{id}", std::process::id()));
        // 使用 create_dir，意外重名时不能复用或删除原有目录。
        fs::create_dir(&path)?;
        Ok(Self(path))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn env_of(values: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    move |key| {
        values
            .iter()
            .find(|(name, _)| *name == key)
            .map(|(_, value)| (*value).to_owned())
    }
}

#[test]
fn normalization_matches_legacy() {
    for value in [
        "zh",
        "ZH",
        "zh_CN.UTF-8",
        " zh-Hans-CN ",
        "zh_",
        "zh-",
        "\u{1c}zh\u{1f}",
    ] {
        assert_eq!(norm(value), Some(Lang::Zh), "{value}");
    }
    for value in ["en", "EN-US", "en_GB.UTF-8", "\t en \n"] {
        assert_eq!(norm(value), Some(Lang::En), "{value}");
    }
    for value in [
        "", " ", "e", "fr_FR", "C", "POSIX", "zh.UTF-8", "english", "zho", "中文",
    ] {
        assert_eq!(norm(value), None, "{value}");
    }
    assert_eq!(Lang::parse("en-US"), Some(Lang::En));
}

#[test]
fn text_in_both_languages() {
    assert_eq!(CACHE.get(Lang::Zh), "缓存文件夹");
    assert_eq!(CACHE.get(Lang::En), "Cache folder");
    let name = "Steam";
    let value = Text::owned(
        format!("已安装的程序：{name}"),
        format!("Installed program: {name}"),
    );
    assert_eq!(value.get(Lang::En), "Installed program: Steam");
    assert_eq!(Lang::Zh.code(), "zh");
    assert_eq!(Lang::En.code(), "en");
}

#[test]
fn nested_scopes_restore_language_and_optional_scope_preserves_it() {
    let _outer = scope(Lang::En);
    assert_eq!(format!("[{CACHE}]"), "[Cache folder]");
    {
        let _inner = scope(Lang::Zh);
        let _unchanged = scope_optional(None);
        assert_eq!(current(), Lang::Zh);
        assert_eq!(CACHE.to_string(), "缓存文件夹");
        assert_eq!(text("中", "en"), "中");
        assert_eq!(tx(&CACHE), "缓存文件夹");
    }
    assert_eq!(current(), Lang::En);
    assert_eq!(text("中", "en"), "en");
}

#[test]
fn panic_unwinds_language_scope() {
    let _outer = scope(Lang::En);
    let result = std::panic::catch_unwind(|| {
        let _inner = scope(Lang::Zh);
        panic!("测试守卫恢复");
    });
    assert!(result.is_err());
    assert_eq!(current(), Lang::En);
}

#[test]
fn simultaneous_threads_have_independent_scopes() {
    let barrier = Arc::new(Barrier::new(3));
    let threads: Vec<_> = [Lang::Zh, Lang::En]
        .into_iter()
        .map(|lang| {
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                let _guard = scope(lang);
                barrier.wait();
                assert_eq!(current(), lang);
                assert_eq!(CACHE.to_string(), CACHE.get(lang));
            })
        })
        .collect();
    let _guard = scope(Lang::Zh);
    barrier.wait();
    assert_eq!(current(), Lang::Zh);
    for thread in threads {
        assert!(thread.join().is_ok());
    }
}

#[test]
fn process_default_is_used_outside_scopes_and_is_not_inherited_from_parent_thread() {
    let _default = DefaultGuard::new();
    assert_eq!(current(), Lang::En);
    {
        let _scope = scope(Lang::Zh);
        assert_eq!(std::thread::spawn(current).join().ok(), Some(Lang::En));
        set_default(Lang::Zh);
        assert_eq!(std::thread::spawn(current).join().ok(), Some(Lang::Zh));
        set_default_name(Some("unsupported"));
        assert_eq!(current(), Lang::Zh);
    }
    assert_eq!(current(), Lang::En);
    set_default_name(Some("ZH_cn"));
    assert_eq!(current(), Lang::Zh);
    set_default_name(None);
    assert_eq!(current(), Lang::En);
}

#[test]
fn nested_structures_are_localized_during_serialization() -> TestResult {
    #[derive(Serialize)]
    struct Response {
        message: Text,
        nested: Vec<(Text, u32, Option<Text>)>,
    }
    let response = Response {
        message: CACHE,
        nested: vec![(Text::new("目录", "Directory"), 2, None)],
    };
    let _guard = scope(Lang::Zh);
    assert_eq!(
        loc(&response, Some(Lang::En))?,
        json!({"message": "Cache folder", "nested": [["Directory", 2, null]]})
    );
    assert_eq!(current(), Lang::Zh);
    assert_eq!(
        loc(&response, None)?,
        json!({"message": "缓存文件夹", "nested": [["目录", 2, null]]})
    );
    assert_eq!(serde_json::to_string(&CACHE)?, "\"缓存文件夹\"");
    Ok(())
}

#[test]
fn lang_serialization_and_errors() -> TestResult {
    let _guard = scope(Lang::En);
    assert_eq!(serde_json::to_string(&Lang::Zh)?, "\"zh\"");
    assert_eq!(serde_json::from_str::<Lang>("\"en-US\"")?, Lang::En);
    assert!(serde_json::from_str::<Lang>("\"fr\"").is_err());
    let error = "fr".parse::<Lang>().expect_err("不支持法语");
    assert_eq!(error.to_string(), "unsupported language: fr");
    assert_eq!(error.text().get(Lang::Zh), "不支持的语言：fr");
    assert_eq!(Lang::Zh.to_string(), "zh");
    Ok(())
}

#[test]
fn loc_snapshots_default_for_entire_structure() -> TestResult {
    struct ChangeDefault;
    impl Serialize for ChangeDefault {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            set_default(Lang::Zh);
            serializer.serialize_unit()
        }
    }
    let _default = DefaultGuard::new();
    assert_eq!(
        loc(&(CACHE, ChangeDefault, CACHE), None)?,
        json!(["Cache folder", null, "Cache folder"])
    );
    assert_eq!(current(), Lang::Zh);
    Ok(())
}

#[test]
fn plural_matches_legacy() {
    assert_eq!(plural(1, "file", None), "file");
    assert_eq!(plural(0, "file", None), "files");
    assert_eq!(plural(2, "directory", Some("directories")), "directories");
    assert_eq!(plural(2, "file", Some("")), "files");
}

#[test]
fn resolve_priority_and_invalid_values() {
    let mut settings = Map::new();
    settings.insert("lang".into(), json!("zh_CN"));
    assert_eq!(
        resolve_with(Some("en"), Some("zh"), &settings, Lang::Zh),
        Lang::En
    );
    assert_eq!(
        resolve_with(Some("invalid"), Some("en-US"), &settings, Lang::Zh),
        Lang::En
    );
    assert_eq!(
        resolve_with(None, Some("invalid"), &settings, Lang::En),
        Lang::Zh
    );
    settings.insert("lang".into(), json!(42));
    assert_eq!(resolve_with(None, None, &settings, Lang::Zh), Lang::Zh);
    settings.clear();
    assert_eq!(resolve_with(None, None, &settings, Lang::En), Lang::En);
    // 有效命令行参数会短路，因此该调用不会访问用户设置或真实 locale。
    assert_eq!(resolve(Some("en-US")), Lang::En);
}

#[test]
fn system_environment_precedence_and_posix_fallbacks() {
    let env = env_of(&[("LANGUAGE", "zh_CN:en_US"), ("LC_ALL", "en_US")]);
    assert_eq!(system_lang_with(Os::Linux, env, Some(Lang::En)), Lang::Zh);
    let env = env_of(&[
        ("LANGUAGE", "C:zh_CN"),
        ("LC_ALL", "POSIX"),
        ("LC_MESSAGES", "C.UTF-8"),
        ("LANG", "zh_TW"),
    ]);
    assert_eq!(system_lang_with(Os::Linux, env, None), Lang::Zh);
    let env = env_of(&[("LANGUAGE", "de_DE:zh_CN"), ("LANG", "zh_TW")]);
    assert_eq!(system_lang_with(Os::Linux, env, None), Lang::En);
    assert_eq!(system_lang_with(Os::Linux, env_of(&[]), None), Lang::En);
    let env = env_of(&[("LC_ALL", "en_US"), ("LC_MESSAGES", "zh_CN")]);
    assert_eq!(system_lang_with(Os::Linux, env, None), Lang::En);
}

#[test]
fn native_language_takes_precedence_on_windows_and_macos() {
    for os in [Os::Windows, Os::Macos] {
        assert_eq!(
            system_lang_with(os, env_of(&[("LANG", "en_US")]), Some(Lang::Zh)),
            Lang::Zh
        );
        assert_eq!(
            system_lang_with(os, env_of(&[("LANG", "zh_CN")]), Some(Lang::En)),
            Lang::En
        );
        assert_eq!(
            system_lang_with(os, env_of(&[("LANG", "zh_CN")]), None),
            Lang::Zh
        );
    }
}

#[test]
fn macos_apple_languages_uses_first_entry() {
    assert_eq!(
        system::mac_language("(\n    \"zh-Hans-CN\",\n    en\n)\n"),
        Some(Lang::Zh)
    );
    assert_eq!(
        system::mac_language("(\n    fr-FR,\n    zh-Hans\n)\n"),
        Some(Lang::En)
    );
    assert_eq!(system::mac_language("(\n)\n"), None);
    assert_eq!(system::mac_language(""), None);
}

#[test]
fn argv_prescan_handles_both_forms_and_first_match() {
    assert_eq!(
        pre_scan_argv(&["/", "--lang", "en"]),
        Some(OsStr::new("en"))
    );
    assert_eq!(pre_scan_argv(&["--lang=zh"]), Some(OsStr::new("zh")));
    assert_eq!(
        pre_scan_argv(&["--lang=en", "--lang", "zh"]),
        Some(OsStr::new("en"))
    );
    assert_eq!(pre_scan_argv(&["--lang"]), None);
    assert_eq!(pre_scan_argv(&["--language", "en"]), None);
    assert_eq!(pre_scan_argv(&["--lang="]), Some(OsStr::new("")));
}

#[cfg(unix)]
#[test]
fn argv_prescan_accepts_non_utf8_paths() {
    use std::os::unix::ffi::OsStrExt;
    let argv = [OsStr::from_bytes(b"/home/\xff"), OsStr::new("--lang=en")];
    assert_eq!(pre_scan_argv(&argv), Some(OsStr::new("en")));
}

#[test]
fn unreadable_or_malformed_settings_are_empty() -> TestResult {
    let temp = TempDir::new()?;
    let path = temp.0.join("settings.json");
    assert!(load_settings_at(&path).is_empty());
    assert!(load_settings_at(&temp.0).is_empty());
    for invalid in ["{", "[]", "null", "42", "\"en\""] {
        fs::write(&path, invalid)?;
        assert!(load_settings_at(&path).is_empty(), "{invalid}");
    }
    fs::write(&path, [0xff, 0xfe])?;
    assert!(load_settings_at(&path).is_empty());
    Ok(())
}

#[test]
fn settings_round_trip_preserves_fields_and_replaces_existing_file() -> TestResult {
    let _default = DefaultGuard::new();
    let temp = TempDir::new()?;
    let path = temp.0.join("nested/settings.json");
    assert_eq!(save_lang_at(&path, "ZH_cn")?, Lang::Zh);
    assert_eq!(current(), Lang::Zh);
    fs::write(
        &path,
        "{\"lang\":\"zh\",\"extra\":{\"title\":\"目录\",\"n\":2},\"flag\":true}",
    )?;
    assert_eq!(save_lang_at(&path, "en-US")?, Lang::En);
    assert_eq!(current(), Lang::En);
    assert_eq!(
        Value::Object(load_settings_at(&path)),
        json!({"lang":"en","extra":{"title":"目录","n":2},"flag":true})
    );
    let written = fs::read_to_string(&path)?;
    assert!(written.contains("目录"));
    assert!(written.contains("\n  \""));
    assert!(!path.with_file_name("settings.json.tmp").exists());
    let before = written;
    assert!(save_lang_at(&path, "fr").is_err());
    assert_eq!(fs::read_to_string(&path)?, before);
    assert_eq!(current(), Lang::En);
    Ok(())
}

#[test]
fn failed_persistence_still_updates_default_without_panicking() -> TestResult {
    let _default = DefaultGuard::new();
    let temp = TempDir::new()?;
    let blocker = temp.0.join("file");
    fs::write(&blocker, b"not a directory")?;
    assert_eq!(
        save_lang_at(&blocker.join("settings.json"), "zh")?,
        Lang::Zh
    );
    assert_eq!(current(), Lang::Zh);
    assert_eq!(fs::read(&blocker)?, b"not a directory");
    Ok(())
}
