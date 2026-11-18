//! 从旧版 test_agent.py 移植的进程级协议、后台工具和窗口接入测试。
//! 每个子进程的缓存、设置和测试文件都在独立临时目录，不改变测试进程环境。

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use diskmap_core::{app::App, classify::Classifier, pkgdb::PkgDb, procinfo::ProcInfo, server};
use serde_json::{Value, json};

#[path = "../../diskmap-core/tests/support/child.rs"]
mod child;
use child::{ChildGuard, command_output};

const CHILD_TIMEOUT: Duration = Duration::from_secs(90);

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static SERIAL: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "diskmap-cli-test-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_diskmap-cli"));
        command
            .args(args)
            .env("DISKMAP_CACHE_DIR", self.0.join("cache"))
            .env("DISKMAP_CONFIG_DIR", self.0.join("config"))
            .env("DISKMAP_LANG", "zh")
            .env_remove("DISKMAP_NO_ATTACH")
            .env_remove("DISKMAP_PORT")
            .env_remove("DISKMAP_TOKEN");
        command
    }

    fn tree(&self) -> (PathBuf, u64) {
        let root = self.0.join("data");
        let mut total = 0;
        for (path, bytes) in [
            ("docs/report.pdf", 400_000),
            ("proj/cache/blob.bin", 2_500_000),
            ("proj/__pycache__/m.pyc", 1_200_000),
            ("proj/build.log", 1_100_000),
            ("proj/main.py", 3_000),
        ] {
            let path = root.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, vec![b'x'; bytes]).unwrap();
            total += bytes as u64;
        }
        (root, total)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Mcp {
    // 从 spawn 成功起持有，即使初始化/收发消息 panic 也结束并回收子进程。
    _process: ChildGuard,
    input: Option<ChildStdin>,
    output: mpsc::Receiver<String>,
    next_id: u64,
}

impl Mcp {
    fn start(fixture: &Fixture, attach: bool) -> Self {
        let mut command = fixture.command(&["mcp"]);
        if !attach {
            command.arg("--no-attach");
        }
        let mut process = ChildGuard::spawn(
            command
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit()),
        )
        .unwrap();
        let input = process.stdin.take();
        let stdout = process.stdout.take().unwrap();
        let (sender, output) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if sender.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        Self {
            _process: process,
            input,
            output,
            next_id: 0,
        }
    }

    fn send(&mut self, text: &str) {
        let input = self.input.as_mut().unwrap();
        writeln!(input, "{text}").unwrap();
        input.flush().unwrap();
    }

    fn receive(&self) -> Value {
        let line = self
            .output
            .recv_timeout(Duration::from_secs(90))
            .expect("MCP 未及时返回协议消息");
        serde_json::from_str(&line)
            .unwrap_or_else(|error| panic!("标准输出不是 JSON：{line:?}，{error}"))
    }

    fn rpc(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        self.send(
            &json!({"jsonrpc":"2.0", "id":self.next_id, "method":method, "params":params})
                .to_string(),
        );
        let response = self.receive();
        assert_eq!(response["id"], self.next_id);
        assert_eq!(response["jsonrpc"], "2.0");
        response
    }

    fn call(&mut self, name: &str, args: Value) -> Value {
        let response = self.rpc("tools/call", json!({"name":name, "arguments":args}));
        assert!(response.get("error").is_none(), "{response}");
        response["result"].clone()
    }

    fn success(&mut self, name: &str, args: Value) -> Value {
        let result = self.call(name, args);
        assert_eq!(result["isError"], false, "{result}");
        let text: Value =
            serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(text, result["structuredContent"]);
        text
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        self.input.take();
    }
}

