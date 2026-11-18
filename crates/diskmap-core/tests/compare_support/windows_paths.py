"""对比脚本复用 T15 路径键；只读取生成表，不依赖宿主 Python 的 Unicode 版本。"""
from functools import lru_cache
from pathlib import Path
import re


@lru_cache(maxsize=1)
def _upcase():
    source = (Path(__file__).resolve().parents[2] / "src/platform/upcase.rs").read_text(encoding="utf-8")
    declaration = "pub(super) const MAPPINGS: &[(u16, u16)] = &["
    body = source.split(declaration, 1)[1].split("];", 1)[0]
    pairs = re.findall(r"\(0x([0-9A-Fa-f]{4}), 0x([0-9A-Fa-f]{4})\),", body)
    # 格式若改变必须失败，不能默默丢掉部分映射或改用 upper/casefold。
    remainder = re.sub(r"\(0x[0-9A-Fa-f]{4}, 0x[0-9A-Fa-f]{4}\),", "", body)
    if remainder.strip() or not pairs:
        raise ValueError("无法完整解析 T15 MAPPINGS / Cannot parse complete T15 MAPPINGS")
    result = list(range(65536))
    seen = set()
    for old, new in pairs:
        old, new = int(old, 16), int(new, 16)
        if old in seen:
            raise ValueError("T15 映射有重复码元 / Duplicate T15 mapping")
        seen.add(old)
        result[old] = new
    return result


def windows_path_key(path: str) -> bytes:
    """输入 Unicode/WTF-16 字符串，输出与 Rust path_key(Windows) 一致的 WTF-8。"""
    return windows_path_bytes_key(path.encode("utf-8", "surrogatepass"))


def windows_path_bytes_key(path: bytes) -> bytes:
    """精确复现 platform/path.rs：BMP 简单大写；增补字符、非法字节原样保留。"""
    upper = _upcase()
    output = bytearray()
    index = 0
    while index < len(path):
        first = path[index]
        if first < 0x80:
            output.append(92 if first == 47 else first - 32 if 97 <= first <= 122 else first)
            index += 1
            continue
        if (0xC2 <= first <= 0xDF and index + 1 < len(path)
                and path[index + 1] & 0xC0 == 0x80):
            unit = (first & 0x1F) << 6 | path[index + 1] & 0x3F
            width = 2
        elif (0xE0 <= first <= 0xEF and index + 2 < len(path)
              and path[index + 1] & 0xC0 == 0x80 and path[index + 2] & 0xC0 == 0x80
              and (first != 0xE0 or path[index + 1] >= 0xA0)):
            unit = (first & 15) << 12 | (path[index + 1] & 63) << 6 | path[index + 2] & 63
            width = 3
        else:
            output.append(first)
            index += 1
            continue
        output.extend(chr(upper[unit]).encode("utf-8", "surrogatepass"))
        index += width
    return bytes(output)


if __name__ == "__main__":
    # Rust 集成测试传十六进制，包含孤立代理项和非法字节也不经过 JSON 字符串解码。
    import json
    import sys
    json.dump([windows_path_bytes_key(bytes.fromhex(path)).hex() for path in json.load(sys.stdin)], sys.stdout)
