# Installing DiskMap and connecting it to an AI client

## Download

Select a **2.0** release, including prereleases, on the releases page. Replace `<v>` below with
the version without a leading `v`.
[Releases](https://github.com/zi7tian/diskmap/releases), [source code](https://github.com/zi7tian/diskmap)

| System | File | Install |
|---|---|---|
| Windows 10/11 (x64) | `DiskMap-<v>-windows-x64-setup.exe` | NSIS installer for the current user, normally `%LOCALAPPDATA%\Programs\DiskMap` |
| Windows 10/11 (x64) | `DiskMap-<v>-windows-x64.msi` | MSI installer requiring administrator approval, normally `C:\Program Files\DiskMap` |
| Windows 10/11 (x64) | `DiskMap-<v>-windows-x64-portable.zip` | Extract `DiskMap.exe` and `diskmap-cli.exe` into the same directory |
| macOS 11+ (Apple silicon) | `DiskMap-<v>-macos-arm64.dmg` | Open and drag DiskMap into Applications |
| macOS 11+ (Intel) | `DiskMap-<v>-macos-x64.dmg` | Same as above |
| Linux (x64, glibc 2.35+) | `DiskMap-<v>-linux-x64.deb` | `sudo apt install ./DiskMap-<v>-linux-x64.deb`; the package is named `disk-map` (`sudo apt remove disk-map` uninstalls it) |
| Linux (x64, glibc 2.35+) | `DiskMap-<v>-linux-x64.AppImage` | Requires FUSE 2. Give it execute permission in the file manager, then open it |
| Linux (x64, glibc 2.35+) | `DiskMap-<v>-linux-x64.tar.gz` | Extract and run `./DiskMap`. `./install-desktop.sh` adds a user menu entry |

FUSE 2 packages are `libfuse2` on Debian 12 and Ubuntu 22.04, `libfuse2t64` starting with Debian 13
and Ubuntu 24.04, `fuse` and `fuse-libs` on Fedora, and `fuse2` on Arch Linux.
[AppImage FUSE guide](https://github.com/AppImage/AppImageKit/wiki/FUSE),
[Debian 13 package](https://packages.debian.org/trixie/libfuse2t64),
[Arch Linux package](https://archlinux.org/packages/extra/x86_64/fuse2/)

Without FUSE, run `./DiskMap-<v>-linux-x64.AppImage --appimage-extract-and-run`. For MCP in this
mode, use `"args": ["--appimage-extract-and-run", "mcp"]`.
[AppImage extract-and-run guide](https://docs.appimage.org/user-guide/troubleshooting/fuse.html#extract-and-run-type-2-appimages)

Each desktop package includes `diskmap-cli` beside `DiskMap`. A CLI-only download is also
available as `diskmap-cli-<v>-<platform>.zip` on Windows or `.tar.gz` elsewhere. Platform names
are `windows-x64`, `macos-arm64`, `macos-x64`, and `linux-x64`. It supports reports, MCP, and a
browser UI. Release attachments also include `diskmap-skill.zip` and `SHA256SUMS.txt`.

On a Mac, Apple menu → About This Mac identifies Apple silicon or Intel. The 2.0 macOS build
awaits real-machine validation.

Linux tar.gz requires WebKitGTK 4.1. Runtime packages are `libwebkit2gtk-4.1-0` on Debian/Ubuntu,
`webkit2gtk4.1` on Fedora, and `webkit2gtk-4.1` on Arch. Desktop source builds use the corresponding
`libwebkit2gtk-4.1-dev`, `webkit2gtk4.1-devel`, or `webkit2gtk-4.1` package plus build tools.
[Tauri prerequisites](https://v2.tauri.app/start/prerequisites/#linux)

Source builds require Rust 1.90+ and the platform compiler toolchain. Build the desktop program
with Tauri's system development dependencies installed. Run these commands at the repository root.

```bash
cargo build --release -p diskmap-cli -p diskmap-desktop
./target/release/DiskMap
./target/release/diskmap-cli analyze ./web --json --lang en -q -o /tmp/diskmap-web-report.json
```

Windows produces `target\release\DiskMap.exe` and `target\release\diskmap-cli.exe`. Choose a
writable report path there. CLI-only source builds use `cargo build --release -p diskmap-cli`.
The Python 1.3 implementation remains available as `python3 legacy/diskmap.py` with Python 3.9+
and the dependencies in `legacy/requirements.txt`.

## First run

- **Windows** uses WebView2 Runtime for its window. The unsigned program may trigger SmartScreen's
  “Windows protected your PC” message. The user can choose “More info” → “Run anyway”.
- **macOS** is not notarized. Allow the app in System Settings → Privacy & Security → “Open Anyway”.
  Protected directories such as Documents and Downloads require privacy approval. Full Disk
  Access can expand access, subject to system restrictions.
- **Windows permissions** enable automatic MFT scanning when an administrator scans an entire
  local NTFS volume such as `C:\`. Subfolders, network paths, other filesystems, or unavailable
  MFT access use the generic scanner. Elevated access can also reveal more process mappings.
  Ordinary unmapped open handles remain outside current Windows in-use detection.
- **Linux permissions** can leave some directories unreadable. Root starts a browser service
  and prints a link for the user's normal browser. In an extracted tar.gz directory, the user
  can launch `./DiskMap --no-open /` with root privileges and stop the service with Ctrl+C.
- **Other access limits** include protected processes, macOS SIP/privacy controls, and the process
  collection time budget. An unmarked file can still be in use.

The default window uses Tauri 2. `--browser` or `--ui browser` opens the default browser.
`--no-open` prints a link and serves until Ctrl+C. `--ui webview`, `--ui gtk`, and `--ui chromium`
all select the Tauri window. Window initialization failure falls back to the browser with the
same scan. `DISKMAP_UI` sets the default, and command-line `--ui` takes precedence.

## Paths to the program

Use an absolute path in the MCP configuration. Expand environment variables in the table to
actual directory names before writing JSON.

| Install | Program path |
|---|---|
| Windows NSIS | `%LOCALAPPDATA%\Programs\DiskMap\DiskMap.exe` |
| Windows MSI | `C:\Program Files\DiskMap\DiskMap.exe` |
| Windows portable | `DiskMap.exe` in the extraction directory |
| macOS app | `/Applications/DiskMap.app/Contents/MacOS/DiskMap` |
| Linux deb | `/usr/bin/DiskMap` |
| Linux tar.gz | `DiskMap` in the extraction directory |
| Linux AppImage | Absolute path to the executable `.AppImage` file |
| Rust source build | `target/release/DiskMap` in the repository, with `.exe` on Windows |
| CLI-only download | Absolute path to the extracted `diskmap-cli`, with `.exe` on Windows |

`diskmap-cli mcp` provides the same MCP interface. For desktop packages, replace the final
`DiskMap` component with `diskmap-cli`, or `DiskMap.exe` with `diskmap-cli.exe`. In the macOS app,
both are inside `Contents/MacOS/`. The “AI 接入” (AI access) button in the DiskMap window shows
configuration for its current executable.

For AppImage, the panel shows the AppImage file's path. Update the MCP configuration path after
moving or renaming the file.

## MCP configuration

DiskMap is a local stdio MCP server started with the `mcp` argument. Replace `<program>` below
with the full program path. Rust source users can point directly to the release executable.

### Claude Code

```text
claude mcp add --scope user diskmap -- <program> mcp
```

Quote a program path containing spaces when entering a shell command. The configuration uses
user scope. [Claude Code MCP documentation](https://code.claude.com/docs/en/mcp)

### Claude Desktop and Cursor

Claude Desktop uses Settings → Developer → Edit Config. Cursor uses `~/.cursor/mcp.json` for
user-level configuration. Both accept a `mcpServers` object.
[Claude Desktop setup](https://modelcontextprotocol.io/docs/develop/connect-local-servers),
[Cursor MCP documentation](https://cursor.com/docs/mcp)

```json
{
  "mcpServers": {
    "diskmap": {
      "command": "<program>",
      "args": ["mcp"]
    }
  }
}
```

### VS Code

With Copilot selected, open `Chat: Open Customizations` → `MCP Servers` → `Add Server` and
choose `Command (stdio)`. Enter the program path with its `mcp` argument. For personal use,
choose `Global` → `Copilot Global`.
[VS Code MCP setup](https://code.visualstudio.com/docs/agent-customization/mcp-servers)

The native user configuration is `$COPILOT_HOME/mcp-config.json`, defaulting to
`~/.copilot/mcp-config.json`. It uses the following format.
[VS Code MCP configuration reference](https://code.visualstudio.com/docs/agents/reference/mcp-configuration)

```json
{
  "mcpServers": {
    "diskmap": {
      "type": "stdio",
      "command": "<program>",
      "args": ["mcp"]
    }
  }
}
```

Existing editor-managed `.vscode/mcp.json` or profile `mcp.json` configurations use a `servers`
object and can be forwarded to supported sessions. Their JSON shape is shown below.
[VS Code MCP configuration reference](https://code.visualstudio.com/docs/agents/reference/mcp-configuration)

```json
{
  "servers": {
    "diskmap": {
      "type": "stdio",
      "command": "<program>",
      "args": ["mcp"]
    }
  }
}
```

### Codex CLI

Add the following to `~/.codex/config.toml`.

```toml
[mcp_servers.diskmap]
command = "<program>"
args = ["mcp"]
```

On Windows, escape JSON backslashes. An NSIS example is
`"command": "C:\\Users\\me\\AppData\\Local\\Programs\\DiskMap\\DiskMap.exe"`.
In TOML, either escape backslashes in double-quoted strings or use a single-quoted literal string.

Restart the AI client after editing its configuration. The tools appear as `diskmap_*`.

Language selection follows command-line `--lang`, then `DISKMAP_LANG`, saved settings, and the
system language. To fix English for AI requests, use `"args": ["mcp", "--lang", "en"]`, or
append `--lang en` to the Claude Code registration command after `mcp`.

## How the MCP server finds the data

- With an available DiskMap window and scan, MCP uses the window's scan and selection. Scans
  started by AI replace that scan. `diskmap_show` highlights an item there, and a green dot next
  to “AI 接入” shows recent AI access. Browser windows work with the same interface.
- Without an available window, MCP scans on demand in its own process. `--no-attach` or a nonempty
  `DISKMAP_NO_ATTACH` selects independent background operation. The background MCP process keeps
  its scan in memory.
- The HTTP service listens on `127.0.0.1`, checks Host and access tokens, and records its address
  in `instance.json`. Normal exit removes its own registration. Unix registration permissions
  are `0600`, and Windows uses inherited permissions in the user cache directory.
- Scan targets are read-only. DiskMap writes its own settings, instance registration, and reports
  explicitly requested with `-o` or `--report`.

| System | Cache directory (`instance.json`) | Settings directory (`settings.json`) |
|---|---|---|
| Linux | `$XDG_CACHE_HOME/diskmap`, default `~/.cache/diskmap` | `$XDG_CONFIG_HOME/diskmap`, default `~/.config/diskmap` |
| macOS | `~/Library/Caches/DiskMap` | `~/Library/Application Support/DiskMap` |
| Windows | `%LOCALAPPDATA%\DiskMap` | `%APPDATA%\DiskMap` |

`DISKMAP_CACHE_DIR` and `DISKMAP_CONFIG_DIR` override those directories, including for tests.
Settings, instance format, and the nine MCP tool names and arguments are shared with 1.x.
For an upgrade, close the old window, install 2.0, update the MCP executable path, and replace
the skill folder or uploaded zip. Old downloaded executables and extracted directories can be
removed once their processes have exited. Existing settings carry over.

## Troubleshooting

- **Windows reports** should use `analyze -o report.json`. `DiskMap.exe` is a GUI program, so cmd
  and PowerShell return the prompt immediately, and PowerShell `>` or `$x = & ...` do not capture
  its output. Scripts should use `diskmap-cli.exe`. `Start-Process -Wait` can wait for the GUI
  executable to finish writing a report.

  ```powershell
  Start-Process -FilePath .\DiskMap.exe -Wait -ArgumentList 'analyze C:\ --json -o report.json'
  .\diskmap-cli.exe analyze C:\ --json -o report.json
  ```

- **Windows MCP** works with `DiskMap.exe mcp` because clients start a process with input/output
  pipes. Use the full installed executable path and keep `mcp` in `args`.
- **Window tools** require a connected window. For a `diskmap_user_view` / `diskmap_show` error,
  open DiskMap and scan a directory, or continue with background analysis tools.
- **Grouped files** can produce a path-not-found error in AI analysis. The 1.x projection retains
  the largest 150 files per folder and those in use at scan start. Analyze a narrower directory
  or inspect the group and its parent.
- **Linux startup** may require WebKitGTK 4.1 for the tar.gz. A glibc version error requires a
  compatible distribution or a source build on that system.
- **Language** can be fixed with `--lang en` or `--lang zh` in the MCP arguments or report command.
- **Version** is printed by `<program> --version`.
