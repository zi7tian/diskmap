//! 自检只操作自己创建的临时目录；通过实际 HTTP 和 MCP 编解码验证打包后的完整流程。

use std::fs;
use std::io::{self, BufRead, Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use diskmap_core::app::App;
use diskmap_core::i18n::{self, Text};
use diskmap_core::server::{ServeOptions, ServerHandle, serve_with_app};
use serde_json::{Value, json};

use crate::http::Endpoint;

#[derive(Default)]
struct Progress {
    results: Vec<Value>,
    step: String,
}

#[derive(Default)]
struct State {
    progress: Mutex<Progress>,
    // 造树和实例登记均在此锁内完成；清理先置取消位，再取得此锁，阻止迟到的写入。
    resources: Mutex<Option<ServerHandle>>,
    canceled: AtomicBool,
}

type Shared = Arc<State>;

const CLEANUP_TIMEOUT: Duration = Duration::from_secs(3);
const CLEANUP_RETRY: Duration = Duration::from_secs(2);
const PROCESS_REFRESH_TIMEOUT: Duration = Duration::from_secs(60);
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(100);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

fn lock(state: &Shared) -> MutexGuard<'_, Progress> {
    state
        .progress
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

fn check(state: &Shared, name: Text, ok: bool, detail: impl ToString) {
    lock(state).results.push(json!({
        "check": name.text(),
        "ok": ok,
        "detail": detail.to_string().chars().take(300).collect::<String>(),
    }));
}

/// 返回旧版的自检退出码：成功 0，失败 1，总超时 3。库入口不终止宿主进程。
pub(crate) fn run(report_path: Option<&Path>, limit: f64) -> i32 {
    let state = Arc::new(State::default());
    lock(&state).step = Text::new("启动", "startup").to_string();
    let timeout = match Duration::try_from_secs_f64(limit) {
        Ok(timeout) if !timeout.is_zero() => timeout,
        _ => {
            eprintln!(
                "{}",
                Text::new(
                    "自检时限必须是有限正数",
                    "The self-test time limit must be finite and positive"
                )
            );
            return 2;
        }
    };
    let code = perform(&state, timeout, limit);
    finish(&state, code, report_path)
}

fn perform(state: &Shared, timeout: Duration, limit: f64) -> i32 {
    let started = Instant::now();
    // 所有权留在监督方；工作线程只有路径，不依赖进程退出时会被跳过的 Drop 清理。
    let temp = match Temporary::new() {
        Ok(temp) => temp,
        Err(error) => {
            check(state, Text::new("异常", "Exception"), false, error);
            return 1;
        }
    };
    let path = temp.0.clone();
    let lang = i18n::current();
    let worker_state = Arc::clone(state);
    let (tx, rx) = mpsc::channel();
    let worker = thread::Builder::new()
        .name("diskmap-selftest".into())
        .spawn(move || {
            let _scope = i18n::scope(lang);
            let outcome = std::panic::catch_unwind(|| exercise(&worker_state, &path));
            match outcome {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    check(&worker_state, Text::new("异常", "Exception"), false, error);
                }
                Err(_) => check(
                    &worker_state,
                    Text::new("异常", "Exception"),
                    false,
                    Text::new("自检线程异常退出", "The self-test worker panicked"),
                ),
            }
            let _ = tx.send(());
        });

    let code = match worker {
        Ok(worker) => match rx.recv_timeout(timeout.saturating_sub(started.elapsed())) {
            Ok(()) => {
                let _ = worker.join();
                if lock(state).results.iter().all(|r| r["ok"] == true) {
                    0
                } else {
                    1
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                state.canceled.store(true, Ordering::Release);
                let step = lock(state).step.clone();
                check(
                    state,
                    Text::new("超时", "Timeout"),
                    false,
                    Text::owned(
                        format!("超过 {limit} 秒仍未完成，卡在：{step}"),
                        format!("Not finished within {limit} seconds; current step: {step}"),
                    ),
                );
                3
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = worker.join();
                check(
                    state,
                    Text::new("异常", "Exception"),
                    false,
                    Text::new(
                        "自检线程未返回结果",
                        "The self-test worker returned no result",
                    ),
                );
                1
            }
        },
        Err(error) => {
            check(state, Text::new("异常", "Exception"), false, error);
            1
        }
    };
    let cleaned = cleanup(state, temp, CLEANUP_TIMEOUT);
    if code == 0
        && (!cleaned
            || lock(state)
                .results
                .iter()
                .any(|result| result["ok"] != true))
    {
        1
    } else {
        code
    }
}

fn finish(state: &Shared, mut code: i32, report_path: Option<&Path>) -> i32 {
    let mut report = report(state, code);
    if let Some(path) = report_path
        && let Err(error) = fs::write(path, pretty(&report))
    {
        check(
            state,
            Text::new("写入报告", "Write report"),
            false,
            Text::owned(
                format!("无法写入 {}：{error}", path.display()),
                format!("Cannot write {}: {error}", path.display()),
            ),
        );
        code = if code == 3 { 3 } else { 1 };
        report = self::report(state, code);
    }
    if let Err(error) = writeln!(io::stdout().lock(), "{}", pretty(&report)) {
        eprintln!(
            "{}",
            Text::owned(
                format!("无法输出报告：{error}"),
                format!("Cannot print report: {error}")
            )
        );
        return if code == 3 { 3 } else { 1 };
    }
    code
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).expect("JSON value serialization cannot fail")
}

