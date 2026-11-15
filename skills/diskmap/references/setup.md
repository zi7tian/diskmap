# Installing DiskMap and connecting it to an AI client

## Download

Latest release: https://github.com/zi7tian/diskmap/releases/latest
Source code (MIT): https://github.com/zi7tian/diskmap

| System | File | Install |
|---|---|---|
| Windows 10/11 (x64) | `DiskMap-windows-x64.exe` | Single file, no installer. Double-click to run. |
| macOS 11+ (Apple silicon) | `DiskMap-macos-arm64.dmg` | Open, drag DiskMap into Applications. |
| macOS 11+ (Intel) | `DiskMap-macos-x64.dmg` | Same as above. |
| Linux (x64, glibc 2.35+) | `DiskMap-linux-x64.tar.gz` | Extract and run `./DiskMap`; `./install-desktop.sh` adds a menu entry. |

To tell which Mac file fits: Apple menu → About This Mac shows "Chip: Apple M…" (arm64) or an
Intel processor (x64).

Running from source works everywhere with Python 3.9+: clone the repository and run
`python3 diskmap.py`. On Windows and macOS, `pip install -r requirements.txt` adds `psutil` (needed
to detect files in use) and `pywebview` (native window). Release builds already include both.

## First run

- **Windows**: the program is not code-signed, so SmartScreen may show "Windows protected your
  PC". The user can choose "More info" → "Run anyway".
- **macOS**: the app is not notarized. The first launch is blocked once; the user allows it in
  System Settings → Privacy & Security → "Open Anyway". To scan Documents, Downloads and other
  protected folders without repeated prompts, add DiskMap under Privacy & Security → Full Disk
  Access.
- **Seeing everything**: as a normal user some system folders are unreadable and only the user's
  own processes are visible for "in use" detection. Running as administrator (Windows: right-click
  → Run as administrator) or as root on Linux gives the full picture. Leave that choice to the
  user.

## Paths to the program

| Install | Program path |
|---|---|
| macOS app | `/Applications/DiskMap.app/Contents/MacOS/DiskMap` |
| Windows | full path of the downloaded `DiskMap-windows-x64.exe` |
| Linux | full path of `DiskMap` in the extracted folder |
| Source | `python3` with `/path/to/diskmap.py` as the first argument |

The "AI 接入" (AI access) button in the top-right corner of the DiskMap window shows ready-made
commands with the correct path for that computer.

## MCP configuration

DiskMap is a local stdio MCP server started with the `mcp` argument. Replace `<program>` with the
program path from the table above. When running from source, use `python3` as the command and
`["/path/to/diskmap.py", "mcp"]` as the arguments.

**Claude Code**

```
claude mcp add --scope user diskmap -- <program> mcp
```

**Claude Desktop** (Settings → Developer → Edit Config), **Cursor** (`~/.cursor/mcp.json`) and most
other clients:

```json
{ "mcpServers": { "diskmap": { "command": "<program>", "args": ["mcp"] } } }
```

**VS Code** (`.vscode/mcp.json` in a workspace, or the user-level MCP configuration):

```json
{ "servers": { "diskmap": { "type": "stdio", "command": "<program>", "args": ["mcp"] } } }
```

**Codex CLI** (`~/.codex/config.toml`):

```toml
[mcp_servers.diskmap]
command = "<program>"
args = ["mcp"]
```

On Windows, write backslashes in JSON as `\\` (for example
`"C:\\Users\\me\\Downloads\\DiskMap-windows-x64.exe"`).

Restart the AI client after changing its configuration. The tools then appear as `diskmap_*`.

**Language.** Results come in DiskMap's interface language (English or Chinese): the language picked
with the button in the top-right corner of the DiskMap window, otherwise the system language. To fix
it for the AI, add it to the arguments, for example `"args": ["mcp", "--lang", "en"]` or
`claude mcp add --scope user diskmap -- <program> mcp --lang en`.

## How the MCP server finds the data

- If the DiskMap window is open, the MCP server connects to it. The AI then sees the window's scan
  and the user's current view and selection, scans started by the AI show up in the window, and
  `diskmap_show` can highlight items there. A green dot next to "AI 接入" shows recent AI access.
- If no window is open, the MCP server scans in the background on its own.
- The window registers its local address and a random access token in a file only the current
  user can read (`~/.cache/diskmap/instance.json` on Linux, `~/Library/Caches/DiskMap` on macOS,
  `%LOCALAPPDATA%\DiskMap` on Windows). The server listens on 127.0.0.1 only.
- All tools are read-only except that `diskmap_scan` starts a scan and `diskmap_show` moves the
  window's view. Nothing on disk is modified.

## Troubleshooting

- `diskmap_user_view` / `diskmap_show` fail with "用户没有打开 DiskMap 窗口" (window not open): ask
  the user to open DiskMap, or continue without the window.
- `analyze` prints nothing on Windows: use `-o report.json` and read the file.
- Linux build fails to start on an old distribution (glibc error): run from source instead.
- Results are in Chinese but the user writes in English: switch the DiskMap window to English
  (top-right button), or add `--lang en` to the MCP arguments or the `analyze` command.
- `--version` prints the installed version; the skill describes version 1.3.0.
