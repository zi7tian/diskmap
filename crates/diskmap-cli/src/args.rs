//! 小型参数解析器：保持旧版选项和退出码，不依赖命令行框架。
use diskmap_core::{i18n::Text, scan::ScanMethod};
use std::ffi::OsString;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Command {
    Root,
    Analyze,
    Mcp,
    Serve,
    Scan,
    Selftest,
    Desktop,
}

#[derive(Debug)]
pub(crate) struct Options {
    pub command: Command,
    pub help: bool,
    pub version: bool,
    pub path: Option<PathBuf>,
    pub output: Option<PathBuf>,
    pub json: bool,
    pub apparent: bool,
    pub same_fs: bool,
    pub quiet: bool,
    pub no_attach: bool,
    pub no_open: bool,
    pub verbose: bool,
    pub top: i64,
    pub port: Option<u16>,
    pub token: Option<String>,
    pub method: ScanMethod,
    pub threads: usize,
    pub limit: f64,
    pub lang: Option<String>,
    pub browser: bool,
    pub ui: Option<String>,
}

impl Options {
    fn new(command: Command) -> Self {
        Self {
            command,
            help: false,
            version: false,
            path: None,
            output: None,
            json: false,
            apparent: false,
            same_fs: false,
            quiet: false,
            no_attach: false,
            no_open: false,
            verbose: false,
            top: 15,
            port: None,
            token: None,
            method: ScanMethod::Auto,
            threads: 0,
            limit: 150.0,
            lang: None,
            browser: false,
            ui: None,
        }
    }
}

fn options(command: Command) -> &'static [&'static str] {
    match command {
        Command::Root => &["--help", "--version", "--lang"],
        Command::Desktop => &[
            "--help",
            "--version",
            "--lang",
            "--port",
            "--token",
            "--apparent",
            "--same-fs",
            "--no-open",
            "--verbose",
            "--browser",
            "--ui",
        ],
        Command::Analyze => &[
            "--help",
            "--lang",
            "--json",
            "--top",
            "--output",
            "--apparent",
            "--same-fs",
            "--quiet",
        ],
        Command::Mcp => &["--help", "--lang", "--no-attach"],
        Command::Serve => &[
            "--help",
            "--lang",
            "--port",
            "--token",
            "--apparent",
            "--same-fs",
            "--no-open",
            "--verbose",
        ],
        Command::Scan => &[
            "--help",
            "--lang",
            "--json",
            "--method",
            "--threads",
            "--apparent",
            "--same-fs",
        ],
        Command::Selftest => &["--help", "--lang", "--selftest", "--report", "--limit"],
    }
}

pub(crate) fn parse(argv: &[OsString]) -> Result<Options, Text> {
    let mut start = 2;
    let command = match argv.get(1).and_then(|a| a.to_str()) {
        Some("analyze") => Command::Analyze,
        Some("mcp") => Command::Mcp,
        Some("serve") => Command::Serve,
        Some("scan") => Command::Scan,
        Some("selftest") => Command::Selftest,
        Some("help") => {
            let mut out = Options::new(Command::Root);
            out.help = true;
            return Ok(out);
        }
        _ => {
            start = 1;
            if argv.iter().skip(1).take_while(|a| *a != "--").any(|a| {
                a == "--selftest"
                    || a == "--report"
                    || a.to_str().is_some_and(|a| a.starts_with("--report="))
            }) {
                Command::Selftest
            } else {
                Command::Root
            }
        }
    };
    parse_options(argv, start, command)
}

pub(crate) fn parse_desktop(argv: &[OsString]) -> Result<Options, Text> {
    parse_options(argv, 1, Command::Desktop)
}

