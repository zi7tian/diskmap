---
name: diskmap
description: Find out what is filling up a disk on Windows, macOS or Linux and give safe cleanup advice, using DiskMap, a free open-source analyzer that classifies every file and folder by importance (system core, in use by a running program, installed software, settings, personal data, cache). Use when the user asks why their disk is full, what is taking up space, whether something is safe to delete, or how to free up space.
license: MIT
metadata:
  version: "1.2.0"
  homepage: https://github.com/zi7tian/diskmap
---

# Disk space analysis with DiskMap

DiskMap scans a folder or a whole drive, measures every file and folder, and labels each one with
an importance category and a safety level ("can this be deleted?"). It is read-only: it never
deletes or changes anything. This skill covers how to reach DiskMap, how to run an analysis, how to
read the results, and how to turn them into cleanup advice the user can trust.

Reference files (load when needed):
- `references/setup.md`: download links, first-run notes, MCP configuration for each AI client
- `references/results.md`: every result field, the JSON report layout, error messages
- `references/cleanup-guide.md`: the right cleanup method for common space hogs on each OS

## 1. Choose how to reach DiskMap

Check these in order and use the first one that works:

1. **MCP tools are available** (tools named `diskmap_status`, `diskmap_scan`, ...). Use them; see
   section 2. This is the best option: if the user has the DiskMap window open, you see the same
   scan they see and can highlight items in their window.
2. **You can run shell commands.** Use report mode (section 3). Find the program first:
   - macOS: `/Applications/DiskMap.app/Contents/MacOS/DiskMap`
   - Windows: wherever the user saved `DiskMap-windows-x64.exe` (ask if unsure; often `Downloads`)
   - Linux: the `DiskMap` file inside the extracted `DiskMap` folder
   - From source: `python3 diskmap.py` in a clone of the repository (Python 3.9+)
   Confirm it runs with `<program> --version`. Report mode and the MCP server need version 1.2.0
   or newer; with an older version, ask the user to update from the releases page.
3. **DiskMap is not installed.** Tell the user what it is and where to get it
   (https://github.com/zi7tian/diskmap/releases/latest; `references/setup.md` says which file
   fits their system). Let the user download and install it themselves, or do it only after
   they explicitly agree. Until then you can still give general advice from
   `references/cleanup-guide.md`, but say clearly that you have not measured anything.

Offer to set up the MCP connection when the user will analyze their disk more than once; the
commands are in `references/setup.md`. Adding an MCP server changes the user's AI client
configuration, so ask before running that command.

## 2. Workflow with the MCP tools

| Tool | Use it to | Main arguments |
|---|---|---|
| `diskmap_status` | See whether a scan exists, its root, progress, and whether the user's window is connected. Call first. | none |
| `diskmap_scan` | Scan a folder or drive. Reuses an existing scan that already covers the path. | `path` (default: system drive), `rescan`, `wait_seconds` (default 120, max 600), `apparent`, `same_fs` |
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
2. A whole drive takes from a few seconds to a few minutes. If `diskmap_scan` comes back with
   `scan.state` still `scanning`, partial results are already usable; call `diskmap_status`
   again later. Mention that numbers are incomplete until `state` is `done`.
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
- Writing to a file with `-o` is the most reliable way to collect output, especially on Windows.
- Allow a generous timeout (10 minutes) for a whole drive. Progress goes to stderr unless `-q`.
- The JSON has three parts: `status`, `summary` and `cleanup`. They use the same fields as the
  MCP tools (see `references/results.md`).
- For anything deeper than the report, run `analyze` on a subfolder.

## 4. Reading the results

Each item has `path`, `type` (`dir`, `file`, or `group` = many small files counted together),
`bytes` and `size`, `category`, `reason`, `safety` and `advice`. Decide using the English keys
(`category`, `safety`). The descriptive text (`category_label`, `reason`, `advice`, `note`,
`used_by`) is written in Chinese; translate it into the user's language when you quote it.

| `category` | What it is | `safety` | What to tell the user |
|---|---|---|---|
| `core` | OS kernel, boot files, drivers, swap / pagefile / hibernation file | `danger` | Never delete. Some can be resized through OS settings (see cleanup guide). |
| `running` | Executable, loaded library or open file of a running program | `in_use` | Close the program first. `static_category` shows what it is otherwise. |
| `system` | OS files and installed software (package manager, Program Files, /Applications) | `uninstall_only` | Remove by uninstalling the software, never by deleting files. `package` names the owner. |
| `config` | Settings: /etc, ~/.config, dotfiles, ~/.ssh, registry hives | `caution` | Deleting loses settings or keys. Rarely large; usually leave alone. |
| `user` | Documents, downloads, photos, projects | `ask_user` | Only the user decides. Point out large or old items; suggest archiving or moving. |
| `cache` | Caches, temp files, logs, trash, re-downloadable packages and build outputs | `safe_to_clean` | Usually safe. Prefer the owning tool's clean command (cleanup guide). |
| `other` | Application data: databases, VMs, containers, mail, app state | `caution` | Belongs to an app. Manage it through that app. |

Also check:
- `in_use_files` / `used_by`: something inside is in use right now.
- `denied: true` or `scan.denied_dirs > 0`: some folders could not be read, so totals are low.
  Running DiskMap as administrator (Windows) or root (Linux/macOS) shows everything.
- `scan.size_mode`: `allocated` (space on disk, default) or `apparent` (file length).
- Platform quirks: on Windows `C:\Windows\WinSxS` looks bigger than it is (hard links are not
  detected) and OneDrive online-only files count as 0. On macOS, APFS snapshots and "purgeable"
  space are not counted. On Linux, hard links count once and virtual filesystems are skipped.

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
