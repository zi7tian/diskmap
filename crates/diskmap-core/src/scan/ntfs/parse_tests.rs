use super::*;

fn put16(data: &mut [u8], offset: usize, n: u16) {
    data[offset..offset + 2].copy_from_slice(&n.to_le_bytes());
}

fn put32(data: &mut [u8], offset: usize, n: u32) {
    data[offset..offset + 4].copy_from_slice(&n.to_le_bytes());
}

fn put64(data: &mut [u8], offset: usize, n: u64) {
    data[offset..offset + 8].copy_from_slice(&n.to_le_bytes());
}

fn boot() -> Vec<u8> {
    let mut data = vec![0; 512];
    data[3..11].copy_from_slice(b"NTFS    ");
    put16(&mut data, 11, 512);
    data[13] = 8;
    put64(&mut data, 40, 8192);
    put64(&mut data, 48, 4);
    data[64] = (-10i8) as u8;
    data[510..512].copy_from_slice(&[0x55, 0xaa]);
    data
}

fn resident(ty: u32, id: u16, name: &[u16], value: &[u8]) -> Vec<u8> {
    let offset = (24 + name.len() * 2).next_multiple_of(8);
    let len = (offset + value.len()).next_multiple_of(8);
    let mut data = vec![0; len];
    put32(&mut data, 0, ty);
    put32(&mut data, 4, len as u32);
    data[9] = name.len() as u8;
    put16(&mut data, 10, 24);
    put16(&mut data, 14, id);
    put32(&mut data, 16, value.len() as u32);
    put16(&mut data, 20, offset as u16);
    for (i, n) in name.iter().enumerate() {
        put16(&mut data, 24 + 2 * i, *n);
    }
    data[offset..offset + value.len()].copy_from_slice(value);
    data
}

fn nonresident(low: u64, high: u64, flags: u16, runs: &[u8]) -> Vec<u8> {
    let offset = if low == 0 && flags & 0x8001 != 0 {
        72
    } else {
        64
    };
    let len = (offset + runs.len()).next_multiple_of(8);
    let mut data = vec![0; len];
    put32(&mut data, 0, 0x80);
    put32(&mut data, 4, len as u32);
    data[8] = 1;
    put16(&mut data, 12, flags);
    put64(&mut data, 16, low);
    put64(&mut data, 24, high);
    put16(&mut data, 32, offset as u16);
    data[offset..offset + runs.len()].copy_from_slice(runs);
    data
}

fn record(attrs: &[Vec<u8>]) -> Vec<u8> {
    let mut data = vec![0; 1024];
    data[..4].copy_from_slice(b"FILE");
    put16(&mut data, 4, 48);
    put16(&mut data, 6, 3);
    put16(&mut data, 16, 7);
    put16(&mut data, 18, 1);
    put16(&mut data, 20, 56);
    put16(&mut data, 22, 1);
    put32(&mut data, 28, 1024);
    let mut off = 56;
    for attribute in attrs {
        data[off..off + attribute.len()].copy_from_slice(attribute);
        off += attribute.len();
    }
    put32(&mut data, off, u32::MAX);
    put32(&mut data, 24, (off + 8) as u32);
    put16(&mut data, 48, 0xabcd);
    for i in 1..=2 {
        let old = u16_at(&data, 512 * i - 2).unwrap();
        put16(&mut data, 48 + 2 * i, old);
        put16(&mut data, 512 * i - 2, 0xabcd);
    }
    data
}

#[test]
fn boot_geometry_and_signed_sizes() {
    let mut b = boot();
    assert_eq!(
        Boot::parse(&b).unwrap(),
        Boot {
            sector_size: 512,
            cluster_size: 4096,
            record_size: 1024,
            mft_offset: 16384,
            volume_size: 4194304,
        }
    );
    b[64] = 1;
    assert_eq!(Boot::parse(&b).unwrap().record_size, 4096);
    b[13] = 0xf4;
    put64(&mut b, 40, 65536);
    b[64] = (-12i8) as u8;
    assert_eq!(Boot::parse(&b).unwrap().cluster_size, 2 * 1024 * 1024);
    b[13] = 3;
    assert!(Boot::parse(&b).is_err());
    b[13] = 8;
    b[64] = 0x80;
    assert!(Boot::parse(&b).is_err());
    b[64] = (-10i8) as u8;
    put64(&mut b, 48, u64::MAX);
    assert!(Boot::parse(&b).is_err());
    put64(&mut b, 48, 4);
    put64(&mut b, 40, u64::MAX);
    assert!(Boot::parse(&b).is_err());
}