fn report(state: &Shared, code: i32) -> Value {
    let platform = if cfg!(windows) {
        "win32"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else {
        std::env::consts::OS
    };
    json!({
        "ok": code == 0,
        "version": diskmap_core::VERSION,
        "platform": platform,
        "frozen": true,
        "results": lock(state).results,
    })
}

fn exercise(state: &Shared, temporary: &Path) -> Result<(), String> {
    let root = temporary.join("data");
    with_writes(state, |_| {
        fs::create_dir_all(root.join("big"))?;
        fs::create_dir(root.join("many"))?;
        fs::write(root.join("big/data.bin"), vec![0; 300_000])
    })?;
    for i in 0..400 {
        with_writes(state, |_| {
            fs::write(root.join("many").join(format!("f{i}.txt")), [b'x'; 100])
        })?;
    }
    let expected = 340_000;
    let ep = with_writes(state, |slot| {
        let server = serve_with_app(
            ServeOptions {
                token: Some("selftest-token".into()),
                instance_path: Some(temporary.join("cache/instance.json")),
                settings_path: Some(temporary.join("config/settings.json")),
                ..ServeOptions::default()
            },
            Arc::new(App::new()),
        )?;
        let endpoint = Endpoint {
            port: server.port,
            token: server.token.clone(),
        };
        *slot = Some(server);
        Ok(endpoint)
    })?;
    let body = request(state, &ep, "GET", "/?t=selftest-token", &[], None)?;
    check(
        state,
        Text::new("首页", "Home page"),
        body.windows(14).any(|s| s == b"selftest-token"),
        "",
    );
    let body = request(state, &ep, "GET", "/static/app.js", &[], None)?;
    check(
        state,
        Text::new("界面脚本", "Interface script"),
        body.windows(8).any(|s| s == b"squarify"),
        "",
    );
    let meta = request_json(state, &ep, "GET", "/api/meta", &[], None)?;
    check(
        state,
        Text::new("元数据", "Metadata"),
        meta["classes"].as_array().is_some_and(|a| a.len() == 7)
            && meta["sep"] == std::path::MAIN_SEPARATOR.to_string(),
        &meta["os"],
    );
    check(
        state,
        Text::new("磁盘列表", "Disk list"),
        meta["disks"].as_array().is_some_and(|a| !a.is_empty()),
        &meta["disks"],
    );
    request_json(
        state,
        &ep,
        "POST",
        "/api/scan",
        &[],
        Some(&json!({"path": root, "apparent": true})),
    )?;
    let mut status = Value::Null;
    for _ in 0..300 {
        status = request_json(state, &ep, "GET", "/api/status", &[], None)?;
        if status["state"] != "scanning" {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    check(
        state,
        Text::new("扫描完成", "Scan completed"),
        status["state"] == "done",
        &status["error"],
    );
    let query = [("path", root.to_string_lossy().into_owned())];
    let tree = request_json(state, &ep, "GET", "/api/tree", &query, None)?;
    check(
        state,
        Text::new("大小正确", "Correct size"),
        tree["tree"]["s"] == expected,
        Text::owned(
            format!("统计 {}，应为 {expected}", tree["tree"]["s"]),
            format!("Counted {}; expected {expected}", tree["tree"]["s"]),
        ),
    );
    let mut names: Vec<_> = tree["tree"]["ch"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["t"] == "d")
        .filter_map(|c| c["n"].as_str())
        .collect();
    names.sort_unstable();
    check(
        state,
        Text::new("子文件夹", "Subfolders"),
        names == ["big", "many"],
        json!(names),
    );
    let file_query = [(
        "path",
        root.join("big/data.bin").to_string_lossy().into_owned(),
    )];
    let info = request_json(state, &ep, "GET", "/api/info", &file_query, None)?;
    check(
        state,
        Text::new("文件详情", "File details"),
        info["size"] == 300_000 && info["type"] == "file",
        &info["reason"],
    );
    let processes = request_json(
        state,
        &ep,
        "POST",
        "/api/refresh-procs",
        &[],
        Some(&json!({})),
    )?;
    let refresh = wait_for_processes(
        state,
        processes["procs"].clone(),
        PROCESS_REFRESH_TIMEOUT,
        |timeout| {
            let bytes = request_with_timeout(state, &ep, "GET", "/api/status", &[], None, timeout)?;
            let status: Value =
                serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
            Ok(status["procs"].clone())
        },
    )?;
    record_process_checks(state, refresh, PROCESS_REFRESH_TIMEOUT);
    let summary = request_json(state, &ep, "GET", "/api/agent/summary", &query, None)?;
    check(
        state,
        Text::new("AI 接口：概览", "AI API: summary"),
        summary["bytes"] == expected
            && summary["by_category"]
                .as_array()
                .is_some_and(|a| !a.is_empty()),
        &summary["size"],
    );
    mcp_roundtrip(state, &root, expected)?;
    lock(state).step = Text::new("清理", "cleanup").to_string();
    Ok(())
}

struct ProcessRefresh {
    procs: Value,
    timed_out: bool,
}

/// 等待已有或新发起的采集完成；不以旧快照的文件数判断是否还应等待。
fn wait_for_processes(
    state: &Shared,
    mut procs: Value,
    budget: Duration,
    mut fetch: impl FnMut(Duration) -> Result<Value, String>,
) -> Result<ProcessRefresh, String> {
    let started = Instant::now();
    let mut idle_seen = false;
    loop {
        checkpoint(state)?;
        let remaining = budget.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            break;
        }
        match fetch(remaining.min(REQUEST_TIMEOUT)) {
            Ok(next) => procs = next,
            Err(error) => {
                checkpoint(state)?;
                if started.elapsed() >= budget {
                    break;
                }
                return Err(error);
            }
        }
        checkpoint(state)?;
        if started.elapsed() >= budget {
            break;
        }
        if procs["refreshing"] == false {
            if idle_seen {
                return Ok(ProcessRefresh {
                    procs,
                    timed_out: false,
                });
            }
            // App::status 先读快照再读 refreshing；两者之间可能刚好完成发布。
            // 首次观察到 false 后立即再 GET，确保使用发布后的快照，而非旧的空快照。
            idle_seen = true;
            continue;
        }
        idle_seen = false;
        thread::sleep(PROCESS_POLL_INTERVAL.min(budget.saturating_sub(started.elapsed())));
    }
    Ok(ProcessRefresh {
        procs,
        timed_out: true,
    })
}

fn record_process_checks(state: &Shared, refresh: ProcessRefresh, budget: Duration) {
    let procs = &refresh.procs;
    let finished = !refresh.timed_out && procs["refreshing"] == false;
    let timeout_detail = refresh.timed_out.then(|| {
        let seconds = budget.as_secs_f64();
        Text::owned(
            format!("等待进程刷新超时（{seconds} 秒）；最后状态：{procs}"),
            format!("Timed out waiting for process refresh after {seconds} s; last state: {procs}"),
        )
        .to_string()
    });
    for (name, ok, detail) in [
        (
            Text::new("读取运行中的程序", "Read running processes"),
            procs["total"].as_u64().is_some_and(|n| n > 0)
                && (procs["error"].is_null() || procs["error"] == ""),
            procs.to_string(),
        ),
        (
            Text::new("识别正在使用的文件", "Identify files in use"),
            procs["files"].as_u64().is_some_and(|n| n > 0),
            procs["files"].to_string(),
        ),
    ] {
        // 超时诊断必须完整保留最后状态，不套用普通检查的 300 字符截断。
        lock(state).results.push(json!({
            "check": name.text(),
            "ok": finished && ok,
            "detail": timeout_detail.as_ref().unwrap_or(&detail),
        }));
    }
}

fn with_writes<T>(
    state: &Shared,
    action: impl FnOnce(&mut Option<ServerHandle>) -> io::Result<T>,
) -> Result<T, String> {
    let mut resources = state
        .resources
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    checkpoint(state)?;
    action(&mut resources).map_err(|error| error.to_string())
}

/// 监督方等待清理结果，不能让工作线程或操作系统的卡顿无限推迟超时报告。
fn cleanup(state: &Shared, temporary: Temporary, budget: Duration) -> bool {
    state.canceled.store(true, Ordering::Release);
    let path = temporary.0.clone();
    let cleanup_state = Arc::clone(state);
    let lang = i18n::current();
    let (tx, rx) = mpsc::channel();
    let started = Instant::now();
    let spawned = thread::Builder::new()
        .name("diskmap-selftest-cleanup".into())
        .spawn(move || {
            let _scope = i18n::scope(lang);
            let mut server = cleanup_state
                .resources
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take();
            // 取消位已经发布；取得锁后再也没有能通过 with_writes 的写入或实例登记。
            if let Some(server) = server.as_mut() {
                server.shutdown();
            }
            drop(server);
            let instance_removed = !temporary.0.join("cache/instance.json").exists();
            let removed = remove_with_retry(&temporary.0, CLEANUP_RETRY);
            let _ = tx.send((instance_removed, removed));
        });
    let result = match spawned {
        Ok(_) => match rx.recv_timeout(budget.saturating_sub(started.elapsed())) {
            Ok((instance_removed, removed)) => {
                check(
                    state,
                    Text::new("实例登记清理", "Instance registration cleanup"),
                    instance_removed,
                    "",
                );
                removed.map_err(|error| error.to_string())
            }
            Err(mpsc::RecvTimeoutError::Timeout) => Err(Text::owned(
                format!("清理超过 {} 秒；工作线程或系统调用尚未返回，临时目录可能保留", budget.as_secs_f64()),
                format!("Cleanup exceeded {} seconds; a worker or system call has not returned, so the temporary folder may remain", budget.as_secs_f64()),
            ).to_string()),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(Text::new(
                "清理线程未返回结果，临时目录可能保留",
                "The cleanup worker returned no result; the temporary folder may remain",
            ).to_string()),
        },
        Err(error) => Err(error.to_string()),
    };
    let ok = result.is_ok();
    // 路径单独保留，不受 detail 的旧版 300 字符截断限制；失败时可据此手动清理。
    lock(state).results.push(json!({
        "check": Text::new("临时目录清理", "Temporary folder cleanup").text(),
        "ok": ok,
        "path": path,
        "detail": result.err().unwrap_or_default(),
    }));
    ok
}

fn remove_with_retry(path: &Path, budget: Duration) -> io::Result<()> {
    let started = Instant::now();
    loop {
        match fs::remove_dir_all(path) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) if started.elapsed() >= budget => return Err(error),
            Err(_) => {
                // Windows 的扫描/HTTP 线程可能短暂持有目录句柄；只重试自己的临时树。
                thread::sleep(
                    Duration::from_millis(20).min(budget.saturating_sub(started.elapsed())),
                );
            }
        }
    }
}

