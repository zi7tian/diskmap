//! 命令行和桌面版共用的入口；返回退出码，不结束宿主进程。

mod args;
mod commands;
pub mod desktop;
mod http;
mod mcp;
mod mcp_tools;
mod selftest;

use diskmap_core::i18n::{self, Text};
use std::ffi::OsString;
use std::io::{self, Write};

/// 桌面版据此将子命令以及旧版自检选项交给 [`run`]。
pub fn is_command(args: &[OsString]) -> bool {
    args.get(1)
        .and_then(|a| a.to_str())
        .is_some_and(|a| matches!(a, "analyze" | "mcp" | "serve" | "scan" | "selftest"))
        || args.iter().skip(1).take_while(|a| *a != "--").any(|a| {
            a == "--selftest"
                || a == "--report"
                || a.to_str().is_some_and(|s| s.starts_with("--report="))
        })
}

/// 参数包含程序名，与 `std::env::args_os()` 相同。正常返回 0，运行失败 1，参数错误 2，自检超时 3。
pub fn run(args: Vec<OsString>) -> i32 {
    let lang = i18n::resolve(i18n::pre_scan_argv(&args).and_then(|s| s.to_str()));
    i18n::set_default(lang);
    let _language = i18n::scope(lang);
    let parsed = match args::parse(&args) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("{error}");
            return 2;
        }
    };
    if parsed.help {
        return write_stdout(&args::help(parsed.command)).map_or_else(report_error, |_| 0);
    }
    if parsed.version {
        return write_stdout(&format!("DiskMap {}\n", diskmap_core::VERSION))
            .map_or_else(report_error, |_| 0);
    }
    match parsed.command {
        args::Command::Mcp => {
            // 旧版 MCP 再应用解析后的语言；重复选项及 argparse 的缩写以最后一项为准。
            let lang = i18n::resolve(parsed.lang.as_deref());
            i18n::set_default(lang);
            let _scope = i18n::scope(lang);
            mcp::run(!parsed.no_attach)
        }
        args::Command::Selftest => selftest::run(parsed.output.as_deref(), parsed.limit),
        args::Command::Analyze => commands::analyze(&parsed),
        args::Command::Serve => commands::serve(&parsed),
        args::Command::Scan => commands::scan(&parsed),
        args::Command::Root | args::Command::Desktop => {
            write_stdout(&help()).map_or_else(report_error, |_| 0)
        }
    }
}

/// 按当前语言返回顶层帮助文字。
pub fn help() -> String {
    args::help(args::Command::Root)
}

fn write_stdout(text: &str) -> io::Result<()> {
    let mut out = io::stdout().lock();
    out.write_all(text.as_bytes())?;
    out.flush()
}

fn report_error(error: impl std::fmt::Display) -> i32 {
    eprintln!(
        "{}",
        Text::owned(
            format!("运行失败：{error}"),
            format!("Operation failed: {error}")
        )
    );
    1
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recognises_subcommands_and_legacy_selftest() {
        for words in [
            vec!["DiskMap", "mcp"],
            vec!["DiskMap", "analyze", "/"],
            vec![
                "DiskMap",
                "--lang",
                "en",
                "--selftest",
                "--report",
                "r.json",
            ],
            vec!["DiskMap", "--report=r.json", "--selftest"],
        ] {
            assert!(is_command(
                &words.into_iter().map(OsString::from).collect::<Vec<_>>()
            ));
        }
        for words in [
            vec!["DiskMap"],
            vec!["DiskMap", "/home"],
            vec!["DiskMap", "--lang", "en"],
            vec!["DiskMap", "--", "--selftest"],
        ] {
            assert!(!is_command(
                &words.into_iter().map(OsString::from).collect::<Vec<_>>()
            ));
        }
    }
}
