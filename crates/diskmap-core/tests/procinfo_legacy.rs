//! 使用同一份离线记录，对比 Rust 公共接口和 Python 旧版的摘要、详情与目录计数。
//! 不读取真实进程；依次探测 python3、python、py -3，均不可用时跳过，旧版执行错误仍失败。
//! 输入的特殊路径按 UTF-16 码元传递，输出路径按 WTF-8 十六进制逐字节比较；其他字段不转换。

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[path = "support/child.rs"]
mod child;
use child::{ChildGuard, command_output};

use diskmap_core::i18n::{self, Lang, Text};
use diskmap_core::platform::Os;
use diskmap_core::procinfo::{Kind, ProcInfo, ProcessMap, Snapshot, Usage};
use serde_json::{Value, json};

const PYTHON: &str = r#"
import json
import ntpath
import posixpath
import sys
from types import SimpleNamespace

sys.path.insert(0, sys.argv[1])
from backend import procinfo
from backend.i18n import T, loc, use

def path_value(value):
    if isinstance(value, str):
        return value
    # JSON 本身无法让 serde_json 保存孤立代理码点，输入按 UTF-16 码元传递。
    return b''.join(unit.to_bytes(2, 'little') for unit in value['utf16']).decode('utf-16-le', 'surrogatepass')

def state(info, scenario):
    languages = {}
    for lang in ('zh', 'en'):
        with use(lang):
            rows = []
            for query in scenario['queries']:
                path = path_value(query)
                rows.append({
                    # WTF-8 保留不成对代理码点，与 Rust 原始 OsStr 字节逐一比较。
                    'path_utf8_hex': path.encode('utf-8', 'surrogatepass').hex(),
                    'summary': info.summary(path),
                    'summary_zero': info.summary(path, 0),
                    'summary_one': info.summary(path, 1),
                    'summary_all': info.summary(path, 100),
                    'details': info.details(path),
                    'details_zero': info.details(path, 0),
                    'details_three': info.details(path, 3),
                    'details_all': info.details(path, 100),
                    'count': len(info.lookup(path) or {}),
                })
            languages[lang] = loc(rows)
    return {
        'files': len(info.files),
        'stamp': info.stamp,
        'dirs': [info.dir_count(path_value(path)) for path in scenario['dirs']],
        'languages': languages,
    }

output = []
for scenario in json.load(sys.stdin):
    # 仅替换 procinfo 模块的局部引用，不修改其他模块共享的 os.path。
    windows = scenario['os'] == 'windows'
    paths = ntpath if windows else posixpath
    procinfo.os = SimpleNamespace(path=paths)
    procinfo.IS_WIN = windows
    procinfo.pkey = paths.normcase if windows else (lambda path: path)
    info = procinfo.ProcInfo()
    stages = [state(info, scenario)]
    raw = {}
    for entry in scenario['files']:
        slot = {}
        for pid, name, kind in entry['processes']:
            comm = T('系统', 'system') if name == '@system' else name
            slot[pid] = (comm, kind)
        raw[path_value(entry['path'])] = slot
    info.install(raw)
    stages.append(state(info, scenario))
    # 第二次安装必须替换旧数据并使 stamp 递增；原来的所有查询现在都为空。
    info.install({})
    stages.append(state(info, scenario))
    output.append(stages)
