//! T15：同一组路径同时经过索引、分类、软件包、进程、视图和分析；完全使用内存数据。

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use diskmap_core::analysis::{self, ScanSnapshot, Snapshot, Source};
use diskmap_core::classify::{Classifier, ClassifierOptions, ReasonId};
use diskmap_core::index::{DirId, Index, NewDir, NewFile};
use diskmap_core::model::Category;
use diskmap_core::pkgdb::PkgDb;
use diskmap_core::platform::{Os, path};
use diskmap_core::procinfo::{Kind, ProcInfo, ProcessMap, Usage};
use diskmap_core::scan::{Progress, ScanMethod, ScanState};
use diskmap_core::view::ViewCache;

struct Frozen(Snapshot);

impl Source for Frozen {
    fn snapshot(&self) -> Snapshot {
        self.0.clone()
    }

    fn request_show(&self, _: &Path, _: &str, _: f64) {}
}

fn joined(os: Os, parent: &Path, name: &OsStr) -> PathBuf {
    let mut result = parent.as_os_str().to_owned();
    result.push(os.sep().to_string());
    result.push(name);
    result.into()
}

fn progress(root: &Path) -> Progress {
    Progress {
        state: ScanState::Done,
        error: None,
        root: root.to_owned(),
        files: 1,
        dirs: 1,
        bytes: 123,
        denied: 0,
        denied_samples: Vec::new(),
        current: None,
        elapsed: Duration::ZERO,
        finished_at: None,
        apparent: true,
        cross_fs: false,
        fs_used: 0,
        mounts: Vec::new(),
        skipped: Vec::new(),
        threads: 1,
        method: ScanMethod::Generic,
        fallback_reason: None,
        records: 0,
        anomalies: 0,
    }
}

fn check_pair(os: Os, stored: &OsStr, queried: &OsStr, equal: bool) {
    let label = format!("{os:?}: {stored:?} / {queried:?}");
    let a = stored.as_encoded_bytes();
    let b = queried.as_encoded_bytes();
    assert_eq!(path::cmp_names(os, a, b).is_eq(), equal, "{label}");
    assert_eq!(
        path::name_key(os, a) == path::name_key(os, b),
        equal,
        "{label}"
    );

    let root = Path::new(if os == Os::Windows {
        r"C:\fixture"
    } else {
        "/fixture"
    });
    let stored_dir = joined(os, root, stored);
    let query_dir = joined(os, root, queried);
    let mut stored_name = stored.to_owned();
    stored_name.push(".data");
    let mut query_name = queried.to_owned();
    query_name.push(".data");
    let stored_file = joined(os, &stored_dir, &stored_name);
    let query_file = joined(os, &query_dir, &query_name);

    let mut db = PkgDb::new(os);
    db.insert_file(&stored_file, "fixture-package");
    db.insert_backup(&stored_file);
    db.insert_app_dir(&stored_dir, "Fixture app");
    assert_eq!(db.owner(&query_file).is_some(), equal, "pkgdb {label}");
    assert_eq!(db.is_backup(&query_file), equal, "backup {label}");
    let db = Arc::new(db);
    let mut options = ClassifierOptions::for_os(os);
    options.homes = Some(Vec::new());
    let classifier = Arc::new(Classifier::with_options(Some(db.clone()), &[], options));
    assert_eq!(
        classifier.root_class(&query_dir).0 == Category::System,
        equal,
        "classify dir {label}"
    );
    let context = classifier.enter_dir(classifier.root_class(&query_dir), &query_dir);
    assert_eq!(
        classifier.file_class(&context, &query_name) == Category::Config,
        equal,
        "classify file {label}"
    );

    let processes = ProcInfo::with_os(os);
    let mut usage = ProcessMap::default();
    usage.insert(42, Usage::new("fixture", Kind::Open));
    processes.install(vec![(stored_file.clone(), usage.clone())]);
    assert_eq!(
        processes.snapshot().lookup(&query_file).is_some(),
        equal,
        "procinfo file {label}"
    );
    assert_eq!(
        processes.snapshot().dir_count(&query_dir),
        usize::from(equal),
        "procinfo dir {label}"
    );
    // 进程只登记查询拼写，让视图和分析从另一个方向匹配索引。
    processes.install(vec![(query_file.clone(), usage)]);
    let processes = processes.snapshot();
    let class = (Category::Other, ReasonId(0));
    let mut index = Index::new(os, root, class);
    index
        .fill_dir(
            DirId::ROOT,
            &[NewDir {
                name: stored,
                class,
                flags: 0,
            }],
            &[],
        )
        .unwrap();
    let child = index.find_subdir(DirId::ROOT, stored).unwrap();
    index
        .fill_dir(
            child,
            &[],
            &[NewFile {
                name: &stored_name,
                size: 123,
                category: Category::Other,
                linked: false,
            }],
        )
        .unwrap();
    assert_eq!(index.find(&query_dir).is_some(), equal, "index dir {label}");
    assert_eq!(
        index.find(&query_file).is_some(),
        equal,
        "index file {label}"
    );
    let running_bytes = if equal { 123 } else { 0 };
    let mut view = ViewCache::with_snapshot(processes.clone());
    let tree = view
        .tree(&index, processes.clone(), root, 0.0, Some(&db), true)
        .unwrap();
    assert_eq!(
        tree["totals"][Category::Running.index()],
        running_bytes,
        "view totals {label}"
    );
    assert_eq!(
        view.tree(&index, processes.clone(), &query_dir, 0.0, Some(&db), true)
            .is_some(),
        equal,
        "view lookup {label}"
    );
    let mut snapshot = Snapshot::new(processes, db);
    snapshot.os = os;
    snapshot.scan = Some(ScanSnapshot::Frozen {
        index: Arc::new(index),
        progress: Box::new(progress(root)),
        classifier,
    });
    let source = Frozen(snapshot);
    let summary = analysis::summary(&source, None, 10).unwrap();
    assert_eq!(
        summary["in_use"]["bytes"], running_bytes,
        "analysis running {label}"
    );
    assert_eq!(
        analysis::list_dir(&source, Some(&query_dir), 10).is_ok(),
        equal,
        "analysis dir {label}"
    );
    assert_eq!(
        analysis::show(&source, &query_file, None).is_ok(),
        equal,
        "analysis file {label}"
    );
}

