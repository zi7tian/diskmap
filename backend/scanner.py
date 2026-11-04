"""磁盘扫描：构建目录树，并在扫描过程中实时累加大小，支持边扫边看。"""
import os
import threading
import time
from collections import deque

from .classify import NCLS, RUNNING
from .mounts import DISK_FS, fs_usage, read_mounts, skip_paths
from .osinfo import IS_WIN, SEP, cluster_size, join_path, norm_path, pkey

DENIED = 1     # 无权限读取
MOUNT = 2      # 另一个分区的挂载点
KEEP_FILES = 150   # 单个文件夹最多单独保留的文件数，其余合并为"N 个较小文件"
_RANGE = range(NCLS)
_REPARSE_SKIP = (0xA0000003, 0xA000000C)       # Windows 目录联接（junction）、符号链接
_CLOUD_ONLY = 0x400000 | 0x40000 | 0x1000     # OneDrive 等"仅在线"文件：不占本地空间


class Dir:
    __slots__ = ("name", "path", "parent", "cls", "reason", "size", "nfiles",
                 "ndirs", "dirs", "files", "csz", "flags")

    def __init__(self, name, path, parent, cls, reason):
        self.name = name
        self.path = path
        self.parent = parent
        self.cls = cls
        self.reason = reason
        self.size = 0
        self.nfiles = 0
        self.ndirs = 0
        self.dirs = _EMPTY_DIRS   # 扫描完成后整体替换（不原地修改，便于其他线程并发读取）
        self.files = ()           # (名称, 大小, 类别) 或聚合项 (名称, 大小, 类别, 数量)
        self.csz = [0] * NCLS     # 子树内每个类别的总大小
        self.flags = 0


_EMPTY_DIRS = {}


def _norm(path):
    return norm_path(path)


def _ci_get(mapping, name):
    """Windows：大小写不敏感地查找子文件夹。"""
    low = name.lower()
    for k, v in mapping.items():
        if k.lower() == low:
            return v
    return None


