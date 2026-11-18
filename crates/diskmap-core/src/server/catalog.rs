//! 界面用的完整索引路由，认证和心跳由普通界面请求入口处理。

use std::collections::HashMap;

use serde_json::json;

use crate::app::App;
use crate::i18n::Text;
use crate::model::Category;
use crate::view::catalog::GroupOptions;

use super::{HttpResponse, optional_result, response, tree_frac};

pub(super) fn aggregate(app: &App, query: &HashMap<String, String>) -> HttpResponse {
    let Some((parent, options)) = group_params(query) else {
        return response(
            400,
            json!({"error": Text::new("无效的合并块参数", "Invalid group parameters")}),
        );
    };
    let path = query.get("path").map_or("", String::as_str);
    match app.group_page(path, parent, options) {
        Some(page) => response(200, json!(page)),
        None => response(
            404,
            json!({"error": Text::new(
                "这个合并块已不在当前视图中，请重新选择。",
                "This group is no longer in the current view. Select a group again.",
            )}),
        ),
    }
}

fn group_params(query: &HashMap<String, String>) -> Option<(&str, GroupOptions)> {
    let frac = tree_frac(query).ok().filter(|frac| !frac.is_nan())?;
    let parent = query.get("parent")?;
    let ordinal = query
        .get("group")?
        .parse::<usize>()
        .ok()
        .filter(|&i| i < 2)?;
    let offset = query
        .get("offset")
        .map_or("0", String::as_str)
        .parse()
        .ok()?;
    let bytes = |key| {
        query
            .get(key)?
            .parse::<f64>()
            .ok()
            .filter(|&n| n.is_finite() && n >= 0.0)
    };
    let size = bytes("size")?;
    let view_size = bytes("view_size")?;
    let parent_files = query.get("parent_files")?.parse().ok()?;
    let parent_dirs = query.get("parent_dirs")?.parse().ok()?;
    let count = query.get("count")?.parse().ok().filter(|&n| n > 0)?;
    let cls = query.get("cls")?.parse::<u8>().ok()?;
    Category::from_index(usize::from(cls))?;
    Some((
        parent,
        GroupOptions {
            frac,
            view_size,
            parent_files,
            parent_dirs,
            ordinal,
            offset,
            size,
            count,
            cls,
        },
    ))
}

pub(super) fn search(app: &App, query: &HashMap<String, String>) -> HttpResponse {
    let Some(name) = query.get("q") else {
        return response(
            400,
            json!({"error": Text::new("请输入要搜索的名字", "Enter a name to search for")}),
        );
    };
    let path = query.get("path").map_or("", String::as_str);
    optional_result(app.search_names(path, name).map(|results| json!(results)))
}
