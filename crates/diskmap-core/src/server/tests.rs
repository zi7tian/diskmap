//! HTTP 安全与实例登记的隔离测试；所有可写路径均显式指向临时目录。

use super::*;
use crate::classify::Classifier;
use crate::pkgdb::PkgDb;
use crate::procinfo::ProcInfo;
use std::fs;
use std::io::Write;
use std::net::TcpStream;
use std::time::Instant;

mod catalog;
#[path = "../../tests/support/child.rs"]
mod child;
mod saved;

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "diskmap-t05-http-{}-{}",
            std::process::id(),
            random_token().unwrap()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn serve(&self, token: &str) -> ServerHandle {
        let pkgdb = Arc::new(PkgDb::default());
        let classifier = Arc::new(Classifier::new(Some(pkgdb.clone()), &[]));
        let app = Arc::new(App::with_components(pkgdb, classifier, ProcInfo::new()));
        serve_with_app(
            ServeOptions {
                token: Some(token.into()),
                instance_path: Some(self.0.join("instance.json")),
                settings_path: Some(self.0.join("settings.json")),
                ..ServeOptions::default()
            },
            app,
        )
        .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(target_os = "linux")]
#[test]
fn appimage_executable_uses_absolute_existing_file() {
    let path = PathBuf::from("/opt/DiskMap.AppImage");
    assert_eq!(appimage_executable(Some(path.clone()), true), Some(path));
}

#[cfg(target_os = "linux")]
#[test]
fn appimage_executable_without_environment_value_falls_back() {
    assert_eq!(appimage_executable(None, false), None);
}

#[cfg(target_os = "linux")]
#[test]
fn appimage_executable_rejects_relative_path() {
    assert_eq!(
        appimage_executable(Some(PathBuf::from("DiskMap.AppImage")), true),
        None
    );
}

#[cfg(target_os = "linux")]
#[test]
fn appimage_executable_rejects_missing_file() {
    assert_eq!(
        appimage_executable(Some(PathBuf::from("/opt/missing.AppImage")), false),
        None
    );
}

struct Reply {
    status: u16,
    headers: String,
    body: Vec<u8>,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap()
    }
}