fn request(
    state: &Shared,
    ep: &Endpoint,
    method: &str,
    path: &str,
    params: &[(&str, String)],
    body: Option<&Value>,
) -> Result<Vec<u8>, String> {
    request_with_timeout(state, ep, method, path, params, body, REQUEST_TIMEOUT)
}

fn request_with_timeout(
    state: &Shared,
    ep: &Endpoint,
    method: &str,
    path: &str,
    params: &[(&str, String)],
    body: Option<&Value>,
    timeout: Duration,
) -> Result<Vec<u8>, String> {
    checkpoint(state)?;
    lock(state).step = path.split('?').next().unwrap_or(path).to_owned();
    let (status, bytes) = ep.request_raw(method, path, params, body, timeout)?;
    if status != 200 {
        return Err(Text::owned(
            format!("HTTP {status}：{}", String::from_utf8_lossy(&bytes)),
            format!("HTTP {status}: {}", String::from_utf8_lossy(&bytes)),
        )
        .to_string());
    }
    Ok(bytes)
}

fn request_json(
    state: &Shared,
    ep: &Endpoint,
    method: &str,
    path: &str,
    params: &[(&str, String)],
    body: Option<&Value>,
) -> Result<Value, String> {
    serde_json::from_slice(&request(state, ep, method, path, params, body)?)
        .map_err(|e| e.to_string())
}

