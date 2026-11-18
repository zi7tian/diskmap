#!/usr/bin/env python3
"""用旧版实现生成分类对照数据；运行测试时只读取已提交的 JSON。

仓库根目录运行：
    python3 crates/diskmap-core/tests/fixtures/classify/generate.py
    python3 crates/diskmap-core/tests/fixtures/classify/generate.py --check

只导入旧版的分类与语言模块，不启动服务、不读取本机软件包数据库。
缓存、设置均隔离到临时目录；禁止生成 legacy 目录内的字节码文件。
"""

import argparse
import json
import os
from pathlib import Path
import sys
import tempfile


class FakePkg:
    """三个系统共用的、完全由样例指定的软件包数据库。"""

    def __init__(self, files, backup, app_dirs):
        self.files = files
        self.backup = set(backup)
        self.app_dirs = app_dirs


def result(value):
    """直接读取两种原文，不依赖生成脚本进程的当前语言。"""
    category, reason = value
    return {"category": category, "zh": reason.text("zh"), "en": reason.text("en")}


def scenarios():
    """固定输入；没有任何本机主目录、包数据库或环境变量参与期望值。"""
    return [
        {
            "name": "linux",
            "os": "linux",
            "env": {},
            "homes": ["/home/alice"],
            "files": {
                "/usr/bin/ls": "coreutils",
                "/etc/pacman.conf": "pacman",
                "/etc/implicit.conf": "implicit-config",
                "/usr/lib/modules/x/a.ko": "linux",
                "/boot/vmlinuz-linux": "linux",
                "/boot/override.conf": "boot-config",
                "/home/alice/projects/owned.data": "demo",
                "/home/alice/projects/owned.log": "demo",
                "/home/alice/projects/options.conf": "demo",
                "/home/alice/projects/active.swap": "swap-package",
                "/var/cache/owned.archive": "cached-package",
            },
            "backup": ["/etc/pacman.conf", "/boot/override.conf",
                       "/home/alice/projects/options.conf", "/unowned/backup.conf"],
            "app_dirs": {"/opt/demo": "演示套件", "/usr": "不得覆盖现有规则"},
            "swapfiles": ["/var/swap/active.swap", "/home/alice/projects/active.swap"],
        },
        {
            "name": "macos",
            "os": "macos",
            "env": {},
            "homes": ["/Users/carol"],
            "files": {
                "/Applications/Demo.app/Contents/MacOS/demo": "demo",
                "/System/Library/example.bin": "system-demo",
                "/Library/Preferences/example.plist": "preferences-demo",
                "/Users/carol/Documents/owned.log": "personal-package",
                "/Users/carol/Documents/options.conf": "personal-package",
            },
            "backup": ["/Users/carol/Documents/options.conf", "/unowned/backup.conf"],
            "app_dirs": {
                "/opt/homebrew/Cellar/git": "git（Homebrew）",
                "/usr/local/Caskroom/editor": "编辑器（Homebrew）",
                "/Applications": "不得覆盖现有规则",
            },
            "swapfiles": ["/private/var/vm/swapfile0", "/Volumes/Swap/active.swap"],
        },
        {
            "name": "windows",
            "os": "windows",
            "env": {
                "SystemDrive": "C:",
                "SystemRoot": "C:\\Windows",
                "ProgramFiles": "C:\\Program Files",
                "ProgramFiles(x86)": "C:\\Program Files (x86)",
                "ProgramData": "C:\\ProgramData",
            },
            "homes": ["C:\\Users\\Bob"],
            "files": {
                "C:\\Program Files\\Foo\\owned.dll": "foo-package",
                "C:\\Users\\Bob\\Documents\\owned.LOG": "personal-package",
                "C:\\Users\\Bob\\Documents\\options.conf": "personal-package",
                "C:\\Windows\\System32\\drivers\\package.sys": "driver-package",
            },
            "backup": ["C:\\Users\\Bob\\Documents\\options.conf", "D:\\unowned\\backup.conf"],
            "app_dirs": {
                "C:\\Program Files\\Foo": "Foo 编辑器",
                "D:\\Apps\\Standalone": "独立应用",
                "C:\\Windows": "不得覆盖现有规则",
            },
            "swapfiles": ["D:\\Memory\\active.sys", "C:\\swapfile.sys"],
        },
        {
            "name": "windows_custom_environment",
            "os": "windows",
            "env": {
                "SystemDrive": "R:",
                "SystemRoot": "R:\\WinRoot",
                "ProgramFiles": "S:\\Applications",
                "ProgramFiles(x86)": "T:\\Legacy Apps",
                "ProgramData": "R:\\Shared Data",
            },
            "homes": ["R:\\Profiles\\Åsa"],
            "files": {},
            "backup": [],
            "app_dirs": {"R:\\Apps\\Éditeur": "Éditeur"},
            "swapfiles": ["S:\\Custom Memory\\swap.bin"],
        },
        {
            "name": "linux_root_home",
            "os": "linux",
            "env": {},
            "homes": ["/"],
            "files": {},
            "backup": [],
            "app_dirs": {},
            "swapfiles": [],
        },
        {
            "name": "windows_empty_environment",
            "os": "windows",
            "env": {"SystemRoot": "", "ProgramFiles": "", "ProgramFiles(x86)": "", "ProgramData": ""},
            "homes": [],
            "files": {},
            "backup": [],
            "app_dirs": {},
            "swapfiles": [],
        },
        {
            "name": "linux_exact_path_boundaries",
            "os": "linux",
            "env": {},
            "homes": [],
            "files": {
                "/pkg/item": "canonical",
                "/pkg//item": "repeated-separator",
                "/pkg/./item": "dot-component",
                "//root-owned": "double-root",
            },
            "backup": ["/pkg//item"],
            "app_dirs": {
                "/apps/demo": "Canonical",
                "/apps//demo": "Repeated",
                "/apps/./demo": "Dot",
            },
            "swapfiles": ["/data//swap", "//root-swap", "/trail/swap/"],
            "focused": True,
            "extra_cases": [
                [path, True] for path in (
                    "/", "//", "/apps/demo", "/apps//demo", "/apps/./demo",
                    "/pkg", "/pkg/", "/pkg//", "/data//", "//root-dir",
                )
            ] + [
                [path, False] for path in (
                    "/data/swap", "/data//swap", "/data///swap",
                    "/root-swap", "//root-swap", "///root-swap",
                    "/pkg/item", "/pkg//item", "/pkg///item", "/pkg/./item",
                    "/root-owned", "//root-owned", "///root-owned",
                    "/trail/swap", "/trail/swap/",
                )
            ],
        },
        {
            "name": "windows_exact_path_and_unicode_boundaries",
            "os": "windows",
            "env": {},
            "homes": ["C:\\Users\\Bob"],
            "files": {
                "C:\\pkg\\item": "canonical",
                "C:\\pkg\\\\item": "repeated-separator",
                "C:\\pkg\\.\\item": "dot-component",
            },
            "backup": ["C:\\pkg\\\\item"],
            "app_dirs": {
                "C:\\apps\\demo": "Canonical",
                "C:\\apps\\\\demo": "Repeated",
                "C:\\apps\\.\\demo": "Dot",
            },
            "swapfiles": ["C:\\data\\\\swap", "C:\\trail\\swap\\"],
            "focused": True,
            "extra_cases": [
                [path, True] for path in (
                    "C:\\", "C:\\\\", "C:\\apps\\demo", "C:\\apps\\\\demo",
                    "C:\\apps\\.\\demo", "C:\\pkg", "C:\\pkg\\", "C:\\pkg\\\\",
                    "C:\\data\\\\", "C:\\data\\Service WorKer",
                    "C:\\Users\\Bob\\.config\\Code\\Service WorKer",
                    "C:\\Windows\\Service WorKer",
                )
            ] + [
                [path, False] for path in (
                    "C:\\data\\swap", "C:\\data\\\\swap", "C:\\data\\\\\\swap",
                    "C:\\pkg\\item", "C:\\pkg\\\\item", "C:\\pkg\\\\\\item", "C:\\pkg\\.\\item",
                    "C:\\trail\\swap", "C:\\trail\\swap\\", "C:\\data\\download.baK",
                    "C:\\data\\download.BAK", "C:\\Windows\\download.baK",
                    "C:\\Users\\Bob\\download.baK",
                )
            ],
        },
    ]