fn request(
    server: &ServerHandle,
    method: &str,
    path: &str,
    host: &str,
    token: Option<&str>,
    body: &str,
    language: &str,
) -> Reply {
    let mut socket = TcpStream::connect(("127.0.0.1", server.port)).unwrap();
    socket
        .set_read_timeout(Some(
            crate::procinfo::DEFAULT_BUDGET + Duration::from_secs(3),
        ))
        .unwrap();
    let token = token.map_or_else(String::new, |token| format!("X-DiskMap-Token: {token}\r\n"));
    write!(socket, "{method} {path} HTTP/1.1\r\nHost: {host}\r\n{token}X-DiskMap-Lang: {language}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
    let mut data = Vec::new();
    socket.read_to_end(&mut data).unwrap();
    let split = data
        .windows(4)
        .position(|bytes| bytes == b"\r\n\r\n")
        .unwrap();
    let headers = String::from_utf8(data[..split].to_vec()).unwrap();
    let status = headers.split_whitespace().nth(1).unwrap().parse().unwrap();
    Reply {
        status,
        headers,
        body: data[split + 4..].to_vec(),
    }
}

#[test]
fn every_api_requires_host_and_token() {
    let fixture = Fixture::new();
    let server = fixture.serve("test-token");
    let host = format!("127.0.0.1:{}", server.port);
    for (method, path) in [
        ("GET", "/api/meta"),
        ("GET", "/api/status"),
        ("GET", "/api/tree"),
        ("GET", "/api/info"),
        ("GET", "/api/aggregate"),
        ("GET", "/api/largest"),
        ("GET", "/api/search"),
        ("GET", "/api/saved"),
        ("POST", "/api/scan"),
        ("POST", "/api/saved/open"),
        ("POST", "/api/saved/delete"),
        ("POST", "/api/cancel"),
        ("POST", "/api/refresh-procs"),
        ("POST", "/api/reveal"),
        ("POST", "/api/settings"),
        ("POST", "/api/ui-state"),
        ("GET", "/api/agent/status"),
        ("GET", "/api/agent/summary"),
        ("GET", "/api/agent/list"),
        ("GET", "/api/agent/explain"),
        ("GET", "/api/agent/cleanup"),
        ("GET", "/api/agent/largest"),
        ("GET", "/api/agent/view"),
        ("POST", "/api/agent/scan"),
        ("POST", "/api/agent/show"),
        ("GET", "/api/unknown"),
        ("POST", "/unknown"),
    ] {
        let bad_host = request(
            &server,
            method,
            path,
            "evil.example",
            Some("test-token"),
            "{}",
            "en",
        );
        assert_eq!(bad_host.status, 403, "{method} {path}");
        assert_eq!(bad_host.json(), json!({"error": "bad host"}));
        let bad_token = request(&server, method, path, &host, None, "{}", "en");
        assert_eq!(bad_token.status, 403, "{method} {path}");
        assert_eq!(bad_token.json(), json!({"error": "bad token"}));
    }
    for host in [
        &host,
        &format!("localhost:{}", server.port),
        &format!("[::1]:{}", server.port),
    ] {
        let response = request(
            &server,
            "GET",
            "/api/status",
            host,
            Some("test-token"),
            "",
            "en",
        );
        assert_eq!(response.status, 200);
        assert_eq!(response.json()["state"], "idle");
        assert!(response.headers.contains("Cache-Control: no-store"));
        assert!(response.headers.contains("X-Content-Type-Options: nosniff"));
    }
}

#[test]
fn homepage_static_and_beacon_match_legacy_exceptions() {
    let fixture = Fixture::new();
    let server = fixture.serve("test-token");
    let host = format!("127.0.0.1:{}", server.port);
    let forbidden = request(&server, "GET", "/", &host, None, "", "en");
    assert_eq!(forbidden.status, 403);
    assert_eq!(forbidden.body, BAD_LINK.as_bytes());
    let homepage = request(&server, "GET", "/?t=test-token", &host, None, "", "en");
    assert_eq!(homepage.status, 200);
    let html = String::from_utf8(homepage.body).unwrap();
    assert!(html.contains("<html lang=\"en\" data-lang=\"en\">"));
    assert!(html.contains("window.DISKMAP_TOKEN = \"test-token\""));
    assert!(!html.contains("__DISKMAP_LANG__"));
    assert!(homepage.headers.contains("Referrer-Policy: no-referrer"));
    let wrong_host = request(
        &server,
        "GET",
        "/?t=test-token",
        "evil.example",
        None,
        "",
        "en",
    );
    assert_eq!(wrong_host.json(), json!({"error": "bad host"}));
    for path in ["/static/app.js", "/static/../app.js", "/static/a/b/app.js"] {
        let static_file = request(&server, "GET", path, "evil.example", None, "", "en");
        assert_eq!(static_file.status, 200);
        assert_eq!(static_file.body, include_bytes!("../../../../web/app.js"));
    }
    for path in [
        "/static/../../Cargo.toml",
        "/static/%2e%2e%2fCargo.toml",
        "/static/README.md",
        "/static/app.js%00",
    ] {
        let forbidden = request(&server, "GET", path, "evil.example", None, "", "en");
        assert_eq!(forbidden.status, 404);
        assert_eq!(forbidden.json(), json!({"error": "not found"}));
    }
    let bye = request(
        &server,
        "POST",
        "/api/bye?t=test-token",
        &host,
        None,
        "",
        "en",
    );
    assert_eq!(bye.status, 204);
    assert!(bye.body.is_empty());
    let bye_bad_host = request(
        &server,
        "POST",
        "/api/bye?t=test-token",
        "evil.example",
        None,
        "",
        "en",
    );
    assert_eq!(bye_bad_host.status, 403);
    assert_eq!(bye_bad_host.json(), json!({"error": "bad token"}));
}

#[test]
fn request_language_errors_and_ui_state_are_preserved() {
    let fixture = Fixture::new();
    let server = fixture.serve("test-token");
    let host = format!("127.0.0.1:{}", server.port);
    for (lang, text) in [
        ("en", "No scan results for this path"),
        ("zh", "没有这个路径的扫描结果"),
    ] {
        let reply = request(
            &server,
            "GET",
            "/api/tree",
            &host,
            Some("test-token"),
            "",
            lang,
        );
        assert_eq!(reply.status, 404);
        assert_eq!(reply.json(), json!({"error": text}));
    }
    let invalid = request(
        &server,
        "GET",
        "/api/tree?frac=bad",
        &host,
        Some("test-token"),
        "",
        "en",
    );
    assert_eq!(invalid.status, 400);
    assert_eq!(
        invalid.json(),
        json!({"error": "could not convert string to float: 'bad'"})
    );
    let settings = request(
        &server,
        "POST",
        "/api/settings",
        &host,
        Some("test-token"),
        "{\"lang\":null}",
        "zh",
    );
    assert_eq!(settings.status, 400);
    assert_eq!(
        settings.json(),
        json!({"error": "unsupported language: None"})
    );
    assert!(!fixture.0.join("settings.json").exists());
    let ui = request(
        &server,
        "POST",
        "/api/ui-state",
        &host,
        Some("test-token"),
        "{\"view\":\"/tmp\",\"selected\":null,\"pinned\":true}",
        "en",
    );
    assert_eq!(ui.json(), json!({"ok": true}));
    assert_eq!(server.app.ui_state().unwrap()["view"], "/tmp");
    let malformed = request(
        &server,
        "POST",
        "/api/ui-state",
        &host,
        Some("test-token"),
        "not-json",
        "en",
    );
    assert_eq!(malformed.status, 200);
    assert!(server.app.ui_state().unwrap()["view"].is_null());
}

#[test]
fn generated_tokens_and_escaped_custom_tokens() {
    let a = random_token().unwrap();
    let b = random_token().unwrap();
    assert_eq!(a.len(), 24);
    assert!(
        a.bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
    );
    assert_ne!(a, b);
    let fixture = Fixture::new();
    let token = "x\"</script> &中文";
    let server = fixture.serve(token);
    let host = format!("127.0.0.1:{}", server.port);
    let path = format!("/?t={}", encode_query(token));
    let homepage = request(&server, "GET", &path, &host, None, "", "en");
    assert_eq!(homepage.status, 200);
    let html = String::from_utf8(homepage.body).unwrap();
    assert!(html.contains("x\\\"\\u003c/script\\u003e \\u0026中文"));
    assert!(!html.contains("x\"</script>"));
    assert!(server.url.ends_with(&path[1..]));
}

#[test]
fn slow_body_does_not_block_other_requests_or_shutdown() {
    let fixture = Fixture::new();
    let mut server = fixture.serve("test-token");
    let host = format!("127.0.0.1:{}", server.port);
    let mut slow = TcpStream::connect(("127.0.0.1", server.port)).unwrap();
    write!(slow, "POST /api/ui-state HTTP/1.1\r\nHost: {host}\r\nX-DiskMap-Token: test-token\r\nContent-Length: 100000\r\nConnection: close\r\n\r\n{{").unwrap();
    let reply = request(
        &server,
        "GET",
        "/api/status",
        &host,
        Some("test-token"),
        "",
        "en",
    );
    assert_eq!(reply.status, 200);
    let started = Instant::now();
    server.shutdown();
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(!fixture.0.join("instance.json").exists());
    drop(slow);
}

#[test]
fn instance_permissions_atomic_replacement_and_owner_check() {
    let fixture = Fixture::new();
    let path = fixture.0.join("cache/instance.json");
    instance::write_at(&path, 4321, "one", "test").unwrap();
    let data = instance::read_at(&path).unwrap();
    assert_eq!(data["port"], 4321);
    assert_eq!(data["token"], "one");
    assert_eq!(data["pid"], std::process::id());
    assert!(data["started"].as_f64().unwrap() > 0.0);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let reading = Arc::new(AtomicBool::new(true));
    let keep_reading = Arc::clone(&reading);
    let reader_path = path.clone();
    let reader = thread::spawn(move || {
        while keep_reading.load(Ordering::Acquire) {
            assert!(instance::read_at(&reader_path).is_some());
        }
    });
    for port in 4322..4332 {
        instance::write_at(&path, port, "two", "test").unwrap();
    }
    reading.store(false, Ordering::Release);
    reader.join().unwrap();
    let mut other = instance::read_at(&path).unwrap();
    other["pid"] = json!(u64::from(std::process::id()) + 1);
    fs::write(&path, serde_json::to_vec(&other).unwrap()).unwrap();
    instance::remove_at(&path);
    assert!(path.exists());
    instance::write_at(&path, 1234, "ours", "test").unwrap();
    instance::remove_at(&path);
    assert!(!path.exists());
    for value in [
        json!(null),
        json!([]),
        json!({"port": "1", "token": "a"}),
        json!({"port": 1, "token": null}),
    ] {
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(instance::read_at(&path).is_none());
    }
}

#[test]
fn query_parsing_uses_first_nonempty_value() {
    let (path, query) =
        split_url("/api/info?path=&path=%2Ftmp%2F%E4%B8%AD+%2B&path=ignored&bare#fragment");
    assert_eq!(path, "/api/info");
    assert_eq!(query.get("path").unwrap(), "/tmp/中 +");
    assert!(!query.contains_key("bare"));
    assert_eq!(decode_query("%zz%a%"), "%zz%a%");
    assert!(token_eq("token", "token"));
    assert!(!token_eq("token", "tokem"));
    assert!(!token_eq("token", ""));
    assert_eq!(
        python_str(&json!({"key": ["x", true, null, {"n": false}]})),
        "{'key': ['x', True, None, {'n': False}]}"
    );
}

fn raw_request(server: &ServerHandle, text: &str, half_close: bool) -> Reply {
    let mut socket = TcpStream::connect(("127.0.0.1", server.port)).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let _ = socket.write_all(text.as_bytes());
    if half_close {
        let _ = socket.shutdown(std::net::Shutdown::Write);
    }
    let mut data = Vec::new();
    // 提前回复后限时丢弃剩余正文；不能再忽略 RST，否则会漏掉 Windows 的回复丢失。
    socket
        .read_to_end(&mut data)
        .expect("complete HTTP response without connection reset");
    let split = data
        .windows(4)
        .position(|bytes| bytes == b"\r\n\r\n")
        .expect("complete HTTP response");
    let headers = String::from_utf8(data[..split].to_vec()).unwrap();
    let status = headers.split_whitespace().nth(1).unwrap().parse().unwrap();
    Reply {
        status,
        headers,
        body: data[split + 4..].to_vec(),
    }
}

#[test]
fn oversized_and_truncated_bodies_never_allocate_the_declared_length() {
    let fixture = Fixture::new();
    let server = fixture.serve("test-token");
    let host = format!("127.0.0.1:{}", server.port);
    for length in [
        "1000001",
        "18446744073709551615",
        "99999999999999999999999999999999999",
    ] {
        let raw = format!(
            "POST /api/ui-state HTTP/1.1\r\nHost: {host}\r\nX-DiskMap-Token: test-token\r\nContent-Length: {length}\r\n\r\n"
        );
        let reply = raw_request(&server, &raw, false);
        assert_eq!(reply.status, 200, "length {length}");
        assert_eq!(reply.json(), json!({"ok": true}));
        let raw =
            format!("POST /api/scan HTTP/1.1\r\nHost: {host}\r\nContent-Length: {length}\r\n\r\n");
        let reply = raw_request(&server, &raw, false);
        assert_eq!(reply.status, 403);
        assert_eq!(reply.json(), json!({"error": "bad token"}));
    }
    let oversized = format!(
        "POST /api/ui-state HTTP/1.1\r\nHost: {host}\r\nX-DiskMap-Token: test-token\r\nContent-Length: 1000001\r\n\r\n{}",
        "x".repeat(1_000_001)
    );
    let reply = raw_request(&server, &oversized, false);
    assert_eq!(reply.status, 200);
    assert_eq!(reply.json(), json!({"ok": true}));
    let raw = format!(
        "POST /api/ui-state HTTP/1.1\r\nHost: {host}\r\nX-DiskMap-Token: test-token\r\nContent-Length: 100000\r\n\r\n{{"
    );
    let reply = raw_request(&server, &raw, true);
    assert_eq!(reply.status, 200);
    assert_eq!(reply.json(), json!({"ok": true}));
    assert_eq!(
        request(
            &server,
            "GET",
            "/api/status",
            &host,
            Some("test-token"),
            "",
            "en"
        )
        .status,
        200
    );
}

#[test]
fn oversized_headers_and_ambiguous_body_lengths_are_rejected() {
    let fixture = Fixture::new();
    let server = fixture.serve("test-token");
    let host = format!("127.0.0.1:{}", server.port);
    for headers in [
        "Content-Length: 1\r\nContent-Length: 2\r\n",
        "Content-Length: 1\r\nTransfer-Encoding: chunked\r\n",
    ] {
        let raw = format!(
            "POST /api/ui-state HTTP/1.1\r\nHost: {host}\r\nX-DiskMap-Token: test-token\r\n{headers}\r\n"
        );
        let reply = raw_request(&server, &raw, false);
        assert_eq!(reply.status, 400);
        assert_eq!(reply.json(), json!({"error": "ambiguous body length"}));
    }
    let raw = format!(
        "GET / HTTP/1.1\r\nHost: {host}\r\nX-Long: {}",
        "a".repeat(70_000)
    );
    let reply = raw_request(&server, &raw, false);
    assert_eq!(reply.status, 431);
    assert_eq!(reply.json(), json!({"error": "headers too large"}));
}

#[test]
fn authentication_preserves_trailing_header_whitespace() {
    let fixture = Fixture::new();
    let server = fixture.serve("test-token");
    let host = format!("127.0.0.1:{}", server.port);
    for suffix in [" ", "\t"] {
        let reply = request(
            &server,
            "GET",
            "/api/status",
            &format!("{host}{suffix}"),
            Some("test-token"),
            "",
            "en",
        );
        assert_eq!(reply.status, 403);
        assert_eq!(reply.json(), json!({"error": "bad host"}));
        let reply = request(
            &server,
            "GET",
            "/api/status",
            &host,
            Some(&format!("test-token{suffix}")),
            "",
            "en",
        );
        assert_eq!(reply.status, 403);
        assert_eq!(reply.json(), json!({"error": "bad token"}));
    }
    let raw = format!(
        "POST /api/ui-state HTTP/1.1\r\nHost: {host}\r\nX-DiskMap-Token: test-token\r\nContent-Length: 15 \t\r\n\r\n{{\"view\":\"/tmp\"}}"
    );
    let reply = raw_request(&server, &raw, false);
    assert_eq!(reply.status, 200);
    assert_eq!(server.app.ui_state().unwrap()["view"], "/tmp");
}

#[test]
fn agent_requests_update_only_the_agent_heartbeat() {
    use crate::analysis::Source;
    let fixture = Fixture::new();
    let server = fixture.serve("test-token");
    let host = format!("127.0.0.1:{}", server.port);
    assert!(server.app.status()["agent_seen_ago"].is_null());
    let reply = request(
        &server,
        "POST",
        "/api/agent/unknown",
        &host,
        Some("test-token"),
        "{}",
        "en",
    );
    assert_eq!(reply.status, 404);
    assert!(server.app.status()["agent_seen_ago"].is_null());
    let reply = request(
        &server,
        "GET",
        "/api/agent/status",
        &host,
        Some("wrong"),
        "",
        "en",
    );
    assert_eq!(reply.status, 403);
    assert!(server.app.status()["agent_seen_ago"].is_null());
    let reply = request(
        &server,
        "GET",
        "/api/agent/unknown",
        &host,
        Some("test-token"),
        "",
        "en",
    );
    assert_eq!(reply.status, 404);
    assert!(server.app.status()["agent_seen_ago"].is_number());
    let reply = request(
        &server,
        "GET",
        "/api/agent/status",
        &host,
        Some("test-token"),
        "",
        "en",
    );
    assert_eq!(reply.status, 200);
    assert_eq!(reply.json()["window_connected"], false);
    assert!(reply.json()["scan"].is_null());
    assert_eq!(server.app.snapshot().last_seen, 0.0);
    let reply = request(
        &server,
        "POST",
        "/api/agent/show",
        &host,
        Some("test-token"),
        "[]",
        "en",
    );
    assert_eq!(reply.status, 500);
    assert_eq!(server.app.snapshot().last_seen, 0.0);
}

#[test]
fn agent_http_scan_analysis_view_and_show_work_together() {
    use crate::analysis::Source;
    let fixture = Fixture::new();
    let root = fixture.0.join("data");
    fs::create_dir_all(root.join("child")).unwrap();
    fs::write(root.join("file.txt"), b"hello").unwrap();
    fs::write(root.join("child/cache.log"), b"log").unwrap();
    let server = fixture.serve("test-token");
    let host = format!("127.0.0.1:{}", server.port);
    let body = json!({"path": root, "apparent": true, "same_fs": false}).to_string();
    let scanned = request(
        &server,
        "POST",
        "/api/agent/scan",
        &host,
        Some("test-token"),
        &body,
        "en",
    );
    assert_eq!(scanned.status, 200);
    assert_eq!(scanned.json()["reused"], false);
    assert_eq!(
        server.app.current_scan().unwrap().wait(),
        crate::scan::ScanState::Done
    );
    let reused = request(
        &server,
        "POST",
        "/api/agent/scan",
        &host,
        Some("test-token"),
        &body,
        "en",
    );
    assert_eq!(reused.json()["reused"], true);
    for route in [
        "/api/agent/status",
        "/api/agent/summary",
        "/api/agent/list",
        "/api/agent/cleanup?min_mb=0",
        "/api/agent/largest",
    ] {
        assert_eq!(
            request(&server, "GET", route, &host, Some("test-token"), "", "en").status,
            200,
            "{route}"
        );
    }
    let file = root.join("file.txt");
    let route = format!(
        "/api/agent/explain?path={}",
        encode_query(&file.to_string_lossy())
    );
    let explained = request(&server, "GET", &route, &host, Some("test-token"), "", "en");
    assert_eq!(explained.status, 200);
    assert_eq!(explained.json()["bytes"], 5);
    assert_eq!(server.app.snapshot().last_seen, 0.0);
    let ui = json!({"view": root, "selected": file, "pinned": "cache"}).to_string();
    assert_eq!(
        request(
            &server,
            "POST",
            "/api/ui-state",
            &host,
            Some("test-token"),
            &ui,
            "en"
        )
        .status,
        200
    );
    let seen = server.app.snapshot().last_seen;
    let view = request(
        &server,
        "GET",
        "/api/agent/view",
        &host,
        Some("test-token"),
        "",
        "en",
    );
    assert_eq!(view.status, 200);
    assert_eq!(view.json()["highlighted_category"], "cache");
    let show = json!({"path": file, "note": "猫".repeat(205)}).to_string();
    let shown = request(
        &server,
        "POST",
        "/api/agent/show",
        &host,
        Some("test-token"),
        &show,
        "zh",
    );
    assert_eq!(shown.status, 200);
    assert_eq!(shown.json()["window_connected"], true);
    assert_eq!(server.app.status()["command"]["id"], 1);
    assert_eq!(
        server.app.status()["command"]["note"]
            .as_str()
            .unwrap()
            .chars()
            .count(),
        200
    );
    let show = json!({"path": file, "note": [1, true, null]}).to_string();
    assert_eq!(
        request(
            &server,
            "POST",
            "/api/agent/show",
            &host,
            Some("test-token"),
            &show,
            "en"
        )
        .status,
        200
    );
    assert_eq!(server.app.status()["command"]["id"], 2);
    assert_eq!(
        server.app.status()["command"]["note"],
        json!([1, true, null])
    );
    let show = json!({"path": file, "note": 1}).to_string();
    let bad = request(
        &server,
        "POST",
        "/api/agent/show",
        &host,
        Some("test-token"),
        &show,
        "en",
    );
    assert_eq!(bad.status, 500);
    assert_eq!(
        bad.json(),
        json!({"error": "TypeError(\"'int' object is not subscriptable\")"})
    );
    assert_eq!(server.app.status()["command"]["id"], 2);
    assert_eq!(server.app.snapshot().last_seen, seen);
}

#[test]
fn agent_numeric_errors_follow_legacy_order_and_status() {
    let fixture = Fixture::new();
    let root = fixture.0.join("data");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("cache.log"), b"log").unwrap();
    let server = fixture.serve("test-token");
    let host = format!("127.0.0.1:{}", server.port);
    let idle = request(
        &server,
        "GET",
        "/api/agent/summary?top=bad",
        &host,
        Some("test-token"),
        "",
        "en",
    );
    assert_eq!(idle.status, 400);
    assert!(
        idle.json()["error"]
            .as_str()
            .unwrap()
            .starts_with("Nothing has been scanned")
    );
    for (value, code, message) in [
        ("bad", 400, "could not convert string to float: 'bad'"),
        ("nan", 400, "cannot convert float NaN to integer"),
        (
            "inf",
            500,
            "OverflowError('cannot convert float infinity to integer')",
        ),
    ] {
        let reply = request(
            &server,
            "GET",
            &format!("/api/agent/cleanup?min_mb={value}&limit=bad"),
            &host,
            Some("test-token"),
            "",
            "en",
        );
        assert_eq!(reply.status, code);
        assert_eq!(reply.json(), json!({"error": message}));
    }
    server
        .app
        .start_scan(&root.to_string_lossy(), true, true)
        .unwrap();
    server.app.current_scan().unwrap().wait();
    let bad = request(
        &server,
        "GET",
        "/api/agent/largest?limit=bad&category=bad",
        &host,
        Some("test-token"),
        "",
        "en",
    );
    assert_eq!(bad.status, 400);
    assert_eq!(
        bad.json(),
        json!({"error": "invalid literal for int() with base 10: 'bad'"})
    );
    for value in ["-9999999999999999999999", "9999999999999999999999", "1_0"] {
        assert_eq!(
            request(
                &server,
                "GET",
                &format!("/api/agent/list?limit={value}"),
                &host,
                Some("test-token"),
                "",
                "en"
            )
            .status,
            200
        );
    }
    for (route, parameter, value) in [
        ("summary", "top", "１２"),
        ("list", "limit", "٢"),
        ("cleanup", "min_mb", "０.５"),
    ] {
        let route = format!("/api/agent/{route}?{parameter}={}", encode_query(value));
        assert_eq!(
            request(&server, "GET", &route, &host, Some("test-token"), "", "en").status,
            200
        );
    }
    let all = request(
        &server,
        "GET",
        "/api/agent/cleanup?min_mb=-1.5",
        &host,
        Some("test-token"),
        "",
        "en",
    );
    assert_eq!(all.status, 200);
    assert_eq!(all.json()["candidates_shown"], 1);
    let none = request(
        &server,
        "GET",
        "/api/agent/cleanup?min_mb=1e30",
        &host,
        Some("test-token"),
        "",
        "en",
    );
    assert_eq!(none.status, 200);
    assert_eq!(none.json()["candidates_shown"], 0);
}

