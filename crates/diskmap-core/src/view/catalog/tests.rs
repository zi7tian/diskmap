use std::ffi::OsStr;
use std::hint::black_box;
use std::time::Instant;

use crate::classify::ReasonId;
use crate::index::{NewDir, NewFile};
use crate::procinfo::ProcInfo;

use super::*;

fn new_dir(name: &str) -> NewDir<'_> {
    NewDir {
        name: OsStr::new(name),
        class: (Category::Other, ReasonId(0)),
        flags: 0,
    }
}

fn new_file(name: &str, size: u64) -> NewFile<'_> {
    NewFile {
        name: OsStr::new(name),
        size,
        category: Category::Other,
        linked: false,
    }
}

fn group_options(
    tree: &serde_json::Value,
    parent: &serde_json::Value,
    frac: f64,
    ordinal: usize,
) -> GroupOptions {
    let group = parent["ch"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|node| node["t"] == "a")
        .nth(ordinal)
        .unwrap();
    GroupOptions {
        frac,
        ordinal,
        offset: 0,
        view_size: tree["s"].as_f64().unwrap(),
        parent_files: parent["nf"].as_u64().unwrap(),
        parent_dirs: parent["nd"].as_u64().unwrap(),
        size: group["s"].as_f64().unwrap(),
        count: group["k"].as_u64().unwrap(),
        cls: group["c"].as_u64().unwrap() as u8,
    }
}

#[test]
fn equal_aggregates_are_distinct_and_use_the_clicked_views_threshold() {
    let root = Path::new("/fixture");
    let parent = Path::new("/fixture/bucket");
    let frac = 0.0035;
    let processes = ProcInfo::with_os(Os::Linux).snapshot();
    for (dir_size, kinds) in [(75, ["rest", "small"]), (90, ["small", "rest"])] {
        let mut index = Index::new(Os::Linux, root, new_dir("").class);
        let ids = index
            .fill_dir(DirId::ROOT, &[new_dir("bucket"), new_dir("outside")], &[])
            .unwrap();
        let bucket = DirId(ids.dirs.start);
        let names: Vec<_> = (0..150).map(|i| format!("big-{i:03}")).collect();
        let mut files: Vec<_> = names.iter().map(|name| new_file(name, 200)).collect();
        files.extend([
            new_file("rest-a", 75),
            new_file("rest-b", 75),
            NewFile {
                linked: true,
                ..new_file("uncounted", 0)
            },
        ]);
        let ids = index
            .fill_dir(bucket, &[new_dir("a"), new_dir("b")], &files)
            .unwrap();
        for id in ids.dirs.clone() {
            index
                .fill_dir(DirId(id), &[], &[new_file("data", dir_size)])
                .unwrap();
        }
        let mut cache = ViewCache::new();
        let tree = cache
            .tree(&index, processes.clone(), root, frac, None, false)
            .unwrap();
        let tree = &tree["tree"];
        let bucket_node = &tree["ch"][0];
        if dir_size == 75 {
            let groups: Vec<_> = bucket_node["ch"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|node| node["t"] == "a")
                .collect();
            assert_eq!(groups[0], groups[1]);
        }
        // 视图里的其他目录发布后，当前阈值已把两块合并。点击时的阈值仍给出原来的成员。
        index
            .fill_dir(DirId(bucket.0 + 1), &[], &[new_file("large", 100_000)])
            .unwrap();
        for (ordinal, kind) in kinds.into_iter().enumerate() {
            let page = cache
                .group_page(
                    &index,
                    &processes,
                    GroupQuery {
                        path: root,
                        parent,
                        options: group_options(tree, bucket_node, frac, ordinal),
                    },
                    false,
                )
                .unwrap();
            assert_eq!(page.kind, kind);
            assert_eq!(page.total, 2);
            let names: Vec<_> = page.items.iter().map(|i| i.name.as_str()).collect();
            assert_eq!(
                names,
                if kind == "rest" {
                    ["rest-a", "rest-b"]
                } else {
                    ["a", "b"]
                }
            );
        }
        // 硬链接计入时，如果被点块所在目录变化，旧页失效。
        assert!(index.count_file(bucket, FileId(ids.files.end - 1)));
        assert!(
            cache
                .group_page(
                    &index,
                    &processes,
                    GroupQuery {
                        path: root,
                        parent,
                        options: group_options(tree, bucket_node, frac, 0),
                    },
                    true
                )
                .is_none()
        );
    }
}

#[test]
fn search_folds_names_without_expanding_characters_or_matching_parent_paths() {
    for (os, root) in [
        (Os::Linux, "/fixture"),
        (Os::Macos, "/fixture"),
        (Os::Windows, "C:\\Fixture"),
    ] {
        let root = Path::new(root);
        let processes = ProcInfo::with_os(os).snapshot();
        let mut index = Index::new(os, root, new_dir("").class);
        let dirs = index
            .fill_dir(DirId::ROOT, &[new_dir("My-Σ-folder")], &[])
            .unwrap();
        let mut files = [
            new_file("ß.txt", 9),
            new_file("name-ς-End", 11),
            new_file("plain", 5),
            new_file("name-ı", 7),
        ];
        files.sort_by(|a, b| index.cmp_names(a.name, b.name));
        index.fill_dir(DirId(dirs.dirs.start), &[], &files).unwrap();

        let result = search(&index, &processes, root, "σ").unwrap();
        assert_eq!(result.total, 2);
        assert_eq!(
            result
                .items
                .iter()
                .map(|item| item.name.as_str())
                .collect::<Vec<_>>(),
            ["My-Σ-folder", "name-ς-End"]
        );
        assert_eq!(search(&index, &processes, root, "SS").unwrap().total, 0);
        assert_eq!(
            search(&index, &processes, root, "E-I").unwrap().items[0].name,
            "name-ı"
        );
        assert_eq!(
            search(&index, &processes, root, "fixture").unwrap().total,
            0
        );
    }
}

