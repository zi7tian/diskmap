"""T10 的版本检查、打包暂存、产物整理和校验和。只使用 Python 标准库。"""

import argparse
import hashlib
import io
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import tempfile
import tomllib
import zipfile


ROOT = Path(__file__).resolve().parents[3]
PACKAGING = ROOT / "crates/diskmap-desktop/packaging"
STAGE = ROOT / "target/packaging"
OUT = STAGE / "out"
RELEASE = ROOT / "target/release"
TAURI_CLI_VERSION = "2.12.5"
TAURI_BUNDLER_VERSION = "2.10.5"
PLATFORMS = (
    "windows-x64", "macos-arm64", "macos-x64", "linux-x64",
)


def metadata():
    """核对 CLI 与库的主、次版本及发布标签，避免昂贵的无效打包。"""
    manifest = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))
    version = manifest["workspace"]["package"]["version"]
    lock = tomllib.loads((ROOT / "Cargo.lock").read_text(encoding="utf-8"))
    tauri_versions = [p["version"] for p in lock["package"] if p["name"] == "tauri"]
    cli_series = TAURI_CLI_VERSION.split(".")[:2]
    if not tauri_versions or any(
        tauri_version.split(".")[:2] != cli_series for tauri_version in tauri_versions
    ):
        raise ValueError(
            "打包 CLI 与 tauri 的主、次版本不匹配 / Packaging CLI and tauri major/minor versions differ: "
            f"cli={TAURI_CLI_VERSION}, tauri={tauri_versions}"
        )
    ref = os.environ.get("GITHUB_REF", "")
    if ref.startswith("refs/tags/") and ref != f"refs/tags/v{version}":
        raise ValueError(f"标签与 workspace 版本不同 / Tag and workspace version differ: {ref}, {version}")
    return {"version": version, "tauri_cli": TAURI_CLI_VERSION}


def replace_once(text, old, new):
    """锁定上游模板的修改位置，升级 Tauri 后必须重新审查。"""
    if text.count(old) != 1:
        raise ValueError(f"上游模板已变化 / Upstream template changed: {old!r}")
    return text.replace(old, new, 1)


def isolate():
    """在步骤中取得 RUNNER_TEMP，再写入后续步骤使用的环境文件。"""
    base = Path(os.environ["RUNNER_TEMP"]) / "diskmap"
    with open(os.environ["GITHUB_ENV"], "a", encoding="utf-8") as output:
        output.write(f"DISKMAP_CACHE_DIR={base / 'cache'}\n")
        output.write(f"DISKMAP_CONFIG_DIR={base / 'config'}\n")


def windows_templates(version):
    # cargo install --locked 下载的源码在 registry/src，生成物全部放 target。
    cargo_home = Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo"))
    source_pattern = f"registry/src/*/tauri-bundler-{TAURI_BUNDLER_VERSION}"
    sources = sorted(cargo_home.glob(source_pattern))
    if not sources:
        # CI 缓存可能只还原已安装的 CLI 和 crate 压缩包，按固定版本补回模板源码。
        subprocess.run(["cargo", "info", f"tauri-bundler@{TAURI_BUNDLER_VERSION}"], cwd=ROOT, check=True)
        sources = sorted(cargo_home.glob(source_pattern))
    if not sources:
        raise FileNotFoundError("请先安装固定版本 Tauri CLI / Install the pinned Tauri CLI first")
    source = sources[0] / "src/bundle/windows"
    nsis = (source / "nsis/installer.nsi").read_text(encoding="utf-8")
    nsis = replace_once(
        nsis,
        'StrCpy $INSTDIR "$LOCALAPPDATA\\${PRODUCTNAME}"',
        'StrCpy $INSTDIR "$LOCALAPPDATA\\Programs\\${PRODUCTNAME}"',
    )
    nsis_path = STAGE / "installer.nsi"
    nsis_path.write_text(nsis, encoding="utf-8")

    wix = (source / "msi/main.wxs").read_text(encoding="utf-8")
    # 上游会复用 NSIS 留下的 HKCU 路径，导致随后安装 MSI 落在当前用户目录。
    # 保留 MSI 自己的命名 InstallDir 搜索，让 MSI 更新仍能复用自己的位置。
    nsis_search = next(line for line in wix.splitlines() if '<RegistrySearch Id="PrevInstallDirNoName"' in line)
    wix = replace_once(wix, nsis_search + "\n", "")
    wix_path = STAGE / "main.wxs"
    wix_path.write_text(wix, encoding="utf-8")

    # Windows Installer 比较前三个数字。Tauri 默认转换会拒绝 alpha.1。
    numeric_version = version.split("-", 1)[0].split("+", 1)[0]
    numbers = [int(part) for part in numeric_version.split(".")]
    if len(numbers) != 3 or any(n > limit for n, limit in zip(numbers, (255, 255, 65535))):
        raise ValueError(f"MSI 版本超出范围 / MSI version out of range: {numeric_version}")
    return {"bundle": {"windows": {
        "nsis": {"template": str(nsis_path)},
        "wix": {"template": str(wix_path), "version": numeric_version},
    }}}


