# DiskMap 黑盒契约测试（T09）

只用 Python 标准库和 `unittest`，启动真实的 HTTP 服务与 MCP 子进程。每次只测一个实现，
不导入 `legacy/backend`，不运行另一实现作为 oracle，也不读取并排对比脚本的输出。
大小、数量、排序、聚合、类别分布、清理总量和占比均从本次生成的文件计算。
旧版退役后，默认的 Rust 测试仍可单独运行。

## 运行

在仓库根目录运行，建议 Python 3.10 或更新版本。旧版程序自身需要 `psutil`；测试脚本不需要第三方包。

```bash
cargo build -p diskmap-cli
python3 -B -m unittest discover -s tests/contract -v

# 只改一个环境变量，运行完全相同的测试。
DISKMAP_CONTRACT_IMPL=legacy python3 -B -m unittest discover -s tests/contract -v
DISKMAP_CONTRACT_IMPL=rust python3 -B -m unittest discover -s tests/contract -v
```

`DISKMAP_CONTRACT_IMPL` 默认 `rust`，只接受 `rust|legacy`。Rust 默认依次查找
`target/debug/diskmap-cli`、`target/release/diskmap-cli`（Windows 带 `.exe`）；
`DISKMAP_CONTRACT_BIN` 可以指定另一个二进制文件，路径可以含空格：

```bash
DISKMAP_CONTRACT_BIN=/path/to/diskmap-cli python3 -B -m unittest discover -s tests/contract -v
```

找不到程序时直接报错并提示 `cargo build -p diskmap-cli`，不会自动编译、下载或跳过测试。
调试版优先，所以每次验证 Rust 改动前应先构建。单独运行一组可用 `-p test_http.py`、
`-p test_mcp.py` 或 `-p test_oracles.py`。

Windows PowerShell：

```powershell
cargo build -p diskmap-cli
$env:DISKMAP_CONTRACT_IMPL = 'rust'
python -B -m unittest discover -s tests/contract -v
$env:DISKMAP_CONTRACT_IMPL = 'legacy'
python -B -m unittest discover -s tests/contract -v
# 可选；不指定时仍会查找 target/debug、target/release。
$env:DISKMAP_CONTRACT_BIN = 'C:\path with spaces\diskmap-cli.exe'
```

## 数据和覆盖范围

基础树有 **167 个文件、6 个子目录、5,442,196 字节**。所有文件关闭写入句柄后才启动服务。
树中含不同大小的普通文件、中文/空格/`+&` 路径、零字节文件，以及同一目录的 160 个文件。
根目录取名 `cache`，让三个系统的静态分类都由公开分类规则确定为缓存。
这不替代 T02 对七类路径规则的测试。

| 测试组 | 检查内容 |
|---|---|
| `test_http.py`：认证、前端、元数据 | 全部 GET/POST API 的令牌和 Host 校验；首页令牌、双语 HTML 替换；四个静态资源的实际内容、类型和白名单；UTF-8 中文不转义；JSON 安全响应头；七类编号及双语标签；磁盘、系统和 MCP 接入信息结构。 |
| `test_http.py`：扫描、树、详情 | 完成状态、精确文件/目录/字节数；150 项保留和 10 项聚合；`frac` 上下界、递归面积守恒、零大小聚合、48 层截断；大小、mtime、mode、Unix owner/group；扫描后文件消失；文件内容未被读取接口修改。 |
| `test_http.py`：agent、状态操作 | 全部七个 agent GET 和两个 POST；概览、排序、占比、所有类别筛选、数量裁剪和 Unicode 数字；清理阈值不改变过滤前总量；扫描范围/统计方式复用、取消及重扫；UI 上报、用户视图、跳转编号和 Unicode 备注截断；AI 请求不维持界面心跳；`bye` 的查询令牌和 204。 |
| `test_http.py`：配置、错误、平台 | 临时设置的读写和重启读取，CLI > 环境变量 > 设置的优先级；400/403/404/500 的错误 JSON 和确定的消息；`reveal` 的鉴权及未扫描路径拒绝；实际占用的独立系统元数据；打开/映射文件的 PID、用途、运行类别和关闭后的刷新；Windows 大小写/分隔符别名；Unix 硬链接只计一次、不跟随符号链接；实例字段、Unix 0600、正常退出和多实例登记所有权。 |
| `test_mcp.py`：stdio 协议 | 两种语言，四个协议版本及未知版本回退；九个工具的 schema、默认值、required、annotations 和双语标题；initialize、ping、tools/list、空资源列表、通知不回复、客户端响应忽略、批量请求；解析错误 -32700、非法请求 -32600、未知工具 -32602、未知方法 -32601。 |
| `test_mcp.py`：工具和后端 | 中英 × 窗口连接/独立扫描，共四种组合，调用全部九个工具及相应错误；按行检查响应 id，成功文本解析后必须等于 structuredContent；精确扫描/概览/列表/解释/清理/最大文件和子范围复用；附着模式核对 UI 视图和跳转，后台模式核对窗口工具错误；EOF 正常退出；`DISKMAP_NO_ATTACH` 覆盖已有登记。status 无参数，保留旧版忽略多余参数的行为。 |
| `test_oracles.py` | 单位/精度计算的边界。Windows 目录元数据记录的字段偏移、Unicode、多记录、截断及非法边界。旧版 Windows 进程预算跳过判定的 40 组组合，覆盖实现、平台、11.9 秒阈值和在用状态，使用纯数据在 Linux 验证。 |

