use std::ffi::OsStr;
use std::path::Path;

use super::*;
use crate::classify::{ClassifierOptions, ReasonId};
use crate::i18n::{Lang, scope};
use crate::index::{FileId, NewDir, NewFile};
use crate::platform::Os;
use crate::procinfo::{Kind, ProcInfo, ProcessMap, Usage};

const OTHER: (Category, ReasonId) = (Category::Other, ReasonId(0));

fn file(name: &str, size: u64, category: Category) -> NewFile<'_> {
    NewFile {
        name: OsStr::new(name),
        size,
        category,
        linked: false,
    }
}

fn procs(paths: &[&str]) -> ProcInfo {
    let proc = ProcInfo::with_os(Os::Linux);
    proc.install(paths.iter().map(|path| {
        let mut map = ProcessMap::default();
        map.insert(42, Usage::new("editor", Kind::Open));
        (PathBuf::from(path), map)
    }));
    proc
}

#[test]
fn tree_fields_totals_small_items_and_packages() {
    let _lang = scope(Lang::En);
    let mut index = Index::new(Os::Linux, Path::new("/fixture"), OTHER);
    index
        .fill_dir(
            DirId::ROOT,
            &[NewDir {
                name: OsStr::new("empty"),
                class: OTHER,
                flags: DENIED,
            }],
            &[
                file("big", 10_000, Category::User),
                file("live", 1_000, Category::Other),
                file("tiny", 1, Category::Cache),
            ],
        )
        .unwrap();
    let proc = procs(&["/fixture/live"]);
    let mut pkgdb = PkgDb::new(Os::Linux);
    pkgdb.insert_file(Path::new("/fixture/big"), "example");
    let value = ViewCache::new()
        .tree(
            &index,
            proc.snapshot(),
            Path::new("/fixture"),
            0.001,
            Some(&pkgdb),
            true,
        )
        .unwrap();
    assert_eq!(value["totals"], json!([0, 1000, 0, 0, 10000, 1, 0]));
    assert_eq!(value["tree"]["r"], 1);
    assert_eq!(
        value["tree"]["ch"],
        json!([
            {"n":"big","s":10000,"c":4,"t":"f","pk":"example"},
            {"n":"live","s":1000,"c":1,"t":"f","sc":6,"p":"editor"},
            {"n":"","s":1,"c":5,"t":"a","k":2},
        ])
    );
}

#[test]
fn largest_150_and_running_files_are_kept() {
    let mut index = Index::new(Os::Linux, Path::new("/fixture"), OTHER);
    let names: Vec<_> = (0..160).map(|i| format!("file{i:03}")).collect();
    let files: Vec<_> = names
        .iter()
        .enumerate()
        .map(|(i, name)| file(name, i as u64 + 1, Category::Other))
        .collect();
    index.fill_dir(DirId::ROOT, &[], &files).unwrap();
    let proc = procs(&["/fixture/file000"]);
    let value = ViewCache::new()
        .tree(
            &index,
            proc.snapshot(),
            Path::new("/fixture"),
            0.0,
            None,
            true,
        )
        .unwrap();
    let children = value["tree"]["ch"].as_array().unwrap();
    assert_eq!(children.len(), 152);
    assert!(children.iter().any(|child| child["n"] == "file000"));
    assert_eq!(
        children.iter().find(|child| child["t"] == "a").unwrap(),
        &json!({"n":"","s":54,"c":6,"t":"a","k":9})
    );
    assert_eq!(value["totals"][1], 1);
}

#[test]
fn breadth_first_budget_and_depth_mark_unexpanded_directories() {
    let mut index = Index::new(Os::Linux, Path::new("/fixture"), OTHER);
    let child = index
        .fill_dir(
            DirId::ROOT,
            &[NewDir {
                name: OsStr::new("dir"),
                class: OTHER,
                flags: 0,
            }],
            &[],
        )
        .unwrap();
    index
        .fill_dir(
            DirId(child.dirs.start),
            &[],
            &[file("data", 1, Category::Other)],
        )
        .unwrap();
    let proc = procs(&[]);
    for options in [
        TreeOptions {
            max_nodes: 1,
            ..TreeOptions::default()
        },
        TreeOptions {
            max_depth: 1,
            ..TreeOptions::default()
        },
    ] {
        let value = ViewCache::new()
            .tree_with_options(
                &index,
                proc.snapshot(),
                Path::new("/fixture"),
                options,
                None,
                true,
            )
            .unwrap();
        assert_eq!(value["tree"]["ch"][0]["m"], 1);
        assert!(value["tree"]["ch"][0].get("ch").is_none());
    }
}

