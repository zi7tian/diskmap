use super::*;
use crate::i18n::{self, Lang};
use crate::procinfo::{Kind, ProcessMap, Usage};
use crate::scan::MountEntry;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "diskmap-t05-app-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        fs::create_dir(path.join("nested")).unwrap();
        fs::write(path.join("nested/file.txt"), [3; 87]).unwrap();
        Self(path)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn app() -> App {
    let pkgdb = Arc::new(PkgDb::default());
    App::with_components(
        pkgdb.clone(),
        Arc::new(Classifier::new(Some(pkgdb), &[])),
        ProcInfo::new(),
    )
}

fn progress() -> Progress {
    Progress {
        state: ScanState::Scanning,
        error: None,
        root: PathBuf::from("fixture"),
        files: 17,
        dirs: 3,
        bytes: 12_345,
        denied: 10,
        denied_samples: (0..10)
            .map(|i| PathBuf::from(format!("denied-{i}")))
            .collect(),
        current: Some(PathBuf::from("current")),
        elapsed: Duration::from_millis(1234),
        finished_at: Some(UNIX_EPOCH),
        apparent: true,
        cross_fs: false,
        fs_used: 987_654,
        mounts: vec![MountEntry {
            path: PathBuf::from("included"),
            fstype: "ntfs".into(),
            source: Some("volume".into()),
        }],
        skipped: (0..32)
            .map(|i| MountEntry {
                path: PathBuf::from(format!("skipped-{i}")),
                fstype: "tmpfs".into(),
                source: None,
            })
            .collect(),
        threads: 4,
        method: ScanMethod::Generic,
        fallback_reason: None,
        records: 45_678,
        anomalies: 2,
    }
}

#[test]
fn idle_status_has_only_legacy_fields() {
    let status = app().status();
    assert_eq!(status["state"], "idle");
    assert_eq!(
        status
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        [
            "admin",
            "agent_seen_ago",
            "command",
            "os",
            "pkg",
            "procs",
            "ready",
            "state"
        ]
    );
}

#[test]
fn fixed_components_do_not_enable_saved_scans() {
    let fixture = Fixture::new();
    let cache = fixture.0.join("unused-cache");
    let app = app();
    #[cfg(feature = "server")]
    app.use_instance_cache_dir(&cache);
    assert!(app.saved_store().is_none());
    app.start_scan(fixture.0.to_str().unwrap(), true, false)
        .unwrap();
    assert_eq!(app.current_scan().unwrap().wait(), ScanState::Done);
    assert!(app.saved_scans().unwrap().is_empty());
    assert!(app.status().get("saved").is_none());
    assert!(!cache.exists());
}

#[test]
fn scan_status_keeps_legacy_fields_outside_active_mft() {
    let _lang = i18n::scope(Lang::En);
    for (state, method) in [
        (ScanState::Scanning, ScanMethod::Generic),
        (ScanState::Scanning, ScanMethod::Auto),
        (ScanState::Done, ScanMethod::Generic),
        (ScanState::Done, ScanMethod::Auto),
        (ScanState::Done, ScanMethod::Mft),
        (ScanState::Cancelled, ScanMethod::Generic),
        (ScanState::Cancelled, ScanMethod::Auto),
        (ScanState::Cancelled, ScanMethod::Mft),
        (ScanState::Error, ScanMethod::Generic),
        (ScanState::Error, ScanMethod::Auto),
        (ScanState::Error, ScanMethod::Mft),
    ] {
        let mut p = progress();
        p.state = state;
        p.method = method;
        p.error = (state == ScanState::Error).then(|| Text::new("测试错误", "fixture error"));
        let expected = json!({
            "state": state.key(), "error": (state == ScanState::Error).then_some("fixture error"),
            "root": "fixture", "files": 17, "dirs": 3, "bytes": 12_345, "denied": 10,
            "denied_samples": (0..8).map(|i| format!("denied-{i}")).collect::<Vec<_>>(),
            "current": "current", "elapsed": 1.23, "apparent": true, "cross_fs": false,
            "fs_used": 987_654,
            "mounts": [{"path": "included", "fstype": "ntfs", "source": "volume"}],
            "skipped": (0..30).map(|i| json!({"path": format!("skipped-{i}"), "fstype": "tmpfs"})).collect::<Vec<_>>(),
        });
        for records in [0, 45_678, u64::MAX] {
            p.records = records;
            // 通用扫描中非零记录模拟 MFT 读取失败后的残留进度。
            p.fallback_reason = (method == ScanMethod::Generic && records > 0)
                .then(|| Text::new("读取 MFT 失败", "MFT read failed"));
            assert_eq!(
                scan_status(&p),
                expected,
                "{state:?} / {method:?} / {records}"
            );
        }
    }
}

