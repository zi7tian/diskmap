use super::*;

const CLASS: DirClass = (Category::Other, ReasonId(130));

fn encode(index: &Index) -> Vec<u8> {
    let mut bytes = Vec::new();
    index.write_encoded(&mut bytes).unwrap();
    assert_eq!(bytes.len() as u64, index.encoded_len());
    bytes
}

fn decode(index: &Index, bytes: &[u8]) -> io::Result<Index> {
    Index::read_encoded(
        bytes,
        index.encoded_len(),
        index.os,
        index.root_path.clone(),
    )
}

fn round_trip(index: &Index) -> Index {
    let restored = decode(index, &encode(index)).unwrap();
    // Debug 包含原始数组和每个字段，包括公开视图隐藏的硬链接和内部标志。
    assert_eq!(format!("{restored:?}"), format!("{index:?}"));
    restored
}

fn directory(name: &str, flags: u8) -> NewDir<'_> {
    NewDir {
        name: name.as_ref(),
        class: CLASS,
        flags,
    }
}

fn file(name: &OsStr, linked: bool) -> NewFile<'_> {
    NewFile {
        name,
        size: 4096,
        category: Category::User,
        linked,
    }
}

fn interleaved() -> Index {
    let mut index = Index::new(Os::Windows, Path::new("C:\\"), CLASS);
    let root = index
        .fill_dir(
            DirId::ROOT,
            &[
                directory("a", 0),
                directory("Denied", DENIED),
                directory("ÜBER", MOUNT),
            ],
            &[file("First".as_ref(), true), file("z.txt".as_ref(), false)],
        )
        .unwrap();
    let [a, denied, mount] = std::array::from_fn(|i| DirId(root.dirs.start + i as u32));
    // 模拟多个扫描线程逆编号完成。两个非空段还有相同的子目录起始编号。
    index
        .fill_dir(mount, &[], &[file("Alias".as_ref(), true)])
        .unwrap();
    index.fill_dir(denied, &[], &[]).unwrap();
    index.fill_dir(a, &[directory("Later", 0)], &[]).unwrap();
    assert!(index.count_file(DirId::ROOT, FileId(root.files.start)));
    index
}

#[test]
fn preserves_windows_lookup_ranges_flags_links_and_empty_directories() {
    let index = interleaved();
    let restored = round_trip(&index);
    for path in [
        "c:/FIRST",
        "c:/über",
        "C:\\a\\Later",
        "C:/ÜBER/ALIAS",
        "c:/denied",
    ] {
        assert_eq!(restored.find(Path::new(path)), index.find(Path::new(path)));
    }
    assert_eq!(restored.find(Path::new("C:/ÜBER/ALIAS")), None);
    assert_eq!(restored.dir(DirId(2)).flags(), DENIED);
    assert!(restored.dir(DirId(2)).is_listed());
    assert_eq!(restored.dir(DirId(3)).flags(), MOUNT);
    assert!(!restored.dir(DirId(4)).is_listed());
    assert_eq!((restored.root().nfiles(), restored.file_count()), (2, 3));
}

#[test]
fn preserves_root_only_and_deep_indexes() {
    let mut index = Index::new(Os::Linux, Path::new("/r"), CLASS);
    round_trip(&index);
    let mut id = DirId::ROOT;
    for _ in 0..1024 {
        id = DirId(
            index
                .fill_dir(id, &[directory("d", 0)], &[])
                .unwrap()
                .dirs
                .start,
        );
    }
    index
        .fill_dir(id, &[], &[file("last".as_ref(), false)])
        .unwrap();
    let loaded = round_trip(&index);
    let path = index.dir_path(id);
    assert_eq!(loaded.dir_path(id), path);
    assert_eq!(loaded.find(&path), Some(Found::Dir(id)));
}

#[cfg(unix)]
#[test]
fn preserves_non_utf8_names_and_root_bytes() {
    use std::os::unix::ffi::OsStrExt;
    let root = Path::new(OsStr::from_bytes(b"/r\xff"));
    let name = OsStr::from_bytes(b"caf\xe9");
    let mut index = Index::new(Os::Linux, root, CLASS);
    index
        .fill_dir(DirId::ROOT, &[], &[file(name, false)])
        .unwrap();
    let loaded = round_trip(&index);
    assert_eq!(loaded.file(FileId(0)).name(), name);
    assert!(matches!(
        loaded.find(&root.join(name)),
        Some(Found::File { .. })
    ));
}

#[cfg(windows)]
#[test]
fn preserves_unpaired_utf16_surrogates_and_rejects_malformed_encoding() {
    use std::os::windows::ffi::OsStringExt;
    let name = OsString::from_wide(&[b'a' as u16, 0xd800, b'b' as u16, 0xdfff]);
    let mut index = Index::new(Os::Windows, Path::new("C:\\"), CLASS);
    index
        .fill_dir(DirId::ROOT, &[], &[file(&name, false)])
        .unwrap();
    assert_eq!(round_trip(&index).file(FileId(0)).name(), name);
    for invalid in [
        vec![0xff],
        vec![0xed, 0xa0],
        vec![0xed, 0xa0, 0x80, 0xed, 0xb0, 0x80],
    ] {
        assert!(validate_windows_name(&invalid).is_err());
    }
}

#[test]
fn rejects_invalid_counts_offsets_parents_flags_and_totals() {
    let index = interleaved();
    let encoded = encode(&index);
    let root = COUNTS_BYTES;
    let child = root + DIR_BYTES;
    let mount = root + 3 * DIR_BYTES;
    let first_file = root + index.dir_count() * DIR_BYTES;
    let name_start = first_file + index.file_count() * FILE_BYTES;
    for (position, value) in [
        (0, 0), // 没有根目录
        (4, u32::MAX),
        (8, u32::MAX),
        (root, 1),
        (root + 20, u32::MAX), // 子目录末尾溢出
        (root + 28, u32::MAX),
        (root + 32, 0),  // 计入文件数量错误
        (child + 12, 1), // 自己是自己的父目录
        (child, u32::MAX),
        (mount + 24, 1), // 文件段与根目录重叠
        (first_file + 8, 0),
    ] {
        let mut bytes = encoded.clone();
        bytes[position..position + 4].copy_from_slice(&value.to_le_bytes());
        assert!(decode(&index, &bytes).is_err(), "偏移 {position}");
    }
    for (position, value) in [
        (root + 7, 0xff),
        (first_file + 14, 0xff),
        (first_file + 15, 0xff),
        (name_start, b'?'),
        (name_start + 3 + "aDeniedÜBER".len(), b'z'), // 破坏同目录文件的名字顺序
    ] {
        let mut bytes = encoded.clone();
        bytes[position] = value;
        assert!(decode(&index, &bytes).is_err(), "偏移 {position}");
    }
    for n in [0, root, child, first_file, name_start, encoded.len() - 1] {
        assert!(decode(&index, &encoded[..n]).is_err());
    }
}
