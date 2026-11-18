//! T06：移植旧版 test_agent 的分析测试，并在同一临时目录比较 Python 假 App 的结果。
//! Python 对照程序只排序 scandir 输出以固定同大小项次序；不会启动服务或读取用户设置。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use diskmap_core::analysis::{self, ScanSnapshot, Snapshot, Source, UiState};
use diskmap_core::classify::{Classifier, ClassifierOptions};
use diskmap_core::i18n::{self, Lang};
use diskmap_core::index::{DENIED, Found};
use diskmap_core::pkgdb::PkgDb;
use diskmap_core::platform::Os;
use diskmap_core::procinfo::{Kind, ProcInfo, ProcessMap, Usage};
use diskmap_core::scan::{Scan, ScanOptions, ScanState};
use serde_json::{Value, json};

const NOW: f64 = 1_700_000_001.25;
const TOTAL: u64 = 5_203_000;

#[path = "support/child.rs"]
mod child;
use child::command_output;

struct TempDir(PathBuf);
impl TempDir {
    fn new() -> Self {
        // 本文件的测试在同一进程里并行运行，macOS 的时钟只精确到微秒，只用时间会重名，所以再加一个计数器。
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "diskmap-analysis-{}-{nonce}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct FakeSource {
    state: Snapshot,
    command: Mutex<Option<Value>>,
}
impl Source for FakeSource {
    fn snapshot(&self) -> Snapshot {
        self.state.clone()
    }
    fn request_show(&self, path: &Path, note: &str, now: f64) {
        let mut slot = self.command.lock().unwrap();
        let id = slot
            .as_ref()
            .map_or(1, |old| old["id"].as_u64().unwrap() + 1);
        *slot = Some(json!({"id":id,"action":"show","path":path,"note":note,"time":now}));
    }
}

fn write(path: &Path, size: usize) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, vec![b'x'; size]).unwrap();
}

// PathBuf::join 在 Windows 上不会改写参数内部的 '/'，逐组件拼接才与扫描器一致。
fn fixture_path(root: &Path, relative: &str) -> PathBuf {
    relative
        .split('/')
        .fold(root.to_path_buf(), |path, part| path.join(part))
}

