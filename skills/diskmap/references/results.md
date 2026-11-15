# DiskMap result reference

All sizes are bytes in `bytes` plus a human-readable `size` in binary units (1 KB = 1024 B).
Keys and enum values (`category`, `safety`, `type`, ...) never change and are meant for decisions.
Text values (labels, reasons, advice, notes, process descriptions, errors) follow DiskMap's
language setting, English or Chinese (`--lang en|zh`); translate them when they are not in the
user's language. Examples below show the English text.

## Item fields (lists, summaries, cleanup candidates)

| Field | Meaning |
|---|---|
| `path` | Absolute path. For `group` items it is the folder that holds the grouped files. |
| `type` | `dir`, `file`, or `group` (many small files counted as one item; see `count`, `note`) |
| `bytes`, `size` | Space used (on disk by default, file length when the scan used `apparent`) |
| `category` | `core`, `running`, `system`, `config`, `user`, `cache`, `other` |
| `category_label` | Display name of the category, e.g. `Cache / temp` |
| `reason` | Why it got this category, e.g. `System programs /usr` |
| `percent_of_parent` | Share of the parent folder, in percent |
| `files`, `dirs` | (folders) number of files and subfolders inside, recursively |
| `in_use_files` | (folders) how many files inside are used by running programs |
| `used_by` | (files) which programs use it, e.g. `chrome ×2, niri and others (7 programs)` |
| `static_category` | (in-use files) the category the file would have if nothing used it |
| `package` | Package or installed program that owns the file (pacman, dpkg, rpm, Homebrew, Windows uninstall registry) |
| `denied` | Folder could not be read; its size is missing from the totals |
| `mount_point` | Another filesystem is mounted here |
| `safety` | `danger`, `in_use`, `uninstall_only`, `caution`, `ask_user`, `safe_to_clean` |
| `advice` | One-sentence advice matching `safety` |

## `diskmap_status` / report `status`

```json
{
  "version": "1.3.0", "os": "linux", "admin": false, "ready": true,
  "window_connected": false,
  "processes": {"total": 320, "in_use_files": 1621, "visible_processes": 294},
  "scan": {
    "root": "/", "state": "done", "files": 812345, "dirs": 90123,
    "bytes": 98765432100, "size": "92.0 GB", "elapsed_seconds": 41.3,
    "finished_at": "2026-11-15 11:41:02", "current": null,
    "denied_dirs": 12, "denied_note": "...",
    "disk_used_bytes": 101234567890, "size_mode": "allocated",
    "includes_other_partitions": true
  },
  "user_view": {"view": "/home/me", "selected": "/home/me/.cache"}
}
```

- `scan` is `null` when nothing has been scanned yet.
- `state`: `scanning`, `done` or `cancelled`. While `scanning`, `current` is the folder being read
  and all numbers are partial.
- `admin: false` with `denied_dirs > 0` means a full picture needs administrator/root rights.
- `visible_processes` lower than `total` means some processes belong to other users and their
  open files are not visible.
- `disk_used_bytes` is how much of the whole filesystem is in use, for comparison with `bytes`.
- The MCP tool adds `connected_to`: `window` (the user's open DiskMap window) or `background`
  (the MCP server's own scan, no window involved).
- `user_view` appears only when the user has the window open.

## `diskmap_scan`

Returns `connected_to`, `reused_existing_scan`, `scan` (as in status) and, once finished,
`summary` (as below, 8 items per list). If the scan did not finish within `wait_seconds`, there is
a `note` instead of `summary`; partial results are still available from the other tools.

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

Item fields plus:
- folders: `largest_children` (8 items)
- files: `used_by` as a list of `{pid, comm, kind}` (kind: executing, loaded into memory, open,
  swap file) and `used_by_count`;
  `file_length_bytes` (file length, which can differ from space on disk for sparse or compressed
  files)
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
in use.

## `diskmap_user_view`

`view` (folder shown in the window), `selected` (selected item or null), `highlighted_category`,
`updated_seconds_ago`, plus `view_summary` (summary of the view, 5 items per list) and
`selected_detail` (explain result for the selection).

## Report mode

`analyze --json` prints `{"status": ..., "summary": ..., "cleanup": ...}` with the structures
above (summary and cleanup for the analyzed path, `--top` items per list). The Markdown report
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