#[test]
fn scanning_mft_status_adds_only_integer_record_count() {
    let mut p = progress();
    for records in [0, 1, 12_345, u64::MAX] {
        p.method = ScanMethod::Mft;
        p.records = records;
        let mut fields = scan_status(&p);
        let count = fields
            .as_object_mut()
            .unwrap()
            .remove("mft_records")
            .unwrap();
        assert_eq!(count.as_u64(), Some(records));
        p.method = ScanMethod::Generic;
        assert_eq!(fields, scan_status(&p));
    }
}

#[test]
fn scan_info_tree_and_refresh_overlay() {
    let fixture = Fixture::new();
    let app = app();
    assert_eq!(app.status()["state"], "idle");
    let root = fixture.0.to_str().unwrap();
    app.start_scan(root, true, false).unwrap();
    app.current_scan().unwrap().wait();
    let status = app.status();
    assert_eq!(status["files"], 1);
    assert_eq!(status["dirs"], 1);
    assert_eq!(status["bytes"], 87);
    assert_eq!(status["state"], "done");
    let path = fixture.0.join("nested/file.txt");
    let mut procs = ProcessMap::default();
    procs.insert(42, Usage::new("fixture", Kind::Open));
    app.procs().install([(path.clone(), procs)]);
    let tree = app.tree(root, 0.000025).unwrap();
    assert_eq!(tree["totals"][1], 87);
    let info = app.info(path.to_str().unwrap()).unwrap();
    assert_eq!(info["nprocs"], 1);
    assert_eq!(info["apparent"], 87);
    assert_eq!(info["cls"], 1);
    assert!(info["mtime"].as_str().unwrap().len() >= 16);
    assert_eq!(info["mode"].as_str().unwrap().len(), 10);
    fs::remove_file(&path).unwrap();
    assert_eq!(app.info(path.to_str().unwrap()).unwrap()["gone"], true);
    assert!(app.tree(path.to_str().unwrap(), 0.01).is_none());
}

#[test]
fn invalid_scan_preserves_current_and_errors_are_localized() {
    let fixture = Fixture::new();
    let app = app();
    app.start_scan(fixture.0.to_str().unwrap(), true, true)
        .unwrap();
    let old = app.current_scan().unwrap();
    let missing = fixture.0.join("missing");
    let error = app
        .start_scan(missing.to_str().unwrap(), false, false)
        .unwrap_err();
    assert_eq!(app.current_scan().unwrap().root(), old.root());
    assert!(error.get(Lang::En).starts_with("Folder does not exist:"));
    assert!(error.get(Lang::Zh).starts_with("文件夹不存在："));
    old.wait();
}

#[test]
fn new_scan_owns_fresh_running_cache() {
    let a = Fixture::new();
    let b = Fixture::new();
    let app = app();
    let mut procs = ProcessMap::default();
    procs.insert(7, Usage::new("fixture", Kind::Open));
    app.procs().install([(a.0.join("nested/file.txt"), procs)]);
    for (fixture, used) in [(&a, 87), (&b, 0)] {
        app.start_scan(fixture.0.to_str().unwrap(), true, false)
            .unwrap();
        app.current_scan().unwrap().wait();
        assert_eq!(
            app.tree(fixture.0.to_str().unwrap(), 0.000025).unwrap()["totals"][1],
            used
        );
    }
}

