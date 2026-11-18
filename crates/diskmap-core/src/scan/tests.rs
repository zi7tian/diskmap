//! 扫描的测试。所有数据都在临时目录里自己生成。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use super::*;
use crate::classify::ClassifierOptions;
use crate::index::{Found, MOUNT};

/// 测试用的临时目录，结束时删除。
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        static N: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "diskmap-scan-{}-{}-{}",
            std::process::id(),
            tag,
            N.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        // 规范化后的路径，与扫描结果里的路径一致
        TempDir(platform::norm_path(path.to_str().unwrap()))
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.0.join(rel)
    }

    fn file(&self, rel: &str, len: usize) -> PathBuf {
        let p = self.path(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, vec![b'x'; len]).unwrap();
        p
    }

    fn dir(&self, rel: &str) -> PathBuf {
        let p = self.path(rel);
        fs::create_dir_all(&p).unwrap();
        p
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn classifier() -> Arc<Classifier> {
    Arc::new(Classifier::with_options(
        None,
        &[],
        ClassifierOptions::for_os(Os::current()),
    ))
}

fn run(root: &Path, apparent: bool, threads: usize) -> Scan {
    let mut opts = ScanOptions::new(root);
    opts.apparent = apparent;
    opts.threads = threads;
    let scan = Scan::start(opts, classifier());
    assert_eq!(scan.wait(), ScanState::Done, "{:?}", scan.progress().error);
    scan
}

fn names<'a>(it: impl Iterator<Item = &'a std::ffi::OsStr>) -> Vec<String> {
    it.map(|n| n.to_string_lossy().into_owned()).collect()
}

/// 进度计数与目录树根节点的统计一致。
fn assert_counters_match(scan: &Scan) {
    let p = scan.progress();
    let index = scan.index();
    let root = index.root();
    assert_eq!(
        (p.files, p.dirs, p.bytes),
        (root.nfiles(), root.ndirs(), root.size())
    );
    assert_eq!(index.dir_count() as u64, root.ndirs() + 1);
    // 每个目录的统计等于直接包含的内容之和（没有计入的硬链接不出现）
    for d in index.dirs() {
        let files: u64 = d.files().map(|f| f.size()).sum();
        let subs: u64 = d.subdirs().map(|s| s.size()).sum();
        assert_eq!(d.size(), files + subs, "{:?}", d.path());
        let nfiles = d.files().count() as u64 + d.subdirs().map(|s| s.nfiles()).sum::<u64>();
        assert_eq!(d.nfiles(), nfiles, "{:?}", d.path());
    }
}

#[test]
fn small_tree_totals_and_lookup() {
    let t = TempDir::new("small");
    t.file("a.txt", 10);
    t.file("sub/b.bin", 1000);
    t.file("sub/deeper/c", 0);
    t.dir("empty");
    for threads in [1, 4] {
        let scan = run(&t.0, true, threads);
        let p = scan.progress();
        assert_eq!((p.files, p.dirs, p.bytes), (3, 3, 1010));
        assert_eq!((p.denied, p.current.as_ref()), (0, None));
        assert!(p.finished_at.is_some() && p.apparent && p.cross_fs);
        assert_eq!(p.threads, threads);
        assert_eq!(p.root, t.0);
        assert_counters_match(&scan);

        let index = scan.index();
        let root = index.root();
        assert_eq!(names(root.subdirs().map(|d| d.name())), ["empty", "sub"]);
        assert_eq!(names(root.files().map(|f| f.name())), ["a.txt"]);
        assert!(
            index
                .dirs()
                .all(|d| d.is_listed() && d.flags() & MOUNT == 0)
        );
        match index.find(&t.path("sub/b.bin")) {
            Some(Found::File { file, .. }) => assert_eq!(index.file(file).size(), 1000),
            other => panic!("unexpected {other:?}"),
        }
        let Some(Found::Dir(sub)) = index.find(&t.path("sub")) else {
            panic!("sub not found")
        };
        let sub = index.dir(sub);
        assert_eq!((sub.size(), sub.nfiles(), sub.ndirs()), (1000, 2, 1));
        assert_eq!(sub.path(), t.path("sub"));
    }
}

