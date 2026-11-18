//! 不打开真实窗口的进程级验收；所有数据、实例登记和设置使用临时目录。

use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[path = "../../diskmap-core/tests/support/child.rs"]
mod child;
use child::{ChildGuard, command_output};

const TIMEOUT: Duration = Duration::from_secs(60);

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static SERIAL: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "diskmap-desktop-test-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            SERIAL.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir_all(path.join("data/中文 空格")).unwrap();
        fs::write(path.join("data/中文 空格/a.bin"), vec![0_u8; 12_345]).unwrap();
        Self(path)
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_DiskMap"));
        command
            .args(args)
            .env("DISKMAP_CACHE_DIR", self.0.join("cache"))
            .env("DISKMAP_CONFIG_DIR", self.0.join("config"))
            .env("DISKMAP_LANG", "en")
            .env_remove("DISKMAP_PORT")
            .env_remove("DISKMAP_TOKEN")
            .env_remove("DISKMAP_UI")
            .env_remove("DISKMAP_NO_ATTACH");
        command
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn help_version_errors_and_legacy_selftest_are_dispatched_without_ui() {
    let fixture = Fixture::new();
    for (lang, expected) in [("zh", "启动后"), ("en", "Choose a folder")] {
        let output =
            command_output(&mut fixture.command(&["--help", "--lang", lang]), TIMEOUT).unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8(output.stdout).unwrap().contains(expected));
    }
    let output = command_output(&mut fixture.command(&["--version"]), TIMEOUT).unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("DiskMap {}\n", diskmap_core::VERSION)
    );
    for args in [
        vec!["--ui=unknown"],
        vec!["--port=65536"],
        vec!["--lang=fr"],
    ] {
        assert_eq!(
            command_output(&mut fixture.command(&args), TIMEOUT)
                .unwrap()
                .status
                .code(),
            Some(2)
        );
    }
    let report = fixture.0.join("selftest.json");
    let output = command_output(
        &mut fixture.command(&["--selftest", "--report", report.to_str().unwrap()]),
        TIMEOUT,
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&fs::read(report).unwrap()).unwrap();
    assert_eq!(report["ok"], true, "{report}");
    assert!(!fixture.0.join("cache/instance.json").exists());
}

