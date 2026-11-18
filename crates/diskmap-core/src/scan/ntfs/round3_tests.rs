//! 顺序收集与批量读取的回归：合法树保持原样，引用及列表损坏继续局部隔离。

use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::classify::{Classifier, ClassifierOptions};
use crate::index::Index;
use crate::platform::Os;

use super::benchmark::synthetic::*;

fn scan(image: &TempImage, apparent: bool) -> std::io::Result<super::ScanResult> {
    super::scan_image(
        &image.path,
        Path::new(r"C:\"),
        apparent,
        &Classifier::with_options(None, &[], ClassifierOptions::for_os(Os::Windows)),
        &AtomicBool::new(false),
        |_| {},
    )
}

fn digest(index: &Index) -> u64 {
    let mut h = DefaultHasher::new();
    for dir in index.dirs() {
        (
            dir.path(),
            dir.size(),
            dir.nfiles(),
            dir.ndirs(),
            dir.class(),
            dir.flags(),
            dir.category_sizes(),
        )
            .hash(&mut h);
        for file in dir.files() {
            (file.name(), file.size(), file.category()).hash(&mut h);
        }
    }
    h.finish()
}

#[test]
fn external_metadata_and_both_extension_orders_preserve_complete_tree() {
    let mixed = TempImage::empty();
    let io = TempImage::empty();
    // 22 000 记录，跨越五个 4 MiB 窗口；扩展放在基础前后两边。
    let expected = write_mixed_benchmark(&mixed.path, 20_000).unwrap();
    write_io_benchmark(&io.path, 20_000).unwrap();
    for apparent in [false, true] {
        let old_layout = scan(&mixed, apparent).unwrap();
        let new_layout = scan(&io, apparent).unwrap();
        assert_eq!(old_layout.anomalies, 0);
        assert_eq!(new_layout.anomalies, 0);
        assert_eq!(new_layout.records, expected.records);
        assert_eq!(
            new_layout.index.root().size(),
            if apparent {
                expected.apparent
            } else {
                expected.allocated
            }
        );
        assert_eq!(digest(&old_layout.index), digest(&new_layout.index));
    }
}

fn base(list: &[u8]) -> Vec<u8> {
    file_record(
        40,
        false,
        0,
        &[
            si(0),
            resident(0x20, 2, "", list),
            filename(1, reference(17), "late.bin", 1),
        ],
    )
}

fn valid_list() -> Vec<u8> {
    [
        (0x10, 40, 0),
        (0x30, 40, 1),
        (0x30, 41, 3),
        (0x30, 41, 4),
        (0x80, 41, 5),
    ]
    .into_iter()
    .flat_map(|(ty, record, id)| list_entry(ty, "", 0, reference(record), id))
    .collect()
}

#[test]
fn collected_extensions_keep_reference_and_completeness_checks() {
    for case in 0..8 {
        let mut bytes = data_in_extension_image(false, 0);
        let mut list = valid_list();
        match case {
            0 => {
                // 扩展引用序号失效。
                put16(&mut bytes, record_offset(41) + 16, 2);
            }
            1 => {
                // 扩展头的基础引用序号失效。
                put64(&mut bytes, record_offset(41) + 32, 40 | (2 << 48));
            }
            2 => {
                // 相同记录/实例重复。
                list.extend(list_entry(0x80, "", 0, reference(41), 5));
                set_record(&mut bytes, 40, &base(&list));
            }
            3 => {
                // 列表漏掉扩展中的一个名字。
                list.drain(2 * 32..3 * 32);
                set_record(&mut bytes, 40, &base(&list));
            }
            4 => {
                // 递归属性列表即便内容为空也不能接受。
                list.extend(list_entry(0x20, "", 0, reference(41), 6));
                set_record(&mut bytes, 40, &base(&list));
                set_record(
                    &mut bytes,
                    41,
                    &file_record(
                        41,
                        false,
                        reference(40),
                        &[
                            filename(3, reference(16), "first.bin", 1),
                            filename(4, reference(16), "second.bin", 1),
                            data(5, 6000, 80, 2),
                            resident(0x20, 6, "", &[]),
                        ],
                    ),
                );
            }
            5 => {
                // 被引用扩展记录已经删除。
                put16(&mut bytes, record_offset(41) + 22, 0);
            }
            6 => {
                // 列表指向不存在的属性实例。
                put16(&mut list, 4 * 32 + 24, 99);
                set_record(&mut bytes, 40, &base(&list));
            }
            7 => {
                // 属性列表超过上限，不能触发巨量分配或提前读盘。
                set_record(
                    &mut bytes,
                    40,
                    &file_record(
                        40,
                        false,
                        0,
                        &[
                            si(0),
                            filename(1, reference(17), "late.bin", 1),
                            nonresident(Stream {
                                ty: 0x20,
                                id: 2,
                                name: "",
                                low: 0,
                                allocated: 65 * CLUSTER as u64,
                                size: 256 * 1024 + 32,
                                flags: 0,
                                physical: 0,
                                runs: &[(Some(80), 65)],
                            }),
                        ],
                    ),
                );
            }
            _ => unreachable!(),
        }
        let result = scan(&TempImage::new(&bytes), false).unwrap();
        assert!(result.anomalies >= 1, "case={case}");
        assert!(
            result.index.find(Path::new(r"C:\a\first.bin")).is_none(),
            "case={case}"
        );
        assert!(
            result.index.find(Path::new(r"C:\a\tiny.txt")).is_some(),
            "case={case}"
        );
    }
}

#[test]
fn cancellation_during_sequential_collection_does_not_publish_deferred_records() {
    let image = TempImage::empty();
    write_io_benchmark(&image.path, 20_000).unwrap();
    let cancel = AtomicBool::new(false);
    let result = super::scan_image(
        &image.path,
        Path::new(r"C:\"),
        false,
        &Classifier::with_options(None, &[], ClassifierOptions::for_os(Os::Windows)),
        &cancel,
        |records| {
            if records > 4096 {
                cancel.store(true, Ordering::Relaxed);
            }
        },
    );
    assert_eq!(
        result.err().unwrap().kind(),
        std::io::ErrorKind::Interrupted
    );
}

#[test]
fn external_metadata_batches_reduce_reads_without_extension_io() {
    let image = TempImage::empty();
    write_io_benchmark(&image.path, 20_000).unwrap();
    let result = scan(&image, false).unwrap();
    assert_eq!(result.anomalies, 0);
    assert_eq!(result.read_stats.extension.calls, 0);
    // 旧实现此规模分别读 400 次；允许因物理间距/分组边界分成多个批次。
    assert!(result.read_stats.attribute_list.calls < 100);
    assert!(result.read_stats.reparse.calls < 100);
    assert!(result.read_stats.attribute_list.bytes >= 400 * 4096);
    assert!(result.read_stats.reparse.bytes >= 400 * 4096);
}

#[test]
fn rejected_list_never_reads_unlisted_external_metadata() {
    let mut bytes = data_in_extension_image(false, 0);
    // 列表没列出 C0，基础记录必须隔离。其物理位置在截短镜像外；不能为了预读它使整卷失败。
    set_record(
        &mut bytes,
        41,
        &file_record(
            41,
            false,
            reference(40),
            &[
                filename(3, reference(16), "first.bin", 1),
                filename(4, reference(16), "second.bin", 1),
                data(5, 6000, 80, 2),
                nonresident(Stream {
                    ty: 0xc0,
                    id: 6,
                    name: "",
                    low: 0,
                    allocated: CLUSTER as u64,
                    size: 8,
                    flags: 0,
                    physical: 0,
                    runs: &[(Some(200), 1)],
                }),
            ],
        ),
    );
    bytes.truncate(32 * CLUSTER);
    let result = scan(&TempImage::new(&bytes), false).unwrap();
    assert!(result.anomalies >= 1);
    assert!(result.index.find(Path::new(r"C:\a\tiny.txt")).is_some());
    assert_eq!(result.read_stats.reparse.calls, 0);
}

#[test]
fn invalid_standard_information_does_not_read_external_reparse() {
    for case in 0..3 {
        let mut bytes = small_image(false);
        let mut attrs = vec![
            if case == 0 {
                resident(0x10, 0, "", &[0])
            } else {
                si(0x400)
            },
            if case == 2 {
                resident(0x30, 1, "", &[0])
            } else {
                filename(1, reference(16), "tiny.txt", 1)
            },
            resident(0x80, 2, "", b"abc"),
            nonresident(Stream {
                ty: 0xc0,
                id: 3,
                name: "",
                low: 0,
                allocated: CLUSTER as u64,
                size: 8,
                flags: 0,
                physical: 0,
                runs: &[(Some(200), 1)],
            }),
        ];
        if case == 1 {
            attrs.push(si(0x400)); // 首条 SI 有有效重解析标志，但重复 SI 必须在读盘前拒绝。
        }
        set_record(&mut bytes, 18, &file_record(18, false, 0, &attrs));
        bytes.truncate(32 * CLUSTER);
        let result = scan(&TempImage::new(&bytes), false).unwrap();
        assert_eq!(result.anomalies, 1, "case={case}");
        assert!(
            result.index.find(Path::new(r"C:\a\tiny.txt")).is_none(),
            "case={case}"
        );
        assert!(
            result.index.find(Path::new(r"C:\a\large.bin")).is_some(),
            "case={case}"
        );
        assert_eq!(result.read_stats.reparse.calls, 0, "case={case}");
    }
}
