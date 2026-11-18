//! 纯数据与线程行为测试，不依赖本机进程或修改系统设置。
use super::*;

fn records(rows: &[(u32, &str, Kind)]) -> ProcessMap {
    let mut map = ProcessMap::default();
    for &(pid, name, kind) in rows {
        map.insert(pid, Usage::new(name, kind));
    }
    map
}
fn one(path: &str, rows: &[(u32, &str, Kind)]) -> [(PathBuf, ProcessMap); 1] {
    [(path.into(), records(rows))]
}
fn collector(os: Os) -> Collector {
    Collector {
        data: Arc::default(),
        cancelled: Arc::default(),
        deadline: Instant::now() + Duration::from_secs(10),
        os,
    }
}

#[test]
fn priority_and_same_rank_keep_first() {
    let mut map = records(&[
        (9, "a", Kind::Open),
        (9, "b", Kind::Lib),
        (9, "c", Kind::Exe),
        (9, "d", Kind::Swap),
    ]);
    map.insert(9, Usage::new("e", Kind::Open));
    assert_eq!(map.len(), 1);
    assert_eq!(map.get(9), Some(&Usage::new("c", Kind::Exe)));
}

#[test]
fn counts_unique_files_and_snapshots_stay_consistent() {
    let p = ProcInfo::with_os(Os::Linux);
    p.install([
        (
            PathBuf::from("/a/b/x"),
            records(&[(1, "a", Kind::Exe), (2, "b", Kind::Lib)]),
        ),
        (PathBuf::from("/a/y"), records(&[(1, "a", Kind::Open)])),
    ]);
    let old = p.snapshot();
    assert_eq!(
        (
            old.file_count(),
            old.dir_count("/"),
            old.dir_count("/a/"),
            old.dir_count("/a/b")
        ),
        (2, 2, 2, 1)
    );
    assert_eq!(old.dir_count("/a/b/x"), 0);
    assert_eq!(old.dir_count("/ab"), 0);
    p.install(one("/other", &[(3, "new", Kind::Exe)]));
    assert!(old.lookup("/a/y").is_some());
    assert!(p.snapshot().lookup("/a/y").is_none());
    assert_eq!(p.snapshot().stamp, old.stamp + 1);
}

#[test]
fn summaries_preserve_ties_and_translate() {
    let p = ProcInfo::with_os(Os::Linux);
    p.install(one(
        "/x",
        &[
            (99, "z", Kind::Open),
            (8, "a", Kind::Exe),
            (9, "z", Kind::Exe),
            (4, "b", Kind::Lib),
            (5, "c", Kind::Open),
            (6, "d", Kind::Open),
        ],
    ));
    let s = p.snapshot();
    assert_eq!(
        s.summary("/x"),
        Some(Text::new(
            "z ×2、a、b、c 等 5 个程序",
            "z ×2, a, b, c and others (5 programs)"
        ))
    );
    assert_eq!(
        s.summary_with_limit("/x", 1),
        Some(Text::new(
            "z ×2 等 5 个程序",
            "z ×2 and others (5 programs)"
        ))
    );
    assert!(s.summary("/missing").is_none());
    let (rows, total) = s.details_with_limit("/x", 3);
    assert_eq!(total, 6);
    assert_eq!(rows.iter().map(|r| r.pid).collect::<Vec<_>>(), [8, 9, 4]);
    for (lang, label) in [(Lang::Zh, "正在执行"), (Lang::En, "executing")] {
        let _scope = crate::i18n::scope(lang);
        let json = serde_json::to_value(&rows).unwrap();
        assert_eq!(
            json[0],
            serde_json::json!({"pid":8,"comm":"a","kind":label})
        );
    }
}

#[test]
fn default_detail_limit_and_totals() {
    let p = ProcInfo::with_os(Os::Linux);
    let mut slot = ProcessMap::default();
    for pid in 1..=45 {
        slot.insert(pid, Usage::new("a", Kind::Open));
    }
    p.install([(PathBuf::from("/x"), slot)]);
    assert_eq!(
        (
            p.snapshot().details("/x").0.len(),
            p.snapshot().details("/x").1
        ),
        (40, 45)
    );
    assert_eq!(p.snapshot().details("/none").1, 0);
}

#[test]
fn windows_path_merge_root_and_unicode() {
    let p = ProcInfo::with_os(Os::Windows);
    p.install([
        (
            PathBuf::from("C:/USERS/ΟΣ/A.DLL"),
            records(&[(1, "app", Kind::Exe)]),
        ),
        (
            PathBuf::from("c:\\users\\ος\\a.dll"),
            records(&[(1, "app", Kind::Open), (2, "two", Kind::Lib)]),
        ),
    ]);
    let s = p.snapshot();
    assert_eq!(s.file_count(), 1);
    assert_eq!(s.lookup("C:/Users/ΟΣ/a.dll").unwrap().len(), 2);
    assert_eq!(
        s.lookup("c:\\users\\ος\\A.DLL")
            .unwrap()
            .get(1)
            .unwrap()
            .kind,
        Kind::Exe
    );
    assert_eq!(s.dir_count("C:/"), 1);
    assert_eq!(s.dir_count("c:\\users\\ΟΣ\\"), 1);
    assert_eq!(s.dir_count("C:"), 0);
}