#[test]
fn fixup_restores_data_and_checks_every_word() {
    let content = vec![0x71; 600];
    let original = record(&[resident(0x80, 4, &[0x6e], &content)]);
    let mut bytes = original.clone();
    let parsed = RawRecord::parse(&mut bytes, 4096).unwrap();
    assert_eq!(parsed.sequence, 7);
    assert_eq!(parsed.flags, 1);
    assert_eq!(parsed.base_reference, 0);
    assert_eq!(parsed.attrs[0].id, 4);
    assert_eq!(parsed.attrs[0].ty, 0x80);
    assert_eq!(parsed.attrs[0].name, [0x6e]);
    assert_eq!(parsed.attrs[0].flags, 0);
    let Value::Resident(value) = &parsed.attrs[0].value else {
        panic!("resident")
    };
    assert_eq!(value, &content);
    for pos in [510, 511, 1022, 1023] {
        let mut broken = original.clone();
        broken[pos] ^= 1;
        let unchanged = broken.clone();
        assert!(RawRecord::parse(&mut broken, 512).is_err());
        assert_eq!(broken, unchanged);
    }
    assert!(RawRecord::parse(&mut bytes, 512).is_err());
}

#[test]
fn reused_records_retain_capacity_and_replace_all_visible_fields() {
    let first = [
        resident(0x10, 0, &[], &[0x11; 72]),
        resident(0x80, 1, &[0x61, 0x62], &[0x22; 32]),
        nonresident(
            0,
            3,
            0,
            &[0x11, 1, 10, 0x11, 1, 1, 0x11, 1, 1, 0x11, 1, 1, 0],
        ),
    ];
    let mut reused = RawRecord::default();
    reused.parse_reuse(&mut record(&first), 512).unwrap();
    let attrs = reused.attrs.as_ptr();
    let name = reused.attrs[1].name.as_ptr();
    let Value::Resident(value) = &reused.attrs[1].value else {
        panic!("resident");
    };
    let resident_buffer = value.as_ptr();
    let Value::NonResident(value) = &reused.attrs[2].value else {
        panic!("nonresident");
    };
    let runs = value.runs.as_ptr();

    let second = [
        resident(0x10, 10, &[], &[0x33; 48]),
        resident(0x80, 11, &[0x63], b"new"),
        nonresident(0, 3, 0x8000, &[0x11, 2, 30, 0x01, 2, 0]),
    ];
    let mut bytes = record(&second);
    put16(&mut bytes, 16, 9);
    put16(&mut bytes, 22, 3);
    put64(&mut bytes, 32, 0x1234);
    reused.parse_reuse(&mut bytes, 4096).unwrap();
    assert_eq!(reused.attrs.as_ptr(), attrs);
    assert_eq!(reused.sequence, 9);
    assert_eq!(reused.flags, 3);
    assert_eq!(reused.base_reference, 0x1234);
    assert_eq!(reused.attrs[1].id, 11);
    assert_eq!(reused.attrs[1].name, [0x63]);
    assert_eq!(reused.attrs[1].name.as_ptr(), name);
    let Value::Resident(value) = &reused.attrs[1].value else {
        panic!("resident");
    };
    assert_eq!(value, b"new");
    assert_eq!(value.as_ptr(), resident_buffer);
    let Value::NonResident(value) = &reused.attrs[2].value else {
        panic!("nonresident");
    };
    assert_eq!(value.runs.len(), 2);
    assert_eq!(value.runs[0].lcn, Some(30));
    assert_eq!(value.runs[1].lcn, None);
    assert_eq!(value.runs.as_ptr(), runs);
    assert_eq!(value.compressed_size, Some(0));

    // 第三个属性坏掉时，只能看到这次解析成功的前两个属性，不能泄漏旧的第三个。
    let mut broken = record(&second);
    put16(
        &mut broken,
        56 + second[0].len() + second[1].len() + 12,
        0x100,
    );
    assert!(reused.parse_reuse(&mut broken, 512).is_err());
    assert_eq!(reused.attrs.len(), 2);
    assert_eq!(reused.sequence, 0);
    assert_eq!(reused.flags, 0);
    assert_eq!(reused.base_reference, 0);
    assert_eq!(reused.attrs[1].id, 11);
    assert_eq!(reused.attrs[1].name, [0x63]);

    // 下一条变为更短的记录以及不同驻留形式，必须清掉此前的名字和所有多余属性。
    reused
        .parse_reuse(&mut record(&[nonresident(0, 1, 0, &[0x11, 2, 45, 0])]), 512)
        .unwrap();
    assert_eq!(reused.attrs.len(), 1);
    assert!(reused.attrs[0].name.is_empty());
    assert!(matches!(reused.attrs[0].value, Value::NonResident(_)));
    reused
        .parse_reuse(&mut record(&[resident(0x80, 19, &[], b"ok")]), 512)
        .unwrap();
    assert_eq!(reused.attrs.len(), 1);
    assert_eq!(reused.attrs[0].id, 19);
    assert_eq!(reused.attrs[0].flags, 0);
    let Value::Resident(value) = &reused.attrs[0].value else {
        panic!("resident");
    };
    assert_eq!(value, b"ok");

    let mut broken = record(&first);
    broken[1022] ^= 1;
    let original = broken.clone();
    assert!(reused.parse_reuse(&mut broken, 512).is_err());
    assert_eq!(broken, original);
    assert!(reused.attrs.is_empty());
    reused.parse_reuse(&mut record(&first), 512).unwrap();
    assert_eq!(reused.attrs.len(), 3);
}