#[test]
fn cache_advances_with_directory_publication_without_rechecking_all_processes() {
    let mut index = Index::new(Os::Linux, Path::new("/fixture"), OTHER);
    let proc = procs(&["/fixture/first", "/fixture/sub/second", "/unrelated/a"]);
    let mut cache = ViewCache::new();
    let request = |cache: &mut ViewCache, index: &Index, finished| {
        cache
            .tree(
                index,
                proc.snapshot(),
                Path::new("/fixture"),
                0.0,
                None,
                finished,
            )
            .unwrap()
    };
    assert_eq!(request(&mut cache, &index, false)["totals"][1], 0);
    let ids = index
        .fill_dir(
            DirId::ROOT,
            &[NewDir {
                name: OsStr::new("sub"),
                class: OTHER,
                flags: 0,
            }],
            &[file("first", 10, Category::User)],
        )
        .unwrap();
    assert_eq!(request(&mut cache, &index, false)["totals"][1], 10);
    for _ in 0..10 {
        request(&mut cache, &index, false);
    }
    assert_eq!(cache.running.file_lookups, 1);
    index
        .fill_dir(
            DirId(ids.dirs.start),
            &[],
            &[file("second", 20, Category::Cache)],
        )
        .unwrap();
    assert_eq!(request(&mut cache, &index, true)["totals"][1], 30);
    assert_eq!(cache.running.file_lookups, 3);
    for _ in 0..10 {
        request(&mut cache, &index, true);
    }
    assert_eq!(cache.running.file_lookups, 3);
    proc.install(Vec::new());
    assert_eq!(request(&mut cache, &index, true)["totals"][1], 0);
}

#[test]
fn delayed_hardlinks_are_added_once_at_scan_end() {
    let mut index = Index::new(Os::Linux, Path::new("/fixture"), OTHER);
    let mut linked = file("linked", 10, Category::System);
    linked.linked = true;
    let ids = index.fill_dir(DirId::ROOT, &[], &[linked]).unwrap();
    let proc = procs(&["/fixture/linked", "/fixture/missing"]);
    let mut cache = ViewCache::new();
    let request = |cache: &mut ViewCache, index: &Index, finished| {
        cache
            .tree(
                index,
                proc.snapshot(),
                Path::new("/fixture"),
                0.0,
                None,
                finished,
            )
            .unwrap()
    };
    assert_eq!(request(&mut cache, &index, false)["totals"][1], 0);
    assert_eq!(cache.running.file_lookups, 2);
    request(&mut cache, &index, false);
    assert_eq!(cache.running.file_lookups, 2);
    index.count_file(DirId::ROOT, FileId(ids.files.start));
    assert_eq!(request(&mut cache, &index, true)["totals"][1], 10);
    assert_eq!(cache.running.file_lookups, 4);
    assert_eq!(request(&mut cache, &index, true)["totals"][1], 10);
    assert_eq!(cache.running.file_lookups, 4);
}

#[test]
fn info_reaches_compacted_files_and_tree_rejects_files() {
    let classifier = Classifier::with_options(None, &[], ClassifierOptions::for_os(Os::Linux));
    let root = Path::new("/fixture");
    let mut index = Index::new(Os::Linux, root, classifier.root_class(root));
    index
        .fill_dir(DirId::ROOT, &[], &[file("data", 1, Category::Other)])
        .unwrap();
    let proc = procs(&["/fixture/data"]);
    let value = info(
        &index,
        &proc.snapshot(),
        &classifier,
        None,
        Path::new("/fixture/data"),
    )
    .unwrap();
    assert_eq!(value["cls"], 1);
    assert_eq!(value["nprocs"], 1);
    assert_eq!(value["size"], 1);
    assert_eq!(value["procs"][0]["pid"], 42);
    assert!(
        ViewCache::new()
            .tree(
                &index,
                proc.snapshot(),
                Path::new("/fixture/data"),
                0.0,
                None,
                true,
            )
            .is_none()
    );
}

#[test]
fn windows_paths_match_case_and_separator_variants() {
    let mut index = Index::new(Os::Windows, Path::new("C:\\Fixture"), OTHER);
    let ids = index
        .fill_dir(
            DirId::ROOT,
            &[NewDir {
                name: OsStr::new("Sub"),
                class: OTHER,
                flags: 0,
            }],
            &[],
        )
        .unwrap();
    index
        .fill_dir(
            DirId(ids.dirs.start),
            &[],
            &[file("DATA", 123, Category::User)],
        )
        .unwrap();
    let proc = ProcInfo::with_os(Os::Windows);
    let mut map = ProcessMap::default();
    map.insert(42, Usage::new("editor", Kind::Open));
    proc.install([(PathBuf::from("c:/fixture/sub/data"), map)]);
    let value = ViewCache::new()
        .tree(
            &index,
            proc.snapshot(),
            Path::new("c:/FIXTURE"),
            0.0,
            None,
            true,
        )
        .unwrap();
    assert_eq!(value["totals"][1], 123);
    assert_eq!(value["tree"]["ch"][0]["ch"][0]["c"], 1);
}