#[test]
fn classes_come_from_the_classifier_in_order() {
    let t = TempDir::new("classes");
    t.file("project/.cache/x.bin", 3);
    t.file("project/notes.tmp", 3);
    t.file("project/src/main.rs", 3);
    let scan = run(&t.0, true, 2);
    let c = scan.classifier().clone();
    let index = scan.index();
    assert_eq!(index.root().class(), c.root_class(&t.0));
    // 每个目录、每个文件的分类与按 root_class → enter_dir → dir_class / file_class 顺序调用的结果相同
    for dir in index.dirs() {
        let path = dir.path();
        let ctx = c.enter_dir(dir.class(), &path);
        for sub in dir.subdirs() {
            assert_eq!(
                sub.class(),
                c.dir_class(&ctx, sub.name(), &sub.path()),
                "{:?}",
                sub.path()
            );
        }
        for f in dir.files() {
            assert_eq!(f.category(), c.file_class(&ctx, f.name()), "{:?}", f.name());
        }
    }
}

#[test]
fn index_can_be_read_while_scanning() {
    let t = TempDir::new("busy");
    for d in 0..40 {
        for f in 0..25 {
            t.file(&format!("d{d:02}/inner/f{f:02}"), 7);
        }
    }
    let mut opts = ScanOptions::new(&t.0);
    opts.apparent = true;
    opts.threads = 4;
    let scan = Scan::start(opts, classifier());
    let mut last = 0;
    while scan.wait_timeout(Duration::from_millis(1)) == ScanState::Scanning {
        let seen = scan.index().root().nfiles();
        assert!(seen >= last);
        last = seen;
        let _ = scan.progress();
    }
    assert_eq!(scan.state(), ScanState::Done);
    let p = scan.progress();
    assert_eq!((p.files, p.dirs, p.bytes), (1000, 80, 7000));
    assert_counters_match(&scan);
}

#[test]
fn not_a_folder_is_an_error() {
    let t = TempDir::new("notdir");
    let file = t.file("plain", 1);
    for root in [file, t.path("missing")] {
        let scan = Scan::start(ScanOptions::new(&root), classifier());
        assert_eq!(scan.wait(), ScanState::Error);
        let p = scan.progress();
        let err = p.error.expect("error text");
        assert!(err.zh.starts_with("不是文件夹：") && err.en.starts_with("Not a folder: "));
        assert!(!scan.index().root().is_listed());
    }
}

#[test]
fn cancel_before_running_keeps_only_the_root() {
    let t = TempDir::new("cancel");
    t.file("a/b", 1);
    let inner = Inner::new(ScanOptions::new(&t.0), classifier());
    inner.cancel();
    inner.run();
    let status = lock(&inner.status);
    assert_eq!(status.state, ScanState::Cancelled);
    assert_eq!(status.fs_used, 0);
    let index = inner.index.read().unwrap();
    assert_eq!((index.dir_count(), index.file_count()), (1, 0));
}

#[test]
fn cancelled_and_failed_scans_never_invoke_the_save_callback() {
    let t = TempDir::new("no-save");
    for (root, cancel, expected) in [
        (t.0.clone(), true, ScanState::Cancelled),
        (t.path("absent"), false, ScanState::Error),
    ] {
        let inner = Arc::new(Inner::new(ScanOptions::new(root), classifier()));
        if cancel {
            inner.cancel();
        }
        let cache = t.path("cache");
        let store = crate::saved::Store::with_cache_dir(&cache);
        Scan::run_with_done(
            inner.clone(),
            Some(Box::new(move |scan| store.save(&scan).unwrap())),
        );
        assert_eq!(lock(&inner.status).state, expected);
        assert!(!cache.exists());
    }
}

#[test]
fn dropping_the_last_handle_cancels() {
    let t = TempDir::new("drop");
    let scan = Scan::start(ScanOptions::new(&t.0), classifier());
    let inner = Arc::clone(&scan.handle.0);
    let copy = scan.clone();
    drop(scan);
    assert!(!inner.cancelled.load(Relaxed));
    drop(copy);
    assert!(inner.cancelled.load(Relaxed));
}

#[test]
fn options_and_states() {
    let o = ScanOptions::new("/x");
    assert_eq!((o.apparent, o.cross_fs, o.threads), (false, true, 0));
    assert!((1..=8).contains(&default_threads()));
    let keys: Vec<_> = [
        ScanState::Scanning,
        ScanState::Done,
        ScanState::Cancelled,
        ScanState::Error,
    ]
    .iter()
    .map(|s| (s.key(), s.is_finished()))
    .collect();
    assert_eq!(
        keys,
        [
            ("scanning", false),
            ("done", true),
            ("cancelled", true),
            ("error", true)
        ]
    );
}

#[test]
fn handles_can_be_shared_between_threads() {
    // T05 的 HTTP 线程共用同一个扫描句柄
    fn shared<T: Send + Sync>() {}
    shared::<Scan>();
    shared::<Progress>();
    shared::<ScanOptions>();
}

