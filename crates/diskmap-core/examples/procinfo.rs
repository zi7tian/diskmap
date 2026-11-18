//! 进程采集测速及旧版对照入口；--interactive 供对照脚本让两个采集进程同时存活。
use std::io::{self, BufRead, Write};
use std::time::Duration;

use diskmap_core::i18n::Text;
use diskmap_core::procinfo::{DEFAULT_BUDGET, ProcInfo};
use serde_json::{Value, json};

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
fn run() -> Result<(), Text> {
    let mut repeat = 1;
    let mut budget = DEFAULT_BUDGET;
    let mut files = false;
    let mut interactive = false;
    let mut diagnose_pid = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--repeat" => {
                repeat = args
                    .next()
                    .and_then(|s| s.parse::<usize>().ok())
                    .filter(|&n| n > 0)
                    .ok_or_else(invalid_args)?
            }
            "--budget" => {
                budget = args
                    .next()
                    .and_then(|s| s.parse::<f64>().ok())
                    .and_then(|n| Duration::try_from_secs_f64(n).ok())
                    .ok_or_else(invalid_args)?
            }
            "--files" => files = true,
            "--interactive" => interactive = true,
            "--diagnose-pid" => {
                diagnose_pid = Some(
                    args.next()
                        .and_then(|s| s.parse::<u32>().ok())
                        .filter(|&pid| pid > 0)
                        .ok_or_else(invalid_args)?,
                );
            }
            "--help" | "-h" => {
                println!(
                    "{}",
                    Text::new(
                        "用法：procinfo [--repeat N] [--budget 秒] [--files] [--interactive] [--diagnose-pid PID]\n输出 JSON；--files 包含路径和 PID。交互模式每读一行刷新一次，quit 退出。Windows 专用的 --diagnose-pid 只读比较模块表与映射路径，不能与 --files 或 --interactive 同用。",
                        "Usage: procinfo [--repeat N] [--budget seconds] [--files] [--interactive] [--diagnose-pid PID]\nOutputs JSON; --files includes paths and PIDs. Interactive mode refreshes on each input line; quit exits. Windows-only --diagnose-pid compares loader and mapped paths read-only; it cannot be combined with --files or --interactive."
                    )
                );
                return Ok(());
            }
            _ => return Err(invalid_args()),
        }
    }
    if let Some(pid) = diagnose_pid {
        if files || interactive {
            return Err(invalid_args());
        }
        for _ in 0..repeat {
            diagnose(pid, budget)?;
        }
        return Ok(());
    }
    let p = ProcInfo::new();
    if interactive {
        output(&json!({"ready":true,"pid":std::process::id()}))?;
        for line in io::stdin().lock().lines() {
            let line = line.map_err(|e| io_error(&e))?;
            if line.trim() == "quit" {
                break;
            }
            measure(&p, budget, files)?;
        }
    } else {
        for _ in 0..repeat {
            measure(&p, budget, files)?;
        }
    }
    Ok(())
}
#[cfg(windows)]
fn diagnose(pid: u32, budget: Duration) -> Result<(), Text> {
    output(&diskmap_core::procinfo::diagnose_windows_process(
        pid, budget,
    ))
}
#[cfg(not(windows))]
fn diagnose(_pid: u32, _budget: Duration) -> Result<(), Text> {
    Err(Text::new(
        "--diagnose-pid 只支持 Windows",
        "--diagnose-pid is only supported on Windows",
    ))
}
fn invalid_args() -> Text {
    Text::new(
        "参数错误，请使用 --help 查看用法",
        "invalid arguments; use --help for usage",
    )
}
fn io_error(error: &dyn std::fmt::Display) -> Text {
    Text::owned(
        format!("读写示例输出失败：{error}"),
        format!("example I/O failed: {error}"),
    )
}
fn output(value: &Value) -> Result<(), Text> {
    let mut stdout = io::stdout().lock();
    serde_json::to_writer(&mut stdout, value).map_err(|e| io_error(&e))?;
    writeln!(stdout)
        .and_then(|()| stdout.flush())
        .map_err(|e| io_error(&e))
}
fn measure(p: &ProcInfo, budget: Duration, files: bool) -> Result<(), Text> {
    let started = p.refresh_with_budget(budget);
    let s = p.snapshot();
    let mut value = json!({"started":started,"nprocs":s.nprocs,"visible":s.visible,"files":s.file_count(),"time":s.time,"stamp":s.stamp,"error":s.error,"timed_out":s.timed_out});
    if files {
        let mut rows:Vec<_> = s.files().map(|(path,slot)| {
            let hex = path.as_os_str().as_encoded_bytes().iter().map(|b|format!("{b:02x}")).collect::<String>();
            json!({"path":path.to_string_lossy(),"path_hex":hex,"processes":slot.iter().map(|(pid,u)|json!({"pid":pid,"comm":u.comm,"kind":u.kind.key()})).collect::<Vec<_>>()})
        }).collect();
        rows.sort_by(|a, b| a["path_hex"].as_str().cmp(&b["path_hex"].as_str()));
        value["paths"] = Value::Array(rows);
    }
    output(&value)
}
