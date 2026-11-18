//! 分析报告、浏览器服务和只读扫描统计。
use crate::{args::Options, report_error, write_stdout};
use diskmap_core::analysis::{self, Snapshot, Source};
use diskmap_core::app::App;
use diskmap_core::classify::Classifier;
use diskmap_core::i18n::Text;
use diskmap_core::scan::{Scan, ScanMethod, ScanOptions, ScanState};
use diskmap_core::{platform, procinfo, server};
use serde_json::json;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

fn root(options: &Options) -> PathBuf {
    match options.path.as_deref() {
        Some(p) if !p.as_os_str().is_empty() => match p.to_str() {
            Some(p) => platform::norm_path(p),
            None => std::path::absolute(p).unwrap_or_else(|_| p.to_owned()),
        },
        _ => platform::system_root(),
    }
}

fn invalid_root(root: &Path) -> i32 {
    eprintln!(
        "{}",
        Text::owned(
            format!("文件夹不存在：{}", root.display()),
            format!("Folder does not exist: {}", root.display())
        )
    );
    2
}

// 报告使用 App 的分类组件/进程快照，扫描直接接收 PathBuf，避免有损转换本机文件名。
struct ReportSource {
    app: App,
    scan: Scan,
    retained: Arc<procinfo::Snapshot>,
}
impl Source for ReportSource {
    fn snapshot(&self) -> Snapshot {
        let mut snapshot = self.app.snapshot();
        snapshot.scan = Some(self.scan.clone().into());
        snapshot.retained_processes = self.retained.clone();
        snapshot
    }
    fn request_show(&self, path: &Path, note: &str, now: f64) {
        self.app.request_show(path, note, now);
    }
}

pub(crate) fn analyze(options: &Options) -> i32 {
    let root = root(options);
    if !root.is_dir() {
        return invalid_root(&root);
    }
    let app = App::new();
    app.wait_ready();
    let classifier = Arc::new(Classifier::new(
        Some(app.pkgdb()),
        &procinfo::read_swapfiles(),
    ));
    app.procs().refresh_async();
    let retained = app.procs().snapshot();
    let mut settings = ScanOptions::new(root);
    settings.apparent = options.apparent;
    settings.cross_fs = !options.same_fs;
    let scan = Scan::start(settings, classifier);
    let mut last = None::<Instant>;
    while scan.wait_timeout(Duration::from_millis(200)) == ScanState::Scanning {
        if !options.quiet && last.is_none_or(|t| t.elapsed() >= Duration::from_secs(1)) {
            last = Some(Instant::now());
            let p = scan.progress();
            eprint!("\r{}", scan_progress(p.method, p.records, p.files, p.bytes));
        }
    }
    let deadline = Instant::now() + Duration::from_secs(20);
    while (app.procs().refreshing() || app.procs().snapshot().time == 0.0)
        && Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(100));
    }
    let p = scan.progress();
    if !options.quiet {
        let count = grouped(p.files);
        let seconds = p.elapsed.as_secs_f64();
        eprintln!(
            "\r{}                    ",
            Text::owned(
                format!("扫描完成：{count} 个文件，用时 {seconds:.1} 秒"),
                format!("Scan complete: {count} files in {seconds:.1} s")
            )
        );
    }
    if p.state == ScanState::Error {
        app.shutdown();
        let error = p.error.map(|e| e.to_string()).unwrap_or_default();
        eprintln!(
            "{}",
            Text::owned(
                format!("扫描出错：{error}"),
                format!("Scan failed: {error}")
            )
        );
        return 1;
    }
    let source = ReportSource {
        app,
        scan,
        retained,
    };
    let text = if options.json {
        analysis::report_json(&source, None, options.top)
            .map_err(|e| e.to_string())
            .and_then(|v| {
                serde_json::to_string_pretty(&v)
                    .map(|s| s + "\n")
                    .map_err(|e| e.to_string())
            })
    } else {
        analysis::report_markdown(&source, None, options.top).map_err(|e| e.to_string())
    };
    source.app.shutdown();
    match text.and_then(|text| {
        if let Some(path) = &options.output {
            std::fs::write(path, text).map_err(|e| e.to_string())?;
            if !options.quiet {
                eprintln!(
                    "{}",
                    Text::owned(
                        format!("报告已写入 {}", path.display()),
                        format!("Report written to {}", path.display())
                    )
                );
            }
            Ok(())
        } else {
            write_stdout(&text).map_err(|e| e.to_string())
        }
    }) {
        Ok(()) => 0,
        Err(error) => report_error(error),
    }
}