fn make_tree(root: &Path) {
    for (name, size) in [
        ("docs/Report.pdf", 400_000),
        ("proj/cache/blob.bin", 2_500_000),
        ("proj/__pycache__/m.pyc", 1_200_000),
        ("proj/Build.log", 1_100_000),
        ("proj/main.py", 3_000),
    ] {
        write(&fixture_path(&root.join("basic"), name), size);
    }
    for i in 0..165 {
        let extension = if i == 0 { "data" } else { "log" };
        write(
            &root.join("dense").join(format!("f{i:03}.{extension}")),
            1000 + i,
        );
    }
    for name in [
        "a/a.data", "a/z.data", "b/a.data", "b/z.data", "a.data", "z.data",
    ] {
        write(&fixture_path(&root.join("ties"), name), 100);
    }
    fs::create_dir_all(root.join("empty").join("child")).unwrap();
    for (name, size) in [
        ("core.sys", 100),
        ("installed.bin", 200),
        ("settings.ini", 300),
        (".config-file", 350),
        (".app/state.bin", 400),
        ("personal.txt", 500),
        (".cache/blob.dat", 600),
    ] {
        write(&fixture_path(&root.join("classes"), name), size);
    }
}
fn source(root: &Path, busy: bool) -> FakeSource {
    let os = Os::current();
    let mut db = PkgDb::new(os);
    let mut swapfiles = Vec::new();
    let mut options = ClassifierOptions::for_os(os);
    options.homes = Some(vec![root.parent().unwrap().to_path_buf()]);
    if root.ends_with("classes") {
        db.insert_file(&root.join("installed.bin"), "test-package");
        db.insert_file(&root.join("settings.ini"), "test-package");
        db.insert_backup(&root.join("settings.ini"));
        swapfiles.push(root.join("core.sys"));
        options.homes.as_mut().unwrap().push(root.to_path_buf());
    }
    let db = Arc::new(db);
    let classifier = Arc::new(Classifier::with_options(
        Some(db.clone()),
        &swapfiles,
        options,
    ));
    let mut opts = ScanOptions::new(root);
    opts.apparent = true;
    opts.cross_fs = false;
    opts.threads = 1;
    let scan = Scan::start(opts, classifier.clone());
    assert_eq!(scan.wait(), ScanState::Done);
    let mut progress = scan.progress();
    progress.elapsed = Duration::from_secs_f64(12.25);
    progress.finished_at = Some(UNIX_EPOCH + Duration::from_secs(1_700_000_000));
    progress.fs_used = 0;
    let procs = ProcInfo::with_os(os);
    if busy {
        let mut slot = ProcessMap::default();
        for i in 0..12 {
            slot.insert(
                100 + i,
                Usage::new(
                    format!("worker{}", i % 3),
                    if i % 2 == 0 { Kind::Exe } else { Kind::Open },
                ),
            );
        }
        let mut solo = ProcessMap::default();
        solo.insert(7, Usage::new("viewer", Kind::Open));
        let raw = if root.ends_with("dense") {
            vec![
                (root.join("f000.data"), slot),
                (root.join("f160.log"), solo),
            ]
        } else {
            vec![
                (fixture_path(root, "proj/Build.log"), slot),
                (fixture_path(root, "docs/Report.pdf"), solo),
            ]
        };
        procs.install(raw);
    }
    let mut processes = (*procs.snapshot()).clone();
    processes.nprocs = 12;
    processes.visible = 10;
    let mut state = Snapshot::new(Arc::new(processes), db);
    state.scan = Some(ScanSnapshot::Frozen {
        index: Arc::new(scan.index().clone()),
        progress: Box::new(progress),
        classifier,
    });
    state.version = "test".into();
    state.admin = false;
    state.ready = true;
    state.now = NOW;
    FakeSource {
        state,
        command: Mutex::new(None),
    }
}
fn value(result: analysis::Result<Value>) -> Value {
    result.unwrap_or_else(|error| json!({"error":error.to_string()}))
}
fn common(source: &FakeSource, root: &Path) -> Value {
    json!({
        "status":analysis::status(source),
        "summary":value(analysis::summary(source,None,50)),
        "summary_low":value(analysis::summary(source,Some(Path::new("")),-3)),
        "list":value(analysis::list_dir(source,None,1000)),
        "list_low":value(analysis::list_dir(source,None,-3)),
        "explain_root":value(analysis::explain(source,root)),
        "cleanup":value(analysis::cleanup(source,None,1000,0)),
        "cleanup_filtered":value(analysis::cleanup(source,None,1,u64::MAX)),
        "largest":value(analysis::largest_files(source,None,1000,None)),
        "largest_low":value(analysis::largest_files(source,None,-3,None)),
        "largest_cache":value(analysis::largest_files(source,None,200,Some("cache"))),
        "largest_user":value(analysis::largest_files(source,None,200,Some("user"))),
        "largest_core":value(analysis::largest_files(source,None,200,Some("core"))),
        "largest_system":value(analysis::largest_files(source,None,200,Some("system"))),
        "largest_config":value(analysis::largest_files(source,None,200,Some("config"))),
        "largest_other":value(analysis::largest_files(source,None,200,Some("other"))),
        "largest_running":value(analysis::largest_files(source,None,200,Some("running"))),
        "invalid_category":value(analysis::largest_files(source,None,20,Some("nonsense"))),
        "empty_category":value(analysis::largest_files(source,None,20,Some(""))),
        "missing_summary":value(analysis::summary(source,Some(&root.join("missing")),5)),
        "missing_explain":value(analysis::explain(source,&root.join("missing"))),
        "missing_show":value(analysis::show(source,&root.join("missing"),None)),
        "user_view_missing":value(analysis::user_view(source)),
        "report_json":value(analysis::report_json(source,None,5)),
        "report_markdown":analysis::report_markdown(source,None,5).unwrap(),
    })
}
fn basic_extras(source: &mut FakeSource, root: &Path) -> Value {
    let doc = fixture_path(root, "docs/Report.pdf");
    let proj = root.join("proj");
    let mut result = json!({
        "explain_file":value(analysis::explain(source,&doc)),
        "explain_used":value(analysis::explain(source,&fixture_path(root,"proj/Build.log"))),
        "explain_dir":value(analysis::explain(source,&proj)),
        "list_file":value(analysis::list_dir(source,Some(&doc),50)),
        "cleanup_cache_root":value(analysis::cleanup(source,Some(&fixture_path(root,"proj/cache")),30,0)),
        "show_disconnected":value(analysis::show(source,&doc,Some("没有窗口"))),
    });
    source.state.last_seen = NOW;
    source.state.ui_state = Some(UiState {
        view: Some(proj),
        selected: Some(fixture_path(root, "proj/Build.log")),
        pinned: json!("cache"),
        time: NOW - 2.25,
    });
    result["user_view"] = value(analysis::user_view(source));
    result["status_with_view"] = analysis::status(source);
    result["show_connected"] = value(analysis::show(
        source,
        &root.join("docs"),
        Some(&"中🦀".repeat(105)),
    ));
    result["command"] = source.command.lock().unwrap().clone().unwrap();
    source.state.last_seen = NOW - 15.0;
    result["window_at_15"] = analysis::status(source);
    source.state.last_seen = NOW - 30.0;
    result["view_at_30"] = value(analysis::user_view(source));
    source.state.last_seen = NOW - 30.01;
    result["view_after_30"] = value(analysis::user_view(source));
    source.state.last_seen = NOW;
    source.state.ui_state = Some(UiState {
        view: Some(root.join("missing")),
        selected: Some(root.join("missing")),
        pinned: Value::Null,
        time: NOW,
    });
    result["view_missing_paths"] = value(analysis::user_view(source));
    source.state.ui_state = None;
    source.state.last_seen = 0.0;
    if let Some(ScanSnapshot::Frozen { progress, .. }) = &mut source.state.scan {
        progress.state = ScanState::Scanning;
        progress.finished_at = None;
        progress.current = None;
    }
    result["initial_status"] = analysis::status(source);
    for (name, state) in [
        ("scanning", ScanState::Scanning),
        ("cancelled", ScanState::Cancelled),
        ("error", ScanState::Error),
    ] {
        if let Some(ScanSnapshot::Frozen { progress, .. }) = &mut source.state.scan {
            progress.state = state;
            progress.finished_at = None;
            progress.current = Some(root.to_path_buf());
        }
        result[name] = json!({
            "status":analysis::status(source),
            "summary":value(analysis::summary(source,None,5)),
            "list":value(analysis::list_dir(source,None,5)),
            "cleanup":value(analysis::cleanup(source,None,5,0)),
            "largest":value(analysis::largest_files(source,None,5,None)),
            "markdown":analysis::report_markdown(source,None,5).unwrap(),
        });
    }
    if let Some(ScanSnapshot::Frozen {
        index, progress, ..
    }) = &mut source.state.scan
    {
        let mut amended = (**index).clone();
        if let Some(Found::Dir(dir)) = amended.find(&root.join("docs")) {
            amended.add_flags(dir, DENIED);
        }
        *index = Arc::new(amended);
        progress.state = ScanState::Done;
        progress.finished_at = Some(UNIX_EPOCH + Duration::from_secs(1_700_000_000));
        progress.current = None;
        progress.denied = 1;
    }
    result["denied"] = json!({"status":analysis::status(source),"explain":value(analysis::explain(source,&root.join("docs"))),"markdown":analysis::report_markdown(source,None,5).unwrap()});
    source.state.scan = None;
    result["no_scan"] = json!({"status":analysis::status(source),"summary":value(analysis::summary(source,None,5)),"explain":value(analysis::explain(source,&doc)),"show":value(analysis::show(source,&doc,None))});
    result
}
fn collect(root: &Path) -> Value {
    let mut languages = json!({});
    for lang in [Lang::Zh, Lang::En] {
        let _scope = i18n::scope(lang);
        let mut output = json!({});
        for (name, busy) in [
            ("basic", false),
            ("busy", true),
            ("dense", false),
            ("dense_busy", true),
            ("ties", false),
            ("empty", false),
            ("classes", false),
        ] {
            let directory = if name == "busy" {
                "basic"
            } else if name == "dense_busy" {
                "dense"
            } else {
                name
            };
            let path = root.join(directory);
            let mut source = source(&path, busy);
            let index = match &source.state.scan {
                Some(ScanSnapshot::Frozen { index, .. }) => Arc::clone(index),
                _ => unreachable!(),
            };
            output[name] = common(&source, &path);
            if name == "basic" || name == "busy" {
                output[name]["extras"] = basic_extras(&mut source, &path);
            }
            if name == "classes" {
                output[name]["explanations"] = [
                    "core.sys",
                    "installed.bin",
                    "settings.ini",
                    ".config-file",
                    ".app/state.bin",
                    "personal.txt",
                    ".cache/blob.dat",
                ]
                .map(|name| value(analysis::explain(&source, &fixture_path(&path, name))))
                .into();
            }
            assert_proc_path_spelling(&output[name], &index);
        }
        output["human"] = [
            0,
            1,
            1023,
            1024,
            10_239,
            10_240,
            102_399,
            102_400,
            1_048_575,
            1_048_576,
            1_u64 << 50,
            u64::MAX,
        ]
        .map(analysis::human)
        .into_iter()
        .map(Value::String)
        .collect();
        languages[lang.code()] = output;
    }
    languages
}

