use super::*;
use crate::procinfo::{Kind, ProcessMap, Usage};

#[test]
fn full_index_routes_keep_existing_tree_and_agent_projection() {
    let fixture = Fixture::new();
    let root = fixture.0.join("cache");
    let scope = root.join("scope");
    let bucket = scope.join("bucket + 数据");
    let outside = root.join("outside");
    for dir in [
        bucket.join("item-empty"),
        bucket.join("item-pocket"),
        outside.clone(),
    ] {
        fs::create_dir_all(dir).unwrap();
    }
    let write =
        |path: &std::path::Path, size| fs::File::create(path).unwrap().set_len(size).unwrap();
    for i in 0..150 {
        write(&bucket.join(format!("item-big-{i:03}")), 1_000);
    }
    for i in 1..=125 {
        write(&bucket.join(format!("item-small-{i:03}")), i);
    }
    let kept = bucket.join("kept-live");
    let opened_after_scan = bucket.join("item-small-125");
    write(&kept, 3);
    write(&bucket.join("item-pocket/data.bin"), 17);
    write(&scope.join("reference.bin"), 10_000);
    write(&outside.join("large.bin"), 5_000_000);

    let server = fixture.serve("test-token");
    let host = format!("127.0.0.1:{}", server.port);
    let send = |method: &str, path: &str, body: &str, lang: &str| {
        request(&server, method, path, &host, Some("test-token"), body, lang)
    };
    let get = |path: &str| {
        let reply = send("GET", path, "", "en");
        assert_eq!(reply.status, 200, "{path}: {}", reply.json());
        reply.json()
    };
    let install = |paths: &[&PathBuf]| {
        server.app.procs().install(paths.iter().map(|path| {
            let mut map = ProcessMap::default();
            map.insert(42, Usage::new("editor", Kind::Open));
            ((*path).clone(), map)
        }));
    };
    install(&[&kept]);
    let scan = send(
        "POST",
        "/api/scan",
        &json!({"path": root, "apparent": true, "cross_fs": false}).to_string(),
        "en",
    );
    assert_eq!(scan.status, 200);
    server.app.current_scan().unwrap().wait();
    assert_eq!(get("/api/status")["state"], "done");
    install(&[&kept, &opened_after_scan]);
    let scope_param = encode_query(&scope.to_string_lossy());
    let bucket_param = encode_query(&bucket.to_string_lossy());

    let tree_route = format!("/api/tree?path={scope_param}&frac=.001");
    let tree = get(&tree_route);
    let view = &tree["tree"];
    let parent = &view["ch"][0];
    assert_eq!(parent["n"], "bucket + 数据");
    let blocks: Vec<_> = parent["ch"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|node| node["t"] == "a")
        .collect();
    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[0]["k"], 125);
    assert_eq!(blocks[1]["k"], 3);
    let group_path = |view: &Value, parent: &Value, block: &Value, group, frac: &str, offset| {
        format!(
            "/api/aggregate?path={scope_param}&parent={bucket_param}&frac={frac}&view_size={}&parent_files={}&parent_dirs={}&group={group}&size={}&count={}&cls={}&offset={offset}",
            view["s"], parent["nf"], parent["nd"], block["s"], block["k"], block["c"]
        )
    };
    let first_route = group_path(view, parent, blocks[0], 0, ".001", 0);
    let first = get(&first_route);
    let second = get(&group_path(view, parent, blocks[0], 0, ".001", 100));
    assert_eq!(first["kind"], "rest");
    assert_eq!(first["total"], 125);
    assert_eq!(first["next_offset"], 100);
    assert_eq!(second["next_offset"], Value::Null);
    let files = first["items"]
        .as_array()
        .unwrap()
        .iter()
        .chain(second["items"].as_array().unwrap());
    let returned: Vec<_> = files
        .map(|item| {
            assert_eq!(item["type"], "file");
            assert_eq!(item["parent"], bucket.to_string_lossy().as_ref());
            (
                item["name"].as_str().unwrap().to_owned(),
                item["size"].as_u64().unwrap(),
            )
        })
        .collect();
    let expected: Vec<_> = (1..=125)
        .rev()
        .map(|i| (format!("item-small-{i:03}"), i))
        .collect();
    assert_eq!(returned, expected);
    assert_eq!(first["items"][0]["cls"], 1);
    let small = get(&group_path(view, parent, blocks[1], 1, ".001", 0));
    assert_eq!(small["kind"], "small");
    assert_eq!(small["total"], 3);
    let names = |page: &Value| {
        page["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["name"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(names(&small), ["item-pocket", "kept-live", "item-empty"]);
    assert_eq!(small["items"][0]["type"], "dir");
    assert_eq!(small["items"][1]["cls"], 1);

    let wide = get(&format!("/api/tree?path={scope_param}&frac=.05"));
    let view = &wide["tree"];
    let parent = &view["ch"][0];
    let blocks = parent["ch"].as_array().unwrap();
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0]["t"], "a");
    let mut merged = Vec::new();
    for (offset, next) in [(0, json!(100)), (100, json!(200)), (200, Value::Null)] {
        let page = get(&group_path(view, parent, &blocks[0], 0, ".05", offset));
        assert_eq!(page["kind"], "small");
        assert_eq!(page["total"], 278);
        assert_eq!(page["next_offset"], next);
        merged.extend(names(&page));
    }
    let mut expected: Vec<_> = (0..150).map(|i| format!("item-big-{i:03}")).collect();
    expected.extend((18..=125).rev().map(|i| format!("item-small-{i:03}")));
    expected.push("item-pocket".into());
    expected.extend((4..=17).rev().map(|i| format!("item-small-{i:03}")));
    expected.extend(
        [
            "item-small-003",
            "kept-live",
            "item-small-002",
            "item-small-001",
            "item-empty",
        ]
        .map(String::from),
    );
    assert_eq!(merged, expected);

    let tree_zoomed_out = get("/api/tree?frac=.05");
    assert!(
        tree_zoomed_out["tree"]["ch"]
            .as_array()
            .unwrap()
            .iter()
            .all(|node| node["n"] != "scope")
    );
    let top = get("/api/largest");
    assert_eq!(top["files"].as_array().unwrap().len(), 10);
    assert_eq!(top["files"][0]["name"], "large.bin");
    assert_eq!(top["files"][1]["name"], "reference.bin");
    assert_eq!(top["files"][1]["parent"], scope.to_string_lossy().as_ref());
    let scoped = get(&format!("/api/largest?path={scope_param}"));
    assert_eq!(scoped["files"][0]["name"], "reference.bin");
    assert_eq!(scoped["files"][9]["size"], 1_000);

    let results = get("/api/search?q=iTeM");
    assert_eq!(results["total"], 277);
    assert_eq!(results["items"].as_array().unwrap().len(), 200);
    assert_eq!(results["items"][150]["name"], "item-small-125");
    assert_eq!(results["items"][150]["cls"], 1);
    assert_eq!(results["items"][199]["name"], "item-small-076");
    assert_eq!(names(&get("/api/search?q=ItEm-PoCkEt")), ["item-pocket"]);
    assert_eq!(
        get(&format!("/api/search?q=large&path={scope_param}"))["total"],
        0
    );
    assert_eq!(
        get("/api/search?q=BuCkEt+%2B+%E6%95%B0%E6%8D%AE")["items"][0]["type"],
        "dir"
    );
    assert_eq!(get("/api/status")["agent_seen_ago"], Value::Null);

    let error = send("GET", "/api/search", "", "zh");
    assert_eq!(error.status, 400);
    assert_eq!(error.json()["error"], "请输入要搜索的名字");
    let error = send(
        "GET",
        &first_route.replace("count=125", "count=124"),
        "",
        "en",
    );
    assert_eq!(error.status, 404);
    let error = send(
        "GET",
        &first_route.replace("frac=.001", "frac=NaN"),
        "",
        "en",
    );
    assert_eq!(error.status, 400);
    assert_eq!(get(&tree_route), tree);
    let detail = get(&format!(
        "/api/info?path={}",
        encode_query(&opened_after_scan.to_string_lossy())
    ));
    assert_eq!(detail["cls"], 1);
    assert_eq!(detail["size"], 125);
    let legacy = get(&format!("/api/agent/largest?path={bucket_param}&limit=200"));
    assert_eq!(legacy["files"].as_array().unwrap().len(), 152); // 150 个大文件、保留的在用文件和子目录里的 data.bin
}