#[test]
fn windows_unicode_directory_aliases_share_running_and_retained_matches() {
    for (stored, queried, matches) in [
        ("Σ", "ς", true),
        ("ΟΣ", "οσ", true),
        ("ß", "ẞ", false),
        ("İ", "i", false),
        ("ı", "i", true),
        ("ﬀ", "FF", false),
        ("Ａ", "ａ", true),
        ("Ꭰ", "ꭰ", true),
        ("𐐀", "𐐨", false),
    ] {
        let root = Path::new(r"C:\Fixture");
        let mut index = Index::new(Os::Windows, root, OTHER);
        let ids = index
            .fill_dir(
                DirId::ROOT,
                &[NewDir {
                    name: OsStr::new(stored),
                    class: OTHER,
                    flags: 0,
                }],
                &[],
            )
            .unwrap();
        let mut names: Vec<_> = (0..150).map(|i| format!("big{i:03}")).collect();
        names.extend(["small.DaT".into(), "spare".into()]);
        names.sort_by(|a, b| index.cmp_names(OsStr::new(a), OsStr::new(b)));
        let files: Vec<_> = names
            .iter()
            .map(|name| {
                file(
                    name,
                    if name.starts_with("big") { 100 } else { 1 },
                    Category::Other,
                )
            })
            .collect();
        index.fill_dir(DirId(ids.dirs.start), &[], &files).unwrap();
        let proc = ProcInfo::with_os(Os::Windows);
        let mut map = ProcessMap::default();
        map.insert(42, Usage::new("editor", Kind::Open));
        proc.install([(
            PathBuf::from(format!("c:/fixture/{queried}/SMALL.dAt")),
            map,
        )]);
        let snapshot = proc.snapshot();
        let mut cache = ViewCache::with_snapshot(snapshot.clone());
        for finished in [false, true] {
            let value = cache
                .tree(&index, snapshot.clone(), root, 0.0, None, finished)
                .unwrap();
            assert_eq!(value["totals"][1], u64::from(matches), "{stored}/{queried}");
            let children = value["tree"]["ch"][0]["ch"].as_array().unwrap();
            assert_eq!(
                children.iter().any(|child| child["n"] == "small.DaT"),
                matches,
                "{stored}/{queried}"
            );
        }
    }
}

#[test]
fn zero_size_and_category_ties_keep_legacy_aggregation() {
    let mut index = Index::new(Os::Linux, Path::new("/fixture"), OTHER);
    index
        .fill_dir(
            DirId::ROOT,
            &[],
            &[file("a", 0, Category::Cache), file("b", 0, Category::User)],
        )
        .unwrap();
    let value = ViewCache::new()
        .tree(
            &index,
            procs(&[]).snapshot(),
            Path::new("/fixture"),
            0.0,
            None,
            true,
        )
        .unwrap();
    assert_eq!(
        value["tree"]["ch"],
        json!([{"n":"","s":0,"c":4,"t":"a","k":2}])
    );
}

#[cfg(unix)]
#[test]
fn invalid_utf8_names_remain_distinct_in_running_cache() {
    use std::os::unix::ffi::OsStrExt;

    let mut index = Index::new(Os::Linux, Path::new("/fixture"), OTHER);
    index
        .fill_dir(
            DirId::ROOT,
            &[],
            &[
                NewFile {
                    name: OsStr::from_bytes(b"a\xfe"),
                    size: 5,
                    category: Category::User,
                    linked: false,
                },
                NewFile {
                    name: OsStr::from_bytes(b"a\xff"),
                    size: 7,
                    category: Category::User,
                    linked: false,
                },
            ],
        )
        .unwrap();
    let proc = ProcInfo::with_os(Os::Linux);
    let mut map = ProcessMap::default();
    map.insert(42, Usage::new("editor", Kind::Open));
    proc.install([(PathBuf::from(OsStr::from_bytes(b"/fixture/a\xff")), map)]);
    let value = ViewCache::new()
        .tree(
            &index,
            proc.snapshot(),
            Path::new("/fixture"),
            0.0,
            None,
            true,
        )
        .unwrap();
    assert_eq!(value["totals"][1], 7);
    assert_eq!(value["totals"][4], 5);
}

