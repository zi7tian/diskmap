//! 浏览器验收入口。先把 DISKMAP_CACHE_DIR、DISKMAP_CONFIG_DIR 指向测试目录。

#[cfg(not(feature = "server"))]
fn main() {
    eprintln!(
        "请启用 server 特性 / Enable the server feature: cargo run -p diskmap-core --features server --example serve"
    );
}

#[cfg(feature = "server")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(debug_assertions)]
    eprintln!("调试版，速度不代表实际 / Debug build: not representative of speed");
    use diskmap_core::server::{ServeOptions, serve_with_app};
    use diskmap_core::{app::App, classify::Classifier, i18n, pkgdb::PkgDb, procinfo::ProcInfo};
    use std::io::{self, Write};
    use std::sync::{Arc, mpsc};
    use std::time::{Duration, Instant};

    let mut options = ServeOptions {
        port: std::env::var("DISKMAP_PORT")
            .ok()
            .filter(|v| !v.is_empty())
            .map(|v| v.parse())
            .transpose()?
            .unwrap_or(0),
        token: std::env::var("DISKMAP_TOKEN").ok(),
        ..ServeOptions::default()
    };
    let mut path = None;
    let mut apparent = false;
    let mut cross_fs = true;
    let mut fixture = false;
    let mut stdin_stop = false;
    let mut browser = false;
    let mut lang = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--port" => {
                options.port = args
                    .next()
                    .ok_or("--port needs a value / 缺少端口")?
                    .parse()?
            }
            "--token" => {
                options.token = Some(args.next().ok_or("--token needs a value / 缺少令牌")?)
            }
            "--lang" => lang = Some(args.next().ok_or("--lang needs a value / 缺少语言")?),
            "--apparent" => apparent = true,
            "--same-fs" => cross_fs = false,
            "--fixture" => fixture = true,
            "--stdin-stop" => stdin_stop = true,
            "--browser" => browser = true,
            "--help" | "-h" => {
                println!(
                    "DiskMap HTTP 验收 / HTTP preview\nserve [PATH] [--port N] [--lang zh|en] [--apparent] [--same-fs]\n默认 Ctrl+C 退出；--browser 在页面关闭后退出 / Ctrl+C to exit; --browser exits after the page closes\n测试选项 / Test options: --fixture --stdin-stop --token TOKEN"
                );
                return Ok(());
            }
            _ if !arg.starts_with('-') && path.is_none() => path = Some(arg),
            _ => return Err(format!("未知参数 / Unknown argument: {arg}").into()),
        }
    }
    i18n::set_default(i18n::resolve(lang.as_deref()));
    let app = if fixture {
        let pkgdb = Arc::new(PkgDb::default());
        Arc::new(App::with_components(
            pkgdb.clone(),
            Arc::new(Classifier::new(Some(pkgdb), &[])),
            ProcInfo::new(),
        ))
    } else {
        Arc::new(App::new())
    };
    let (stop_tx, stop_rx) = mpsc::channel();
    let signal_tx = stop_tx.clone();
    ctrlc::set_handler(move || {
        let _ = signal_tx.send(());
    })?;
    if stdin_stop {
        std::thread::spawn(move || {
            let mut line = String::new();
            let _ = io::stdin().read_line(&mut line);
            let _ = stop_tx.send(());
        });
    }
    let mut server = serve_with_app(options, app.clone())?;
    if let Some(path) = path {
        app.start_scan(&path, apparent, cross_fs)
            .map_err(|e| e.to_string())?;
    }
    println!("{}", server.url);
    io::stdout().flush()?;
    let start = Instant::now();
    loop {
        match stop_rx.recv_timeout(Duration::from_millis(200)) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if browser && app.ui_closed(start.elapsed()) {
            break;
        }
    }
    server.shutdown();
    Ok(())
}