#[test]
fn reused_file_names_keep_capacity_but_never_stale_names() {
    let mut bytes = vec![0; 74];
    put64(&mut bytes, 0, 42);
    bytes[64] = 4;
    bytes[65] = 3;
    for (i, unit) in [0xd800, 0x61, 0x62, 0x63].into_iter().enumerate() {
        put16(&mut bytes, 66 + i * 2, unit);
    }
    let mut name = FileName::default();
    parse_file_name_into(&bytes, &mut name).unwrap();
    let buffer = name.name.as_ptr();
    assert_eq!(name.name, [0xd800, 0x61, 0x62, 0x63]);
    bytes[64] = 1;
    bytes[65] = 1;
    put64(&mut bytes, 0, 99);
    put16(&mut bytes, 66, 0x7a);
    parse_file_name_into(&bytes, &mut name).unwrap();
    assert_eq!(name.name.as_ptr(), buffer);
    assert_eq!(name.name, [0x7a]);
    assert_eq!(name.parent_reference, 99);
    assert_eq!(name.namespace, 1);
    assert!(parse_file_name_into(&bytes[..65], &mut name).is_err());
    assert!(name.name.is_empty());
    assert_eq!(name.parent_reference, 0);
    bytes[65] = 5;
    assert!(parse_file_name_into(&bytes, &mut name).is_err());
    assert!(name.name.is_empty());
    bytes[65] = 1;
    parse_file_name_into(&bytes, &mut name).unwrap();
    assert_eq!(name.name, [0x7a]);
    assert_eq!(name.name.as_ptr(), buffer);
}

#[test]
fn malformed_record_regions_are_rejected() {
    let valid = record(&[resident(0x80, 0, &[], &[1, 2, 3])]);
    for (off, value) in [(4, 510), (6, 2), (20, 48), (20, 57)] {
        let mut malformed = valid.clone();
        put16(&mut malformed, off, value);
        assert!(RawRecord::parse(&mut malformed, 512).is_err());
    }
    for (off, value) in [(24, 2048), (28, 512), (60, 0), (60, 25), (60, u32::MAX)] {
        let mut malformed = valid.clone();
        put32(&mut malformed, off, value);
        assert!(RawRecord::parse(&mut malformed, 512).is_err());
    }
}