#[test]
fn legacy_agent_analysis_semantics() {
    let tmp = TempDir::new();
    make_tree(&tmp.0);
    let root = tmp.0.join("basic");
    let source = source(&root, false);
    let _scope = i18n::scope(Lang::Zh);
    let summary = analysis::summary(&source, None, 5).unwrap();
    assert_eq!(summary["bytes"], TOTAL);
    assert_eq!(
        summary["by_category"]
            .as_array()
            .unwrap()
            .iter()
            .map(|cat| cat["bytes"].as_u64().unwrap())
            .sum::<u64>(),
        TOTAL
    );
    assert_eq!(
        summary["largest_files"][0]["path"],
        json!(fixture_path(&root, "proj/cache/blob.bin"))
    );
    assert_eq!(summary["cleanup"]["reclaimable_bytes"], 4_800_000);
    for item in summary["largest_children"].as_array().unwrap() {
        assert!(
            [
                "danger",
                "in_use",
                "uninstall_only",
                "caution",
                "ask_user",
                "safe_to_clean"
            ]
            .contains(&item["safety"].as_str().unwrap())
        );
    }
    let cleanup = analysis::cleanup(&source, Some(&root.join("proj")), 30, 0).unwrap();
    let mut paths: Vec<_> = cleanup["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["path"].as_str().unwrap().to_owned())
        .collect();
    let mut expected: Vec<_> = ["proj/cache", "proj/__pycache__", "proj/Build.log"]
        .map(|p| fixture_path(&root, p).to_str().unwrap().to_owned())
        .into();
    paths.sort();
    expected.sort();
    assert_eq!(paths, expected);
    assert!(
        cleanup["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["safety"] == "safe_to_clean")
    );
    assert_eq!(
        analysis::list_dir(&source, None, 50).unwrap()["items"][0]["path"],
        json!(root.join("proj"))
    );
    let explain = analysis::explain(&source, &fixture_path(&root, "docs/Report.pdf")).unwrap();
    assert_eq!(explain["type"], "file");
    assert_eq!(explain["bytes"], 400_000);
    assert!(explain.get("advice").is_some());
    let largest = analysis::largest_files(&source, None, 2, Some("cache")).unwrap();
    assert_eq!(largest["files"].as_array().unwrap().len(), 2);
    assert!(
        largest["files"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["category"] == "cache")
    );
    assert!(analysis::largest_files(&source, None, 2, Some("nonsense")).is_err());
    assert!(analysis::explain(&source, &root.join("missing")).is_err());
    let markdown = analysis::report_markdown(&source, None, 5).unwrap();
    for text in ["# DiskMap 磁盘分析", "可清理候选", "blob.bin"] {
        assert!(markdown.contains(text));
    }
}

#[cfg(unix)]
#[test]
fn running_paths_keep_trailing_spaces() {
    let tmp = TempDir::new();
    let root = tmp.0.join("data");
    write(&root.join("x"), 100);
    write(&root.join("x "), 1);
    let mut source = source(&root, false);
    let processes = ProcInfo::with_os(Os::current());
    let mut slot = ProcessMap::default();
    slot.insert(1, Usage::new("reader", Kind::Open));
    processes.install([(root.join("x"), slot.clone()), (root.join("x "), slot)]);
    source.state.processes = processes.snapshot();
    let _scope = i18n::scope(Lang::En);
    // 内部原始路径不能按用户输入裁掉尾部空格，否则重复扣除 x 会导致类别大小下溢。
    let summary = analysis::summary(&source, None, 5).unwrap();
    assert_eq!(summary["bytes"], 101);
    assert_eq!(summary["in_use"]["files"], 2);
    assert_eq!(summary["in_use"]["bytes"], 101);
    assert_eq!(summary["by_category"][0]["category"], "running");
    assert_eq!(summary["by_category"][0]["bytes"], 101);
}

#[cfg(unix)]
#[test]
#[cfg_attr(
    target_os = "macos",
    ignore = "APFS 要求文件名是合法 UTF-8，无法创建本测试的非法字节名称"
)]
fn running_non_unicode_paths_preserve_identity() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let tmp = TempDir::new();
    let root = tmp.0.join("data");
    let first = root.join(OsString::from_vec(b"name\x80".to_vec()));
    let second = root.join(OsString::from_vec(b"name\x81".to_vec()));
    write(&first, 7);
    write(&second, 11);
    let mut source = source(&root, false);
    let processes = ProcInfo::with_os(Os::current());
    let mut slot = ProcessMap::default();
    slot.insert(1, Usage::new("reader", Kind::Open));
    processes.install([(first, slot.clone()), (second, slot)]);
    source.state.processes = processes.snapshot();
    let _scope = i18n::scope(Lang::En);
    // 两个名字有损显示相同，索引与进程查询仍须保留不同的原始字节身份。
    let summary = analysis::summary(&source, None, 5).unwrap();
    assert_eq!(summary["bytes"], 18);
    assert_eq!(summary["in_use"]["files"], 2);
    assert_eq!(summary["in_use"]["bytes"], 18);
    assert_eq!(summary["by_category"][0]["category"], "running");
    assert_eq!(summary["by_category"][0]["bytes"], 18);
    assert_eq!(
        analysis::largest_files(&source, None, 5, Some("running")).unwrap()["files"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
#[ignore = "报告性能测量：创建最多两万个临时文件，不纳入常规测试"]
fn all_files_in_use_report_benchmark() {
    let _scope = i18n::scope(Lang::En);
    for count in [1_000, 5_000, 20_000] {
        let tmp = TempDir::new();
        let root = tmp.0.join("data");
        let mut raw = Vec::with_capacity(count);
        for i in 0..count {
            let path = root.join(format!("file{i:05}.dat"));
            write(&path, i % 1024 + 1);
            let mut slot = ProcessMap::default();
            slot.insert(1, Usage::new("benchmark-reader", Kind::Open));
            raw.push((path, slot));
        }
        let mut source = source(&root, false);
        let processes = ProcInfo::with_os(Os::current());
        processes.install(raw);
        source.state.processes = processes.snapshot();
        source.state.retained_processes = Arc::clone(&source.state.processes);
        let mut samples = Vec::with_capacity(5);
        for _ in 0..5 {
            // 只测分析函数本身；造树、扫描、快照和进程索引安装均在计时之前。
            let started = Instant::now();
            let report = analysis::report_json(&source, None, 15).unwrap();
            samples.push(started.elapsed());
            assert_eq!(report["summary"]["in_use"]["files"], count);
        }
        samples.sort();
        println!(
            "T06 report_json：单目录 {count} 文件，全部在用，5 次中位 {:.3} ms",
            samples[2].as_secs_f64() * 1000.0
        );
    }
}

#[test]
fn refresh_keeps_original_group_membership() {
    let tmp = TempDir::new();
    make_tree(&tmp.0);
    let root = tmp.0.join("dense");
    let mut source = source(&root, false);
    let processes = ProcInfo::with_os(Os::current());
    let mut slot = ProcessMap::default();
    slot.insert(1, Usage::new("late-reader", Kind::Open));
    processes.install([(root.join("f000.data"), slot)]);
    source.state.processes = processes.snapshot();
    let _scope = i18n::scope(Lang::En);
    // 刷新不改变扫描时已合并的小文件；目录的在用计数仍来自新快照。
    assert_eq!(source.state.retained_processes.file_count(), 0);
    let summary = analysis::summary(&source, None, 5).unwrap();
    assert_eq!(summary["in_use"]["files"], 0);
    assert_eq!(summary["in_use"]["bytes"], 0);
    assert_eq!(
        analysis::largest_files(&source, None, 5, Some("running")).unwrap()["files"],
        json!([])
    );
    assert_eq!(
        analysis::explain(&source, &root).unwrap()["in_use_files"],
        1
    );
}

fn difference(actual: &Value, expected: &Value, path: &str) -> Option<String> {
    match (actual, expected) {
        (Value::Object(a), Value::Object(b)) if a.keys().eq(b.keys()) => a
            .iter()
            .find_map(|(key, value)| difference(value, &b[key], &format!("{path}.{key}"))),
        (Value::Array(a), Value::Array(b)) if a.len() == b.len() => a
            .iter()
            .zip(b)
            .enumerate()
            .find_map(|(i, (a, b))| difference(a, b, &format!("{path}[{i}]"))),
        _ if actual == expected => None,
        _ => Some(format!("{path}\nRust: {actual}\nPython: {expected}")),
    }
}

fn is_summary(value: &Value) -> bool {
    [
        "by_category",
        "largest_children",
        "cleanup",
        "in_use",
        "path",
        "files",
        "dirs",
    ]
    .iter()
    .all(|key| value.get(key).is_some())
}

fn assert_proc_path_spelling(value: &Value, index: &diskmap_core::index::Index) {
    if is_summary(value) {
        for entry in value["in_use"]["largest"].as_array().unwrap() {
            let path = Path::new(entry["path"].as_str().unwrap());
            let Some(Found::File { parent, file }) = index.find(path) else {
                panic!("在用路径必须在扫描索引中：{path:?}");
            };
            // 即使新旧双方都改成小写，也必须在这里失败；单凭相同路径键不够。
            assert_eq!(
                path.as_os_str(),
                index.file_path(parent, file).as_os_str(),
                "Rust 在用路径必须保留扫描索引的真实拼写"
            );
        }
    }
    match value {
        Value::Object(items) => items
            .values()
            .for_each(|v| assert_proc_path_spelling(v, index)),
        Value::Array(items) => items
            .iter()
            .for_each(|v| assert_proc_path_spelling(v, index)),
        _ => {}
    }
}

fn adapt_proc_paths(actual: &Value, expected: &mut Value, windows: bool) -> usize {
    if !windows {
        return 0;
    }
    let mut differences = 0;
    if is_summary(actual)
        && is_summary(expected)
        && let (Some(a), Some(b)) = (
            actual["in_use"]["largest"].as_array(),
            expected["in_use"]["largest"].as_array_mut(),
        )
        && a.len() == b.len()
    {
        for (a, b) in a.iter().zip(b) {
            if let (Some(new), Some(old)) = (a["path"].as_str(), b["path"].as_str())
                && new != old
                && diskmap_core::platform::path::path_key(Os::Windows, new.as_bytes())
                    == diskmap_core::platform::path::path_key(Os::Windows, old.as_bytes())
            {
                b["path"] = a["path"].clone();
                differences += 1;
            }
        }
    }
    match (actual, expected) {
        (Value::Object(a), Value::Object(b)) => {
            for (key, new) in a {
                if let Some(old) = b.get_mut(key) {
                    differences += adapt_proc_paths(new, old, true);
                }
            }
        }
        (Value::Array(a), Value::Array(b)) if a.len() == b.len() => {
            for (new, old) in a.iter().zip(b) {
                differences += adapt_proc_paths(new, old, true);
            }
        }
        _ => {}
    }
    differences
}

#[test]
fn windows_proc_path_exception_is_field_limited() {
    let summary = |path: &str| {
        json!({"path": "C:\\Data", "files": 1, "dirs": 0,
        "by_category": [], "largest_children": [], "cleanup": {},
        "in_use": {"largest": [{"path": path, "size": "1 B", "used_by": "reader"}]}})
    };
    let actual = summary("C:\\Data\\Report.PDF");
    let mut expected = summary("c:/data/report.pdf");
    assert_eq!(adapt_proc_paths(&actual, &mut expected, false), 0);
    assert!(difference(&actual, &expected, "$").is_some());
    assert_eq!(adapt_proc_paths(&actual, &mut expected, true), 1);
    assert!(difference(&actual, &expected, "$").is_none());
    expected["path"] = json!("c:/data");
    assert_eq!(adapt_proc_paths(&actual, &mut expected, true), 0);
    assert!(difference(&actual, &expected, "$").is_some());
    expected = summary("C:\\Data\\Different.PDF");
    assert_eq!(adapt_proc_paths(&actual, &mut expected, true), 0);
    assert!(difference(&actual, &expected, "$").is_some());
}

#[test]
fn python_input_timeout_and_panic_reap_children() {
    let Some(mut command) = python_command() else {
        return;
    };
    command
        .args(["-B", "-c", "import time; time.sleep(60)"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = child::ChildGuard::spawn(&mut command).unwrap();
    let timed_out_pid = child.id();
    let started = Instant::now();
    let error = child
        .wait_with_input(vec![b'x'; 2_000_000], Duration::from_millis(150))
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert!(started.elapsed() < Duration::from_secs(5));
    let child = child::ChildGuard::spawn(&mut command).unwrap();
    let panicked_pid = child.id();
    let result = std::panic::catch_unwind(move || {
        let _owned = child;
        panic!("故意验证子进程 RAII 回收");
    });
    assert!(result.is_err());
    #[cfg(target_os = "linux")]
    for pid in [timed_out_pid, panicked_pid] {
        assert!(
            !PathBuf::from(format!("/proc/{pid}")).exists(),
            "子进程仍未回收：{pid}"
        );
    }
    #[cfg(not(target_os = "linux"))]
    let _ = (timed_out_pid, panicked_pid);
}

#[test]
fn matches_legacy_in_both_languages() {
    let tmp = TempDir::new();
    make_tree(&tmp.0);
    let actual = collect(&tmp.0);
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/analysis_compare.py");
    let Some(mut command) = python_command() else {
        return;
    };
    let output = command_output(
        command
            .arg("-B")
            .arg(&script)
            .arg("--reference")
            .arg(&tmp.0)
            .env("DISKMAP_CACHE_DIR", tmp.0.join("isolated-cache"))
            .env("DISKMAP_CONFIG_DIR", tmp.0.join("isolated-config"))
            .env("PYTHONDONTWRITEBYTECODE", "1"),
        Duration::from_secs(60),
    )
    .unwrap_or_else(|error| panic!("无法运行已探测可用的 Python 对照程序：{error}"));
    assert!(
        output.status.success(),
        "Python 对照失败：{}",
        utf8_diagnostic(&output.stderr)
    );
    let stdout = std::str::from_utf8(&output.stdout).expect("Python 对照必须输出 UTF-8");
    let mut expected: Value = serde_json::from_str(stdout).expect("Python 应返回 JSON");
    let known_paths = adapt_proc_paths(&actual, &mut expected, cfg!(windows));
    if let Some(diff) = difference(&actual, &expected, "$") {
        panic!("分析结果与旧版不一致：{diff}");
    }
    println!(
        "T06：中英各 7 个数据场景；Windows procinfo路径已知差异 {known_paths} 个字段（仅in_use.largest[*].path），其余字段全部一致。"
    );
}

/// Windows 的应用执行别名可能返回 9009；可启动不等于可运行 Python。
fn python_command() -> Option<Command> {
    let mut failures = Vec::new();
    for (program, prefix) in [
        ("python3", &[][..]),
        ("python", &[][..]),
        ("py", &["-3"][..]),
    ] {
        let mut command = Command::new(program);
        command
            .args(prefix)
            .env("PYTHONUTF8", "1")
            .env("PYTHONIOENCODING", "utf-8");
        let probe = command_output(command.args(["-B", "-c", "import sys; print('diskmap-python3:' + sys.version.split()[0] if sys.version_info.major == 3 else 'unsupported')"])
            .stdin(Stdio::null()), Duration::from_secs(10));
        match probe {
            Ok(output)
                if output.status.success() && output.stdout.starts_with(b"diskmap-python3:") =>
            {
                eprintln!(
                    "analysis 旧版对照：{}（stdin/stdout UTF-8）",
                    std::str::from_utf8(&output.stdout)
                        .expect("Python 探测应输出 UTF-8")
                        .trim()
                );
            }
            Ok(output) => {
                failures.push(format!(
                    "{program} {}: status={}, stderr={}",
                    prefix.join(" "),
                    output.status,
                    utf8_diagnostic(&output.stderr).trim()
                ));
                continue;
            }
            Err(error) => {
                failures.push(format!("{program} {}: {error}", prefix.join(" ")));
                continue;
            }
        }
        let mut command = Command::new(program);
        command
            .args(prefix)
            .env("PYTHONUTF8", "1")
            .env("PYTHONIOENCODING", "utf-8");
        return Some(command);
    }
    eprintln!(
        "没有可用 Python 3，跳过 analysis 旧版对照；Rust 语义测试仍执行：{}",
        failures.join("; ")
    );
    None
}

fn utf8_diagnostic(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap_or_else(|error| {
        format!(
            "非 UTF-8 诊断：{error}；原始字节：{:02x?}",
            error.as_bytes()
        )
    })
}
