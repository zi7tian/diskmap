use super::*;
use crate::classify::ClassifierOptions;
use crate::index::Found;

fn reference(id: u64) -> u64 {
    id | (1 << 48)
}

struct Spec {
    id: u64,
    directory: bool,
    attrs: u32,
    tag: u32,
    apparent: u64,
    allocated: u64,
    names: Vec<(u64, u8, Vec<u16>)>,
}

impl Spec {
    fn file(id: u64, parent: u64, name: &str, apparent: u64, allocated: u64) -> Self {
        Self {
            id,
            directory: false,
            attrs: 0,
            tag: 0,
            apparent,
            allocated,
            names: vec![(reference(parent), 1, name.encode_utf16().collect())],
        }
    }

    fn dir(id: u64, parent: u64, name: &str) -> Self {
        let mut result = Self::file(id, parent, name, 4096, 8192);
        result.directory = true;
        result.attrs = 0x10;
        result
    }

    fn link(mut self, parent: u64, name: &str, namespace: u8) -> Self {
        self.names
            .push((reference(parent), namespace, name.encode_utf16().collect()));
        self
    }

    fn reparse(mut self, tag: u32) -> Self {
        self.attrs |= ATTR_REPARSE;
        self.tag = tag;
        self
    }
}

fn table(specs: Vec<Spec>) -> io::Result<Table> {
    table_with_anomalies(specs, &mut Anomalies::default())
}

fn table_with_anomalies(mut specs: Vec<Spec>, anomalies: &mut Anomalies) -> io::Result<Table> {
    specs.sort_unstable_by_key(|s| s.id);
    let mut table = Table::new();
    for spec in specs {
        let result = table.push(
            EntryMeta {
                reference: reference(spec.id),
                directory: spec.directory,
                attributes: spec.attrs,
                reparse_tag: spec.tag,
                apparent_size: spec.apparent,
                allocated_size: spec.allocated,
            },
            spec.names.iter().map(|(parent, namespace, name)| Name {
                parent_reference: *parent,
                namespace: *namespace,
                name,
            }),
        );
        if let Err(e) = result {
            if e.kind() != io::ErrorKind::InvalidData {
                return Err(e);
            }
            anomalies.note(spec.id);
        }
    }
    Ok(table)
}

fn classifier() -> Classifier {
    Classifier::with_options(None, &[], ClassifierOptions::for_os(Os::Windows))
}

fn build(specs: Vec<Spec>, apparent: bool) -> io::Result<Index> {
    build_with_anomalies(specs, apparent).map(|r| r.0)
}

fn build_with_anomalies(specs: Vec<Spec>, apparent: bool) -> io::Result<(Index, u64)> {
    let mut anomalies = Anomalies::default();
    let index = table_with_anomalies(specs, &mut anomalies)?.build(
        Path::new("C:\\"),
        &classifier(),
        apparent,
        &AtomicBool::new(false),
        &mut anomalies,
        &mut Timings::default(),
    )?;
    Ok((index, anomalies.count()))
}

fn file_size(index: &Index, path: &str) -> Option<(u64, Category)> {
    match index.find(Path::new(path)) {
        Some(Found::File { file, .. }) => {
            Some((index.file(file).size(), index.file(file).category()))
        }
        _ => None,
    }
}

fn push_spec(table: &mut Table, spec: &Spec) -> io::Result<()> {
    table.push(
        EntryMeta {
            reference: reference(spec.id),
            directory: spec.directory,
            attributes: spec.attrs,
            reparse_tag: spec.tag,
            apparent_size: spec.apparent,
            allocated_size: spec.allocated,
        },
        spec.names.iter().map(|(parent, namespace, name)| Name {
            parent_reference: *parent,
            namespace: *namespace,
            name,
        }),
    )
}

