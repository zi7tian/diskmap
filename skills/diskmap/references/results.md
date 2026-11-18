# DiskMap result reference

All sizes are bytes in `bytes` plus a human-readable `size` in binary units (1 KB = 1024 B).
Keys and enum values (`category`, `safety`, `type`, ...) never change and are meant for decisions.
Text values (labels, reasons, advice, notes, process descriptions, errors) follow the selected
language. Selection uses command-line `--lang`, then `DISKMAP_LANG`, saved settings, and the system
language. Translate the text when needed. Examples below use English.

AI lists retain the 1.x projection of the largest 150 files per folder plus files in use at scan
start. Other files can be grouped. The complete 2.0 index counts them, while AI explain/show
queries for individual grouped files can return a path-not-found error.

## Item fields (lists, summaries, cleanup candidates)

| Field | Meaning |
|---|---|
| `path` | Absolute path. For `group` items it is the folder that holds the grouped files. |
| `type` | `dir`, `file`, or `group` (many small files counted as one item; see `count`, `note`) |
| `bytes`, `size` | Space used (on disk by default, file length when the scan used `apparent`) |
| `category` | `core`, `running`, `system`, `config`, `user`, `cache`, `other` |
| `category_label` | Display name of the category, e.g. `Cache / temp` |
| `reason` | (ordinary files/folders) why it got this category, e.g. `System programs /usr` |
| `percent_of_parent` | Share of the parent folder, in percent |
| `files`, `dirs` | (folders) number of files and subfolders inside, recursively |
| `in_use_files` | (folders) how many files inside are used by running programs |
| `used_by` | (files) which programs use it, e.g. `chrome ×2, niri and others (7 programs)` |
| `static_category` | (in-use files) the category the file would have if nothing used it |
| `package` | Package or installed program that owns the file (pacman, dpkg, rpm, Homebrew, Windows uninstall registry) |
| `denied` | Folder could not be read; its size is missing from the totals |
| `mount_point` | Another filesystem is mounted here |
| `safety` | (ordinary files/folders) `danger`, `in_use`, `uninstall_only`, `caution`, `ask_user`, `safe_to_clean` |
| `advice` | (ordinary files/folders) one-sentence advice matching `safety` |
| `count`, `note` | (groups) number of grouped files and a description of the group |

Group items provide category and size information. Their `path` is the parent directory, and
`reason`, `safety`, and `advice` can be absent.

## `diskmap_status` / report `status`

The following status comes from a Linux release build analyzing three temporary files totaling
28,672 bytes with `--apparent --same-fs --lang en`. Process counts and filesystem use describe
that test run.

```json
{
  "version": "2.0.0", "os": "linux", "admin": false, "ready": true,
  "window_connected": false,
  "processes": {"total": 4, "in_use_files": 33, "visible_processes": 4},
  "scan": {
    "root": "/tmp/diskmap-t11-5nw5zamz/data", "state": "done", "files": 3, "dirs": 1,
    "bytes": 28672, "size": "28.0 KB", "elapsed_seconds": 0.0,
    "finished_at": "2026-11-17 16:05:38", "current": null,
    "denied_dirs": 0,
    "disk_used_bytes": 76521472, "size_mode": "apparent",
    "includes_other_partitions": false
  }
}
```

- `scan` is `null` when nothing has been scanned yet.
- `state` can be `scanning`, `done`, `cancelled`, or `error`. During a generic scan, `current`
  identifies the directory being read and totals are partial. MFT publishes its tree at completion.
- `denied_dirs > 0` means unreadable directories were omitted. `denied_note` appears in that case.
  Elevated privileges can expand access, subject to OS privacy and protection rules.
- `visible_processes` counts processes whose relevant process information could be read. Permissions,
  protection rules, process exits, and collection limits can reduce it. On Windows it measures
  complete mapping-table traversal. Unmapped open handles remain a detection gap.
- `disk_used_bytes` is filesystem-level use, or `null` when unavailable. It can differ from the
  file totals because of metadata, snapshots, scan scope, and hard-link accounting.