fn mcp_roundtrip(state: &Shared, root: &Path, expected: u64) -> Result<(), String> {
    checkpoint(state)?;
    lock(state).step = "MCP".into();
    let requests = [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"selftest","version":"1"}}}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"diskmap_scan","arguments":{"path":root,"wait_seconds":60,"apparent":true}}}),
        json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"diskmap_explain","arguments":{"path":root.join("big/data.bin")}}}),
        json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"diskmap_explain","arguments":{"path":root.join("does-not-exist")}}}),
    ];
    let mut input = Vec::new();
    for request in requests {
        serde_json::to_writer(&mut input, &request).map_err(|e| e.to_string())?;
        input.push(b'\n');
    }
    let mut output = Vec::new();
    crate::mcp::serve_io(
        CancellableInput {
            input: Cursor::new(input),
            state,
        },
        &mut output,
        false,
    )
    .map_err(|e| e.to_string())?;
    checkpoint(state)?;
    let replies = output
        .split(|&c| c == b'\n')
        .filter(|line| !line.is_empty())
        .map(serde_json::from_slice)
        .collect::<Result<Vec<Value>, _>>()
        .map_err(|e| e.to_string())?;
    let result = |id: u64| {
        replies
            .iter()
            .find(|r| r["id"] == id)
            .map(|r| &r["result"])
            .unwrap_or(&Value::Null)
    };
    let init = result(1);
    check(
        state,
        Text::new("MCP 初始化", "MCP initialization"),
        init["serverInfo"]["name"] == "diskmap" && init["protocolVersion"] == "2025-06-18",
        &init["protocolVersion"],
    );
    let tools = &result(2)["tools"];
    check(
        state,
        Text::new("MCP 工具列表", "MCP tool list"),
        tools.as_array().is_some_and(|a| a.len() >= 9),
        tools,
    );
    let scan = result(3);
    check(
        state,
        Text::new("MCP 扫描", "MCP scan"),
        scan["isError"] != true && scan["structuredContent"]["summary"]["bytes"] == expected,
        &scan["content"][0]["text"],
    );
    let explanation = result(4);
    check(
        state,
        Text::new("MCP 解释文件", "MCP explain file"),
        explanation["structuredContent"]["bytes"] == 300_000,
        &explanation["content"][0]["text"],
    );
    let error = result(5);
    check(
        state,
        Text::new("MCP 错误处理", "MCP error handling"),
        error["isError"] == true,
        &error["content"][0]["text"],
    );
    check(
        state,
        Text::new("MCP 正常退出", "MCP clean exit"),
        replies.len() == 5,
        replies.len(),
    );
    Ok(())
}