#[test]
fn filtering_deleted_and_system_text() {
    let out = collector(Os::Linux);
    for path in [
        "relative",
        "/proc/1/exe",
        "/sys/x",
        "/dev/shm/x",
        "/memfd:x",
        "/run/x",
        "/SYSV000",
        "socket:[1]",
    ] {
        out.add(path, 1, "app", Kind::Open);
    }
    out.add("/tmp/a (deleted)", 1, "app", Kind::Open);
    out.add("/tmp/a", 1, "app", Kind::Exe);
    out.add("/swapfile", 0, "", Kind::Swap);
    let data = out.data.lock().unwrap();
    assert_eq!(data.files.len(), 2);
    assert_eq!(
        data.files[b"/tmp/a".as_slice()]
            .processes
            .get(1)
            .unwrap()
            .kind,
        Kind::Exe
    );
    assert_eq!(
        data.files[b"/swapfile".as_slice()]
            .processes
            .get(0)
            .unwrap()
            .comm,
        Text::new("系统", "system")
    );
    assert!(path::usable(Os::Windows, Path::new("C:\\a")));
    for p in ["C:\\", "\\\\server\\share\\a", "\\\\?\\C:\\a"] {
        assert!(!path::usable(Os::Windows, Path::new(p)));
    }
}

#[cfg(unix)]
#[test]
fn non_utf8_and_trailing_spaces_survive() {
    use std::os::unix::ffi::OsStrExt;
    let out = collector(Os::Linux);
    let p = Path::new(std::ffi::OsStr::from_bytes(b"/tmp/\xff  (deleted)"));
    out.add(p, 1, "app", Kind::Open);
    let data = out.data.lock().unwrap();
    assert!(data.files.contains_key(b"/tmp/\xff ".as_slice()));
    assert!(!data.files.contains_key(b"/tmp/\xef\xbf\xbd ".as_slice()));
}

#[test]
fn timeout_publishes_partial_and_late_worker_cannot_overwrite() {
    let p = ProcInfo::with_os(Os::Linux);
    let (release, wait) = mpsc::channel();
    let start = Instant::now();
    assert!(p.refresh_using(Duration::from_millis(100), move |out| {
        out.add("/early", 1, "a", Kind::Exe);
        wait.recv().unwrap();
        out.add("/late", 2, "b", Kind::Open);
        Ok(())
    }));
    assert!(start.elapsed() < Duration::from_secs(2));
    assert!(!p.refreshing());
    assert!(p.snapshot().timed_out);
    assert!(p.snapshot().lookup("/early").is_some());
    assert!(!p.refresh_async());
    assert!(!p.refresh());
    p.install(one("/manual", &[(3, "c", Kind::Exe)]));
    let stamp = p.snapshot().stamp;
    release.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while p.inner.collecting.load(Ordering::Acquire) && Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert!(!p.inner.collecting.load(Ordering::Acquire));
    assert_eq!(p.snapshot().stamp, stamp);
    assert!(p.snapshot().lookup("/manual").is_some());
    assert!(p.snapshot().lookup("/late").is_none());
}

#[test]
fn failure_and_panic_keep_old_data_and_release_worker() {
    let p = ProcInfo::with_os(Os::Linux);
    p.install(one("/old", &[(1, "a", Kind::Exe)]));
    for panic in [false, true] {
        assert!(p.refresh_using(Duration::from_secs(1), move |_| {
            if panic {
                panic!("模拟工作线程失败");
            }
            Err(io::Error::other("模拟读取失败"))
        }));
        assert!(p.snapshot().lookup("/old").is_some());
        assert!(p.snapshot().error.is_some());
        assert!(!p.refreshing());
    }
    assert!(p.refresh_using(Duration::from_secs(1), |out| {
        out.add("/new", 2, "b", Kind::Exe);
        Ok(())
    }));
    assert!(p.snapshot().error.is_none());
    assert!(p.snapshot().lookup("/old").is_none());
}

#[test]
fn zero_budget_and_send_sync() {
    fn assert_shared<T: Send + Sync>() {}
    assert_shared::<ProcInfo>();
    assert_shared::<Snapshot>();
    let p = ProcInfo::with_os(Os::Linux);
    assert!(p.refresh_using(Duration::ZERO, |out| {
        out.add("/x", 1, "a", Kind::Exe);
        Ok(())
    }));
    assert!(p.snapshot().timed_out);
    assert_eq!(p.snapshot().file_count(), 0);
}

#[test]
fn system_name_grouping_matches_python_string_keys() {
    let p = ProcInfo::with_os(Os::Linux);
    let mut slot = ProcessMap::default();
    slot.insert(
        0,
        Usage {
            comm: Text::new("系统", "system"),
            kind: Kind::Swap,
        },
    );
    slot.insert(1, Usage::new("系统", Kind::Open));
    p.install([(PathBuf::from("/swap"), slot)]);
    assert_eq!(
        p.snapshot().summary("/swap"),
        Some(Text::new("系统 ×2", "system ×2"))
    );
}

#[test]
fn offline_install_retains_refresh_metadata_and_empty_slots() {
    let p = ProcInfo::with_os(Os::Linux);
    assert!(p.refresh_using(Duration::from_secs(1), |out| {
        out.process_seen();
        out.process_visible();
        Ok(())
    }));
    let before = p.snapshot();
    p.install([(PathBuf::from("/empty"), ProcessMap::default())]);
    let after = p.snapshot();
    assert_eq!(
        (after.nprocs, after.visible, after.time),
        (before.nprocs, before.visible, before.time)
    );
    assert_eq!(after.file_count(), 1);
    assert_eq!(after.dir_count("/"), 1);
    assert!(after.lookup("/empty").unwrap().is_empty());
    assert!(after.summary("/empty").is_none());
}