#[test]
fn agent_view_preserves_raw_ui_values_without_spurious_nested_queries() {
    let fixture = Fixture::new();
    let root = fixture.0.join("data");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("file.txt"), b"hello").unwrap();
    let server = fixture.serve("test-token");
    let host = format!("127.0.0.1:{}", server.port);
    server
        .app
        .start_scan(&root.to_string_lossy(), true, true)
        .unwrap();
    server.app.current_scan().unwrap().wait();
    for (view, selected) in [
        (json!(0), json!(false)),
        (json!(false), json!([])),
        (json!([]), json!({})),
        (json!(true), json!(42)),
        (json!(42), json!(true)),
        (json!(null), json!("")),
        (json!([1]), json!({"key": false})),
    ] {
        let ui = json!({"view": view, "selected": selected, "pinned": null}).to_string();
        assert_eq!(
            request(
                &server,
                "POST",
                "/api/ui-state",
                &host,
                Some("test-token"),
                &ui,
                "en"
            )
            .status,
            200
        );
        let reply = request(
            &server,
            "GET",
            "/api/agent/view",
            &host,
            Some("test-token"),
            "",
            "en",
        );
        assert_eq!(reply.status, 200);
        let body = reply.json();
        assert_eq!(body["view"], view);
        assert_eq!(body["selected"], selected);
        assert!(body.get("view_summary").is_none());
        assert!(body.get("selected_detail").is_none());
        let status = request(
            &server,
            "GET",
            "/api/agent/status",
            &host,
            Some("test-token"),
            "",
            "en",
        );
        assert_eq!(status.status, 200);
        assert_eq!(
            status.json()["user_view"],
            json!({"view": view, "selected": selected})
        );
    }
}