#[test]
#[ignore = "在用命中缓存基准：仅测量缓存更新，不包含构造快照和索引"]
fn running_cache_incremental_benchmark() {
    use std::time::Instant;

    const DIRS: usize = 100;
    const FILES: usize = 100;
    const REPEATS: usize = 10_000;
    let mut index = Index::new(Os::Linux, Path::new("/fixture"), OTHER);
    let dir_names: Vec<_> = (0..DIRS).map(|n| format!("dir{n:03}")).collect();
    let file_names: Vec<_> = (0..FILES).map(|n| format!("file{n:03}")).collect();
    let dirs: Vec<_> = dir_names
        .iter()
        .map(|name| NewDir {
            name: OsStr::new(name),
            class: OTHER,
            flags: 0,
        })
        .collect();
    let files: Vec<_> = file_names
        .iter()
        .map(|name| file(name, 1, Category::Other))
        .collect();
    let proc = ProcInfo::with_os(Os::Linux);
    proc.install(dir_names.iter().flat_map(|dir| {
        file_names.iter().map(move |file| {
            let mut map = ProcessMap::default();
            map.insert(42, Usage::new("editor", Kind::Open));
            (PathBuf::from(format!("/fixture/{dir}/{file}")), map)
        })
    }));
    let snapshot = proc.snapshot();
    let mut cache = running::RunningCache::default();
    let start = Instant::now();
    cache.update(&index, Arc::clone(&snapshot), &snapshot, false);
    let initial = start.elapsed();
    let range = index.fill_dir(DirId::ROOT, &dirs, &[]).unwrap().dirs;
    let mut incremental = std::time::Duration::ZERO;
    for id in range {
        index.fill_dir(DirId(id), &[], &files).unwrap();
        let start = Instant::now();
        cache.update(&index, Arc::clone(&snapshot), &snapshot, false);
        incremental += start.elapsed();
    }
    assert_eq!(cache.file_lookups, DIRS * FILES);
    assert_eq!(cache.sizes(DirId::ROOT)[Category::Other.index()], 10_000);
    let start = Instant::now();
    for _ in 0..REPEATS {
        cache.update(&index, Arc::clone(&snapshot), &snapshot, false);
    }
    let cached = start.elapsed();
    assert_eq!(cache.file_lookups, DIRS * FILES);
    eprintln!(
        "{DIRS} 目录 × {FILES} 在用文件：构建候选 {initial:?}，逐目录增量共 {incremental:?}；{REPEATS} 次扫描中缓存查询 {cached:?}，文件查找仍为 {} 次",
        cache.file_lookups
    );
}

#[test]
fn refreshing_processes_does_not_uncompact_small_files() {
    let mut index = Index::new(Os::Linux, Path::new("/fixture"), OTHER);
    let names: Vec<_> = (0..160).map(|i| format!("file{i:03}")).collect();
    let files: Vec<_> = names
        .iter()
        .enumerate()
        .map(|(i, name)| file(name, i as u64 + 1, Category::Other))
        .collect();
    index.fill_dir(DirId::ROOT, &[], &files).unwrap();
    let initial = procs(&[]).snapshot();
    let proc = procs(&["/fixture/file000", "/fixture/file159"]);
    let mut cache = ViewCache::with_snapshot(initial);
    let value = cache
        .tree(
            &index,
            proc.snapshot(),
            Path::new("/fixture"),
            0.0,
            None,
            true,
        )
        .unwrap();
    let children = value["tree"]["ch"].as_array().unwrap();
    assert!(!children.iter().any(|child| child["n"] == "file000"));
    assert_eq!(children[0]["c"], 1);
    assert_eq!(value["totals"][1], 160);
    assert_eq!(value["tree"]["r"], 2);
    assert_eq!(
        children.iter().find(|child| child["t"] == "a").unwrap()["k"],
        10
    );
}

#[cfg(any(unix, windows))]
#[test]
fn windows_distinct_surrogate_paths_keep_their_own_running_sizes() {
    fn name(surrogate: u16) -> std::ffi::OsString {
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStringExt;
            std::ffi::OsString::from_wide(&[u16::from(b'a'), surrogate])
        }
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            // 在 Unix 上注入与 Windows OsStr 相同的 WTF-8 字节，无需真实文件或系统调用。
            std::ffi::OsString::from_vec(vec![
                b'a',
                0xe0 | ((surrogate >> 12) as u8),
                0x80 | (((surrogate >> 6) & 0x3f) as u8),
                0x80 | ((surrogate & 0x3f) as u8),
            ])
        }
    }
    let root = Path::new("C:\\Fixture");
    let (first, second) = (name(0xd800), name(0xd801));
    let mut index = Index::new(Os::Windows, root, OTHER);
    index
        .fill_dir(
            DirId::ROOT,
            &[],
            &[
                NewFile {
                    name: &first,
                    size: 123,
                    category: Category::Other,
                    linked: false,
                },
                NewFile {
                    name: &second,
                    size: 77,
                    category: Category::Cache,
                    linked: false,
                },
            ],
        )
        .unwrap();
    let paths = [root.join(first), root.join(second)];
    assert_ne!(index.find(&paths[0]), index.find(&paths[1]));
    for path in &paths {
        assert!(matches!(index.find(path), Some(Found::File { .. })));
    }
    let proc = ProcInfo::with_os(Os::Windows);
    proc.install(paths.iter().cloned().map(|path| {
        let mut map = ProcessMap::default();
        map.insert(42, Usage::new("editor", Kind::Open));
        (path, map)
    }));
    assert_eq!(proc.snapshot().file_count(), 2);
    let mut cache = ViewCache::with_snapshot(proc.snapshot());
    for finished in [false, false, true, true] {
        let value = cache
            .tree(&index, proc.snapshot(), root, 0.0, None, finished)
            .unwrap();
        assert_eq!(value["totals"], json!([0, 200, 0, 0, 0, 0, 0]));
    }
    let mut map = ProcessMap::default();
    map.insert(42, Usage::new("editor", Kind::Open));
    proc.install([(paths[1].clone(), map)]);
    let value = cache
        .tree(&index, proc.snapshot(), root, 0.0, None, true)
        .unwrap();
    assert_eq!(value["totals"], json!([0, 77, 0, 0, 0, 0, 123]));
}

