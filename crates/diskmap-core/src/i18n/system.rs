//! 系统语言检测；平台探测结果和环境变量均可注入测试。

use crate::platform::Os;

use super::{Lang, norm};

/// 检测系统语言：Windows UI 语言 / macOS AppleLanguages 优先，然后读取 locale 环境变量。
pub fn system_lang() -> Lang {
    system_lang_with(
        Os::current(),
        |key| std::env::var(key).ok(),
        native_language(),
    )
}

/// 注入平台语言与环境变量。`native` 是 Windows / macOS 的原生语言探测结果。
/// Linux 忽略 `native`，环境变量按 `LANGUAGE`、`LC_ALL`、`LC_MESSAGES`、`LANG` 顺序处理。
pub fn system_lang_with(
    os: Os,
    env: impl Fn(&str) -> Option<String>,
    native: Option<Lang>,
) -> Lang {
    if os != Os::Linux
        && let Some(lang) = native
    {
        return lang;
    }
    for key in ["LANGUAGE", "LC_ALL", "LC_MESSAGES", "LANG"] {
        let value = env(key).unwrap_or_default();
        let value = value.split(':').next().unwrap_or_default();
        if !value.is_empty() && value != "C" && value != "POSIX" && !value.starts_with("C.") {
            return if norm(value) == Some(Lang::Zh) {
                Lang::Zh
            } else {
                Lang::En
            };
        }
    }
    Lang::En
}

#[cfg(windows)]
fn native_language() -> Option<Lang> {
    Some(super::windows::language())
}

#[cfg(not(any(windows, target_os = "macos")))]
fn native_language() -> Option<Lang> {
    None
}

#[cfg(target_os = "macos")]
fn native_language() -> Option<Lang> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    let mut child = Command::new("defaults")
        .args(["read", "-g", "AppleLanguages"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    };
    // 并发读管道，避免 defaults 输出过多时阻塞；与旧版一样最多等三秒。
    let reader = std::thread::Builder::new().spawn(move || {
        let mut output = String::new();
        let mut stdout = stdout;
        stdout.read_to_string(&mut output).map(|_| output)
    });
    let reader = match reader {
        Ok(reader) => reader,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
    };
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                return reader
                    .join()
                    .ok()?
                    .ok()
                    .and_then(|text| mac_language(&text));
            }
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return None;
            }
        }
    }
}

#[cfg(any(target_os = "macos", test))]
pub(super) fn mac_language(output: &str) -> Option<Lang> {
    for line in output.lines() {
        let line = line.trim().trim_matches(['"', ',', '(', ')']);
        if !line.is_empty() {
            return Some(norm(line).unwrap_or(Lang::En));
        }
    }
    None
}
