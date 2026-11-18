use super::*;

#[test]
fn windows_difficult_equivalence_classes() {
    let groups: &[&[&[u8]]] = &[
        &[b"File.ExE", b"FILE.exe"],
        &["Σ".as_bytes(), "σ".as_bytes(), "ς".as_bytes()],
        &["ΟΣ".as_bytes(), "οσ".as_bytes(), "ος".as_bytes()],
        &[b"I", b"i", "ı".as_bytes()],
        &["İ".as_bytes()],
        &["i\u{307}".as_bytes()],
        &["ß".as_bytes()],
        &["ẞ".as_bytes()],
        &["ﬀ".as_bytes()],
        &[b"FF", b"ff"],
        &["Ａ".as_bytes(), "ａ".as_bytes()],
        &["Ꭰ".as_bytes(), "ꭰ".as_bytes()],
        &["ᾀ".as_bytes(), "ᾈ".as_bytes()],
        &["𐐀".as_bytes()],
        &["𐐨".as_bytes()],
        &[b"\xed\xa0\x80A", b"\xed\xa0\x80a"],
        &[b"\xed\xa0\x81A"],
        &[b"\xed\xb0\x80A"],
        &["�A".as_bytes()],
        &[b"\xffA", b"\xffa"],
        &[b"\xfeA"],
    ];
    for (i, left) in groups.iter().enumerate() {
        for (j, right) in groups.iter().enumerate() {
            for a in *left {
                for b in *right {
                    let ka = name_key(Os::Windows, a);
                    let kb = name_key(Os::Windows, b);
                    assert_eq!(ka == kb, i == j, "{a:?} / {b:?}");
                    assert_eq!(cmp_names(Os::Windows, a, b), ka.cmp(&kb));
                }
            }
        }
    }
}

#[test]
fn every_bmp_code_unit_matches_simple_uppercase_and_is_idempotent() {
    let mut pair = super::super::upcase::MAPPINGS.iter().peekable();
    for unit in 0..=u16::MAX {
        let expected = if pair.peek().is_some_and(|&&(from, _)| from == unit) {
            pair.next().unwrap().1
        } else {
            unit
        };
        let encode = |unit: u16| {
            if let Some(c) = char::from_u32(unit as u32) {
                let mut bytes = [0; 4];
                c.encode_utf8(&mut bytes).as_bytes().to_vec()
            } else {
                vec![
                    0xe0 | (unit >> 12) as u8,
                    0x80 | ((unit >> 6) & 63) as u8,
                    0x80 | (unit & 63) as u8,
                ]
            }
        };
        let input = encode(unit);
        let expected = encode(expected);
        assert_eq!(name_key(Os::Windows, &input), expected, "U+{unit:04X}");
        assert_eq!(cmp_names(Os::Windows, &input, &expected), Ordering::Equal);
        assert_eq!(name_key(Os::Windows, &expected), expected);
        assert_eq!(path_key(Os::Linux, &input), input);
        assert_eq!(path_key(Os::Macos, &input), input);
    }
}

#[test]
fn path_and_name_separators_and_invalid_encodings() {
    let cases: &[&[u8]] = &[
        b"C:/MiXeD/",
        b"c:\\mixed\\",
        b"C:/.//..\\",
        b"A\xc0\xafa",
        b"A\xe0\x80\xafa",
        b"A\xed\xa0\x80a",
        b"A\xe2a",
        b"A\xf0\x90\x90\x80a",
        b"A\xffa",
        b"",
        "C:/ᾀ/ı/ﬀ".as_bytes(),
        "C:\\ᾈ\\I\\ﬀ".as_bytes(),
    ];
    for os in [Os::Windows, Os::Linux, Os::Macos] {
        for a in cases {
            let key = path_key(os, a);
            for b in cases {
                assert_eq!(cmp_paths(os, a, b), key.cmp(&path_key(os, b)));
                assert_eq!(cmp_path_key(os, &key, b), cmp_paths(os, a, b));
                assert_eq!(cmp_names(os, a, b), name_key(os, a).cmp(&name_key(os, b)));
            }
        }
    }
    assert_eq!(cmp_paths(Os::Windows, cases[0], cases[1]), Ordering::Equal);
    assert_ne!(cmp_names(Os::Windows, cases[0], cases[1]), Ordering::Equal);
    assert_ne!(cmp_names(Os::Macos, b"File", b"file"), Ordering::Equal);
}