- The MCP tool adds `connected_to`: `window` (the user's open DiskMap window) or `background`
  (the MCP server's own scan, no window involved).
- `user_view` contains `view` and `selected` when UI state has been reported. `window_connected`
  indicates recent window activity.

## `diskmap_scan`

Returns `connected_to`, `reused_existing_scan`, `scan` (as in status) and, once finished,
`summary` (as below, 8 items per main list). If the scan did not finish within `wait_seconds`,
there is a `note` instead of `summary`. Generic scans provide partial results through the other
tools, and MFT publishes its tree at completion.
Reuse requires a covering path and compatible size/partition options. `rescan: true` starts again.

## `diskmap_summary` / report `summary`

| Field | Meaning |
|---|---|
| `path`, `bytes`, `size`, `files`, `dirs`, `category`, `reason` | The folder itself |
| `by_category` | `[{category, category_label, bytes, size, percent}]`, largest first |
| `largest_children` | Direct children, largest first (item fields) |
| `largest_files` | Largest files anywhere below (item fields) |
| `cleanup` | `reclaimable_bytes`, `reclaimable_size`, `top_candidates` |
| `in_use` | `files`, `bytes`, `size`, `largest: [{path, size, used_by}]` |
| `note` | Present when the scan is still running or was stopped |

## `diskmap_list`

`{path, bytes, size, total_items, shown, items: [...]}`. `items` are direct children only, largest
first. Call it again on a child folder to go deeper.

## `diskmap_explain`

Explain results add the following fields when applicable.

- Folders include `largest_children` (up to 8 items).
- In-use files include `used_by`, a list of up to 10 `{pid, comm, kind}` objects, and
  `used_by_count` for the total. `kind` is translated text such as `executing`, `loaded into
  memory`, `open`, or `swap file`.
- Files include `file_length_bytes` for the current file length, which can differ from allocated
  space for sparse or compressed files.
- `modified` (local time), `mode`, `owner`, `group`, `mount` (`fstype`, `source`) when available
- `note`: the item was deleted or moved since the scan, or the folder could not be read (size not
  counted)

## `diskmap_cleanup` / report `cleanup`

`{path, reclaimable_bytes, reclaimable_size, candidates_shown, candidates: [...], note}`.
Candidates are the largest cache/temp subtrees, never counted twice: a cache folder is listed as
one item, not together with its subfolders. When `path` itself is a cache folder, its direct
children are listed instead. `reclaimable_bytes` counts all candidates, including those below
`min_mb` that are not listed. Candidates can still contain files in use (`in_use_files`).

## `diskmap_largest_files`

`{path, category, files: [...]}`. With `category: "running"` it lists the largest files currently
in use. Other category filters use the static category, and returned files can have a `running`
label with `static_category` identifying that underlying category.

## `diskmap_user_view`

`view` (folder shown in the window), `selected` (selected item or null), `highlighted_category`,
`updated_seconds_ago`, plus `view_summary` (summary of the view, 5 items per main list) and
`selected_detail` when the corresponding paths resolve in the scan.

## Report mode

`analyze --json` prints `{"status": ..., "summary": ..., "cleanup": ...}` with the structures
above (summary and cleanup for the analyzed path, `--top` controls the main lists). The Markdown report
has the same content under the headings "By importance", "Largest items", "Largest files" and
"Cleanup candidates" (in Chinese: 按重要程度, 最大的子项, 最大的文件, 可清理候选).

## Errors

| Message (English / Chinese) | What to do |
|---|---|
| `Nothing has been scanned yet` / `还没有扫描结果` | Call `diskmap_scan` |
| `This path is not in the scan results` / `扫描结果中没有这个路径` | Check the path, or scan a folder that contains it |
| `This is a file, not a folder` / `这是一个文件而不是文件夹` | Use `diskmap_explain` |
| `Unknown category` / `未知类别` | Use one of the seven category keys |
| `The DiskMap window is not open` / `用户没有打开 DiskMap 窗口` | Ask the user to open DiskMap, or tell them the path directly |