fn parse_options(argv: &[OsString], start: usize, command: Command) -> Result<Options, Text> {
    let mut out = Options::new(command);
    let mut args = argv
        .iter()
        .skip(start)
        .cloned()
        .collect::<std::collections::VecDeque<_>>();
    let mut positional = false;
    while let Some(arg) = args.pop_front() {
        if !positional && arg == "--" {
            positional = true;
            continue;
        }
        let text = arg.to_str();
        if !positional && text.is_some_and(|s| s.starts_with('-') && s != "-") {
            let text = text.unwrap();
            let (mut name, mut inline) = text
                .split_once('=')
                .map_or((text, None), |(k, v)| (k, Some(OsString::from(v))));
            name = match name {
                "-h" => "--help",
                "-V" if matches!(command, Command::Root | Command::Desktop) => "--version",
                "-q" => "--quiet",
                "-o" => "--output",
                _ => name,
            };
            if text.starts_with("-o")
                && !text.starts_with("--")
                && text.len() > 2
                && command == Command::Analyze
            {
                name = "--output";
                inline = Some(text.strip_prefix("-o=").unwrap_or(&text[2..]).into());
            } else if text.starts_with("-q")
                && !text.starts_with("--")
                && text.len() > 2
                && command == Command::Analyze
            {
                name = "--quiet";
                inline = None;
                args.push_front(format!("-{}", &text[2..]).into());
            }
            let available = options(command);
            if !available.contains(&name) {
                let matches = available
                    .iter()
                    .filter(|s| s.starts_with(name))
                    .copied()
                    .collect::<Vec<_>>();
                if name.starts_with("--") && matches.len() == 1 {
                    name = matches[0];
                } else {
                    return Err(Text::owned(
                        format!("未知或不明确的选项：{text}"),
                        format!("Unknown or ambiguous option: {text}"),
                    ));
                }
            }
            let valued = matches!(
                name,
                "--lang"
                    | "--top"
                    | "--output"
                    | "--report"
                    | "--port"
                    | "--token"
                    | "--threads"
                    | "--method"
                    | "--limit"
                    | "--ui"
            );
            let value = if valued {
                inline
                    .or_else(|| {
                        let next = args.front()?;
                        let s = next.to_string_lossy();
                        if s.starts_with('-') && s != "-" && s.parse::<f64>().is_err() {
                            return None;
                        }
                        args.pop_front()
                    })
                    .ok_or_else(|| {
                        Text::owned(
                            format!("{name} 需要一个值"),
                            format!("{name} requires a value"),
                        )
                    })?
            } else {
                if inline.is_some() {
                    return Err(Text::owned(
                        format!("{name} 不接受值"),
                        format!("{name} does not take a value"),
                    ));
                }
                OsString::new()
            };
            let invalid = || {
                Text::owned(
                    format!("{name} 的值无效：{}", value.to_string_lossy()),
                    format!("Invalid value for {name}: {}", value.to_string_lossy()),
                )
            };
            macro_rules! number {
                () => {
                    value
                        .to_str()
                        .and_then(|s| s.parse().ok())
                        .ok_or_else(invalid)?
                };
            }
            match name {
                "--help" => {
                    out.help = true;
                    return Ok(out);
                }
                "--version" => {
                    out.version = true;
                    return Ok(out);
                }
                "--selftest" => {}
                "--json" => out.json = true,
                "--apparent" => out.apparent = true,
                "--same-fs" => out.same_fs = true,
                "--quiet" => out.quiet = true,
                "--no-attach" => out.no_attach = true,
                "--no-open" => out.no_open = true,
                "--verbose" => out.verbose = true,
                "--browser" => out.browser = true,
                "--ui" => {
                    if !matches!(
                        value.to_str(),
                        Some("webview" | "gtk" | "chromium" | "browser")
                    ) {
                        return Err(invalid());
                    }
                    out.ui = value.into_string().ok();
                }
                "--output" | "--report" => out.output = Some(value.into()),
                "--lang" => {
                    if value != "en" && value != "zh" {
                        return Err(invalid());
                    }
                    out.lang = value.into_string().ok();
                }
                "--top" => {
                    out.top = value
                        .to_str()
                        .and_then(|s| crate::mcp::parse_integer(s).ok())
                        .ok_or_else(invalid)?
                }
                "--port" => out.port = Some(number!()),
                "--token" => out.token = Some(value.to_str().ok_or_else(invalid)?.to_owned()),
                "--threads" => out.threads = number!(),
                "--limit" => {
                    out.limit = number!();
                    if !out.limit.is_finite() || out.limit <= 0.0 {
                        return Err(invalid());
                    }
                }
                "--method" => {
                    out.method = match value.to_str() {
                        Some("auto") => ScanMethod::Auto,
                        Some("generic") => ScanMethod::Generic,
                        Some("mft") => ScanMethod::Mft,
                        _ => return Err(invalid()),
                    }
                }
                _ => unreachable!(),
            }
        } else if matches!(
            command,
            Command::Analyze | Command::Serve | Command::Scan | Command::Desktop
        ) && out.path.is_none()
        {
            out.path = Some(arg.into());
        } else {
            return Err(Text::owned(
                format!("无法识别的参数：{}", arg.to_string_lossy()),
                format!("Unrecognized argument: {}", arg.to_string_lossy()),
            ));
        }
    }
    Ok(out)
}