fn scan_progress(method: ScanMethod, records: u64, files: u64, bytes: u64) -> Text {
    // MFT 在完整建树后才发布文件和字节数，读取期间只能显示已处理的记录数。
    if method == ScanMethod::Mft {
        let count = grouped(records);
        let unit = diskmap_core::i18n::plural(records, "record", None);
        return Text::owned(
            format!("正在读取 MFT，已处理 {count} 条记录…"),
            format!("Reading MFT, {count} {unit} processed…"),
        );
    }
    let count = grouped(files);
    let size = analysis::human(bytes);
    Text::owned(
        format!("正在扫描 {count} 个文件，{size}…"),
        format!("Scanning {count} files, {size}…"),
    )
}

fn grouped(count: u64) -> String {
    let digits = count.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub(crate) fn scan(options: &Options) -> i32 {
    let root = root(options);
    if !root.is_dir() {
        return invalid_root(&root);
    }
    let mut settings = ScanOptions::new(root);
    settings.apparent = options.apparent;
    settings.cross_fs = !options.same_fs;
    settings.method = options.method;
    settings.threads = options.threads;
    let scan = Scan::start(settings, Arc::new(Classifier::new(None, &[])));
    scan.wait();
    let p = scan.progress();
    let peak = peak_rss_bytes();
    let stats = json!({ "root": p.root.to_string_lossy(), "state": p.state.key(), "files": p.files,
        "dirs": p.dirs, "bytes": p.bytes, "elapsed_seconds": p.elapsed.as_secs_f64(),
        "method": p.method.key(), "fallback_reason": p.fallback_reason, "peak_rss_bytes": peak,
        "threads": p.threads, "apparent": p.apparent, "cross_fs": p.cross_fs,
        "denied": p.denied, "records": p.records, "anomalies": p.anomalies, "error": p.error });
    let output = if options.json {
        serde_json::to_string_pretty(&stats)
            .map(|s| s + "\n")
            .map_err(|e| e.to_string())
    } else {
        let elapsed = p.elapsed.as_secs_f64();
        let method = p.method.key();
        let fallback = p
            .fallback_reason
            .as_ref()
            .map(|t| t.to_string())
            .unwrap_or_else(|| Text::new("无", "none").to_string());
        let memory = peak
            .map(|n| format!("{n} B"))
            .unwrap_or_else(|| Text::new("不可用", "unavailable").to_string());
        let mut text = Text::owned(
            format!("文件：{}；目录：{}；字节：{}；用时：{elapsed:.6} 秒\n扫描方式：{method}；回退原因：{fallback}；峰值内存：{memory}\n", p.files, p.dirs, p.bytes),
            format!("Files: {}; folders: {}; bytes: {}; elapsed: {elapsed:.6} s\nMethod: {method}; fallback: {fallback}; peak memory: {memory}\n", p.files, p.dirs, p.bytes)).to_string();
        if let Some(error) = &p.error {
            text.push_str(&format!("{error}\n"));
        }
        Ok(text)
    };
    if let Err(error) = output.and_then(|s| write_stdout(&s).map_err(|e| e.to_string())) {
        return report_error(error);
    }
    i32::from(p.state != ScanState::Done)
}

static STOP: AtomicBool = AtomicBool::new(false);
static SIGNAL: OnceLock<Result<(), String>> = OnceLock::new();

#[derive(Clone, Copy, PartialEq, Eq)]
enum ServeMode {
    Browser,
    NoOpen,
    RootNoOpen,
}

impl ServeMode {
    fn select(no_open: bool, os: platform::Os, admin: bool) -> Self {
        if no_open {
            Self::NoOpen
        } else if os != platform::Os::Windows && admin {
            Self::RootNoOpen
        } else {
            Self::Browser
        }
    }

    fn page_closed(self, app: &App, elapsed: Duration) -> bool {
        self == Self::Browser && app.ui_closed(elapsed)
    }
}

fn prepare_ui_server(options: &Options) -> Result<(server::ServeOptions, Option<PathBuf>), i32> {
    let port = match options.port {
        Some(port) => port,
        None => match std::env::var("DISKMAP_PORT").ok().filter(|v| !v.is_empty()) {
            None => 0,
            Some(v) => match v.parse() {
                Ok(port) => port,
                Err(_) => {
                    eprintln!(
                        "{}",
                        Text::new(
                            "DISKMAP_PORT 必须是 0–65535 的整数",
                            "DISKMAP_PORT must be an integer from 0 to 65535"
                        )
                    );
                    return Err(2);
                }
            },
        },
    };
    let path = options.path.as_ref().map(|_| root(options));
    if let Some(path) = &path {
        if !path.is_dir() {
            return Err(invalid_root(path));
        }
        if path.to_str().is_none() {
            eprintln!(
                "{}",
                Text::new(
                    "浏览器接口的扫描路径需要有效的 Unicode",
                    "The browser API requires a valid Unicode scan path"
                )
            );
            return Err(2);
        }
    }
    let settings = server::ServeOptions {
        port,
        token: options
            .token
            .clone()
            .or_else(|| std::env::var("DISKMAP_TOKEN").ok()),
        ..Default::default()
    };
    Ok((settings, path))
}

fn start_ui_server(
    options: &Options,
    settings: server::ServeOptions,
    path: Option<PathBuf>,
) -> Result<server::ServerHandle, i32> {
    let app = Arc::new(App::new());
    let mut server = match server::serve_with_app(settings, app.clone()) {
        Ok(server) => server,
        Err(error) => return Err(report_error(error)),
    };
    if let Some(path) = path {
        let text = path.to_str().expect("路径已校验");
        if let Err(error) = app.start_scan(text, options.apparent, !options.same_fs) {
            eprintln!("{error}");
            return Err(2);
        }
        server.url.push_str("&view=");
        server.url.push_str(&url_component(text));
    }
    Ok(server)
}

pub(crate) fn start_desktop_server(options: &Options) -> Result<server::ServerHandle, i32> {
    let (settings, path) = prepare_ui_server(options)?;
    start_ui_server(options, settings, path)
}

pub(crate) fn serve(options: &Options) -> i32 {
    serve_with_announcement(options, |url| format!("{url}\n"))
}

pub(crate) fn serve_desktop(options: &Options) -> i32 {
    serve_with_announcement(options, crate::desktop::startup_message)
}

fn serve_with_announcement(options: &Options, announcement: fn(&str) -> String) -> i32 {
    let (settings, path) = match prepare_ui_server(options) {
        Ok(prepared) => prepared,
        Err(code) => return code,
    };
    STOP.store(false, Ordering::Release);
    if let Err(error) = SIGNAL.get_or_init(|| {
        ctrlc::set_handler(|| STOP.store(true, Ordering::Release)).map_err(|e| e.to_string())
    }) {
        return report_error(error);
    }
    let mut server = match start_ui_server(options, settings, path) {
        Ok(server) => server,
        Err(code) => return code,
    };
    if let Err(error) = write_stdout(&announcement(&server.url)) {
        return report_error(error);
    }
    if options.verbose {
        eprintln!(
            "{}",
            Text::owned(
                format!("本地服务已启动，端口 {}；按 Ctrl+C 退出。", server.port),
                format!(
                    "Local server started on port {}; press Ctrl+C to quit.",
                    server.port
                )
            )
        );
    }
    let code = wait_for_ui(&server, options.no_open, || STOP.load(Ordering::Acquire));
    server.shutdown();
    code
}

pub(crate) fn wait_for_ui(
    server: &server::ServerHandle,
    no_open: bool,
    stopped: impl Fn() -> bool,
) -> i32 {
    let mut mode = ServeMode::select(no_open, platform::Os::current(), platform::is_admin());
    if mode == ServeMode::RootNoOpen {
        let notice = Text::new(
            "以 root 运行：为安全起见不自动打开窗口，请在普通用户的浏览器中打开上面的链接。",
            "Running as root: no window is opened for safety. Open the link above in a browser as a normal user.",
        );
        if let Err(error) = write_stdout(&format!("{notice}\n")) {
            return report_error(error);
        }
    }
    if mode == ServeMode::Browser
        && let Err(error) = open_browser(&server.url)
    {
        eprintln!(
            "{}",
            Text::owned(
                format!("无法打开浏览器：{error}。请手动打开上面的链接。"),
                format!("Could not open the browser: {error}. Open the URL above manually."),
            )
        );
        mode = ServeMode::NoOpen;
    }
    if mode != ServeMode::Browser {
        let notice = Text::new("按 Ctrl+C 退出。", "Press Ctrl+C to quit.");
        if let Err(error) = write_stdout(&format!("{notice}\n")) {
            return report_error(error);
        }
    }
    let started = Instant::now();
    while !stopped() && !mode.page_closed(&server.app, started.elapsed()) {
        std::thread::sleep(Duration::from_millis(200));
    }
    0
}

fn url_component(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            out.push(byte as char);
        } else {
            use std::fmt::Write;
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

#[cfg(not(windows))]
fn open_browser(url: &str) -> io::Result<()> {
    let command = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let mut child = std::process::Command::new(command)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    // 某些桌面开启程序直到浏览器退出才返回，后台回收它，不阻塞服务生命周期。
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(windows)]
fn open_browser(url: &str) -> io::Result<()> {
    use windows_sys::Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL};
    let url = url.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
    let verb = "open\0".encode_utf16().collect::<Vec<_>>();
    // SAFETY: 两个字符串均以 NUL 结尾且在调用期间有效；其余可选参数为空。不经命令解释器传递令牌。
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            url.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    } as isize;
    if result > 32 {
        Ok(())
    } else {
        Err(io::Error::other(format!("ShellExecuteW: {result}")))
    }
}