fn compare_reference(
    cache: &mut ViewCache,
    index: &Index,
    current: &Arc<Snapshot>,
    retained: &Snapshot,
    options: TreeOptions,
    pkgdb: Option<&PkgDb>,
    finished: bool,
) -> Value {
    let expected = super::reference::render(index, DirId::ROOT, current, retained, options, pkgdb);
    let value = cache
        .tree_with_options(
            index,
            Arc::clone(current),
            index.root_path(),
            options,
            pkgdb,
            finished,
        )
        .unwrap();
    assert_eq!(value["tree"], expected);
    if options.max_nodes == TreeOptions::default().max_nodes
        && options.max_depth == TreeOptions::default().max_depth
    {
        let state = if finished { "done" } else { "scanning" };
        let bytes = cache
            .tree_bytes(
                index,
                Arc::clone(current),
                index.root_path(),
                options.frac,
                pkgdb,
                state,
            )
            .unwrap();
        let mut expected_response = value.clone();
        expected_response["root"] = json!(index.root_path().to_string_lossy());
        expected_response["state"] = json!(state);
        assert_eq!(
            serde_json::from_slice::<Value>(&bytes).unwrap(),
            expected_response
        );
        // 不仅字段和值一致，仍保留原 Value 字典序；路径、进程文字的转义也必须相同。
        assert_eq!(bytes, serde_json::to_vec(&expected_response).unwrap());
    }
    value
}

#[test]
fn optimized_render_matches_reference_for_ties_budgets_and_languages() {
    for os in [Os::Linux, Os::Windows] {
        let root = Path::new(if os == Os::Windows {
            "C:\\Fixture"
        } else {
            "/fixture"
        });
        let mut index = Index::new(os, root, OTHER);
        let names: Vec<_> = (0..233).map(|i| format!("file{i:03}")).collect();
        let mut seed = 123456u64;
        let files: Vec<_> = names
            .iter()
            .enumerate()
            .map(|(i, name)| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                file(
                    name,
                    (seed % 11) * 1024,
                    Category::from_index(i % NCLS).unwrap(),
                )
            })
            .collect();
        let ids = index
            .fill_dir(
                DirId::ROOT,
                &[
                    NewDir {
                        name: OsStr::new("a"),
                        class: OTHER,
                        flags: DENIED,
                    },
                    NewDir {
                        name: OsStr::new("b"),
                        class: OTHER,
                        flags: crate::index::MOUNT,
                    },
                ],
                &files,
            )
            .unwrap();
        for id in ids.dirs {
            index.fill_dir(DirId(id), &[], &files).unwrap();
        }
        let initial = ProcInfo::with_os(os);
        let current = ProcInfo::with_os(os);
        let mut before = Vec::new();
        let mut after = Vec::new();
        let mut pkgdb = PkgDb::new(os);
        for dir in index.dirs() {
            for (i, file) in dir.files().enumerate() {
                let path = index.file_path(dir.id(), file.id());
                let mut slot = ProcessMap::default();
                slot.insert(42, Usage::new("editor", Kind::Open));
                if i % 17 == 0 {
                    before.push((path.clone(), slot.clone()));
                }
                if i % 13 == 0 {
                    after.push((path.clone(), slot));
                }
                if i % 19 == 0 {
                    pkgdb.insert_file(&path, "example");
                }
            }
        }
        initial.install(before);
        current.install(after);
        let retained = initial.snapshot();
        let current = current.snapshot();
        let mut cache = ViewCache::with_snapshot(retained.clone());
        for lang in [Lang::Zh, Lang::En] {
            let _language = scope(lang);
            for frac in [0.0, 1e-6, 0.001, 0.05, 1.0] {
                for (max_nodes, max_depth) in [(0, 48), (3, 48), (60_000, 0), (60_000, 48)] {
                    compare_reference(
                        &mut cache,
                        &index,
                        &current,
                        &retained,
                        TreeOptions {
                            frac,
                            max_nodes,
                            max_depth,
                        },
                        Some(&pkgdb),
                        true,
                    );
                }
            }
        }
    }
}