json.dump(output, sys.stdout, ensure_ascii=False)
"#;

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "diskmap-procinfo-legacy-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn fixture(os: Os) -> Value {
    let path = |tail: &str| {
        if os == Os::Windows {
            format!("C:\\{}", tail.replace('/', "\\"))
        } else {
            format!("/{tail}")
        }
    };
    let shared = path("data/alpha/shared.bin");
    // 同名的进程故意交错登记；同计数程序的先后不能按名字或 PID 重新排序。
    let mut processes = vec![
        json!([91, "zeta", "open"]),
        json!([9, "alpha", "exe"]),
        json!([17, "zeta", "lib"]),
        json!([3, "beta", "exe"]),
        json!([8, "alpha", "lib"]),
        json!([7, "middle", "open"]),
        json!([111, "delta", "exe"]),
        json!([2, "gamma", "lib"]),
        json!([93, "theta", "open"]),
    ];
    // 超过默认的 40 行详情；同名、同类型还必须按 PID 排序。
    for i in (0..45).rev() {
        processes.push(json!([200 + i, format!("worker{:02}", i % 6), "lib"]));
    }
    let solo = path("data/beta/solo.data");
    let unicode = path("data/ΟΣ/İ.DAT");
    let chinese = path("data/alpha/中文.data");
    let swap = path(if os == Os::Windows {
        "pagefile.sys"
    } else {
        "swapfile"
    });
    let mut queries = vec![
        shared.clone(),
        solo.clone(),
        unicode.clone(),
        chinese.clone(),
        swap.clone(),
        path("missing"),
    ];
    let mut dirs = vec![
        path(""),
        path("data"),
        path("data/alpha"),
        path("data/beta"),
        path("data/ΟΣ"),
        path("missing"),
    ];
    if os == Os::Windows {
        queries.push("c:/DATA/ALPHA/SHARED.BIN".into());
        queries.push("c:/data/ος/i\u{307}.dat".into());
        dirs.push("c:/DATA/ALPHA".into());
        dirs.push("c:/".into());
    }
    let mut fixture = json!({
        "os": os.key(),
        "queries": queries,
        "dirs": dirs,
        "files": [
            {"path": shared, "processes": processes},
            {"path": solo, "processes": [[51, "single", "open"]]},
            {"path": unicode, "processes": [[701, "unicode", "open"]]},
            {"path": chinese, "processes": [[601, "同名程序", "open"], [600, "同名程序", "exe"]]},
            {"path": swap, "processes": [
                [777, "zeta", "open"], [12, "aaa", "exe"],
                [0, "@system", "swap"], [5, "zzz", "lib"],
            ]},
        ],
    });
    if os == Os::Windows {
        // 孤立高/低代理码点、补充平面字符及替换字符必须是四个不同的路径。
        for (number, units) in [
            vec![0xd800],
            vec![0xdc80],
            vec![0xd83e, 0xdd80],
            vec![0xfffd],
        ]
        .into_iter()
        .enumerate()
        {
            let raw = |tail: &str| {
                let utf16: Vec<_> = "C:\\data\\raw-"
                    .encode_utf16()
                    .chain(units.iter().copied())
                    .chain(tail.encode_utf16())
                    .collect();
                json!({"utf16": utf16})
            };
            let path = raw("\\item.dat");
            fixture["queries"]
                .as_array_mut()
                .unwrap()
                .extend([path.clone(), raw("\\ITEM.DAT")]);
            fixture["dirs"].as_array_mut().unwrap().push(raw(""));
            fixture["files"].as_array_mut().unwrap().push(json!({
                "path": path, "processes": [[800 + number, format!("raw-{number}"), "open"]]
            }));
        }
    }
    fixture
}

fn path_value(value: &Value) -> PathBuf {
    if let Some(path) = value.as_str() {
        return path.into();
    }
    let units: Vec<_> = value["utf16"]
        .as_array()
        .unwrap()
        .iter()
        .map(|unit| u16::try_from(unit.as_u64().unwrap()).unwrap())
        .collect();
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        std::ffi::OsString::from_wide(&units).into()
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        let mut bytes = Vec::new();
        for scalar in char::decode_utf16(units) {
            match scalar {
                Ok(character) => {
                    bytes.extend_from_slice(character.encode_utf8(&mut [0; 4]).as_bytes())
                }
                Err(error) => {
                    let unit = error.unpaired_surrogate();
                    bytes.extend_from_slice(&[
                        0xe0 | (unit >> 12) as u8,
                        0x80 | ((unit >> 6) & 0x3f) as u8,
                        0x80 | (unit & 0x3f) as u8,
                    ]);
                }
            }
        }
        std::ffi::OsString::from_vec(bytes).into()
    }
}

fn path_hex(path: &std::path::Path) -> String {
    path.as_os_str()
        .as_encoded_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn installed_records(fixture: &Value) -> Vec<(PathBuf, ProcessMap)> {
    fixture["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| {
            let mut slot = ProcessMap::default();
            for row in entry["processes"].as_array().unwrap() {
                let kind = match row[2].as_str().unwrap() {
                    "exe" => Kind::Exe,
                    "lib" => Kind::Lib,
                    "open" => Kind::Open,
                    "swap" => Kind::Swap,
                    _ => unreachable!(),
                };
                let name = row[1].as_str().unwrap();
                let usage = if name == "@system" {
                    Usage {
                        comm: Text::new("系统", "system"),
                        kind,
                    }
                } else {
                    Usage::new(name, kind)
                };
                slot.insert(u32::try_from(row[0].as_u64().unwrap()).unwrap(), usage);
            }
            (path_value(&entry["path"]), slot)
        })
        .collect()
}

