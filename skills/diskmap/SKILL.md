---
name: diskmap
description: Find out what is filling up a disk on Windows, macOS or Linux and give safe cleanup advice, using DiskMap, a free open-source analyzer that classifies every file and folder by importance (system core, in use by a running program, installed software, settings, personal data, cache). Use when the user asks why their disk is full, what is taking up space, whether something is safe to delete, or how to free up space.
license: MIT
metadata:
  version: "2.0.0"
  homepage: https://github.com/zi7tian/diskmap
---

# Disk space analysis with DiskMap

DiskMap scans a folder or a whole drive, measures files and folders, and classifies them by
importance with cleanup advice. Scan targets are read-only. This skill covers how to reach
DiskMap, run an analysis, read the results, and turn them into cleanup advice the user can trust.

Load the relevant reference when needed.

- [Setup](references/setup.md) covers downloads, program paths, and MCP configuration.
- [Results](references/results.md) covers result fields, the JSON report layout, and errors.
- [Cleanup guide](references/cleanup-guide.md) covers cleanup methods for each OS.

## 1. Choose how to reach DiskMap

Check these in order and use the first one that works:

1. **MCP tools are available** (tools named `diskmap_status`, `diskmap_scan`, ...). Use them; see
   section 2. This is the best option: if the user has the DiskMap window open, you see the same
   scan they see and can highlight items in their window.
2. **You can run shell commands.** Use report mode (section 3). Locate the program using the
   installation paths below or the setup reference.
   - macOS app programs are in `/Applications/DiskMap.app/Contents/MacOS/`.
   - Windows NSIS installs in `%LOCALAPPDATA%\Programs\DiskMap`, MSI in `C:\Program Files\DiskMap`,
     and the portable zip contains both programs in its extraction directory.
   - Linux deb installs `/usr/bin/DiskMap` and `/usr/bin/diskmap-cli`. The tar.gz contains both
     programs in its extraction directory. An executable AppImage also accepts subcommands.
   - Rust source builds produce `target/release/DiskMap` and `target/release/diskmap-cli`, with
     `.exe` on Windows.

   Every desktop package includes `diskmap-cli` beside `DiskMap`. Prefer `diskmap-cli.exe` for
   Windows shell scripts. Confirm the version with `<program> --version`. This skill describes
   DiskMap 2.0, including its scan methods and platform limits.
