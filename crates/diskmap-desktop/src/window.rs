//! WebView 创建与事件循环；初始化失败交回入口使用浏览器。

use diskmap_core::i18n::Text;
use std::any::Any;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use tauri::{RunEvent, Url, WebviewUrl, WebviewWindowBuilder, WindowEvent};

pub fn run(url: &str, stopped: Arc<AtomicBool>) -> Result<i32, String> {
    // tao 在 GTK 初始化失败时使用 panic；在边界捕获，保留同一个 HTTP 服务供浏览器使用。
    catch_unwind(AssertUnwindSafe(|| run_inner(url, stopped)))
        .unwrap_or_else(|panic| Err(panic_message(panic)))
}

fn run_inner(url: &str, stopped: Arc<AtomicBool>) -> Result<i32, String> {
    let url = Url::parse(url).map_err(|error| error.to_string())?;
    let allowed = url.clone();
    let app = tauri::Builder::default()
        .build(tauri::generate_context!())
        .map_err(|error| error.to_string())?;
    let mut builder = WebviewWindowBuilder::new(&app, "main", WebviewUrl::External(url))
        .title(Text::new("DiskMap 磁盘地图", "DiskMap").text())
        .inner_size(1440.0, 900.0)
        .min_inner_size(820.0, 560.0)
        .background_color(tauri::window::Color(0x0e, 0x10, 0x14, 255))
        .on_navigation(move |destination| local_navigation(&allowed, destination))
        .on_new_window(|_, _| tauri::webview::NewWindowResponse::Deny);
    if let Some(icon) = app.default_window_icon() {
        builder = builder
            .icon(icon.clone())
            .map_err(|error| error.to_string())?;
    }
    let _window = builder.build().map_err(|error| error.to_string())?;
    let _watcher =
        SignalWatcher::new(app.handle().clone(), stopped).map_err(|error| error.to_string())?;
    // run_return 确保外层可以在事件循环结束后关闭服务，而不是由 Tauri 直接结束进程。
    Ok(app.run_return(|handle, event| {
        if matches!(event, RunEvent::WindowEvent { ref label, event: WindowEvent::CloseRequested { .. } | WindowEvent::Destroyed, .. } if label == "main") {
            handle.exit(0);
        }
    }))
}

fn local_navigation(server: &Url, destination: &Url) -> bool {
    destination.scheme() == "http"
        && destination.host_str() == Some("127.0.0.1")
        && destination.port_or_known_default() == server.port_or_known_default()
        && destination.username().is_empty()
        && destination.password().is_none()
}

fn panic_message(panic: Box<dyn Any + Send>) -> String {
    if let Some(message) = panic.downcast_ref::<String>() {
        message.clone()
    } else if let Some(message) = panic.downcast_ref::<&str>() {
        (*message).to_owned()
    } else {
        Text::new("WebView 初始化失败", "WebView initialization failed").to_string()
    }
}

struct SignalWatcher {
    finished: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl SignalWatcher {
    fn new(handle: tauri::AppHandle, stopped: Arc<AtomicBool>) -> std::io::Result<Self> {
        let finished = Arc::new(AtomicBool::new(false));
        let done = Arc::clone(&finished);
        let thread = thread::Builder::new()
            .name("diskmap-window-signal".into())
            .spawn(move || {
                while !done.load(Ordering::Acquire) {
                    if stopped.load(Ordering::Acquire) {
                        handle.exit(0);
                        break;
                    }
                    thread::sleep(Duration::from_millis(50));
                }
            })?;
        Ok(Self {
            finished,
            thread: Some(thread),
        })
    }
}

impl Drop for SignalWatcher {
    fn drop(&mut self) {
        self.finished.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigation_stays_on_this_service() {
        let server: Url = "http://127.0.0.1:12345/?t=token".parse().unwrap();
        for url in [
            "http://127.0.0.1:12345/?t=token&view=/tmp",
            "http://127.0.0.1:12345/static/app.js",
        ] {
            assert!(local_navigation(&server, &url.parse().unwrap()));
        }
        for url in [
            "https://example.com",
            "http://127.0.0.1:12346/",
            "http://localhost:12345/",
            "https://127.0.0.1:12345/",
            "http://user@127.0.0.1:12345/",
            "file:///tmp/index.html",
            "javascript:alert(1)",
            "about:blank",
        ] {
            assert!(!local_navigation(&server, &url.parse().unwrap()), "{url}");
        }
    }
}
