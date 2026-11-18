// 发布版使用 Windows 窗口子系统；命令行输出由 platform 模块接管。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Tauri 只负责显示窗口；扫描、HTTP、实例登记和浏览器模式复用 core/cli。

mod platform;
mod window;

use diskmap_cli::desktop::Options;
use diskmap_core::i18n::{self, Text};
use std::ffi::OsString;
use std::io::{self, Write};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

fn main() -> ExitCode {
    // 必须在 Tauri、信号处理和扫描创建任何线程之前设置 WebKit 环境。
    platform::configure_environment();
    let args: Vec<OsString> = std::env::args_os().collect();
    let _console = platform::attach_parent_console(&args);
    let code = run(args);
    let _ = io::stdout().flush();
    let _ = io::stderr().flush();
    ExitCode::from(code as u8)
}

fn run(args: Vec<OsString>) -> i32 {
    if diskmap_cli::is_command(&args) {
        return diskmap_cli::run(args);
    }
    let lang = i18n::resolve(i18n::pre_scan_argv(&args).and_then(|s| s.to_str()));
    i18n::set_default(lang);
    let _scope = i18n::scope(lang);
    let options = match Options::parse(&args) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("{error}");
            return 2;
        }
    };
    if options.help_requested() {
        return write_stdout(&diskmap_cli::desktop::help());
    }
    if options.version_requested() {
        return write_stdout(&format!("DiskMap {}\n", diskmap_core::VERSION));
    }
    if options.uses_browser(
        diskmap_core::platform::Os::current(),
        diskmap_core::platform::is_admin(),
    ) {
        return options.serve();
    }

    let stopped = Arc::new(AtomicBool::new(false));
    let signal = Arc::clone(&stopped);
    if let Err(error) = ctrlc::set_handler(move || signal.store(true, Ordering::Release)) {
        return report_error(error);
    }
    let mut server = match options.start_server() {
        Ok(server) => server,
        Err(code) => return code,
    };
    if write_stdout(&diskmap_cli::desktop::startup_message(&server.url)) != 0 {
        return 1;
    }
    if options.verbose() {
        eprintln!(
            "{}",
            Text::owned(
                format!("本地服务已启动，端口 {}。", server.port),
                format!("Local server started on port {}.", server.port),
            )
        );
    }
    let code = run_ui(
        || window::run(&server.url, Arc::clone(&stopped)),
        || options.wait_in_browser(&server, || stopped.load(Ordering::Acquire)),
        || stopped.load(Ordering::Acquire),
    );
    server.shutdown();
    code
}

fn run_ui(
    open_window: impl FnOnce() -> Result<i32, String>,
    browser: impl FnOnce() -> i32,
    stopped: impl Fn() -> bool,
) -> i32 {
    if stopped() {
        return 0;
    }
    match open_window() {
        Ok(code) => code,
        Err(_) if stopped() => 0,
        Err(error) => {
            eprintln!(
                "{}",
                Text::owned(
                    format!("无法启动窗口，改用默认浏览器：{error}"),
                    format!("Could not start the window; opening the default browser: {error}"),
                )
            );
            browser()
        }
    }
}

fn write_stdout(text: &str) -> i32 {
    let mut out = io::stdout().lock();
    match out.write_all(text.as_bytes()).and_then(|()| out.flush()) {
        Ok(()) => 0,
        Err(error) => report_error(error),
    }
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
    use std::cell::Cell;

    #[test]
    fn failed_webview_falls_back_once_and_normal_close_does_not() {
        let calls = Cell::new(0);
        assert_eq!(
            run_ui(
                || Err("WebView2".into()),
                || {
                    calls.set(calls.get() + 1);
                    0
                },
                || false
            ),
            0
        );
        assert_eq!(calls.get(), 1);
        assert_eq!(
            run_ui(|| Ok(0), || panic!("正常关闭不应打开浏览器"), || false),
            0
        );
        assert_eq!(
            run_ui(
                || panic!("结束信号后不应打开窗口"),
                || panic!("结束信号后不应打开浏览器"),
                || true
            ),
            0
        );
        let checks = Cell::new(0);
        assert_eq!(
            run_ui(
                || Err("GTK".into()),
                || panic!("结束信号后不应回退"),
                || {
                    checks.set(checks.get() + 1);
                    checks.get() > 1
                }
            ),
            0
        );
    }
}
