//! 命令行 Markdown 报告；文案、空行和表格格式与旧版保持一致。

use serde_json::Value;

use crate::i18n::Text;

/// 将同一次查询得到的 JSON 报告渲染成 Markdown，不再读取扫描或进程状态。
pub(super) fn markdown(report: &Value, apparent: bool) -> String {
    let st = &report["status"]["scan"];
    let sm = &report["summary"];
    let cl = &report["cleanup"];
    let in_use = &sm["in_use"];
    let path = value(&sm["path"]);
    let size = value(&sm["size"]);
    let files = grouped(sm["files"].as_u64().unwrap_or(0));
    let dirs = grouped(sm["dirs"].as_u64().unwrap_or(0));
    let mut lines = vec![
        Text::owned(
            format!("# DiskMap 磁盘分析：{path}"),
            format!("# DiskMap disk analysis: {path}"),
        )
        .to_string(),
        String::new(),
        Text::owned(
            format!("- 总大小：**{size}**（{files} 个文件，{dirs} 个文件夹）"),
            format!("- Total size: **{size}** ({files} files, {dirs} folders)"),
        )
        .to_string(),
    ];
    let mode = if apparent {
        Text::new("文件大小", "file length")
    } else {
        Text::new("实际占用空间", "space on disk")
    };
    let finished = st["finished_at"].as_str().filter(|s| !s.is_empty());
    let elapsed = value(&st["elapsed_seconds"]);
    lines.push(
        Text::owned(
            format!(
                "- 扫描：{}，用时 {elapsed} 秒，按{mode}统计",
                finished.unwrap_or("进行中")
            ),
            format!(
                "- Scan: {}, took {elapsed} s, measured by {mode}",
                finished.unwrap_or("in progress")
            ),
        )
        .to_string(),
    );
    let note = Text::new("- 注意：", "- Note: ");
    if st["denied_dirs"].as_u64().unwrap_or(0) != 0 {
        lines.push(format!("{note}{}", value(&st["denied_note"])));
    }
    if let Some(scan_note) = sm["note"].as_str().filter(|s| !s.is_empty()) {
        lines.push(format!("{note}{scan_note}"));
    }
    let reclaimable = value(&cl["reclaimable_size"]);
    lines.push(
        Text::owned(
            format!("- 可清理（缓存/临时）：约 **{reclaimable}**"),
            format!("- Reclaimable (cache / temp): about **{reclaimable}**"),
        )
        .to_string(),
    );
    let running_files = value(&in_use["files"]);
    let running_size = value(&in_use["size"]);
    lines.push(
        Text::owned(
            format!("- 正被程序使用的文件：{running_files} 个，共 {running_size}"),
            format!("- Files in use by programs: {running_files}, {running_size} in total"),
        )
        .to_string(),
    );
    lines.extend([
        String::new(),
        Text::new("## 按重要程度", "## By importance").to_string(),
        String::new(),
        Text::new("| 类别 | 大小 | 占比 |", "| Category | Size | Share |").to_string(),
        "|---|---:|---:|".into(),
    ]);
    for c in array(&sm["by_category"]) {
        lines.push(format!(
            "| {} | {} | {}% |",
            value(&c["category_label"]),
            value(&c["size"]),
            value(&c["percent"]),
        ));
    }
    lines.extend([
        String::new(),
        Text::new("## 最大的子项", "## Largest items").to_string(),
        String::new(),
        Text::new(
            "| 路径 | 大小 | 占比 | 类别 | 原因 |",
            "| Path | Size | Share | Category | Reason |",
        )
        .to_string(),
        "|---|---:|---:|---|---|".into(),
    ]);
    for entry in array(&sm["largest_children"]) {
        lines.push(format!(
            "| `{}` | {} | {}% | {} | {} |",
            entry_name(entry),
            value(&entry["size"]),
            value(&entry["percent_of_parent"]),
            value(&entry["category_label"]),
            value(&entry["reason"]),
        ));
    }
    lines.extend([
        String::new(),
        Text::new("## 最大的文件", "## Largest files").to_string(),
        String::new(),
        Text::new(
            "| 文件 | 大小 | 类别 | 说明 |",
            "| File | Size | Category | Details |",
        )
        .to_string(),
        "|---|---:|---|---|".into(),
    ]);
    for entry in array(&sm["largest_files"]) {
        let extra = if let Some(used) = entry["used_by"].as_str().filter(|s| !s.is_empty()) {
            Text::owned(format!("正被 {used} 使用"), format!("in use by {used}")).to_string()
        } else if let Some(package) = entry["package"].as_str().filter(|s| !s.is_empty()) {
            Text::owned(format!("软件包 {package}"), format!("package {package}")).to_string()
        } else {
            value(&entry["reason"])
        };
        lines.push(format!(
            "| `{}` | {} | {} | {extra} |",
            value(&entry["path"]),
            value(&entry["size"]),
            value(&entry["category_label"]),
        ));
    }
    lines.extend([
        String::new(),
        Text::new("## 可清理候选", "## Cleanup candidates").to_string(),
        String::new(),
    ]);
    let candidates = array(&cl["candidates"]);
    if candidates.is_empty() {
        lines.push(
            Text::new(
                "没有找到明显的缓存/临时文件。",
                "No obvious cache or temporary files found.",
            )
            .to_string(),
        );
    } else {
        lines.extend([
            Text::new(
                "| 路径 | 大小 | 原因 | 备注 |",
                "| Path | Size | Reason | Notes |",
            )
            .to_string(),
            "|---|---:|---|---|".into(),
        ]);
        for entry in candidates {
            let running = entry["in_use_files"].as_u64().unwrap_or(0);
            let busy = if running != 0 {
                Text::owned(
                    format!("{running} 个文件正在使用"),
                    format!("{running} files in use"),
                )
                .to_string()
            } else {
                String::new()
            };
            lines.push(format!(
                "| `{}` | {} | {} | {busy} |",
                entry_name(entry),
                value(&entry["size"]),
                value(&entry["reason"]),
            ));
        }
    }
    lines.push(String::new());
    lines.push(
        Text::new(
            "> DiskMap 只读取磁盘，不会删除任何文件。以上清理建议请在确认后手动处理。",
            "> DiskMap only reads the disk and never deletes files. Review the suggestions above before acting on them.",
        )
        .to_string(),
    );
    lines.join("\n") + "\n"
}

