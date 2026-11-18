//! 用系统分配器的薄包装计数，验证扫描热路径（含 Windows Unicode 折叠）不分配。

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::ffi::OsStr;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use diskmap_core::classify::{Classifier, ClassifierOptions};
use diskmap_core::pkgdb::PkgDb;
use diskmap_core::platform::Os;

thread_local! {
    static COUNT: Cell<Option<usize>> = const { Cell::new(None) };
}
struct CountingSystem;
fn allocated() {
    let _ = COUNT.try_with(|n| {
        if let Some(value) = n.get() {
            n.set(Some(value + 1));
        }
    });
}

// SAFETY: 所有内存操作原样委托标准库 System 分配器，计数器不分配内存、不保存指针。
unsafe impl GlobalAlloc for CountingSystem {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        allocated();
        // SAFETY: 调用者保证 GlobalAlloc 的布局有效性，原样交给系统分配器。
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        allocated();
        // SAFETY: 同 alloc，布局由调用者保证。
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: 指针及原始布局由 GlobalAlloc 调用者保证有效。
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        allocated();
        // SAFETY: 指针、原布局及新大小满足 GlobalAlloc 的调用约定。
        unsafe { System.realloc(ptr, layout, size) }
    }
}
#[global_allocator]
static ALLOCATOR: CountingSystem = CountingSystem;

fn setup(os: Os) -> (Classifier, PathBuf, Vec<(String, PathBuf)>) {
    let (home, separator) = match os {
        Os::Linux => ("/home/alice", '/'),
        Os::Macos => ("/Users/alice", '/'),
        Os::Windows => ("C:\\Users\\ALİCEΟΣ", '\\'),
    };
    let mut db = PkgDb::new(os);
    let mut items = Vec::new();
    for name in [
        "owned.bin",
        "settings.conf",
        "kernel.swap",
        "ordinary.txt",
        "a.log",
        "a.LOG",
        ".dotfile",
        "NTUSER.DAT",
        ".config",
        "Cache",
        "ΟΔΟΣ",
        "İ.DAT",
        "foo.baK",
        "Service WorKer",
    ] {
        items.push((
            name.to_string(),
            PathBuf::from(format!("{home}{separator}{name}")),
        ));
    }
    db.insert_file(&items[0].1, "demo");
    db.insert_file(&items[1].1, "demo");
    db.insert_backup(&items[1].1);
    db.insert_file(&items[10].1, "unicode");
    db.insert_app_dir(&items[10].1, "Unicode");
    let mut options = ClassifierOptions::for_os(os);
    options.homes = Some(vec![home.into()]);
    let c = Classifier::with_options(
        Some(Arc::new(db)),
        std::slice::from_ref(&items[2].1),
        options,
    );
    (c, home.into(), items)
}

#[test]
fn file_and_directory_classification_allocate_nothing() {
    for os in [Os::Linux, Os::Macos, Os::Windows] {
        let (c, home, items) = setup(os);
        let ctx = c.enter_dir(c.root_class(&home), &home);
        COUNT.with(|count| count.set(Some(0)));
        for _ in 0..1000 {
            for (name, path) in &items {
                black_box(c.file_class(black_box(&ctx), black_box(OsStr::new(name))));
                black_box(c.dir_class(
                    black_box(&ctx),
                    black_box(OsStr::new(name)),
                    black_box(path),
                ));
            }
        }
        let allocations = COUNT.with(|count| count.replace(None)).expect("计数已启动");
        assert_eq!(allocations, 0, "{os:?}");
    }
}

#[test]
#[ignore = "手动性能测量：cargo test --release -p diskmap-core --test classify_allocations -- --ignored --nocapture"]
fn classification_timing() {
    let iterations = 1_000_000;
    for os in [Os::Linux, Os::Macos, Os::Windows] {
        let (c, home, items) = setup(os);
        let ctx = c.enter_dir(c.root_class(&home), &home);
        let start = Instant::now();
        for i in 0..iterations {
            let (name, _) = &items[i % items.len()];
            black_box(c.file_class(black_box(&ctx), black_box(OsStr::new(name))));
        }
        let files = start.elapsed();
        let start = Instant::now();
        for i in 0..iterations {
            let (name, path) = &items[i % items.len()];
            black_box(c.dir_class(
                black_box(&ctx),
                black_box(OsStr::new(name)),
                black_box(Path::new(path)),
            ));
        }
        eprintln!(
            "{}: {} file_class={:.3}ms dir_class={:.3}ms",
            os.key(),
            iterations,
            files.as_secs_f64() * 1000.0,
            start.elapsed().as_secs_f64() * 1000.0
        );
    }
}
