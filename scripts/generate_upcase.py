#!/usr/bin/env python3
"""由固定版本的 UnicodeData.txt 重建 Windows BMP 简单大写表。"""

import argparse
import hashlib
from pathlib import Path
import sys
from urllib.request import urlopen


# 升级 Unicode 时必须同时更新版本、SHA-256，并审查路径等价关系的兼容性变化。
UNICODE_VERSION = "17.0.0"
SOURCE_URL = f"https://www.unicode.org/Public/{UNICODE_VERSION}/ucd/UnicodeData.txt"
SOURCE_SHA256 = "2e1efc1dcb59c575eedf5ccae60f95229f706ee6d031835247d843c11d96470c"
DEFAULT_OUTPUT = (
    Path(__file__).resolve().parent.parent
    / "crates/diskmap-core/src/platform/upcase.rs"
)

# 模板包含完整许可和表定义，不读取旧映射；目标文件不存在时也能完整重建。
LICENSE = """// 以下 Unicode 数据适用 Unicode License V3：
// UNICODE LICENSE V3
//
// COPYRIGHT AND PERMISSION NOTICE
//
// Copyright © 1991-2026 Unicode, Inc.
//
// NOTICE TO USER: Carefully read the following legal agreement. BY
// DOWNLOADING, INSTALLING, COPYING OR OTHERWISE USING DATA FILES, AND/OR
// SOFTWARE, YOU UNEQUIVOCALLY ACCEPT, AND AGREE TO BE BOUND BY, ALL OF THE
// TERMS AND CONDITIONS OF THIS AGREEMENT. IF YOU DO NOT AGREE, DO NOT
// DOWNLOAD, INSTALL, COPY, DISTRIBUTE OR USE THE DATA FILES OR SOFTWARE.
//
// Permission is hereby granted, free of charge, to any person obtaining a
// copy of data files and any associated documentation (the "Data Files") or
// software and any associated documentation (the "Software") to deal in the
// Data Files or Software without restriction, including without limitation
// the rights to use, copy, modify, merge, publish, distribute, and/or sell
// copies of the Data Files or Software, and to permit persons to whom the
// Data Files or Software are furnished to do so, provided that either (a)
// this copyright and permission notice appear with all copies of the Data
// Files or Software, or (b) this copyright and permission notice appear in
// associated Documentation.
//
// THE DATA FILES AND SOFTWARE ARE PROVIDED "AS IS", WITHOUT WARRANTY OF ANY
// KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
// MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT OF
// THIRD PARTY RIGHTS.
//
// IN NO EVENT SHALL THE COPYRIGHT HOLDER OR HOLDERS INCLUDED IN THIS NOTICE
// BE LIABLE FOR ANY CLAIM, OR ANY SPECIAL INDIRECT OR CONSEQUENTIAL DAMAGES,
// OR ANY DAMAGES WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS,
// WHETHER IN AN ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION,
// ARISING OUT OF OR IN CONNECTION WITH THE USE OR PERFORMANCE OF THE DATA
// FILES OR SOFTWARE.
//
// Except as contained in this notice, the name of a copyright holder shall
// not be used in advertising or otherwise to promote the sale, use or other
// dealings in these Data Files or Software without prior written
// authorization of the copyright holder.
"""

TABLE = """
pub(super) static UPPER: [u16; 65536] = table();

const fn table() -> [u16; 65536] {
    let mut result = [0; 65536];
    let mut i = 0;
    while i < result.len() {
        result[i] = i as u16;
        i += 1;
    }
    i = 0;
    while i < MAPPINGS.len() {
        let (from, to) = MAPPINGS[i];
        result[from as usize] = to;
        i += 1;
    }
    result
}

pub(super) const MAPPINGS: &[(u16, u16)] = &[
"""


def generate(data):
    digest = hashlib.sha256(data).hexdigest()
    if digest != SOURCE_SHA256:
        raise ValueError(
            f"UnicodeData.txt 校验失败 / checksum mismatch: {digest}; "
            f"预期 / expected: {SOURCE_SHA256}"
        )
    mappings = []
    for row in data.decode("utf-8").splitlines():
        fields = row.split(";")
        source = int(fields[0], 16)
        if fields[12]:
            target = int(fields[12], 16)
            if source <= 0xFFFF and target <= 0xFFFF:
                mappings.append((source, target))
    header = f"""
// Unicode {UNICODE_VERSION} UnicodeData.txt 第 12 列（从零计数）的 BMP 简单大写映射。
// 来源：{SOURCE_URL}
// 原文件 SHA-256：{SOURCE_SHA256}
// 生成脚本：scripts/generate_upcase.py（从仓库根目录运行）。
// 在线重建：python3 -B scripts/generate_upcase.py
// 离线检查：python3 -B scripts/generate_upcase.py --input /path/to/UnicodeData.txt --check
// 可用 --output 指定其他目标；--check 只比较完整生成内容，不写文件。
// 升级时同步修改脚本的版本和 SHA-256，并审查路径等价关系的兼容性变化。
// 生成：筛选码点及 Simple_Uppercase_Mapping 均 <= 0xFFFF 的行；空映射保留原值。
// 固定数据版本保证各平台和 Rust 工具链使用同一规则；不读取具体卷的 $UpCase。
"""
    entries = "".join(f"    (0x{source:04X}, 0x{target:04X}),\n" for source, target in mappings)
    return (LICENSE + header + TABLE + entries + "];\n").encode("utf-8"), len(mappings)


def main():
    parser = argparse.ArgumentParser(
        description="重建固定版本的 Unicode 简单大写表 / Rebuild the pinned Unicode uppercase table"
    )
    parser.add_argument(
        "--input", type=Path,
        help="使用本地 UnicodeData.txt；默认下载 / Local input; download by default",
    )
    parser.add_argument(
        "--output", type=Path, default=DEFAULT_OUTPUT,
        help="目标文件；默认 crates/diskmap-core/src/platform/upcase.rs / Destination Rust file",
    )
    parser.add_argument(
        "--check", action="store_true",
        help="验证目标与生成内容一致，不写文件 / Check the destination without writing",
    )
    args = parser.parse_args()
    try:
        if args.input is not None:
            data = args.input.read_bytes()
        else:
            with urlopen(SOURCE_URL, timeout=30) as response:
                data = response.read()
        generated, count = generate(data)
        if args.check:
            if args.output.read_bytes() != generated:
                raise ValueError(
                    f"生成内容与目标不同 / generated content differs: {args.output}"
                )
            print(f"已验证 / Verified: {args.output} ({count} mappings)")
        else:
            args.output.write_bytes(generated)
            print(f"已生成 / Generated: {args.output} ({count} mappings)")
    except (OSError, ValueError) as error:
        print(f"生成失败 / Generation failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
