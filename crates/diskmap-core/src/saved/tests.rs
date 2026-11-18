use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::*;
use crate::VERSION;
use crate::analysis;
use crate::classify::{ClassifierOptions, ReasonId};
use crate::i18n::{self, Lang, Text};
use crate::index::{DirId, Index, NewFile};
use crate::model::Category;
use crate::pkgdb::PkgDb;
use crate::procinfo::ProcInfo;
use crate::scan::{MountEntry, ScanMethod, ScanOptions};
use crate::view;

#[cfg(feature = "server")]
mod benchmark;

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let mut bytes = [0; 16];
        getrandom::fill(&mut bytes).unwrap();
        let path =
            std::env::temp_dir().join(format!("diskmap-saved-{:032x}", u128::from_le_bytes(bytes)));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn classifier(os: Os) -> Arc<Classifier> {
    let mut options = ClassifierOptions::for_os(os);
    options.homes = Some(Vec::new());
    Arc::new(Classifier::with_options(None, &[], options))
}

fn progress(index: &Index, finished_at: SystemTime) -> Progress {
    let root = index.root();
    Progress {
        state: ScanState::Done,
        root: index.root_path().to_owned(),
        error: None,
        files: root.nfiles(),
        dirs: root.ndirs(),
        bytes: root.size(),
        denied: 0,
        denied_samples: Vec::new(),
        current: None,
        elapsed: Duration::new(3, 987_654_321),
        finished_at: Some(finished_at),
        apparent: true,
        cross_fs: false,
        fs_used: 321_456,
        mounts: Vec::new(),
        skipped: Vec::new(),
        threads: 4,
        method: ScanMethod::Generic,
        fallback_reason: None,
        records: 0,
        anomalies: 0,
    }
}

fn result_index(os: Os, root: &Path, size: u64) -> Index {
    let mut index = Index::new(os, root, (Category::Other, ReasonId(0)));
    index
        .fill_dir(
            DirId::ROOT,
            &[],
            &[NewFile {
                name: "one.bin".as_ref(),
                size,
                category: Category::User,
                linked: false,
            }],
        )
        .unwrap();
    index
}

fn result(os: Os, root: impl AsRef<Path>, time: u64, size: u64) -> Scan {
    let index = result_index(os, root.as_ref(), size);
    let progress = progress(&index, UNIX_EPOCH + Duration::from_secs(time));
    Scan::from_saved(index, progress, classifier(os)).unwrap()
}

#[test]
fn restores_progress_and_done_behavior_without_the_source() {
    let temp = Scratch::new();
    let store = Store::with_cache_dir(&temp.0);
    let root = temp.0.join("already-gone");
    let index = result_index(Os::current(), &root, 777);
    let mut expected = progress(&index, UNIX_EPOCH - Duration::new(1, 123_000));
    expected.denied = 4;
    expected.denied_samples = vec![root.join("first"), root.join("second")];
    expected.mounts.push(MountEntry {
        path: root.join("mnt"),
        fstype: "ntfs".into(),
        source: Some("D:".into()),
    });
    expected.skipped.push(MountEntry {
        path: root.join("proc"),
        fstype: "proc".into(),
        source: None,
    });
    expected.fallback_reason = Some(Text::new("回退到通用扫描", "Fell back to a generic scan"));
    expected.records = 2237;
    expected.anomalies = 4;
    let c = classifier(Os::current());
    let mut mismatched = expected.clone();
    mismatched.files += 1;
    assert!(Scan::from_saved(index.clone(), mismatched, c.clone()).is_err());
    let scan = Scan::from_saved(index, expected.clone(), c.clone()).unwrap();
    for codec in [Codec::Raw, Codec::Lz4] {
        store.save_with_codec(&scan, codec).unwrap();
        let items = store.list().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].progress, expected);
        assert_eq!(items[0].os, Os::current());
        assert_eq!(items[0].version, VERSION);
        let restored = store.load(&items[0].id, c.clone()).unwrap().unwrap();
        assert_eq!(restored.progress(), expected);
        assert_eq!(restored.root(), &root);
        assert_eq!(restored.apparent(), expected.apparent);
        assert_eq!(restored.cross_fs(), expected.cross_fs);
        assert!(!Arc::ptr_eq(restored.classifier(), &c));
        assert_eq!(restored.classifier().reasons(), c.reasons());
        assert!(restored.is_saved());
        restored.cancel();
        assert_eq!(restored.wait(), ScanState::Done);
        assert_eq!(restored.wait_timeout(Duration::ZERO), ScanState::Done);
        assert_eq!(restored.progress(), expected);
        assert_eq!(restored.index().root().size(), 777);
    }
}

