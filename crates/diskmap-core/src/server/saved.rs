//! 保存结果的界面路由。由普通界面请求入口统一处理鉴权和心跳。

use std::io;
use std::path::Path;

use serde_json::{Value, json};

use crate::app::{App, saved::unix_seconds};
use crate::i18n::Text;
use crate::platform::{self, Os, path::cmp_paths};

use super::{HttpResponse, response};

pub(super) fn list(app: &App) -> HttpResponse {
    let items = match app.saved_scans() {
        Ok(items) => items,
        Err(_) => {
            return error(
                500,
                Text::new("无法列出保存的扫描结果", "Could not list saved scans"),
            );
        }
    };
    if items.is_empty() {
        return response(200, json!({"items": []}));
    }
    let disks = platform::mounts::disk_mounts();
    let home = platform::home_dir().map(|path| match path.to_str() {
        Some(text) => platform::norm_path(text),
        None => path,
    });
    let entries: Vec<_> = items.into_iter().map(|item| {
        let p = item.progress;
        json!({
            "id": item.id,
            "root": p.root.to_string_lossy(),
            "finished_at": p.finished_at.map(unix_seconds),
            "files": p.files, "dirs": p.dirs, "bytes": p.bytes,
            "method": p.method.key(),
            "card": matching_card(Os::current(), &p.root, home.as_deref(), disks.iter().map(|disk| disk.path.as_path())),
        })
    }).collect();
    response(200, json!({"items": entries}))
}

fn matching_card<'a>(
    os: Os,
    root: &Path,
    home: Option<&Path>,
    disks: impl IntoIterator<Item = &'a Path>,
) -> Value {
    let matches = |path: &Path| {
        cmp_paths(
            os,
            root.as_os_str().as_encoded_bytes(),
            path.as_os_str().as_encoded_bytes(),
        )
        .is_eq()
    };
    if home.is_some_and(matches) {
        json!({"kind": "home"})
    } else if let Some(path) = disks.into_iter().find(|path| matches(path)) {
        json!({"kind": "disk", "path": path.to_string_lossy()})
    } else {
        Value::Null
    }
}

pub(super) fn open(app: &App, body: &Value) -> HttpResponse {
    let Some(id) = body.get("id").and_then(Value::as_str) else {
        return invalid_id();
    };
    match app.open_saved_scan(id) {
        Ok(Some(status)) => response(200, status),
        Ok(None) => unavailable(),
        Err(err) if err.kind() == io::ErrorKind::InvalidInput => invalid_id(),
        Err(err) if err.kind() == io::ErrorKind::NotConnected => {
            error(503, Text::new("服务已关闭", "The service is closed"))
        }
        Err(_) => error(
            500,
            Text::new("无法打开保存的扫描结果", "Could not open the saved scan"),
        ),
    }
}

pub(super) fn delete(app: &App, body: &Value) -> HttpResponse {
    let Some(id) = body.get("id").and_then(Value::as_str) else {
        return invalid_id();
    };
    match app.delete_saved_scan(id) {
        Ok(true) => response(200, json!({"ok": true})),
        Ok(false) => unavailable(),
        Err(err) if err.kind() == io::ErrorKind::InvalidInput => invalid_id(),
        Err(_) => error(
            500,
            Text::new("无法删除保存的扫描结果", "Could not delete the saved scan"),
        ),
    }
}

fn invalid_id() -> HttpResponse {
    error(
        400,
        Text::new("无效的保存结果编号", "Invalid saved scan ID"),
    )
}

fn unavailable() -> HttpResponse {
    error(
        404,
        Text::new(
            "保存的扫描结果已损坏或被移除",
            "The saved scan is damaged or no longer available",
        ),
    )
}

fn error(code: u16, message: Text) -> HttpResponse {
    response(code, json!({"error": message}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn welcome_card_matches_the_platform_path_rules() {
        let home = Path::new(r"C:\Users\Σ");
        let disk = Path::new(r"C:\");
        let card = |root| matching_card(Os::Windows, Path::new(root), Some(home), [disk]);
        assert_eq!(card("c:/users/ς"), json!({"kind": "home"}));
        assert_eq!(card("c:/"), json!({"kind": "disk", "path": disk}));
        assert!(card("C:/Users/Σ/Documents").is_null());
        assert!(
            matching_card(Os::Linux, Path::new("/Home"), Some(Path::new("/home")), []).is_null()
        );
    }
}
