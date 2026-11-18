use super::*;
use crate::saved::Store;
use crate::scan::ScanState;

fn serve_saved(fixture: &Fixture) -> ServerHandle {
    let pkgdb = Arc::new(PkgDb::default());
    let classifier = Arc::new(Classifier::new(Some(pkgdb.clone()), &[]));
    let app = App::with_components(pkgdb, classifier, ProcInfo::new())
        .with_saved_store(Store::with_cache_dir(fixture.0.join("cache")));
    serve_with_app(
        ServeOptions {
            token: Some("saved-test".into()),
            register_instance: false,
            settings_path: Some(fixture.0.join("settings.json")),
            ..ServeOptions::default()
        },
        Arc::new(app),
    )
    .unwrap()
}

fn call(server: &ServerHandle, method: &str, path: &str, body: &str, lang: &str) -> Reply {
    request(
        server,
        method,
        path,
        &format!("127.0.0.1:{}", server.port),
        Some("saved-test"),
        body,
        lang,
    )
}

fn wait_saved(server: &ServerHandle) -> Value {
    let start = Instant::now();
    loop {
        let reply = call(server, "GET", "/api/saved", "", "en");
        assert_eq!(reply.status, 200);
        let out = reply.json();
        if let Some(item) = out["items"].as_array().and_then(|items| items.first()) {
            return item.clone();
        }
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "未发布保存结果 / Saved scan was not published"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn saved_http_round_trip_preserves_queries_and_ai_forces_a_fresh_scan() {
    let fixture = Fixture::new();
    let root = fixture.0.join("scanned");
    fs::create_dir_all(root.join("empty")).unwrap();
    let file = root.join("largest.bin");
    fs::write(&file, [1; 4096]).unwrap();
    fs::write(root.join("small.tmp"), [2; 1]).unwrap();
    let directory = fixture.0.join("cache/saved");
    fs::create_dir_all(&directory).unwrap();
    let gate = fs::File::create(directory.join(".lock")).unwrap();
    gate.lock().unwrap();
    let server = serve_saved(&fixture);
    let get = |path: &str| {
        let reply = call(&server, "GET", path, "", "en");
        assert_eq!(reply.status, 200, "{path}: {}", reply.json());
        reply
    };
    let post = |path: &str, body: Value| call(&server, "POST", path, &body.to_string(), "en");
    assert!(get("/api/status").json().get("saved").is_none());
    assert_eq!(
        post(
            "/api/scan",
            json!({"path":root,"apparent":true,"cross_fs":false})
        )
        .status,
        200
    );
    assert_eq!(
        server
            .app
            .current_scan()
            .unwrap()
            .wait_timeout(Duration::from_secs(5)),
        ScanState::Done
    );
    let live_status = get("/api/status").json();
    assert_eq!(live_status["state"], "done");
    assert!(live_status.get("saved").is_none());
    let tree = get("/api/tree?frac=.05").json();
    assert_eq!(tree["tree"]["s"], 4097);
    // 文件锁持有期间，后台保存不能发布；状态和树已经可以读取。
    assert!(fs::read_dir(&directory).unwrap().all(|entry| {
        entry
            .unwrap()
            .path()
            .extension()
            .is_none_or(|ext| ext != "dmap")
    }));
    drop(gate);
    let saved = wait_saved(&server);
    assert_eq!(saved["root"], live_status["root"]);
    assert_eq!(saved["files"], 2);
    assert_eq!(saved["dirs"], 1);
    assert_eq!(saved["bytes"], 4097);
    assert_eq!(saved["method"], "generic");
    assert!(saved["finished_at"].as_f64().unwrap() > 0.0);
    assert!(saved["card"].is_null());
    let id = &saved["id"];
    let node = &tree["tree"];
    let block = node["ch"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["t"] == "a")
        .unwrap();
    let group = format!(
        "/api/aggregate?parent={}&frac=.05&view_size={}&parent_files={}&parent_dirs={}&group=0&size={}&count={}&cls={}",
        encode_query(live_status["root"].as_str().unwrap()),
        node["s"],
        node["nf"],
        node["nd"],
        block["s"],
        block["k"],
        block["c"]
    );
    let info = format!("/api/info?path={}", encode_query(&file.to_string_lossy()));
    let explain = format!(
        "/api/agent/explain?path={}",
        encode_query(&file.to_string_lossy())
    );
    let routes = [
        "/api/tree?frac=.05",
        &info,
        &group,
        "/api/largest",
        "/api/search?q=largest",
        "/api/agent/summary",
        "/api/agent/list",
        &explain,
    ];
    let before = routes.map(|route| get(route).body);
    let before_agent = get("/api/agent/status").json();
    assert!(before_agent.get("saved").is_none());

    let reply = post("/api/saved/open", json!({"id": id}));
    assert_eq!(reply.status, 200, "{}", reply.json());
    let marker = json!({"finished_at": saved["finished_at"]});
    assert_eq!(reply.json()["saved"], marker);
    assert!(server.app.current_scan().unwrap().is_saved());
    assert_eq!(get("/api/status").json()["saved"], marker);
    let mut agent = get("/api/agent/status").json();
    assert_eq!(agent.as_object_mut().unwrap().remove("saved"), Some(marker));
    assert_eq!(agent, before_agent);
    assert_eq!(routes.map(|route| get(route).body), before);

    assert_eq!(
        post("/api/saved/delete", json!({"id": id})).json(),
        json!({"ok":true})
    );
    assert_eq!(post("/api/cancel", json!({})).json()["state"], "done");
    assert_eq!(get("/api/saved").json(), json!({"items": []}));
    assert_eq!(get("/api/tree?frac=.05").json(), tree);
    assert_eq!(post("/api/saved/open", json!({"id": id})).status, 404);

    fs::write(&file, [1; 8192]).unwrap();
    let reply = post(
        "/api/agent/scan",
        json!({"path":root, "apparent":true, "same_fs":true}),
    );
    assert_eq!(reply.status, 200);
    assert_eq!(reply.json()["reused"], false);
    assert_eq!(
        server
            .app
            .current_scan()
            .unwrap()
            .wait_timeout(Duration::from_secs(5)),
        ScanState::Done
    );
    assert!(get("/api/status").json().get("saved").is_none());
    assert!(get("/api/agent/status").json().get("saved").is_none());
    assert_eq!(wait_saved(&server)["bytes"], 8193);
}

#[test]
fn invalid_and_damaged_saved_results_return_localized_errors() {
    let fixture = Fixture::new();
    let root = fixture.0.join("scanned");
    fs::create_dir(&root).unwrap();
    let server = serve_saved(&fixture);
    server
        .app
        .start_scan(root.to_str().unwrap(), true, false)
        .unwrap();
    let saved = wait_saved(&server);
    let id = saved["id"].as_str().unwrap();
    let path = fixture.0.join(format!("cache/saved/scan-{id}.dmap"));
    let mut data = fs::read(&path).unwrap();
    *data.last_mut().unwrap() ^= 1;
    fs::write(&path, &data).unwrap();
    let body = json!({"id":id}).to_string();
    let bad = call(&server, "POST", "/api/saved/open", &body, "en");
    assert_eq!(bad.status, 404);
    assert_eq!(
        bad.json()["error"],
        "The saved scan is damaged or no longer available"
    );
    assert!(!path.exists());
    let gone = call(&server, "POST", "/api/saved/open", &body, "zh");
    assert_eq!(gone.status, 404);
    assert_eq!(gone.json()["error"], "保存的扫描结果已损坏或被移除");
    assert_eq!(
        call(
            &server,
            "POST",
            "/api/saved/delete",
            r#"{"id":"../unexpected"}"#,
            "en"
        )
        .status,
        400
    );
    assert!(!server.app.current_scan().unwrap().is_saved());
}