#[test]
fn rejects_an_unsuccessful_scan_without_creating_cache() {
    let temp = Scratch::new();
    let store = Store::with_cache_dir(&temp.0);
    let scan = Scan::start(
        ScanOptions::new(temp.0.join("absent")),
        classifier(Os::current()),
    );
    assert_eq!(scan.wait(), ScanState::Error);
    assert!(!scan.is_saved());
    assert_eq!(
        store.save(&scan).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    assert!(store.list().unwrap().is_empty());
    assert!(!store.directory.exists());
}

#[test]
fn replaces_by_platform_path_key_and_completion_time() {
    let temp = Scratch::new();
    let store = Store::with_cache_dir(&temp.0);
    let os = Os::Windows;
    store.save(&result(os, "C:\\Work\\Σ", 40, 10)).unwrap();
    let id = store.list().unwrap()[0].id.clone();
    let original = store.load(&id, classifier(os)).unwrap().unwrap();
    store.save(&result(os, "c:/work/ς", 50, 42)).unwrap();
    store.save(&result(os, "c:/WORK/σ", 30, 11)).unwrap();
    let entries = store.list().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id, id);
    assert_eq!(entries[0].progress.root, Path::new("c:/work/ς"));
    assert_eq!(entries[0].progress.bytes, 42);
    assert_eq!(original.index().root().size(), 10);
    assert_eq!(
        store
            .load(&id, classifier(os))
            .unwrap()
            .unwrap()
            .index()
            .root()
            .size(),
        42
    );
    let linux = Os::Linux;
    store.save(&result(linux, "/work", 60, 10)).unwrap();
    store.save(&result(linux, "/Work", 70, 10)).unwrap();
    assert_eq!(store.list().unwrap().len(), 3);
    assert_eq!(temp_files(&store), 0);
    assert!(store.delete(&id).unwrap());
    assert!(!store.delete(&id).unwrap());
    assert!(store.load(&id, classifier(os)).unwrap().is_none());
}

#[test]
fn evicts_oldest_completion_even_when_saves_finish_out_of_order() {
    let temp = Scratch::new();
    let store = Store::with_cache_dir(&temp.0);
    let os = Os::Linux;
    for time in [50, 10, 60, 40, 30] {
        store
            .save(&result(os, format!("/test/{time}"), time, time))
            .unwrap();
    }
    store.save(&result(os, "/test/20", 20, 20)).unwrap();
    store.save(&result(os, "/test/1", 1, 1)).unwrap();
    let entries = store.list().unwrap();
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.progress.bytes)
            .collect::<Vec<_>>(),
        [60, 50, 40, 30, 20]
    );
    let files = fs::read_dir(&store.directory)
        .unwrap()
        .filter(|entry| {
            entry
                .as_ref()
                .unwrap()
                .file_name()
                .to_str()
                .and_then(file_id)
                .is_some()
        })
        .count();
    assert_eq!(files, MAX_SAVED);
    assert_eq!(temp_files(&store), 0);
}