class Scan:
    def __init__(self, root, classifier, procinfo, apparent=False, cross_fs=True):
        self.root_path = _norm(root)
        self.classifier = classifier
        self.procinfo = procinfo
        self.apparent = apparent
        self.cross_fs = cross_fs
        self.state = "scanning"
        self.error = None
        self.nfiles = 0
        self.ndirs = 0
        self.bytes = 0
        self.denied = 0
        self.denied_samples = []
        self.current = ""
        self.started = time.time()
        self.finished = None
        self.cancelled = False
        self.included_mounts = []
        self.skipped_mounts = []
        self.fs_used = 0
        cls, reason = classifier.root_class(self.root_path)
        name = self.root_path
        self.root = Dir(name, self.root_path, None, cls, reason)
        self._run_cache = None
        self.thread = None

    # ------------------------------------------------------------ 扫描
    def start(self, wait_for=None):
        def runner():
            if wait_for is not None:
                wait_for.wait()
            try:
                self._run()
                self.state = "cancelled" if self.cancelled else "done"
            except Exception as e:  # pragma: no cover - 防御
                self.state = "error"
                self.error = repr(e)
            self.finished = time.time()
        self.thread = threading.Thread(target=runner, name="diskmap-scan", daemon=True)
        self.thread.start()
        return self

    def cancel(self):
        self.cancelled = True

    def _run(self):
        if not os.path.isdir(self.root_path):
            raise FileNotFoundError(f"不是文件夹：{self.root_path}")
        mounts = {pkey(mp): (mp, info) for mp, info in read_mounts().items()}
        # 找到起点所在的文件系统（最长的挂载点前缀）
        rootk = pkey(self.root_path)
        best = None
        for mk in mounts:
            prefix = mk if mk.endswith(SEP) else mk + SEP
            if (rootk == mk or rootk.startswith(prefix)) and (best is None or len(mk) > len(best)):
                best = mk
        seen_devs = set()
        fs_mps = [mounts[best][0] if best else self.root_path]
        if best:
            seen_devs.add(mounts[best][1]["dev"])
        always_skip = skip_paths()
        cluster = 0 if self.apparent else cluster_size(self.root_path)

        classifier = self.classifier
        dir_class = classifier.dir_class
        file_class = classifier.file_class
        running = self.procinfo.files
        apparent = self.apparent
        cross_fs = self.cross_fs
        seen_inodes = set()
        stack = [self.root]

        while stack:
            if self.cancelled:
                return
            d = stack.pop()
            dpath = d.path
            self.current = dpath
            try:
                it = os.scandir(dpath)
            except PermissionError:
                d.flags |= DENIED
                self.denied += 1
                if len(self.denied_samples) < 50:
                    self.denied_samples.append(dpath)
                continue
            except OSError:
                continue

            files = []
            subdirs = []
            direct = 0
            dcsz = [0] * NCLS
            dcls, dreason = d.cls, d.reason
            with it:
                for e in it:
                    try:
                        if e.is_dir(follow_symlinks=False):
                            p = e.path
                            if IS_WIN:
                                # 目录联接（如 "Application Data"）指向别处，跟进去会重复计算甚至死循环
                                if (getattr(e, "is_junction", bool)() or getattr(
                                        e.stat(follow_symlinks=False), "st_reparse_tag", 0) in _REPARSE_SKIP):
                                    continue
                                pk = p.lower()
                            else:
                                pk = p
                            if pk in always_skip:
                                continue
                            m = mounts.get(pk)
                            flags = 0
                            if m is not None:
                                m = m[1]
                                if (not cross_fs or m["fstype"] not in DISK_FS or m.get("hidden")
                                        or m["dev"] in seen_devs):
                                    self.skipped_mounts.append({"path": p, "fstype": m["fstype"]})
                                    continue
                                seen_devs.add(m["dev"])
                                fs_mps.append(p)
                                self.included_mounts.append({"path": p, "fstype": m["fstype"],
                                                             "source": m["source"]})
                                flags = MOUNT
                            cls, reason = dir_class(dcls, dreason, dpath, e.name, p)
                            child = Dir(e.name, p, d, cls, reason)
                            child.flags = flags
                            subdirs.append(child)
                        else:
                            st = e.stat(follow_symlinks=False)
                            if IS_WIN:
                                # Windows 的 DirEntry 不提供硬链接计数；"实际占用"按簇大小向上取整估算
                                sz = st.st_size
                                if not apparent:
                                    if st.st_file_attributes & _CLOUD_ONLY:
                                        sz = 0
                                    elif sz and cluster:
                                        sz = -(-sz // cluster) * cluster
                            else:
                                if st.st_nlink > 1:
                                    key = (st.st_dev, st.st_ino)
                                    if key in seen_inodes:
                                        continue
                                    seen_inodes.add(key)
                                sz = st.st_size if apparent else st.st_blocks * 512
                            cls = file_class(dcls, dpath, e.name, e.path)
                            files.append((e.name, sz, cls))
                            direct += sz
                            dcsz[cls] += sz
                    except OSError:
                        continue

            nf = len(files)
            if nf > KEEP_FILES:
                files.sort(key=lambda f: -f[1])
                keep = files[:KEEP_FILES]
                rest_sz = 0
                rest_n = 0
                rest_cls = [0] * NCLS
                for f in files[KEEP_FILES:]:
                    if pkey(join_path(dpath, f[0])) in running:
                        keep.append(f)   # 正在使用的文件不合并，以便标注
                    else:
                        rest_sz += f[1]
                        rest_n += 1
                        rest_cls[f[2]] += f[1] + 1
                if rest_n:
                    dom = max(_RANGE, key=rest_cls.__getitem__)
                    keep.append(("", rest_sz, dom, rest_n))
                files = keep
            d.files = tuple(files)
            d.dirs = {c.name: c for c in subdirs}

            nd = len(subdirs)
            pairs = [(k, dcsz[k]) for k in _RANGE if dcsz[k]]
            a = d
            while a is not None:
                a.size += direct
                a.nfiles += nf
                a.ndirs += nd
                if pairs:
                    cs = a.csz
                    for k, v in pairs:
                        cs[k] += v
                a = a.parent
            self.nfiles += nf
            self.ndirs += nd
            self.bytes += direct
            # 逆序压栈，让字母序靠前的先被扫描（只影响实时显示的观感）
            stack.extend(reversed(subdirs))

        used = 0
        for mp in fs_mps:
            u = fs_usage(mp)
            if u:
                used += u["used"]
        self.fs_used = used

    # ------------------------------------------------------------ 查询
    def find(self, path):
        """返回 (文件夹节点, None) 或 (父文件夹节点, 文件元组)，找不到返回 (None, None)。"""
        path = _norm(path)
        root = self.root
        if pkey(path) == pkey(root.path):
            return root, None
        prefix = root.path if root.path.endswith(SEP) else root.path + SEP
        if not pkey(path).startswith(pkey(prefix)):
            return None, None
        comps = path[len(prefix):].split(SEP)
        node = root
        for c in comps[:-1]:
            nxt = node.dirs.get(c)
            if nxt is None and IS_WIN:
                nxt = _ci_get(node.dirs, c)
            if nxt is None:
                return None, None
            node = nxt
        last = comps[-1]
        child = node.dirs.get(last)
        if child is None and IS_WIN:
            child = _ci_get(node.dirs, last)
        if child is not None:
            return child, None
        low = last.lower() if IS_WIN else last
        for f in node.files:
            if len(f) == 3 and (f[0] == last or (IS_WIN and f[0].lower() == low)):
                return node, f
        return None, None

    def running_hits(self):
        """扫描树中正在使用的文件：[(路径, 大小, 静态类别)]。"""
        stamp = self.procinfo.stamp
        if self._run_cache and self._run_cache[0] == stamp and self.state != "scanning":
            return self._run_cache[1]
        hits = []
        for p in list(self.procinfo.files):
            node, f = self.find(p)
            if f is not None:
                hits.append((p, f[1], f[2]))
        if self.state != "scanning":
            self._run_cache = (stamp, hits)
        return hits

    def totals(self, node):
        cs = list(node.csz)
        prefix = pkey(node.path if node.path.endswith(SEP) else node.path + SEP)
        for p, sz, cls in self.running_hits():
            if pkey(p).startswith(prefix):
                cs[cls] -= sz
                cs[RUNNING] += sz
        return cs

    def view(self, path, frac=1 / 40000, max_nodes=60000, max_depth=48, pkgdb=None):
        """把以 path 为根的子树序列化为 JSON 友好的结构。

        小于 (根大小 × frac) 的项目会被合并为一个"N 个小项目"块，
        以控制传给前端的数据量（反正它们在屏幕上也小于几个像素）。
        """
        node, f = self.find(path)
        if node is None or f is not None:
            return None
        proc = self.procinfo
        running = proc.files
        rdirs = proc.dirs
        pkgfiles = pkgdb.files if pkgdb is not None else {}
        minsz = max(1, int(node.size * frac))
        budget = max_nodes

        def ser_dir(d):
            o = {"n": d.name, "s": d.size, "c": d.cls, "t": "d", "nf": d.nfiles, "nd": d.ndirs}
            if d.flags:
                o["fl"] = d.flags
            r = rdirs.get(pkey(d.path))
            if r:
                o["r"] = r
            return o

        out = ser_dir(node)
        queue = deque([(node, out, 0)])
        while queue:
            d, o, depth = queue.popleft()
            if depth >= max_depth or budget <= 0:
                if d.dirs or d.files:
                    o["m"] = 1
                continue
            kids = []
            small_sz = 0
            small_n = 0
            small_cls = [0] * NCLS
            for c in list(d.dirs.values()):
                if c.size >= minsz:
                    co = ser_dir(c)
                    kids.append(co)
                    queue.append((c, co, depth + 1))
                else:
                    small_sz += c.size
                    small_n += 1
                    small_cls[c.cls] += c.size + 1
            for f in d.files:
                sz = f[1]
                if sz >= minsz:
                    if len(f) == 4:
                        kids.append({"n": "", "s": sz, "c": f[2], "t": "a", "k": f[3]})
                        continue
                    fo = {"n": f[0], "s": sz, "c": f[2], "t": "f"}
                    p = join_path(d.path, f[0])
                    if pkey(p) in running:
                        fo["sc"] = f[2]
                        fo["c"] = RUNNING
                        fo["p"] = proc.summary(p)
                    pk = pkgfiles.get(p)
                    if pk:
                        fo["pk"] = pk
                    kids.append(fo)
                else:
                    small_sz += sz
                    small_n += f[3] if len(f) == 4 else 1
                    small_cls[f[2]] += sz + 1
            acc = small_sz
            for k in kids:
                acc += k["s"]
            if d.size > acc:   # 扫描进行中时大小可能还在增长，补齐差额
                small_sz += d.size - acc
            if small_sz > 0 or small_n:
                dom = max(_RANGE, key=small_cls.__getitem__) if small_n else d.cls
                kids.append({"n": "", "s": small_sz, "c": dom, "t": "a", "k": small_n})
            kids.sort(key=lambda k: -k["s"])
            o["ch"] = kids
            budget -= len(kids)
        return node, out