#[test]
fn serialized_tree_hits_and_invalidates_for_processes_language_settings_and_rescan() {
    let fixture = Fixture::new();
    let app = app();
    let root = fixture.0.to_str().unwrap();
    app.start_scan(root, true, false).unwrap();
    app.current_scan().unwrap().wait();
    let _lang = i18n::scope(Lang::En);
    let first = app.tree_bytes(root, 0.000025).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&first).unwrap(),
        app.tree(root, 0.000025).unwrap()
    );
    assert!(Arc::ptr_eq(
        &first,
        &app.tree_bytes(root, 0.000025).unwrap()
    ));
    let mut used = ProcessMap::default();
    used.insert(
        0,
        Usage {
            comm: Text::new("系统", "system"),
            kind: Kind::Swap,
        },
    );
    app.procs()
        .install([(fixture.0.join("nested/file.txt"), used)]);
    let running = app.tree_bytes(root, 0.000025).unwrap();
    assert!(!Arc::ptr_eq(&first, &running));
    assert_eq!(
        serde_json::from_slice::<Value>(&running).unwrap()["totals"][1],
        87
    );
    {
        let _lang = i18n::scope(Lang::Zh);
        let chinese = app.tree_bytes(root, 0.000025).unwrap();
        assert!(!Arc::ptr_eq(&running, &chinese));
        assert_ne!(running, chinese);
    }
    assert!(Arc::ptr_eq(
        &running,
        &app.tree_bytes(root, 0.000025).unwrap()
    ));
    app.invalidate_tree_responses();
    let settings = app.tree_bytes(root, 0.000025).unwrap();
    assert!(!Arc::ptr_eq(&running, &settings));
    assert_eq!(running, settings);
    fs::write(fixture.0.join("extra.txt"), [0; 120]).unwrap();
    app.start_scan(root, true, false).unwrap();
    app.current_scan().unwrap().wait();
    let rescanned = app.tree_bytes(root, 0.000025).unwrap();
    assert!(!Arc::ptr_eq(&settings, &rescanned));
    assert_eq!(
        serde_json::from_slice::<Value>(&rescanned).unwrap()["tree"]["nf"],
        2
    );
    app.refresh_procs();
    assert!(!Arc::ptr_eq(
        &rescanned,
        &app.tree_bytes(root, 0.000025).unwrap()
    ));
}

#[test]
fn agent_contact_does_not_keep_browser_alive() {
    let app = app();
    app.touch_agent();
    assert!(app.ui_closed(Duration::from_secs(121)));
    app.touch_ui();
    assert!(!app.ui_closed(Duration::from_secs(121)));
    app.say_bye();
    assert!(!app.ui_closed(Duration::from_secs(121)));
    app.set_ui_state(json!({"view": "/a", "selected": "/a/b", "pinned": "cache"}));
    assert_eq!(app.ui_state().unwrap()["pinned"], "cache");
    let _lang = i18n::scope(Lang::En);
    assert!(app.reveal("/outside").is_err());
}

#[test]
fn cancel_allows_rescan_but_shutdown_prevents_late_requests() {
    let fixture = Fixture::new();
    let app = app();
    let root = fixture.0.to_str().unwrap();
    app.start_scan(root, true, false).unwrap();
    app.cancel();
    app.start_scan(root, true, false).unwrap();
    app.current_scan().unwrap().wait();
    app.shutdown();
    assert!(app.start_scan(root, true, false).is_err());
}

#[test]
fn analysis_adapter_preserves_category_keys_and_atomic_command_ids() {
    use crate::analysis::Source;
    let app = app();
    app.set_ui_state(json!({"view": null, "selected": null, "pinned": "cache"}));
    app.touch_ui();
    assert_eq!(
        crate::analysis::user_view(&app).unwrap()["highlighted_category"],
        "cache"
    );
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let app = &app;
            scope.spawn(move || {
                for _ in 0..100 {
                    app.request_show(Path::new("/fixture"), "note", 123.0);
                }
            });
        }
    });
    assert_eq!(app.status()["command"]["id"], 800);
    assert_eq!(app.status()["command"]["action"], "show");
}

#[test]
fn agent_scan_reuses_compatible_scope_and_modes() {
    let fixture = Fixture::new();
    let root = fixture.0.to_str().unwrap();
    let nested = fixture.0.join("nested");
    for (old_cross, new_cross, reused) in [
        (true, true, true),
        (true, false, true),
        (false, false, true),
        (false, true, false),
    ] {
        let app = app();
        app.start_scan(root, true, old_cross).unwrap();
        app.current_scan().unwrap().wait();
        let result = app
            .agent_scan(nested.to_str().unwrap(), false, true, new_cross)
            .unwrap();
        assert_eq!(result["reused"], reused);
        app.current_scan().unwrap().wait();
        assert_eq!(
            app.current_scan().unwrap().root(),
            if reused { &fixture.0 } else { &nested }
        );
    }
    let app = app();
    app.start_scan(root, true, false).unwrap();
    app.current_scan().unwrap().wait();
    assert_eq!(
        app.agent_scan(root, false, false, false).unwrap()["reused"],
        false
    );
    app.current_scan().unwrap().wait();
    assert!(!app.current_scan().unwrap().apparent());
    app.shutdown();
    assert!(app.agent_scan(root, false, false, false).is_err());
}