fn checkpoint(state: &Shared) -> Result<(), String> {
    if state.canceled.load(Ordering::Acquire) {
        Err(Text::new("自检已取消", "The self-test was canceled").to_string())
    } else {
        Ok(())
    }
}

struct CancellableInput<'a> {
    input: Cursor<Vec<u8>>,
    state: &'a Shared,
}

impl Read for CancellableInput<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.state.canceled.load(Ordering::Acquire) {
            Ok(0)
        } else {
            self.input.read(buffer)
        }
    }
}

impl BufRead for CancellableInput<'_> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        if self.state.canceled.load(Ordering::Acquire) {
            Ok(&[])
        } else {
            self.input.fill_buf()
        }
    }

    fn consume(&mut self, amount: usize) {
        self.input.consume(amount);
    }
}

struct Temporary(PathBuf);

impl Temporary {
    fn new() -> io::Result<Self> {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        for index in 0..100 {
            let path = std::env::temp_dir().join(format!(
                "diskmap-selftest-{}-{stamp}-{index}",
                std::process::id()
            ));
            let builder = fs::DirBuilder::new();
            #[cfg(unix)]
            let builder = {
                use std::os::unix::fs::DirBuilderExt;
                let mut builder = builder;
                builder.mode(0o700);
                builder
            };
            match builder.create(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            Text::new(
                "无法创建自检临时目录",
                "Cannot create the self-test temporary folder",
            )
            .to_string(),
        ))
    }
}

