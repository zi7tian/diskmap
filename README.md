# DiskMap 磁盘地图

用"方框套方框"的方式查看磁盘空间都去哪儿了：每个方框是一个文件或文件夹，面积等于它占用的空间，
文件夹里套着它的子文件夹和文件。方框的颜色表示**重要程度**，一眼就能看出哪些能删、哪些不能碰。

支持 **Windows 10/11、macOS 11+、Linux**。DiskMap 只读取磁盘，不会删除或修改任何文件。

| 颜色 | 类别 | Windows | macOS | Linux |
|---|---|---|---|---|
| 紫 | 系统核心 | 驱动、注册表、启动文件、`pagefile.sys`/`hiberfil.sys` | `/System`、虚拟内存 | `/boot`、内核模块、固件、交换文件 |
| 红 | 正在使用 | 正被运行中的程序执行、加载（`.dll`/`.so`）或打开的文件 | 同左 | 同左 |
| 橙 | 系统文件 | `C:\Windows`、Program Files，并显示注册表里登记的程序名 | `/Applications`、`/Library`、Homebrew | 由 pacman/dpkg/rpm 安装，显示所属软件包 |
| 黄 | 配置文件 | `.ssh`、`NTUSER.DAT` | `~/Library/Preferences` | `/etc`、`~/.config`、点文件 |
| 蓝 | 用户数据 | 文档、下载、OneDrive | 文稿、iCloud 云盘 | 主目录中的个人文件 |
| 绿 | 缓存/临时 | Temp、回收站、更新缓存、`Windows.old` | `~/Library/Caches`、废纸篓、Xcode DerivedData | `~/.cache`、`/var/cache`、日志、回收站 |
| 灰 | 其他/应用数据 | AppData、ProgramData | `~/Library/Application Support` | `/var/lib`、`~/.local/state` |

## 下载安装

在 GitHub 仓库的 **Releases**（或 Actions 某次运行的 **Artifacts**）中下载对应文件：

| 系统 | 文件 | 使用方法 |
|---|---|---|
| Windows | `DiskMap-windows-x64.exe` | 双击运行。首次运行若出现「Windows 已保护你的电脑」，点「更多信息 → 仍要运行」 |
| macOS（Apple 芯片） | `DiskMap-macos-arm64.dmg` | 打开后把 DiskMap 拖进「应用程序」 |
| macOS（Intel） | `DiskMap-macos-x64.dmg` | 同上 |
| Linux | `DiskMap-linux-x64.tar.gz` | 解压后运行 `./DiskMap`；`./install-desktop.sh` 可加入应用菜单 |

**macOS 首次打开**：程序没有经过 Apple 公证，第一次会提示"无法验证开发者"。
打开「系统设置 → 隐私与安全性」，在页面下方点「仍要打开」即可（只需一次）。
扫描「文稿」「下载」等文件夹时系统会询问是否允许访问；想一次扫全，可以在
「隐私与安全性 → 完全磁盘访问权限」中添加 DiskMap。

**看到更多内容**：普通权限下部分系统文件夹读不到，也只能看到自己进程打开的文件。
Windows 右键「以管理员身份运行」；Linux 用 `sudo ./DiskMap --no-open /`，
然后在自己的浏览器里打开终端输出的链接（以 root 运行时不会自动弹出窗口）。

## 界面窗口

DiskMap 的界面是一个本地网页，按以下顺序选择显示方式：

1. Windows：Edge WebView2（Windows 10/11 自带）；macOS：系统 WebKit —— 都是独立窗口
2. Linux：GTK + WebKit2GTK 窗口（借用系统自带的 Python，大多数桌面发行版都有）
3. 上面都不可用时：Chrome / Edge / Chromium 的"应用模式"窗口
4. 最后：默认浏览器。关闭标签页约 10 秒后程序自动退出

可以用 `--ui webview|gtk|chromium|browser` 强制指定，`--verbose` 查看选择过程。

## 操作

