//! 用发布版和本机 HTTP 测量后台保存、打开和返回首棵树。

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Instant;

use super::*;
use crate::app::App;
use crate::index::NewDir;
use crate::server::{ServeOptions, ServerHandle, serve_with_app};
use serde_json::{Value, json};

#[test]
#[ignore = "发布版存盘测量，约 280 万项 / Release save benchmark, about 2.8 million entries"]
fn saved_benchmark_synthetic() {
    const GROUPS: u32 = 100;
    const DIRS_PER_GROUP: u32 = 1000;
    const FILES_PER_DIR: usize = 27;
    let start = Instant::now();
    let os = Os::Windows;
    let class = (Category::User, ReasonId(0));
    let mut index = Index::new(os, Path::new("C:\\"), class);
    let groups: Vec<_> = (0..GROUPS).map(|g| format!("project-{g:03}")).collect();
    let directory = |name| NewDir {
        name,
        class,
        flags: 0,
    };
    let groups = index
        .fill_dir(
            DirId::ROOT,
            &groups
                .iter()
                .map(|name| directory(name.as_ref()))
                .collect::<Vec<_>>(),
            &[],
        )
        .unwrap()
        .dirs;
    let names: Vec<_> = (0..DIRS_PER_GROUP)
        .map(|n| format!("folder-{n:04}"))
        .collect();
    let directories: Vec<_> = names.iter().map(|name| directory(name.as_ref())).collect();
    let extensions = ["bin", "exe", "sys", "rs", "tmp", "jpg", "数据库"];
    let mut seed = 0x2026_1118u32;
    for group in groups {
        let dirs = index
            .fill_dir(DirId(group), &directories, &[])
            .unwrap()
            .dirs;
        for dir in dirs {
            let names: [String; FILES_PER_DIR] = std::array::from_fn(|i| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let kind = extensions[(seed >> 16) as usize % extensions.len()];
                format!("file-{i:02}-component-{seed:08x}.{kind}")
            });
            let files: [NewFile<'_>; FILES_PER_DIR] = std::array::from_fn(|i| NewFile {
                name: names[i].as_ref(),
                size: u64::from(seed.rotate_right(i as u32) & 0x1ff) * 4096,
                category: Category::ALL[i % Category::ALL.len()],
                linked: i == 17 && dir % 10 == 0,
            });
            index.fill_dir(DirId(dir), &[], &files).unwrap();
        }
    }
    index.shrink_to_fit();
    let progress = progress(&index, SystemTime::now());
    let scan = Scan::from_saved(index, progress, classifier(os)).unwrap();
    assert_eq!(
        scan.index().dir_count() + scan.index().file_count(),
        2_800_101
    );
    measure("synthetic", &scan, start.elapsed());
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "只读扫描 Linux 根目录 / Read-only scan of the Linux root"]
fn saved_benchmark_linux_root() {
    let scan = Scan::start(ScanOptions::new("/"), Arc::new(Classifier::new(None, &[])));
    assert_eq!(scan.wait(), ScanState::Done);
    measure("linux-root", &scan, scan.progress().elapsed);
}

fn measure(dataset: &str, scan: &Scan, build: Duration) {
    let temp = Scratch::new();
    let (dirs, files) = {
        let index = scan.index();
        (index.dir_count(), index.file_count())
    };
    println!(
        "存盘测量 / Save benchmark {dataset}: dirs={dirs} files={files} build={:.3}s",
        build.as_secs_f64()
    );
    let store = Store::with_cache_dir(&temp.0);
    let write = std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let start = Instant::now();
                store.save(scan).unwrap();
                start.elapsed()
            })
            .join()
            .unwrap()
    });
    let listed = store.list().unwrap();
    let id = &listed[0].id;
    let size = fs::metadata(store.path(id).unwrap()).unwrap().len();
    let app = App::with_components(
        Arc::new(PkgDb::default()),
        scan.classifier().clone(),
        ProcInfo::new(),
    )
    .with_saved_store(store);
    let server = serve_with_app(
        ServeOptions {
            register_instance: false,
            settings_path: Some(temp.0.join("settings.json")),
            ..ServeOptions::default()
        },
        Arc::new(app),
    )
    .unwrap();

    let request = json!({"id": id}).to_string();
    let start = Instant::now();
    let status: Value =
        serde_json::from_slice(&http(&server, "/api/saved/open", Some(&request))).unwrap();
    assert!(status["saved"]["finished_at"].is_number());
    let open = start.elapsed();
    let tree = http(&server, "/api/tree?frac=.0000167", None);
    let total = start.elapsed();

    let restored = server.app.current_scan().unwrap();
    assert_eq!(restored.progress(), scan.progress());
    let index = restored.index();
    assert_eq!((index.dir_count(), index.file_count()), (dirs, files));
    let tree: Value = serde_json::from_slice(&tree).unwrap();
    assert_eq!(tree["state"], "done");
    assert_eq!(tree["tree"]["s"], status["bytes"]);
    println!(
        "{dataset} LZ4: bytes={size} save={:.3}ms open={:.3}ms open-to-tree={:.3}ms",
        write.as_secs_f64() * 1000.0,
        open.as_secs_f64() * 1000.0,
        total.as_secs_f64() * 1000.0
    );
}

fn http(server: &ServerHandle, path: &str, body: Option<&str>) -> Vec<u8> {
    let mut stream = TcpStream::connect(("127.0.0.1", server.port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(15)))
        .unwrap();
    let method = if body.is_some() { "POST" } else { "GET" };
    let body = body.unwrap_or_default();
    write!(stream,
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nX-DiskMap-Token: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        server.port, server.token, body.len()
    ).unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).unwrap();
    assert!(response.starts_with(b"HTTP/1.1 200"));
    let head = response
        .windows(4)
        .position(|part| part == b"\r\n\r\n")
        .unwrap();
    response.drain(..head + 4);
    response
}