#[test]
fn concurrent_store_handles_keep_one_snapshot_per_root() {
    let temp = Scratch::new();
    let store = Store::with_cache_dir(&temp.0);
    let os = Os::Windows;
    std::thread::scope(|scope| {
        for time in 1..=8 {
            let store = Store::with_cache_dir(&temp.0);
            scope.spawn(move || {
                let root = if time % 2 == 0 {
                    "c:/cache"
                } else {
                    "C:\\CACHE"
                };
                store.save(&result(os, root, time, time)).unwrap();
                for file in store.list().unwrap() {
                    let scan = store.load(&file.id, classifier(os)).unwrap().unwrap();
                    assert!(scan.progress().bytes >= file.progress.bytes);
                }
            });
        }
    });
    let entries = store.list().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].progress.bytes, 8);
    assert_eq!(temp_files(&store), 0);
}

#[test]
fn failed_publication_preserves_previous_result_and_removes_temporary_file() {
    let temp = Scratch::new();
    let store = Store::with_cache_dir(&temp.0);
    let os = Os::Linux;
    store.save(&result(os, "/data", 10, 77)).unwrap();
    let lock = store.directory.join(".lock");
    fs::remove_file(&lock).unwrap();
    fs::create_dir(&lock).unwrap();
    assert!(store.save(&result(os, "/data", 20, 91)).is_err());
    fs::remove_dir(&lock).unwrap();
    assert_eq!(store.list().unwrap()[0].progress.bytes, 77);
    assert_eq!(temp_files(&store), 0);
}

fn temp_files(store: &Store) -> usize {
    fs::read_dir(&store.directory)
        .unwrap()
        .filter(|entry| {
            entry
                .as_ref()
                .unwrap()
                .path()
                .extension()
                .is_some_and(|ext| ext == "tmp")
        })
        .count()
}

fn body_start(bytes: &[u8]) -> usize {
    40 + u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize
}

fn recheck(bytes: &mut [u8]) {
    let offset = body_start(bytes);
    let stored_len = (bytes.len() - offset) as u64;
    bytes[16..24].copy_from_slice(&stored_len.to_le_bytes());
    let body_crc = crc32fast::hash(&bytes[offset..]);
    bytes[32..36].copy_from_slice(&body_crc.to_le_bytes());
    let mut header_crc = crc32fast::Hasher::new();
    header_crc.update(&bytes[..36]);
    header_crc.update(&bytes[40..offset]);
    bytes[36..40].copy_from_slice(&header_crc.finalize().to_le_bytes());
}

