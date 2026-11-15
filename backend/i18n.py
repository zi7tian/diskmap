"""界面与输出语言：中文（zh）和英文（en）。

- 长期保存的文字（分类规则的原因、类别名称等）用 T(中文, 英文) 表示：它本身是中文字符串
  （便于比较和测试），在格式化 / 输出时按当前语言取值。
- 当场生成的文字用 L(中文, 英文)，直接返回当前语言的版本。
- 当前语言：每个 HTTP 请求可以单独指定（线程局部），否则用全局默认值。
  默认值的优先级：命令行 --lang > 环境变量 DISKMAP_LANG > 界面里保存的设置 > 系统语言。
"""
import json
import os
import subprocess
import threading
from contextlib import contextmanager

from .osinfo import IS_MAC, IS_WIN, user_config_dir

LANGS = ("zh", "en")
_default = None
_local = threading.local()


class T(str):
    """中英双语文本。str 值为中文；str() / f-string / loc() 输出当前语言。"""

    def __new__(cls, zh, en):
        obj = str.__new__(cls, zh)
        obj.en = en
        return obj

    def text(self, lang=None):
        return self.en if (lang or current()) == "en" else str.__str__(self)

    def __str__(self):
        return self.text()

    def __format__(self, spec):
        return format(self.text(), spec)

    def __reduce__(self):
        return (T, (str.__str__(self), self.en))


def norm(lang):
    """'zh_CN.UTF-8' → 'zh'，'en-US' → 'en'，其他 → None。"""
    if not lang:
        return None
    lang = str(lang).strip().lower()
    for code in LANGS:
        if lang == code or lang.startswith(code + "_") or lang.startswith(code + "-"):
            return code
    return None


def current():
    lang = getattr(_local, "lang", None)
    if lang:
        return lang
    if _default is None:
        set_default(resolve())
    return _default


def L(zh, en):
    return en if current() == "en" else zh


def plural(n, one, many=None):
    return one if n == 1 else (many or one + "s")


def tx(value):
    """T → 当前语言的普通字符串；其他值原样返回。"""
    return value.text() if isinstance(value, T) else value


def loc(obj, lang=None):
    """把数据结构里的所有 T 换成指定语言（默认当前语言）的普通字符串，用于输出 JSON。"""
    lang = lang or current()

    def walk(o):
        if isinstance(o, T):
            return o.text(lang)
        if isinstance(o, dict):
            return {k: walk(v) for k, v in o.items()}
        if isinstance(o, (list, tuple)):
            return [walk(v) for v in o]
        return o
    return walk(obj)


@contextmanager
def use(lang):
    """在当前线程中临时使用某种语言（lang 为 None 时不变）。"""
    prev = getattr(_local, "lang", None)
    _local.lang = norm(lang) or prev
    try:
        yield
    finally:
        _local.lang = prev


def set_default(lang):
    global _default
    _default = norm(lang) or "en"


# ------------------------------------------------------------------ 系统语言与保存的设置
def _mac_lang():
    try:
        out = subprocess.run(["defaults", "read", "-g", "AppleLanguages"], capture_output=True,
                             text=True, timeout=3).stdout
    except (OSError, subprocess.SubprocessError):
        return None
    for line in out.splitlines():
        line = line.strip().strip('",()')
        if line:
            return norm(line) or "en"
    return None


def _win_lang():
    try:
        import ctypes
        langid = ctypes.windll.kernel32.GetUserDefaultUILanguage()
    except Exception:
        return None
    return "zh" if (langid & 0x3FF) == 0x04 else "en"


def system_lang():
    if IS_WIN:
        found = _win_lang()
        if found:
            return found
    if IS_MAC:
        found = _mac_lang()   # 从访达启动的程序没有 LANG 环境变量
        if found:
            return found
    for var in ("LANGUAGE", "LC_ALL", "LC_MESSAGES", "LANG"):
        value = os.environ.get(var, "").split(":")[0]
        if value and value not in ("C", "POSIX") and not value.startswith("C."):
            return "zh" if norm(value) == "zh" else "en"
    return "en"


def settings_path():
    return os.path.join(user_config_dir(), "settings.json")


def load_settings():
    try:
        with open(settings_path(), encoding="utf-8") as f:
            data = json.load(f)
        return data if isinstance(data, dict) else {}
    except (OSError, ValueError):
        return {}


def save_lang(lang):
    """保存界面里选择的语言，下次启动（包括 mcp / analyze）沿用。"""
    code = norm(lang)
    if code is None:
        raise ValueError(f"unsupported language: {lang}")
    lang = code
    data = load_settings()
    data["lang"] = lang
    path = settings_path()
    try:
        os.makedirs(os.path.dirname(path), exist_ok=True)
        tmp = path + ".tmp"
        with open(tmp, "w", encoding="utf-8") as f:
            json.dump(data, f, ensure_ascii=False, indent=2)
        os.replace(tmp, path)
    except OSError:
        pass   # 只是下次启动记不住，不影响本次使用
    set_default(lang)
    return lang


def resolve(cli=None):
    return (norm(cli) or norm(os.environ.get("DISKMAP_LANG"))
            or norm(load_settings().get("lang")) or system_lang())


def pre_scan_argv(argv):
    """在建立命令行解析器之前取出 --lang（帮助文字也要用对应语言）。"""
    for i, a in enumerate(argv):
        if a == "--lang" and i + 1 < len(argv):
            return argv[i + 1]
        if a.startswith("--lang="):
            return a.split("=", 1)[1]
    return None
