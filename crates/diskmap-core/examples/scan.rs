//! 扫描性能测量（只读磁盘）：
//!
//! ```text
//! cargo run --release -p diskmap-core --example scan -- <路径> [--threads N] [--apparent] [--same-fs] [--pkgdb] [--repeat N] [--digest] [--method auto|generic|mft] [--image raw.ntfs] [--read-delay-us 128] [--export new.jsonl]
//! ```
//!
//! 每次扫描输出一行：状态、线程数、文件数、目录数、字节数、无权限目录数、用时、索引占用的内存
//! （以及平均每个条目多少字节），最后输出进程的峰值内存（Linux、Windows）。`--digest` 另外输出整棵树
//! （名字、大小、类别、标志）按名字顺序算出的摘要，用来确认多次扫描的结果相同。

use std::fs::OpenOptions;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{self, BufWriter, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use diskmap_core::classify::{Classifier, ClassifierOptions};
use diskmap_core::index::{DENIED, DirId, Index};
use diskmap_core::pkgdb::PkgDb;
use diskmap_core::platform::Os;
use diskmap_core::scan::{Scan, ScanMethod, ScanOptions, ntfs};

const USAGE: &str = "用法 / usage: scan [path] [--threads N] [--apparent] [--same-fs] [--pkgdb] [--repeat N] [--digest] [--method auto|generic|mft] [--image raw.ntfs] [--read-delay-us 128] [--export new.jsonl]";

fn main() {
    let mut args = std::env::args().skip(1);
    let mut root = None;
    let mut threads = 0;
    let mut apparent = false;
    let mut cross_fs = true;
    let mut pkgdb = false;
    let mut repeat = 1;
    let mut digest = false;
    let mut method = ScanMethod::Auto;
    let mut image = None;
    let mut export = None;
    let mut read_delay_us = None;
    while let Some(arg) = args.next() {
        let mut number = |name: &str| -> usize {
            args.next()
                .and_then(|v| v.parse().ok())
                .unwrap_or_else(|| exit(&format!("{name} 需要一个数字 / needs a number")))
        };
        match arg.as_str() {
            "--method" => {
                method = match args.next().as_deref() {
                    Some("auto") => ScanMethod::Auto,
                    Some("generic") => ScanMethod::Generic,
                    Some("mft") => ScanMethod::Mft,
                    _ => exit("--method 需要 auto/generic/mft / expects auto/generic/mft"),
                }
            }
            "--export" => {
                export = Some(
                    args.next()
                        .unwrap_or_else(|| exit("--export 缺少文件 / missing file")),
                );
            }
            "--image" => {
                image = Some(
                    args.next()
                        .unwrap_or_else(|| exit("--image 缺少文件 / missing file")),
                )
            }
            "--threads" => threads = number("--threads"),
            "--repeat" => repeat = number("--repeat").max(1),
            "--read-delay-us" => read_delay_us = Some(number("--read-delay-us") as u64),
            "--apparent" => apparent = true,
            "--same-fs" => cross_fs = false,
            "--pkgdb" => pkgdb = true,
            "--digest" => digest = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                return;
            }
            _ if root.is_none() && !arg.starts_with('-') => root = Some(arg),
            _ => exit(&format!("未知参数 / unknown argument: {arg}")),
        }
    }
    let root = root.unwrap_or_else(|| {
        if image.is_some() {
            "C:\\".to_owned()
        } else {
            exit("缺少路径 / missing path")
        }
    });
    if image.is_some() && (method == ScanMethod::Generic || pkgdb) {
        exit(
            "--image 不支持 --method generic 或 --pkgdb / cannot use generic or host package DB for images",
        );
    }

    if export.is_some() && repeat != 1 {
        exit("--export 需要 --repeat 1 / requires --repeat 1");
    }
    if read_delay_us.is_some() && image.is_none() {
        exit("--read-delay-us 只用于 --image 测量 / requires --image");
    }
    let t = Instant::now();
    let db = pkgdb.then(|| Arc::new(PkgDb::load()));
    let classifier = Arc::new(if image.is_some() {
        Classifier::with_options(None, &[], ClassifierOptions::for_os(Os::Windows))
    } else {
        Classifier::new(db, &[])
    });
    println!(
        "classifier ready in {:.3}s (pkgdb: {pkgdb})",
        t.elapsed().as_secs_f64()
    );

    let mut failed = false;
    for _ in 0..repeat {
        let scan_started_unix_ms = unix_ms();
        if image.is_some() || method == ScanMethod::Mft {
            let wall_started = Instant::now();
            let result = if let Some(image) = &image {
                ntfs::scan_image_with_options(
                    Path::new(image),
                    Path::new(&root),
                    &classifier,
                    &AtomicBool::new(false),
                    ntfs::ImageOptions {
                        apparent,
                        read_delay: Duration::from_micros(read_delay_us.unwrap_or(0)),
                    },
                    |_| {},
                )
                .unwrap_or_else(|e| exit(&format!("镜像扫描失败 / Image scan failed: {e}")))
            } else {
                ntfs::scan_volume(
                    Path::new(&root),
                    apparent,
                    &classifier,
                    &AtomicBool::new(false),
                    |_, _| {},
                )
                .unwrap_or_else(|e| exit(&format!("MFT 扫描失败 / MFT scan failed: {e}")))
            };
            let elapsed = wall_started.elapsed().as_secs_f64();
            let scan_finished_unix_ms = unix_ms();
            let index = &result.index;
            println!(
                "method=mft records={} anomalies={} files={} dirs={} bytes={} parse={:.3}s build={:.3}s temporary={:.1}MiB index={:.1}MiB",
                result.records,
                result.anomalies,
                index.root().nfiles(),
                index.root().ndirs(),
                index.root().size(),
                result.parse_elapsed.as_secs_f64(),
                result.build_elapsed.as_secs_f64(),
                result.temporary_bytes as f64 / 1048576.0,
                index.heap_bytes() as f64 / 1048576.0
            );
            println!(
                "stages / 阶段: {} read_calls={} read_bytes={} elapsed={elapsed:.6}s peak_rss_bytes={:?}",
                timing_json(&result.timings),
                result.read_calls,
                result.read_bytes,
                peak_rss_kib().map(|n| n * 1024)
            );
            println!(
                "reads / 读取分类: {} io_busy={:.6}s io_overlap={:.6}s read_delay_us={}",
                read_stats_json(&result.read_stats),
                read_busy(&result.read_stats).as_secs_f64(),
                result.io_overlap.as_secs_f64(),
                read_delay_us.unwrap_or(0)
            );
            println!(
                "collection / 收集表: bytes={} extensions={} deferred={}",
                result.collection_bytes, result.extension_records, result.deferred_records
            );
            if let Some(path) = &export {
                let summary = serde_json::json!({"type":"summary", "schema":1,
                    "root": text_path(index.root_path()).unwrap_or_else(|e| exit(&e.to_string())),
                    "method":"mft", "apparent":apparent, "state":"done",
                    "files":index.root().nfiles(), "dirs":index.root().ndirs(), "bytes":index.root().size(),
                    "elapsed_seconds":elapsed, "timings":timing_json(&result.timings),
                    "read_calls":result.read_calls,"read_bytes":result.read_bytes,
                    "read_stats":read_stats_json(&result.read_stats), "io_overlap_seconds":result.io_overlap.as_secs_f64(),
                    "io_busy_seconds":read_busy(&result.read_stats).as_secs_f64(),
                    "collection_bytes":result.collection_bytes, "extension_records":result.extension_records,
                    "deferred_records":result.deferred_records,
                    "read_delay_us":read_delay_us.unwrap_or(0),
                    "scan_started_unix_ms":scan_started_unix_ms,"scan_finished_unix_ms":scan_finished_unix_ms,
                    "peak_rss_bytes":peak_rss_kib().map(|n| n*1024),
                    "records":result.records, "anomalies":result.anomalies, "denied":0});
                export_index(Path::new(path), index, &summary)
                    .unwrap_or_else(|e| exit(&format!("导出失败 / Export failed: {e}")));
            }
            if digest {
                println!("digest={:016x}", tree_digest(index));
            }
            continue;
        }
        let mut opts = ScanOptions::new(&root);
        opts.method = method;
        opts.threads = threads;
        opts.apparent = apparent;
        opts.cross_fs = cross_fs;
        let t = Instant::now();
        let scan = Scan::start(opts, Arc::clone(&classifier));
        let state = scan.wait();
        let secs = t.elapsed().as_secs_f64();
        let scan_finished_unix_ms = unix_ms();
        let p = scan.progress();
        let index = scan.index();
        let entries = index.dir_count() + index.file_count();
        let heap = index.heap_bytes();
        println!(
            "{} threads={} files={} dirs={} bytes={} denied={} time={secs:.3}s index={:.1}MiB per_entry={:.1}B mounts={} skipped={}",
            state.key(),
            p.threads,
            p.files,
            p.dirs,
            p.bytes,
            p.denied,
            heap as f64 / 1048576.0,
            heap as f64 / entries.max(1) as f64,
            p.mounts.len(),
            p.skipped.len(),
        );
        println!(
            "method={} records={} anomalies={}",
            p.method.key(),
            p.records,
            p.anomalies
        );
        if let Some(path) = &export {
            if state != diskmap_core::scan::ScanState::Done {
                eprintln!("扫描未完成，不导出 / Scan did not complete; no export");
                failed = true;
            } else {
                let summary = serde_json::json!({"type":"summary", "schema":1,
                    "root":text_path(&p.root).unwrap_or_else(|e| exit(&e.to_string())),
                    "method":p.method.key(), "apparent":p.apparent, "state":state.key(),
                    "files":p.files, "dirs":p.dirs, "bytes":p.bytes, "elapsed_seconds":secs,
                    "records":p.records, "anomalies":p.anomalies, "denied":p.denied,
                    "scan_started_unix_ms":scan_started_unix_ms,"scan_finished_unix_ms":scan_finished_unix_ms,
                    "peak_rss_bytes":peak_rss_kib().map(|n|n*1024)});
                export_index(Path::new(path), &index, &summary)
                    .unwrap_or_else(|e| exit(&format!("导出失败 / Export failed: {e}")));
            }
        }
        if let Some(reason) = p.fallback_reason {
            println!("回退原因 / fallback: {reason}");
        }
        if let Some(e) = p.error {
            failed = true;
            println!("error: {e}");
        }
        if digest {
            println!("digest={:016x}", tree_digest(&index));
        }
    }
    if let Some(kib) = peak_rss_kib() {
        println!("peak_rss={:.1}MiB", kib as f64 / 1024.0);
    }
    if failed {
        std::process::exit(1);
    }
}