#[test]
fn aggregated_files_do_not_construct_paths_and_finished_projection_is_reused() {
    let mut index = Index::new(Os::Linux, Path::new("/fixture"), OTHER);
    let names: Vec<_> = (0..2000).map(|i| format!("file{i:04}")).collect();
    let files: Vec<_> = names
        .iter()
        .map(|name| file(name, 1, Category::Other))
        .collect();
    index.fill_dir(DirId::ROOT, &[], &files).unwrap();
    let proc = ProcInfo::with_os(Os::Linux);
    proc.install(names.iter().step_by(3).map(|name| {
        let mut slot = ProcessMap::default();
        slot.insert(42, Usage::new("editor", Kind::Open));
        (PathBuf::from(format!("/fixture/{name}")), slot)
    }));
    let snapshot = proc.snapshot();
    let mut cache = ViewCache::with_snapshot(snapshot.clone());
    compare_reference(
        &mut cache,
        &index,
        &snapshot,
        &snapshot,
        TreeOptions {
            frac: 1.0,
            ..TreeOptions::default()
        },
        None,
        true,
    );
    assert_eq!(cache.rendered_file_paths, 0);
    assert_eq!(cache.projections.builds, 1);
    assert_eq!(
        cache
            .retained_files
            .as_ref()
            .unwrap()
            .directory_lookups
            .get(),
        1
    );
    let value = compare_reference(
        &mut cache,
        &index,
        &snapshot,
        &snapshot,
        TreeOptions {
            frac: 0.0,
            ..TreeOptions::default()
        },
        None,
        true,
    );
    assert_eq!(cache.projections.builds, 1);
    assert_eq!(
        cache
            .retained_files
            .as_ref()
            .unwrap()
            .directory_lookups
            .get(),
        1
    );
    assert_eq!(
        cache.rendered_file_paths,
        value["tree"]["ch"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|child| child["t"] == "f")
            .count()
    );
}

#[test]
fn scan_publication_and_delayed_hardlinks_rebuild_file_projection() {
    let mut index = Index::new(Os::Linux, Path::new("/fixture"), OTHER);
    let snapshot = procs(&[]).snapshot();
    let mut cache = ViewCache::with_snapshot(snapshot.clone());
    let options = TreeOptions {
        frac: 0.0,
        ..TreeOptions::default()
    };
    compare_reference(
        &mut cache, &index, &snapshot, &snapshot, options, None, false,
    );
    let names: Vec<_> = (0..200).map(|i| format!("file{i:03}")).collect();
    let mut files: Vec<_> = names
        .iter()
        .map(|name| file(name, 10, Category::Other))
        .collect();
    files[199].linked = true;
    files[199].size = 100_000;
    let ids = index.fill_dir(DirId::ROOT, &[], &files).unwrap();
    let before = compare_reference(
        &mut cache, &index, &snapshot, &snapshot, options, None, false,
    );
    assert_eq!(before["tree"]["ch"][0]["t"], "a");
    index.count_file(DirId::ROOT, FileId(ids.files.end - 1));
    let after = compare_reference(
        &mut cache, &index, &snapshot, &snapshot, options, None, false,
    );
    assert_eq!(after["tree"]["ch"][0]["n"], "file199");
    compare_reference(
        &mut cache, &index, &snapshot, &snapshot, options, None, true,
    );
    let builds = cache.projections.builds;
    let changed = procs(&["/fixture/file199"]).snapshot();
    let refreshed = compare_reference(&mut cache, &index, &changed, &snapshot, options, None, true);
    assert_eq!(refreshed["tree"]["ch"][0]["c"], 1);
    assert_eq!(cache.projections.builds, builds);
}

