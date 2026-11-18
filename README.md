# DiskMap 2.0 磁盘地图

用“方框套方框”的方式查看磁盘空间都去哪儿了。每个方框是一个文件或文件夹，面积等于它占用的空间，
文件夹里套着它的子文件夹和文件。颜色表示**重要程度**，详情和 AI 接口会给出分类原因及清理建议。

支持 **Windows 10/11（x64）、macOS 11+（Apple 芯片和 Intel）、Linux（x64）**，界面有**中文和英文**。
DiskMap 只读扫描目标，清理由用户自行操作。程序会保存自己的设置、运行实例登记，以及显式指定的报告。

本文对应 **Rust + Tauri 2 的 2.0 版**。Python 1.3 版保留在 `legacy/`。

*DiskMap visualizes disk usage as nested boxes colored by importance. It provides a Chinese/English
desktop interface, command-line reports, and an MCP server for AI assistants. Scanning is read-only.*

| 颜色 | 类别 | Windows | macOS | Linux |
|---|---|---|---|---|
| 紫 | 系统核心 | 驱动、注册表、启动文件、`pagefile.sys` / `hiberfil.sys`、MFT 扫描可见的 NTFS 元文件 | `/System`、虚拟内存 | `/boot`、内核模块、固件、交换文件 |
| 红 | 正在使用 | 运行中的可执行文件、DLL、映像和数据映射文件 | 可执行文件、打开的普通文件 | 可执行文件、加载和打开的文件 |
| 橙 | 系统文件 | `C:\Windows`、Program Files，显示注册表登记的程序名 | `/Applications`、`/Library`、Homebrew | pacman / dpkg / rpm 安装的文件，显示所属软件包 |
| 黄 | 配置文件 | `.ssh`、`NTUSER.DAT` | `~/Library/Preferences` | `/etc`、`~/.config`、点文件 |
| 蓝 | 用户数据 | 文档、下载、OneDrive | 文稿、iCloud 云盘 | 主目录中的个人文件 |
| 绿 | 缓存/临时 | Temp、回收站、更新缓存、`Windows.old` | `~/Library/Caches`、废纸篓、Xcode DerivedData | `~/.cache`、`/var/cache`、日志、回收站 |
| 灰 | 其他/应用数据 | AppData、ProgramData | `~/Library/Application Support` | `/var/lib`、`~/.local/state` |

## 下载安装