def prepare(target):
    values = metadata()
    host_info = subprocess.check_output(["rustc", "-vV"], text=True)
    host = re.search(r"^host: (.+)$", host_info, re.MULTILINE).group(1)
    if host != target:
        raise ValueError(f"打包 runner 架构不符 / Packaging runner architecture mismatch: {host}, {target}")
    suffix = ".exe" if "windows" in target else ""
    sidecars = STAGE / "sidecars"
    sidecars.mkdir(parents=True, exist_ok=True)
    shutil.copy2(RELEASE / f"diskmap-cli{suffix}", sidecars / f"diskmap-cli-{target}{suffix}")
    config = windows_templates(values["version"]) if suffix else {}
    (STAGE / "tauri.platform.json").write_text(json.dumps(config, indent=2), encoding="utf-8")
    # target 可能来自上一版本的 CI 缓存，发布目录每轮重新生成。
    if OUT.exists():
        shutil.rmtree(OUT)
    OUT.mkdir()


def bundle_file(kind, version, extension):
    candidates = sorted((RELEASE / "bundle" / kind).glob(f"*{version}*.{extension}"))
    if len(candidates) != 1:
        raise ValueError(f"需要唯一安装包 / Expected one bundle: {kind}, {candidates}")
    return candidates[0]


def archive(path, files):
    """files 是包内名字到源文件的映射，程序与许可证置于压缩包根目录。"""
    if path.suffix == ".zip":
        with zipfile.ZipFile(path, "w", compression=zipfile.ZIP_DEFLATED) as output:
            for name, source in files.items():
                output.write(source, name)
    else:
        with tarfile.open(path, "w:gz") as output:
            for name, source in files.items():
                output.add(source, arcname=name, recursive=False)


def normalize_deb(source, destination, version):
    # Debian 用 ~ 排序预发布版本，使 alpha < 正式版。文件名仍保留 SemVer。
    # Tauri 2.10.5 固定生成这三个 ar 成员，不需要 dpkg 或管理员权限。
    deb_version = version.replace("-", "~", 1)
    with tempfile.TemporaryDirectory(prefix="deb-", dir=STAGE) as work:
        work = Path(work)
        for member in ("debian-binary", "control.tar.gz", "data.tar.gz"):
            with (work / member).open("wb") as output:
                subprocess.run(["ar", "p", str(source), member], stdout=output, check=True)
        control = work / "control.tar.gz"
        with tarfile.open(control, "r:gz") as incoming:
            contents = []
            for item in incoming:
                data = incoming.extractfile(item).read() if item.isfile() else None
                if item.name.removeprefix("./") == "control":
                    old = f"Version: {version}\n".encode()
                    if data.count(old) != 1:
                        raise ValueError("deb 版本字段不符 / Unexpected deb version field")
                    data = data.replace(old, f"Version: {deb_version}\n".encode(), 1)
                    item.size = len(data)
                contents.append((item, data))
        with tarfile.open(control, "w:gz") as outgoing:
            for item, data in contents:
                outgoing.addfile(item, io.BytesIO(data) if data is not None else None)
        destination.unlink(missing_ok=True)
        subprocess.run([
            "ar", "cr", str(destination), "debian-binary", "control.tar.gz", "data.tar.gz",
        ], cwd=work, check=True)