fn cached_tree(app: &App, path: &str, frac: f64, language: i18n::Lang) -> Arc<[u8]> {
    let _scope = i18n::scope(language);
    app.tree_bytes(path, frac).unwrap()
}

#[test]
fn tree_http_cache_reuses_bytes_and_invalidates_settings_and_processes() {
    const TEST: &str =
        "server::tests::tree_http_cache_reuses_bytes_and_invalidates_settings_and_processes";
    const CHILD: &str = "DISKMAP_T05_CACHE_TEST_CHILD";
    // settings 改进程默认语言；另起只运行本测试的进程，避免干扰 i18n 的并发默认值测试。
    if std::env::var(CHILD).ok().as_deref() != Some(TEST) {
        let isolated = Fixture::new();
        let output = child::command_output(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
                .env(CHILD, TEST)
                .env("DISKMAP_CACHE_DIR", isolated.0.join("cache"))
                .env("DISKMAP_CONFIG_DIR", isolated.0.join("config"))
                .env("DISKMAP_LANG", "en"),
            Duration::from_secs(120),
        )
        .expect("HTTP 缓存测试子进程应在两分钟内完成并回收");
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    let fixture = Fixture::new();
    let root = fixture.0.join("data");
    fs::create_dir(&root).unwrap();
    let file = root.join("active.txt");
    fs::write(&file, b"in use").unwrap();
    let server = fixture.serve("test-token");
    let mut processes = crate::procinfo::ProcessMap::default();
    processes.insert(
        42,
        crate::procinfo::Usage {
            comm: i18n::Text::new("测试程序", "Test process"),
            kind: crate::procinfo::Kind::Open,
        },
    );
    server.app.procs().install([(file, processes)]);
    let path = root.to_string_lossy();
    server.app.start_scan(&path, true, true).unwrap();
    assert_eq!(
        server.app.current_scan().unwrap().wait(),
        crate::scan::ScanState::Done
    );
    let host = format!("127.0.0.1:{}", server.port);
    let route = format!("/api/tree?path={}", encode_query(&path));
    let frac = 0.000025;
    let en = cached_tree(&server.app, &path, frac, i18n::Lang::En);
    let zh = cached_tree(&server.app, &path, frac, i18n::Lang::Zh);
    assert!(!Arc::ptr_eq(&en, &zh));
    assert_ne!(&*en, &*zh);
    assert!(String::from_utf8_lossy(&en).contains("Test process"));
    assert!(String::from_utf8_lossy(&zh).contains("测试程序"));

    for _ in 0..3 {
        for (language, expected) in [("en", &en), ("zh", &zh)] {
            let reply = request(
                &server,
                "GET",
                &route,
                &host,
                Some("test-token"),
                "",
                language,
            );
            assert_eq!(reply.status, 200);
            assert_eq!(reply.body.as_slice(), expected.as_ref());
            assert!(reply.headers.contains("Server-Timing: tree;dur="));
        }
    }
    assert!(Arc::ptr_eq(
        &en,
        &cached_tree(&server.app, &path, frac, i18n::Lang::En)
    ));
    assert!(Arc::ptr_eq(
        &zh,
        &cached_tree(&server.app, &path, frac, i18n::Lang::Zh)
    ));
    let bad_host = request(
        &server,
        "GET",
        &route,
        "evil.example",
        Some("test-token"),
        "",
        "en",
    );
    assert_eq!(bad_host.status, 403);
    assert_eq!(bad_host.json(), json!({"error": "bad host"}));
    let bad_token = request(&server, "GET", &route, &host, Some("wrong-token"), "", "en");
    assert_eq!(bad_token.status, 403);
    assert_eq!(bad_token.json(), json!({"error": "bad token"}));
    assert!(Arc::ptr_eq(
        &en,
        &cached_tree(&server.app, &path, frac, i18n::Lang::En)
    ));

    let changed = request(
        &server,
        "POST",
        "/api/settings",
        &host,
        Some("test-token"),
        "{\"lang\":\"zh\"}",
        "en",
    );
    assert_eq!(changed.status, 200);
    let en_after_settings = cached_tree(&server.app, &path, frac, i18n::Lang::En);
    let zh_after_settings = cached_tree(&server.app, &path, frac, i18n::Lang::Zh);
    assert!(!Arc::ptr_eq(&en, &en_after_settings));
    assert!(!Arc::ptr_eq(&zh, &zh_after_settings));
    assert_eq!(&*en, &*en_after_settings);
    assert_eq!(&*zh, &*zh_after_settings);
    let default_zh = request(&server, "GET", &route, &host, Some("test-token"), "", "");
    assert_eq!(default_zh.status, 200);
    assert_eq!(default_zh.body.as_slice(), zh_after_settings.as_ref());
    let explicit_en = request(&server, "GET", &route, &host, Some("test-token"), "", "en");
    assert_eq!(explicit_en.status, 200);
    assert_eq!(explicit_en.body.as_slice(), en_after_settings.as_ref());

    let refreshed = request(
        &server,
        "POST",
        "/api/refresh-procs",
        &host,
        Some("test-token"),
        "{}",
        "en",
    );
    assert_eq!(refreshed.status, 200);
    let en_after_refresh = cached_tree(&server.app, &path, frac, i18n::Lang::En);
    assert!(!Arc::ptr_eq(&en_after_settings, &en_after_refresh));
    let reply = request(&server, "GET", &route, &host, Some("test-token"), "", "en");
    assert_eq!(reply.status, 200);
    assert_eq!(reply.body.as_slice(), en_after_refresh.as_ref());
    assert!(Arc::ptr_eq(
        &en_after_refresh,
        &cached_tree(&server.app, &path, frac, i18n::Lang::En)
    ));
}

#[test]
fn shutdown_wakes_an_idle_blocking_accept() {
    let fixture = Fixture::new();
    let mut server = fixture.serve("test-token");
    let host = format!("127.0.0.1:{}", server.port);
    assert_eq!(
        request(
            &server,
            "GET",
            "/api/status",
            &host,
            Some("test-token"),
            "",
            "en"
        )
        .status,
        200
    );
    let started = Instant::now();
    server.shutdown();
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(!fixture.0.join("instance.json").exists());
    // 二次关闭无需连接已释放的端口，也不会等待新请求。
    let started = Instant::now();
    server.shutdown();
    assert!(started.elapsed() < Duration::from_secs(1));
}
