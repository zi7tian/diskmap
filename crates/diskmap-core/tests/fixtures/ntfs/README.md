# NTFS 合成镜像与 Windows 手动验收

本目录不包含下载的二进制镜像。`synthetic.rs` 在每个测试的私有临时目录中生成卷，测试退出时只删除自己
创建的镜像和目录。没有调用 ntfs-3g、挂载卷或运行下载程序；仓库中的镜像压缩体积为 0。
生成器与项目使用相同许可证；结构依据如下，不复制第三方镜像字节：

- [Microsoft：Master File Table](https://learn.microsoft.com/en-us/windows/win32/devnotes/master-file-table)
- [Microsoft：FILE_RECORD_SEGMENT_HEADER](https://learn.microsoft.com/en-us/windows/win32/devnotes/file-record-segment-header)
- [Microsoft：ATTRIBUTE_RECORD_HEADER](https://learn.microsoft.com/en-us/windows/win32/devnotes/attribute-record-header)
- [Linux NTFS：layout.h](https://kernel.googlesource.com/pub/scm/linux/kernel/git/torvalds/linux/+/865730eec5435ce40a5dc3c615077d04c8f95098/fs/ntfs/layout.h)

## Linux / macOS / Windows 自动测试

```sh
cargo test -p diskmap-core --test ntfs_images
cargo test --release -p diskmap-core --test ntfs_images million_record_benchmark -- --ignored --nocapture
```

普通测试生成 1 MiB 的分区镜像：512 字节扇区、4096 字节簇、1024 字节 FILE 记录，MFT 分为两段，
中间有未映射区域；第二段包含有效文件。驻留和非驻留 `$ATTRIBUTE_LIST` 使用同一组预期结果。
覆盖修正序列、SI、UTF-16 文件名、DOS 名、驻留/非驻留 DATA、跨目录硬链接、ADS、稀疏、压缩、云文件、
目录联接、目录符号链接、其他目录重解析点、元文件归类和 `$BITMAP` 跳过损坏的未使用记录。
错误案例包含截断、引导扇区损坏、属性零长度、修正序列不符、父记录丢失/代号不符、目录环和扩展记录复用。
单条记录的异常只跳过受影响条目或使用保守口径，记录到 `anomalies`，健康记录继续进入索引；只有引导扇区、
`$MFT` 运行列表或实际读取失败才终止整次 MFT 扫描。`$MFT` 自身的属性列表扩展仍须能补全可读映射。
同一流各段的压缩/稀疏 flags 取并集，覆盖 Windows 报告中六段 flags 不同的布局；DATA 全在扩展记录、
扩展记录先于基记录、多名字和 EA 位的合成回归也在这里。固定种子的字段突变检查异常输入不导致 panic。
`$BadClus` 的 8 号记录另有专门回归：命名流 `$Bad` 可在未设置 SPARSE 标记时含空洞，逻辑长度覆盖全卷；
实际占用只累计运行列表中的物理坏簇，无坏簇时为 0，有一个坏簇时为 4096 字节。
这项例外只允许记录8的 `$Bad` 流，普通记录或其他流的同样布局记为异常并隔离；文件长度口径保留全卷逻辑长度。
测试还验证扫描前后镜像字节完全相同，以及开始前/读取途中取消不发布完整目录树。

百万记录测试流式写出约 977 MiB 临时镜像，含 1,000,000 个用户文件、MFT 和根目录；每个文件的名字为
`file-NNNNNNN.txt`，驻留 DATA 为 9 字节。生成阶段内存保持常量，不把整个镜像读入内存。
输出生成耗时、解析耗时、建树耗时、总扫描耗时、临时结构容量、最终索引容量，以及 Linux 进程 `VmHWM`。
峰值 RSS 是整个测试进程峰值，分别输出扫描前和扫描后值；生成阶段仅有约 1 MiB 写缓冲区和 bitmap。
记录测试机器 CPU、存储、Rust 版本、命令和冷热缓存条件；此合成结果不能代替 Windows 实盘/WizTree 对比。

T03 历史测量（2026-11-15）：Intel i5-11320H（4 核/8 线程）、Arch Linux 7.3.4、rustc 1.100.0，
使用上面的 release 命令单次运行；`/tmp` 为 tmpfs，刚生成后立即读取，不代表物理磁盘吞吐。
1,000,002 个有效 FILE 记录（用户文件 + MFT + 根目录），生成 0.440 s，解析 0.390 s，
建树 0.490 s，总扫描 0.881 s，异常记录数为 0；解析阶段临时结构容量 107.12 MiB，最终索引 30.52 MiB。
扫描前 `VmHWM=4240 KiB`（4.14 MiB），扫描后 `VmHWM=239116 KiB`（233.51 MiB）。
这些容量不包括 tmpfs 镜像占用的系统内存；树构建时的排序、目录项及索引分配使进程峰值高于解析临时容量。

## T14 读取诊断与延迟模拟

第三轮 Windows 验收中 C: 为 12.4–13.3 秒，
约 6.2 万次随机小读占去约 8 秒。本轮将普通扩展记录改为顺序收集、延后组装，外置属性列表及重解析数据
按物理位置合并读取；使用两个 4 MiB 缓冲让顺序读取与解析重叠，不把整个 `$MFT` 放进内存。
保留已有的属性引用/序号/完整性校验与异常隔离规则。

`scan --method mft` 和 `scan --image` 新增以下诊断，导出 JSONL 的 summary 也包含同名字段：

| 字段 | 口径 |
|---|---|
| `read_stats.sequential` | 顺序读取 `$MFT` 窗口；跨物理运行段时可能一次窗口有多个请求 |
| `read_stats.extension` | 单独读取扩展记录；普通文件扩展从顺序扫描取得，引导补齐 `$MFT` 本身所需的读取仍保留 |
| `read_stats.attribute_list` | 非驻留属性列表的读取；驻留内容或缓存命中不增加次数 |
| `read_stats.reparse` | 非驻留重解析数据的读取；相近物理区间会合并 |
| `read_stats.other` | 引导扇区、初始 MFT 记录、位图等其他读取 |
| `collection_bytes` | 基础/扩展收集表及紧凑属性区的堆容量，不是进程峰值，也不包含完整原始 MFT |
| `extension_records` / `deferred_records` | 已收集的扩展记录数 / 推迟组装的基础记录数 |

每类读取含 `calls`、`bytes`、`elapsed_seconds`；各类次数与字节合计应等于 `read_calls`、`read_bytes`。
字节数是数据源请求量，包含合并读取跨过的间隙，不等于 Windows 对齐后设备实际传输量。
终端 `collection / 收集表` 行以 `bytes`、`extensions`、`deferred` 打印后三项。

`io_busy`（JSON 为 `io_busy_seconds`）是五类实际读取耗时之和；`io_overlap`（JSON 为
`io_overlap_seconds`）是其中被解析覆盖的时间。`stages.io` 只计未被覆盖的读取等待，与 `parse`、`tree`、
`classify`、`hardlinks`、`index` 保持互斥，可相加得到扫描阶段墙钟时间。不要再把 `io_busy` 与纯解析时间
直接相加；首行旧字段 `parse=... build=...` 仍分别表示“读盘加解析”和“建树加分类等”的合并时间。

Linux 可用同一只读分区镜像比较正常读取及每请求固定延迟：

```sh
cargo run --release -p diskmap-core --example scan -- --image partition.img --repeat 3 --digest
cargo run --release -p diskmap-core --example scan -- --image partition.img --read-delay-us 128 --repeat 3 --digest
```

`--read-delay-us` 仅允许与 `--image` 一起使用，默认没有延迟；每次实际读取前 `sleep` 指定时长，
缓存命中不等待。操作系统调度可能使实际等待超过 128 微秒，输出按实测耗时记录；它只模拟每请求的固定开销，
不模拟设备带宽、BitLocker 或 Defender 本身。记录正常/延迟两组的阶段时间、读取次数、摘要和峰值 RSS。
真实 Windows 卷验收使用下文原命令，**不要加 `--read-delay-us`**。

## Windows 生成测试卷

首次 Windows 11 验收已确认合成 VHD 的各项行为，第二轮验收已确认 C:、D: 的 MFT 正确性；T14 继续验证速度和通用扫描压缩文件回退。
以下步骤用于重建可丢弃测试卷；真实卷验收使用下一节的一键脚本。管理员 PowerShell 中先进入仓库根目录。
只使用新建的临时 VHD，不将任何命令中的测试盘符替换成已有数据卷。命令出错时停止，不跳过错误继续执行。
DiskPart 的 [attach vdisk 文档](https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/attach-vdisk)
说明了虚拟卷的挂载及只读挂载方式。

```powershell
$ErrorActionPreference = 'Stop'
$NtfsWork = Join-Path $env:TEMP ('diskmap-ntfs-' + [guid]::NewGuid())
New-Item -ItemType Directory $NtfsWork | Out-Null
$NtfsVhd = Join-Path $NtfsWork 'fixture.vhd'
$NtfsLetter = ([char[]](82..90) | Where-Object { -not (Test-Path ("{0}:\" -f $_)) } | Select-Object -First 1)
if (-not $NtfsLetter) { throw '没有可用测试盘符' }
$NtfsRoot = "${NtfsLetter}:\"
@"
create vdisk file="$NtfsVhd" maximum=256 type=expandable
select vdisk file="$NtfsVhd"
attach vdisk
create partition primary
format fs=ntfs unit=4096 label=DiskMapTest quick
assign letter=$NtfsLetter
exit
"@ | Set-Content -Encoding ASCII (Join-Path $NtfsWork 'create.txt')
diskpart /s (Join-Path $NtfsWork 'create.txt')
if ($LASTEXITCODE -ne 0 -or -not (Test-Path $NtfsRoot)) { throw '测试卷创建失败' }
Get-Volume -DriveLetter $NtfsLetter | Format-List DriveLetter,FileSystem,FileSystemLabel,Size
$env:DISKMAP_CACHE_DIR = Join-Path $NtfsWork 'cache'
$env:DISKMAP_CONFIG_DIR = Join-Path $NtfsWork 'config'

New-Item -ItemType Directory (Join-Path $NtfsRoot 'a'),(Join-Path $NtfsRoot 'z') | Out-Null
[IO.File]::WriteAllBytes((Join-Path $NtfsRoot 'a\tiny.txt'), [byte[]](1,2,3))
[IO.File]::WriteAllBytes((Join-Path $NtfsRoot 'a\normal.bin'), (New-Object byte[] 8192))
fsutil hardlink create (Join-Path $NtfsRoot 'z\normal-link.bin') (Join-Path $NtfsRoot 'a\normal.bin')
[IO.File]::WriteAllBytes((Join-Path $NtfsRoot 'streams.bin'), [byte[]](1,2,3))
[IO.File]::WriteAllBytes((Join-Path $NtfsRoot 'streams.bin:small'), [byte[]](4,5,6,7))
[IO.File]::WriteAllBytes((Join-Path $NtfsRoot 'streams.bin:big'), (New-Object byte[] 8192))
[IO.File]::WriteAllBytes((Join-Path $NtfsRoot 'compressed.bin'), (New-Object byte[] 131072))
compact /c /i (Join-Path $NtfsRoot 'compressed.bin')
[IO.File]::WriteAllBytes((Join-Path $NtfsRoot 'sparse.bin'), [byte[]]@())
fsutil sparse setflag (Join-Path $NtfsRoot 'sparse.bin')
$NtfsSparse = [IO.File]::OpenWrite((Join-Path $NtfsRoot 'sparse.bin'))
try { $NtfsSparse.SetLength(1048576); $NtfsSparse.Write((New-Object byte[] 4096),0,4096) } finally { $NtfsSparse.Dispose() }
cmd /c mklink /J (Join-Path $NtfsRoot 'junction') (Join-Path $NtfsRoot 'a')
cmd /c mklink /D (Join-Path $NtfsRoot 'directory-link') (Join-Path $NtfsRoot 'a')
New-Item -ItemType Directory (Join-Path $NtfsRoot 'denied') | Out-Null
[IO.File]::WriteAllBytes((Join-Path $NtfsRoot 'denied\hidden.bin'), (New-Object byte[] 8192))
$NtfsSid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
icacls (Join-Path $NtfsRoot 'denied') /deny "*${NtfsSid}:(OI)(CI)(RX)"
fsutil fsinfo ntfsinfo $NtfsRoot
fsutil file queryextents (Join-Path $NtfsRoot 'sparse.bin')
fsutil hardlink list (Join-Path $NtfsRoot 'a\normal.bin')
Get-Item (Join-Path $NtfsRoot 'streams.bin') -Stream *
```

需要扩展记录时，在测试卷新建一个文件，再用循环建立数百个不同名字的硬链接或命名流；先用
`fsutil file queryfileid` 记录其 MFT 编号，再核对解析结果。高度碎片化的 `$MFT`、非驻留属性列表、
4Kn 卷另外在可丢弃的测试 VHD 上验证。OneDrive 仅在线文件需在已有 OneDrive 测试目录手工创建/释放空间，
不要在真实用户目录批量改属性；记录逻辑长度、属性和实际占用，比较两种扫描口径。

## 对比命令与验收表

构建在测量前完成；Windows 上 `--image` 接收从 NTFS 分区起点开始的原始镜像，不直接解析 VHD/VHDX 容器。

```powershell
cargo build --release -p diskmap-core --example scan
& .\target\release\examples\scan.exe $NtfsRoot --method generic --repeat 3 --digest
& .\target\release\examples\scan.exe $NtfsRoot --method mft --repeat 3 --digest
& .\target\release\examples\scan.exe $NtfsRoot --method generic --apparent --repeat 3
& .\target\release\examples\scan.exe $NtfsRoot --method mft --apparent --repeat 3
& .\target\release\examples\scan.exe $NtfsRoot --method auto
& .\target\release\examples\scan.exe (Join-Path $NtfsRoot 'a') --method auto
& .\target\release\examples\scan.exe C:\ --method generic --repeat 3
& .\target\release\examples\scan.exe C:\ --method mft --repeat 3
```

在普通（未提权）终端再次运行 `--method auto` / `--method mft`，以及非 NTFS 测试卷的自动/强制模式。
自动模式应回退、保留双语原因；强制 MFT 应报告错误。子目录自动模式应选择通用扫描。
扫描途中取消，核对记录进度会增长、取消及时、没有把半棵 MFT 树发布成完成结果。

| 项目 | 要记录/核对的内容 |
|---|---|
| 基本数字 | Windows 版本、盘符/卷标、文件系统、簇/扇区大小、权限、文件/目录数、总字节、记录数、异常记录数 `anomalies` |
| 大小 | 驻留 3 字节实际占用按目录枚举或 `GetFileInformationByHandleEx(FileStandardInfo).AllocationSize` 核对，应为 8；普通非驻留、稀疏、压缩同样记录所用 API |
| 硬链接 | MFT 只计一次且固定选择名字排序的深度优先首项；通用 Windows 扫描重复计数，差值逐组解释 |
| ADS | MFT 将所有 DATA 流累加到所属文件；通用扫描只读默认流，差值按流大小列出 |
| 元文件 | 记录 0–15 和 `$Extend` 后代文件列在卷根、类别 Core；目录自身索引块不计 |
| 重解析/云 | junction 不出现；目录 symlink 作为文件；其他目录重解析点进入；仅在线文件实际0、apparent保留长度 |
| 权限 | 通用扫描 denied 样例与 MFT 额外读取的文件逐项核对 |
| 性能 | 通用/MFT/WizTree 各冷缓存一次、暖缓存三次，中位耗时、文件数、大小口径；MFT 同时记录六阶段、五类读取次数/字节、io_busy/io_overlap |
| 内存 | 示例输出的索引 MiB 和进程 Peak Working Set；记录进程启动/结束，每次新进程比较，重复扫描的峰值不会重置 |
| 稳定性 | 写入测试数据后关闭句柄、静置再扫；SVI 自动新增和稀疏延迟分配仍可能变化，记录两次扫描前后卷状态；只有同一只读镜像/只读挂载卷才要求 digest 一致 |

WizTree 使用当前安装版本手动选相同卷和分配大小/文件长度口径，记录界面扫描耗时、文件数和总量，
并导出报告作逐项核对；记录版本及是否将 ADS/元文件/硬链接按相同方式统计。首次验收机器没有安装 WizTree，
本轮 Linux 开发环境也没有 Windows，不填虚构的修复后实盘或 WizTree 数字。

不能用 `GetCompressedFileSize` 验证驻留文件的 8 字节分配口径：Windows 实测它对 3 字节驻留文件返回 3，
对 `streams.bin` 也只给默认流，不能代表 ADS 总占用。T14 的通用扫描仅在目录枚举分配大小为 0、逻辑长度大于 0、
非仅在线云文件且使用实际占用口径时回退到 `GetCompressedFileSizeW`；其余仍使用原目录枚举值。
独立工具应使用 `FileStandardInfo.AllocationSize`，命名流另用
[`FileStreamInfo.StreamAllocationSize`](https://learn.microsoft.com/en-us/windows/win32/api/winbase/ns-winbase-file_stream_info)。

零大小问题的合成对照确认：旧代码将原始 `$STANDARD_INFORMATION` 中表示扩展属性 EA 的 `0x40000`，
误当作目录枚举语境中的 `RECALL_ON_OPEN`，从而把带 EA 的本地文件误判为仅在线文件并清零实际占用。
这两个常量同值但适用语境不同，依据 [Microsoft 文件属性常量文档](https://learn.microsoft.com/en-us/windows/win32/fileio/file-attribute-constants)
及 [Microsoft 工程师对同值问题的说明](https://learn.microsoft.com/en-us/answers/questions/1294919/file-attribute-constants-strange-value)。
修复已去掉原始 SI 云文件判断中的 EA 位；合成镜像可复现修复前清零、修复后恢复正确大小。
作为对照，全部 `$DATA` 位于扩展记录、扩展记录先于或晚于基本记录、同一文件多个硬链接名字的布局，
在修复前已能正确取到大小，因此这些布局本身不是此次合成复现的原因。第二轮 Windows 验收已经确认成片的零大小
现象不再出现，MFT 总额补回约 55.7 GB；该数字只作为用户报告中的修前/修后对照。

已知差异：Windows 报告在同一个稀疏文件上观测到管理员通用扫描为 0、普通会话为 65,536 字节；
目录缓存中的 `FILE_FULL_DIR_INFO.AllocationSize` 与 MFT 分配信息可能暂不同步。T14 只修复上述零分配大小回退，
不重算所有非零目录枚举值。句柄口径与 MFT 完全吻合的差额单列 `compressed_sparse`，证据不足仍为 `other`，不能当作 ADS。
活动卷扫描既非 VSS 快照，也不会锁卷：文件删除、移动、大小变化和 SVI 自动创建都可能产生对账余项。

## 一次重跑真实卷验收

先构建，再在管理员 PowerShell 7 中执行（C:、D: 可以换成要验证的本地 NTFS 盘符）：

```powershell
cargo build --release -p diskmap-core --example scan
# 在新进程中测 MFT 的六阶段；重复三次用于观察暖缓存，进程峰值是三次的最高值。
& .\target\release\examples\scan.exe C:\ --method mft --repeat 3
# 正式对账会让每种方式使用独立进程，并记录各自峰值及阶段耗时。
& .\crates\diskmap-core\tests\fixtures\ntfs\accept-volumes.ps1 -Volume @('C:', 'D:')
# 如需按逻辑长度重跑，开启新一次验收，输出会进入另一个临时目录。
& .\crates\diskmap-core\tests\fixtures\ntfs\accept-volumes.ps1 -Volume @('C:', 'D:') -Apparent
# 可用 -ScanExe 指定已构建的 scan.exe；脚本本身不构建或下载程序。
```

脚本只读取给定卷和元数据；所有输出都用 CreateNew 写到新建的 `%TEMP%\diskmap-ntfs-accept-<GUID>`。
不删除原文件、不启用调试/备份特权，不读取普通文件内容；元数据句柄仅请求 `FILE_READ_ATTRIBUTES`，并带
`OPEN_REPARSE_POINT | OPEN_NO_RECALL`，不跟随重解析点或请求云文件回迁。
验收诊断另外以 `GENERIC_READ` 打开卷，使用只读 `FSCTL_QUERY_USN_JOURNAL` / `FSCTL_READ_USN_JOURNAL`
获取扫描时间窗的变化证据；不创建、修改或删除 journal。产品扫描本身不使用 USN。
它先对每个卷各做一次 MFT/通用扫描，记录 scan 的耗时、六阶段、峰值工作集、文件/目录数、总量、已处理记录和异常数；
失败即停止并留下日志，不能把回退的通用扫描当作 MFT 成功。随后逐条读取导出的 UTF-8 JSONL，对账耗时另计；摘要、结束行和逐条统计必须吻合，截断清单会被拒绝。
路径表使用 `Dictionary<string,long>`，不会保存数百万个 PowerShell 对象；每次只处理一个卷。

输出包括每种扫描的 `.log` 和 `.jsonl`、扫描前后卷容量/空闲空间/UTC 状态、USN 窗口状态、每条差异的 JSONL、对账 JSON。
正数表示 MFT 比通用扫描多，负数表示少；七类字节差和文件数差必须严格合计为两个摘要的差，否则脚本报错：

- `hardlinks`：只在通用扫描中出现，并通过硬链接枚举和相同卷/文件 ID 找到 MFT 已计入的别名。
- `ads`：`FileStreamInfo` 查得完整命名 DATA 流总量，当前默认流大小与通用扫描清单吻合，正差足以包含 ADS；只归入可证实的部分。
- `metadata`：MFT 清单中的卷根 `$` 文件且类别为 Core，依据扫描器的元文件放置规则。
- `permission`：MFT 独有路径位于通用索引实际标记 DENIED 的目录下；不用 `System Volume Information` 名字猜测。
- `compressed_sparse`：实际占用模式中，`FileStandardInfo.AllocationSize` 加上 `FileStreamInfo` 的 ADS 总量与 MFT 完全吻合；
  ADS 部分单列，剩余默认流与通用扫描的差额归此类。句柄查询所得逻辑长度也须与流查询一致。
- `race`：USN 在 MFT 开始至通用扫描结束的精确时间窗内，记录了同文件 ID 的数据/流/压缩变化，或同父目录 ID、
  同名字的创建/删除/重命名。记录的 journal ID、USN、时间戳、累计原因、新增原因、文件和父目录 ID 写入每条证据。
  不按缓存、日志后缀猜测；当前查询返回 NotFound 本身不能证明两次扫描之间发生删除。
- `other`：其余无法证实的差额，包括元数据查询失败、异常记录、无法匹配身份的变化。
  对账 JSON 和终端列出绝对差额最大的 20 个路径，完整差异仍保存在 JSONL。

默认 `%TEMP%` 通常在 C:；第一份导出清单和日志可能被随后扫描计入，这也是可解释的现场变化。
脚本给本次输出目录下的差异加 `Acceptance-generated temporary output` 证据，并单列其字节/文件数差额、
扣除这些条目后的净差额；原始七类对账仍与扫描总量严格一致，不悄悄修改扫描数字。
若要避免这种扰动，可每次只验一个盘，并在调用前将进程的 TEMP 指向另一个未验收卷中自己拥有的临时目录。

USN 读取设有 15 秒处理预算、64 MiB、200,000 条记录上限，只支持 NTFS 的 V2 记录并校验长度、名字边界、游标推进。
时间预算在每次同步系统调用之间检查，不会取消正在执行的系统调用。
关闭 journal、期间截断/重建、不支持版本、超限或旧版本导出缺少精确扫描时间戳时，都把证据标为不可用；
不会将读取失败当作“无变化”。USN 的非 CLOSE 记录也会累计原因：脚本按同一文件 ID 的连续记录只取新增原因位，
见到 CLOSE 后重置基线，CLOSE 本身不作变更证明。首条没有历史基线或原因意外减少时，只接受单一原因位；
多位累计原因无法判定哪个发生在窗口内，保守留在 `other`。因此窗口前写入、窗口内只改时间戳不会误归为写入竞态。
父目录已消失或身份无法取得时，可能无法证明删除；这类条目保留在 `other`。
原理和结构见 Microsoft 的 [读取 USN journal](https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ni-winioctl-fsctl_read_usn_journal)、
[USN_RECORD_V2](https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ns-winioctl-usn_record_v2)、
[变化原因如何累计](https://learn.microsoft.com/en-us/windows/win32/fileio/change-journal-records) 文档。

文件身份和流信息在扫描后查询，证据带有时间差；脚本不会把无法证明的差异强行归类。
第三轮后 Claude 决定的实机目标是 C: MFT 不超过 **6 秒**、峰值工作集不超过第三轮 511 MiB 的 **1.1 倍**，
同时核对 `collection_bytes`、`extension_records`、`deferred_records`，并继续解释 `other`；
已证实的竞态单列，不能仅因包含 `other` 的字节已经对平就判为验收通过。
原第二轮 C: MFT 为 17.95 秒、通用为 24.87 秒，D: 为 1.88 秒和 2.12 秒。本机合成镜像的阶段计时
不能代替 Windows 实盘数字。
本开发机没有 PowerShell/Windows，脚本已静态审查，仍需用户在 Windows 执行此步骤。

## 导出只读原始分区镜像与收尾

先关闭写入测试卷的程序和文件句柄，静置，记录在线扫描结果。把刚创建的测试 VHD 卸载后只读重挂，
再用只读卷句柄导出；这里只能操作上面自己新建的 `$NtfsVhd`，不要把命令指向真实卷。
下列输出文件位于本次新建的 `$NtfsWork`，输入使用 `FileAccess.Read`，输出用 CreateNew：

```powershell
@"
select vdisk file="$NtfsVhd"
detach vdisk
attach vdisk readonly
exit
"@ | Set-Content -Encoding ASCII (Join-Path $NtfsWork 'readonly.txt')
diskpart /s (Join-Path $NtfsWork 'readonly.txt')
if ($LASTEXITCODE -ne 0 -or -not (Test-Path $NtfsRoot)) { throw '测试卷只读重挂失败' }
# 只读重挂之后重新记录摘要，避免拿写入期间的 SVI/延迟分配变化作比较。
& .\target\release\examples\scan.exe $NtfsRoot --method mft --digest
$NtfsImage = Join-Path $NtfsWork 'partition.img'
$NtfsShare = [IO.FileShare]::ReadWrite -bor [IO.FileShare]::Delete
$NtfsInput = [IO.FileStream]::new("\\.\${NtfsLetter}:",[IO.FileMode]::Open,[IO.FileAccess]::Read,$NtfsShare,1048576)
$NtfsOutput = [IO.FileStream]::new($NtfsImage,[IO.FileMode]::CreateNew,[IO.FileAccess]::Write)
try { $NtfsInput.CopyTo($NtfsOutput,1048576) } finally { $NtfsInput.Dispose(); $NtfsOutput.Dispose() }
Get-FileHash -Algorithm SHA256 $NtfsImage
& .\target\release\examples\scan.exe $NtfsRoot --image $NtfsImage --digest
# 也可以复制原始分区镜像到 Linux，再使用 --image 扫描。
@"
select vdisk file="$NtfsVhd"
detach vdisk
exit
"@ | Set-Content -Encoding ASCII (Join-Path $NtfsWork 'detach.txt')
diskpart /s (Join-Path $NtfsWork 'detach.txt')
```

Windows 的可读卷长度可能比引导扇区声明的总扇区数短一个簇以内（首次报告短 7 个扇区）。
现在 `--image` 接受这类原样导出，不需要 `SetLength` 补零，也不要用伪造的末尾掩盖真正缺失的数据；
只有解析实际访问到镜像 EOF 之外才报错。比较摘要时必须给在线扫描与镜像相同的 `$NtfsRoot`。

验收后由用户检查 `$NtfsWork` 中确实只有本次测试材料，再决定是否删除；不执行对真实卷的清理命令。
若把实机生成的镜像加入仓库，必须先去除用户数据、压缩、记载生成命令与 SHA-256，所有镜像压缩后合计不超过 2 MiB。