#[test]
fn largest_and_search_only_count_the_indexed_hard_link() {
    let root = Path::new("/fixture");
    let processes = ProcInfo::with_os(Os::Linux).snapshot();
    let mut index = Index::new(Os::Linux, root, new_dir("").class);
    let dirs = index
        .fill_dir(DirId::ROOT, &[new_dir("sub")], &[new_file("ordinary", 1)])
        .unwrap();
    let sub = DirId(dirs.dirs.start);
    let ids = index
        .fill_dir(
            sub,
            &[],
            &[
                NewFile {
                    linked: true,
                    ..new_file("link-a", 10)
                },
                NewFile {
                    linked: true,
                    ..new_file("link-b", 10)
                },
            ],
        )
        .unwrap();
    assert_eq!(largest(&index, &processes, root).unwrap().len(), 1);
    assert_eq!(search(&index, &processes, root, "link").unwrap().total, 0);
    index.count_file(sub, FileId(ids.files.start));
    let files = largest(&index, &processes, root).unwrap();
    assert_eq!(
        files
            .iter()
            .map(|file| file.name.as_str())
            .collect::<Vec<_>>(),
        ["link-a", "ordinary"]
    );
    assert_eq!(
        largest(&index, &processes, &index.dir_path(sub))
            .unwrap()
            .len(),
        1
    );
    assert_eq!(search(&index, &processes, root, "link").unwrap().total, 1);
}

#[test]
#[ignore = "280 万条目性能测量，请使用 release"]
fn full_index_benchmark() {
    let root = Path::new("/synthetic");
    let mut index = Index::new(Os::Linux, root, new_dir("").class);
    let names: Vec<_> = (0..6_500)
        .map(|i| format!("dir-{i:05}"))
        .chain(["wide".into()])
        .collect();
    let dirs: Vec<_> = names.iter().map(|name| new_dir(name)).collect();
    let ids = index.fill_dir(DirId::ROOT, &dirs, &[]).unwrap();
    let wide_id = DirId(ids.dirs.end - 1);
    let names: Vec<_> = (0..200_000)
        .map(|i| {
            let extension = if i % 8 == 0 { "Σdata" } else { "data" };
            format!("f{i:06}.{extension}")
        })
        .collect();
    let files: Vec<_> = names
        .iter()
        .enumerate()
        .map(|(i, name)| new_file(name, 10 + (i as u64 * 29) % 2_000))
        .collect();
    for id in ids.dirs {
        let files = if DirId(id) == wide_id {
            &files[..]
        } else {
            &files[..400]
        };
        index.fill_dir(DirId(id), &[], files).unwrap();
    }
    assert_eq!(index.file_count() + index.dir_count(), 2_806_502);
    let processes = ProcInfo::with_os(Os::Linux).snapshot();
    let wide = index.dir_path(wide_id);
    let frac = 1e-6;
    let mut cache = ViewCache::new();
    let tree = cache
        .tree(&index, processes.clone(), &wide, frac, None, true)
        .unwrap();
    let tree = &tree["tree"];
    for (label, query) in [
        ("search ASCII dense", "DATA"),
        ("search Unicode", "ςdata"),
        ("search miss", "nomatch"),
    ] {
        time(label, || {
            let results = search(&index, &processes, root, query).unwrap();
            assert_eq!(
                results.items.len(),
                if query == "nomatch" { 0 } else { SEARCH_LIMIT }
            );
            black_box(results);
        });
    }
    time("largest", || {
        let results = largest(&index, &processes, root).unwrap();
        assert_eq!(results.len(), LARGEST_LIMIT);
        black_box(results);
    });
    for (label, finished) in [
        ("group 200k files cold projection", false),
        ("group 200k files cached projection", true),
    ] {
        time(label, || {
            let page = cache
                .group_page(
                    &index,
                    &processes,
                    GroupQuery {
                        path: &wide,
                        parent: &wide,
                        options: group_options(tree, tree, frac, 0),
                    },
                    finished,
                )
                .unwrap();
            assert_eq!(
                (page.kind, page.total, page.items.len(), page.next_offset),
                ("rest", 199_850, 100, Some(100))
            );
            black_box(page);
        });
    }
}

fn time(label: &str, mut query: impl FnMut()) {
    query();
    let mut times = [0.; 7];
    for time in &mut times {
        let start = Instant::now();
        query();
        *time = start.elapsed().as_secs_f64() * 1_000.;
    }
    times.sort_unstable_by(f64::total_cmp);
    println!(
        "{label}: median {:.3} ms, range {:.3}–{:.3} ms",
        times[3], times[0], times[6]
    );
}