#[cfg(test)]
mod process_refresh_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleanup_waits_for_active_writer_and_blocks_late_recreation() {
        let temporary = Temporary::new().unwrap();
        let path = temporary.0.clone();
        let state = Arc::new(State::default());
        let worker_state = Arc::clone(&state);
        let worker_path = path.clone();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let writer = thread::spawn(move || {
            with_writes(&worker_state, |_| {
                fs::create_dir(worker_path.join("in-flight"))?;
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                fs::write(worker_path.join("in-flight/file"), b"fixture")
            })
            .unwrap();
            // 模拟已通过外层检查、到清理开始后才拿到锁的下一步写入。
            assert!(
                with_writes(&worker_state, |_| {
                    fs::create_dir_all(worker_path.join("must-not-reappear"))
                })
                .is_err()
            );
        });
        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let cleanup_state = Arc::clone(&state);
        let cleaner = thread::spawn(move || cleanup(&cleanup_state, temporary, CLEANUP_TIMEOUT));
        let deadline = Instant::now() + Duration::from_secs(1);
        while !state.canceled.load(Ordering::Acquire) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }
        assert!(state.canceled.load(Ordering::Acquire));
        assert!(path.exists(), "写入尚未离开临界区，不能提前删除");
        release_tx.send(()).unwrap();
        writer.join().unwrap();
        assert!(cleaner.join().unwrap());
        assert!(!path.exists());
    }

    #[test]
    fn blocked_cleanup_is_bounded_and_reports_the_full_path() {
        let temporary = Temporary::new().unwrap();
        let path = temporary.0.clone();
        let state = Arc::new(State::default());
        let guard = state.resources.lock().unwrap();
        let started = Instant::now();
        assert!(!cleanup(&state, temporary, Duration::from_millis(20)));
        assert!(started.elapsed() < Duration::from_secs(1));
        let results = lock(&state).results.clone();
        let failure = results.last().unwrap();
        assert_eq!(failure["ok"], false);
        assert_eq!(failure["path"], json!(path));
        assert!(!failure["detail"].as_str().unwrap().is_empty());
        assert!(path.exists());
        drop(guard);
        // 库宿主继续存活时，迟到的清理仍能完成；不会重置取消位允许工作线程重建。
        let deadline = Instant::now() + Duration::from_secs(2);
        while path.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        assert!(!path.exists());
        assert!(with_writes(&state, |_| fs::create_dir_all(&path)).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn cleanup_retries_until_windows_file_handle_is_released() {
        use std::os::windows::fs::OpenOptionsExt;
        let temporary = Temporary::new().unwrap();
        let file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .share_mode(0)
            .open(temporary.0.join("held-open"))
            .unwrap();
        let release = thread::spawn(move || {
            thread::sleep(Duration::from_millis(60));
            drop(file);
        });
        remove_with_retry(&temporary.0, CLEANUP_RETRY).unwrap();
        release.join().unwrap();
        assert!(!temporary.0.exists());
    }
}