fn complete_snapshot(index: &Index) -> Vec<String> {
    let mut rows = Vec::new();
    for dir in index.dirs() {
        rows.push(format!(
            "{:?}",
            (
                dir.id(),
                dir.path(),
                dir.parent(),
                dir.class(),
                dir.flags(),
                dir.size(),
                dir.nfiles(),
                dir.ndirs(),
                dir.category_sizes(),
                dir.subdirs().map(|d| d.id()).collect::<Vec<_>>(),
                dir.files().map(|file| file.id()).collect::<Vec<_>>()
            )
        ));
    }
    // 包含隐藏硬链接的所有存储项；上面的逐目录可见编号同时验证计数选择。
    for id in 0..index.file_count() {
        let file = index.file(FileId(id as u32));
        rows.push(format!(
            "{:?}",
            (file.id(), file.name(), file.size(), file.category())
        ));
    }
    rows
}

#[test]
fn deferred_records_rebuild_the_same_complete_tree_in_any_order() {
    let specs = vec![
        Spec::file(0, 5, "$MFT", 8192, 16384),
        Spec::dir(5, 5, "."),
        Spec::dir(11, 5, "$Extend"),
        Spec::dir(16, 5, "z"),
        Spec::dir(17, 5, "a"),
        Spec::dir(18, 17, "nested"),
        Spec::file(20, 16, "shared.bin", 9, 4096)
            .link(17, "shared.bin", 1)
            .link(18, "SHARED~1.BIN", 2),
        Spec::file(21, 11, "$UsnJrnl", 10, 4096),
        Spec::file(22, 18, "文档😀.txt", 3, 8),
        Spec::file(23, 16, "SHARED.BIN", 99, 8192),
        Spec::file(24, 999, "orphan", 7, 8),
        Spec::file(25, 5, "at-root", 11, 16).link(18, "at-depth", 1),
    ];
    for apparent in [false, true] {
        let finish = |table: Table| {
            let mut anomalies = Anomalies::default();
            let index = table
                .build(
                    Path::new("C:\\"),
                    &classifier(),
                    apparent,
                    &AtomicBool::new(false),
                    &mut anomalies,
                    &mut Timings::default(),
                )
                .unwrap();
            (complete_snapshot(&index), anomalies.count())
        };
        let mut ordered = Table::new();
        for spec in &specs {
            push_spec(&mut ordered, spec).unwrap();
        }
        let expected = finish(ordered);
        for prefix in [0, 1, 3, 8] {
            for rotation in 0..specs.len() - prefix {
                let mut table = Table::new();
                for spec in &specs[..prefix] {
                    push_spec(&mut table, spec).unwrap();
                }
                table.begin_deferred();
                let mut order: Vec<_> = (prefix..specs.len()).collect();
                order.rotate_left(rotation);
                if rotation % 2 == 0 {
                    order.reverse();
                }
                for position in order {
                    push_spec(&mut table, &specs[position]).unwrap();
                }
                assert_eq!(
                    finish(table),
                    expected,
                    "prefix={prefix}, rotation={rotation}"
                );
            }
        }
    }
}