#[test]
fn protocol_versions_errors_notifications_and_batches() {
    let fixture = Fixture::new();
    let mut mcp = Mcp::start(&fixture, false);
    for version in [
        "2025-11-25",
        "2025-06-18",
        "2025-03-26",
        "2024-11-05",
        "1999-01-01",
    ] {
        let result = mcp.rpc("initialize", json!({"protocolVersion":version}));
        assert_eq!(
            result["result"]["protocolVersion"],
            if version == "1999-01-01" {
                "2025-11-25"
            } else {
                version
            }
        );
        assert_eq!(
            result["result"]["capabilities"]["tools"]["listChanged"],
            false
        );
    }
    let result = mcp.rpc("tools/list", json!({}));
    let tools = result["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 9);
    for tool in tools {
        assert_eq!(tool["inputSchema"]["type"], "object");
        assert_eq!(tool["inputSchema"]["additionalProperties"], false);
        assert_eq!(tool["annotations"]["destructiveHint"], false);
    }
    assert_eq!(mcp.rpc("ping", json!({}))["result"], json!({}));
    assert_eq!(mcp.rpc("no/such", json!({}))["error"]["code"], -32601);
    assert_eq!(
        mcp.rpc("tools/call", json!({"name":"nope"}))["error"]["code"],
        -32602
    );
    for (method, key) in [
        ("resources/list", "resources"),
        ("prompts/list", "prompts"),
        ("resources/templates/list", "resourceTemplates"),
    ] {
        assert_eq!(mcp.rpc(method, json!({}))["result"][key], json!([]));
    }
    mcp.send("{malformed");
    assert_eq!(mcp.receive()["error"]["code"], -32700);
    mcp.send(r#"{"jsonrpc":"1.0","id":90,"method":"ping"}"#);
    assert_eq!(mcp.receive()["error"]["code"], -32600);
    // 后面的 ping 相当于屏障；收到它之前不能出现通知或客户端响应的回包。
    mcp.send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
    mcp.send(r#"{"jsonrpc":"2.0","id":91,"result":{}}"#);
    assert_eq!(mcp.rpc("ping", json!({}))["result"], json!({}));
    mcp.send(r#"[{"jsonrpc":"2.0","id":101,"method":"ping"},{"jsonrpc":"2.0","method":"notifications/cancelled"},{"jsonrpc":"2.0","id":102,"method":"ping"}]"#);
    let mut ids = [
        mcp.receive()["id"].as_u64().unwrap(),
        mcp.receive()["id"].as_u64().unwrap(),
    ];
    ids.sort_unstable();
    assert_eq!(ids, [101, 102]);
}

#[test]
fn background_scan_and_all_tools() {
    let fixture = Fixture::new();
    let (root, total) = fixture.tree();
    let mut mcp = Mcp::start(&fixture, false);
    let scan = mcp.success(
        "diskmap_scan",
        json!({"path":root,"wait_seconds":30,"apparent":true}),
    );
    assert_eq!(scan["connected_to"], "background");
    assert_eq!(scan["summary"]["bytes"], total);
    assert_eq!(scan["scan"]["state"], "done");
    let status = mcp.success("diskmap_status", json!({}));
    assert_eq!(status["connected_to"], "background");
    let summary = mcp.success("diskmap_summary", json!({"top":3}));
    assert_eq!(summary["bytes"], total);
    assert_eq!(summary["largest_files"].as_array().unwrap().len(), 3);
    let cleanup = mcp.success("diskmap_cleanup", json!({"min_mb":0}));
    assert!(cleanup["reclaimable_bytes"].as_u64().unwrap() <= total);
    let list = mcp.success("diskmap_list", json!({"path":root,"limit":1}));
    assert_eq!(list["shown"], 1);
    let largest = mcp.success("diskmap_largest_files", json!({"limit":1}));
    assert!(
        largest["files"][0]["path"]
            .as_str()
            .unwrap()
            .ends_with("blob.bin")
    );
    let explain = mcp.success(
        "diskmap_explain",
        json!({"path":root.join("docs/report.pdf")}),
    );
    assert_eq!(explain["bytes"], 400_000);
    for (name, args) in [
        ("diskmap_explain", json!({"path":root.join("missing")})),
        ("diskmap_summary", json!({"top":"invalid"})),
        ("diskmap_list", json!({"path":root,"limit":"invalid"})),
        ("diskmap_cleanup", json!({"min_mb":"invalid"})),
        ("diskmap_largest_files", json!({"category":"nonsense"})),
        ("diskmap_user_view", json!({})),
        ("diskmap_show", json!({"path":root})),
        ("diskmap_scan", json!({"path":root.join("missing")})),
    ] {
        assert_eq!(mcp.call(name, args)["isError"], true, "{name}");
    }
    let reused = mcp.success(
        "diskmap_scan",
        json!({"path":root.join("proj"),"apparent":true}),
    );
    assert_eq!(reused["reused_existing_scan"], true);
    assert!(!fixture.0.join("cache/instance.json").exists());
}

fn app_for(root: &Path) -> Arc<App> {
    let package = Arc::new(PkgDb::default());
    let classifier = Arc::new(Classifier::new(Some(Arc::clone(&package)), &[]));
    let app = Arc::new(App::with_components(package, classifier, ProcInfo::new()));
    app.start_scan(root.to_str().unwrap(), true, true).unwrap();
    app.current_scan().unwrap().wait();
    app
}

#[test]
fn attach_window_view_show_reuse_and_no_attach() {
    let fixture = Fixture::new();
    let (root, _) = fixture.tree();
    let app = app_for(&root);
    let mut server = server::serve_with_app(
        server::ServeOptions {
            instance_path: Some(fixture.0.join("cache/instance.json")),
            settings_path: Some(fixture.0.join("config/settings.json")),
            token: Some("agent-test-token".into()),
            ..Default::default()
        },
        Arc::clone(&app),
    )
    .unwrap();
    let mut mcp = Mcp::start(&fixture, true);
    let status = mcp.success("diskmap_status", json!({}));
    assert_eq!(status["connected_to"], "window");
    assert_eq!(status["scan"]["root"], root.to_str().unwrap());
    app.touch_ui();
    app.set_ui_state(
        json!({"view":root.join("proj"),"selected":root.join("proj/build.log"),"pinned":null}),
    );
    let view = mcp.success("diskmap_user_view", json!({}));
    assert_eq!(view["selected_detail"]["bytes"], 1_100_000);
    assert_eq!(
        view["view_summary"]["path"],
        root.join("proj").to_str().unwrap()
    );
    let shown = mcp.success(
        "diskmap_show",
        json!({"path":root.join("docs"),"note":"这里是文档"}),
    );
    assert_eq!(shown["ok"], true);
    assert_eq!(app.status()["command"]["note"], "这里是文档");
    assert!(!app.status()["agent_seen_ago"].is_null());
    let scan = mcp.success(
        "diskmap_scan",
        json!({"path":root.join("proj"),"apparent":true}),
    );
    assert_eq!(scan["reused_existing_scan"], true);
    let mut background = Mcp::start(&fixture, false);
    assert_eq!(
        background.success("diskmap_status", json!({}))["connected_to"],
        "background"
    );
    server.shutdown();
    assert!(!fixture.0.join("cache/instance.json").exists());
    assert_eq!(
        mcp.success("diskmap_status", json!({}))["connected_to"],
        "background"
    );
}

#[test]
fn analyze_json_and_markdown_file() {
    let fixture = Fixture::new();
    let (root, total) = fixture.tree();
    let output = command_output(
        &mut fixture.command(&[
            "analyze",
            root.to_str().unwrap(),
            "--json",
            "--apparent",
            "-q",
            "--top",
            "3",
        ]),
        CHILD_TIMEOUT,
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["summary"]["bytes"], total);
    assert_eq!(
        report["summary"]["largest_files"].as_array().unwrap().len(),
        3
    );
    let target = fixture.0.join("report.md");
    let output = command_output(
        &mut fixture.command(&[
            "analyze",
            root.to_str().unwrap(),
            "-o",
            target.to_str().unwrap(),
            "-q",
        ]),
        CHILD_TIMEOUT,
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    assert!(fs::read_to_string(target).unwrap().contains("可清理候选"));
}

#[test]
fn selftest_and_legacy_alias_write_reports() {
    let fixture = Fixture::new();
    for command in ["selftest", "--selftest"] {
        let report = fixture.0.join(format!("{command}.json"));
        let output = command_output(
            &mut fixture.command(&[command, "--report", report.to_str().unwrap()]),
            CHILD_TIMEOUT,
        )
        .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let data: Value = serde_json::from_slice(&fs::read(report).unwrap()).unwrap();
        assert_eq!(data["ok"], true, "{data}");
        assert!(data["version"].is_string());
        assert!(data["platform"].is_string());
        assert!(data["frozen"].is_boolean());
        let results = data["results"].as_array().unwrap();
        assert!(results.len() >= 17, "{data}");
        assert!(results.iter().all(|result| result["ok"] == true), "{data}");
    }
}

struct RunningServer {
    process: ChildGuard,
    port: u16,
    output: mpsc::Receiver<String>,
}

impl RunningServer {
    fn start(fixture: &Fixture, mut command: Command) -> Self {
        let mut process = ChildGuard::spawn(
            command
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
        )
        .unwrap();
        let stdout = process.stdout.take().unwrap();
        let stderr = process.stderr.take().unwrap();
        let (sender, output) = mpsc::channel();
        let stderr_sender = sender.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if sender.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines() {
                if stderr_sender.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        let line = output.recv_timeout(Duration::from_secs(30)).unwrap();
        let port = line
            .trim()
            .strip_prefix("http://127.0.0.1:")
            .unwrap()
            .split('/')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert!(line.contains("?t=lifecycle-token"));
        assert!(fixture.0.join("cache/instance.json").exists());
        Self {
            process,
            port,
            output,
        }
    }

    fn wait_message(&self, expected: &str) {
        let started = Instant::now();
        let mut received = Vec::new();
        loop {
            let remaining = Duration::from_secs(10).saturating_sub(started.elapsed());
            let line = self.output.recv_timeout(remaining).unwrap_or_else(|error| {
                panic!("没有收到提示 {expected:?}：{error}；已收到 {received:?}")
            });
            if line == expected {
                return;
            }
            received.push(line);
        }
    }

    fn request(&self, method: &str, path: &str) -> u16 {
        let mut stream = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        write!(stream, "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nX-DiskMap-Token: lifecycle-token\r\nContent-Length: 0\r\nConnection: close\r\n\r\n", self.port).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response.split_whitespace().nth(1).unwrap().parse().unwrap()
    }

    fn page_bye(&self) {
        assert_eq!(self.request("GET", "/api/status"), 200);
        assert_eq!(self.request("POST", "/api/bye?t=lifecycle-token"), 204);
    }

    #[cfg(unix)]
    fn wait_success(&mut self, timeout: Duration) {
        let start = Instant::now();
        loop {
            if let Some(status) = self.process.try_wait().unwrap() {
                assert!(status.success(), "{status}");
                return;
            }
            assert!(start.elapsed() < timeout, "serve 未在时限内结束");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    #[cfg(unix)]
    fn interrupt_success(&mut self) {
        let output = command_output(
            Command::new("kill").args(["-INT", &self.process.id().to_string()]),
            Duration::from_secs(5),
        )
        .unwrap();
        assert!(output.status.success());
        self.wait_success(Duration::from_secs(10));
    }
}

#[test]
fn serve_no_open_ignores_page_bye_until_interrupt() {
    let fixtures = [Fixture::new(), Fixture::new()];
    let mut servers = Vec::new();
    for (fixture, (lang, message)) in fixtures
        .iter()
        .zip([("zh", "按 Ctrl+C 退出。"), ("en", "Press Ctrl+C to quit.")])
    {
        let command = fixture.command(&[
            "serve",
            "--no-open",
            "--token",
            "lifecycle-token",
            "--lang",
            lang,
        ]);
        #[cfg(unix)]
        let command = {
            // 即使不开浏览器的逻辑回归，也不能在测试时打开用户的真实浏览器。
            let (bin, marker) = fake_browser(fixture);
            let mut command = command;
            command
                .env("PATH", bin)
                .env("DISKMAP_TEST_OPEN_MARKER", marker);
            command
        };
        let server = RunningServer::start(fixture, command);
        server.wait_message(message);
        server.page_bye();
        servers.push(server);
    }
    // 超过普通浏览器模式的 8 秒告别宽限，常驻服务仍须保留实例登记。
    std::thread::sleep(Duration::from_secs(9));
    for (fixture, server) in fixtures.iter().zip(&mut servers) {
        assert!(server.process.try_wait().unwrap().is_none());
        assert!(fixture.0.join("cache/instance.json").exists());
        #[cfg(unix)]
        {
            assert!(!fixture.0.join("opened-url").exists());
            server.interrupt_success();
            assert!(!fixture.0.join("cache/instance.json").exists());
        }
    }
}

#[cfg(unix)]
fn fake_browser(fixture: &Fixture) -> (PathBuf, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let bin = fixture.0.join("bin");
    fs::create_dir(&bin).unwrap();
    for executable in ["xdg-open", "open"] {
        let path = bin.join(executable);
        fs::write(
            &path,
            "#!/bin/sh\nprintf '%s\\n' \"$1\" > \"$DISKMAP_TEST_OPEN_MARKER\"\n",
        )
        .unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    (bin, fixture.0.join("opened-url"))
}

#[cfg(unix)]
#[test]
fn serve_default_mode_respects_root_and_browser_lifetime() {
    let fixtures = [Fixture::new(), Fixture::new()];
    let root = diskmap_core::platform::is_admin();
    let mut servers = Vec::new();
    for (fixture, (lang, root_message, quit_message)) in fixtures.iter().zip([
        ("zh", "以 root 运行：为安全起见不自动打开窗口，请在普通用户的浏览器中打开上面的链接。", "按 Ctrl+C 退出。"),
        ("en", "Running as root: no window is opened for safety. Open the link above in a browser as a normal user.", "Press Ctrl+C to quit."),
    ]) {
        let (bin, marker) = fake_browser(fixture);
        let mut command = fixture.command(&["serve", "--token", "lifecycle-token", "--lang", lang]);
        command.env("PATH", bin).env("DISKMAP_TEST_OPEN_MARKER", &marker);
        let server = RunningServer::start(fixture, command);
        if root {
            server.wait_message(root_message);
            server.wait_message(quit_message);
            assert!(!marker.exists(), "root 模式不得运行浏览器开启程序");
        } else {
            let expected = format!("http://127.0.0.1:{}/?t=lifecycle-token", server.port);
            let started = Instant::now();
            while fs::read_to_string(&marker).ok().is_none_or(|value| value.trim() != expected) {
                assert!(started.elapsed() < Duration::from_secs(10), "临时浏览器开启程序未收到正确的链接");
                std::thread::sleep(Duration::from_millis(25));
            }
        }
        server.page_bye();
        servers.push(server);
    }
    if root {
        std::thread::sleep(Duration::from_secs(9));
    }
    for (fixture, server) in fixtures.iter().zip(&mut servers) {
        if root {
            assert!(server.process.try_wait().unwrap().is_none());
            assert!(!fixture.0.join("opened-url").exists());
            server.interrupt_success();
        } else {
            server.wait_success(Duration::from_secs(15));
        }
        assert!(!fixture.0.join("cache/instance.json").exists());
    }
}

#[test]
fn scan_statistics_match_generated_files_without_writing_state() {
    let fixture = Fixture::new();
    let (root, total) = fixture.tree();
    let output = command_output(
        &mut fixture.command(&[
            "scan",
            root.to_str().unwrap(),
            "--json",
            "--apparent",
            "--same-fs",
            "--method",
            "generic",
            "--threads",
            "1",
        ]),
        CHILD_TIMEOUT,
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let data: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(data["state"], "done");
    assert_eq!(data["files"], 5);
    assert_eq!(data["bytes"], total);
    assert_eq!(data["method"], "generic");
    assert_eq!(data["threads"], 1);
    assert_eq!(data["apparent"], true);
    assert_eq!(data["cross_fs"], false);
    assert!(data["elapsed_seconds"].as_f64().unwrap() >= 0.0);
    assert!(data["fallback_reason"].is_null());
    assert!(
        data["peak_rss_bytes"].is_null() || data["peak_rss_bytes"].as_u64().is_some_and(|n| n > 0)
    );
    assert!(!fixture.0.join("cache").exists());
    assert!(!fixture.0.join("config").exists());
}

#[test]
fn abbreviated_language_final_override_and_attached_output() {
    let fixture = Fixture::new();
    for (arguments, title) in [
        (vec!["mcp", "--la", "zh"], "DiskMap 磁盘地图"),
        (
            vec!["mcp", "--lang", "en", "--lang", "zh"],
            "DiskMap 磁盘地图",
        ),
        (vec!["mcp", "--lang", "zh", "--lang", "en"], "DiskMap"),
    ] {
        let mut child = ChildGuard::spawn(
            fixture
                .command(&arguments)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped()),
        )
        .unwrap();
        writeln!(
            child.stdin.take().unwrap(),
            "{}",
            json!({"jsonrpc":"2.0","id":1,"method":"initialize"})
        )
        .unwrap();
        let output = child.wait_with_output(CHILD_TIMEOUT).unwrap();
        assert!(output.status.success());
        let reply: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(reply["result"]["serverInfo"]["title"], title);
    }
    let (root, total) = fixture.tree();
    let report = fixture.0.join("attached.json");
    let output = command_output(
        fixture
            .command(&[
                "analyze",
                root.to_str().unwrap(),
                "--json",
                "--apparent",
                "-q",
            ])
            .arg(format!("-o={}", report.display())),
        CHILD_TIMEOUT,
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    let report: Value = serde_json::from_slice(&fs::read(report).unwrap()).unwrap();
    assert_eq!(report["summary"]["bytes"], total);
}

#[test]
fn selftest_timeout_reports_failure_and_exit_three() {
    let fixture = Fixture::new();
    let report = fixture.0.join("timeout.json");
    let child_temp = fixture.0.join("child-temp");
    fs::create_dir(&child_temp).unwrap();
    let output = command_output(
        fixture
            .command(&[
                "selftest",
                "--limit",
                "0.001",
                "--report",
                report.to_str().unwrap(),
            ])
            .env("TMPDIR", &child_temp)
            .env("TEMP", &child_temp)
            .env("TMP", &child_temp),
        CHILD_TIMEOUT,
    )
    .unwrap();
    assert_eq!(output.status.code(), Some(3));
    let remaining: Vec<_> = fs::read_dir(child_temp)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert!(remaining.is_empty(), "超时自检遗留临时内容：{remaining:?}");
    let data: Value = serde_json::from_slice(&fs::read(report).unwrap()).unwrap();
    assert_eq!(data["ok"], false);
    assert!(
        data["results"]
            .as_array()
            .unwrap()
            .iter()
            .any(|result| result["check"] == "超时" && result["ok"] == false),
        "{data}"
    );
}
