//! 按行 JSON-RPC MCP 服务；本进程分析与已打开窗口共用同一套工具语义。

use std::io::{self, BufRead, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use diskmap_core::{
    analysis,
    app::App,
    i18n::{self, Text},
    platform,
    server::instance,
};
use serde_json::{Value, json};

use crate::http::Endpoint;
use crate::mcp_tools::{INSTRUCTIONS, NAMES, PROTOCOLS, tools};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) struct Server {
    backend: Backend,
}

struct Backend {
    attach: bool,
    local: OnceLock<App>,
    closed: AtomicBool,
}

#[derive(Clone)]
enum Target {
    Window(Endpoint),
    Background,
}

impl Target {
    fn source(&self) -> &'static str {
        match self {
            Self::Window(_) => "window",
            Self::Background => "background",
        }
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl Backend {
    fn shutdown(&self) {
        self.closed.store(true, Ordering::Release);
        if let Some(app) = self.local.get() {
            app.shutdown();
        }
    }

    fn local(&self) -> &App {
        let app = self.local.get_or_init(App::new);
        // 初始化可能与 EOF 后的清理同时发生；迟到的初始化同样不能再启动扫描。
        if self.closed.load(Ordering::Acquire) {
            app.shutdown();
        }
        app
    }

    fn gui(&self) -> Option<(Endpoint, Value)> {
        if !self.attach || self.closed.load(Ordering::Acquire) {
            return None;
        }
        let info = instance::read()?;
        let ep = Endpoint {
            port: u16::try_from(info.get("port")?.as_u64()?).ok()?,
            token: info.get("token")?.as_str()?.to_owned(),
        };
        let status = ep
            .request(
                "GET",
                "/api/agent/status",
                &[],
                None,
                Duration::from_secs(2),
            )
            .ok()?;
        if !status.is_object() {
            return None;
        }
        Some((ep, status))
    }

    fn endpoint(&self) -> Target {
        if self.closed.load(Ordering::Acquire) {
            return Target::Background;
        }
        let gui = self.gui();
        let local_has_scan = self
            .local
            .get()
            .is_some_and(|app| app.current_scan().is_some());
        if let Some((endpoint, status)) = gui
            && (status.get("scan").is_some_and(truthy) || !local_has_scan)
        {
            return Target::Window(endpoint);
        }
        self.local();
        Target::Background
    }

    fn request(
        &self,
        target: &Target,
        method: &str,
        route: &str,
        params: &[(&str, String)],
        body: Option<&Value>,
    ) -> Result<Value, String> {
        if self.closed.load(Ordering::Acquire) {
            return Err(Text::new("服务已关闭", "The service is closed").to_string());
        }
        if let Target::Window(ep) = target {
            return ep.request(method, route, params, body, REQUEST_TIMEOUT);
        }
        let app = self.local();
        app.touch_agent();
        if method == "POST" {
            let body = body.unwrap_or(&Value::Null);
            return app
                .agent_scan(
                    &body
                        .get("path")
                        .filter(|v| truthy(v))
                        .map_or_else(String::new, python_str),
                    body.get("rescan").is_some_and(truthy),
                    body.get("apparent").is_some_and(truthy),
                    !body.get("same_fs").is_some_and(truthy),
                )
                .map_err(|e| e.to_string());
        }
        local_get(app, route, params)
    }

    fn get(&self, route: &str, params: &[(&str, String)]) -> Result<Value, String> {
        self.request(&self.endpoint(), "GET", route, params, None)
    }

    fn call(&self, name: &str, args: &Value) -> Result<Value, String> {
        if self.closed.load(Ordering::Acquire) {
            return Err(Text::new("服务已关闭", "The service is closed").to_string());
        }
        // 旧版 status 和窗口工具不读取 arguments；其余工具保留 .get 的类型错误。
        let argument = |key: &str| -> Result<&Value, String> {
            if truthy(args) && !args.is_object() {
                return Err(internal(&missing_get(args)));
            }
            Ok(args.get(key).unwrap_or(&Value::Null))
        };
        let params = |keys: &[(&'static str, Option<Value>)]| -> Result<Vec<(&'static str, String)>, String> {
            let mut result = Vec::new();
            for (key, default) in keys {
                let value = argument(key)?;
                let value = if args.get(*key).is_none() { default.as_ref().unwrap_or(value) } else { value };
                if !value.is_null() { result.push((*key, python_str(value))); }
            }
            Ok(result)
        };
        match name {
            "diskmap_status" => {
                let endpoint = self.endpoint();
                let mut result = self.request(&endpoint, "GET", "/api/agent/status", &[], None)?;
                result["connected_to"] = json!(endpoint.source());
                result["connected_to_label"] = json!(match endpoint {
                    Target::Window(_) => Text::new(
                        "用户打开的 DiskMap 窗口",
                        "the DiskMap window the user has open"
                    ),
                    Target::Background => Text::new(
                        "后台扫描（用户没有打开 DiskMap 窗口，或窗口里还没有扫描）",
                        "background scan (the DiskMap window is not open, or has no scan yet)"
                    ),
                });
                Ok(result)
            }
            "diskmap_scan" => {
                let path = argument("path")?;
                let path = if truthy(path) {
                    path.clone()
                } else {
                    json!(platform::system_root())
                };
                let target = self.endpoint();
                let result = self.request(
                    &target,
                    "POST",
                    "/api/agent/scan",
                    &[],
                    Some(&json!({
                        "path": path,
                        "rescan": truthy(argument("rescan")?),
                        "apparent": truthy(argument("apparent")?),
                        "same_fs": truthy(argument("same_fs")?),
                    })),
                )?;
                let wait = argument("wait_seconds")?;
                let wait = if truthy(wait) {
                    direct_integer(wait)?
                } else {
                    120
                }
                .clamp(1, 600);
                let deadline = Instant::now() + Duration::from_secs(wait as u64);
                let mut state = Value::Null;
                while Instant::now() < deadline {
                    state = self.request(&target, "GET", "/api/agent/status", &[], None)?;
                    if state["scan"]["state"] != "scanning" {
                        break;
                    }
                    thread::sleep(Duration::from_millis(500));
                }
                let scan = state
                    .get("scan")
                    .filter(|v| truthy(v))
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                let mut output = json!({"connected_to":target.source(), "reused_existing_scan":result.get("reused").is_some_and(truthy), "scan":scan});
                if scan["state"] == "scanning" {
                    let files = python_str(&scan["files"]);
                    output["note"] = json!(Text::owned(
                        format!(
                            "{wait} 秒内还没扫完（已统计 {files} 个文件）。可以稍后再调用 diskmap_status；扫描进行中也能用 diskmap_summary 查看部分结果。"
                        ),
                        format!(
                            "Not finished within {wait} s ({files} files counted so far). Call diskmap_status later; diskmap_summary already shows partial results."
                        ),
                    ));
                } else {
                    output["summary"] = self.request(
                        &target,
                        "GET",
                        "/api/agent/summary",
                        &[("path", python_str(&path)), ("top", "8".into())],
                        None,
                    )?;
                }
                Ok(output)
            }
            "diskmap_summary" => self.get(
                "/api/agent/summary",
                &params(&[("path", None), ("top", Some(json!(10)))])?,
            ),
            "diskmap_list" => self.get(
                "/api/agent/list",
                &params(&[("path", None), ("limit", Some(json!(40)))])?,
            ),
            "diskmap_explain" => self.get("/api/agent/explain", &params(&[("path", None)])?),
            "diskmap_cleanup" => self.get(
                "/api/agent/cleanup",
                &params(&[
                    ("path", None),
                    ("limit", Some(json!(30))),
                    ("min_mb", Some(json!(1))),
                ])?,
            ),
            "diskmap_largest_files" => self.get(
                "/api/agent/largest",
                &params(&[
                    ("path", None),
                    ("limit", Some(json!(20))),
                    ("category", None),
                ])?,
            ),
            "diskmap_user_view" => {
                let (ep, _) = self.gui().ok_or_else(|| Text::new(
                    "用户没有打开 DiskMap 窗口，无法知道用户正在看什么。可以用 diskmap_summary 等工具自行分析。",
                    "The DiskMap window is not open, so what the user is looking at is unknown. Use diskmap_summary and the other tools to analyze on your own.",
                ).to_string())?;
                ep.request("GET", "/api/agent/view", &[], None, REQUEST_TIMEOUT)
            }
            "diskmap_show" => {
                let (ep, _) = self.gui().ok_or_else(|| Text::new(
                    "用户没有打开 DiskMap 窗口，无法在窗口中显示。请直接把路径告诉用户。",
                    "The DiskMap window is not open, so nothing can be shown there. Tell the user the path instead.",
                ).to_string())?;
                ep.request(
                    "POST",
                    "/api/agent/show",
                    &[],
                    Some(&json!({"path": argument("path")?, "note": argument("note")?})),
                    REQUEST_TIMEOUT,
                )
            }
            _ => unreachable!("tool names checked before dispatch"),
        }
    }
}

impl Server {
    pub(crate) fn new(attach: bool) -> Self {
        Self {
            backend: Backend {
                attach: attach
                    && std::env::var_os("DISKMAP_NO_ATTACH").is_none_or(|s| s.is_empty()),
                local: OnceLock::new(),
                closed: AtomicBool::new(false),
            },
        }
    }

    pub(crate) fn handle(&self, message: Value) -> Option<Value> {
        if !message.is_object() || message["jsonrpc"] != "2.0" {
            return message
                .get("id")
                .map(|id| rpc_error(id.clone(), -32600, "Invalid Request"));
        }
        let id = message.get("id")?.clone();
        let method = message.get("method").filter(|v| !v.is_null())?;
        let null = Value::Null;
        let params = message.get("params").filter(|v| truthy(v)).unwrap_or(&null);
        let get = |key| -> Result<&Value, String> {
            if !params.is_null() && !params.is_object() {
                return Err(missing_get(params));
            }
            Ok(params.get(key).unwrap_or(&null))
        };
        let result = match method.as_str() {
            Some("initialize") => match get("protocolVersion") {
                Ok(want) => {
                    let protocol = want
                        .as_str()
                        .filter(|s| PROTOCOLS.contains(s))
                        .unwrap_or(PROTOCOLS[0]);
                    json!({"protocolVersion":protocol,"capabilities":{"tools":{"listChanged":false}},"serverInfo":{"name":"diskmap","title":Text::new("DiskMap 磁盘地图","DiskMap"),"version":diskmap_core::VERSION},"instructions":INSTRUCTIONS})
                }
                Err(error) => return Some(rpc_error(id, -32603, &error)),
            },
            Some("ping") => json!({}),
            Some("tools/list") => json!({"tools":tools()}),
            Some("tools/call") => {
                let name = match get("name") {
                    Ok(name) => name,
                    Err(error) => return Some(rpc_error(id, -32603, &error)),
                };
                if name.is_array() || name.is_object() {
                    return Some(rpc_error(
                        id,
                        -32603,
                        &format!("TypeError(\"unhashable type: '{}'\")", python_type(name)),
                    ));
                }
                let Some(name) = name.as_str().filter(|name| NAMES.contains(name)) else {
                    let name = python_str(name);
                    return Some(rpc_error(
                        id,
                        -32602,
                        &Text::owned(format!("未知工具：{name}"), format!("Unknown tool: {name}"))
                            .to_string(),
                    ));
                };
                let arguments = params.get("arguments").unwrap_or(&null);
                match self.backend.call(name, arguments) {
                    Ok(data) => {
                        json!({"content":[{"type":"text","text":pretty(&data)}], "structuredContent":data, "isError":false})
                    }
                    Err(error) => json!({"content":[{"type":"text","text":error}], "isError":true}),
                }
            }
            Some("resources/list") => json!({"resources":[]}),
            Some("prompts/list") => json!({"prompts":[]}),
            Some("resources/templates/list") => json!({"resourceTemplates":[]}),
            _ => {
                return Some(rpc_error(
                    id,
                    -32601,
                    &format!("Method not found: {}", python_str(method)),
                ));
            }
        };
        Some(json!({"jsonrpc":"2.0","id":id,"result":result}))
    }
}

fn rpc_error(id: Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}

fn pretty(value: &Value) -> String {
    // Python json.dumps(indent=1) 每层一个空格；内层 JSON 内容与 structuredContent 相同。
    serde_json::to_string_pretty(value)
        .expect("JSON value is serializable")
        .lines()
        .map(|line| {
            let trimmed = line.trim_start_matches(' ');
            format!(
                "{}{}",
                " ".repeat((line.len() - trimmed.len()) / 2),
                trimmed
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn send(output: &mut impl Write, value: &Value) -> io::Result<()> {
    serde_json::to_writer(&mut *output, value)?;
    output.write_all(b"\n")?;
    output.flush()
}

fn parse_line(line: &[u8]) -> Result<Vec<Value>, Value> {
    match serde_json::from_slice(line.trim_ascii()) {
        Ok(Value::Array(batch)) => Ok(batch),
        Ok(value) => Ok(vec![value]),
        Err(_) => Err(rpc_error(Value::Null, -32700, "Parse error")),
    }
}

/// 内存流也走相同的按行解析、批处理、通知过滤和 JSON 序列化，供自检使用。
pub(crate) fn serve_io(
    input: impl BufRead,
    mut output: impl Write,
    attach: bool,
) -> io::Result<()> {
    let server = Server::new(attach);
    for line in input.split(b'\n') {
        let line = line?;
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        match parse_line(&line) {
            Ok(messages) => {
                for message in messages {
                    if let Some(reply) = server.handle(message) {
                        send(&mut output, &reply)?;
                    }
                }
            }
            Err(error) => send(&mut output, &error)?,
        }
    }
    Ok(())
}

pub(crate) fn run(attach: bool) -> i32 {
    let server = Arc::new(Server::new(attach));
    let output = Arc::new(Mutex::new(io::stdout()));
    struct StopOnReturn(Arc<Server>, Arc<Mutex<io::Stdout>>);
    impl Drop for StopOnReturn {
        fn drop(&mut self) {
            // 输出和关闭共享一把锁；已开始的回复先发完，迟到回复不得越过 run 返回。
            let _output = self.1.lock().unwrap_or_else(|error| error.into_inner());
            self.0.backend.shutdown();
        }
    }
    let _stop = StopOnReturn(Arc::clone(&server), Arc::clone(&output));
    let mut workers = Vec::new();
    let lang = i18n::current();
    for line in io::stdin().lock().split(b'\n') {
        let line = match line {
            Ok(line) => line,
            Err(error) => {
                eprintln!("{error}");
                return 1;
            }
        };
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let messages = match parse_line(&line) {
            Ok(messages) => messages,
            Err(error) => {
                if send(
                    &mut *output.lock().unwrap_or_else(|e| e.into_inner()),
                    &error,
                )
                .is_err()
                {
                    return 1;
                }
                continue;
            }
        };
        for message in messages {
            if message["method"] == "tools/call" {
                let server = Arc::clone(&server);
                let output = Arc::clone(&output);
                workers.push(thread::spawn(move || {
                    let _language = i18n::scope(lang);
                    if let Some(reply) = server.handle(message) {
                        let mut output = output.lock().unwrap_or_else(|error| error.into_inner());
                        if !server.backend.closed.load(Ordering::Acquire) {
                            let _ = send(&mut *output, &reply);
                        }
                    }
                }));
            } else if let Some(reply) = server.handle(message)
                && send(
                    &mut *output.lock().unwrap_or_else(|e| e.into_inner()),
                    &reply,
                )
                .is_err()
            {
                return 1;
            }
        }
        workers.retain(|worker| !worker.is_finished());
    }
    // 与旧版相同：输入结束后每个仍在执行的工具最多等待十秒，不阻塞进程退出。
    for worker in workers {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !worker.is_finished() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        if worker.is_finished() {
            let _ = worker.join();
        }
    }
    0
}

fn local_get(app: &App, route: &str, params: &[(&str, String)]) -> Result<Value, String> {
    // HTTP parse_qs 丢弃空值；后台必须保留这层转换，不能直接给 analysis 原始 JSON。
    let query = |key: &str| {
        params
            .iter()
            .find(|(name, value)| *name == key && !value.is_empty())
            .map(|(_, value)| value.as_str())
    };
    let path = query("path").map(Path::new);
    let integer = |key: &str, default: i64| -> Result<i64, String> {
        let Some(value) = query(key) else {
            return Ok(default);
        };
        match parse_integer(value) {
            Ok(value) => Ok(value),
            Err(error) => {
                analysis::list_dir(app, path, 1).map_err(|e| e.to_string())?;
                Err(error)
            }
        }
    };
    match route {
        "/api/agent/status" => Ok(analysis::status(app)),
        "/api/agent/summary" => {
            analysis::summary(app, path, integer("top", 10)?).map_err(|e| e.to_string())
        }
        "/api/agent/list" => {
            analysis::list_dir(app, path, integer("limit", 50)?).map_err(|e| e.to_string())
        }
        "/api/agent/explain" => {
            analysis::explain(app, path.unwrap_or_else(|| Path::new(""))).map_err(|e| e.to_string())
        }
        "/api/agent/largest" => {
            analysis::largest_files(app, path, integer("limit", 20)?, query("category"))
                .map_err(|e| e.to_string())
        }
        "/api/agent/cleanup" => {
            let minimum = parse_float(query("min_mb").unwrap_or("1"))? * (1_u64 << 20) as f64;
            if minimum.is_nan() {
                return Err("cannot convert float NaN to integer".into());
            }
            if minimum.is_infinite() {
                return Err("OverflowError('cannot convert float infinity to integer')".into());
            }
            let mut value =
                analysis::cleanup(app, path, integer("limit", 30)?, minimum.max(0.0) as u64)
                    .map_err(|e| e.to_string())?;
            if minimum >= u64::MAX as f64 {
                value["candidates"] = json!([]);
                value["candidates_shown"] = json!(0);
            }
            Ok(value)
        }
        _ => Err("not found".into()),
    }
}

fn internal(error: &str) -> String {
    Text::owned(
        format!("DiskMap 内部错误：{error}"),
        format!("DiskMap internal error: {error}"),
    )
    .to_string()
}

fn missing_get(value: &Value) -> String {
    format!(
        "AttributeError(\"'{}' object has no attribute 'get'\")",
        python_type(value)
    )
}

fn direct_integer(value: &Value) -> Result<i64, String> {
    match value {
        Value::String(value) => parse_integer(value),
        Value::Bool(value) => Ok(i64::from(*value)),
        Value::Number(value) => Ok(value
            .as_i64()
            .unwrap_or_else(|| value.as_f64().unwrap_or(0.0) as i64)),
        _ => Err(internal(&format!(
            "TypeError(\"int() argument must be a string, a bytes-like object or a real number, not '{}'\")",
            python_type(value)
        ))),
    }
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

pub(crate) fn parse_integer(value: &str) -> Result<i64, String> {
    let normalized = normalize_decimal_digits(value);
    let text = normalized.trim();
    let (negative, digits) = if let Some(digits) = text.strip_prefix('-') {
        (true, digits)
    } else {
        (false, text.strip_prefix('+').unwrap_or(text))
    };
    if digits.is_empty()
        || !valid_underscores(digits)
        || !digits
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'_')
    {
        return Err(format!(
            "invalid literal for int() with base 10: {}",
            python_repr(value)
        ));
    }
    // 这些参数随即限制到 1..=500；饱和转换保留 Python 任意精度整数的可见行为。
    let magnitude = digits
        .bytes()
        .filter(|byte| *byte != b'_')
        .fold(0_i64, |value, byte| {
            value
                .saturating_mul(10)
                .saturating_add(i64::from(byte - b'0'))
        });
    Ok(if negative {
        magnitude.saturating_neg()
    } else {
        magnitude
    })
}

/// 普通 tree 与 AI cleanup 共用 Python float() 的文本参数规则。
fn parse_float(value: &str) -> std::result::Result<f64, String> {
    let normalized = normalize_decimal_digits(value);
    let text = normalized.trim();
    if valid_underscores(text)
        && let Ok(value) = text.replace('_', "").parse::<f64>()
    {
        return Ok(value);
    }
    Err(format!(
        "could not convert string to float: {}",
        python_repr(value)
    ))
}

fn valid_underscores(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.iter().enumerate().all(|(index, &byte)| {
        byte != b'_'
            || (index > 0
                && index + 1 < bytes.len()
                && bytes[index - 1].is_ascii_digit()
                && bytes[index + 1].is_ascii_digit())
    })
}

// Python unicodedata 16.0.0 的 Nd 数字零码点；每组十个码点对应十进制 0..9。
// 与 Python int/float 一样接受全角、阿拉伯等数字，仍使用原文本报告转换错误。
const DECIMAL_ZEROES: &[u32] = &[
    0x30, 0x660, 0x6f0, 0x7c0, 0x966, 0x9e6, 0xa66, 0xae6, 0xb66, 0xbe6, 0xc66, 0xce6, 0xd66,
    0xde6, 0xe50, 0xed0, 0xf20, 0x1040, 0x1090, 0x17e0, 0x1810, 0x1946, 0x19d0, 0x1a80, 0x1a90,
    0x1b50, 0x1bb0, 0x1c40, 0x1c50, 0xa620, 0xa8d0, 0xa900, 0xa9d0, 0xa9f0, 0xaa50, 0xabf0, 0xff10,
    0x104a0, 0x10d30, 0x10d40, 0x11066, 0x110f0, 0x11136, 0x111d0, 0x112f0, 0x11450, 0x114d0,
    0x11650, 0x116c0, 0x116d0, 0x116da, 0x11730, 0x118e0, 0x11950, 0x11bf0, 0x11c50, 0x11d50,
    0x11da0, 0x11f50, 0x16130, 0x16a60, 0x16ac0, 0x16b50, 0x16d70, 0x1ccf0, 0x1d7ce, 0x1d7d8,
    0x1d7e2, 0x1d7ec, 0x1d7f6, 0x1e140, 0x1e2f0, 0x1e4f0, 0x1e5f1, 0x1e950, 0x1fbf0,
];

fn normalize_decimal_digits(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            let code = u32::from(ch);
            let upper = DECIMAL_ZEROES.partition_point(|zero| *zero <= code);
            if upper > 0 {
                let digit = code - DECIMAL_ZEROES[upper - 1];
                if digit < 10 {
                    return char::from(b'0' + digit as u8);
                }
            }
            ch
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use diskmap_core::i18n::Lang;

    fn rpc(server: &Server, method: &str, params: Value) -> Value {
        server
            .handle(json!({"jsonrpc":"2.0","id":7,"method":method,"params":params}))
            .unwrap()
    }

    #[test]
    fn negotiates_every_protocol_and_describes_nine_tools() {
        let _lang = i18n::scope(Lang::En);
        let server = Server::new(false);
        for protocol in PROTOCOLS {
            assert_eq!(
                rpc(&server, "initialize", json!({"protocolVersion":protocol}))["result"]["protocolVersion"],
                *protocol
            );
        }
        assert_eq!(
            rpc(
                &server,
                "initialize",
                json!({"protocolVersion":"1999-01-01"})
            )["result"]["protocolVersion"],
            PROTOCOLS[0]
        );
        let list = rpc(&server, "tools/list", Value::Null);
        let tools = list["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 9);
        for tool in tools {
            assert_eq!(tool["inputSchema"]["type"], "object");
            assert_eq!(tool["inputSchema"]["additionalProperties"], false);
            assert_eq!(tool["annotations"]["destructiveHint"], false);
        }
        assert_eq!(rpc(&server, "ping", json!({}))["result"], json!({}));
        assert_eq!(rpc(&server, "no/such", json!({}))["error"]["code"], -32601);
        assert_eq!(
            rpc(&server, "tools/call", json!({"name":"nope"}))["error"]["code"],
            -32602
        );
        assert!(
            server
                .handle(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
                .is_none()
        );
        assert!(
            server
                .handle(json!({"jsonrpc":"2.0","id":7,"result":{}}))
                .is_none()
        );
    }

    #[test]
    fn framed_messages_handle_batches_and_errors() {
        let input = b"\nnot-json\n[{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"},{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}]\n{\"id\":2,\"method\":\"ping\"}\n\xff\n";
        let mut output = Vec::new();
        serve_io(&input[..], &mut output, false).unwrap();
        let replies: Vec<Value> = output
            .split(|&b| b == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice(line).unwrap())
            .collect();
        assert_eq!(replies.len(), 4);
        assert_eq!(replies[0]["error"]["code"], -32700);
        assert_eq!(replies[1]["result"], json!({}));
        assert_eq!(replies[2]["error"]["code"], -32600);
        assert_eq!(replies[3]["error"]["code"], -32700);
    }

    #[test]
    fn preserves_python_parameter_error_classes() {
        let _lang = i18n::scope(Lang::En);
        let server = Server::new(false);
        let response = rpc(
            &server,
            "tools/call",
            json!({"name":"diskmap_summary","arguments":[1]}),
        );
        assert_eq!(response["result"]["isError"], true);
        assert_eq!(
            response["result"]["content"][0]["text"],
            "DiskMap internal error: AttributeError(\"'list' object has no attribute 'get'\")"
        );
        let response = rpc(&server, "initialize", json!([1]));
        assert_eq!(response["error"]["code"], -32603);
        assert_eq!(
            response["error"]["message"],
            "AttributeError(\"'list' object has no attribute 'get'\")"
        );
        assert_eq!(parse_integer("  +１_٢٣ ").unwrap(), 123);
        assert_eq!(parse_integer("9".repeat(100).as_str()).unwrap(), i64::MAX);
        assert!(parse_integer("1.0").is_err());
        assert_eq!(parse_float("１_２.٥").unwrap(), 12.5);
    }

    #[test]
    fn shutdown_closes_existing_and_late_background_work() {
        use diskmap_core::{classify::Classifier, pkgdb::PkgDb, procinfo::ProcInfo};

        let server = Server::new(false);
        let pkgdb = Arc::new(PkgDb::default());
        let classifier = Arc::new(Classifier::new(Some(Arc::clone(&pkgdb)), &[]));
        let app = App::with_components(pkgdb, classifier, ProcInfo::new());
        assert!(server.backend.local.set(app.clone()).is_ok());
        server.backend.shutdown();
        // 关闭先于路径校验，测试不读取或生成实际扫描目录。
        assert!(app.agent_scan("unused", false, false, true).is_err());
        assert!(server.backend.call("diskmap_status", &Value::Null).is_err());

        let fresh = Server::new(false);
        fresh.backend.shutdown();
        let target = fresh.backend.endpoint();
        assert!(matches!(target, Target::Background));
        assert!(fresh.backend.local.get().is_none());
        assert!(
            fresh
                .backend
                .request(&target, "GET", "/api/agent/status", &[], None)
                .is_err()
        );
    }
}
