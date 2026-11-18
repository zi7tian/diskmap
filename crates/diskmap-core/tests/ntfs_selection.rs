//! 扫描方式的公共接口测试：只读测试自己建立的临时目录。

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use diskmap_core::classify::{Classifier, ClassifierOptions};
use diskmap_core::i18n::{Lang, Text};
use diskmap_core::platform::Os;
use diskmap_core::scan::{Scan, ScanMethod, ScanOptions, ScanState};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "diskmap-ntfs-selection-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir(&root).unwrap();
        let fixture = Self(root);
        fs::create_dir(fixture.0.join("nested")).unwrap();
        fs::create_dir(fixture.0.join("nested/empty")).unwrap();
        fs::write(fixture.0.join("first.bin"), [1; 123]).unwrap();
        fs::write(fixture.0.join("nested/second.bin"), [2; 7]).unwrap();
        fs::write(fixture.0.join("zero.bin"), []).unwrap();
        fixture
    }

    fn start(&self, method: ScanMethod) -> Scan {
        let mut options = ScanOptions::new(&self.0);
        options.method = method;
        options.apparent = true;
        options.threads = 1;
        Scan::start(
            options,
            Arc::new(Classifier::with_options(
                None,
                &[],
                ClassifierOptions::for_os(Os::current()),
            )),
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // 只清理用 create_dir 独占创建的目录及本测试已知的固定名字。
        for file in ["first.bin", "nested/second.bin", "zero.bin"] {
            let _ = fs::remove_file(self.0.join(file));
        }
        for dir in ["nested/empty", "nested"] {
            let _ = fs::remove_dir(self.0.join(dir));
        }
        let _ = fs::remove_dir(&self.0);
    }
}

fn assert_bilingual(text: &Text) {
    assert!(!text.get(Lang::Zh).is_empty());
    assert!(!text.get(Lang::En).is_empty());
    assert_ne!(text.get(Lang::Zh), text.get(Lang::En));
}

fn assert_complete(scan: &Scan) {
    assert_eq!(scan.wait(), ScanState::Done);
    let progress = scan.progress();
    let index = scan.index();
    assert_eq!((progress.files, progress.dirs, progress.bytes), (3, 2, 130));
    assert_eq!(progress.denied, 0);
    assert_eq!(progress.records, 0);
    assert!(progress.error.is_none());
    assert_eq!(progress.method, ScanMethod::Generic);
    assert_eq!(index.root().nfiles(), progress.files);
    assert_eq!(index.root().ndirs(), progress.dirs);
    assert_eq!(index.root().size(), progress.bytes);
    assert!(index.dirs().all(|dir| dir.is_listed()));
}

#[test]
fn explicit_generic_preserves_counts_and_has_no_fallback_reason() {
    let fixture = Fixture::new();
    let scan = fixture.start(ScanMethod::Generic);
    assert_complete(&scan);
    assert!(scan.progress().fallback_reason.is_none());
}

#[test]
fn automatic_subfolder_scan_reports_why_it_uses_generic() {
    let fixture = Fixture::new();
    assert_eq!(ScanOptions::new(&fixture.0).method, ScanMethod::Auto);
    let scan = fixture.start(ScanMethod::Auto);
    assert_complete(&scan);
    let reason = scan.progress().fallback_reason.unwrap();
    assert_bilingual(&reason);
    #[cfg(not(windows))]
    assert!(reason.get(Lang::En).contains("Windows"));
    #[cfg(windows)]
    assert!(reason.get(Lang::En).contains("entire local volume"));
}

#[test]
fn forced_mft_failure_is_an_error_and_publishes_no_partial_index() {
    let fixture = Fixture::new();
    let scan = fixture.start(ScanMethod::Mft);
    assert_eq!(scan.wait(), ScanState::Error);
    let progress = scan.progress();
    assert_eq!(progress.method, ScanMethod::Mft);
    assert!(progress.fallback_reason.is_none());
    assert_bilingual(progress.error.as_ref().unwrap());
    assert_eq!(
        (
            progress.files,
            progress.dirs,
            progress.bytes,
            progress.records
        ),
        (0, 0, 0, 0)
    );
    let index = scan.index();
    assert_eq!(index.dir_count(), 1);
    assert_eq!(index.file_count(), 0);
    assert_eq!(index.root().size(), 0);
    assert!(!index.root().is_listed());
    // 强制失败也不能修改读到的目录内容。
    assert_eq!(fs::read(fixture.0.join("first.bin")).unwrap(), [1; 123]);
}