/// 导出的是完成索引的可见条目；隐藏硬链接不会被伪装成零字节文件。
/// 文件必须不存在，防止验收覆盖用户数据。扫描耗时不包括导出及后续对账。
fn export_index(path: &Path, index: &Index, summary: &serde_json::Value) -> io::Result<()> {
    let file = OpenOptions::new().write(true).create_new(true).open(path)?;
    let mut writer = BufWriter::with_capacity(1024 * 1024, file);
    json_line(&mut writer, summary)?;
    let (mut count, mut bytes) = (0u64, 0u64);
    for dir in index.dirs() {
        if dir.flags() & DENIED != 0 {
            json_line(
                &mut writer,
                &serde_json::json!({"type":"denied_dir", "path":text_path(&dir.path())?}),
            )?;
        }
        for file in dir.files() {
            let path = index.file_path(dir.id(), file.id());
            json_line(
                &mut writer,
                &serde_json::json!({"type":"file", "path":text_path(&path)?,
                "size":file.size(), "category":file.category().key()}),
            )?;
            count += 1;
            bytes += file.size();
        }
    }
    json_line(
        &mut writer,
        &serde_json::json!({"type":"end", "files":count, "bytes":bytes}),
    )?;
    writer.flush()
}

fn text_path(path: &Path) -> io::Result<&str> {
    path.to_str().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "路径无法无损编码为 JSON 文本 / Path cannot be represented losslessly in JSON text",
        )
    })
}