在仓库的 Releases 中下载 **2.0** 版本。开发构建可从 Actions 的 `ci` 工作流运行中下载
Artifacts。下面的 `<v>` 替换成不带 `v` 的版本号，例如 `2.0.0`。[Releases](https://github.com/zi7tian/diskmap/releases)

### 安装包

| 系统 | 文件 | 安装位置与用法 |
|---|---|---|
| Windows | `DiskMap-<v>-windows-x64-setup.exe` | NSIS，安装给当前用户，默认 `%LOCALAPPDATA%\Programs\DiskMap`，安装本身无需管理员权限 |
| Windows | `DiskMap-<v>-windows-x64.msi` | MSI，默认 `C:\Program Files\DiskMap`，安装需要管理员权限 |
| macOS（Apple 芯片） | `DiskMap-<v>-macos-arm64.dmg` | 打开后把 DiskMap 拖进“应用程序”，得到 `/Applications/DiskMap.app` |
| macOS（Intel） | `DiskMap-<v>-macos-x64.dmg` | 同上 |
| Linux | `DiskMap-<v>-linux-x64.deb` | `sudo apt install ./DiskMap-<v>-linux-x64.deb`，程序为 `/usr/bin/DiskMap` 和 `/usr/bin/diskmap-cli`；包名 `disk-map`，用 `sudo apt remove disk-map` 卸载 |

每个安装包都带 `DiskMap` 和 `diskmap-cli`。Windows 上分别为 `DiskMap.exe`、`diskmap-cli.exe`，位于同一目录。
macOS 上两者位于 `DiskMap.app/Contents/MacOS/`。MSI 安装数据库的数字版本省略预发布后缀，程序自身仍显示完整版本。

**Windows 首次运行**可能出现“Windows 已保护你的电脑”，可点“更多信息 → 仍要运行”。程序使用 WebView2 Runtime
显示窗口，免安装版的电脑也需要此运行时。

**macOS 首次打开**可能提示“无法验证开发者”。在“系统设置 → 隐私与安全性”中点“仍要打开”。程序未经 Apple 公证。
扫描“文稿”“下载”等受保护目录时按系统提示授权，需要更广的访问范围时可将 DiskMap 加入“完全磁盘访问权限”。

### 免安装版

| 系统 | 文件 | 使用方法 |
|---|---|---|
| Windows | `DiskMap-<v>-windows-x64-portable.zip` | 解压，在同一目录保留 `DiskMap.exe` 和 `diskmap-cli.exe`，双击 `DiskMap.exe` |
| Linux | `DiskMap-<v>-linux-x64.AppImage` | 需要 FUSE 2，在文件属性中允许作为程序执行后打开 |
| Linux | `DiskMap-<v>-linux-x64.tar.gz` | 解压，运行 `./DiskMap`，附带的 `./install-desktop.sh` 可加入当前用户的应用菜单 |

FUSE 2 的包名在 Debian 12、Ubuntu 22.04 上为 `libfuse2`，Debian 13 起和 Ubuntu 24.04 起为 `libfuse2t64`，
Fedora 为 `fuse` 和 `fuse-libs`，Arch Linux 为 `fuse2`。[AppImage FUSE 说明](https://github.com/AppImage/AppImageKit/wiki/FUSE)、
[Debian 13 包信息](https://packages.debian.org/trixie/libfuse2t64)、[Arch Linux 包信息](https://archlinux.org/packages/extra/x86_64/fuse2/)

没有 FUSE 时可运行 `./DiskMap-<v>-linux-x64.AppImage --appimage-extract-and-run`，此模式接入 MCP 时使用
`"args": ["--appimage-extract-and-run", "mcp"]`。[AppImage 解包运行说明](https://docs.appimage.org/user-guide/troubleshooting/fuse.html#extract-and-run-type-2-appimages)

Linux tar.gz 需要系统的 **WebKitGTK 4.1**。对应的运行库包名如下，编译桌面版时还需要开发包及编译工具。

| 发行版 | 运行库包名 | 开发包名 |
|---|---|---|
| Debian / Ubuntu | `libwebkit2gtk-4.1-0` | `libwebkit2gtk-4.1-dev` |
| Fedora | `webkit2gtk4.1` | `webkit2gtk4.1-devel` |
| Arch Linux | `webkit2gtk-4.1` | `webkit2gtk-4.1` |

运行库包信息见 [Debian](https://packages.debian.org/stable/libs/libwebkit2gtk-4.1-0)、
[Fedora](https://packages.fedoraproject.org/pkgs/webkitgtk/webkit2gtk4.1/)、
[Arch Linux](https://archlinux.org/packages/extra/x86_64/webkit2gtk-4.1/)。

Linux 发行包以 Ubuntu 22.04 构建，要求 glibc 2.35 或更新版本。构建依赖的完整清单见
[Tauri Prerequisites](https://v2.tauri.app/start/prerequisites/#linux)。

### 只要命令行

下载 `diskmap-cli-<v>-<平台>.zip`（Windows）或 `diskmap-cli-<v>-<平台>.tar.gz`（其他系统），解压后运行
`diskmap-cli`。平台名为 `windows-x64`、`macos-arm64`、`macos-x64`、`linux-x64`。CLI 可生成报告、启动浏览器界面、
提供 MCP 服务，运行时仅需命令行程序。发布附件还包含 `diskmap-skill.zip` 和 `SHA256SUMS.txt`。

### 权限

- **Windows** 右键“以管理员身份运行”后，扫描**整个本地 NTFS 卷**（例如 `C:\`）时会自动尝试 MFT 快速扫描。
  扫描子目录、网络路径、其他文件系统或 MFT 不可读时使用通用扫描。管理员权限还能读取更多进程的可执行文件、DLL
  和映射文件。当前检测范围见“各系统的统计差异”。
- **Linux** 普通用户可能遇到无权读取的目录。需要 root 权限时可在解压目录执行下列命令，再用普通用户的浏览器打开
  终端输出的完整链接。root 启动进入浏览器服务模式，终端服务持续运行到 Ctrl+C。

  ```bash
  sudo ./DiskMap --no-open /
  ```

- **macOS** 受系统隐私权限和 SIP 限制，管理员权限也可能无法读取某些项目。

## 界面窗口

默认使用 Tauri 2 独立窗口，Windows 使用 WebView2，macOS 使用 WKWebView，Linux 使用 WebKitGTK 4.1。
窗口初始化失败时会尝试打开默认浏览器，原来的扫描与链接继续可用。

- `--browser` 或 `--ui browser` 使用默认浏览器，关闭页面约 8 秒后服务退出。
- `--no-open` 输出带令牌的链接，持续运行到 Ctrl+C，适合无图形界面的机器。
- `--ui webview`、`--ui gtk`、`--ui chromium` 都选择同一个 Tauri 窗口。`DISKMAP_UI` 也可设置默认方式，
  命令行的 `--ui` 优先。
- `--verbose` 显示启动信息。Linux 缺省设置 `WEBKIT_DISABLE_DMABUF_RENDERER=1`，以兼容部分显卡，用户自设的值优先。

## 操作

- **悬停**查看大小、占比、类别、所属软件包、正在使用它的程序。
- **单击**在右侧显示详情，包括分类原因、权限、修改时间、进程列表和卸载方式。
- **双击**进入文件夹，**右键 / Backspace / Esc** 返回上一级。
- **图例**悬停时突出显示某一类，单击固定，每类显示在当前视图里的占用。
- 标题栏的**红点**表示文件夹里有正在使用的文件，斜线方框是合并显示的小项目。
- 单击**合并块**，右侧按大小列出其中的项目。点“显示更多”每次再看 100 项，点文件查看详情，点文件夹进入。
- 右侧的**当前视图中最大的文件**从完整索引取视图范围内的前 10 个文件，包括地图上未画出的文件。
  点文件查看详情，可进入它所在的文件夹。
- 在顶栏用**按名字搜索**查找整个扫描里的文件和文件夹。不区分大小写，按大小降序最多显示 200 项。
  点文件跳到所在文件夹并选中，点文件夹进入。
- 通用扫描会实时更新画面。MFT 扫描先显示进度，完成读取和建树后发布整棵目录树。

## 上次的扫描结果

从界面或 MCP 发起的扫描完成后会在后台自动保存。下次启动时，有历史结果的磁盘或主目录卡片会显示上次扫描时间。
点“打开上次结果”查看快照，点卡片主体开始新扫描。其他位置的结果列在磁盘卡片下方。
打开旧结果后，底部显示扫描时间和过时提示，点“重新扫描”可按当前设置更新同一位置。

快照保存在下表所列**缓存目录的 `saved/` 子目录**，同一位置保留最近一份，合计最多 5 份。
点结果旁的“删除”可清除该份缓存。缓存记录扫描到的**所有文件名**、类别和空间统计。

## 界面语言

- 支持 `zh`（中文）和 `en`（英文）。启动语言的优先级为**命令行 `--lang` > `DISKMAP_LANG` > 保存的设置 > 系统语言**。
  首次启动且未指定语言时，中文系统使用中文，其他系统使用英文。
- 点右上角的 **English / 中文** 按钮切换，选择保存到 `settings.json`，供以后启动和分析使用。
- MCP 和报告可用 `--lang en`、`--lang zh` 单独指定输出语言。

| 系统 | 设置目录（`settings.json`） | 缓存目录（`instance.json`） |
|---|---|---|
| Linux | `$XDG_CONFIG_HOME/diskmap`，默认 `~/.config/diskmap` | `$XDG_CACHE_HOME/diskmap`，默认 `~/.cache/diskmap` |
| macOS | `~/Library/Application Support/DiskMap` | `~/Library/Caches/DiskMap` |
| Windows | `%APPDATA%\DiskMap` | `%LOCALAPPDATA%\DiskMap` |

`DISKMAP_CONFIG_DIR`、`DISKMAP_CACHE_DIR` 可分别指定其他目录。测试或同时运行独立实例时可用临时目录隔离这两项。

## 命令行

以下示例假定程序目录已在 `PATH` 中。使用绝对路径也可以。Linux/macOS 从程序目录运行时加 `./`，
Windows PowerShell 从当前目录运行时加 `.\`，例如 `.\diskmap-cli.exe --help`。

```text
DiskMap [路径] [--port N] [--browser] [--no-open] [--apparent] [--same-fs]
        [--verbose] [--ui webview|gtk|chromium|browser] [--lang zh|en]
```

省略路径会显示启动页，指定路径会立即扫描。`--port N` 指定本地端口，默认自动选择，被占用时回退到随机端口。
`--apparent` 按文件长度统计，默认按实际占用统计。`--same-fs` 限制在同一文件系统。`--help` 查看帮助，
`--version` 查看版本。

`diskmap-cli` 提供下列子命令，`DiskMap` 也接受同样的子命令和选项。每个子命令均支持 `--help` 和 `--lang zh|en`。

| 子命令 | 选项和用途 |
|---|---|
| `analyze [路径]` | 扫描并输出 Markdown 报告，`--json` 输出 JSON，`--top N` 控制列表数量（默认 15），`-o/--output 文件` 写报告，`-q/--quiet` 关闭进度。支持 `--apparent`、`--same-fs` |
| `mcp` | 标准输入输出 MCP 服务，默认连接运行中的窗口，`--no-attach` 使用独立后台扫描 |
| `serve [路径]` | 本地 HTTP 服务，默认打开浏览器。支持 `--port N`、`--token TOKEN`、`--apparent`、`--same-fs`、`--no-open`、`--verbose` |
| `scan [路径]` | 输出文件数、目录数、字节数、用时、实际扫描方式、回退原因和峰值内存。支持 `--json`、`--method auto\|generic\|mft`、`--threads N`、`--apparent`、`--same-fs` |
| `selftest` | 在私有临时目录造数据，检查扫描、HTTP、MCP。`--report 文件` 保存 JSON 自检报告，`--limit 秒` 指定超时（默认 150 秒） |

`analyze` 和 `scan` 的路径默认系统盘，Linux/macOS 为 `/`，Windows 通常为 `C:\`。`serve` 省略路径时等待在界面选取。
`scan --threads 0` 自动选择线程数，线程数只影响通用扫描。`--method auto` 自动选择和回退，`generic` 强制通用扫描，
`mft` 强制 MFT 并在不可用时返回错误。`scan` 适合核对扫描统计，分类报告使用 `analyze`。

在要分析的文件夹中运行报告，输出文件写到当前目录，现有同名报告会被覆盖。

```bash
DiskMap analyze . --top 20 -o report.md
diskmap-cli analyze . --json --apparent --same-fs -q -o report.json
diskmap-cli scan . --json --method generic --threads 2 --apparent --same-fs
diskmap-cli serve . --no-open
diskmap-cli selftest --report selftest.json
```

`analyze` 的报告默认写标准输出，进度写标准错误。`DiskMap --selftest --report 文件` 兼容 1.x 自检入口。
命令成功返回 0，运行失败返回 1，参数错误返回 2，自检超时返回 3。

**Windows 的 `DiskMap.exe` 是窗口程序。** cmd、PowerShell 直接运行时会立即返回提示符，输出随后出现。
PowerShell 的 `>` 和 `$x = & …` 无法取得它的输出。报告请用 `analyze -o 文件`，脚本优先使用随包的
`diskmap-cli.exe`。需要等待窗口程序写完报告时可使用 PowerShell 的 `Start-Process -Wait`。

```powershell
Start-Process -FilePath .\DiskMap.exe -Wait -ArgumentList 'analyze C:\ --json -o report.json'
.\diskmap-cli.exe analyze C:\ --json -o report.json
```

## 让 AI 助手读取分析结果（MCP）

DiskMap 内置标准输入输出 MCP 服务。窗口右上角的 **“AI 接入”** 面板会显示当前程序的配置。
Claude Code 可用下列命令注册，将路径替换成实际的绝对路径。配置方法见
[Claude Code MCP 文档](https://code.claude.com/docs/en/mcp)。

```bash
claude mcp add --scope user diskmap -- /absolute/path/DiskMap mcp
```

Claude Desktop、Cursor 等使用 `mcpServers` 的客户端可填写以下配置。`command` 是程序的绝对路径，`args` 单独填写参数。
各客户端的配置位置见技能包的 [接入说明](skills/diskmap/references/setup.md)。

```json
{
  "mcpServers": {
    "diskmap": {
      "command": "/absolute/path/DiskMap",
      "args": ["mcp"]
    }
  }
}
```

| 安装方式 | MCP 的程序路径 |
|---|---|
| Windows NSIS | `%LOCALAPPDATA%\Programs\DiskMap\DiskMap.exe` |
| Windows MSI | `C:\Program Files\DiskMap\DiskMap.exe` |
| Windows 免安装版 | 解压目录内 `DiskMap.exe` 的绝对路径 |
| macOS | `/Applications/DiskMap.app/Contents/MacOS/DiskMap` |
| Linux deb | `/usr/bin/DiskMap` |
| Linux tar.gz | 解压目录内 `DiskMap` 的绝对路径 |
| Linux AppImage | 下载的 `.AppImage` 文件的绝对路径，需有执行权限 |
| 源码构建 | 仓库 `target/release/DiskMap` 的绝对路径，Windows 加 `.exe` |

AppImage 的“AI 接入”面板显示的就是 AppImage 文件的路径，移动或改名后要重新填写 MCP 配置中的路径。

表中的环境变量需展开成实际路径再填入 JSON，例如 Windows NSIS 安装的
`"command": "C:\\Users\\me\\AppData\\Local\\Programs\\DiskMap\\DiskMap.exe"`。
`diskmap-cli mcp` 提供相同接口，换用同目录的 `diskmap-cli` 即可，Windows 为 `diskmap-cli.exe`。
只下载 CLI 的用户填写该 CLI 的绝对路径。

MCP 客户端直接启动进程并读写管道，因此 Windows 的 `DiskMap.exe mcp` 可用。
添加 `"args": ["mcp", "--lang", "en"]` 可固定英文，`--no-attach` 或非空的 `DISKMAP_NO_ATTACH` 可使用独立后台模式。

| 工具 | 作用 |
|---|---|
| `diskmap_status` | 查看扫描范围、进度和窗口连接情况 |
| `diskmap_scan` | 扫描文件夹或整盘，已有范围及统计方式相容的结果时可复用 |
| `diskmap_summary` | 按重要程度的分布、最大的子项和文件、可清理空间、在用大文件 |
| `diskmap_list` | 列出直接子项，逐层深入 |
| `diskmap_explain` | 分类原因、软件包、进程和清理建议 |
| `diskmap_cleanup` | 缓存、临时、日志、回收站等候选及可回收总量 |
| `diskmap_largest_files` | 最大的文件，可按类别筛选 |
| `diskmap_user_view` | 用户在窗口里查看的目录和选中的项目 |
| `diskmap_show` | 让窗口跳转并高亮项目，可附一句说明 |

列表中的普通文件/目录项和 `diskmap_explain` 结果带 `safety` 和建议，等级包括 `danger`、`in_use`、`uninstall_only`、
`caution`、`ask_user`、`safe_to_clean`。合并的 `group` 项用类别、数量和备注描述。含义和字段见技能包的
[结果参考](skills/diskmap/references/results.md)。

- 已打开窗口且有扫描结果时，MCP 可读取同一份结果，AI 发起的扫描也会替换窗口里的扫描。
  “AI 接入”旁的绿点表示最近的 AI 访问。没有可用窗口时，MCP 在本进程按需扫描。
- `diskmap_user_view` 和 `diskmap_show` 需要连接窗口，其他分析工具也可在后台模式使用。
- HTTP 只监听 `127.0.0.1`，使用随机令牌和 Host 校验。地址、令牌等写入缓存目录的 `instance.json`，
  Unix 文件权限为 `0600`。程序正常退出时删除属于自己的登记。

**技能包（Agent Skill）**保留在 [skills/diskmap/SKILL.md](skills/diskmap/SKILL.md)，包含分析流程、安全等级、
接入方法和各系统的清理指南。Claude Code 用户可将整个 `skills/diskmap/` 复制到 `~/.claude/skills/diskmap/`。
需要上传 zip 的平台可下载发布附件，或在仓库根目录生成到临时目录。

```bash
skill_dir=$(mktemp -d)
python3 scripts/make_skill_zip.py -o "$skill_dir/diskmap-skill.zip"
```

## 从 1.x 升级

1. 关闭旧版窗口，安装 2.0 并启动。设置目录、缓存目录和 `settings.json` 格式沿用 1.x。
   `instance.json` 仍用于发现当前服务，关闭旧窗口后由新版重新登记。扫描结果目前保存在内存中，每次启动后重新扫描。
2. 更新 MCP 配置中的程序路径。Windows 的旧单文件 exe 改为安装目录或 portable.zip 解压目录中的 `DiskMap.exe`。
   macOS 放在“应用程序”时路径沿用，Linux tar.gz 用户指向新的解压目录。九个 MCP 工具的名称、参数、类别和安全等级沿用 1.x。
3. 用 2.0 的 `skills/diskmap/` 或 `diskmap-skill.zip` 替换旧技能包。
4. 关闭旧版进程后，可删除单独保留的旧单文件 exe、旧解压目录或旧 `DiskMap.app` 副本。macOS 同一路径的应用由新包覆盖。
   自行创建的应用菜单项和快捷方式也需更新。
   Python 源码用户可删除旧的独立源码副本及专用虚拟环境。设置和缓存继续供新版使用。

**与 1.x 不同的地方**

- Windows 新增 MFT 快速扫描，通用扫描、MFT 的大小口径和在用文件检测范围见下文。
- 2.0 提供独立 CLI，窗口统一使用 Tauri，`--ui gtk`、`--ui chromium` 都打开 Tauri 窗口。
- 同大小条目的顺序、硬链接计入的目录，以及少数 Unicode 名字在 Windows 上的匹配可能与 1.x 不同。
- JSON 对象的键序和排版可能变化。

**沿用 1.x 的展示方式**

- 界面和 AI 列表会合并小文件，AI 分析按每目录最大的 150 个文件加扫描开始时在用文件的规则展示。
- 部分合并小文件的 AI 详情查询仍可能返回“扫描结果中没有这个路径”。

## 从源码运行

需要 **Rust 1.90+** 和对应平台的编译工具链。编译桌面程序还需安装 Tauri 2 的系统开发依赖，Linux 的包名见前面的表，
完整环境配置见 [Tauri Prerequisites](https://v2.tauri.app/start/prerequisites/)。本项目的界面是静态 HTML/Canvas，
构建使用 Cargo。下列命令在仓库根目录运行，Linux/macOS 可直接复制。

```bash
cargo build --release -p diskmap-cli -p diskmap-desktop
./target/release/DiskMap
./target/release/DiskMap ./web --lang zh
./target/release/DiskMap ./web --browser
./target/release/diskmap-cli analyze ./web --json -o /tmp/diskmap-web-report.json
./target/release/diskmap-cli serve ./web --no-open
```

Windows 构建命令相同，程序位于 `target\release\DiskMap.exe` 和 `target\release\diskmap-cli.exe`，
报告路径改用自己的可写目录。单独使用 CLI 时可执行 `cargo build --release -p diskmap-cli`。

也可以通过 Cargo 启动，例如指定扫描仓库的 `web/`。

```bash
cargo run --release -p diskmap-desktop -- ./web --lang zh
cargo run --release -p diskmap-cli -- analyze ./web --json --lang en -q -o /tmp/diskmap-web-report.json
```

开发验证可用以下命令。测试和服务调试可先将 `DISKMAP_CACHE_DIR`、`DISKMAP_CONFIG_DIR` 指向临时目录。

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

旧版仍可在仓库根目录运行 `python3 legacy/diskmap.py`，需要 Python 3.9+，依赖见
[legacy/requirements.txt](legacy/requirements.txt)，其打包配置保留在 `legacy/packaging/`。

## 打包

2.0 使用 Tauri CLI，GitHub Actions 的 `ci` 工作流负责检查、契约测试、三个系统的安装包和 CLI。
推送 `main`、pull request 或手动触发会构建产物，`v2.*` 标签通过检查后发布到 Releases（标签必须是 `v` 加
workspace 版本号），带预发布后缀的版本标记为预发布。旧版的 **build** 工作流继续处理 `v1.*` 标签。
[ci 工作流](.github/workflows/ci.yml)

每个平台的安装包先在原生 runner 上安装、运行包内程序自检、再卸载，汇总任务核对 13 个发布文件并生成
`SHA256SUMS.txt`。Windows 版没有代码签名；macOS 版使用 ad-hoc 签名，未经 Apple 公证。

### 本地打包

需要 Python 3.11 以上和各平台的 Tauri 系统构建依赖。Tauri CLI 固定为 **2.12.5**，`package.py metadata`
会核对它与 `Cargo.lock` 中 `tauri` 的主、次版本一致。打包时另外合并 `crates/diskmap-desktop/packaging/tauri.bundle.json`
和 `package.py prepare` 生成的平台配置（附带的 `diskmap-cli`、Windows 安装器模板、MSI 数字版本），
平常的 Cargo 构建只用 `tauri.conf.json`。下面以 Linux 只生成 deb 为例，在仓库根目录运行，中间文件和产物都在 `target/`。

```bash
python3 -B crates/diskmap-desktop/packaging/package.py metadata
cargo install tauri-cli --version 2.12.5 --locked --root target/tools --target-dir target/tauri-cli-build
cargo build --release -p diskmap-cli --locked
python3 -B crates/diskmap-desktop/packaging/package.py prepare --target x86_64-unknown-linux-gnu
(
  cd crates/diskmap-desktop
  ../../target/tools/bin/cargo-tauri build --ci \
    --config packaging/tauri.bundle.json --config ../../target/packaging/tauri.platform.json \
    --bundles deb -- --locked
)
python3 -B crates/diskmap-desktop/packaging/package.py collect --platform linux-x64 --bundles deb
# 版本参数与 metadata 输出的版本一致；用 ar/tar 解包检查，不调用 sudo
bash crates/diskmap-desktop/packaging/smoke-linux.sh 2.0.0 --extract-deb --skip-appimage
python3 -B scripts/make_skill_zip.py -o target/packaging/out/diskmap-skill.zip
```

CI 的 Linux 构建改用 `--bundles deb,appimage`，并运行完整的 `smoke-linux.sh`。macOS 的 target 为
`aarch64-apple-darwin` 或 `x86_64-apple-darwin`：先执行 `codesign --force --sign - target/release/diskmap-cli`，
再 `prepare`、用 `--bundles dmg` 构建，`collect --platform macos-arm64|macos-x64 --bundles dmg` 后运行
`smoke-macos.sh <v> <平台>`。Windows 在 PowerShell 里用 target `x86_64-pc-windows-msvc` 和 `cargo-tauri.exe`，
构建参数为 `--bundles nsis,msi`，整理参数为 `collect --platform windows-x64 --bundles nsis,msi`，最后运行
`./crates/diskmap-desktop/packaging/smoke-windows.ps1 -Version <v>`，它会在默认位置安装再卸载，适合临时构建机器。
完整步骤见 `ci.yml`。

图标由 `scripts/make_icons.py` 生成，结果已保存在 `assets/icons/`。

## 各系统的统计差异

- **Windows 通用扫描**使用系统报告的分配大小，压缩文件在特定零分配结果下补查压缩大小。1.x 使用的簇大小估算已替换。
  该模式尚未对硬链接去重，`WinSxS` 等目录可能重复计入文件。OneDrive 等仅在线文件在实际占用模式计为 0，
  文件长度模式保留逻辑长度。目录联接跳过。
- **Windows MFT 扫描**需要管理员权限和整个本地 NTFS 卷，硬链接只计一次，ADS 合并到所属文件，
  NTFS 元文件也可见，并能读取通用扫描无权限进入的目录。驻留数据按 8 字节对齐计量，非驻留数据使用分配大小，
  稀疏和压缩流计实际分配。目录自身的索引块等元数据未计入，扫描总数与系统“已用空间”有口径差异。
  活动卷上的增删改也会影响一次扫描的结果。
- **Windows 在用检测**覆盖可执行文件、DLL、映像和普通数据映射。普通打开句柄中尚未映射的文件仍可能遗漏，
  管理员权限下也受进程保护和采集预算限制。1.x 用 psutil 查询的普通 `open_files` 尚未补齐。
- **macOS**使用 `st_blocks × 512` 统计实际占用，硬链接只计一次，跳过 `/System/Volumes` 和快照内容。
  APFS 快照、可清除空间等与系统存储页面的口径不同。在用检测采集可执行文件和打开的普通文件。
  2.0 的 macOS 版未在真机上验证，CLI 峰值内存显示为不可用。
- **Linux**使用 `st_blocks × 512`，硬链接只计一次，跳过 `/proc`、`/sys`、tmpfs 等非磁盘文件系统，
  同一文件系统的重复挂载只进入一次。默认可进入其他磁盘文件系统，`--same-fs` 限制在起点的文件系统。
  在用检测通过 `/proc` 读取可执行文件、内存映射和打开的文件，受其他用户进程访问权限限制。

无权读取的目录会记录在扫描状态和界面中，此时总量只包含成功读取的内容。符号链接保留链接自身的大小。
程序的扫描结果按文件累计，和文件系统级的剩余空间统计可能不同。

## 项目结构

```text
crates/
  diskmap-core/              扫描、完整索引、分类、软件包、进程检测、分析、本地 HTTP 接口
  diskmap-cli/               analyze / mcp / serve / scan / selftest，也是桌面程序调用的库
  diskmap-desktop/           Tauri 2 窗口和平台适配，可执行文件 DiskMap
web/                         中英界面，Canvas 方块图，新旧版共用
assets/icons/                图标
scripts/                     图标生成、技能包打包脚本
skills/diskmap/              SKILL.md 和接入、结果、清理参考
tests/contract/              新旧版 HTTP、MCP 黑盒契约测试
legacy/                      Python 1.3 版及其测试、PyInstaller 配置
.github/workflows/           2.0 CI 与保留的 1.x 构建工作流
```

## 许可证

[MIT](LICENSE)。可以自由使用、修改和分发，保留版权声明即可。
