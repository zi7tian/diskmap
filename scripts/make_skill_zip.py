"""把 skills/diskmap 打包成可上传的技能 zip（Claude、Agensi 等平台通用格式）。

    skill_dir=$(mktemp -d)
    python3 scripts/make_skill_zip.py -o "$skill_dir/diskmap-skill.zip"

zip 内是一个顶层文件夹 diskmap/，SKILL.md 在其中。打包前检查 frontmatter、版本号、
文件类型，以及市场安全扫描常会标记的写法。同样的内容总是生成完全相同的 zip。
"""
import argparse
import os
import re
import sys
import zipfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SKILL_DIR = os.path.join(ROOT, "skills", "diskmap")
TEXT_EXTS = {".md", ".txt", ".json", ".toml", ".yaml", ".yml", ".py", ".sh"}
# 市场安全扫描会警告或拒绝的写法：出现时提示人工确认
FLAGGED = [r"\bsudo\b", r"\bcurl\b", r"\bwget\b", r"chmod\s+777", r"rm\s+-rf", r"\bbase64\b",
           r"os\.system", r"shell\s*=\s*True", r"ignore (all |any )?previous", r"\bsecretly\b",
           r"do not tell the user", r"\|\s*(ba)?sh\b"]


def frontmatter(text):
    m = re.match(r"---\r?\n(.*?)\r?\n---\r?\n", text, re.S)
    if not m:
        sys.exit("SKILL.md 必须以 --- 包围的 frontmatter 开头")
    fields, parent = {}, None
    for line in m.group(1).splitlines():
        kv = re.match(r"^(\s*)([\w-]+):\s*(.*)$", line)
        if not kv:
            continue
        indent, key, val = kv.groups()
        val = val.strip().strip('"\'')
        if indent and parent:
            fields[f"{parent}.{key}"] = val
        else:
            fields[key] = val
            parent = key if not val else None
    return fields, text[m.end():]


def workspace_version():
    """读取 Rust workspace 的版本，不加载旧版 Python 程序。"""
    with open(os.path.join(ROOT, "Cargo.toml"), encoding="utf-8") as f:
        manifest = f.read()
    section = re.search(r"(?ms)^\[workspace\.package\]\s*\n(.*?)(?=^\[|\Z)", manifest)
    if section:
        version = re.search(r'(?m)^version\s*=\s*"([^"]+)"\s*(?:#.*)?$', section.group(1))
        if version:
            return version.group(1)
    sys.exit("Cargo.toml 中缺少 workspace.package.version 字符串")


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("-o", "--output", default=os.path.join(ROOT, "dist", "diskmap-skill.zip"))
    args = ap.parse_args()

    with open(os.path.join(SKILL_DIR, "SKILL.md"), encoding="utf-8") as f:
        fm, body = frontmatter(f.read())
    name, desc = fm.get("name", ""), fm.get("description", "")
    folder = os.path.basename(SKILL_DIR)
    errors = []
    if not re.fullmatch(r"[a-z0-9]+(-[a-z0-9]+)*", name) or len(name) > 64:
        errors.append(f"name 只能用小写字母、数字和连字符，最长 64：{name!r}")
    if name != folder:
        errors.append(f"name ({name}) 必须和文件夹名 ({folder}) 一致")
    if not desc or len(desc) > 1024 or "<" in desc or ">" in desc:
        errors.append(f"description 必须为 1-1024 个字符且不含尖括号（现在 {len(desc)}）")
    if len(body.strip()) < 200:
        errors.append("SKILL.md 正文太短")
    version = workspace_version()
    if fm.get("metadata.version") != version:
        errors.append(f"metadata.version ({fm.get('metadata.version')}) 与程序版本 ({version}) 不一致")

    files = []
    for dirpath, dirnames, filenames in os.walk(SKILL_DIR):
        dirnames[:] = sorted(d for d in dirnames if not d.startswith((".", "__")))
        for fn in sorted(filenames):
            if fn.startswith("."):
                continue
            full = os.path.join(dirpath, fn)
            rel = os.path.relpath(full, SKILL_DIR).replace(os.sep, "/")
            if os.path.splitext(fn)[1].lower() not in TEXT_EXTS:
                errors.append(f"不是文本文件，不应放进技能包：{rel}")
            files.append((rel, full))
    if errors:
        sys.exit("技能包检查未通过：\n  " + "\n  ".join(errors))

    warnings = []
    for rel, full in files:
        with open(full, encoding="utf-8") as f:
            for no, line in enumerate(f, 1):
                for pat in FLAGGED:
                    if re.search(pat, line, re.I):
                        warnings.append(f"{rel}:{no}: {line.strip()[:90]}")
    for w in warnings:
        print("提示（安全扫描可能标记）：" + w, file=sys.stderr)

    os.makedirs(os.path.dirname(os.path.abspath(args.output)), exist_ok=True)
    with zipfile.ZipFile(args.output, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as z:
        for rel, full in files:
            info = zipfile.ZipInfo(f"{folder}/{rel}", date_time=(2026, 1, 1, 0, 0, 0))
            info.external_attr = 0o644 << 16
            info.compress_type = zipfile.ZIP_DEFLATED
            with open(full, "rb") as f:
                z.writestr(info, f.read())
    print(f"{args.output}：{len(files)} 个文件，{os.path.getsize(args.output):,} 字节，版本 {version}")


if __name__ == "__main__":
    main()