fn state(snapshot: &Snapshot, fixture: &Value) -> Value {
    let mut languages = serde_json::Map::new();
    for lang in [Lang::Zh, Lang::En] {
        let _guard = i18n::scope(lang);
        let rows: Vec<_> = fixture["queries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|path| {
                let path = path_value(path);
                json!({
                    "path_utf8_hex": path_hex(&path),
                    "summary": snapshot.summary(&path),
                    "summary_zero": snapshot.summary_with_limit(&path, 0),
                    "summary_one": snapshot.summary_with_limit(&path, 1),
                    "summary_all": snapshot.summary_with_limit(&path, 100),
                    "details": snapshot.details(&path),
                    "details_zero": snapshot.details_with_limit(&path, 0),
                    "details_three": snapshot.details_with_limit(&path, 3),
                    "details_all": snapshot.details_with_limit(&path, 100),
                    "count": snapshot.lookup(&path).map_or(0, ProcessMap::len),
                })
            })
            .collect();
        languages.insert(lang.code().to_owned(), json!(rows));
    }
    let dirs: Vec<_> = fixture["dirs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|path| snapshot.dir_count(path_value(path)))
        .collect();
    json!({
        "files": snapshot.file_count(),
        "stamp": snapshot.stamp,
        "dirs": dirs,
        "languages": languages,
    })
}

/// Windows 的应用执行别名可能存在但返回 9009，不能仅凭进程能否启动判断。
fn python_command() -> Option<Command> {
    let mut failures = Vec::new();
    for (program, prefix) in [
        ("python3", &[][..]),
        ("python", &[][..]),
        ("py", &["-3"][..]),
    ] {
        let mut command = Command::new(program);
        command
            .args(prefix)
            .env("PYTHONUTF8", "1")
            .env("PYTHONIOENCODING", "utf-8");
        command.args(["-B", "-c", "import sys; print('diskmap-python3:' + sys.version.split()[0] if sys.version_info.major == 3 else 'unsupported')"]);
        let probe = command_output(&mut command, Duration::from_secs(10));
        match probe {
            Ok(output)
                if output.status.success() && output.stdout.starts_with(b"diskmap-python3:") =>
            {
                eprintln!(
                    "procinfo 旧版对照：{}（stdin/stdout UTF-8）",
                    std::str::from_utf8(&output.stdout)
                        .expect("Python 探测应输出 UTF-8")
                        .trim()
                );
            }
            Ok(output) => {
                failures.push(format!(
                    "{program} {}: status={}, stderr={}",
                    prefix.join(" "),
                    output.status,
                    utf8_diagnostic(&output.stderr).trim()
                ));
                continue;
            }
            Err(error) => {
                failures.push(format!("{program} {}: {error}", prefix.join(" ")));
                continue;
            }
        }
        let mut command = Command::new(program);
        command
            .args(prefix)
            .env("PYTHONUTF8", "1")
            .env("PYTHONIOENCODING", "utf-8");
        return Some(command);
    }
    eprintln!(
        "没有可用 Python 3，跳过 procinfo 旧版对比 / No usable Python 3; skipping legacy comparison: {}",
        failures.join("; ")
    );
    None
}

fn utf8_diagnostic(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap_or_else(|error| {
        format!(
            "非 UTF-8 诊断：{error}；原始字节：{:02x?}",
            error.as_bytes()
        )
    })
}