def generate_scenario(spec, classify):
    c = classify.Classifier(
        FakePkg(spec["files"], spec["backup"], spec["app_dirs"]),
        swapfiles=spec["swapfiles"], flavor=spec["os"], env=spec["env"], homes=spec["homes"],
    )
    pm = c.pm
    samples = {}

    def add(path, is_dir, tag):
        samples.setdefault((path, is_dir), set()).add(tag)

    # 枚举展开后的全部规则，覆盖系统规则、共同主目录规则、系统专用主目录规则、程序安装目录。
    for path in sorted(c.dir_rules):
        add(path, True, "directory_rule")
        add(pm.join(path, "fixture-child"), True, "directory_rule_descendant")
        add(pm.join(path, "fixture.bin"), False, "directory_rule_file")
    for path in sorted(c.file_rules):
        add(path, False, "file_rule")
    for path in sorted(spec["files"]):
        add(path, False, "package_file")
    for path in spec["backup"]:
        add(path, False, "package_backup")
    for path in spec["swapfiles"]:
        add(path, False, "swap_file")
    for path in spec["app_dirs"]:
        add(pm.join(path, "cache"), True, "installed_app_protects_cache_name")
        add(pm.join(path, "example.log"), False, "installed_app_protects_cache_extension")

    base = spec["homes"][0] if spec["homes"] else ("D:\\Data" if c.win else "/data")
    system = spec["env"].get("SystemRoot") or "C:\\Windows" if c.win else "/usr"
    user_data = pm.join(base, "projects")
    config = pm.join(base, ".config", "Code")
    for name in sorted(classify.CACHE_DIR_NAMES):
        add(pm.join(user_data, name), True, "cache_name_in_user_data")
        add(pm.join(config, name), True, "cache_name_in_config")
        add(pm.join(system, "fixture", name), True, "cache_name_in_system")
    for ext in classify.CACHE_EXTS:
        add(pm.join(user_data, "download" + ext), False, "cache_extension")
        add(pm.join(user_data, "download" + ext.upper()), False, "cache_extension_case")
        add(pm.join(system, "download" + ext), False, "system_protects_cache_extension")
    for rel in (".bashrc", ".profile", "normal.txt", "NTUSER.DAT", "ntuser.dat.LOG1", ".hidden.log"):
        add(pm.join(base, rel), False, "home_file")
    for rel in ("Documents", ".rustup", ".unknown", "Cache", "CACHE", "cache", "cache-like", "配置"):
        add(pm.join(base, rel), True, "home_child")
        add(pm.join(user_data, rel), True, "nested_child")
    for rel in ("nested/.config", ".config/Code/Cache", ".config/Code/User", ".unknown/cache"):
        add(pm.join(base, *rel.split("/")), True, "nested_home_precedence")

    if c.win:
        # 直接调用的路径采用目标系统分隔符；root_class / explain 的原样输入保留斜杠混用。
        for path in ("C:\\", "D:\\", "C:", "D:", "\\", "\\\\server\\share\\",
                     "\\\\server\\share\\projects\\cache", "\\\\?\\C:\\Windows",
                     "D:\\Games\\x", "C:\\Windows\\System32\\drivers\\etc",
                     "c:\\windows\\system32", "C:\\Windows\\Temp\\x",
                     "C:\\Windows\\SoftwareDistribution\\Download", "C:\\Program Files\\Foo\\bin",
                     "C:\\Program Files (x86)\\Bar", "C:\\Users\\Bob\\AppData\\Local\\Programs",
                     "C:\\Users\\Bob\\AppData\\Roaming\\Code\\Cache", "C:\\Users\\bob\\.ssh",
                     "C:\\$Recycle.Bin\\S-1-5-21", "c:/WINDOWS/System32/DRIVERS",
                     "c:/users/BOB/.CONFIG/Code/cAcHe", "C:\\WindowsElsewhere",
                     "R:/profiles/åSA/.SSH", "r:/apps/ÉDITEUR/cache", "R:\\WinRoot\\Temp",
                     "S:\\Applications\\demo", "T:\\Legacy Apps\\demo", "R:\\Shared Data\\demo"):
            add(path, True, "windows_boundary")
        for path in ("C:\\PAGEFILE.SYS", "c:/HIBERFIL.SYS", "C:/Users/Bob/Desktop/a.txt",
                     "C:\\Users\\Bob\\Downloads\\setup.LOG", "C:\\Users\\Bob\\NTUSER.DAT",
                     "C:\\Windows\\debug.LOG", "D:\\free.LOG", "D:\\free.log.backup",
                     "R:\\Profiles\\ÅSA\\NTUSER.DAT", "R:\\WinRoot\\hiberfil.sys"):
            add(path, False, "windows_file_boundary")
    else:
        for path in ("/", "//", "///", "", ".", "relative/folder", "/boot/", "/bootloader",
                     "/usr/share/icons/cache", "/usr/../cache", "/var/cache/pacman",
                     "/var/lib/docker", "/home/alice/.local/state/x", "/System/Library/Frameworks",
                     "/Applications/Safari.app", "/Library/Caches/x", "/private/var/folders/ab/cd",
                     "/opt/homebrew/Cellar/git/2.50/bin", "/Users/carol/Movies",
                     "/Users/carol/Library/Application Support/Code/Cache",
                     "/Users/carol/Library/Application Support/Code/User", "/home/alice/", "/HOME/alice"):
            add(path, True, "unix_boundary")
        for path in ("/home/alice/notes/run.log", "/home/alice/notes/a.txt", "/unowned/FILE.LOG",
                     "/unowned/file.log", "/unowned/backup.conf", "/var/cache/archive.bin"):
            add(path, False, "unix_file_boundary")

    if spec.get("focused"):
        # 专门保留不同拼写的路径，避免广泛规则样例掩盖字节精确匹配的边界。
        samples = {}
        for path, is_dir in spec["extra_cases"]:
            add(path, is_dir, "exact_path_boundary")

    cases = []
    for (path, is_dir), tags in sorted(samples.items()):
        scan_path = path.replace("/", "\\") if c.win else path
        parent, name = pm.dirname(scan_path), pm.basename(scan_path)
        joined = parent + name if parent.endswith(pm.sep) else parent + pm.sep + name
        if joined != scan_path and name:
            # dirname 会把 /data//file 的父目录化为 /data，但扫描器可能正在读 /data//。
            # 保留实际前缀，保证新API仅凭ctx/name就能表达旧版的完整文件路径。
            prefix = scan_path[:-len(name)]
            if prefix.endswith(pm.sep):
                parent = prefix
                joined = parent + name
        parent_class = c.root_class(parent)
        directory = c.dir_class(*parent_class, parent, name, scan_path) if is_dir and name else None
        file_class = (c.file_class(parent_class[0], parent, name, scan_path)
                      if not is_dir and joined == scan_path else None)
        cases.append({
            "path": path,
            "is_dir": is_dir,
            "parent": parent,
            "name": name,
            "scan_path": scan_path,
            "root": result(c.root_class(path)),
            "parent_class": result(parent_class),
            "dir": result(directory) if directory is not None else None,
            "file": file_class,
            "explain": result(c.explain(path, is_dir)),
            "tags": sorted(tags),
        })

    # 规则增加时自动扩展样例，并在生成时检查每条规则确实有直接与后代样例。
    if not spec.get("focused"):
        for path in c.dir_rules:
            assert (path, True) in samples
            assert (pm.join(path, "fixture-child"), True) in samples
            assert (pm.join(path, "fixture.bin"), False) in samples
        for path in c.file_rules:
            assert (path, False) in samples
    return {
        **{key: value for key, value in spec.items() if key not in ("focused", "extra_cases")},
        "coverage": {
            "directory_rules": len(set(c.dir_rules) & {
                pm.normcase(path) for path, is_dir in samples if is_dir
            }),
            "file_rules": len(set(c.file_rules) & {
                pm.normcase(path) for path, is_dir in samples if not is_dir
            }),
            "cache_directory_names": len(classify.CACHE_DIR_NAMES),
            "cache_extensions": len(classify.CACHE_EXTS),
            "cases": len(cases),
        },
        "cases": cases,
    }


