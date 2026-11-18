//! 仅入口使用的平台适配，不在 MCP 管道上接管控制台。

use std::ffi::OsString;

#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub use windows::attach_parent_console;

#[cfg(not(windows))]
pub struct ParentConsole;

#[cfg(not(windows))]
pub fn attach_parent_console(_args: &[OsString]) -> ParentConsole {
    ParentConsole
}

pub fn configure_environment() {
    #[cfg(target_os = "linux")]
    if default_dmabuf_value(std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").as_deref()).is_some()
    {
        // SAFETY: 只在 main 的第一步调用，此时尚未创建线程或初始化 GTK/WebKit；
        // 设置的是进程环境中的系统库选项，用户已提供的值（包括空串）保持不变。
        unsafe { std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1") };
    }
}

#[cfg(any(target_os = "linux", test))]
fn default_dmabuf_value(existing: Option<&std::ffi::OsStr>) -> Option<&'static str> {
    existing.is_none().then_some("1")
}

#[cfg(any(windows, test))]
fn should_attach(args: &[OsString]) -> bool {
    args.get(1).is_some_and(|arg| arg != "mcp")
}

#[cfg(any(windows, test))]
fn write_console_separator(
    output: &mut impl std::io::Write,
    is_console: bool,
) -> std::io::Result<()> {
    if is_console {
        output.write_all(b"\n")?;
        output.flush()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_separator_only_goes_to_a_console() {
        for is_console in [false, true] {
            let mut output = Vec::new();
            write_console_separator(&mut output, is_console).unwrap();
            assert_eq!(
                output,
                if is_console {
                    b"\n".as_slice()
                } else {
                    b"".as_slice()
                }
            );
        }
    }

    #[test]
    fn keeps_user_renderer_setting_and_mcp_stdio() {
        assert_eq!(default_dmabuf_value(None), Some("1"));
        for value in ["0", "1", ""] {
            assert_eq!(default_dmabuf_value(Some(value.as_ref())), None);
        }
        for words in [
            vec!["DiskMap"],
            vec!["DiskMap", "mcp"],
            vec!["DiskMap", "mcp", "--help"],
        ] {
            assert!(!should_attach(
                &words.into_iter().map(OsString::from).collect::<Vec<_>>()
            ));
        }
        for argument in ["analyze", "--help", "--version", "--selftest", "--no-open"] {
            assert!(should_attach(&["DiskMap".into(), argument.into()]));
        }
    }
}