#[test]
fn deferred_reordering_keeps_arenas_and_rejects_duplicate_physical_ids() {
    let mut table = Table::new();
    push_spec(&mut table, &Spec::dir(5, 5, ".")).unwrap();
    push_spec(&mut table, &Spec::file(30, 5, "late", 1, 8)).unwrap();
    assert!(push_spec(&mut table, &Spec::file(20, 5, "early", 2, 8)).is_err());
    table.begin_deferred();
    push_spec(&mut table, &Spec::file(20, 5, "early", 2, 8)).unwrap();
    let pointers = (
        table.records.as_ptr(),
        table.links.as_ptr(),
        table.names.as_ptr(),
    );
    let bytes = table.heap_bytes();
    table.order_deferred(&AtomicBool::new(false)).unwrap();
    assert_eq!(
        pointers,
        (
            table.records.as_ptr(),
            table.links.as_ptr(),
            table.names.as_ptr()
        )
    );
    assert_eq!(table.heap_bytes(), bytes);
    assert_eq!(
        table.records.iter().map(Record::id).collect::<Vec<_>>(),
        [5, 20, 30]
    );
    for (index, record) in table.records.iter().enumerate() {
        for link in &table.links
            [record.first_name as usize..record.first_name as usize + record.name_count as usize]
        {
            assert_eq!(link.record as usize, index);
        }
    }
    table.begin_deferred();
    let mut duplicate = Spec::file(20, 5, "duplicate", 3, 8);
    duplicate.names[0].0 = reference(5);
    push_spec(&mut table, &duplicate).unwrap();
    assert_eq!(
        table
            .order_deferred(&AtomicBool::new(false))
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
#[ignore = "百万条目、多层目录和硬链接的建树阶段基准"]
fn tree_layout_benchmark() {
    const DIRS: u64 = 150_000;
    const FILES: u64 = 850_000;
    let mut table = Table::new();
    let mut push = |id, directory, parent, name: &str, alias: Option<u64>| {
        let units: Vec<_> = name.encode_utf16().collect();
        let mut names = vec![Name {
            parent_reference: reference(parent),
            namespace: 1,
            name: &units,
        }];
        if let Some(parent) = alias {
            names.push(Name {
                parent_reference: reference(parent),
                namespace: 1,
                name: &units,
            });
        }
        table
            .push(
                EntryMeta {
                    reference: reference(id),
                    directory,
                    attributes: if directory { 0x10 } else { 0 },
                    reparse_tag: 0,
                    apparent_size: if directory { 0 } else { 9 },
                    allocated_size: if directory { 0 } else { 16 },
                },
                names,
            )
            .unwrap();
    };
    push(5, true, 5, ".", None);
    for i in 0..DIRS {
        let parent = if i == 0 { 5 } else { 16 + (i - 1) / 8 };
        push(16 + i, true, parent, &format!("dir-{i:07}"), None);
    }
    for i in 0..FILES {
        let parent = 16 + i * 7919 % DIRS;
        let alias = (i % 25 == 0).then_some(16 + (i * 7919 + 17) % DIRS);
        push(
            16 + DIRS + i,
            false,
            parent,
            &format!("file-{i:07}.bin"),
            alias,
        );
    }
    let temporary = table.heap_bytes();
    let classifier = classifier();
    let mut timings = Timings::default();
    let mut anomalies = Anomalies::default();
    let started = Instant::now();
    let index = table
        .build(
            Path::new("C:\\"),
            &classifier,
            false,
            &AtomicBool::new(false),
            &mut anomalies,
            &mut timings,
        )
        .unwrap();
    let total = started.elapsed();
    assert_eq!(index.root().nfiles(), FILES);
    assert_eq!(index.root().ndirs(), DIRS);
    assert_eq!(index.root().size(), FILES * 16);
    assert_eq!(anomalies.count(), 0);
    println!(
        "records={} dirs={DIRS} files={FILES} aliases={} total={:.6}s tree={:.6}s classify={:.6}s hardlinks={:.6}s index={:.6}s temporary={:.2}MiB index_heap={:.2}MiB",
        DIRS + FILES + 1,
        FILES / 25,
        total.as_secs_f64(),
        timings.tree.as_secs_f64(),
        timings.classify.as_secs_f64(),
        timings.hardlinks.as_secs_f64(),
        timings.index.as_secs_f64(),
        temporary as f64 / 1_048_576.0,
        index.heap_bytes() as f64 / 1_048_576.0,
    );
    if let Ok(status) = std::fs::read_to_string("/proc/self/status")
        && let Some(line) = status.lines().find(|line| line.starts_with("VmHWM:"))
    {
        println!("{line}");
    }
}

#[test]
fn builds_sorted_windows_tree_and_deterministic_hard_links() {
    let index = build(
        vec![
            Spec::dir(5, 5, "."),
            Spec::dir(16, 5, "z"),
            Spec::dir(17, 5, "a"),
            Spec::file(20, 16, "shared.bin", 10, 4096)
                .link(17, "shared.bin", 1)
                .link(17, "SHARED~1.BIN", 2),
            Spec::file(21, 5, "中文文件", 20, 4096),
        ],
        false,
    )
    .unwrap();
    assert_eq!(index.root().nfiles(), 2);
    assert_eq!(index.root().ndirs(), 2);
    assert_eq!(index.root().size(), 8192);
    assert!(file_size(&index, "c:/A/SHARED.BIN").is_some());
    assert!(file_size(&index, "C:\\z\\shared.bin").is_none());
    assert!(file_size(&index, "C:\\a\\SHARED~1.BIN").is_none());
    assert_eq!(
        index
            .root()
            .subdirs()
            .map(|d| d.name().to_owned())
            .collect::<Vec<_>>(),
        ["a", "z"]
    );
    assert!(index.dirs().all(|d| d.is_listed()));
}

#[test]
fn metadata_files_are_flattened_and_core_but_directory_data_is_ignored() {
    let index = build(
        vec![
            Spec::file(0, 5, "$MFT", 8192, 16384),
            Spec::dir(5, 5, "."),
            Spec::dir(11, 5, "$Extend"),
            Spec::dir(16, 11, "$RmMetadata"),
            Spec::file(17, 16, "$TxfLog", 123, 4096),
            Spec::file(18, 11, "$UsnJrnl", 100, 8192),
        ],
        false,
    )
    .unwrap();
    assert_eq!(index.root().size(), 28672);
    assert_eq!(index.root().nfiles(), 3);
    for (path, expected) in [
        ("C:\\$MFT", 16384),
        ("C:\\$TxfLog", 4096),
        ("C:\\$UsnJrnl", 8192),
    ] {
        assert_eq!(file_size(&index, path), Some((expected, Category::Core)));
    }
    assert!(file_size(&index, "C:\\$Extend\\$UsnJrnl").is_none());
    let extend = index.find_subdir(DirId::ROOT, "$Extend".as_ref()).unwrap();
    assert_eq!(index.dir(extend).category(), Category::Core);
}

fn reparse_specs() -> Vec<Spec> {
    let mut cloud = Spec::file(20, 5, "cloud.bin", 1000, 4096);
    cloud.attrs = CLOUD_ONLY;
    vec![
        Spec::dir(5, 5, "."),
        Spec::dir(16, 5, "junction").reparse(TAG_MOUNT_POINT),
        Spec::file(17, 16, "not-followed", 100, 200),
        Spec::dir(18, 5, "dir-symlink").reparse(TAG_SYMLINK),
        Spec::file(19, 18, "also-not-followed", 100, 200),
        cloud,
        Spec::dir(21, 5, "cloud-dir").reparse(0x9000_001a),
        Spec::file(22, 21, "present.bin", 7, 4096),
    ]
}

#[test]
fn reparse_and_cloud_rules_match_windows_generic_scanner() {
    let allocated = build(reparse_specs(), false).unwrap();
    assert!(allocated.find(Path::new("C:\\junction")).is_none());
    assert_eq!(file_size(&allocated, "C:\\dir-symlink").unwrap().0, 8192);
    assert_eq!(file_size(&allocated, "C:\\cloud.bin").unwrap().0, 0);
    assert_eq!(
        file_size(&allocated, "C:\\cloud-dir\\present.bin")
            .unwrap()
            .0,
        4096
    );
    assert_eq!(allocated.root().nfiles(), 3);
    assert_eq!(allocated.root().ndirs(), 1);
    assert_eq!(allocated.root().size(), 12288);
    let apparent = build(reparse_specs(), true).unwrap();
    assert_eq!(file_size(&apparent, "C:\\cloud.bin").unwrap().0, 1000);
    assert_eq!(apparent.root().size(), 5103);
}

#[test]
fn malformed_parent_references_and_cycles_are_isolated() {
    let mut stale = Spec::file(16, 5, "bad", 1, 1);
    stale.names[0].0 += 1 << 48;
    for mut specs in [
        vec![Spec::dir(5, 5, "."), stale],
        vec![Spec::dir(5, 5, "."), Spec::file(16, 99, "bad", 1, 1)],
        vec![
            Spec::dir(5, 5, "."),
            Spec::dir(16, 17, "a"),
            Spec::dir(17, 16, "b"),
        ],
        vec![Spec::dir(5, 5, "."), Spec::dir(16, 16, "a")],
        vec![Spec::file(5, 5, ".", 0, 0)],
        vec![
            Spec::dir(5, 5, "."),
            Spec::file(16, 5, "f", 1, 1),
            Spec::file(17, 16, "x", 1, 1),
        ],
    ] {
        specs.push(Spec::file(200, 5, "healthy", 7, 8));
        let (index, anomalies) = build_with_anomalies(specs, false).unwrap();
        assert_eq!(file_size(&index, "C:\\healthy").unwrap().0, 8);
        assert!(anomalies >= 1);
        assert!(index.dirs().all(|dir| dir.is_listed()));
    }
}

#[test]
fn names_are_checked_and_duplicate_namespace_names_are_deduplicated() {
    for name in [".", "..", "", "part/name", "part\\name", "null\0name"] {
        let (index, anomalies) = build_with_anomalies(
            vec![
                Spec::dir(5, 5, "."),
                Spec::file(16, 5, name, 1, 1),
                Spec::file(17, 5, "healthy", 7, 8),
            ],
            false,
        )
        .unwrap();
        assert_eq!(index.root().nfiles(), 1);
        assert_eq!(index.root().size(), 8);
        assert_eq!(anomalies, 1);
    }
    let index = build(
        vec![
            Spec::dir(5, 5, "."),
            Spec::file(16, 5, "same", 1, 1).link(5, "same", 0),
        ],
        false,
    )
    .unwrap();
    assert_eq!(index.root().nfiles(), 1);
    let (index, anomalies) = build_with_anomalies(
        vec![
            Spec::dir(5, 5, "."),
            Spec::file(16, 5, "same", 1, 1),
            Spec::file(17, 5, "SAME", 1, 1),
            Spec::file(18, 5, "healthy", 7, 8),
        ],
        false,
    )
    .unwrap();
    assert_eq!(index.root().nfiles(), 2);
    assert_eq!(index.root().size(), 9);
    assert_eq!(anomalies, 2);
}

#[test]
fn overflowing_sizes_are_isolated_before_index_insertion() {
    let (index, anomalies) = build_with_anomalies(
        vec![
            Spec::dir(5, 5, "."),
            Spec::file(16, 5, "huge", u64::MAX, u64::MAX),
            Spec::file(17, 5, "one", 1, 1),
        ],
        false,
    )
    .unwrap();
    assert_eq!(index.root().nfiles(), 1);
    assert_eq!(index.root().size(), 1);
    assert_eq!(anomalies, 1);
    assert!(file_size(&index, "C:\\huge").is_none());
    let (index, anomalies) = build_with_anomalies(
        vec![
            Spec::dir(5, 5, "."),
            Spec::file(16, 5, "a", i64::MAX as u64, i64::MAX as u64),
            Spec::file(17, 5, "b", i64::MAX as u64, i64::MAX as u64),
            Spec::file(18, 5, "c", 2, 2),
        ],
        false,
    )
    .unwrap();
    assert_eq!(index.root().size(), u64::MAX - 1);
    assert_eq!(index.root().nfiles(), 2);
    assert_eq!(anomalies, 1);
}

#[test]
fn cancellation_discards_staging_and_deep_chains_are_bounded() {
    let table = table(vec![Spec::dir(5, 5, ".")]).unwrap();
    assert_eq!(
        table
            .build(
                Path::new("C:\\"),
                &classifier(),
                false,
                &AtomicBool::new(true),
                &mut Anomalies::default(),
                &mut Timings::default(),
            )
            .unwrap_err()
            .kind(),
        io::ErrorKind::Interrupted
    );
    let mut specs = vec![Spec::dir(5, 5, ".")];
    let mut parent = 5;
    for id in 16..16 + u64::from(MAX_DEPTH) + 1 {
        specs.push(Spec::dir(id, parent, "d"));
        parent = id;
    }
    let (index, anomalies) = build_with_anomalies(specs, false).unwrap();
    assert_eq!(index.root().ndirs(), u64::from(MAX_DEPTH));
    assert_eq!(anomalies, 1);
}

#[test]
fn staging_layout_is_compact_and_counts_records() {
    assert_eq!(std::mem::size_of::<Record>(), 40);
    assert_eq!(std::mem::size_of::<Link>(), 24);
    assert_eq!(std::mem::size_of::<Child>(), 20);
    let table = table(vec![Spec::dir(5, 5, "."), Spec::file(16, 5, "f", 1, 1)]).unwrap();
    assert_eq!(table.record_count(), 2);
    assert!(table.heap_bytes() < 512);
}

#[test]
fn pooled_names_preserve_native_spelling_and_clear_boundaries() {
    let mut names = ChildNames::default();
    for name in ["a", "中文😀", "İΣς", "𐐀", "e\u{301}"] {
        let units: Vec<_> = name.encode_utf16().collect();
        let child = names.push(16, &units).unwrap();
        assert_eq!(names.name(&child), native_name(&units).unwrap());
    }
    let units = [0x61, 0xd800, 0x62];
    let bad = names.push(17, &units);
    #[cfg(windows)]
    assert_eq!(names.name(&bad.unwrap()), native_name(&units).unwrap());
    #[cfg(not(windows))]
    assert!(bad.is_err());
    let good = names.push(18, &[0x61, 0x62, 0x63]).unwrap();
    assert_eq!(names.name(&good), "abc");
    names.clear();
    let good = names.push(19, &[0x64]).unwrap();
    assert_eq!(names.name(&good), "d");
}

#[test]
fn cached_sort_keys_match_index_unicode_and_native_order() {
    let index = Index::new(
        Os::Windows,
        Path::new("C:\\"),
        classifier().root_class(Path::new("C:\\")),
    );
    let mut names = ChildNames::default();
    let mut children = Vec::new();
    for name in [
        "A",
        "a",
        "aa",
        "aA",
        "B",
        "file-123",
        "File-123",
        "0",
        "~",
        "İ",
        "i\u{307}",
        "I",
        "ı",
        "Σ",
        "σ",
        "ς",
        "ΟΣ",
        "ος",
        "οσ",
        "ß",
        "SS",
        "ẞ",
        "ﬀ",
        "FF",
        "Ａ",
        "ａ",
        "Ꭰ",
        "ꭰ",
        "ᾀ",
        "ᾈ",
        "é",
        "e\u{301}",
        "中文",
        "😀",
        "𐐀",
        "𐐨",
        "\u{fffd}",
        "\u{fffd}\u{fffd}\u{fffd}",
        "\u{10ffff}",
    ] {
        let units: Vec<_> = name.encode_utf16().collect();
        let child = names.push(16, &units).unwrap();
        if name.is_ascii() && !name.as_bytes().iter().any(u8::is_ascii_lowercase) {
            assert_eq!(child.key_len, 0);
        }
        children.push(child);
    }
    #[cfg(windows)]
    for units in [&[0xd800][..], &[0xdc00], &[0xd800, 0xd801], &[0x41, 0xd800]] {
        children.push(names.push(17, units).unwrap());
    }
    for left in &children {
        for right in &children {
            assert_eq!(
                names.sort_key(left).cmp(names.sort_key(right)),
                index.cmp_names(names.name(left), names.name(right)),
                "left={:?} right={:?}",
                names.name(left),
                names.name(right),
            );
        }
    }
}

#[test]
fn hardlinks_choose_files_before_descendants_and_sorted_siblings() {
    let index = build(
        vec![
            Spec::dir(5, 5, "."),
            Spec::dir(16, 5, "z"),
            Spec::dir(17, 5, "a"),
            Spec::dir(18, 17, "deep"),
            Spec::file(20, 16, "early-id.bin", 1, 8)
                .link(17, "nested.bin", 1)
                .link(5, "z-root.bin", 1)
                .link(5, "A-root.bin", 1),
            Spec::file(21, 18, "a-deep.bin", 2, 16)
                .link(16, "b.bin", 1)
                .link(17, "z-shallow.bin", 1),
        ],
        false,
    )
    .unwrap();
    assert_eq!(index.root().nfiles(), 2);
    assert_eq!(index.root().size(), 24);
    assert_eq!(file_size(&index, "C:\\A-root.bin").unwrap().0, 8);
    assert_eq!(file_size(&index, "C:\\a\\z-shallow.bin").unwrap().0, 16);
    for hidden in [
        "C:\\z-root.bin",
        "C:\\z\\early-id.bin",
        "C:\\a\\nested.bin",
        "C:\\a\\deep\\a-deep.bin",
        "C:\\z\\b.bin",
    ] {
        assert!(file_size(&index, hidden).is_none(), "{hidden}");
    }
}

#[test]
fn reused_paths_classify_like_full_index_paths() {
    fn check(root: &Path) {
        let mut specs = vec![
            Spec::dir(5, 5, "."),
            Spec::dir(16, 5, "Windows"),
            Spec::dir(17, 5, "Program Files"),
            Spec::dir(18, 16, "System32"),
            Spec::dir(19, 18, "drivers"),
            Spec::dir(20, 5, "Users"),
            Spec::dir(21, 20, "测试😀"),
            Spec::dir(22, 21, "Documents"),
            Spec::dir(23, 21, "AppData"),
            Spec::dir(24, 23, "Local"),
            Spec::dir(25, 24, "Temp"),
            Spec::dir(26, 5, "zz-after"),
            Spec::dir(29, 5, "😀-after"),
            Spec::file(60, 29, "after-native.dll", 1, 8),
        ];
        for id in 16..=26 {
            specs.push(Spec::file(32 + id, id, "item.dll", 1, 8));
        }
        #[cfg(windows)]
        {
            let mut unusual = Spec::dir(27, 5, "unused");
            unusual.names[0].2 = vec![0xd800];
            specs.extend([
                unusual,
                Spec::dir(28, 27, "System32"),
                Spec::file(59, 28, "nested.dll", 1, 8),
            ]);
        }
        let classifier = classifier();
        let mut anomalies = Anomalies::default();
        let index = table(specs)
            .unwrap()
            .build(
                root,
                &classifier,
                false,
                &AtomicBool::new(false),
                &mut anomalies,
                &mut Timings::default(),
            )
            .unwrap();
        assert_eq!(anomalies.count(), 0);
        for dir in index.dirs() {
            let path = dir.path();
            if let Some(parent) = dir.parent() {
                let parent = index.dir(parent);
                let ctx = classifier.enter_dir(parent.class(), &parent.path());
                assert_eq!(
                    dir.class(),
                    classifier.dir_class(&ctx, dir.name(), &path),
                    "{path:?}"
                );
            }
            let ctx = classifier.enter_dir(dir.class(), &path);
            for file in dir.files() {
                assert_eq!(file.category(), classifier.file_class(&ctx, file.name()));
            }
        }
    }
    check(Path::new("C:\\"));
    check(Path::new("C:/"));
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        check(Path::new(&OsString::from_wide(&[67, 58, 92, 0xd800])));
    }
}

#[test]
fn reserved_empty_records_need_no_names_but_live_user_records_do() {
    let mut empty = Spec::file(12, 5, "reserved", 0, 0);
    empty.names.clear();
    let index = build(vec![Spec::dir(5, 5, "."), empty], false).unwrap();
    assert_eq!(index.root().nfiles(), 0);
    let mut unnamed = Spec::file(16, 5, "reserved", 0, 0);
    unnamed.names.clear();
    let (index, anomalies) =
        build_with_anomalies(vec![Spec::dir(5, 5, "."), unnamed], false).unwrap();
    assert_eq!(index.root().nfiles(), 0);
    assert_eq!(anomalies, 1);
}

#[test]
fn ea_attribute_is_not_cloud_recall_in_standard_information() {
    let mut ea = Spec::file(16, 5, "has-ea", 1000, 4096).link(5, "second-link", 1);
    ea.attrs = 0x40000;
    let (index, anomalies) = build_with_anomalies(vec![Spec::dir(5, 5, "."), ea], false).unwrap();
    assert_eq!(index.root().nfiles(), 1);
    assert_eq!(index.root().size(), 4096);
    assert_eq!(file_size(&index, "C:\\has-ea").unwrap().0, 4096);
    assert_eq!(anomalies, 0);
}

#[test]
fn missing_root_and_broken_alias_preserve_healthy_paths() {
    let (index, anomalies) = build_with_anomalies(
        vec![
            Spec::file(0, 5, "$MFT", 8000, 8192),
            Spec::dir(16, 5, "folder"),
            Spec::file(17, 999, "stale-alias", 10, 16).link(16, "valid-alias", 1),
        ],
        false,
    )
    .unwrap();
    assert_eq!(index.root().nfiles(), 2);
    assert_eq!(index.root().ndirs(), 1);
    assert_eq!(index.root().size(), 8208);
    assert_eq!(file_size(&index, "C:\\folder\\valid-alias").unwrap().0, 16);
    assert_eq!(anomalies, 2);
}

#[test]
fn rejected_push_rolls_back_all_names() {
    let mut table = table(vec![Spec::dir(5, 5, ".")]).unwrap();
    let before = (table.records.len(), table.links.len(), table.names.len());
    let good: Vec<u16> = "good".encode_utf16().collect();
    let invalid: Vec<u16> = "bad/name".encode_utf16().collect();
    let result = table.push(
        EntryMeta {
            reference: reference(16),
            directory: false,
            attributes: 0,
            reparse_tag: 0,
            apparent_size: 3,
            allocated_size: 8,
        },
        [&good, &invalid].into_iter().map(|name| Name {
            parent_reference: reference(5),
            namespace: 1,
            name,
        }),
    );
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
    assert_eq!(
        before,
        (table.records.len(), table.links.len(), table.names.len())
    );
    table
        .push(
            EntryMeta {
                reference: reference(17),
                directory: false,
                attributes: 0,
                reparse_tag: 0,
                apparent_size: 3,
                allocated_size: 8,
            },
            [Name {
                parent_reference: reference(5),
                namespace: 1,
                name: &good,
            }],
        )
        .unwrap();
    let index = table
        .build(
            Path::new("C:\\"),
            &classifier(),
            false,
            &AtomicBool::new(false),
            &mut Anomalies::default(),
            &mut Timings::default(),
        )
        .unwrap();
    assert_eq!(index.root().nfiles(), 1);
    assert_eq!(index.root().size(), 8);
}

#[cfg(not(windows))]
#[test]
fn unrepresentable_name_does_not_abort_other_records() {
    let mut bad = Spec::file(16, 5, "invalid", 20, 24);
    bad.names[0].2 = vec![0xd800];
    let (index, anomalies) = build_with_anomalies(
        vec![Spec::dir(5, 5, "."), bad, Spec::file(17, 5, "good", 3, 8)],
        false,
    )
    .unwrap();
    assert_eq!(index.root().nfiles(), 1);
    assert_eq!(index.root().size(), 8);
    assert_eq!(anomalies, 1);
}

#[test]
fn reverse_order_deep_chain_retains_valid_ancestors() {
    let end = 16 + u64::from(MAX_DEPTH);
    let mut specs = vec![
        Spec::dir(5, 5, "."),
        Spec::file(end + 1, 5, "healthy", 7, 8),
    ];
    for id in 16..=end {
        specs.push(Spec::dir(id, if id == end { 5 } else { id + 1 }, "d"));
    }
    let (index, anomalies) = build_with_anomalies(specs, false).unwrap();
    assert_eq!(index.root().ndirs(), u64::from(MAX_DEPTH));
    assert_eq!(index.root().nfiles(), 1);
    assert_eq!(index.root().size(), 8);
    assert_eq!(anomalies, 1);
}

#[test]
fn directory_name_conflict_is_local_and_does_not_alias_subtrees() {
    let (index, anomalies) = build_with_anomalies(
        vec![
            Spec::dir(5, 5, "."),
            Spec::dir(16, 5, "NAME"),
            Spec::dir(17, 5, "name"),
            Spec::file(18, 16, "kept", 3, 8),
            Spec::file(19, 17, "hidden", 5, 8),
            Spec::file(20, 5, "healthy", 7, 8),
        ],
        false,
    )
    .unwrap();
    assert_eq!(index.root().ndirs(), 1);
    assert_eq!(index.root().nfiles(), 2);
    assert!(file_size(&index, "C:\\NAME\\kept").is_some());
    assert!(file_size(&index, "C:\\NAME\\hidden").is_none());
    assert_eq!(anomalies, 3);
}