取消请求与扫描结束存在竞争，最终允许 `cancelled` 或已经完成的 `done`，但都核对计数上界、
字节和文件数关系；`done` 必须完整统计，`cancelled` 必须明确提示数据不完整。随后重新扫描基础树并检查全部计数。
没有把不同线程调度下的中间结果当成固定数值。

实时磁盘使用量、耗时、全局软件包/进程统计检查类型、范围和结构；没有硬编码本机环境。
Linux 的异步进程采集可能捕获扫描器临时打开的目录 FD，目录的在用计数及相应建议按返回计数校验；
文件在用场景则由测试进程持有唯一的打开文件/映射，精确核对 PID、用途、1 个文件和 8,193 字节。

## 有意差异

差异都限定在对应平台、实现和字段，不删除整块结果或接受任意错误。

| 差异及出处 | 本测试的具体期望 |
|---|---|
| T01/T05 完整索引 | `many/f000.bin` 属于最小的 10 项：旧版 `/api/info` 返回明确的 404；Rust 返回该文件完整详情和 1,000 字节。树及分析列表仍按 150 项投影聚合。 |
| T04/T06/T15 Windows procinfo 路径 | 只有 `summary.in_use.largest[*].path`，旧版是本次实际路径的 `.lower()`，Rust 保留真实拼写。普通 info、explain、largest、show 的索引路径仍严格等于实际拼写；不对所有路径统一 lower/replace。分隔符按宿主 `os.sep` 断言，Windows 别名测试同时输入 `/` 和混合大小写。 |
| T04 旧版 Windows 进程采集预算 | `legacy/backend/procinfo.py` 的 `ProcInfo.refresh(budget=12.0)` 调用 `_collect_psutil`，119–120 行在超预算后只收可执行文件，略过后续 PID 的映射和打开文件。仅当实现为 legacy、平台为 Windows、显式 `/api/refresh-procs` 请求耗时 ≥11.9 秒（12 秒预算减 0.1 秒容差），且测试文件未被识别为在用时，整项测试标记 skip，并输出含代码位置和行为的中英原因。等待已有异步采集的时间排除在计时外。持有映射和关闭句柄后的刷新都用相同条件，关闭句柄并完成第二次刷新后才报告整项跳过。Rust、其他平台、未达阈值，以及已识别在用的情况都执行严格断言。 |
| T01/T14 Windows allocated | 旧版读取所在盘符根目录的实际簇大小，对每个文件长度向上取整求和（与旧版相同，目录挂载卷也仍查盘符根目录）。Rust 读取文件所在目录的 `FILE_FULL_DIR_INFORMATION.AllocationSize`；本地非空且分配为零的文件独立查询 `GetCompressedFileSizeW`。逐文件与总和均精确匹配，不用 apparent 到旧值之间的范围断言，也不把压缩 API 总和误当全部文件的目录分配大小。Unix 两版都严格等于 `st_blocks * 512`。 |
| T05 非空字典备注的内部异常 | Python <3.12 的旧版精确返回 `TypeError("unhashable type: 'slice'")`；Python ≥3.12 旧版及 Rust 精确返回 `KeyError(slice(None, 200, None))`，都为 500。 |
| T07 MCP JSON 键序/缩进 | 文本必须能解析为与 structuredContent 完全相等的 JSON 对象；不要求对象键序或空白一致。 |

## 隔离、清理和验收边界

每个工作目录自动设置临时 `DISKMAP_CACHE_DIR`、`DISKMAP_CONFIG_DIR`、`TMPDIR`、`TMP` 和 `TEMP`。
测试只在自己创建的树里写入、造链接或删除“扫描后消失”的文件，不接触用户真实设置或登记。
服务用 `--no-open`，没有启动浏览器、桌面窗口或真正打开文件管理器。

进程所有权复用 `crates/diskmap-core/tests/compare_support/process.py`：Unix 使用独立进程组，
Windows 使用带 `KILL_ON_JOB_CLOSE` 的 Job。构造失败、断言异常、超时、Ctrl+C 都经 cleanup/atexit
回收子进程后再删除临时目录；SIGTERM/SIGHUP 转成正常的 Python 退出也执行清理。
stdout/stderr 始终严格 UTF-8，stdin/读取/等待有超时。MCP 正常场景先关闭 stdin，要求退出码 0。
Unix 另向自己启动的服务发送 SIGINT，检查正常退出和实例清理。

Linux 上运行整套应远小于每种实现两分钟。Claude 第 2 轮在约 300 个进程的 Windows 管理员 SSH 会话中
运行原 28 项测试，使用 Python 3.10.11 和 psutil 7.2。Rust 约 13 秒（13.2 秒），跳过 2 项 Unix 测试，
路径别名、allocated 和在用文件检查通过。旧版约 98 秒（98.1 秒），主要花在每次约 12 秒的进程采集上，
在用文件测试因上述预算限制失败。本轮新增该条件的跳过处理，需 Claude 在同机复测。
Windows 不创建需要权限的符号链接，也不发送 Unix 信号；这两组 Unix 测试会标明跳过。
macOS 未做真机验收。真实 `reveal`、GUI、跨真实分区/挂载点、MFT 整卷、权限受限目录、CLI 报告和
超大/慢 HTTP 传输不在本套测试范围内，分别由对应模块测试和人工验收覆盖。