#[test]
fn list_rejects_bad_header_wrong_format_version_and_truncation() {
    let temp = Scratch::new();
    let store = Store::with_cache_dir(&temp.0);
    store.save(&result(Os::Linux, "/data", 10, 77)).unwrap();
    let item = &store.list().unwrap()[0];
    let path = store.path(&item.id).unwrap();
    let original = fs::read(&path).unwrap();
    let mut magic = original.clone();
    magic[0] ^= 1;
    let mut version = original.clone();
    version[8..12].copy_from_slice(&2u32.to_le_bytes());
    recheck(&mut version);
    let mut metadata = original.clone();
    metadata[44] ^= 1;
    let mut oversized_metadata = original.clone();
    oversized_metadata[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
    let truncated_header = original[..31].to_vec();
    let truncated_body = original[..original.len() - 1].to_vec();
    let mut appended = original;
    appended.push(0);
    for data in [
        magic,
        version,
        metadata,
        oversized_metadata,
        truncated_header,
        truncated_body,
        appended,
    ] {
        fs::write(&path, data).unwrap();
        assert!(store.list().unwrap().is_empty());
        assert!(!path.exists());
    }
}

#[test]
fn list_leaves_same_length_body_damage_for_load_to_detect() {
    let temp = Scratch::new();
    let store = Store::with_cache_dir(&temp.0);
    for codec in [Codec::Raw, Codec::Lz4] {
        store
            .save_with_codec(&result(Os::Linux, "/data", 10, 77), codec)
            .unwrap();
        let item = &store.list().unwrap()[0];
        let path = store.path(&item.id).unwrap();
        let mut bytes = fs::read(&path).unwrap();
        let position = bytes.len() - 1;
        bytes[position] ^= 1;
        fs::write(&path, &bytes).unwrap();
        assert_eq!(store.list().unwrap().len(), 1);
        assert!(
            store
                .load(&item.id, classifier(Os::Linux))
                .unwrap()
                .is_none()
        );
        assert!(!path.exists());
    }
}

#[test]
fn detects_structural_corruption_even_with_updated_checksums() {
    let temp = Scratch::new();
    let store = Store::with_cache_dir(&temp.0);
    let scan = result(Os::Linux, "/data", 10, 77);
    for (codec, invalid_reason) in [(Codec::Raw, false), (Codec::Raw, true), (Codec::Lz4, false)] {
        store.save_with_codec(&scan, codec).unwrap();
        let item = &store.list().unwrap()[0];
        let path = store.path(&item.id).unwrap();
        let mut bytes = fs::read(&path).unwrap();
        match codec {
            Codec::Raw if invalid_reason => {
                let offset = body_start(&bytes);
                bytes[offset + 20..offset + 24].copy_from_slice(&u32::MAX.to_le_bytes());
            }
            Codec::Raw => {
                let offset = body_start(&bytes);
                // 把根目录的父编号指回自己，原有长度及 CRC 仍可匹配。
                bytes[offset + 24..offset + 28].fill(0);
            }
            Codec::Lz4 => bytes.truncate(bytes.len() - 4), // 缺帧结束标识
        }
        recheck(&mut bytes);
        fs::write(&path, &bytes).unwrap();
        assert_eq!(store.list().unwrap().len(), 1);
        assert!(
            store
                .load(&item.id, classifier(Os::Linux))
                .unwrap()
                .is_none()
        );
        assert!(!path.exists());
    }
}

#[test]
fn old_directory_reason_is_remapped_and_current_packages_explain_files() {
    let temp = Scratch::new();
    let store = Store::with_cache_dir(&temp.0);
    let os = Os::Windows;
    let root = Path::new(r"C:\Apps\Program");
    let file = Path::new(r"C:\Apps\Program\tool.bin");
    let build_classifier = |program, package| {
        let mut db = PkgDb::new(os);
        db.insert_app_dir(root, program);
        db.insert_file(file, package);
        Arc::new(Classifier::with_options(
            Some(Arc::new(db)),
            &[],
            ClassifierOptions::for_os(os),
        ))
    };
    let old = build_classifier("Old program", "old-package");
    let current = build_classifier("New program", "current-package");
    let old_class = old.root_class(root);
    let reason = old.reason(old_class.1);
    assert_eq!(old_class.1, current.root_class(root).1);
    assert_eq!(reason.en, "Installed program: Old program");
    assert_eq!(
        current.reason(old_class.1).en,
        "Installed program: New program"
    );
    let mut index = Index::new(os, root, old_class);
    index
        .fill_dir(
            DirId::ROOT,
            &[],
            &[NewFile {
                name: "tool.bin".as_ref(),
                size: 1,
                category: Category::System,
                linked: false,
            }],
        )
        .unwrap();
    let old_progress = progress(&index, UNIX_EPOCH);
    let scan = Scan::from_saved(index, old_progress, old.clone()).unwrap();
    store.save(&scan).unwrap();

    let id = &store.list().unwrap()[0].id;
    let restored = store.load(id, current.clone()).unwrap().unwrap();
    let index = restored.index();
    let class = index.root().class();
    let restored_classifier = restored.classifier();
    assert_ne!(class.1, old_class.1);
    assert_eq!(class.0, old_class.0);
    assert_eq!(restored_classifier.reason(class.1), reason);
    assert_eq!(
        restored_classifier.reasons().len(),
        current.reasons().len() + 1
    );
    assert_eq!(
        restored_classifier.reasons()[..current.reasons().len()],
        *current.reasons()
    );
    let (_, file_reason) = restored_classifier.explain_file(class, root, "tool.bin".as_ref(), file);
    assert!(file_reason.en.contains("current-package"));
    assert!(file_reason.zh.contains("current-package"));
}

#[test]
fn list_and_save_remove_only_expired_inactive_temporary_files() {
    let temp = Scratch::new();
    let store = Store::with_cache_dir(&temp.0);
    let scan = result(Os::current(), "/data", 10, 77);
    store.save(&scan).unwrap();
    let old = SystemTime::now() - TEMP_MAX_AGE - Duration::from_secs(60);
    let obsolete = store.directory.join(format!(".{:032x}.tmp", 1));
    let active = store.directory.join(format!(".{:032x}.tmp", 2));
    let fresh = store.directory.join(format!(".{:032x}.tmp", 3));
    let unrelated = store.directory.join("unrelated.tmp");
    File::create(&obsolete).unwrap().set_modified(old).unwrap();
    File::create(&unrelated).unwrap().set_modified(old).unwrap();
    File::create(&fresh).unwrap();
    let writer = File::create(&active).unwrap();
    writer.set_modified(old).unwrap();
    writer.lock().unwrap();

    assert_eq!(store.list().unwrap().len(), 1);
    assert!(!obsolete.exists());
    assert!(active.exists());
    assert!(fresh.exists());
    assert!(unrelated.exists());

    drop(writer);
    store.save(&scan).unwrap();
    assert!(!active.exists());
    assert!(fresh.exists());
    assert!(unrelated.exists());
}

#[cfg(unix)]
#[test]
fn private_permissions_and_no_following_cache_symlinks() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let temp = Scratch::new();
    let store = Store::with_cache_dir(&temp.0);
    let external = temp.0.join("outside");
    fs::write(&external, "untouched").unwrap();
    fs::create_dir(&store.directory).unwrap();
    fs::set_permissions(&store.directory, fs::Permissions::from_mode(0o755)).unwrap();
    let id = "00000000000000000000000000000000";
    let link = store.path(id).unwrap();
    symlink(&external, &link).unwrap();
    let unrelated = store.directory.join("unrelated.dmap");
    fs::write(&unrelated, "untouched").unwrap();
    assert!(store.list().unwrap().is_empty());
    assert!(store.load(id, classifier(Os::current())).unwrap().is_none());
    assert!(!store.delete(id).unwrap());
    assert!(store.delete("../outside").is_err());
    store.save(&result(Os::current(), "/data", 10, 77)).unwrap();
    let path = store.path(&store.list().unwrap()[0].id).unwrap();
    let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o7777;
    assert_eq!(mode(&store.directory), 0o700);
    assert_eq!(mode(&path), 0o600);
    assert_eq!(fs::read_to_string(external).unwrap(), "untouched");
    assert!(fs::symlink_metadata(link).unwrap().is_symlink());
    assert_eq!(fs::read_to_string(unrelated).unwrap(), "untouched");

    let outside = temp.0.join("outside-cache");
    fs::create_dir(&outside).unwrap();
    let other = Store::with_cache_dir(temp.0.join("link-cache"));
    fs::create_dir(other.directory.parent().unwrap()).unwrap();
    symlink(&outside, &other.directory).unwrap();
    assert!(other.save(&result(Os::current(), "/data", 10, 77)).is_err());
    assert_eq!(fs::read_dir(outside).unwrap().count(), 0);
}

