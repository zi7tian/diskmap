# diskmap-cli（T07）

独立命令行程序，也是桌面版可调用的 Rust 库。`is_command(&args)` 识别五个子命令及旧版
`--selftest` / `--report`；`run(args) -> i32` 的参数包含程序名，返回退出码，不退出宿主进程。
桌面版仍可把 `DiskMap mcp`、`DiskMap analyze …` 交给这个入口。

```text
diskmap-cli analyze [路径] [--json] [--top N] [-o 文件] [--apparent] [--same-fs] [-q] [--lang zh|en]
diskmap-cli mcp [--no-attach] [--lang zh|en]
diskmap-cli serve [路径] [--port N] [--token TOKEN] [--apparent] [--same-fs] [--no-open] [--verbose] [--lang zh|en]
diskmap-cli scan [路径] [--json] [--method auto|generic|mft] [--threads N] [--apparent] [--same-fs] [--lang zh|en]
diskmap-cli selftest [--report 文件] [--limit 秒] [--lang zh|en]
diskmap-cli --selftest [--report 文件] [--lang zh|en]
```

`analyze` / `scan` 未传路径时使用系统盘。报告调用 core 的 analysis，进度只写 stderr，`-q` 关闭进度。
`scan` 不加载软件包数据库，只输出扫描统计，不写结果文件；`peak_rss_bytes` 为进程峰值，Linux 从
VmHWM 读取，Windows 从 PeakWorkingSetSize 读取，macOS 当前为 null。MFT 的自动选择与回退由 core 决定。

MCP 使用换行分隔的 JSON-RPC，支持 2025-11-25、2025-06-18、2025-03-26、2024-11-05 和全部九个工具。
默认动态读取 instance.json；窗口可用时经只连接 127.0.0.1 的 TCP HTTP 客户端工作，否则直接在本进程中
调用 App / analysis，不创建 HTTP 服务或实例登记。`--no-attach` 或非空 `DISKMAP_NO_ATTACH` 禁止接入窗口。
stdout 仅含协议消息；工具并发执行，不阻塞 ping。EOF 后按旧版等待在途工具，随后取消后台任务。

`serve` 调用 `serve_with_app`，首行输出带令牌的完整 URL。默认调用系统浏览器；`--no-open` 另打印当前语言的
“按 Ctrl+C 退出。”，持续运行到 Ctrl+C（Unix 也含 SIGTERM），忽略页面关闭和心跳超时。与旧版相同，非 Windows
的 root 用户默认不打开浏览器，先打印旧版提示，再按 `--no-open` 处理。请求打开浏览器失败时也保持服务等待。
仅浏览器模式遵循 App::ui_closed：首次无人连接 120 秒、告别 8 秒宽限、心跳失联 180 秒后退出。
正常退出关闭服务并清理本进程的实例登记。
浏览器接口仍要求扫描起点为有效 Unicode；`analyze` / `scan` 使用原生路径。没有必需的 core 接口改动。

自检在私有临时目录生成 401 个文件、340000 apparent bytes；检查覆盖 HTTP、扫描、运行进程、AI、
MCP JSONL 往返和实例清理。MCP 使用同一 JSONL 引擎的内存流，避免要求库宿主能重新启动自己；独立进程
stdio 另由集成测试覆盖。刷新进程信息后轮询 `/api/status`，最多等待 60 秒；连续两次确认刷新结束后，
使用第二份状态检查进程和在用文件数量，避免初始化或慢刷新时误判空快照。等待超时则这两项均失败，
报告完整的超时说明和最后状态。正常返回 0、运行失败 1、参数错误 2、自检总超时 3。`--limit` 是新增 CI 选项，
默认 150 秒。临时目录由监督方持有；正常、失败或超时都会先取消后续写入，关闭自检服务，再删除临时目录。
Windows 短暂占用会重试；监督方最多额外等待 3 秒清理。系统调用卡住、权限拒绝或清理线程无法启动时，
报告明确标记清理失败并保留完整路径，不把残留误报为成功。自检报告 frozen=true，检查文字双语。

JSON 对象键序和排版不保证与 Python 相同；MCP text 中的 JSON 与 structuredContent 内容相同。
core 已记录的排序平局、完整索引投影、NTFS 和进程检测边界仍适用。非法 JSON 的非有限数按 JSON 规范拒绝。

## 验证

```bash
cargo build -p diskmap-cli
python3 -B crates/diskmap-cli/tests/cli_compare.py
cargo test -p diskmap-cli
```

对比脚本自行隔离 cache/config，在同一个临时目录上运行真实新旧 CLI。覆盖中英 × 四协议 × 窗口/后台，
所有工具、错误参数、协议错误，以及 JSON/Markdown、文件/stdout、实际占用/文件长度报告。
只归一化版本、用时、完成/UI 时间、磁盘实时占用和全局进程计数；保留实际分析字段。
Linux 持续以只读 FD 打开测试树的目录，让两版采集到稳定的目录在用集合，避免扫描器临时 FD 引入竞态。
测试另覆盖语言选项、非正常输入、扫描统计、页面告别、SIGINT 实例清理、自检两入口和超时退出。