#[cfg(unix)]
mod unix {
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

    use super::*;

    #[test]
    fn allocated_sizes_use_blocks() {
        let t = TempDir::new("blocks");
        let a = t.file("a", 5000);
        let b = t.file("sub/b", 1);
        let expected: u64 = [a, b]
            .iter()
            .map(|p| fs::metadata(p).unwrap().blocks() * 512)
            .sum();
        let scan = run(&t.0, false, 2);
        assert_eq!(scan.progress().bytes, expected);
        assert!(!scan.progress().apparent);
        assert_counters_match(&scan);
    }

    #[test]
    fn hard_links_are_counted_once_at_a_fixed_place() {
        let t = TempDir::new("links");
        let x = t.file("b/x", 5000);
        t.dir("a/sub");
        fs::hard_link(&x, t.path("a/sub/y")).unwrap();
        fs::hard_link(&x, t.path("a/z")).unwrap();
        fs::hard_link(&x, t.path("c")).unwrap();
        let w = t.file("b/w", 300);
        fs::hard_link(&w, t.path("a/sub/v")).unwrap();
        for threads in [1, 2, 8] {
            let scan = run(&t.0, true, threads);
            let p = scan.progress();
            assert_eq!((p.files, p.bytes), (2, 5300));
            assert_counters_match(&scan);
            let index = scan.index();
            // 第一组：根目录里的 c 最先遇到；第二组：a/sub/v 先于 b/w
            for (path, counted) in [
                ("c", true),
                ("a/z", false),
                ("a/sub/y", false),
                ("b/x", false),
                ("a/sub/v", true),
                ("b/w", false),
            ] {
                assert_eq!(index.find(&t.path(path)).is_some(), counted, "{path}");
            }
            assert_eq!(index.file_count(), 6);
        }
    }

    #[test]
    fn symlinks_are_files_and_not_followed() {
        let t = TempDir::new("symlinks");
        t.file("sub/f", 100);
        symlink("sub", t.path("link")).unwrap();
        symlink("nowhere", t.path("dangling")).unwrap();
        symlink(&t.0, t.path("sub/loop")).unwrap();
        let scan = run(&t.0, true, 2);
        let index = scan.index();
        let root = index.root();
        assert_eq!(names(root.subdirs().map(|d| d.name())), ["sub"]);
        let sizes: Vec<_> = root
            .files()
            .map(|f| (f.name().to_string_lossy().into_owned(), f.size()))
            .collect();
        assert_eq!(sizes, [("dangling".to_owned(), 7), ("link".to_owned(), 3)]);
        // sub 里的 f 和 loop 各算一次，没有跟着 link、loop 重复进入
        assert_eq!((root.nfiles(), root.ndirs()), (4, 1));
    }

    #[test]
    fn denied_directory_is_flagged() {
        // SAFETY: geteuid 没有前置条件。
        if unsafe { libc::geteuid() } == 0 {
            eprintln!("以 root 运行，跳过权限测试");
            return;
        }
        struct Restore(PathBuf);
        impl Drop for Restore {
            fn drop(&mut self) {
                let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
            }
        }
        let t = TempDir::new("denied");
        t.file("ok/a", 1);
        t.file("locked/hidden", 50);
        let locked = t.path("locked");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let _restore = Restore(locked.clone());

        let scan = run(&t.0, true, 2);
        let p = scan.progress();
        assert_eq!(
            (p.denied, p.denied_samples.clone()),
            (1, vec![locked.clone()])
        );
        assert_eq!((p.files, p.dirs, p.bytes), (1, 2, 1));
        let index = scan.index();
        let Some(Found::Dir(id)) = index.find(&locked) else {
            panic!("locked not found")
        };
        let d = index.dir(id);
        assert_eq!(d.flags(), DENIED);
        assert!(d.is_listed() && d.files().next().is_none());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn non_utf8_names_are_kept() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let t = TempDir::new("bytes");
        let name = OsStr::from_bytes(b"caf\xe9");
        fs::write(t.0.join(name), b"abc").unwrap();
        fs::create_dir(t.0.join(OsStr::from_bytes(b"dir\xff"))).unwrap();
        let scan = run(&t.0, true, 1);
        let index = scan.index();
        assert!(matches!(
            index.find(&t.0.join(name)),
            Some(Found::File { .. })
        ));
        assert_eq!(index.root().ndirs(), 1);
        assert_eq!(index.root().size(), 3);
    }
}
