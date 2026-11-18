//! 桌面外壳复用命令行的参数、服务配置和浏览器生命周期；不依赖 Tauri。

use crate::{args, commands};
use diskmap_core::{i18n::Text, platform, server::ServerHandle};
use std::ffi::OsString;

/// 已解析的旧版桌面参数。子命令应先交给 [`crate::is_command`] 和 [`crate::run`]。
#[derive(Debug)]
pub struct Options {
    inner: args::Options,
}

impl Options {
    /// 参数包含程序名；支持 `--`、长选项缩写和 `--选项=值`，与 CLI 共用解析器。
    pub fn parse(argv: &[OsString]) -> Result<Self, Text> {
        Self::parse_with_ui(argv, std::env::var("DISKMAP_UI").ok().as_deref())
    }

    fn parse_with_ui(argv: &[OsString], default_ui: Option<&str>) -> Result<Self, Text> {
        let mut inner = args::parse_desktop(argv)?;
        if inner.ui.is_none() {
            inner.ui = default_ui
                .filter(|ui| matches!(*ui, "webview" | "gtk" | "chromium" | "browser"))
                .map(str::to_owned);
        }
        Ok(Self { inner })
    }

    pub fn help_requested(&self) -> bool {
        self.inner.help
    }

    pub fn version_requested(&self) -> bool {
        self.inner.version
    }

    pub fn verbose(&self) -> bool {
        self.inner.verbose
    }

    /// Unix 管理员与旧版一样转为只提供链接；其余浏览器选项沿用 `serve`。
    pub fn uses_browser(&self, os: platform::Os, admin: bool) -> bool {
        self.inner.no_open
            || self.inner.browser
            || self.inner.ui.as_deref() == Some("browser")
            || (os != platform::Os::Windows && admin)
    }

    /// 浏览器/无窗口模式复用 `serve` 的生命周期，启动提示采用旧版桌面的双语格式。
    pub fn serve(&self) -> i32 {
        commands::serve_desktop(&self.inner)
    }

    /// 启动窗口使用的服务；返回地址已包含令牌及可选的初始扫描位置。
    /// 句柄析构负责取消扫描、关闭服务和删除本进程登记。
    pub fn start_server(&self) -> Result<ServerHandle, i32> {
        commands::start_desktop_server(&self.inner)
    }

    /// 窗口初始化失败后沿用同一个服务，打开浏览器并等待页面关闭或调用方的结束信号。
    /// 不重复扫描、不更换访问令牌和实例登记。
    pub fn wait_in_browser(&self, server: &ServerHandle, stopped: impl Fn() -> bool) -> i32 {
        commands::wait_for_ui(server, self.inner.no_open, stopped)
    }
}

/// 按当前语言返回桌面入口的帮助；隐藏选项不列出。
pub fn help() -> String {
    args::help(args::Command::Desktop)
}

/// 旧版桌面入口的启动提示，各种窗口及浏览器模式共用。
pub fn startup_message(url: &str) -> String {
    format!(
        "{}\n",
        Text::owned(
            format!("DiskMap 已启动：{url}"),
            format!("DiskMap is running: {url}"),
        )
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(words: &[&str]) -> Result<Options, Text> {
        Options::parse_with_ui(&words.iter().map(OsString::from).collect::<Vec<_>>(), None)
    }

    #[test]
    fn ui_environment_is_a_default_and_unknown_values_are_ignored() {
        for ui in [
            None,
            Some(""),
            Some("unknown"),
            Some("BROWSER"),
            Some("browser "),
            Some("webview"),
            Some("gtk"),
            Some("chromium"),
            Some("browser"),
        ] {
            let options = Options::parse_with_ui(&["DiskMap".into()], ui).unwrap();
            assert_eq!(
                options.uses_browser(platform::Os::Linux, false),
                ui == Some("browser")
            );
            for override_ui in ["webview", "gtk", "chromium", "browser"] {
                let args = ["DiskMap".into(), "--ui".into(), override_ui.into()];
                let options = Options::parse_with_ui(&args, ui).unwrap();
                assert_eq!(options.inner.ui.as_deref(), Some(override_ui));
                assert_eq!(
                    options.uses_browser(platform::Os::Linux, false),
                    override_ui == "browser"
                );
            }
            let args = ["DiskMap".into(), "--ui=unknown".into()];
            assert!(Options::parse_with_ui(&args, ui).is_err());
        }
    }

    #[test]
    fn startup_message_matches_legacy_in_both_languages() {
        let url = "http://127.0.0.1:12345/?t=token&view=%2Ftmp";
        for (lang, prefix) in [
            (diskmap_core::i18n::Lang::Zh, "DiskMap 已启动："),
            (diskmap_core::i18n::Lang::En, "DiskMap is running: "),
        ] {
            let _scope = diskmap_core::i18n::scope(lang);
            assert_eq!(startup_message(url), format!("{prefix}{url}\n"));
        }
    }

    #[test]
    fn legacy_window_options_and_browser_selection() {
        let options = parse(&[
            "DiskMap",
            "/tmp",
            "--port=1234",
            "--token",
            "a b",
            "--apparent",
            "--same-fs",
            "--verbose",
            "--ui",
            "gtk",
            "--lang=en",
        ])
        .unwrap();
        assert_eq!(
            options.inner.path.as_deref(),
            Some(std::path::Path::new("/tmp"))
        );
        assert_eq!(options.inner.port, Some(1234));
        assert_eq!(options.inner.token.as_deref(), Some("a b"));
        assert!(options.inner.apparent && options.inner.same_fs && options.verbose());
        assert_eq!(options.inner.lang.as_deref(), Some("en"));
        assert!(!options.uses_browser(platform::Os::Linux, false));
        assert!(options.uses_browser(platform::Os::Linux, true));
        assert!(!options.uses_browser(platform::Os::Windows, true));
        for flag in ["--browser", "--no-open", "--ui=browser"] {
            assert!(
                parse(&["DiskMap", flag])
                    .unwrap()
                    .uses_browser(platform::Os::Windows, false)
            );
        }
        for ui in ["webview", "gtk", "chromium"] {
            assert!(
                !parse(&["DiskMap", "--ui", ui])
                    .unwrap()
                    .uses_browser(platform::Os::Linux, false)
            );
        }
    }

    #[test]
    fn help_errors_and_literal_paths() {
        assert!(parse(&["DiskMap", "--help"]).unwrap().help_requested());
        assert!(
            parse(&["DiskMap", "--version"])
                .unwrap()
                .version_requested()
        );
        assert_eq!(
            parse(&["DiskMap", "--", "--browser"]).unwrap().inner.path,
            Some("--browser".into())
        );
        for words in [
            vec!["DiskMap", "--ui=unknown"],
            vec!["DiskMap", "--port=65536"],
            vec!["DiskMap", "--browser=false"],
            vec!["DiskMap", "--lang=fr"],
            vec!["DiskMap", "a", "b"],
        ] {
            assert!(parse(&words).is_err(), "{words:?}");
        }
        for lang in [diskmap_core::i18n::Lang::Zh, diskmap_core::i18n::Lang::En] {
            let _scope = diskmap_core::i18n::scope(lang);
            let text = help();
            for word in [
                "--browser",
                "--no-open",
                "--ui",
                "--port",
                "--lang",
                "mcp",
                "analyze",
            ] {
                assert!(text.contains(word));
            }
            assert!(!text.contains("--token") && !text.contains("--report"));
        }
    }
}