fn json_line(writer: &mut impl Write, value: &serde_json::Value) -> io::Result<()> {
    serde_json::to_writer(&mut *writer, value)?;
    writer.write_all(b"\n")
}

/// 从根目录按名字顺序深度优先遍历整棵树，对每个目录和文件的名字、大小、类别、标志求摘要。
fn tree_digest(index: &Index) -> u64 {
    let mut h = DefaultHasher::new();
    let mut stack = vec![DirId::ROOT];
    while let Some(id) = stack.pop() {
        let d = index.dir(id);
        (
            d.name(),
            d.size(),
            d.nfiles(),
            d.ndirs(),
            d.class(),
            d.flags(),
        )
            .hash(&mut h);
        for f in d.files() {
            (f.name(), f.size(), f.category()).hash(&mut h);
        }
        let subs: Vec<DirId> = d.subdirs().map(|s| s.id()).collect();
        stack.extend(subs.into_iter().rev());
    }
    h.finish()
}

fn exit(msg: &str) -> ! {
    eprintln!("{msg}\n{USAGE}");
    std::process::exit(2)
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

fn timing_json(t: &ntfs::Timings) -> serde_json::Value {
    serde_json::json!({"io":t.io.as_secs_f64(), "parse":t.parse.as_secs_f64(), "tree":t.tree.as_secs_f64(),
        "classify":t.classify.as_secs_f64(), "hardlinks":t.hardlinks.as_secs_f64(), "index":t.index.as_secs_f64()})
}

fn read_stats_json(stats: &ntfs::ReadStats) -> serde_json::Value {
    let count = |value: &ntfs::ReadCount| {
        serde_json::json!({
            "calls": value.calls, "bytes": value.bytes, "elapsed_seconds": value.elapsed.as_secs_f64()
        })
    };
    serde_json::json!({
        "sequential":count(&stats.sequential), "extension":count(&stats.extension),
        "attribute_list":count(&stats.attribute_list), "reparse":count(&stats.reparse),
        "other":count(&stats.other)
    })
}

fn read_busy(stats: &ntfs::ReadStats) -> Duration {
    stats.sequential.elapsed
        + stats.extension.elapsed
        + stats.attribute_list.elapsed
        + stats.reparse.elapsed
        + stats.other.elapsed
}

/// Linux 的 VmHWM；Windows 的 PeakWorkingSetSize。macOS 不做运行时性能探测。
#[cfg(not(windows))]
fn peak_rss_kib() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("VmHWM:"))?;
    line.split_whitespace().nth(1)?.parse().ok()
}