#[test]
fn windows_simple_uppercase_is_consistent_across_modules() {
    for (a, b, windows_equal) in [
        ("Σ", "σ", true),
        ("Σ", "ς", true),
        ("οΣ", "ΟΣ", true),
        ("ß", "ẞ", false),
        ("ß", "ss", false),
        ("İ", "i", false),
        ("ı", "i", true),
        ("ﬀ", "ff", false),
        ("ａｂ", "ＡＢ", true),
        ("ꭰ", "Ꭰ", true),
        ("ᾀ", "ᾈ", true),
        ("K", "k", false),
        ("𐐀", "𐐨", false),
        ("é", "e\u{301}", false),
        ("name.ExE", "NAME.eXe", true),
        ("identical", "identical", true),
    ] {
        for os in [Os::Windows, Os::Linux, Os::Macos] {
            check_pair(
                os,
                OsStr::new(a),
                OsStr::new(b),
                if os == Os::Windows {
                    windows_equal
                } else {
                    a == b
                },
            );
        }
    }
}

#[cfg(any(unix, windows))]
fn surrogate(unit: u16, suffix: &str) -> OsString {
    #[cfg(unix)]
    let mut name = {
        use std::os::unix::ffi::OsStringExt;
        OsString::from_vec(vec![
            0xe0 | (unit >> 12) as u8,
            0x80 | ((unit >> 6) & 63) as u8,
            0x80 | (unit & 63) as u8,
        ])
    };
    #[cfg(windows)]
    let mut name = {
        use std::os::windows::ffi::OsStringExt;
        OsString::from_wide(&[unit])
    };
    name.push(suffix);
    name
}

#[cfg(any(unix, windows))]
#[test]
fn unpaired_surrogates_remain_distinct_across_modules() {
    for os in [Os::Windows, Os::Linux, Os::Macos] {
        check_pair(
            os,
            &surrogate(0xd800, "A"),
            &surrogate(0xd800, "a"),
            os == Os::Windows,
        );
        check_pair(os, &surrogate(0xd800, "A"), &surrogate(0xd801, "A"), false);
        check_pair(os, &surrogate(0xdc00, "A"), OsStr::new("�A"), false);
    }
}