#[test]
fn round_trips_native_metadata_paths_without_unicode_conversion() {
    use std::ffi::OsString;
    let temp = Scratch::new();
    let store = Store::with_cache_dir(&temp.0);
    #[cfg(unix)]
    let raw = {
        use std::os::unix::ffi::OsStringExt;
        OsString::from_vec(b"\xffcaf\xe9".to_vec())
    };
    #[cfg(windows)]
    let raw = {
        use std::os::windows::ffi::OsStringExt;
        OsString::from_wide(&[0xd800, b'a' as u16, 0xdfff])
    };
    let root = temp.0.join(&raw);
    let index = result_index(Os::current(), &root, 9);
    let mut progress = progress(&index, UNIX_EPOCH);
    progress.denied = 1;
    progress.denied_samples.push(root.join(&raw));
    progress.mounts.push(MountEntry {
        path: root.join(&raw),
        fstype: "ext4".into(),
        source: Some("/dev/test".into()),
    });
    let scan = Scan::from_saved(index, progress.clone(), classifier(Os::current())).unwrap();
    store.save(&scan).unwrap();
    let items = store.list().unwrap();
    assert_eq!(items[0].progress, progress);
    assert_eq!(
        store
            .load(&items[0].id, classifier(Os::current()))
            .unwrap()
            .unwrap()
            .progress(),
        progress
    );
}

