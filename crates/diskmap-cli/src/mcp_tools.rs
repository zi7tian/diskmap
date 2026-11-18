//! MCP 工具说明和模式；中英文本逐项对应旧版 backend/mcp_server.py。

use diskmap_core::i18n::Text;
use serde_json::{Value, json};

pub(crate) const PROTOCOLS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

pub(crate) const NAMES: &[&str] = &[
    "diskmap_status",
    "diskmap_scan",
    "diskmap_summary",
    "diskmap_list",
    "diskmap_explain",
    "diskmap_cleanup",
    "diskmap_largest_files",
    "diskmap_user_view",
    "diskmap_show",
];

pub(crate) const INSTRUCTIONS: Text = Text::new(
    "DiskMap 分析磁盘空间占用，并按重要程度给每个文件/文件夹分类：\ncore 系统核心（不能动）、running 正被程序使用、system 系统/已安装程序（应卸载而非删除）、\nconfig 配置、user 用户个人文件（删不删由用户决定）、cache 缓存/临时（通常可安全清理）、other 应用数据。\n每个结果都附带 safety 字段（danger / in_use / uninstall_only / caution / ask_user / safe_to_clean）和中文建议。\n\n建议流程：diskmap_status → （没有结果时）diskmap_scan → diskmap_summary → 用 diskmap_list 逐层深入，\n或用 diskmap_cleanup 找可清理的空间。用户提到“这个”“我选中的”时用 diskmap_user_view。\n需要给用户指出某个项目时用 diskmap_show（会在用户的 DiskMap 窗口中高亮）。\n这些工具不会删除文件；建议删除前务必征得用户同意，并说明 safety 等级。",
    "DiskMap measures disk usage and classifies every file and folder by importance:\ncore = system core (never touch), running = in use by a program, system = OS or installed software\n(uninstall instead of deleting), config = settings, user = personal files (the user decides),\ncache = cache/temp (usually safe to clean), other = application data.\nEvery result has a safety field (danger / in_use / uninstall_only / caution / ask_user / safe_to_clean)\nand a short advice text.\n\nSuggested flow: diskmap_status → (if there is no scan) diskmap_scan → diskmap_summary → drill down with\ndiskmap_list, or use diskmap_cleanup to find reclaimable space. When the user says \"this\" or \"the selected\none\", use diskmap_user_view. To point something out to the user, use diskmap_show (it highlights the item\nin the user's DiskMap window). These tools never delete files; before suggesting any deletion, get the\nuser's consent and explain the safety level.",
);

