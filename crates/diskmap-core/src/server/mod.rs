//! 仅监听回环地址的本地 HTTP 服务；路由、认证及响应字段与 Python 版一致。
//!
//! 界面文件通过 `include_bytes!` 编译进程序，静态请求不访问磁盘。`ServerHandle`
//! 释放时停止接收请求、取消扫描并清除属于当前进程的实例登记。

pub mod instance;

mod agent;
mod catalog;
mod saved;
mod transport;

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::io::{self, Read};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use transport::{Header, Method, Request, Response, StatusCode};

use crate::app::App;
use crate::i18n::{self, Text};
use crate::platform::{self, Os};

const INDEX: &str = include_str!("../../../../web/index.html");
const BAD_LINK: &str = "<meta charset=utf-8><p style='font:16px sans-serif;padding:2em'>请使用 DiskMap 启动时输出的完整链接打开（链接里包含访问令牌）。<br>Please open the full link printed when DiskMap started (it contains the access token).</p>";
const JSON_TYPE: &str = "application/json; charset=utf-8";
const HTML_TYPE: &str = "text/html; charset=utf-8";
const MAX_BODY: usize = 1_000_000;
const MAX_CONNECTIONS: usize = 64;

/// 启动选项；实例及设置路径可注入，便于不接触用户数据的自动测试。
#[derive(Clone, Debug)]
pub struct ServeOptions {
    /// 0 表示随机端口；指定端口被占用时与旧版一样回退到随机端口。
    pub port: u16,
    /// 未提供或为空时从操作系统生成随机令牌。
    pub token: Option<String>,
    /// `None` 使用平台缓存目录中的 `instance.json`。
    pub instance_path: Option<PathBuf>,
    /// `None` 使用平台设置目录中的 `settings.json`。
    pub settings_path: Option<PathBuf>,
    /// 嵌入程序可传入可实际运行的 MCP 命令；缺省为 Linux 的有效 AppImage 路径或当前可执行文件加 `mcp`。
    pub agent_command: Option<Vec<String>>,
    /// 启动后登记，关闭时删除；测试或无窗口调用方可以关闭登记。
    pub register_instance: bool,
}

impl Default for ServeOptions {
    fn default() -> Self {
        Self {
            port: 0,
            token: None,
            instance_path: None,
            settings_path: None,
            agent_command: None,
            register_instance: true,
        }
    }
}

/// 服务句柄；应由窗口或命令行入口持有到用户退出。
pub struct ServerHandle {
    pub app: Arc<App>,
    pub port: u16,
    pub token: String,
    pub url: String,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    instance_path: Option<PathBuf>,
}

impl ServerHandle {
    /// 可重复调用；不会等待不完整的外部请求，也不会删除其他进程的新登记。
    pub fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.thread.take() {
            // 阻塞 accept 无轮询延迟；本机短连接只负责唤醒，不进入请求解析。
            let address = ([127, 0, 0, 1], self.port).into();
            let _ = TcpStream::connect_timeout(&address, Duration::from_secs(1));
            let _ = handle.join();
        }
        self.app.shutdown();
        if let Some(path) = self.instance_path.take() {
            instance::remove_at(&path);
        }
    }
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// 启动后台初始化和 HTTP 服务，返回带访问令牌的地址。
pub fn serve(options: ServeOptions) -> io::Result<ServerHandle> {
    serve_with_app(options, Arc::new(App::new()))
}

/// 路径和文件检查结果由调用方传入，选择逻辑不读取环境变量或文件系统。
#[cfg(target_os = "linux")]
fn appimage_executable(path: Option<PathBuf>, is_file: bool) -> Option<PathBuf> {
    path.filter(|path| path.is_absolute() && is_file)
}

