//! 合成卷端到端测试：镜像全部由测试生成，不读取本机文件系统内容。

#[path = "fixtures/ntfs/synthetic.rs"]
mod synthetic;

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use diskmap_core::classify::{Classifier, ClassifierOptions};
use diskmap_core::index::{Found, Index};
use diskmap_core::model::Category;
use diskmap_core::platform::Os;
use diskmap_core::scan::ntfs::{self, ScanResult};

use synthetic::*;

fn classifier() -> Classifier {
    Classifier::with_options(None, &[], ClassifierOptions::for_os(Os::Windows))
}

fn scan(image: &TempImage, apparent: bool) -> std::io::Result<ScanResult> {
    ntfs::scan_image(
        &image.path,
        Path::new(r"C:\"),
        apparent,
        &classifier(),
        &AtomicBool::new(false),
        |_| {},
    )
}

fn file(index: &Index, relative: &str) -> (u64, Category) {
    let path = format!(r"C:\{relative}");
    let Some(Found::File { file, .. }) = index.find(Path::new(&path)) else {
        panic!("缺少文件：{relative}");
    };
    let item = index.file(file);
    (item.size(), item.category())
}

#[test]
fn sizes_links_streams_reparse_and_metadata() {
    for nonresident_list in [false, true] {
        let bytes = small_image(nonresident_list);
        let image = TempImage::new(&bytes);
        let result = scan(&image, false).unwrap();
        let index = result.index;
        assert_eq!(index.root().nfiles(), 14);
        assert_eq!(index.root().ndirs(), 4);
        assert_eq!(index.root().size(), 98_352);
        for (path, size) in [
            (r"a\tiny.txt", 8),
            (r"a\large.bin", 8192),
            (r"a\shared.bin", 4096),
            ("sparse.bin", 4096),
            ("compressed.bin", 4096),
            ("streams.bin", 4112),
            ("cloud.bin", 0),
            ("dir-link", 0),
            (r"cloud-dir\nested.txt", 8),
            ("extended.bin", 8192),
            ("fragment-two.txt", 8),
        ] {
            assert_eq!(file(&index, path).0, size, "{path}");
        }
        assert_eq!(file(&index, "$MFT"), (65_536, Category::Core));
        assert_eq!(file(&index, "$Volume"), (0, Category::Core));
        assert_eq!(file(&index, "$UsnJrnl"), (8, Category::Core));
        for absent in [
            r"C:\z\shared.bin",
            r"C:\a\SHARED~1.BIN",
            r"C:\junction",
            r"C:\$Extend\$UsnJrnl",
        ] {
            assert!(index.find(Path::new(absent)).is_none(), "{absent}");
        }
        // 第二次用文件长度口径；稀疏、压缩、仅在线文件仍保留逻辑长度。
        let logical = scan(&image, true).unwrap().index;
        assert_eq!(logical.root().size(), 162_717);
        for (path, size) in [
            (r"a\tiny.txt", 3),
            ("sparse.bin", 16_384),
            ("compressed.bin", 65_536),
            ("streams.bin", 1007),
            ("cloud.bin", 999),
            ("extended.bin", 6000),
        ] {
            assert_eq!(file(&logical, path).0, size, "{path}");
        }
        assert_eq!(std::fs::read(&image.path).unwrap(), bytes);
    }
}

#[test]
fn unicode_names_and_windows_case_insensitive_lookup() {
    let mut bytes = small_image(false);
    set_record(
        &mut bytes,
        18,
        &file_record(
            18,
            false,
            0,
            &[
                si(0),
                filename(1, reference(16), "文档-😀.TXT", 1),
                resident(0x80, 2, "", b"abc"),
            ],
        ),
    );
    let result = scan(&TempImage::new(&bytes), false).unwrap();
    assert_eq!(file(&result.index, r"A\文档-😀.txt").0, 8);
}

#[test]
fn directory_index_allocation_is_not_counted() {
    let mut bytes = small_image(false);
    set_record(
        &mut bytes,
        16,
        &file_record(
            16,
            true,
            0,
            &[
                si(0x10),
                filename(1, reference(5), "a", 1),
                nonresident(Stream {
                    ty: 0xa0,
                    id: 2,
                    name: "$I30",
                    low: 0,
                    allocated: 8192,
                    size: 8192,
                    flags: 0,
                    physical: 0,
                    runs: &[(Some(80), 2)],
                }),
            ],
        ),
    );
    let result = scan(&TempImage::new(&bytes), false).unwrap();
    assert_eq!(result.index.root().size(), 98_352);
}

#[test]
fn bad_cluster_stream_counts_only_physical_bad_clusters() {
    for physical in [false, true] {
        let image = TempImage::new(&bad_cluster_image(physical, 8, "$Bad"));
        let actual = scan(&image, false).unwrap();
        let size = if physical { CLUSTER as u64 } else { 0 };
        assert_eq!(file(&actual.index, "$BadClus"), (size, Category::Core));
        assert_eq!(actual.index.root().size(), 98_352 + size);
        let apparent = scan(&image, true).unwrap();
        assert_eq!(file(&apparent.index, "$BadClus").0, VOLUME_BYTES as u64);
        assert_eq!(apparent.index.root().size(), 162_717 + VOLUME_BYTES as u64);
    }
}

#[test]
fn unflagged_sparse_runs_outside_bad_cluster_stream_are_isolated() {
    // 相同文件名/流名不能让普通记录绕过稀疏标志校验；记录8的其他流也没有例外。
    for (record, stream) in [(18, "$Bad"), (8, "$Other")] {
        let image = TempImage::new(&bad_cluster_image(false, record, stream));
        assert_record_anomaly(&image);
    }
}

#[test]
fn each_cloud_flag_zeros_only_allocated_size() {
    for flag in [0x40_0000, 0x1000] {
        let mut bytes = small_image(false);
        set_record(
            &mut bytes,
            24,
            &file_record(
                24,
                false,
                0,
                &[
                    si(flag),
                    filename(1, reference(5), "cloud.bin", 1),
                    data(2, 999, 55, 1),
                ],
            ),
        );
        let image = TempImage::new(&bytes);
        assert_eq!(file(&scan(&image, false).unwrap().index, "cloud.bin").0, 0);
        assert_eq!(file(&scan(&image, true).unwrap().index, "cloud.bin").0, 999);
    }
}

#[test]
fn record_deleted_after_bitmap_read_is_not_counted() {
    let mut bytes = small_image(false);
    put16(&mut bytes, record_offset(40) + 22, 0);
    let result = scan(&TempImage::new(&bytes), false).unwrap();
    assert_eq!(result.index.root().nfiles(), 13);
    assert_eq!(result.index.root().size(), 98_344);
    assert!(
        result
            .index
            .find(Path::new(r"C:\fragment-two.txt"))
            .is_none()
    );
}

#[test]
fn malformed_or_short_mft_bitmap_falls_back_without_counting_free_slots() {
    let baseline = scan(&TempImage::new(&small_image(false)), false).unwrap();
    assert_eq!(baseline.anomalies, 0);
    for missing_first_extent in [false, true] {
        let mut bytes = invalid_bitmap_image(missing_first_extent);
        // 除全零空槽外，再放一个带损坏旧属性的已删除 FILE 槽，降级扫描也不能误报。
        let mut deleted = file_record(
            63,
            false,
            0,
            &[
                si(0),
                filename(1, reference(5), "deleted.bin", 1),
                resident(0x80, 2, "", b"stale"),
            ],
        );
        put16(&mut deleted, 22, 0);
        put32(&mut deleted, 56 + 4, 0);
        set_record(&mut bytes, 63, &deleted);
        let image = TempImage::new(&bytes);
        let result = scan(&image, false).unwrap();
        assert_eq!(result.anomalies, 1, "只有记录 0 的 BITMAP 异常");
        assert_eq!(result.records, baseline.records);
        assert_eq!(result.records, 21);
        assert_eq!(result.index.root().size(), baseline.index.root().size());
        assert_eq!(result.index.root().nfiles(), baseline.index.root().nfiles());
        assert_eq!(result.index.root().ndirs(), baseline.index.root().ndirs());
        assert_eq!(file(&result.index, "streams.bin").0, 4112);
        assert_eq!(file(&result.index, "extended.bin").0, 8192);
        assert_eq!(file(&result.index, "fragment-two.txt").0, 8);
        assert!(result.index.find(Path::new(r"C:\deleted.bin")).is_none());
        assert_eq!(std::fs::read(&image.path).unwrap(), bytes);
    }
}

#[test]
fn invalid_boot_record_and_truncation_are_errors() {
    let bytes = small_image(false);
    for length in [0, 1, 511, MFT_LCN as usize * CLUSTER + RECORD - 1] {
        assert!(scan(&TempImage::new(&bytes[..length]), false).is_err());
    }
    let mut bad = bytes.clone();
    bad[3] = b'X';
    assert!(scan(&TempImage::new(&bad), false).is_err());
    let mut bad = bytes.clone();
    bad[record_offset(0) + SECTOR - 1] ^= 1;
    assert!(scan(&TempImage::new(&bad), false).is_err());
    let mut bad = bytes;
    // MFT 本身第一属性长度为0，无法继续；不能形成永不前进的解析循环。
    put32(&mut bad, record_offset(0) + 56 + 4, 0);
    assert!(scan(&TempImage::new(&bad), false).is_err());
}

#[test]
fn missing_reused_parent_and_directory_cycle_are_isolated() {
    for parent in [reference(63), 16 | (2 << 48)] {
        let mut bytes = small_image(false);
        set_record(
            &mut bytes,
            18,
            &file_record(
                18,
                false,
                0,
                &[
                    si(0),
                    filename(1, parent, "orphan.txt", 1),
                    resident(0x80, 2, "", b"abc"),
                ],
            ),
        );
        assert_record_anomaly(&TempImage::new(&bytes));
    }
    let mut bytes = small_image(false);
    for (id, parent, name) in [(16, 17, "a"), (17, 16, "z")] {
        set_record(
            &mut bytes,
            id,
            &file_record(
                id as u64,
                true,
                0,
                &[si(0x10), filename(1, reference(parent), name, 1)],
            ),
        );
    }
    assert_record_anomaly(&TempImage::new(&bytes));
}

#[test]
fn stale_extension_reference_is_isolated() {
    let mut bytes = small_image(false);
    // 属性列表仍指向31号记录的第1代；实际记录已回收为第2代。
    put16(&mut bytes, record_offset(31) + 16, 2);
    assert_record_anomaly(&TempImage::new(&bytes));
}

#[test]
fn incomplete_attribute_lists_are_isolated() {
    let mut bytes = small_image(false);
    let list: Vec<_> = [(0x10, 0, 30, 0), (0x80, 0, 30, 2), (0x80, 1, 31, 3)]
        .into_iter()
        .flat_map(|(ty, low, id, instance)| list_entry(ty, "", low, reference(id), instance))
        .collect();
    // 列表漏掉基记录的 FILE_NAME，必须记录异常，不能影响健康文件。
    set_record(
        &mut bytes,
        30,
        &file_record(
            30,
            false,
            0,
            &[
                si(0),
                resident(0x20, 4, "", &list),
                filename(1, reference(5), "extended.bin", 1),
                nonresident(Stream {
                    ty: 0x80,
                    id: 2,
                    name: "",
                    low: 0,
                    allocated: 8192,
                    size: 6000,
                    flags: 0,
                    physical: 0,
                    runs: &[(Some(60), 1)],
                }),
            ],
        ),
    );
    assert_record_anomaly(&TempImage::new(&bytes));

    let mut bytes = small_image(false);
    // 扩展记录出现列表未列出的命名流，也必须记录异常并继续扫描。
    set_record(
        &mut bytes,
        31,
        &file_record(
            31,
            false,
            reference(30),
            &[
                nonresident(Stream {
                    ty: 0x80,
                    id: 3,
                    name: "",
                    low: 1,
                    allocated: 0,
                    size: 0,
                    flags: 0,
                    physical: 0,
                    runs: &[(Some(61), 1)],
                }),
                resident(0x80, 7, "unlisted", b"unexpected"),
            ],
        ),
    );
    assert_record_anomaly(&TempImage::new(&bytes));
}

#[test]
fn mft_bootstrap_resolves_reachable_mapping_extensions() {
    let result = scan(&TempImage::new(&bootstrap_image(false)), false).unwrap();
    assert_eq!(result.index.root().size(), 98_352);
    assert_eq!(file(&result.index, "fragment-two.txt").0, 8);
    assert!(scan(&TempImage::new(&bootstrap_image(true)), false).is_err());
    let mut stale = bootstrap_image(false);
    put16(&mut stale, record_offset(1) + 16, 2);
    assert!(scan(&TempImage::new(&stale), false).is_err());
}

#[test]
fn mft_auxiliary_metadata_damage_does_not_abort_data_bootstrap() {
    use MftAuxiliaryDamage::{InvalidAttributeList, OmittedFileName, StaleBitmapReference};

    for (data_extension, damage) in [
        (false, StaleBitmapReference),
        (true, StaleBitmapReference),
        (false, OmittedFileName),
        (true, OmittedFileName),
        (false, InvalidAttributeList),
    ] {
        let image = TempImage::new(&mft_auxiliary_damage_image(data_extension, damage));
        let result = scan(&image, false).expect("非 DATA 元数据损坏不能使 MFT 引导失败");
        assert_eq!(result.anomalies, 1, "基础记录 0 的多处错误只计一次");
        assert!(result.index.root().nfiles() >= 13);
        for (path, size) in [
            (r"a\tiny.txt", 8),
            (r"a\large.bin", 8192),
            (r"a\shared.bin", 4096),
            ("sparse.bin", 4096),
            ("compressed.bin", 4096),
            ("streams.bin", 4112),
            ("cloud.bin", 0),
            ("dir-link", 0),
            (r"cloud-dir\nested.txt", 8),
            ("extended.bin", 8192),
            ("fragment-two.txt", 8),
        ] {
            assert_eq!(file(&result.index, path).0, size, "{path}");
        }
    }
}

#[test]
fn mutated_images_never_panic() {
    let original = small_image(false);
    let active = [0, 3, 5, 11, 18, 19, 20, 21, 22, 23, 24, 25, 30, 31, 40];
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    for case in 0..256 {
        let mut bytes = original.clone();
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let start = if case % 16 == 0 {
            (seed as usize) % SECTOR
        } else {
            record_offset(active[case % active.len()]) + (seed as usize % RECORD)
        };
        for i in 0..8 {
            bytes[start + i] ^= (seed >> (i * 8)) as u8;
        }
        // 部分修改落在保留字段，成功也是合法结果；错误必须作为 Result 返回。
        let _ = scan(&TempImage::new(&bytes), case % 2 == 0);
    }
}

#[test]
fn cancellation_before_and_during_mft_read() {
    let image = TempImage::new(&small_image(false));
    for initially_cancelled in [true, false] {
        let cancel = AtomicBool::new(initially_cancelled);
        let mut progress = 0;
        let result = ntfs::scan_image(
            &image.path,
            Path::new(r"C:\"),
            false,
            &classifier(),
            &cancel,
            |records| {
                progress = records;
                cancel.store(true, Ordering::Relaxed);
            },
        );
        let error = result.err().expect("取消不能发布完整目录树");
        assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
        if !initially_cancelled {
            assert!(progress > 0);
        }
    }
}

#[test]
#[ignore = "生成约 977 MiB 的临时镜像；性能测量时显式运行"]
fn million_record_benchmark() {
    let image = TempImage::empty();
    let generation = Instant::now();
    write_benchmark(&image.path, 1_000_000).unwrap();
    println!(
        "生成 / generated: {:.3}s",
        generation.elapsed().as_secs_f64()
    );
    print_peak("扫描前 / before scan");
    let started = Instant::now();
    let result = scan(&image, false).unwrap();
    assert_eq!(result.index.root().nfiles(), 1_000_001);
    assert_eq!(result.index.root().ndirs(), 0);
    assert_eq!(result.records, 1_000_002);
    assert_eq!(result.anomalies, 0);
    println!(
        "records={} parse={:.3}s build={:.3}s total={:.3}s temporary={:.2}MiB index={:.2}MiB",
        result.records,
        result.parse_elapsed.as_secs_f64(),
        result.build_elapsed.as_secs_f64(),
        started.elapsed().as_secs_f64(),
        result.temporary_bytes as f64 / 1_048_576.0,
        result.index.heap_bytes() as f64 / 1_048_576.0,
    );
    print_peak("扫描后 / after scan");
}

fn print_peak(label: &str) {
    if let Ok(status) = std::fs::read_to_string("/proc/self/status")
        && let Some(line) = status.lines().find(|s| s.starts_with("VmHWM:"))
    {
        println!("{label}: {line}");
    }
}

#[test]
fn all_data_in_extension_is_counted_for_one_hard_link_in_either_record_order() {
    for extension_first in [false, true] {
        let image = TempImage::new(&data_in_extension_image(extension_first, 0));
        let actual = scan(&image, false).unwrap();
        assert_eq!(file(&actual.index, r"a\first.bin").0, 8192);
        assert_eq!(actual.index.root().size(), 98_352 - 8 + 8192);
        assert!(actual.index.find(Path::new(r"C:\a\second.bin")).is_none());
        assert!(actual.index.find(Path::new(r"C:\z\late.bin")).is_none());
        assert_eq!(
            file(&scan(&image, true).unwrap().index, r"a\first.bin").0,
            6000
        );
    }
}

#[test]
fn standard_information_ea_bit_does_not_zero_extended_hard_link_data() {
    // 0x40000 在磁盘 SI 是 FILE_ATTRIBUTE_EA，只有目录枚举接口才解释为 RECALL_ON_OPEN。
    // Microsoft： https://learn.microsoft.com/en-us/windows/win32/fileio/file-attribute-constants
    for extension_first in [false, true] {
        let image = TempImage::new(&data_in_extension_image(extension_first, 0x40000));
        let actual = scan(&image, false).unwrap();
        assert_eq!(file(&actual.index, r"a\first.bin").0, 8192);
        assert_eq!(
            file(&scan(&image, true).unwrap().index, r"a\first.bin").0,
            6000
        );
    }
}

#[test]
fn windows_acceptance_six_compressed_extents_merge_their_flags() {
    let image = TempImage::new(&acceptance_split_compressed_image());
    let actual = scan(&image, false).unwrap();
    assert_eq!(file(&actual.index, "seg0.log").0, 48 * CLUSTER as u64);
    assert_eq!(actual.index.root().nfiles(), 15);
    for i in 1..6 {
        assert!(
            actual
                .index
                .find(Path::new(&format!(r"C:\seg{i}.log")))
                .is_none()
        );
    }
    assert_eq!(
        file(&scan(&image, true).unwrap().index, "seg0.log").0,
        4_494_815
    );
}

#[test]
fn sparse_and_ordinary_extents_share_merged_stream_flags() {
    for sparse_flag_on_head in [true, false] {
        let image = TempImage::new(&mixed_sparse_flags_image(sparse_flag_on_head));
        let actual = scan(&image, false).unwrap();
        assert_eq!(
            file(&actual.index, "mixed-sparse.bin").0,
            2 * CLUSTER as u64
        );
        assert_eq!(
            file(&scan(&image, true).unwrap().index, "mixed-sparse.bin").0,
            4 * CLUSTER as u64
        );
    }
}

fn assert_record_anomaly(image: &TempImage) {
    let result = scan(image, false).expect("单记录异常不能让整卷失败");
    assert!(result.anomalies > 0, "损坏记录必须计数");
    assert_eq!(file(&result.index, "streams.bin").0, 4112);
    assert_eq!(file(&result.index, "fragment-two.txt").0, 8);
    assert!(result.index.root().nfiles() > 1);
}

#[test]
fn record_fixup_attribute_and_size_damage_are_isolated() {
    let original = small_image(false);
    let mut fixup = original.clone();
    fixup[record_offset(18) + SECTOR - 1] ^= 1;
    assert_record_anomaly(&TempImage::new(&fixup));
    let mut zero_length = original.clone();
    put32(&mut zero_length, record_offset(18) + 56 + 4, 0);
    assert_record_anomaly(&TempImage::new(&zero_length));
    let mut size_mismatch = original;
    let mut invalid_data = data(2, 6000, 48, 2);
    put64(&mut invalid_data, 40, u64::MAX);
    set_record(
        &mut size_mismatch,
        19,
        &file_record(
            19,
            false,
            0,
            &[
                si(0),
                filename(1, reference(16), "large.bin", 1),
                invalid_data,
            ],
        ),
    );
    assert_record_anomaly(&TempImage::new(&size_mismatch));
}

#[test]
fn short_exported_image_is_valid_until_a_required_read_reaches_eof() {
    let bytes = small_image(false);
    let full = scan(&TempImage::new(&bytes), false).unwrap();
    for length in [VOLUME_BYTES - 7 * SECTOR, 32 * CLUSTER] {
        let short = scan(&TempImage::new(&bytes[..length]), false).unwrap();
        assert_eq!(short.index.root().size(), full.index.root().size());
        assert_eq!(short.index.root().nfiles(), full.index.root().nfiles());
        assert_eq!(short.records, full.records);
        assert_eq!(short.anomalies, 0);
    }
    // 第二段MFT运行列表指向镜像真正的末尾之外，不能用全零假数据代替。
    assert!(scan(&TempImage::new(&bytes[..32 * CLUSTER - 1]), false).is_err());
    // 非驻留属性列表的内容确实需要读取；读到真实EOF也应报告I/O错误。
    let bytes = small_image(true);
    assert!(scan(&TempImage::new(&bytes[..100 * CLUSTER + 1]), false).is_err());
}

#[test]
fn index_metadata_and_file_symlinks_without_data_are_valid_zero_sized_files() {
    let bytes = files_without_data_image();
    let image = TempImage::new(&bytes);
    for apparent in [false, true] {
        let result = scan(&image, apparent).unwrap();
        assert_eq!(result.anomalies, 0);
        assert_eq!(result.index.root().nfiles(), 17);
        for name in ["$Quota", "$ObjId", "$Reparse"] {
            assert_eq!(file(&result.index, name), (0, Category::Core));
            assert!(
                result
                    .index
                    .find(Path::new(&format!(r"C:\$Extend\{name}")))
                    .is_none()
            );
        }
        assert_eq!(file(&result.index, "file-link").0, 0);
        assert_eq!(
            file(&result.index, r"a\tiny.txt").0,
            if apparent { 3 } else { 8 }
        );
        assert_eq!(
            result.index.root().size(),
            if apparent { 162_711 } else { 98_344 }
        );
    }
    assert_eq!(std::fs::read(&image.path).unwrap(), bytes);
}

#[test]
fn invalid_index_or_reparse_metadata_does_not_allow_missing_data() {
    for attrs in [
        vec![
            si(0),
            filename(1, reference(5), "missing-data.bin", 1),
            resident(0x90, 2, "$O", &[0; 31]),
        ],
        vec![si(0x400), filename(1, reference(5), "missing-data.bin", 1)],
    ] {
        let mut bytes = small_image(false);
        set_record(&mut bytes, 40, &file_record(40, false, 0, &attrs));
        let result = scan(&TempImage::new(&bytes), false).unwrap();
        assert_eq!(result.anomalies, 1);
        assert!(
            result
                .index
                .find(Path::new(r"C:\missing-data.bin"))
                .is_none()
        );
        assert_eq!(file(&result.index, "streams.bin").0, 4112);
    }
}

#[test]
fn missing_data_is_reported_instead_of_publishing_a_zero_sized_live_file() {
    let mut bytes = small_image(false);
    set_record(
        &mut bytes,
        40,
        &file_record(
            40,
            false,
            0,
            &[si(0), filename(1, reference(5), "missing-data.bin", 1)],
        ),
    );
    let result = scan(&TempImage::new(&bytes), false).unwrap();
    assert!(result.anomalies > 0);
    assert!(
        result
            .index
            .find(Path::new(r"C:\missing-data.bin"))
            .is_none()
    );
    assert_eq!(file(&result.index, "streams.bin").0, 4112);
}