#[test]
fn mapping_pairs_handle_signed_deltas_and_sparse_runs() {
    let runs = parse_runs(
        &[0x11, 4, 100, 0x11, 2, 0xfd, 0x01, 3, 0x11, 1, 2, 0],
        8,
        17,
    )
    .unwrap();
    assert_eq!(
        runs,
        [
            Run {
                vcn: 8,
                len: 4,
                lcn: Some(100)
            },
            Run {
                vcn: 12,
                len: 2,
                lcn: Some(97)
            },
            Run {
                vcn: 14,
                len: 3,
                lcn: None
            },
            Run {
                vcn: 17,
                len: 1,
                lcn: Some(99)
            },
        ]
    );
    // 8 字节的有符号偏移也应正确解码，不能错误地移位 64 位。
    let mut bytes = vec![0x81, 1];
    bytes.extend(100i64.to_le_bytes());
    bytes.extend([0x81, 1]);
    bytes.extend((-90i64).to_le_bytes());
    bytes.push(0);
    let runs = parse_runs(&bytes, 0, 1).unwrap();
    assert_eq!(runs[1].lcn, Some(10));
}

#[test]
fn invalid_mapping_pairs_never_wrap_or_stall() {
    for bytes in [
        vec![],
        vec![0x10, 1, 0],
        vec![0x19, 1, 0],
        vec![0x91, 1, 0],
        vec![0x11, 0, 1, 0],
        vec![0x11, 1, 0xff, 0],
        vec![0x11, 1, 1],
        vec![0x11, 2, 1, 0],
        vec![0x18, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 1, 0],
    ] {
        assert!(parse_runs(&bytes, 0, 0).is_err(), "{bytes:?}");
    }
    assert!(parse_runs(&[0x11, 1, 1, 0], i64::MAX as u64, i64::MAX as u64).is_err());
    assert!(parse_runs(&[0], 5, 4).is_err());
    assert!(parse_runs(&[0], 0, u64::MAX).unwrap().is_empty());
    assert!(parse_runs(&[0], 0, 0).unwrap().is_empty());
}

#[test]
fn nonresident_sparse_and_extension_headers() {
    let mut compressed = nonresident(0, 3, 1, &[0x11, 2, 100, 0x01, 2, 0]);
    put64(&mut compressed, 40, 16384);
    put64(&mut compressed, 48, 16000);
    put64(&mut compressed, 56, 15000);
    put64(&mut compressed, 64, 8192);
    let extension = nonresident(4, 5, 1, &[0x11, 2, 110, 0]);
    let mut rec = record(&[compressed, extension]);
    let parsed = RawRecord::parse(&mut rec, 512).unwrap();
    let Value::NonResident(first) = &parsed.attrs[0].value else {
        panic!("nonresident")
    };
    assert_eq!(first.lowest_vcn, 0);
    assert_eq!(first.highest_vcn, 3);
    assert_eq!(first.allocated, 16384);
    assert_eq!(first.data_size, 16000);
    assert_eq!(first.initialized, 15000);
    assert_eq!(first.compressed_size, Some(8192));
    assert_eq!(first.runs.len(), 2);
    let Value::NonResident(next) = &parsed.attrs[1].value else {
        panic!("nonresident")
    };
    assert_eq!(next.compressed_size, None);
    assert_eq!(next.lowest_vcn, 4);
}

#[test]
fn attribute_name_and_content_cannot_overlap() {
    let mut attribute = resident(0x80, 0, &[0x61], &[0x42]);
    put16(&mut attribute, 10, 32);
    assert!(parse_attribute(&attribute, 0x80).is_err());
    let mut attribute = nonresident(0, 0, 0, &[0x11, 1, 100, 0]);
    attribute[9] = 1;
    put16(&mut attribute, 10, 48);
    assert!(parse_attribute(&attribute, 0x80).is_err());
    put16(&mut attribute, 10, 64);
    assert!(parse_attribute(&attribute, 0x80).is_err());
}

#[test]
fn unknown_flags_and_invalid_compression_units_are_rejected() {
    let mut attribute = nonresident(0, 0, 0, &[0x11, 1, 100, 0]);
    put16(&mut attribute, 12, 2);
    assert!(parse_attribute(&attribute, 0x80).is_err());
    put16(&mut attribute, 12, 0);
    put16(&mut attribute, 34, 1);
    assert!(parse_attribute(&attribute, 0x80).is_err());
    let mut compressed = nonresident(0, 0, 1, &[0x11, 1, 100, 0]);
    for unit in [0, 4, 16] {
        put16(&mut compressed, 34, unit);
        assert!(parse_attribute(&compressed, 0x80).is_ok());
    }
    for unit in [17, u16::MAX] {
        put16(&mut compressed, 34, unit);
        assert!(parse_attribute(&compressed, 0x80).is_err());
    }
    let mut resident = resident(0x80, 0, &[], &[1]);
    put16(&mut resident, 12, 0x0100);
    assert!(parse_attribute(&resident, 0x80).is_err());
}