pub(crate) fn help(command: Command) -> String {
    let usage = Text::new("用法", "Usage");
    let lang = Text::new(
        "--lang zh|en：输出语言（命令行 > 环境变量 > 设置 > 系统语言）",
        "--lang zh|en: output language (command line > environment > settings > system)",
    );
    let detail = match command {
        Command::Root => Text::new(
            "diskmap-cli <命令> [参数]\n\n  analyze   扫描并输出 Markdown 或 JSON 报告\n  mcp       标准输入输出 MCP 服务，供 AI 调用\n  serve     本地 HTTP 服务，默认用浏览器打开\n  scan      只扫描并输出统计\n  selftest  自检扫描、HTTP 和 MCP\n\n使用 <命令> --help 查看选项；--version 查看版本。",
            "diskmap-cli <command> [options]\n\n  analyze   Scan and print a Markdown or JSON report\n  mcp       MCP server over stdio for AI assistants\n  serve     Local HTTP server, opens a browser by default\n  scan      Scan only and print statistics\n  selftest  Check scanning, HTTP and MCP\n\nUse <command> --help for options; --version for the version.",
        ),
        Command::Desktop => Text::new(
            "DiskMap [路径] [--port N] [--browser] [--no-open] [--apparent] [--same-fs] [--verbose] [--ui webview|gtk|chromium|browser]\n启动后可在窗口里选择文件夹；指定路径会立即扫描。--browser 使用默认浏览器；--no-open 只输出链接，按 Ctrl+C 退出。\n--ui 选择界面方式，优先于 DISKMAP_UI。webview、gtk、chromium 均打开独立窗口。--version 查看版本。\n\nAI 助手 / 脚本接口：DiskMap mcp；DiskMap analyze [路径] [--json]。\nserve、scan、selftest 的选项见 <命令> --help。",
            "DiskMap [path] [--port N] [--browser] [--no-open] [--apparent] [--same-fs] [--verbose] [--ui webview|gtk|chromium|browser]\nChoose a folder in the window, or supply a path to scan immediately. --browser opens the default browser; --no-open only prints a link and runs until Ctrl+C.\n--ui selects the interface and overrides DISKMAP_UI. webview, gtk and chromium all open the same window. --version prints the version.\n\nFor AI assistants / scripts: DiskMap mcp; DiskMap analyze [path] [--json].\nUse <command> --help for serve, scan and selftest options.",
        ),
        Command::Analyze => Text::new(
            "diskmap-cli analyze [路径] [--json] [--top N] [-o 文件] [--apparent] [--same-fs] [-q]\n路径默认系统盘；--top 默认 15；-q/--quiet 关闭进度；-o/--output 写入报告。",
            "diskmap-cli analyze [path] [--json] [--top N] [-o file] [--apparent] [--same-fs] [-q]\nPath defaults to the system drive; --top defaults to 15; -q/--quiet hides progress; -o/--output writes a report.",
        ),
        Command::Mcp => Text::new(
            "diskmap-cli mcp [--no-attach]\n默认连接已打开的窗口；--no-attach 或 DISKMAP_NO_ATTACH 在后台独立扫描。",
            "diskmap-cli mcp [--no-attach]\nConnects to an open window by default; --no-attach or DISKMAP_NO_ATTACH scans independently.",
        ),
        Command::Serve => Text::new(
            "diskmap-cli serve [路径] [--port N] [--token TOKEN] [--apparent] [--same-fs] [--no-open] [--verbose]\n--port 默认自动选择；浏览器模式在页面关闭后退出；--no-open 持续运行直到 Ctrl+C。非 Windows 的 root 用户不自动打开浏览器。",
            "diskmap-cli serve [path] [--port N] [--token TOKEN] [--apparent] [--same-fs] [--no-open] [--verbose]\nThe port is automatic by default; browser mode exits after page closure; --no-open runs until Ctrl+C. Root users on non-Windows systems do not open a browser automatically.",
        ),
        Command::Scan => Text::new(
            "diskmap-cli scan [路径] [--json] [--method auto|generic|mft] [--threads N] [--apparent] [--same-fs]\n路径默认系统盘；--threads 0 自动选择；输出文件、目录、字节、用时、扫描方式、回退原因、峰值内存。",
            "diskmap-cli scan [path] [--json] [--method auto|generic|mft] [--threads N] [--apparent] [--same-fs]\nPath defaults to the system drive; --threads 0 chooses automatically; prints files, folders, bytes, time, method, fallback reason and peak memory.",
        ),
        Command::Selftest => Text::new(
            "diskmap-cli selftest [--report 文件] [--limit 秒]\n兼容 --selftest --report；临时生成数据，默认 150 秒超时。",
            "diskmap-cli selftest [--report file] [--limit seconds]\nAlso accepts --selftest --report; creates temporary data, with a default timeout of 150 seconds.",
        ),
    };
    let counting = Text::new(
        "--apparent 按文件长度统计；--same-fs 不进入其他分区。",
        "--apparent counts file length; --same-fs stays on one file system.",
    );
    format!(
        "DiskMap {}\n{usage}: {detail}\n\n{lang}\n{}",
        diskmap_core::VERSION,
        if matches!(
            command,
            Command::Analyze | Command::Scan | Command::Serve | Command::Desktop
        ) {
            format!("{counting}\n")
        } else {
            String::new()
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn parse_words(words: &[&str]) -> Result<Options, Text> {
        parse(&words.iter().map(OsString::from).collect::<Vec<_>>())
    }
    #[test]
    fn aliases_values_and_terminator() {
        let args = parse_words(&[
            "dm",
            "analyze",
            "--top=-1",
            "-q",
            "-oreport.md",
            "--",
            "-folder",
        ])
        .unwrap();
        assert_eq!(args.top, -1);
        assert!(args.quiet);
        assert_eq!(args.output, Some("report.md".into()));
        assert_eq!(args.path, Some("-folder".into()));
        assert_eq!(
            parse_words(&["dm", "--report=x.json", "--selftest"])
                .unwrap()
                .command,
            Command::Selftest
        );
        assert!(parse_words(&["dm", "analyze", "--app"]).unwrap().apparent);
        assert_eq!(
            parse_words(&["dm", "analyze", "-o=report.md"])
                .unwrap()
                .output,
            Some("report.md".into())
        );
        assert_eq!(
            parse_words(&["dm", "analyze", "-qoreport=2.md"])
                .unwrap()
                .output,
            Some("report=2.md".into())
        );
        assert_eq!(
            parse_words(&["dm", "analyze", "--top=１２"]).unwrap().top,
            12
        );
    }
    #[test]
    fn rejects_invalid_and_unrelated_options() {
        for words in [
            vec!["dm", "mcp", "--json"],
            vec!["dm", "serve", "--port", "65536"],
            vec!["dm", "analyze", "--lang=fr"],
            vec!["dm", "analyze", "--top", "--json"],
            vec!["dm", "scan", "--method=x"],
            vec!["dm", "selftest", "--limit=NaN"],
            vec!["dm", "analyze", "--json=false"],
        ] {
            assert!(parse_words(&words).is_err(), "{words:?}");
        }
    }
}
