"""运行中的 DiskMap 窗口把自己的地址和令牌写到用户缓存目录，
让同一用户的 AI 接口（diskmap mcp）能找到它，读取用户正在看的扫描结果。

文件权限为 0600（Windows 上位于用户自己的 LocalAppData），其他用户读不到。
"""
import json
import os
import time

from .osinfo import user_cache_dir


def instance_path():
    return os.path.join(user_cache_dir(), "instance.json")


def write(port, token, version):
    path = instance_path()
    os.makedirs(os.path.dirname(path), exist_ok=True)
    tmp = f"{path}.{os.getpid()}.tmp"
    fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as f:
        json.dump({"port": port, "token": token, "pid": os.getpid(), "version": version,
                   "started": time.time()}, f)
    os.replace(tmp, path)


def read():
    try:
        with open(instance_path(), encoding="utf-8") as f:
            data = json.load(f)
        if isinstance(data.get("port"), int) and isinstance(data.get("token"), str):
            return data
    except (OSError, ValueError, AttributeError):
        pass
    return None


def remove():
    data = read()
    if data and data.get("pid") == os.getpid():
        try:
            os.unlink(instance_path())
        except OSError:
            pass