pub(crate) fn tools() -> Value {
    json!([
        {
            "name": "diskmap_status",
            "title": Text::new("DiskMap 状态", "DiskMap status"),
            "description": Text::new("查看当前是否已有扫描结果、扫描范围与进度、是否连接到了用户打开的 DiskMap 窗口，以及用户在窗口中正在看的位置。建议第一个调用。", "Whether a scan exists, its scope and progress, whether the user's DiskMap window is connected, and where the user is looking in it. Call this first."),
            "inputSchema": {
                "type": "object",
                "properties": {

                },
                "required": [

                ],
                "additionalProperties": false
            },
            "annotations": {
                "title": Text::new("DiskMap 状态", "DiskMap status"),
                "readOnlyHint": true,
                "destructiveHint": false,
                "idempotentHint": true,
                "openWorldHint": false
            }
        },
        {
            "name": "diskmap_scan",
            "title": Text::new("扫描磁盘", "Scan disk"),
            "description": Text::new("扫描一个文件夹或整个磁盘（Linux/macOS 用 /，Windows 如 C:\\）。如果已有覆盖该路径的扫描结果会直接复用（除非 rescan=true）。连接到用户的 DiskMap 窗口时，新扫描会显示在窗口中并替换当前结果。完成后返回该路径的概览；整盘扫描通常需要几秒到一两分钟。", "Scan a folder or a whole drive (/ on Linux/macOS, e.g. C:\\ on Windows). An existing scan that covers the path is reused unless rescan=true. When connected to the user's DiskMap window, a new scan appears there and replaces the current one. Returns an overview of the path when done; a whole drive usually takes a few seconds to a couple of minutes."),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": Text::new("要扫描的文件夹，省略时为系统盘", "Folder to scan; defaults to the system drive")
                    },
                    "rescan": {
                        "type": "boolean",
                        "description": Text::new("忽略已有结果重新扫描", "Scan again even if results exist"),
                        "default": false
                    },
                    "wait_seconds": {
                        "type": "integer",
                        "description": Text::new("最多等待扫描完成的秒数（1-600）", "Maximum seconds to wait for the scan to finish (1-600)"),
                        "default": 120
                    },
                    "apparent": {
                        "type": "boolean",
                        "description": Text::new("按文件长度而不是实际占用空间统计", "Count file length instead of space on disk"),
                        "default": false
                    },
                    "same_fs": {
                        "type": "boolean",
                        "description": Text::new("不进入其他分区（类似 du -x）", "Stay on one file system (like du -x)"),
                        "default": false
                    }
                },
                "required": [

                ],
                "additionalProperties": false
            },
            "annotations": {
                "title": Text::new("扫描磁盘", "Scan disk"),
                "readOnlyHint": false,
                "destructiveHint": false,
                "idempotentHint": true,
                "openWorldHint": false
            }
        },
        {
            "name": "diskmap_summary",
            "title": Text::new("占用概览", "Usage overview"),
            "description": Text::new("某个文件夹的占用概览：按重要程度分类的空间分布、最大的子项、最大的文件、可清理空间与候选、正被程序使用的大文件。", "Overview of a folder: space by importance category, largest children, largest files, reclaimable space and candidates, large files in use."),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": Text::new("文件夹绝对路径，省略时为扫描的根目录", "Absolute folder path; defaults to the scan root")
                    },
                    "top": {
                        "type": "integer",
                        "description": Text::new("每个列表返回几项（1-50）", "Items per list (1-50)"),
                        "default": 10
                    }
                },
                "required": [

                ],
                "additionalProperties": false
            },
            "annotations": {
                "title": Text::new("占用概览", "Usage overview"),
                "readOnlyHint": true,
                "destructiveHint": false,
                "idempotentHint": true,
                "openWorldHint": false
            }
        },
        {
            "name": "diskmap_list",
            "title": Text::new("列出子项", "List folder"),
            "description": Text::new("列出文件夹的直接子项（按大小从大到小），每项带类别、原因、占比、是否正在使用和安全建议。用于一层层往下找空间被谁占了。", "List a folder's direct children, largest first, each with category, reason, share, in-use state and safety advice. Use it to drill down level by level."),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": Text::new("文件或文件夹的绝对路径", "Absolute path of a file or folder")
                    },
                    "limit": {
                        "type": "integer",
                        "description": Text::new("最多返回几项（1-500）", "Maximum items (1-500)"),
                        "default": 40
                    }
                },
                "required": [
                    "path"
                ],
                "additionalProperties": false
            },
            "annotations": {
                "title": Text::new("列出子项", "List folder"),
                "readOnlyHint": true,
                "destructiveHint": false,
                "idempotentHint": true,
                "openWorldHint": false
            }
        },
        {
            "name": "diskmap_explain",
            "title": Text::new("解释项目", "Explain item"),
            "description": Text::new("解释一个文件或文件夹：大小、重要程度类别和原因、所属软件包、正在使用它的进程、修改时间、能不能删的建议。", "Explain a file or folder: size, importance category and reason, owning package, processes using it, modification time, and whether it can be deleted."),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": Text::new("文件或文件夹的绝对路径", "Absolute path of a file or folder")
                    }
                },
                "required": [
                    "path"
                ],
                "additionalProperties": false
            },
            "annotations": {
                "title": Text::new("解释项目", "Explain item"),
                "readOnlyHint": true,
                "destructiveHint": false,
                "idempotentHint": true,
                "openWorldHint": false
            }
        },
        {
            "name": "diskmap_cleanup",
            "title": Text::new("可清理空间", "Reclaimable space"),
            "description": Text::new("找出可以清理的缓存、临时文件、日志、回收站、可重新下载的依赖包等，按大小排序，并给出可回收的总空间。只提供建议，不会删除任何东西。", "Find caches, temporary files, logs, trash, re-downloadable packages and the like, largest first, with the total reclaimable space. Suggestions only; nothing is deleted."),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": Text::new("文件夹绝对路径，省略时为扫描的根目录", "Absolute folder path; defaults to the scan root")
                    },
                    "limit": {
                        "type": "integer",
                        "description": Text::new("最多返回几项（1-200）", "Maximum items (1-200)"),
                        "default": 30
                    },
                    "min_mb": {
                        "type": "number",
                        "description": Text::new("只列出不小于这个大小（MB）的项目", "Only list items of at least this size (MB)"),
                        "default": 1
                    }
                },
                "required": [

                ],
                "additionalProperties": false
            },
            "annotations": {
                "title": Text::new("可清理空间", "Reclaimable space"),
                "readOnlyHint": true,
                "destructiveHint": false,
                "idempotentHint": true,
                "openWorldHint": false
            }
        },
        {
            "name": "diskmap_largest_files",
            "title": Text::new("最大的文件", "Largest files"),
            "description": Text::new("找出某个文件夹下（含所有子文件夹）最大的文件，可按类别筛选。", "Largest files under a folder (including all subfolders), optionally of one category."),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": Text::new("文件夹绝对路径，省略时为扫描的根目录", "Absolute folder path; defaults to the scan root")
                    },
                    "limit": {
                        "type": "integer",
                        "description": Text::new("返回几项（1-200）", "Number of items (1-200)"),
                        "default": 20
                    },
                    "category": {
                        "type": "string",
                        "enum": [
                            "core",
                            "running",
                            "system",
                            "config",
                            "user",
                            "cache",
                            "other"
                        ],
                        "description": Text::new("只看某一类", "Only this category")
                    }
                },
                "required": [

                ],
                "additionalProperties": false
            },
            "annotations": {
                "title": Text::new("最大的文件", "Largest files"),
                "readOnlyHint": true,
                "destructiveHint": false,
                "idempotentHint": true,
                "openWorldHint": false
            }
        },
        {
            "name": "diskmap_user_view",
            "title": Text::new("用户正在看什么", "What the user is viewing"),
            "description": Text::new("获取用户在 DiskMap 窗口里正在查看的文件夹和选中的项目（附带概览和详情）。用户说“这个”“我选中的”“现在看的”时使用。需要用户打开了 DiskMap 窗口。", "The folder the user is viewing and the item selected in the DiskMap window, with an overview and details. Use when the user says \"this\", \"the selected one\" or \"what I'm looking at\". Requires the DiskMap window to be open."),
            "inputSchema": {
                "type": "object",
                "properties": {

                },
                "required": [

                ],
                "additionalProperties": false
            },
            "annotations": {
                "title": Text::new("用户正在看什么", "What the user is viewing"),
                "readOnlyHint": true,
                "destructiveHint": false,
                "idempotentHint": true,
                "openWorldHint": false
            }
        },
        {
            "name": "diskmap_show",
            "title": Text::new("在窗口中指给用户看", "Show in window"),
            "description": Text::new("让用户的 DiskMap 窗口跳转到并高亮某个文件或文件夹，可附一句说明（会显示在窗口里）。用来给用户指出你正在说的东西。", "Make the user's DiskMap window jump to and highlight a file or folder, with an optional short note shown in the window. Use it to point out what you are talking about."),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": Text::new("文件或文件夹的绝对路径", "Absolute path of a file or folder")
                    },
                    "note": {
                        "type": "string",
                        "description": Text::new("显示给用户的简短说明", "Short note shown to the user")
                    }
                },
                "required": [
                    "path"
                ],
                "additionalProperties": false
            },
            "annotations": {
                "title": Text::new("在窗口中指给用户看", "Show in window"),
                "readOnlyHint": false,
                "destructiveHint": false,
                "idempotentHint": true,
                "openWorldHint": false
            }
        }
    ])
}
