//! AI HTTP 路由及 Python 参数转换顺序；分析逻辑始终调用只读的 analysis 模块。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::{HttpResponse, python_repr, python_str, python_type, response, truthy};
use crate::analysis::{self, Source};
use crate::app::App;

struct RequestSource<'a> {
    app: &'a App,
    snapshot: analysis::Snapshot,
    raw_ui: Option<Value>,
    note: Note,
}

enum Note {
    Text,
    Array(Value),
    Invalid(ApiError),
}

impl<'a> RequestSource<'a> {
    fn new(app: &'a App) -> Self {
        let (mut snapshot, raw_ui) = app.analysis_snapshot_with_ui_state();
        if let (Some(ui), Some(raw)) = (&mut snapshot.ui_state, &raw_ui) {
            // 旧版查询路径先 str()；但 falsey 值跳过嵌套分析，响应顶层仍保留上报的原始类型。
            let path = |key: &str| {
                raw.get(key)
                    .filter(|value| truthy(value))
                    .map(|value| PathBuf::from(python_str(value)))
            };
            ui.view = path("view");
            ui.selected = path("selected");
        }
        Self {
            app,
            snapshot,
            raw_ui,
            note: Note::Text,
        }
    }

    fn restore_ui_values(&self, out: &mut Value) {
        if let Some(raw) = &self.raw_ui {
            for key in ["view", "selected"] {
                out[key] = raw.get(key).cloned().unwrap_or(Value::Null);
            }
        }
    }
}

impl Source for RequestSource<'_> {
    fn snapshot(&self) -> analysis::Snapshot {
        self.snapshot.clone()
    }

    fn request_show(&self, path: &Path, note: &str, now: f64) {
        match &self.note {
            Note::Text => self.app.request_show(path, note, now),
            Note::Array(value) => self.app.request_show_value(path, value.clone(), now),
            // 旧版先验证路径，再尝试切片 note；切片失败不得写入命令。
            Note::Invalid(_) => {}
        }
    }
}

struct ApiError {
    code: u16,
    message: String,
}

impl ApiError {
    fn bad(message: impl Into<String>) -> Self {
        Self {
            code: 400,
            message: message.into(),
        }
    }
    fn internal(message: impl Into<String>) -> Self {
        Self {
            code: 500,
            message: message.into(),
        }
    }
}

impl From<analysis::AnalysisError> for ApiError {
    fn from(error: analysis::AnalysisError) -> Self {
        Self::bad(error.to_string())
    }
}

type Result<T> = std::result::Result<T, ApiError>;

pub(super) fn get(app: &App, route: &str, query: &HashMap<String, String>) -> HttpResponse {
    let source = RequestSource::new(app);
    match get_value(&source, route, query) {
        Ok(value) => response(200, value),
        Err(error) => response(error.code, json!({"error": error.message})),
    }
}

fn get_value(
    source: &RequestSource<'_>,
    route: &str,
    query: &HashMap<String, String>,
) -> Result<Value> {
    let path = query.get("path").map(Path::new);
    match route {
        "/api/agent/status" => {
            let mut value = analysis::status(source);
            if let Some(ui) = value.get_mut("user_view") {
                source.restore_ui_values(ui);
            }
            if let Some(analysis::ScanSnapshot::Live(scan)) = &source.snapshot.scan
                && let Some(saved) = crate::app::saved::status_marker(scan)
            {
                value["saved"] = saved;
            }
            Ok(value)
        }
        "/api/agent/summary" => {
            let top = integer_after_directory(source, path, query.get("top"), 10)?;
            Ok(analysis::summary(source, path, top)?)
        }
        "/api/agent/list" => {
            let limit = integer_after_directory(source, path, query.get("limit"), 50)?;
            Ok(analysis::list_dir(source, path, limit)?)
        }
        "/api/agent/explain" => Ok(analysis::explain(
            source,
            path.unwrap_or_else(|| Path::new("")),
        )?),
        "/api/agent/cleanup" => {
            // Python 在调用 analysis.cleanup 之前计算 min_mb，所以其错误优先于未扫描/无效目录。
            let min_text = query.get("min_mb").map_or("1", String::as_str);
            let minimum = parse_float(min_text).map_err(ApiError::bad)? * (1_u64 << 20) as f64;
            if minimum.is_nan() {
                return Err(ApiError::bad("cannot convert float NaN to integer"));
            }
            if minimum.is_infinite() {
                return Err(ApiError::internal(
                    "OverflowError('cannot convert float infinity to integer')",
                ));
            }
            let limit = integer_after_directory(source, path, query.get("limit"), 30)?;
            let mut value = analysis::cleanup(source, path, limit, minimum.max(0.0) as u64)?;
            // Python 整数无上限；超过索引 u64 容量的有限阈值必然筛掉所有候选。
            if minimum >= u64::MAX as f64 {
                value["candidates"] = json!([]);
                value["candidates_shown"] = json!(0);
            }
            Ok(value)
        }
        "/api/agent/largest" => {
            let limit = integer_after_directory(source, path, query.get("limit"), 20)?;
            let category = query
                .get("category")
                .filter(|value| !value.is_empty())
                .map(String::as_str);
            Ok(analysis::largest_files(source, path, limit, category)?)
        }
        "/api/agent/view" => {
            let mut value = analysis::user_view(source)?;
            source.restore_ui_values(&mut value);
            Ok(value)
        }
        _ => Err(ApiError {
            code: 404,
            message: "not found".into(),
        }),
    }
}

/// 只在整数格式无效时预查目录，保留旧版「未扫描/路径错误先于整数转换」的顺序。
fn integer_after_directory(
    source: &RequestSource<'_>,
    path: Option<&Path>,
    value: Option<&String>,
    default: i64,
) -> Result<i64> {
    let Some(value) = value else {
        return Ok(default);
    };
    match parse_integer(value) {
        Ok(value) => Ok(value),
        Err(error) => {
            analysis::list_dir(source, path, 1)?;
            Err(error)
        }
    }
}

fn parse_integer(value: &str) -> Result<i64> {
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
        return Err(ApiError::bad(format!(
            "invalid literal for int() with base 10: {}",
            python_repr(value)
        )));
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
pub(super) fn parse_float(value: &str) -> std::result::Result<f64, String> {
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

pub(super) fn show(app: &App, body: &Value) -> HttpResponse {
    let mut source = RequestSource::new(app);
    let note = body.get("note");
    if let Some(note) = note.filter(|value| truthy(value)) {
        source.note = match note {
            Value::String(_) => Note::Text,
            Value::Array(items) => {
                Note::Array(Value::Array(items.iter().take(200).cloned().collect()))
            }
            Value::Object(_) => {
                Note::Invalid(ApiError::internal("KeyError(slice(None, 200, None))"))
            }
            _ => Note::Invalid(ApiError::internal(format!(
                "TypeError(\"'{}' object is not subscriptable\")",
                python_type(note)
            ))),
        };
    }
    let path = body.get("path").map_or_else(String::new, python_str);
    let result = analysis::show(&source, Path::new(&path), note.and_then(Value::as_str));
    match result {
        Err(error) => response(400, json!({"error": error.to_string()})),
        Ok(value) => match source.note {
            Note::Invalid(error) => response(error.code, json!({"error": error.message})),
            _ => response(200, value),
        },
    }
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