def generate(classify, i18n):
    names = ("_LINUX_DIRS", "_LINUX_FILES", "_LINUX_HOME", "_MAC_DIRS", "_MAC_HOME",
             "_WIN_DIRS", "_WIN_FILES", "_WIN_HOME", "_COMMON_HOME")
    return {
        "schema_version": 1,
        "source": "legacy/backend/classify.py",
        "rule_tables": {name: len(getattr(classify, name)) for name in names},
        "classes": {lang: i18n.loc(classify.CLASSES, lang) for lang in ("zh", "en")},
        "scenarios": [generate_scenario(spec, classify) for spec in scenarios()],
    }


# 每行样例使用固定列数组，保留原始路径、扫描上下文及覆盖标签；列名同时写进 JSON。
CASE_COLUMNS = ("path", "is_dir", "parent", "name", "scan_path", "root",
                "parent_class", "dir", "file", "explain", "tags")
EXPECTED_COLUMNS = ("category", "reason")
REASON_COLUMNS = ("zh", "en")


def compact_fixture(output):
    """只改变存储形式；原因按首次出现编号，不丢弃任何样例字段。"""
    reasons = []
    reason_ids = {}

    def expected(value):
        if value is None:
            return None
        pair = (value["zh"], value["en"])
        if pair not in reason_ids:
            reason_ids[pair] = len(reasons)
            reasons.append(pair)
        return [value["category"], reason_ids[pair]]

    scenarios = []
    for scenario in output["scenarios"]:
        rows = []
        for case in scenario["cases"]:
            rows.append([
                expected(case[column]) if column in ("root", "parent_class", "dir", "explain")
                else case[column]
                for column in CASE_COLUMNS
            ])
        scenarios.append({**scenario, "cases": rows})
    return {
        **output,
        "schema_version": 2,
        "case_columns": CASE_COLUMNS,
        "expected_columns": EXPECTED_COLUMNS,
        "reason_columns": REASON_COLUMNS,
        "reasons": reasons,
        "scenarios": scenarios,
    }