#[test]
fn retained_unicode_names_use_shared_equivalence_without_index_lookup() {
    for (os, root) in [
        (Os::Linux, "/fixture"),
        (Os::Linux, "/"),
        (Os::Windows, r"C:\Fixture"),
        (Os::Windows, r"C:\"),
        (Os::Windows, r"\\server\share\"),
        (Os::Windows, r"\\server\share"),
    ] {
        let root = Path::new(root);
        let mut index = Index::new(os, root, OTHER);
        let mut names: Vec<_> = (0..150).map(|i| format!("big{i:03}")).collect();
        names.extend(["ΟΣ".into(), "尾空白 ".into()]);
        names.sort_by(|a, b| index.cmp_names(OsStr::new(a), OsStr::new(b)));
        let files: Vec<_> = names
            .iter()
            .map(|name| {
                file(
                    name,
                    if name.starts_with("big") { 100 } else { 1 },
                    Category::Other,
                )
            })
            .collect();
        index.fill_dir(DirId::ROOT, &[], &files).unwrap();
        let proc = ProcInfo::with_os(os);
        let sigma = if os == Os::Windows { "ος" } else { "ΟΣ" };
        proc.install([sigma, "尾空白 "].into_iter().map(|name| {
            let mut slot = ProcessMap::default();
            slot.insert(42, Usage::new("editor", Kind::Open));
            (join(&index, root, OsStr::new(name)), slot)
        }));
        let retained = proc.snapshot();
        let current = ProcInfo::with_os(os).snapshot();
        let mut cache = ViewCache::with_snapshot(retained.clone());
        let value = compare_reference(
            &mut cache,
            &index,
            &current,
            &retained,
            TreeOptions {
                frac: 0.0,
                ..TreeOptions::default()
            },
            None,
            true,
        );
        assert_eq!(value["tree"]["ch"].as_array().unwrap().len(), 152);
    }
}

#[test]
fn breadth_first_paths_preserve_deep_directory_process_counts() {
    let mut index = Index::new(Os::Linux, Path::new("/fixture"), OTHER);
    let mut parent = DirId::ROOT;
    for _ in 0..20 {
        let ids = index
            .fill_dir(
                parent,
                &[NewDir {
                    name: OsStr::new("sub"),
                    class: OTHER,
                    flags: 0,
                }],
                &[file("data", 100, Category::Other)],
            )
            .unwrap();
        parent = DirId(ids.dirs.start);
    }
    let proc = ProcInfo::with_os(Os::Linux);
    proc.install(index.dirs().flat_map(|dir| {
        let index = &index;
        dir.files()
            .map(move |file| {
                let mut slot = ProcessMap::default();
                slot.insert(42, Usage::new("editor", Kind::Open));
                (index.file_path(dir.id(), file.id()), slot)
            })
            .collect::<Vec<_>>()
    }));
    let snapshot = proc.snapshot();
    compare_reference(
        &mut ViewCache::with_snapshot(snapshot.clone()),
        &index,
        &snapshot,
        &snapshot,
        TreeOptions::default(),
        None,
        true,
    );
}

#[test]
fn all_running_large_directory_matches_reference_order() {
    let mut index = Index::new(Os::Linux, Path::new("/fixture"), OTHER);
    let names: Vec<_> = (0..2048).map(|i| format!("file{i:04}")).collect();
    let files: Vec<_> = names
        .iter()
        .enumerate()
        .map(|(i, name)| file(name, (i % 7) as u64, Category::Other))
        .collect();
    index.fill_dir(DirId::ROOT, &[], &files).unwrap();
    let proc = ProcInfo::with_os(Os::Linux);
    proc.install(names.iter().map(|name| {
        let mut slot = ProcessMap::default();
        slot.insert(42, Usage::new("editor", Kind::Open));
        (PathBuf::from(format!("/fixture/{name}")), slot)
    }));
    let snapshot = proc.snapshot();
    let mut cache = ViewCache::with_snapshot(snapshot.clone());
    for frac in [0.0, 0.001, 0.05] {
        compare_reference(
            &mut cache,
            &index,
            &snapshot,
            &snapshot,
            TreeOptions {
                frac,
                ..TreeOptions::default()
            },
            None,
            true,
        );
    }
}

#[test]
#[ignore = "合成目录的完整树生成及 JSON 序列化基准，不代表真实磁盘扫描"]
fn render_and_serialize_benchmark() {
    use std::time::Instant;

    const DIRS: usize = 512;
    const FILES: usize = 1024;
    let mut index = Index::new(Os::Linux, Path::new("/fixture"), OTHER);
    let directory_names: Vec<_> = (0..DIRS).map(|i| format!("dir{i:04}")).collect();
    let file_names: Vec<_> = (0..FILES).map(|i| format!("file{i:04}")).collect();
    let dirs: Vec<_> = directory_names
        .iter()
        .map(|name| NewDir {
            name: OsStr::new(name),
            class: OTHER,
            flags: 0,
        })
        .collect();
    let files: Vec<_> = file_names
        .iter()
        .enumerate()
        .map(|(i, name)| file(name, 1 << (i % 22), Category::Other))
        .collect();
    let ids = index.fill_dir(DirId::ROOT, &dirs, &[]).unwrap();
    for id in ids.dirs {
        index.fill_dir(DirId(id), &[], &files).unwrap();
    }
    let snapshot = procs(&[]).snapshot();
    for frac in [1.67e-5, 1e-6] {
        let options = TreeOptions {
            frac,
            ..TreeOptions::default()
        };
        let start = Instant::now();
        let expected = json!({
            "path": index.root_path().to_string_lossy(),
            "totals": index.root().category_sizes(),
            "tree": super::reference::render(
                &index, DirId::ROOT, &snapshot, &snapshot, options, None,
            ),
        });
        let expected = serde_json::to_vec(&expected).unwrap();
        let reference = start.elapsed();
        let mut cache = ViewCache::with_snapshot(snapshot.clone());
        let start = Instant::now();
        let value = cache
            .tree(
                &index,
                snapshot.clone(),
                index.root_path(),
                frac,
                None,
                true,
            )
            .unwrap();
        let bytes = serde_json::to_vec(&value).unwrap();
        let cold = start.elapsed();
        assert_eq!(bytes, expected);
        let mut warm = Vec::new();
        for _ in 0..3 {
            let start = Instant::now();
            let value = cache
                .tree(
                    &index,
                    snapshot.clone(),
                    index.root_path(),
                    frac,
                    None,
                    true,
                )
                .unwrap();
            let actual = serde_json::to_vec(&value).unwrap();
            warm.push(start.elapsed());
            assert_eq!(actual, expected);
        }
        warm.sort();
        let mut expected_http: Value = serde_json::from_slice(&expected).unwrap();
        expected_http["root"] = json!(index.root_path().to_string_lossy());
        expected_http["state"] = json!("done");
        let expected_http = serde_json::to_vec(&expected_http).unwrap();
        let mut direct = ViewCache::with_snapshot(snapshot.clone());
        let start = Instant::now();
        let bytes = direct
            .tree_bytes(
                &index,
                snapshot.clone(),
                index.root_path(),
                frac,
                None,
                "done",
            )
            .unwrap();
        let direct_cold = start.elapsed();
        assert_eq!(bytes, expected_http);
        let mut direct_warm = Vec::new();
        for _ in 0..3 {
            let start = Instant::now();
            let bytes = direct
                .tree_bytes(
                    &index,
                    snapshot.clone(),
                    index.root_path(),
                    frac,
                    None,
                    "done",
                )
                .unwrap();
            direct_warm.push(start.elapsed());
            assert_eq!(bytes, expected_http);
        }
        direct_warm.sort();
        eprintln!(
            "合成 {DIRS} 目录 × {FILES} 文件，frac={frac}：{} 字节，原 render+序列化 {reference:?}，新首次 {cold:?}，投影缓存后中位 {:?}（包含JSON生成/序列化，不是HTTP字节缓存）",
            expected.len(),
            warm[1],
        );
        eprintln!(
            "完整HTTP direct_bytes {} 字节：首次 {direct_cold:?}，投影缓存中位 {:?}（均包括root/state/totals及JSON序列化）",
            expected_http.len(),
            direct_warm[1],
        );
    }
}

#[test]
fn direct_bytes_preserve_null_process_summary_language_and_scan_state() {
    for (os, root, name) in [
        (Os::Linux, "/fixture", "中文\"\\\n"),
        (Os::Windows, r"C:\Fixture", "中文 空格"),
    ] {
        let root = Path::new(root);
        let mut index = Index::new(os, root, OTHER);
        let ids = index
            .fill_dir(
                DirId::ROOT,
                &[],
                &[
                    file("empty", 10, Category::Other),
                    file(name, 20, Category::Core),
                ],
            )
            .unwrap();
        let proc = ProcInfo::with_os(os);
        let mut slot = ProcessMap::default();
        slot.insert(
            0,
            Usage {
                comm: crate::i18n::Text::new("系统", "system"),
                kind: Kind::Swap,
            },
        );
        // 注入系统可以与宿主系统不同：Path::join 使用宿主分隔符，必须改用索引的路径规则。
        let empty = index.file_path(DirId::ROOT, FileId(ids.files.start));
        let named = index.file_path(DirId::ROOT, FileId(ids.files.start + 1));
        proc.install([
            (empty.clone(), ProcessMap::default()),
            (named.clone(), slot),
        ]);
        let snapshot = proc.snapshot();
        assert!(snapshot.lookup(&empty).is_some());
        assert!(snapshot.lookup(&named).is_some());
        let mut cache = ViewCache::with_snapshot(snapshot.clone());
        for lang in [Lang::Zh, Lang::En] {
            let _language = scope(lang);
            for state in ["scanning", "done", "cancelled", "error"] {
                let mut expected = cache
                    .tree(
                        &index,
                        snapshot.clone(),
                        root,
                        0.0,
                        None,
                        state != "scanning",
                    )
                    .unwrap();
                expected["root"] = json!(root.to_string_lossy());
                expected["state"] = json!(state);
                expected["tree"] = super::reference::render(
                    &index,
                    DirId::ROOT,
                    &snapshot,
                    &snapshot,
                    TreeOptions {
                        frac: 0.0,
                        ..TreeOptions::default()
                    },
                    None,
                );
                let bytes = cache
                    .tree_bytes(&index, snapshot.clone(), root, 0.0, None, state)
                    .unwrap();
                assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap(), expected);
                assert_eq!(bytes, serde_json::to_vec(&expected).unwrap());
                assert_eq!(expected["tree"]["ch"][1]["p"], Value::Null);
                assert!(expected["tree"]["ch"][1].get("p").is_some());
                assert_eq!(
                    expected["tree"]["ch"][0]["p"],
                    if lang == Lang::Zh { "系统" } else { "system" }
                );
            }
        }
    }
}