#[test]
fn list_entry_references_and_malformed_lengths() {
    let mut data = vec![0; 32];
    put32(&mut data, 0, 0x80);
    put16(&mut data, 4, 32);
    data[6] = 2;
    data[7] = 26;
    put64(&mut data, 8, 123);
    put64(&mut data, 16, (7u64 << 48) | 42);
    put16(&mut data, 24, 9);
    put16(&mut data, 26, 0xd800);
    put16(&mut data, 28, 0x61);
    let parsed = parse_attribute_list(&data).unwrap();
    assert_eq!(
        parsed,
        [ListEntry {
            ty: 0x80,
            id: 9,
            name: vec![0xd800, 0x61],
            lowest_vcn: 123,
            reference: (7u64 << 48) | 42
        }]
    );
    for len in [0, 24, 26, 40, 65535] {
        let mut broken = data.clone();
        put16(&mut broken, 4, len);
        assert!(parse_attribute_list(&broken).is_err());
    }
    data[6] = 255;
    assert!(parse_attribute_list(&data).is_err());
}

#[test]
fn file_names_standard_information_and_reparse_points() {
    let mut si = vec![0; 72];
    put32(&mut si, 32, 0x400410);
    assert_eq!(parse_standard_information(&si).unwrap(), 0x400410);
    assert!(parse_standard_information(&si[..47]).is_err());
    let mut name = vec![0; 70];
    put64(&mut name, 0, (1u64 << 48) | 5);
    name[64] = 2;
    name[65] = 3;
    put16(&mut name, 66, 0xd83d);
    put16(&mut name, 68, 0xde00);
    let parsed = parse_file_name(&name).unwrap();
    assert_eq!(parsed.parent_reference, (1u64 << 48) | 5);
    assert_eq!(parsed.name, [0xd83d, 0xde00]);
    assert_eq!(parsed.namespace, 3);
    for invalid in [0, 0x2f, 0x5c] {
        let mut broken = name.clone();
        put16(&mut broken, 66, invalid);
        assert!(parse_file_name(&broken).is_err());
    }
    name[65] = 4;
    assert!(parse_file_name(&name).is_err());
    let mut reparse = vec![0; 16];
    put32(&mut reparse, 0, 0xa000000c);
    put16(&mut reparse, 4, 8);
    assert_eq!(parse_reparse_tag(&reparse).unwrap(), 0xa000000c);
    assert!(parse_reparse_tag(&reparse[..15]).is_err());
}

#[test]
fn truncated_and_mutated_input_does_not_panic() {
    let base = record(&[
        resident(0x10, 0, &[], &[0; 72]),
        resident(0x30, 1, &[], &[0; 80]),
        nonresident(0, 3, 0x8000, &[0x11, 2, 100, 0x01, 2, 0]),
    ]);
    let mut reused = RawRecord::default();
    for len in 0..base.len() {
        let _ = RawRecord::parse(&mut base[..len].to_vec(), 512);
        let _ = reused.parse_reuse(&mut base[..len].to_vec(), 512);
        let _ = Boot::parse(&base[..len]);
        let _ = parse_attribute_list(&base[..len]);
        let _ = parse_file_name(&base[..len]);
        let _ = parse_standard_information(&base[..len]);
        let _ = parse_reparse_tag(&base[..len]);
    }
    // 可复现的变异覆盖不可信的尺寸、偏移、运行列表和终止符。
    let mut state = 0x34125abcu32;
    for _ in 0..20000 {
        let mut data = base.clone();
        for _ in 0..4 {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            let pos = state as usize % data.len();
            data[pos] = (state >> 24) as u8;
        }
        let _ = reused.parse_reuse(&mut data.clone(), 512);
        let _ = RawRecord::parse(&mut data, 512);
        let _ = Boot::parse(&data);
        let _ = parse_attribute_list(&data);
        let _ = parse_file_name(&data);
        let _ = parse_runs(&data[..32], 0, 3);
    }
}