def serialize_fixture(output):
    """每对原因、每个样例各占一行，便于审查重新生成后的 diff；末尾始终换行。"""
    compact = compact_fixture(output)

    def dump(value):
        return json.dumps(value, ensure_ascii=False, separators=(",", ":"))

    header = {key: value for key, value in compact.items() if key not in ("reasons", "scenarios")}
    lines = ["{"]
    lines.extend(dump(key) + ":" + dump(value) + "," for key, value in header.items())
    lines.append('"reasons":[')
    lines.extend(dump(reason) + ("," if i + 1 < len(compact["reasons"]) else "")
                 for i, reason in enumerate(compact["reasons"]))
    lines.append('],"scenarios":[')
    for i, scenario in enumerate(compact["scenarios"]):
        metadata = {key: value for key, value in scenario.items() if key != "cases"}
        lines.append(dump(metadata)[:-1] + ',"cases":[')
        lines.extend(dump(row) + ("," if j + 1 < len(scenario["cases"]) else "")
                     for j, row in enumerate(scenario["cases"]))
        lines.append("]}" + ("," if i + 1 < len(compact["scenarios"]) else ""))
    lines.append("]}")
    return "\n".join(lines) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="验证已提交 JSON 与重新计算结果完全一致")
    args = parser.parse_args()
    destination = Path(__file__).with_name("legacy.json")
    sys.dont_write_bytecode = True
    sys.path.insert(0, str(Path(__file__).resolve().parents[5] / "legacy"))
    with tempfile.TemporaryDirectory(prefix="diskmap-classify-fixtures-") as temp:
        os.environ["DISKMAP_CACHE_DIR"] = str(Path(temp) / "cache")
        os.environ["DISKMAP_CONFIG_DIR"] = str(Path(temp) / "config")
        from backend import classify, i18n
        output = generate(classify, i18n)
    serialized = serialize_fixture(output)
    if args.check:
        if not destination.exists() or destination.read_text(encoding="utf-8") != serialized:
            parser.exit(1, "分类对照数据与旧版不一致，请重新生成。\n")
    else:
        destination.write_text(serialized, encoding="utf-8")
    for item in output["scenarios"]:
        counts = item["coverage"]
        print(f"{item['name']}: {counts['cases']} 个样例，"
              f"{counts['directory_rules']} 条目录规则，{counts['file_rules']} 条文件规则")
    print(f"合计：{sum(len(item['cases']) for item in output['scenarios'])} 个样例")


if __name__ == "__main__":
    main()