/// 使用已有应用状态启动服务；`App` 可由多个调用方共享。
pub fn serve_with_app(options: ServeOptions, app: Arc<App>) -> io::Result<ServerHandle> {
    let token = match options.token.filter(|token| !token.is_empty()) {
        Some(token) => token,
        None => random_token()?,
    };
    let listener = TcpListener::bind(("127.0.0.1", options.port))
        .or_else(|_| TcpListener::bind(("127.0.0.1", 0)))?;
    let port = listener.local_addr()?.port();
    if let Some(cache_dir) = options
        .instance_path
        .as_deref()
        .and_then(|path| path.parent())
    {
        app.use_instance_cache_dir(cache_dir);
    }
    let stop = Arc::new(AtomicBool::new(false));
    let context = Arc::new(Context {
        stop: Arc::clone(&stop),
        app: Arc::clone(&app),
        token: token.clone(),
        port,
        settings_path: options.settings_path.unwrap_or_else(i18n::settings_path),
        agent_command: options.agent_command.unwrap_or_else(|| {
            #[cfg(target_os = "linux")]
            let appimage = {
                let path = std::env::var_os("APPIMAGE").map(PathBuf::from);
                let is_file = path.as_ref().is_some_and(|path| path.is_file());
                appimage_executable(path, is_file)
            };
            #[cfg(not(target_os = "linux"))]
            let appimage: Option<PathBuf> = None;

            vec![
                appimage
                    .or_else(|| std::env::current_exe().ok())
                    .unwrap_or_else(|| PathBuf::from("DiskMap"))
                    .to_string_lossy()
                    .into_owned(),
                "mcp".into(),
            ]
        }),
    });
    let stopping = Arc::clone(&stop);
    let active = Arc::new(AtomicUsize::new(0));
    let handle = thread::Builder::new()
        .name("diskmap-http".into())
        .spawn(move || {
            while !stopping.load(Ordering::Acquire) {
                let stream = match listener.accept() {
                    Ok((stream, _)) => stream,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                };
                if stopping.load(Ordering::Acquire) {
                    break;
                }
                if active.load(Ordering::Acquire) >= MAX_CONNECTIONS {
                    let _ = stream.shutdown(Shutdown::Both);
                    continue;
                }
                let context = Arc::clone(&context);
                active.fetch_add(1, Ordering::AcqRel);
                let count = ConnectionCount(Arc::clone(&active));
                // 解析请求头也在受限工作线程中，不让慢客户端占住接收循环。
                let _ = thread::Builder::new()
                    .name("diskmap-request".into())
                    .spawn(move || {
                        let _count = count;
                        if let Ok(request) = Request::read(stream) {
                            handle_request(request, &context);
                        }
                    });
            }
        })?;
    let instance_path = options
        .register_instance
        .then(|| options.instance_path.unwrap_or_else(instance::path));
    if let Some(path) = &instance_path {
        // 与旧版一样，缓存目录不可写不会阻止窗口使用。
        let _ = instance::write_at(path, port, &token, crate::VERSION);
    }
    Ok(ServerHandle {
        app,
        port,
        url: format!("http://127.0.0.1:{port}/?t={}", encode_query(&token)),
        token,
        stop,
        thread: Some(handle),
        instance_path,
    })
}

struct ConnectionCount(Arc<AtomicUsize>);