#[cfg(target_os = "linux")]
fn peak_rss_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status
        .lines()
        .find(|l| l.starts_with("VmHWM:"))?
        .split_whitespace()
        .nth(1)?
        .parse::<u64>()
        .ok()?
        .checked_mul(1024)
}
#[cfg(not(any(target_os = "linux", windows)))]
fn peak_rss_bytes() -> Option<u64> {
    None
}
#[cfg(windows)]
fn peak_rss_bytes() -> Option<u64> {
    use windows_sys::Win32::System::{
        ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS},
        Threading::GetCurrentProcess,
    };
    let mut counters = PROCESS_MEMORY_COUNTERS::default();
    let len = std::mem::size_of_val(&counters) as u32;
    counters.cb = len;
    // SAFETY: 当前进程伪句柄无需关闭；counters 可写，结构长度准确。
    let ok = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, len) };
    (ok != 0).then_some(counters.PeakWorkingSetSize as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use diskmap_core::i18n::Lang;
    use diskmap_core::{pkgdb::PkgDb, platform::Os, procinfo::ProcInfo};

    #[test]
    fn mft_progress_uses_record_counts_in_both_languages() {
        for (records, count, unit) in [
            (0, "0", "records"),
            (1, "1", "record"),
            (1_234_567, "1,234,567", "records"),
            (u64::MAX, "18,446,744,073,709,551,615", "records"),
        ] {
            let progress = scan_progress(ScanMethod::Mft, records, 0, 0);
            assert_eq!(
                progress.get(Lang::Zh),
                format!("正在读取 MFT，已处理 {count} 条记录…")
            );
            assert_eq!(
                progress.get(Lang::En),
                format!("Reading MFT, {count} {unit} processed…")
            );
        }
    }

    #[test]
    fn generic_progress_preserves_existing_output_after_mft_fallback() {
        for method in [ScanMethod::Auto, ScanMethod::Generic] {
            for records in [0, 9_876] {
                for (files, bytes, count, size) in [
                    (0, 0, "0", "0 B"),
                    (1_234_567, 1_048_576, "1,234,567", "1.00 MB"),
                ] {
                    let progress = scan_progress(method, records, files, bytes);
                    assert_eq!(
                        progress.get(Lang::Zh),
                        format!("正在扫描 {count} 个文件，{size}…")
                    );
                    assert_eq!(
                        progress.get(Lang::En),
                        format!("Scanning {count} files, {size}…")
                    );
                }
            }
        }
    }

    #[test]
    fn persistent_modes_ignore_page_timeout_on_all_platforms() {
        let app = App::with_components(
            Arc::new(PkgDb::default()),
            Arc::new(Classifier::new(None, &[])),
            ProcInfo::new(),
        );
        let elapsed = Duration::from_secs(121);
        assert!(app.ui_closed(elapsed));
        for os in [Os::Linux, Os::Macos, Os::Windows] {
            for admin in [false, true] {
                assert!(!ServeMode::select(true, os, admin).page_closed(&app, elapsed));
                let mode = ServeMode::select(false, os, admin);
                assert_eq!(mode.page_closed(&app, elapsed), os == Os::Windows || !admin);
                assert_eq!(mode == ServeMode::RootNoOpen, os != Os::Windows && admin);
            }
        }
    }
}