#[test]
fn python_windows_path_keys_match_rust_for_every_bmp_unit_and_invalid_bytes() {
    use diskmap_core::platform::path::path_key;

    let Some(mut python) = python_command() else {
        return;
    };
    let mut bmp = Vec::new();
    for unit in 0..=u16::MAX {
        // 按单个码元编码，包含孤立代理项；NUL 分隔避免邻接字节改变解码边界。
        if let Some(character) = char::from_u32(u32::from(unit)) {
            bmp.extend_from_slice(character.encode_utf8(&mut [0; 4]).as_bytes());
        } else {
            bmp.extend_from_slice(&[
                0xe0 | (unit >> 12) as u8,
                0x80 | ((unit >> 6) & 63) as u8,
                0x80 | (unit & 63) as u8,
            ]);
        }
        bmp.push(0);
    }
    let mut cases = vec![bmp];
    cases.extend(
        ["c:/Σ/σ/ς/ß/ẞ/İ/ı/i/ﬀ/FF/Ａ/ａ/Ꭰ/ꭰ", "C:/𐐀/𐐨/🦀/.././tail//"]
            .map(|case| case.as_bytes().to_vec()),
    );
    cases.push(vec![0xff, 0xc0, 0xaf, 0xe0, 0x80, 0xaf, b'a', 0xed, 0xa0]);
    // 覆盖任意两字节组合和截断 BMP 编码，防止 Python 解码器偷偷修复输入。
    let mut invalid = Vec::new();
    for first in 0..=u8::MAX {
        for second in 0..=u8::MAX {
            invalid.extend_from_slice(&[first, second, 0]);
        }
    }
    cases.push(invalid);
    let hex = |bytes: &[u8]| -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() };
    let input: Vec<_> = cases.iter().map(|case| hex(case)).collect();
    let expected: Vec<_> = cases
        .iter()
        .map(|case| hex(&path_key(Os::Windows, case)))
        .collect();
    python
        .arg("-B")
        .arg(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/compare_support/windows_paths.py"),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = ChildGuard::spawn(&mut python)
        .unwrap()
        .wait_with_input(serde_json::to_vec(&input).unwrap(), Duration::from_secs(60))
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        utf8_diagnostic(&output.stderr)
    );
    let actual: Vec<String> = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(actual.len(), expected.len());
    for (number, (actual, expected)) in actual.iter().zip(expected).enumerate() {
        assert!(
            actual == &expected,
            "Python/Rust T15 键不一致，输入组 {number}"
        );
    }
}

#[test]
fn windows_source_classification_keeps_image_misses_visible() {
    let Some(mut python) = python_command() else {
        return;
    };
    python
        .arg("-B")
        .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/procinfo_compare.py"))
        .arg("--selftest");
    let output = command_output(&mut python, Duration::from_secs(30)).unwrap();
    assert!(
        output.status.success(),
        "{}",
        utf8_diagnostic(&output.stderr)
    );
}

#[test]
fn summaries_details_and_parent_counts_match_python_on_three_platforms() {
    let systems = [Os::Linux, Os::Macos, Os::Windows];
    let fixtures: Vec<_> = systems.into_iter().map(fixture).collect();
    let temp = TempDir::new();
    let legacy = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../legacy");
    let Some(mut python) = python_command() else {
        return;
    };
    python
        .args(["-B", "-c", PYTHON])
        .arg(legacy)
        .env("DISKMAP_CACHE_DIR", temp.0.join("cache"))
        .env("DISKMAP_CONFIG_DIR", temp.0.join("config"))
        .env("DISKMAP_LANG", "en")
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = ChildGuard::spawn(&mut python)
        .unwrap_or_else(|error| panic!("无法启动已探测可用的 Python 旧版对比：{error}"));
    let output = child
        .wait_with_input(
            serde_json::to_vec(&fixtures).unwrap(),
            Duration::from_secs(60),
        )
        .unwrap();
    assert!(
        output.status.success(),
        "Python 对比失败：{}",
        utf8_diagnostic(&output.stderr)
    );
    let stdout = std::str::from_utf8(&output.stdout).expect("Python 对照必须输出 UTF-8");
    let expected: Vec<Vec<Value>> = serde_json::from_str(stdout).unwrap();
    assert_eq!(expected.len(), systems.len());
    for ((os, fixture), expected) in systems.into_iter().zip(fixtures).zip(expected) {
        let info = ProcInfo::with_os(os);
        let mut actual = vec![state(&info.snapshot(), &fixture)];
        info.install(installed_records(&fixture));
        actual.push(state(&info.snapshot(), &fixture));
        info.install([]);
        actual.push(state(&info.snapshot(), &fixture));
        let empty = actual[0].clone();
        for (stage, (actual, mut expected)) in actual.into_iter().zip(expected).enumerate() {
            if os == Os::Windows && stage == 1 {
                // Linux 上 ntpath 的 Python fallback 会展开 İ；真实 Windows 的
                // normcase 使用系统折叠，可能不展开。只认可这个查询的 0/1 差异。
                let query = fixture["queries"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .position(|query| query.as_str() == Some("c:/data/ος/i\u{307}.dat"))
                    .unwrap();
                for lang in ["zh", "en"] {
                    let old_count = expected["languages"][lang][query]["count"]
                        .as_u64()
                        .unwrap();
                    assert!(old_count <= 1, "旧版 dotted-I 查询计数异常：{old_count}");
                    assert_eq!(actual["languages"][lang][query]["count"], 0);
                    if old_count == 1 {
                        expected["languages"][lang][query] =
                            empty["languages"][lang][query].clone();
                    }
                }
            }
            assert_eq!(actual, expected, "系统 {}，安装阶段 {stage}", os.key());
        }
    }
}