struct Source(analysis::Snapshot);

impl analysis::Source for Source {
    fn snapshot(&self) -> analysis::Snapshot {
        self.0.clone()
    }
    fn request_show(&self, _path: &Path, _note: &str, _now: f64) {}
}

#[test]
fn live_scan_matches_restored_tree_details_and_analysis_in_both_languages() {
    let temp = Scratch::new();
    let root = temp.0.join("source");
    let file = root.join("big.tmp");
    fs::create_dir_all(root.join("empty")).unwrap();
    fs::write(&file, [0; 100]).unwrap();
    fs::write(root.join("readme"), [0; 42]).unwrap();
    #[cfg(unix)]
    fs::hard_link(&file, root.join("link.tmp")).unwrap();
    let mut options = ScanOptions::new(&root);
    options.apparent = true;
    let c = classifier(Os::current());
    let live = Scan::start(options, c.clone());
    assert_eq!(live.wait(), ScanState::Done);
    assert!(!live.is_saved());
    let store = Store::with_cache_dir(&temp.0);
    store.save(&live).unwrap();
    let restored = store
        .load(&store.list().unwrap()[0].id, c.clone())
        .unwrap()
        .unwrap();
    assert_eq!(restored.progress(), live.progress());
    let snapshot = ProcInfo::new().snapshot();
    let mut data = analysis::Snapshot::new(snapshot.clone(), Arc::new(PkgDb::default()));
    data.scan = Some(live.clone().into());
    let before = Source(data.clone());
    data.scan = Some(restored.clone().into());
    let after = Source(data);
    let query_file = live.root().join("big.tmp");
    for lang in [Lang::Zh, Lang::En] {
        let _language = i18n::scope(lang);
        assert_eq!(analysis::status(&before), analysis::status(&after));
        assert_eq!(
            analysis::summary(&before, None, 10).unwrap(),
            analysis::summary(&after, None, 10).unwrap()
        );
        assert_eq!(
            analysis::list_dir(&before, None, 50).unwrap(),
            analysis::list_dir(&after, None, 50).unwrap()
        );
        assert_eq!(
            analysis::explain(&before, &query_file).unwrap(),
            analysis::explain(&after, &query_file).unwrap()
        );
        assert_eq!(
            analysis::largest_files(&before, None, 20, None).unwrap(),
            analysis::largest_files(&after, None, 20, None).unwrap()
        );
        assert_eq!(
            analysis::cleanup(&before, None, 30, 1).unwrap(),
            analysis::cleanup(&after, None, 30, 1).unwrap()
        );
        let a = live.index();
        let b = restored.index();
        assert_eq!(
            view::ViewCache::new().tree(&a, snapshot.clone(), live.root(), 0.0, None, true),
            view::ViewCache::new().tree(&b, snapshot.clone(), live.root(), 0.0, None, true)
        );
        for path in [live.root(), &query_file, &live.root().join("empty")] {
            assert_eq!(
                view::info(&a, &snapshot, &c, None, path),
                view::info(&b, &snapshot, &c, None, path)
            );
        }
    }
}