impl Drop for ConnectionCount {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

struct Context {
    stop: Arc<AtomicBool>,
    app: Arc<App>,
    token: String,
    port: u16,
    settings_path: PathBuf,
    agent_command: Vec<String>,
}

fn handle_request(mut request: Request, context: &Context) {
    if context.stop.load(Ordering::Acquire) {
        let _ = request.respond(response(503, json!({"error": "server stopped"})));
        return;
    }
    let lang = header(&request, "X-DiskMap-Lang").and_then(i18n::norm);
    let _language = i18n::scope_optional(lang);
    let (path, query) = split_url(request.url());
    let is_agent = path.starts_with("/api/agent/");
    let method = request.method().clone();
    if method == Method::Post && path == "/api/bye" {
        let answer = if host_ok(&request, context.port)
            && token_eq(query.get("t").map_or("", String::as_str), &context.token)
        {
            context.app.say_bye();
            bytes_response(204, Vec::new(), JSON_TYPE)
        } else {
            response(403, json!({"error": "bad token"}))
        };
        let _ = request.respond(answer);
        return;
    }
    if !is_agent && matches!(method, Method::Get | Method::Post) {
        context.app.touch_ui();
    }
    let answer = match method {
        Method::Get => get(&request, context, &path, &query),
        Method::Post => post(&mut request, context, &path),
        _ => response(501, json!({"error": "unsupported method"})),
    };
    let _ = request.respond(answer);
}

type HttpResponse = Response;

fn get(
    request: &Request,
    context: &Context,
    path: &str,
    query: &HashMap<String, String>,
) -> HttpResponse {
    if path == "/" {
        if !host_ok(request, context.port) {
            return response(403, json!({"error": "bad host"}));
        }
        if !token_eq(query.get("t").map_or("", String::as_str), &context.token) {
            return bytes_response(403, BAD_LINK.as_bytes().to_vec(), HTML_TYPE);
        }
        let lang = i18n::current().code();
        // JSON 字符串转义可安全接受测试令牌里的引号、反斜杠及 HTML 结束标记。
        let escaped = serde_json::to_string(&context.token)
            .expect("string serialization")
            .replace('<', "\\u003c")
            .replace('>', "\\u003e")
            .replace('&', "\\u0026");
        let html = INDEX
            .replace("\"__DISKMAP_TOKEN__\"", &escaped)
            .replace("__DISKMAP_LANG__", lang)
            .replace(
                "<html lang=\"zh-CN\">",
                &format!(
                    "<html lang=\"{}\" data-lang=\"{lang}\">",
                    if lang == "en" { "en" } else { "zh-CN" }
                ),
            );
        return bytes_response(200, html.into_bytes(), HTML_TYPE).with_header(
            Header::from_bytes("Referrer-Policy", "no-referrer").expect("fixed header"),
        );
    }
    if path.starts_with("/static/") {
        // 旧版静态资源是认证例外，且只按 basename 查找；嵌入表根本不访问文件系统。
        return match asset(path.rsplit('/').next().unwrap_or_default()) {
            Some((data, content_type)) => bytes_response(200, data.to_vec(), content_type),
            None => response(404, json!({"error": "not found"})),
        };
    }
    if !path.starts_with("/api/") {
        return response(404, json!({"error": "not found"}));
    }
    if let Some(answer) = authenticate(request, context) {
        return answer;
    }
    if path.starts_with("/api/agent/") {
        context.app.touch_agent();
        return agent::get(&context.app, path, query);
    }
    match path {
        "/api/meta" => response(200, meta(context)),
        "/api/status" => response(200, context.app.status()),
        "/api/tree" => {
            let frac = match tree_frac(query) {
                Ok(frac) => frac,
                Err(error) => return response(400, json!({"error": error})),
            };
            let started = Instant::now();
            let tree = context
                .app
                .tree_bytes(query.get("path").map_or("", String::as_str), frac);
            let elapsed = started.elapsed();
            if frac.is_nan() && tree.is_some() {
                response(400, json!({"error": "cannot convert float NaN to integer"}))
            } else if let Some(bytes) = tree {
                common_headers(Response::from_shared(bytes), JSON_TYPE).with_header(
                    Header::from_bytes(
                        "Server-Timing",
                        &format!("tree;dur={:.3}", elapsed.as_secs_f64() * 1000.0),
                    )
                    .expect("timing header"),
                )
            } else {
                optional_result(None)
            }
        }
        "/api/info" => optional_result(
            context
                .app
                .info(query.get("path").map_or("", String::as_str)),
        ),
        "/api/aggregate" => catalog::aggregate(&context.app, query),
        "/api/largest" => optional_result(
            context
                .app
                .largest_files(query.get("path").map_or("", String::as_str))
                .map(|files| json!({"files": files})),
        ),
        "/api/search" => catalog::search(&context.app, query),
        "/api/saved" => saved::list(&context.app),
        _ => response(404, json!({"error": "not found"})),
    }
}

fn tree_frac(query: &HashMap<String, String>) -> Result<f64, String> {
    agent::parse_float(query.get("frac").map_or("0.000025", String::as_str))
        .map(|frac| frac.clamp(1e-6, 0.05))
}

fn post(request: &mut Request, context: &Context, path: &str) -> HttpResponse {
    if let Some(answer) = authenticate(request, context) {
        return answer;
    }
    let body = read_body(request);
    if context.stop.load(Ordering::Acquire) {
        return response(503, json!({"error": "server stopped"}));
    }
    if !body.is_object() && matches!(path, "/api/ui-state" | "/api/scan" | "/api/reveal") {
        return missing_get(&body);
    }
    match path {
        "/api/agent/scan" => {
            context.app.touch_agent();
            if !body.is_object() {
                return missing_get(&body);
            }
            let path = body.get("path").filter(|value| truthy(value));
            result(context.app.agent_scan(
                &path.map_or_else(String::new, python_str),
                body.get("rescan").is_some_and(truthy),
                body.get("apparent").is_some_and(truthy),
                !body.get("same_fs").is_some_and(truthy),
            ))
        }
        "/api/agent/show" => {
            context.app.touch_agent();
            if !body.is_object() {
                return missing_get(&body);
            }
            agent::show(&context.app, &body)
        }
        "/api/settings" => {
            match &body {
                Value::Array(items) if items.iter().any(|item| item == "lang") => {
                    return missing_get(&body);
                }
                Value::String(value) if value.contains("lang") => return missing_get(&body),
                Value::Null | Value::Bool(_) | Value::Number(_) => {
                    return response(
                        500,
                        json!({"error": format!("TypeError(\"argument of type '{}' is not a container or iterable\")", python_type(&body))}),
                    );
                }
                _ => {}
            }
            if let Some(lang) = body.get("lang") {
                let lang = python_str(lang);
                if i18n::norm(&lang).is_none() {
                    return response(
                        400,
                        json!({"error": format!("unsupported language: {lang}")}),
                    );
                }
                let _ = i18n::save_lang_at(&context.settings_path, &lang);
                context.app.invalidate_tree_responses();
            }
            response(200, json!({"lang": i18n::current()}))
        }
        "/api/ui-state" => {
            context.app.set_ui_state(body);
            response(200, json!({"ok": true}))
        }
        "/api/scan" => {
            let path = body.get("path").filter(|value| truthy(value));
            result(context.app.start_scan(
                &path.map_or_else(String::new, python_str),
                body.get("apparent").is_some_and(truthy),
                body.get("cross_fs") != Some(&Value::Bool(false)),
            ))
        }
        "/api/cancel" => response(200, context.app.cancel()),
        "/api/saved/open" => saved::open(&context.app, &body),
        "/api/saved/delete" => saved::delete(&context.app, &body),
        "/api/refresh-procs" => response(200, context.app.refresh_procs()),
        "/api/reveal" => match context
            .app
            .reveal(&body.get("path").map_or_else(String::new, python_str))
        {
            Ok(()) => response(200, json!({"ok": true})),
            Err(error) => response(400, json!({"error": error})),
        },
        _ => response(404, json!({"error": "not found"})),
    }
}

fn meta(context: &Context) -> Value {
    let disks: Vec<Value> = platform::mounts::disk_mounts()
        .into_iter()
        .map(|disk| json!({"path": disk.path, "fstype": disk.fstype, "source": disk.source,
            "system": disk.system, "total": disk.usage.total, "used": disk.usage.used, "free": disk.usage.free}))
        .collect();
    json!({
        "classes": crate::classify::CLASSES,
        "home": platform::home_dir().unwrap_or_else(|| PathBuf::from("~")),
        "disks": disks,
        "os": Os::current().key(),
        "sep": Os::current().sep().to_string(),
        "system_root": platform::system_root(),
        "admin": platform::is_admin(),
        "version": crate::VERSION,
        "agent": agent_info(&context.agent_command),
        "lang": i18n::current(),
    })
}

/// 生成 AI 面板的命令与 JSON 配置；参数是完整的 MCP 命令（包括 `mcp`）。
pub fn agent_info(command: &[String]) -> Value {
    let fallback = vec!["DiskMap".to_owned(), "mcp".to_owned()];
    let command = if command.is_empty() {
        &fallback
    } else {
        command
    };
    let config = json!({"mcpServers": {"diskmap": {"command": command[0], "args": command[1..]}}});
    let mut report = command[..command.len().saturating_sub(1)].to_vec();
    report.push("analyze".into());
    report.push(platform::system_root().to_string_lossy().into_owned());
    json!({
        "command": command,
        "claude_code": format!("claude mcp add --scope user diskmap -- {}", shell_join(command)),
        "json": serde_json::to_string_pretty(&config).expect("serializable configuration"),
        "report": shell_join(&report),
    })
}

fn shell_join(command: &[String]) -> String {
    command
        .iter()
        .map(|arg| shell_quote(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_quote(arg: &str) -> String {
    if cfg!(windows) {
        // 与 Python subprocess.list2cmdline 相同的 CRT 引号和反斜杠规则。
        let quote = arg.is_empty() || arg.contains([' ', '\t']);
        let mut out = String::new();
        if quote {
            out.push('"');
        }
        let mut slashes = 0;
        for ch in arg.chars() {
            if ch == '\\' {
                slashes += 1;
                continue;
            }
            if ch == '"' {
                out.extend(std::iter::repeat_n('\\', slashes * 2 + 1));
            } else {
                out.extend(std::iter::repeat_n('\\', slashes));
            }
            slashes = 0;
            out.push(ch);
        }
        out.extend(std::iter::repeat_n('\\', slashes));
        if quote {
            out.extend(std::iter::repeat_n('\\', slashes));
            out.push('"');
        }
        out
    } else if !arg.is_empty()
        && arg
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || "_@%+=:,./-".contains(ch))
    {
        arg.to_owned()
    } else {
        format!("'{}'", arg.replace('\'', "'\"'\"'"))
    }
}

fn asset(name: &str) -> Option<(&'static [u8], &'static str)> {
    match name {
        "index.html" => Some((INDEX.as_bytes(), HTML_TYPE)),
        "app.js" => Some((
            include_bytes!("../../../../web/app.js"),
            "text/javascript; charset=utf-8",
        )),
        "style.css" => Some((
            include_bytes!("../../../../web/style.css"),
            "text/css; charset=utf-8",
        )),
        "favicon.png" => Some((include_bytes!("../../../../web/favicon.png"), "image/png")),
        _ => None,
    }
}

fn result(value: Result<Value, Text>) -> HttpResponse {
    match value {
        Ok(value) => response(200, value),
        Err(error) => response(400, json!({"error": error})),
    }
}

fn optional_result(value: Option<Value>) -> HttpResponse {
    match value {
        Some(value) => response(200, value),
        None => response(
            404,
            json!({"error": Text::new("没有这个路径的扫描结果", "No scan results for this path")}),
        ),
    }
}

fn authenticate(request: &Request, context: &Context) -> Option<HttpResponse> {
    if !host_ok(request, context.port) {
        Some(response(403, json!({"error": "bad host"})))
    } else if !token_eq(
        header(request, "X-DiskMap-Token").unwrap_or(""),
        &context.token,
    ) {
        Some(response(403, json!({"error": "bad token"})))
    } else {
        None
    }
}

fn header<'a>(request: &'a Request, name: &'static str) -> Option<&'a str> {
    request
        .headers()
        .iter()
        .find(|header| header.name.eq_ignore_ascii_case(name))
        .map(|header| header.value.as_str())
}

fn host_ok(request: &Request, port: u16) -> bool {
    let Some(host) = header(request, "Host") else {
        return false;
    };
    host == format!("127.0.0.1:{port}")
        || host == format!("localhost:{port}")
        || host == format!("[::1]:{port}")
}

fn token_eq(received: &str, expected: &str) -> bool {
    if received.len() != expected.len() {
        return false;
    }
    received
        .as_bytes()
        .iter()
        .zip(expected.as_bytes())
        .fold(0_u8, |diff, (left, right)| diff | (left ^ right))
        == 0
}

fn read_body(request: &mut Request) -> Value {
    let Some(length) =
        header(request, "Content-Length").and_then(|length| length.trim().parse::<usize>().ok())
    else {
        return json!({});
    };
    if length == 0 || length > MAX_BODY {
        return json!({});
    }
    let mut data = Vec::with_capacity(length);
    if request
        .as_reader()
        .take(length as u64)
        .read_to_end(&mut data)
        .is_err()
    {
        return json!({});
    }
    serde_json::from_slice(&data).unwrap_or_else(|_| json!({}))
}

fn response(code: u16, body: Value) -> HttpResponse {
    bytes_response(
        code,
        serde_json::to_vec(&body).expect("serializable response"),
        JSON_TYPE,
    )
}

fn bytes_response(code: u16, body: Vec<u8>, content_type: &str) -> HttpResponse {
    common_headers(
        Response::from_data(body).with_status_code(StatusCode(code)),
        content_type,
    )
}

fn common_headers(response: Response, content_type: &str) -> HttpResponse {
    response
        .with_header(Header::from_bytes("Content-Type", content_type).expect("content type"))
        .with_header(Header::from_bytes("Cache-Control", "no-store").expect("fixed header"))
        .with_header(Header::from_bytes("X-Content-Type-Options", "nosniff").expect("fixed header"))
}

fn split_url(url: &str) -> (String, HashMap<String, String>) {
    let url = url.split('#').next().unwrap_or(url);
    let (path, query) = url.split_once('?').unwrap_or((url, ""));
    let mut values = HashMap::new();
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        // Python parse_qs 默认不保留空值，并只使用第一个非空值。
        if !value.is_empty() {
            values
                .entry(decode_query(key))
                .or_insert_with(|| decode_query(value));
        }
    }
    (path.to_owned(), values)
}

fn decode_query(value: &str) -> String {
    let input = value.as_bytes();
    let mut output = Vec::with_capacity(input.len());
    let mut index = 0;
    while index < input.len() {
        if input[index] == b'%'
            && index + 2 < input.len()
            && let (Some(a), Some(b)) = (hex(input[index + 1]), hex(input[index + 2]))
        {
            output.push(a * 16 + b);
            index += 3;
            continue;
        }
        output.push(if input[index] == b'+' {
            b' '
        } else {
            input[index]
        });
        index += 1;
    }
    String::from_utf8_lossy(&output).into_owned()
}

fn hex(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn encode_query(value: &str) -> String {
    let mut output = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.~".contains(&byte) {
            output.push(char::from(byte));
        } else {
            use std::fmt::Write;
            write!(output, "%{byte:02X}").expect("string write");
        }
    }
    output
}

fn random_token() -> io::Result<String> {
    let mut bytes = [0_u8; 18];
    getrandom::fill(&mut bytes).map_err(|error| io::Error::other(error.to_string()))?;
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut token = String::with_capacity(24);
    for bytes in bytes.as_chunks::<3>().0 {
        let value = (u32::from(bytes[0]) << 16) | (u32::from(bytes[1]) << 8) | u32::from(bytes[2]);
        for shift in [18, 12, 6, 0] {
            token.push(char::from(ALPHABET[((value >> shift) & 63) as usize]));
        }
    }
    Ok(token)
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64() != Some(0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
    }
}

fn missing_get(body: &Value) -> HttpResponse {
    response(
        500,
        json!({"error": format!("AttributeError(\"'{}' object has no attribute 'get'\")", python_type(body))}),
    )
}

fn python_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(value) if value.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

fn python_str(value: &Value) -> String {
    match value {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::String(value) => value.clone(),
        Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(python_value_repr)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Object(values) => format!(
            "{{{}}}",
            values
                .iter()
                .map(|(key, value)| format!("{}: {}", python_repr(key), python_value_repr(value)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Number(value) => value.to_string(),
    }
}

fn python_value_repr(value: &Value) -> String {
    match value {
        Value::String(value) => python_repr(value),
        _ => python_str(value),
    }
}

fn python_repr(value: &str) -> String {
    let quote = if value.contains('\'') && !value.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut output = String::from(quote);
    for ch in value.chars() {
        match ch {
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            ch if ch == quote => {
                output.push('\\');
                output.push(ch);
            }
            ch => output.push(ch),
        }
    }
    output.push(quote);
    output
}