def collect(platform, bundles):
    version = metadata()["version"]
    OUT.mkdir(parents=True, exist_ok=True)
    suffix = ".exe" if platform == "windows-x64" else ""
    cli = {f"diskmap-cli{suffix}": RELEASE / f"diskmap-cli{suffix}", "LICENSE": ROOT / "LICENSE"}
    cli_extension = "zip" if suffix else "tar.gz"
    archive(OUT / f"diskmap-cli-{version}-{platform}.{cli_extension}", cli)
    if suffix:
        archive(OUT / f"DiskMap-{version}-{platform}-portable.zip", {
            f"DiskMap{suffix}": RELEASE / f"DiskMap{suffix}", **cli,
        })
    if platform == "linux-x64":
        archive(OUT / f"DiskMap-{version}-{platform}.tar.gz", {
            "DiskMap": RELEASE / "DiskMap", **cli,
            "install-desktop.sh": PACKAGING / "install-desktop.sh",
            "diskmap.png": ROOT / "assets/icons/diskmap-512.png",
        })
    formats = {
        "nsis": ("exe", "-setup.exe"), "msi": ("msi", ".msi"),
        "dmg": ("dmg", ".dmg"), "deb": ("deb", ".deb"),
        "appimage": ("AppImage", ".AppImage"),
    }
    for kind in bundles.split(","):
        extension, ending = formats[kind]
        source = bundle_file(kind, version, extension)
        destination = OUT / f"DiskMap-{version}-{platform}{ending}"
        if kind == "deb":
            normalize_deb(source, destination, version)
        else:
            shutil.copy2(source, destination)
    for path in sorted(OUT.iterdir()):
        print(path.relative_to(ROOT))


def checksums(directory):
    version = metadata()["version"]
    expected = {"diskmap-skill.zip"}
    endings = {
        "windows-x64": ("-setup.exe", ".msi", "-portable.zip"),
        "macos-arm64": (".dmg",), "macos-x64": (".dmg",),
        "linux-x64": (".AppImage", ".deb", ".tar.gz"),
    }
    for platform in PLATFORMS:
        expected.update(f"DiskMap-{version}-{platform}{ending}" for ending in endings[platform])
        extension = "zip" if platform == "windows-x64" else "tar.gz"
        expected.add(f"diskmap-cli-{version}-{platform}.{extension}")
    actual = {p.name for p in directory.iterdir() if p.name != "SHA256SUMS.txt"}
    if actual != expected:
        raise ValueError(f"发布产物不完整 / Release artifact mismatch: missing={expected - actual}, extra={actual - expected}")
    lines = []
    for name in sorted(expected):
        with (directory / name).open("rb") as source:
            digest = hashlib.file_digest(source, "sha256").hexdigest()
        lines.append(f"{digest}  {name}\n")
    (directory / "SHA256SUMS.txt").write_text("".join(lines), encoding="utf-8")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("metadata")
    commands.add_parser("isolate")
    preparation = commands.add_parser("prepare")
    preparation.add_argument("--target", required=True)
    collection = commands.add_parser("collect")
    collection.add_argument("--platform", choices=PLATFORMS, required=True)
    collection.add_argument("--bundles", required=True)
    checksum = commands.add_parser("checksums")
    checksum.add_argument("--directory", type=Path, default=OUT)
    args = parser.parse_args()
    if args.command == "metadata":
        values = metadata()
        for name, value in values.items():
            print(f"{name}={value}")
        if os.environ.get("GITHUB_OUTPUT"):
            with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as output:
                output.writelines(f"{name}={value}\n" for name, value in values.items())
    elif args.command == "isolate":
        isolate()
    elif args.command == "prepare":
        prepare(args.target)
    elif args.command == "collect":
        collect(args.platform, args.bundles)
    else:
        checksums(args.directory.resolve())


if __name__ == "__main__":
    main()