#[test]
fn agent_rescan_observes_new_files_and_does_not_refresh_ui_contact() {
    use crate::analysis::Source;
    let fixture = Fixture::new();
    let root = fixture.0.to_str().unwrap();
    let app = app();
    app.start_scan(root, true, true).unwrap();
    app.current_scan().unwrap().wait();
    fs::write(fixture.0.join("later.txt"), "new file").unwrap();
    assert_eq!(app.agent_scan(root, false, true, true).unwrap()["files"], 1);
    assert_eq!(
        app.agent_scan(root, true, true, true).unwrap()["reused"],
        false
    );
    app.current_scan().unwrap().wait();
    assert_eq!(app.status()["files"], 2);
    assert_eq!(app.snapshot().last_seen, 0.0);
    assert!(app.ui_closed(Duration::from_secs(121)));
}

#[test]
fn agent_scan_uses_original_file_projection_after_process_refresh() {
    let fixture = Fixture::new();
    let many = fixture.0.join("many");
    fs::create_dir(&many).unwrap();
    let small = many.join("small");
    fs::write(&small, "x").unwrap();
    for n in 0..150 {
        fs::write(many.join(format!("large-{n:03}")), vec![0; n + 2]).unwrap();
    }
    let app = app();
    let root = fixture.0.to_str().unwrap();
    app.start_scan(root, true, false).unwrap();
    app.current_scan().unwrap().wait();
    // 小文件虽可通过完整索引查详情，但不能用它复用旧版不存在的文件结果。
    assert!(app.info(small.to_str().unwrap()).is_some());
    assert!(
        app.agent_scan(small.to_str().unwrap(), false, true, false)
            .is_err()
    );
    let large = many.join("large-149");
    assert_eq!(
        app.agent_scan(large.to_str().unwrap(), false, true, false)
            .unwrap()["reused"],
        true
    );
    let mut processes = ProcessMap::default();
    processes.insert(123, Usage::new("fixture", Kind::Open));
    app.procs().install([(small.clone(), processes)]);
    app.start_scan(root, true, false).unwrap();
    app.current_scan().unwrap().wait();
    app.procs().install([]);
    assert_eq!(
        app.agent_scan(small.to_str().unwrap(), false, true, false)
            .unwrap()["reused"],
        true
    );
    // 已扫描路径即使后来消失，也应允许复用已有结果。
    fs::remove_file(&small).unwrap();
    assert_eq!(
        app.agent_scan(small.to_str().unwrap(), false, true, false)
            .unwrap()["reused"],
        true
    );
}

#[test]
fn agent_scope_checks_a_path_component_boundary() {
    assert!(path_is_covered(
        Os::Linux,
        Path::new("/"),
        Path::new("/dir")
    ));
    assert!(path_is_covered(
        Os::Linux,
        Path::new("/dir"),
        Path::new("/dir/child")
    ));
    assert!(!path_is_covered(
        Os::Linux,
        Path::new("/dir"),
        Path::new("/directory")
    ));
    assert!(!path_is_covered(
        Os::Linux,
        Path::new("/dir"),
        Path::new("/Dir")
    ));
    assert!(path_is_covered(
        Os::Windows,
        Path::new(r"C:\Dir"),
        Path::new("c:/dir/child")
    ));
    assert!(!path_is_covered(
        Os::Windows,
        Path::new(r"C:\Dir"),
        Path::new(r"c:\directory")
    ));
    for (root, path, covered) in [
        (r"C:\ΟΣ", "c:/οσ/child", true),
        (r"C:\ß", "c:/ẞ/child", false),
        (r"C:\ı", "c:/I/child", true),
        (r"C:\İ", "c:/i/child", false),
        (r"C:\ﬀ", "c:/FF/child", false),
        (r"C:\Ꭰ", "c:/ꭰ/child", true),
        (r"C:\𐐀", "c:/𐐨/child", false),
        (r"C:\Σ", "c:/σibling", false),
    ] {
        assert_eq!(
            path_is_covered(Os::Windows, Path::new(root), Path::new(path)),
            covered,
            "{root}/{path}"
        );
    }
}