#[test]
fn analyze_redirection_and_mcp_pipes_work_through_the_desktop_binary() {
    let fixture = Fixture::new();
    let root = fixture.0.join("data");
    let args = ["analyze", root.to_str().unwrap(), "--apparent", "-q"];
    let target = fixture.0.join("out.md");
    let mut command = fixture.command(&args);
    command
        .stdin(Stdio::null())
        .stdout(fs::File::create(&target).unwrap())
        .stderr(Stdio::piped());
    let output = ChildGuard::spawn(&mut command)
        .unwrap()
        .wait_with_output(TIMEOUT)
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        fs::read_to_string(&target)
            .unwrap()
            .starts_with("# DiskMap")
    );
    let output = command_output(fixture.command(&args).arg("--json"), TIMEOUT).unwrap();
    assert!(output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["summary"]["bytes"], 12_345);

    let mut command = fixture.command(&["mcp", "--no-attach"]);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = ChildGuard::spawn(&mut command).unwrap().wait_with_input(
        b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-11-25\"}}\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"ping\"}\n".to_vec(),
        TIMEOUT,
    ).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let replies = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(replies.len(), 2);
    assert!(
        replies
            .iter()
            .any(|reply| reply["id"] == 1 && reply["result"]["protocolVersion"] == "2025-11-25")
    );
    assert!(
        replies
            .iter()
            .any(|reply| reply["id"] == 2 && reply["result"].is_object())
    );
    assert!(!fixture.0.join("cache/instance.json").exists());
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpStream;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::mpsc;
    use std::time::Instant;

    struct Service {
        process: ChildGuard,
        announcement: String,
        url: String,
        port: u16,
        output: mpsc::Receiver<String>,
    }

    impl Service {
        fn start(fixture: &Fixture, args: &[&str]) -> Self {
            Self::start_with_ui(fixture, args, None)
        }

        fn start_with_ui(fixture: &Fixture, args: &[&str], ui: Option<&str>) -> Self {
            let bin = fixture.0.join("bin");
            fs::create_dir_all(&bin).unwrap();
            let opener = bin.join("xdg-open");
            fs::write(
                &opener,
                "#!/bin/sh\nprintf '%s\\n' \"$1\" > \"$DISKMAP_TEST_OPEN_MARKER\"\n",
            )
            .unwrap();
            fs::set_permissions(opener, fs::Permissions::from_mode(0o700)).unwrap();
            let mut command = fixture.command(args);
            if let Some(ui) = ui {
                command.env("DISKMAP_UI", ui);
            }
            // 明确让 GTK 无法连接显示服务器，测试默认入口的初始化失败和浏览器回退。
            command
                .env("PATH", bin)
                .env_remove("DISPLAY")
                .env_remove("WAYLAND_DISPLAY")
                .env("GDK_BACKEND", "x11")
                .env("DISKMAP_TEST_OPEN_MARKER", fixture.0.join("opened-url"))
                .env("DISKMAP_TOKEN", "desktop test")
                .env("DISKMAP_PORT", "0")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let mut process = ChildGuard::spawn(&mut command).unwrap();
            let stdout = process.stdout.take().unwrap();
            let (send, receive) = mpsc::channel();
            std::thread::spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    if send.send(line.unwrap()).is_err() {
                        break;
                    }
                }
            });
            let announcement = receive.recv_timeout(TIMEOUT).unwrap();
            let url = announcement
                .strip_prefix("DiskMap is running: ")
                .or_else(|| announcement.strip_prefix("DiskMap 已启动："))
                .unwrap_or_else(|| panic!("启动提示格式与旧版不同：{announcement}"))
                .to_owned();
            assert!(url.contains("?t=desktop%20test"), "{url}");
            let port = url
                .strip_prefix("http://127.0.0.1:")
                .unwrap()
                .split('/')
                .next()
                .unwrap()
                .parse()
                .unwrap();
            assert!(fixture.0.join("cache/instance.json").exists());
            Self {
                process,
                announcement,
                url,
                port,
                output: receive,
            }
        }

        fn request(&self, method: &str, path: &str) -> (u16, String) {
            let mut socket = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            socket
                .set_write_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            write!(socket, "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nX-DiskMap-Token: desktop test\r\nContent-Length: 0\r\nConnection: close\r\n\r\n", self.port).unwrap();
            let mut response = String::new();
            socket.read_to_string(&mut response).unwrap();
            let status = response.split_whitespace().nth(1).unwrap().parse().unwrap();
            (
                status,
                response.split_once("\r\n\r\n").unwrap().1.to_owned(),
            )
        }

        fn wait_success(&mut self) {
            let started = Instant::now();
            loop {
                if let Some(status) = self.process.try_wait().unwrap() {
                    assert!(status.success(), "{status}");
                    return;
                }
                assert!(
                    started.elapsed() < Duration::from_secs(15),
                    "桌面服务未在时限内退出"
                );
                std::thread::sleep(Duration::from_millis(25));
            }
        }

        fn signal(&mut self, name: &str) {
            let output = command_output(
                Command::new("kill").args([name, &self.process.id().to_string()]),
                Duration::from_secs(5),
            )
            .unwrap();
            assert!(output.status.success());
            self.wait_success();
        }

        fn wait_for_browser(&self, fixture: &Fixture) {
            let started = Instant::now();
            while fs::read_to_string(fixture.0.join("opened-url"))
                .ok()
                .is_none_or(|url| url.trim() != self.url)
            {
                assert!(started.elapsed() < TIMEOUT, "未调用临时浏览器开启程序");
                std::thread::sleep(Duration::from_millis(25));
            }
        }

        fn remaining_output(&self) -> Vec<String> {
            let mut lines = Vec::new();
            loop {
                match self.output.recv_timeout(Duration::from_secs(5)) {
                    Ok(line) => lines.push(line),
                    Err(mpsc::RecvTimeoutError::Disconnected) => return lines,
                    Err(mpsc::RecvTimeoutError::Timeout) => panic!("退出后标准输出管道未关闭"),
                }
            }
        }
    }

    #[test]
    fn startup_output_matches_legacy_for_each_desktop_mode() {
        for (lang, prefix, quit, root_notice) in [
            (
                "zh",
                "DiskMap 已启动：",
                "按 Ctrl+C 退出。",
                "以 root 运行：为安全起见不自动打开窗口，请在普通用户的浏览器中打开上面的链接。",
            ),
            (
                "en",
                "DiskMap is running: ",
                "Press Ctrl+C to quit.",
                "Running as root: no window is opened for safety. Open the link above in a browser as a normal user.",
            ),
        ] {
            for mode in [
                Some("--no-open"),
                Some("--browser"),
                Some("--ui=browser"),
                None,
            ] {
                let fixture = Fixture::new();
                let mut args = vec!["--lang", lang];
                args.extend(mode);
                let mut service = Service::start(&fixture, &args);
                assert_eq!(service.announcement, format!("{prefix}{}", service.url));
                service.signal("-TERM");
                let expected = if mode == Some("--no-open") {
                    vec![quit]
                } else if diskmap_core::platform::is_admin() {
                    vec![root_notice, quit]
                } else {
                    Vec::new()
                };
                assert_eq!(service.remaining_output(), expected);
                assert!(!fixture.0.join("cache/instance.json").exists());
            }
        }
    }

    #[test]
    fn ui_environment_is_applied_by_the_desktop_entry() {
        for (ui, args, window_expected) in [
            ("browser", vec![], false),
            ("browser", vec!["--ui=webview"], true),
            ("unknown", vec![], true),
        ] {
            let fixture = Fixture::new();
            let mut service = Service::start_with_ui(&fixture, &args, Some(ui));
            if !diskmap_core::platform::is_admin() {
                service.wait_for_browser(&fixture);
            }
            service.signal("-TERM");
            let mut stderr = String::new();
            service
                .process
                .stderr
                .take()
                .unwrap()
                .read_to_string(&mut stderr)
                .unwrap();
            let fell_back =
                stderr.contains("Could not start the window; opening the default browser:");
            assert_eq!(
                fell_back,
                window_expected && !diskmap_core::platform::is_admin(),
                "{stderr}"
            );
            assert!(!fixture.0.join("cache/instance.json").exists());
        }
    }

    #[test]
    fn no_open_scans_and_cleans_on_interrupt_and_termination() {
        for signal in ["-INT", "-TERM"] {
            let fixture = Fixture::new();
            let root = fixture.0.join("data");
            let mut service = Service::start(
                &fixture,
                &[
                    "--no-open",
                    root.to_str().unwrap(),
                    "--apparent",
                    "--same-fs",
                ],
            );
            let started = Instant::now();
            loop {
                let (status, body) = service.request("GET", "/api/status");
                assert_eq!(status, 200);
                let status: Value = serde_json::from_str(&body).unwrap();
                if status["state"] == "done" {
                    break;
                }
                assert!(started.elapsed() < TIMEOUT, "{status}");
                std::thread::sleep(Duration::from_millis(20));
            }
            assert!(!fixture.0.join("opened-url").exists());
            service.signal(signal);
            assert!(!fixture.0.join("cache/instance.json").exists());
            assert!(TcpStream::connect(("127.0.0.1", service.port)).is_err());
        }
    }

    #[test]
    fn browser_flags_and_webview_failure_keep_the_service_until_page_bye() {
        for args in [vec!["--browser"], vec!["--ui=browser"], Vec::new()] {
            let fixture = Fixture::new();
            let mut service = Service::start(&fixture, &args);
            if diskmap_core::platform::is_admin() {
                // root 的旧版兼容行为是不打开任何窗口/浏览器。
                service.signal("-TERM");
            } else {
                service.wait_for_browser(&fixture);
                assert_eq!(service.request("GET", "/api/status").0, 200);
                assert_eq!(service.request("POST", "/api/bye?t=desktop%20test").0, 204);
                assert!(
                    service.process.try_wait().unwrap().is_none(),
                    "bye 后应保留 8 秒宽限"
                );
                service.wait_success();
            }
            assert!(!fixture.0.join("cache/instance.json").exists());
        }
    }
}