#[cfg(windows)]
fn peak_rss_kib() -> Option<u64> {
    use windows_sys::Win32::System::ProcessStatus::{
        K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    let mut counters = PROCESS_MEMORY_COUNTERS::default();
    let len = std::mem::size_of_val(&counters) as u32;
    counters.cb = len;
    // SAFETY: 当前进程伪句柄无需关闭；counters有效可写且传入准确结构长度。
    let ok = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, len) };
    (ok != 0).then_some(counters.PeakWorkingSetSize as u64 / 1024)
}

#[cfg(test)]
mod tests {
    use super::*;
    use diskmap_core::classify::ReasonId;
    use diskmap_core::index::{FileId, NewDir, NewFile};
    use diskmap_core::model::Category;
    use std::ffi::OsStr;

    #[test]
    fn export_contains_only_counted_files_all_denied_dirs_and_completion() {
        let root = std::env::temp_dir().join(format!(
            "diskmap-export-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("scan.jsonl");
        let class = (Category::Other, ReasonId(0));
        let mut index = Index::new(Os::Windows, Path::new(r"C:\"), class);
        let names: Vec<_> = (0..52).map(|i| format!("denied-{i:02}")).collect();
        let dirs: Vec<_> = names
            .iter()
            .map(|name| NewDir {
                name: OsStr::new(name),
                class,
                flags: DENIED,
            })
            .collect();
        let files = ["a-中文.bin", "z-link.bin"].map(|name| NewFile {
            name: OsStr::new(name),
            size: 8192,
            category: Category::Other,
            linked: true,
        });
        let filled = index.fill_dir(DirId::ROOT, &dirs, &files).unwrap();
        index.count_file(DirId::ROOT, FileId(filled.files.start));
        let summary = serde_json::json!({"type":"summary", "schema":1, "files":1, "bytes":8192});
        export_index(&path, &index, &summary).unwrap();
        let before = std::fs::read(&path).unwrap();
        let rows: Vec<serde_json::Value> = String::from_utf8(before.clone())
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(
            rows.iter()
                .filter(|row| row["type"] == "denied_dir")
                .count(),
            52
        );
        let files: Vec<_> = rows.iter().filter(|row| row["type"] == "file").collect();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0]["size"], 8192);
        assert_eq!(files[0]["path"], r"C:\a-中文.bin");
        assert_eq!(
            rows.last().unwrap(),
            &serde_json::json!({"type":"end", "files":1, "bytes":8192})
        );
        assert_eq!(
            export_index(&path, &index, &summary).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
        // 只清理本测试自己创建的输出及目录。
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
}