/// 原 index 比较器与 view 名字键，保留作 T15 前后的可复现基准，不用于生产。
/// 两边都生成独立字节键，测折叠算法；不包含 NTFS 连续缓冲复用与原名复用的差异。
fn previous_compare(a: &[u8], b: &[u8]) -> Ordering {
    if a.is_ascii() && b.is_ascii() {
        return a
            .iter()
            .map(u8::to_ascii_lowercase)
            .cmp(b.iter().map(u8::to_ascii_lowercase));
    }
    String::from_utf8_lossy(a)
        .chars()
        .flat_map(char::to_lowercase)
        .cmp(
            String::from_utf8_lossy(b)
                .chars()
                .flat_map(char::to_lowercase),
        )
}

fn previous_key(input: &[u8]) -> Vec<u8> {
    if input.is_ascii() {
        input.iter().map(u8::to_ascii_lowercase).collect()
    } else {
        String::from_utf8_lossy(input)
            .chars()
            .flat_map(char::to_lowercase)
            .collect::<String>()
            .into_bytes()
    }
}

#[test]
#[ignore = "百万名字的发布版性能基准，手动运行"]
fn million_names_benchmark() {
    use std::hint::black_box;
    use std::time::Instant;
    const COUNT: usize = 1_000_000;
    for unicode in [false, true] {
        let prefixes = [
            "Report", "photo", "CACHE", "File", "data", "Ος", "ıİ", "ꭰᎠ", "ａｂ", "ᾀßﬀ",
        ];
        let names: Vec<_> = (0..COUNT)
            .map(|i| {
                let n = (i as u64)
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let prefix = prefixes[if unicode { i % prefixes.len() } else { i % 5 }];
                format!("{prefix}-{n:016x}.DaT").into_bytes()
            })
            .collect();
        let mut sorts = [Vec::new(), Vec::new()];
        let mut keys = [Vec::new(), Vec::new()];
        for round in 0..6 {
            for which in [round % 2, 1 - round % 2] {
                let mut refs: Vec<_> = names.iter().map(Vec::as_slice).collect();
                let start = Instant::now();
                if which == 0 {
                    refs.sort_unstable_by(|a, b| previous_compare(a, b));
                } else {
                    refs.sort_unstable_by(|a, b| cmp_names(Os::Windows, a, b));
                }
                let sort_time = start.elapsed().as_secs_f64();
                black_box(refs);
                let start = Instant::now();
                let built: Vec<_> = if which == 0 {
                    names.iter().map(|n| previous_key(n)).collect()
                } else {
                    names.iter().map(|n| name_key(Os::Windows, n)).collect()
                };
                let key_time = start.elapsed().as_secs_f64();
                black_box(built);
                if round > 0 {
                    sorts[which].push(sort_time);
                    keys[which].push(key_time);
                }
            }
        }
        for times in sorts.iter_mut().chain(keys.iter_mut()) {
            times.sort_by(f64::total_cmp);
        }
        println!(
            "names={COUNT} unicode={unicode} median(5) sort_before={:.6}s sort_after={:.6}s ratio={:.3} key_before={:.6}s key_after={:.6}s ratio={:.3}",
            sorts[0][2],
            sorts[1][2],
            sorts[1][2] / sorts[0][2],
            keys[0][2],
            keys[1][2],
            keys[1][2] / keys[0][2]
        );
    }
}