- **悬停**：查看大小、占比、类别、所属软件包、正在使用它的程序
- **单击**：右侧显示详情（分类原因、权限、修改时间、进程列表、卸载方式）
- **双击**：进入文件夹；**右键 / Backspace / Esc**：返回上一级
- **图例**：悬停只突出显示某一类，单击固定；每类都显示在当前视图里占了多少空间
- 标题栏上的 **红点** 表示文件夹里有正在使用的文件；斜线方框是合并显示的小项目
- 扫描过程中画面实时更新，不用等扫描结束

## 从源码运行

需要 Python 3.9+。Linux 上不需要任何第三方包；Windows / macOS 建议装上 `psutil`（识别"正在使用"）和
`pywebview`（独立窗口）：

```bash
python -m pip install -r requirements.txt   # Windows / macOS
python diskmap.py            # 打开窗口，在启动页选择要扫描的磁盘
python diskmap.py D:\        # 立即扫描指定位置
python diskmap.py --browser  # 用默认浏览器
```

其他参数：`--apparent` 按文件长度而不是实际占用统计；`--same-fs` 不进入其他分区；`--port` 指定端口。

## 打包

打包工具 PyInstaller **只能打包当前所在系统的版本**，所以三个系统的安装包由 GitHub Actions
分别在 Windows、macOS（Apple 芯片和 Intel）、Linux 虚拟机上构建，见 `.github/workflows/build.yml`：

1. 把本文件夹作为一个 GitHub 仓库推送上去
2. 推送到 `main` 或在 Actions 页面手动运行 *build*：每个系统都会跑单元测试、打包、
   再用 `--selftest` 启动打包好的程序走一遍扫描流程，产物在该次运行的 Artifacts 里
3. 推送 `v` 开头的标签（`git tag v1.1.0 && git push --tags`）：额外自动发布到 Releases

在本机打包当前系统的版本：

```bash
python -m pip install -r requirements-build.txt
python -m PyInstaller --noconfirm --clean packaging/diskmap.spec   # 产物在 dist/
dist/DiskMap --selftest                                             # 自检（macOS 为 dist/DiskMap.app/Contents/MacOS/DiskMap）
```

Linux 版要分发给别人时，请用 CI 在 Ubuntu 22.04 上构建的版本：在新系统上打包的程序依赖较新的
glibc，在老发行版上可能无法运行。图标由 `packaging/make_icons.py` 生成（需要 Pillow，结果已提交）。

## 各系统的统计差异

- **Windows**：没有 `st_blocks`，"实际占用"按磁盘簇大小向上取整估算；OneDrive"仅在线"文件计为 0；
  目录联接（如 `Application Data`）不会跟进；硬链接无法识别，`C:\Windows\WinSxS` 会显示得比实际偏大。
- **macOS**：跳过 `/System/Volumes`（数据卷的内容已经通过固定链接出现在 `/` 下，避免重复计算），
  也跳过 Time Machine 本地快照。
- **Linux**：硬链接只计一次；跳过 `/proc`、`/sys`、tmpfs 等虚拟文件系统和重复的 bind mount。

## 项目结构

```
diskmap.py            启动器：本地服务 + 窗口（pywebview / GTK / Chrome 应用模式 / 浏览器）+ 自检
backend/osinfo.py     平台差异（路径、管理员判断、交换文件、打包环境）
backend/scanner.py    扫描：os.scandir 遍历，实时向上累加大小
backend/classify.py   三套重要程度规则（路径规则 + 软件包归属 + 文件名/扩展名）
backend/pkgdb.py      pacman / dpkg / rpm 数据库、Windows 注册表、Homebrew
backend/procinfo.py   正在使用的文件：Linux 读 /proc，Windows / macOS 用 psutil
backend/mounts.py     挂载点与磁盘列表
backend/server.py     只监听 127.0.0.1 的 HTTP 接口；随机令牌 + Host 校验
web/                  界面：Canvas 绘制的 squarified treemap
packaging/            PyInstaller 配置与图标
tests/                python -m unittest discover -s tests（三个系统通用）
```
