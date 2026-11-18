//! 同一合成数据上的阶段对照；显式运行，普通测试不分配大镜像。

use std::path::Path;
use std::sync::atomic::AtomicBool;

use crate::classify::{Classifier, ClassifierOptions};
use crate::platform::Os;

#[allow(dead_code)]
#[path = "../../../tests/fixtures/ntfs/synthetic.rs"]
pub(super) mod synthetic;

#[test]
#[ignore = "生成约 977 MiB 私有临时镜像，显式运行分阶段基准"]
fn staged_million_records() {
    staged(false, 1_000_000);
}

#[test]
#[ignore = "含多层目录、硬链接、扩展记录的百万文件分阶段基准"]
fn staged_mixed_million_records() {
    staged(true, 1_000_000);
}

#[test]
fn mixed_fixture_has_expected_totals() {
    staged(true, 617);
}

fn staged(mixed: bool, files: usize) {
    let image = synthetic::TempImage::empty();
    let expected = if mixed {
        Some(synthetic::write_mixed_benchmark(&image.path, files).unwrap())
    } else {
        synthetic::write_benchmark(&image.path, files).unwrap();
        None
    };
    let classifier = Classifier::with_options(None, &[], ClassifierOptions::for_os(Os::Windows));
    for run in 0..3 {
        let result = super::scan_image(
            &image.path,
            Path::new("C:\\"),
            false,
            &classifier,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        if let Some(expected) = &expected {
            assert_eq!(result.records, expected.records);
            assert_eq!(result.index.root().nfiles(), expected.files);
            assert_eq!(result.index.root().ndirs(), expected.dirs);
            assert_eq!(result.index.root().size(), expected.allocated);
            if files == 617 {
                // 引导、MFT头、位图、顺序块各读一次；扩展在顺序窗口中收集，不增加定位读取。
                assert_eq!(result.read_calls, 4);
            }
        } else {
            assert_eq!(result.records, files as u64 + 2);
            assert_eq!(result.index.root().nfiles(), files as u64 + 1);
        }
        assert_eq!(result.anomalies, 0);
        let t = result.timings;
        println!(
            "run={run} io={:.6} parse={:.6} tree={:.6} classify={:.6} hardlinks={:.6} index={:.6} total={:.6} read_calls={} read_bytes={} temporary={} index_bytes={}",
            t.io.as_secs_f64(),
            t.parse.as_secs_f64(),
            t.tree.as_secs_f64(),
            t.classify.as_secs_f64(),
            t.hardlinks.as_secs_f64(),
            t.index.as_secs_f64(),
            (result.parse_elapsed + result.build_elapsed).as_secs_f64(),
            result.read_calls,
            result.read_bytes,
            result.temporary_bytes,
            result.index.heap_bytes()
        );
    }
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        for line in status.lines().filter(|s| s.starts_with("VmHWM:")) {
            println!("{line}");
        }
    }
}

#[test]
#[ignore = "百万文件、两万组外置列表及重解析数据的 IO 基准"]
fn staged_io_million_records() {
    staged_io(1_000_000, std::time::Duration::ZERO);
}

#[test]
#[ignore = "同一 IO 基准，每次真实读取另加 128 微秒模拟延迟"]
fn staged_io_million_records_with_delay() {
    staged_io(1_000_000, std::time::Duration::from_micros(128));
}

fn staged_io(files: usize, delay: std::time::Duration) {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let image = synthetic::TempImage::empty();
    let expected = synthetic::write_io_benchmark(&image.path, files).unwrap();
    let classifier = Classifier::with_options(None, &[], ClassifierOptions::for_os(Os::Windows));
    for run in 0..3 {
        let result = super::scan_image_with_options(
            &image.path,
            Path::new(r"C:\"),
            &classifier,
            &AtomicBool::new(false),
            super::ImageOptions {
                apparent: false,
                read_delay: delay,
            },
            |_| {},
        )
        .unwrap();
        assert_eq!(result.records, expected.records);
        assert_eq!(result.index.root().nfiles(), expected.files);
        assert_eq!(result.index.root().ndirs(), expected.dirs);
        assert_eq!(result.index.root().size(), expected.allocated);
        assert_eq!(result.anomalies, 0);
        let mut digest = DefaultHasher::new();
        for dir in result.index.dirs() {
            (
                dir.path(),
                dir.size(),
                dir.nfiles(),
                dir.ndirs(),
                dir.class(),
                dir.flags(),
                dir.category_sizes(),
            )
                .hash(&mut digest);
            for file in dir.files() {
                (file.name(), file.size(), file.category()).hash(&mut digest);
            }
        }
        let t = result.timings;
        let counts = &result.read_stats;
        let reads: serde_json::Map<String, serde_json::Value> = [
            ("sequential", counts.sequential),
            ("extension", counts.extension),
            ("attribute_list", counts.attribute_list),
            ("reparse", counts.reparse),
            ("other", counts.other),
        ]
        .into_iter()
        .map(|(key, count)| {
            (
                key.to_owned(),
                serde_json::json!({
                    "calls":count.calls, "bytes":count.bytes, "seconds":count.elapsed.as_secs_f64()
                }),
            )
        })
        .collect();
        println!(
            "IO_BENCH {}",
            serde_json::json!({
                "run":run, "delay_us":delay.as_micros(), "files":files,
                "io":t.io.as_secs_f64(), "parse":t.parse.as_secs_f64(), "tree":t.tree.as_secs_f64(),
                "classify":t.classify.as_secs_f64(), "hardlinks":t.hardlinks.as_secs_f64(), "index":t.index.as_secs_f64(),
                "total":(result.parse_elapsed+result.build_elapsed).as_secs_f64(), "io_overlap":result.io_overlap.as_secs_f64(),
                "read_calls":result.read_calls, "read_bytes":result.read_bytes, "reads":reads,
                "temporary":result.temporary_bytes, "index_bytes":result.index.heap_bytes(),
            "collection_bytes":result.collection_bytes, "extension_records":result.extension_records, "deferred_records":result.deferred_records,
                "digest":format!("{:016x}",digest.finish())
            })
        );
    }
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        for line in status.lines().filter(|s| s.starts_with("VmHWM:")) {
            println!("{line}");
        }
    }
}