3. **DiskMap is not installed.** Tell the user what it is and where to get it
   using the 2.0 releases and the setup reference. The user can download and install it, or
   authorize you to do so. Until then you can still give general advice from
   the cleanup guide, and identify any estimates as unmeasured. [Releases](https://github.com/zi7tian/diskmap/releases)

Offer to set up the MCP connection when the user will analyze their disk more than once; the
commands are in `references/setup.md`. Adding an MCP server changes the user's AI client
configuration, so ask before running that command.

## 2. Workflow with the MCP tools

| Tool | Use it to | Main arguments |
|---|---|---|
| `diskmap_status` | See whether a scan exists, its root, progress, and whether the user's window is connected. Call first. | none |
| `diskmap_scan` | Scan a folder or drive. Reuses compatible results covering the path. | `path` (default system drive), `rescan`, `wait_seconds` (default 120, 1-600), `apparent`, `same_fs` |
| `diskmap_summary` | Overview of a folder: space by category, largest children, largest files, reclaimable space, big in-use files. | `path`, `top` (1-50) |
| `diskmap_list` | List a folder's direct children, largest first, to drill down level by level. | `path` (required), `limit` (1-500) |
| `diskmap_explain` | Everything about one file or folder: category and reason, owning package, processes using it, safety advice. | `path` (required) |
| `diskmap_cleanup` | Caches, temp files, logs, trash and re-downloadable packages, with total reclaimable space. | `path`, `limit`, `min_mb` |
| `diskmap_largest_files` | Largest files under a folder, optionally one category only. | `path`, `limit`, `category` |
| `diskmap_user_view` | What the user is looking at and has selected in the DiskMap window. Use when they say "this one" or "the selected folder". | none |
| `diskmap_show` | Jump to and highlight an item in the user's window, with a short note shown there. | `path` (required), `note` |

Typical session:

1. `diskmap_status`. If `scan` is null, or its `root` does not contain the path you need, call
   `diskmap_scan`. Use `/` on Linux and macOS and `C:\` on Windows for a whole-drive scan.
2. A whole drive takes from a few seconds to a few minutes. If `diskmap_scan` returns with
   `scan.state` still `scanning`, call `diskmap_status` again later. Generic scans provide partial
   results, and MFT publishes its tree at completion. Numbers are complete when `state` is `done`.
3. `diskmap_summary` for the overview, then `diskmap_list` on the biggest folders until the space
   is explained. Use `diskmap_explain` before giving advice about a specific item.
4. `diskmap_cleanup` to estimate what can be reclaimed safely.
5. When you talk about a specific item and the window is connected (`window_connected: true`), call
   `diskmap_show` so the user can see it on the map.

Scanning a new path while the user's window is connected replaces the scan shown in their window.
If they are in the middle of looking at something, ask before switching to a different drive.

## 3. Report mode (shell only)

```
<program> analyze <path> --json -q -o diskmap-report.json
```

- Without `--json` the report is Markdown. `--top N` sets list length (default 15).
- `--apparent` counts file length instead of space on disk; `--same-fs` stays on one partition.
- `--lang en` or `--lang zh` sets the report language. The default uses `DISKMAP_LANG`, then the
  saved window language, then the system language.
- Writing to a file with `-o` is the most reliable way to collect output, especially on Windows.
- Windows `DiskMap.exe` uses the GUI subsystem. cmd and PowerShell return the prompt before it
  finishes. PowerShell `>` and `$x = & ...` do not capture its output. Use `analyze -o` and
  `Start-Process -Wait` when calling this executable from PowerShell. MCP clients launch it with
  process pipes, so `DiskMap.exe mcp` works. `diskmap-cli.exe` is suited to scripts.
- Allow a generous timeout (10 minutes) for a whole drive. Progress goes to stderr unless `-q`.
- The JSON has three parts: `status`, `summary` and `cleanup`. They use the same fields as the
  MCP tools (see `references/results.md`).
- For anything deeper than the report, run `analyze` on a subfolder.
- AI analysis retains the 1.x presentation of the largest 150 files per folder plus files in use
  when the scan started. Other files may appear as a group. `diskmap_explain` and `diskmap_show`
  can return a path-not-found error for a grouped file, even though the complete scan counted it.

## 4. Reading the results

Items have `path`, `type` (`dir`, `file`, or `group` for grouped small files), `bytes`, `size`,
and `category`. Ordinary file/directory entries include `reason`, `safety`, and `advice`.
Groups provide `count` and `note`, and their `path` identifies the parent folder. Inspect that
folder before giving item-specific cleanup advice. Decide using the stable keys `category` and
`safety` when present. The descriptive text (`category_label`, `reason`,
`advice`, `note`, `used_by`) follows DiskMap's selected language, subject to command-line and
`DISKMAP_LANG` overrides. Add `--lang en` to the MCP server arguments or to `analyze` to force
English. Translate the text when needed.

| `category` | What it is | `safety` | What to tell the user |
|---|---|---|---|
| `core` | OS kernel, boot files, drivers, swap / pagefile / hibernation file | `danger` | Never delete. Some can be resized through OS settings (see cleanup guide). |
| `running` | Executable, loaded library or open file of a running program | `in_use` | Close the program first. `static_category` shows what it is otherwise. |
| `system` | OS files and installed software (package manager, Program Files, /Applications) | `uninstall_only` | Remove by uninstalling the software, never by deleting files. `package` names the owner. |
| `config` | Settings: /etc, ~/.config, dotfiles, ~/.ssh, registry hives | `caution` | Deleting loses settings or keys. Rarely large; usually leave alone. |
| `user` | Documents, downloads, photos, projects | `ask_user` | Only the user decides. Point out large or old items; suggest archiving or moving. |
| `cache` | Caches, temp files, logs, trash, re-downloadable packages and build outputs | `safe_to_clean` | Usually safe. Prefer the owning tool's clean command (cleanup guide). |
| `other` | Application data: databases, VMs, containers, mail, app state | `caution` | Belongs to an app. Manage it through that app. |

Also check the following fields and platform limits.

- `in_use_files` / `used_by`: something inside is in use right now.
- `denied: true` or `scan.denied_dirs > 0`: some folders could not be read, so totals are low.
  Administrator/root access can reveal more directories and processes. Protected processes,
  macOS privacy controls, and collection time limits can still restrict the results.
- `scan.size_mode`: `allocated` (space on disk, default) or `apparent` (file length).
- Windows automatically attempts MFT when an administrator scans a whole local NTFS volume.
  MFT counts hard links once, includes alternate data streams and NTFS metadata files, and
  publishes the tree after reading and building it. Subdirectories and unavailable MFT access
  use the generic scanner, which can count hard links repeatedly, including in `WinSxS`.
  Allocated mode uses system allocation information, and online-only cloud files count as 0.
- Windows in-use detection covers executables, DLLs, image mappings, and data mappings. Ordinary
  open handles without a file mapping remain a known gap, including with administrator rights.
- On macOS, APFS snapshots and "purgeable" space are outside the file totals. In-use detection
  collects executables and open regular files. The 2.0 macOS behavior awaits real-machine validation.
- On Linux, hard links count once and virtual filesystems and duplicate mounts are skipped.

## 5. Safety rules

- DiskMap only reports. Nothing gets deleted unless the user confirms the exact items.
- Never suggest deleting `danger` or `uninstall_only` items by hand. For `in_use`, close the
  program first. For `ask_user`, list the items and let the user choose.
- Prefer, in order: the owning program's own cleanup (package manager, `npm cache clean`,
  `docker system prune`, Windows Storage settings), then moving items to the Trash / Recycle Bin,
  and only last a permanent deletion.
- Before running any cleanup command, show the user the command, what it removes and roughly how
  much space it frees, and wait for a yes. Commands that need administrator rights should be run
  by the user.
- After cleanup, scan again (`diskmap_scan` with `rescan: true`, or a new `analyze`) and report
  how much space was actually freed.

## 6. Presenting the answer

1. One line with the total: how big the scanned area is, and how full the drive is if known
   (`scan.disk_used_bytes`).
2. The top 3-5 consumers with size, share and category, as a short table with full paths.
3. Reclaimable space in two groups: "safe to clean" (`safe_to_clean`) with the exact method for
   each, and "your decision" (`ask_user`, `caution`) with what each item is.
4. Caveats that affect the numbers: incomplete scan, permission-denied folders, platform quirks.
5. Next step offers: drill into a folder, show an item in the window, or run a cleanup command
   after confirmation.

Keep sizes as DiskMap reports them (binary units: 1 GB = 1024 MB). Do not invent numbers you did
not measure.
