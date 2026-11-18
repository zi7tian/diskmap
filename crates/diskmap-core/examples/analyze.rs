//! 独立扫描并输出分析报告，不启动服务、不写配置或用户文件。
//! cargo run --release -p diskmap-core --example analyze -- <路径> --json --apparent --lang en

use diskmap_core::analysis::{self, Snapshot, Source};
use diskmap_core::classify::Classifier;
use diskmap_core::i18n::{self, Lang, Text};
use diskmap_core::pkgdb::PkgDb;
use diskmap_core::procinfo::{self, ProcInfo};
use diskmap_core::scan::{Scan, ScanOptions, ScanState};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

struct Offline {
    snapshot: Snapshot,
    command: Mutex<Option<serde_json::Value>>,
}
impl Source for Offline {
    fn snapshot(&self) -> Snapshot {
        self.snapshot.clone()
    }
    fn request_show(&self, path: &Path, note: &str, now: f64) {
        let mut command = self.command.lock().unwrap();
        let id = command.as_ref().and_then(|c| c["id"].as_u64()).unwrap_or(0) + 1;
        *command = Some(
            serde_json::json!({"id":id,"action":"show","path":path.to_string_lossy(),"note":note,"time":now}),
        );
    }
}
fn run() -> Result<(), String> {
    let mut args = std::env::args_os().skip(1);
    let mut path = None;
    let mut json = false;
    let mut apparent = false;
    let mut cross_fs = true;
    let mut top = 15;
    let mut lang = None;
    let mut pkgdb = true;
    let mut processes = true;
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--json") => json = true,
            Some("--apparent") => apparent = true,
            Some("--same-fs") => cross_fs = false,
            Some("--no-pkgdb") => pkgdb = false,
            Some("--no-procs") => processes = false,
            Some("--top") => {
                top = args
                    .next()
                    .and_then(|v| v.to_str().and_then(|s| s.parse().ok()))
                    .ok_or_else(|| {
                        Text::new("--top 需要整数", "--top requires an integer").to_string()
                    })?
            }
            Some("--lang") => {
                lang = Some(
                    args.next()
                        .and_then(|v| v.to_str().and_then(Lang::parse))
                        .ok_or_else(|| {
                            Text::new("--lang 需要 zh 或 en", "--lang requires zh or en")
                                .to_string()
                        })?,
                )
            }
            Some("--help" | "-h") => {
                println!(
                    "{}",
                    Text::new(
                        "用法：analyze <路径> [--json] [--apparent] [--same-fs] [--top N] [--lang zh|en] [--no-pkgdb] [--no-procs]",
                        "Usage: analyze <path> [--json] [--apparent] [--same-fs] [--top N] [--lang zh|en] [--no-pkgdb] [--no-procs]"
                    )
                );
                return Ok(());
            }
            Some(s) if s.starts_with('-') => {
                return Err(
                    Text::owned(format!("未知选项：{s}"), format!("Unknown option: {s}"))
                        .to_string(),
                );
            }
            _ if path.is_none() => path = Some(PathBuf::from(arg)),
            _ => {
                return Err(
                    Text::new("只能指定一个扫描路径", "Specify only one scan path").to_string(),
                );
            }
        }
    }
    let _language = i18n::scope(lang.unwrap_or_else(|| i18n::resolve(None)));
    let path = path.ok_or_else(|| {
        Text::new(
            "请指定扫描路径（--help 查看用法）",
            "Specify a scan path (see --help)",
        )
        .to_string()
    })?;
    let pkgdb = Arc::new(if pkgdb {
        PkgDb::load()
    } else {
        PkgDb::default()
    });
    let proc = ProcInfo::new();
    if processes {
        proc.refresh();
    }
    let swapfiles = if processes {
        procinfo::read_swapfiles()
    } else {
        Vec::new()
    };
    let classifier = Arc::new(Classifier::new(Some(Arc::clone(&pkgdb)), &swapfiles));
    let mut options = ScanOptions::new(path);
    options.apparent = apparent;
    options.cross_fs = cross_fs;
    let scan = Scan::start(options, classifier);
    if scan.wait() == ScanState::Error {
        return Err(scan
            .progress()
            .error
            .map(|e| e.to_string())
            .unwrap_or_else(|| Text::new("扫描失败", "Scan failed").to_string()));
    }
    let mut snapshot = Snapshot::new(proc.snapshot(), pkgdb);
    snapshot.ready = true;
    snapshot.scan = Some(scan.into());
    let source = Offline {
        snapshot,
        command: Mutex::new(None),
    };
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(
                &analysis::report_json(&source, None, top).map_err(|e| e.to_string())?
            )
            .map_err(|e| e.to_string())?
        );
    } else {
        print!(
            "{}",
            analysis::report_markdown(&source, None, top).map_err(|e| e.to_string())?
        );
    }
    Ok(())
}
fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