fn array(value: &Value) -> &[Value] {
    value.as_array().map_or(&[], Vec::as_slice)
}

/// JSON 数字直接显示，保留 Python `str(float)` 对整数浮点数的 `.0`。
fn value(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        _ => value.to_string(),
    }
}

fn entry_name(entry: &Value) -> String {
    value(if entry["type"] == "group" {
        &entry["note"]
    } else {
        &entry["path"]
    })
}

fn grouped(number: u64) -> String {
    let digits = number.to_string();
    let mut result = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, digit) in digits.chars().enumerate() {
        if i != 0 && (digits.len() - i).is_multiple_of(3) {
            result.push(',');
        }
        result.push(digit);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::{Lang, scope};
    use serde_json::json;

    #[test]
    fn empty_report_preserves_spacing_and_number_format() {
        let _language = scope(Lang::En);
        let report = json!({
            "status": {"scan": {"finished_at": null, "elapsed_seconds": 0.0, "denied_dirs": 0}},
            "summary": {
                "path": "/data", "size": "0 B", "files": 1234, "dirs": 5678901,
                "in_use": {"files": 0, "size": "0 B"},
                "by_category": [], "largest_children": [], "largest_files": []
            },
            "cleanup": {"reclaimable_size": "0 B", "candidates": []}
        });
        assert_eq!(
            markdown(&report, true),
            concat!(
                "# DiskMap disk analysis: /data\n\n",
                "- Total size: **0 B** (1,234 files, 5,678,901 folders)\n",
                "- Scan: in progress, took 0.0 s, measured by file length\n",
                "- Reclaimable (cache / temp): about **0 B**\n",
                "- Files in use by programs: 0, 0 B in total\n\n",
                "## By importance\n\n| Category | Size | Share |\n|---|---:|---:|\n\n",
                "## Largest items\n\n| Path | Size | Share | Category | Reason |\n|---|---:|---:|---|---|\n\n",
                "## Largest files\n\n| File | Size | Category | Details |\n|---|---:|---|---|\n\n",
                "## Cleanup candidates\n\nNo obvious cache or temporary files found.\n\n",
                "> DiskMap only reads the disk and never deletes files. Review the suggestions above before acting on them.\n",
            )
        );
    }

    #[test]
    fn groups_and_running_files_keep_legacy_table_contents() {
        let _language = scope(Lang::Zh);
        let report = json!({
            "status": {"scan": {"finished_at": "2026-11-15 01:02:03", "elapsed_seconds": 1.2,
                "denied_dirs": 1, "denied_note": "权限不足"}},
            "summary": {
                "path": "/data", "size": "10 B", "files": 1, "dirs": 0, "note": "不完整",
                "in_use": {"files": 1, "size": "10 B"},
                "by_category": [{"category_label": "缓存", "size": "10 B", "percent": 100.0}],
                "largest_children": [{"type": "group", "path": "/data", "note": "小文件",
                    "size": "0 B", "category_label": "缓存"}],
                "largest_files": [{"path": "/data/f", "size": "10 B", "category_label": "正在使用",
                    "used_by": "程序", "package": "包", "reason": "原因"}]
            },
            "cleanup": {"reclaimable_size": "10 B", "candidates": [{
                "type": "dir", "path": "/data/cache", "size": "10 B", "reason": "缓存目录", "in_use_files": 2
            }]}
        });
        let markdown = markdown(&report, false);
        assert!(markdown.contains("按实际占用空间统计\n- 注意：权限不足\n- 注意：不完整\n"));
        assert!(markdown.contains("| 缓存 | 10 B | 100.0% |\n"));
        assert!(markdown.contains("| `小文件` | 0 B | % | 缓存 |  |\n"));
        assert!(markdown.contains("| `/data/f` | 10 B | 正在使用 | 正被 程序 使用 |\n"));
        assert!(markdown.contains("| `/data/cache` | 10 B | 缓存目录 | 2 个文件正在使用 |\n"));
    }
}
