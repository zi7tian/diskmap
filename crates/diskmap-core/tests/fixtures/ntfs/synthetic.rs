//! 合成 NTFS 卷：只生成测试自己拥有的临时文件，不依赖挂载器或真实磁盘。

use std::fs::{self, File};
use std::io::{self, BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub const SECTOR: usize = 512;
pub const CLUSTER: usize = 4096;
pub const RECORD: usize = 1024;
pub const MFT_LCN: u64 = 4;
pub const RECORDS: usize = 64;
pub const VOLUME_BYTES: usize = 1024 * 1024;

static NEXT: AtomicU64 = AtomicU64::new(0);

pub struct TempImage {
    dir: PathBuf,
    pub path: PathBuf,
}

impl TempImage {
    pub fn new(bytes: &[u8]) -> Self {
        let temp = Self::empty();
        fs::write(&temp.path, bytes).unwrap();
        temp
    }

    pub fn empty() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "diskmap-ntfs-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("volume.img");
        Self { dir, path }
    }
}

impl Drop for TempImage {
    fn drop(&mut self) {
        // 只清理由此测试使用 create_dir 新建、且文件名固定的私有目录。
        let _ = fs::remove_file(&self.path);
        let _ = fs::remove_dir(&self.dir);
    }
}

pub fn put16(dst: &mut [u8], offset: usize, value: u16) {
    dst[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

pub fn put32(dst: &mut [u8], offset: usize, value: u32) {
    dst[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

pub fn put64(dst: &mut [u8], offset: usize, value: u64) {
    dst[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn align8(value: usize) -> usize {
    value.div_ceil(8) * 8
}

pub const fn reference(record: u64) -> u64 {
    record | (1 << 48)
}

fn wide(name: &str) -> Vec<u8> {
    name.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

pub fn resident(ty: u32, id: u16, name: &str, value: &[u8]) -> Vec<u8> {
    let name = wide(name);
    let value_offset = align8(24 + name.len());
    let mut out = vec![0; align8(value_offset + value.len())];
    let length = out.len();
    put32(&mut out, 0, ty);
    put32(&mut out, 4, length as u32);
    out[9] = (name.len() / 2) as u8;
    put16(&mut out, 10, if name.is_empty() { 0 } else { 24 });
    put16(&mut out, 14, id);
    put32(&mut out, 16, value.len() as u32);
    put16(&mut out, 20, value_offset as u16);
    out[24..24 + name.len()].copy_from_slice(&name);
    out[value_offset..value_offset + value.len()].copy_from_slice(value);
    out
}

/// 固定 8 字节长度/偏移编码，便于构造正、负 LCN 差值与稀疏运行。
fn runlist(runs: &[(Option<u64>, u64)]) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut last = 0i64;
    for &(lcn, len) in runs {
        bytes.push(if lcn.is_some() { 0x88 } else { 0x08 });
        bytes.extend_from_slice(&len.to_le_bytes());
        if let Some(lcn) = lcn {
            let delta = lcn as i64 - last;
            bytes.extend_from_slice(&delta.to_le_bytes());
            last = lcn as i64;
        }
    }
    bytes.push(0);
    bytes
}

pub struct Stream<'a> {
    pub ty: u32,
    pub id: u16,
    pub name: &'a str,
    pub low: u64,
    pub allocated: u64,
    pub size: u64,
    pub flags: u16,
    pub physical: u64,
    pub runs: &'a [(Option<u64>, u64)],
}

pub fn nonresident(spec: Stream<'_>) -> Vec<u8> {
    let name = wide(spec.name);
    let header = if spec.flags & 0x8001 != 0 { 72 } else { 64 };
    let run_offset = align8(header + name.len());
    let runs = runlist(spec.runs);
    let mut out = vec![0; align8(run_offset + runs.len())];
    let length = out.len();
    put32(&mut out, 0, spec.ty);
    put32(&mut out, 4, length as u32);
    out[8] = 1;
    out[9] = (name.len() / 2) as u8;
    put16(
        &mut out,
        10,
        if name.is_empty() { 0 } else { header as u16 },
    );
    put16(&mut out, 12, spec.flags);
    put16(&mut out, 14, spec.id);
    put64(&mut out, 16, spec.low);
    put64(
        &mut out,
        24,
        spec.low + spec.runs.iter().map(|r| r.1).sum::<u64>() - 1,
    );
    put16(&mut out, 32, run_offset as u16);
    out[34] = if spec.flags & 1 != 0 { 4 } else { 0 };
    put64(&mut out, 40, spec.allocated);
    put64(&mut out, 48, spec.size);
    put64(&mut out, 56, spec.size);
    if header == 72 {
        put64(&mut out, 64, spec.physical);
    }
    out[header..header + name.len()].copy_from_slice(&name);
    out[run_offset..run_offset + runs.len()].copy_from_slice(&runs);
    out
}

pub fn data(id: u16, size: u64, lcn: u64, clusters: u64) -> Vec<u8> {
    nonresident(Stream {
        ty: 0x80,
        id,
        name: "",
        low: 0,
        allocated: clusters * CLUSTER as u64,
        size,
        flags: 0,
        physical: 0,
        runs: &[(Some(lcn), clusters)],
    })
}

pub fn si(attrs: u32) -> Vec<u8> {
    let mut value = vec![0; 72];
    put32(&mut value, 32, attrs);
    resident(0x10, 0, "", &value)
}

pub fn filename(id: u16, parent: u64, name: &str, namespace: u8) -> Vec<u8> {
    let name = wide(name);
    let mut value = vec![0; 66 + name.len()];
    put64(&mut value, 0, parent);
    value[64] = (name.len() / 2) as u8;
    value[65] = namespace;
    value[66..].copy_from_slice(&name);
    resident(0x30, id, "", &value)
}

pub fn reparse(id: u16, tag: u32) -> Vec<u8> {
    let mut value = vec![0; 8];
    put32(&mut value, 0, tag);
    resident(0xc0, id, "", &value)
}

/// 一条属性列表项：type/name/lowest_vcn/reference/instance。
pub fn list_entry(ty: u32, name: &str, low: u64, target: u64, id: u16) -> Vec<u8> {
    let name = wide(name);
    let mut bytes = vec![0; align8(26 + name.len())];
    let length = bytes.len();
    put32(&mut bytes, 0, ty);
    put16(&mut bytes, 4, length as u16);
    bytes[6] = (name.len() / 2) as u8;
    bytes[7] = if name.is_empty() { 0 } else { 26 };
    put64(&mut bytes, 8, low);
    put64(&mut bytes, 16, target);
    put16(&mut bytes, 24, id);
    bytes[26..26 + name.len()].copy_from_slice(&name);
    bytes
}

pub fn file_record(id: u64, directory: bool, base: u64, attrs: &[Vec<u8>]) -> Vec<u8> {
    let mut bytes = vec![0; RECORD];
    bytes[..4].copy_from_slice(b"FILE");
    put16(&mut bytes, 4, 48);
    put16(&mut bytes, 6, 3);
    put16(&mut bytes, 16, 1);
    let links = attrs.iter().filter(|a| a[0] == 0x30).count();
    put16(&mut bytes, 18, links as u16);
    put16(&mut bytes, 20, 56);
    put16(&mut bytes, 22, if directory { 3 } else { 1 });
    put32(&mut bytes, 28, RECORD as u32);
    put64(&mut bytes, 32, base);
    put16(&mut bytes, 40, 32);
    put32(&mut bytes, 44, id as u32);
    let mut pos = 56;
    for attr in attrs {
        bytes[pos..pos + attr.len()].copy_from_slice(attr);
        pos += attr.len();
    }
    put32(&mut bytes, pos, u32::MAX);
    put32(&mut bytes, 24, (pos + 8) as u32);
    put16(&mut bytes, 48, 0xa55a);
    for sector in 1..=2 {
        let end = sector * SECTOR - 2;
        let value = u16::from_le_bytes([bytes[end], bytes[end + 1]]);
        put16(&mut bytes, 48 + sector * 2, value);
        put16(&mut bytes, end, 0xa55a);
    }
    bytes
}

pub fn boot(volume_size: usize) -> Vec<u8> {
    let mut bytes = vec![0; SECTOR];
    bytes[0..3].copy_from_slice(&[0xeb, 0x52, 0x90]);
    bytes[3..11].copy_from_slice(b"NTFS    ");
    put16(&mut bytes, 11, SECTOR as u16);
    bytes[13] = (CLUSTER / SECTOR) as u8;
    put64(&mut bytes, 40, (volume_size / SECTOR) as u64);
    put64(&mut bytes, 48, MFT_LCN);
    put64(&mut bytes, 56, 2);
    bytes[64] = (-10i8) as u8;
    bytes[68] = 1;
    put64(&mut bytes, 72, 0x1234_5678_90ab_cdef);
    bytes[510..512].copy_from_slice(&[0x55, 0xaa]);
    bytes
}

/// 第一段 32 个记录放 LCN4，第二段放 LCN24，留下真实空洞以检查逻辑偏移映射。
pub fn record_offset(id: usize) -> usize {
    if id < 32 {
        MFT_LCN as usize * CLUSTER + id * RECORD
    } else {
        24 * CLUSTER + (id - 32) * RECORD
    }
}

pub fn set_record(image: &mut [u8], id: usize, bytes: &[u8]) {
    let offset = record_offset(id);
    image[offset..offset + RECORD].copy_from_slice(bytes);
}

fn small_mft_record(bitmap: &[u8]) -> Vec<u8> {
    small_mft_with_bitmap(resident(0xb0, 3, "", bitmap))
}

fn small_mft_with_bitmap(bitmap: Vec<u8>) -> Vec<u8> {
    file_record(
        0,
        false,
        0,
        &[
            si(6),
            filename(1, reference(5), "$MFT", 1),
            nonresident(Stream {
                ty: 0x80,
                id: 2,
                name: "",
                low: 0,
                allocated: (RECORDS * RECORD) as u64,
                size: (RECORDS * RECORD) as u64,
                flags: 0,
                physical: 0,
                runs: &[(Some(MFT_LCN), 8), (Some(24), 8)],
            }),
            bitmap,
        ],
    )
}

/// BITMAP 结构可解析，但缺少 VCN 0 或位数不够；MFT DATA 自身仍完整。
pub fn invalid_bitmap_image(missing_first_extent: bool) -> Vec<u8> {
    let mut image = small_image(false);
    // small_image 的槽 32 故意放了损坏垃圾，用于验证有效位图过滤；这里改为明确空槽。
    set_record(&mut image, 32, &[0; RECORD]);
    let bitmap = if missing_first_extent {
        nonresident(Stream {
            ty: 0xb0,
            id: 3,
            name: "",
            low: 1,
            allocated: CLUSTER as u64,
            size: (RECORDS / 8) as u64,
            flags: 0,
            physical: 0,
            runs: &[(Some(100), 1)],
        })
    } else {
        resident(0xb0, 3, "", &[0xff; RECORDS / 8 - 1])
    };
    set_record(&mut image, 0, &small_mft_with_bitmap(bitmap));
    image
}

/// 合法无 DATA 的文件：$Extend 索引元文件和普通文件符号链接。
pub fn files_without_data_image() -> Vec<u8> {
    let mut image = small_image(false);
    let mut bitmap = vec![0; RECORDS / 8];
    for id in [0, 3, 5, 11].into_iter().chain(16..=31).chain(40..=43) {
        bitmap[id / 8] |= 1 << (id % 8);
    }
    set_record(&mut image, 0, &small_mft_record(&bitmap));
    // 空索引包含 INDEX_ROOT、INDEX_HEADER 和一个终结 INDEX_ENTRY。
    let mut index_root = vec![0; 48];
    put32(&mut index_root, 8, CLUSTER as u32);
    index_root[12] = 1;
    put32(&mut index_root, 16, 16);
    put32(&mut index_root, 20, 32);
    put32(&mut index_root, 24, 32);
    put16(&mut index_root, 40, 16);
    put16(&mut index_root, 44, 2);
    for (id, name, indexes) in [
        (40, "$Quota", &["$O", "$Q"][..]),
        (41, "$ObjId", &["$O"][..]),
        (42, "$Reparse", &["$R"][..]),
    ] {
        let mut attrs = vec![si(6), filename(1, reference(11), name, 1)];
        for (instance, name) in indexes.iter().enumerate() {
            attrs.push(resident(0x90, instance as u16 + 2, name, &index_root));
        }
        set_record(&mut image, id, &file_record(id as u64, false, 0, &attrs));
    }
    let target = wide(r"a\tiny.txt");
    let mut symlink = vec![0; 20 + 2 * target.len()];
    put32(&mut symlink, 0, 0xa000_000c);
    put16(&mut symlink, 4, (12 + 2 * target.len()) as u16);
    put16(&mut symlink, 10, target.len() as u16);
    put16(&mut symlink, 12, target.len() as u16);
    put16(&mut symlink, 14, target.len() as u16);
    put32(&mut symlink, 16, 1);
    symlink[20..20 + target.len()].copy_from_slice(&target);
    symlink[20 + target.len()..].copy_from_slice(&target);
    set_record(
        &mut image,
        43,
        &file_record(
            43,
            false,
            0,
            &[
                si(0x420),
                filename(1, reference(5), "file-link", 1),
                resident(0xc0, 2, "", &symlink),
            ],
        ),
    );
    image
}

pub fn small_image(nonresident_list: bool) -> Vec<u8> {
    let mut image = vec![0; VOLUME_BYTES];
    image[..SECTOR].copy_from_slice(&boot(VOLUME_BYTES));
    let mut records = Vec::new();
    let mut bitmap = vec![0; RECORDS / 8];
    for id in [0, 3, 5, 11].into_iter().chain(16..=31).chain([40]) {
        bitmap[id / 8] |= 1 << (id % 8);
    }
    records.push((0, small_mft_record(&bitmap)));
    let mut volume_info = vec![0; 12];
    volume_info[8] = 3;
    volume_info[9] = 1;
    records.push((
        3,
        file_record(
            3,
            false,
            0,
            &[
                si(6),
                filename(1, reference(5), "$Volume", 1),
                resident(0x70, 2, "", &volume_info),
            ],
        ),
    ));
    for (id, parent, name, attrs) in [
        (5, 5, ".", 0x10),
        (11, 5, "$Extend", 0x10),
        (16, 5, "a", 0x10),
        (17, 5, "z", 0x10),
    ] {
        records.push((
            id,
            file_record(
                id as u64,
                true,
                0,
                &[si(attrs), filename(1, reference(parent), name, 1)],
            ),
        ));
    }
    records.push((
        18,
        file_record(
            18,
            false,
            0,
            &[
                si(0),
                filename(1, reference(16), "tiny.txt", 1),
                resident(0x80, 2, "", b"abc"),
            ],
        ),
    ));
    records.push((
        19,
        file_record(
            19,
            false,
            0,
            &[
                si(0),
                filename(1, reference(16), "large.bin", 1),
                data(2, 6000, 48, 2),
            ],
        ),
    ));
    records.push((
        20,
        file_record(
            20,
            false,
            0,
            &[
                si(0),
                filename(1, reference(16), "shared.bin", 1),
                filename(2, reference(17), "shared.bin", 1),
                filename(3, reference(16), "SHARED~1.BIN", 2),
                data(4, 1234, 50, 1),
            ],
        ),
    ));
    records.push((
        21,
        file_record(
            21,
            false,
            0,
            &[
                si(0x200),
                filename(1, reference(5), "sparse.bin", 1),
                nonresident(Stream {
                    ty: 0x80,
                    id: 2,
                    name: "",
                    low: 0,
                    allocated: 16384,
                    size: 16384,
                    flags: 0x8000,
                    physical: 4096,
                    runs: &[(Some(52), 1), (None, 3)],
                }),
            ],
        ),
    ));
    records.push((
        22,
        file_record(
            22,
            false,
            0,
            &[
                si(0x800),
                filename(1, reference(5), "compressed.bin", 1),
                nonresident(Stream {
                    ty: 0x80,
                    id: 2,
                    name: "",
                    low: 0,
                    allocated: 65536,
                    size: 65536,
                    flags: 1,
                    physical: 4096,
                    runs: &[(Some(53), 1), (None, 15)],
                }),
            ],
        ),
    ));
    records.push((
        23,
        file_record(
            23,
            false,
            0,
            &[
                si(0),
                filename(1, reference(5), "streams.bin", 1),
                resident(0x80, 2, "", b"abc"),
                nonresident(Stream {
                    ty: 0x80,
                    id: 4,
                    name: "big",
                    low: 0,
                    allocated: 4096,
                    size: 1000,
                    flags: 0,
                    physical: 0,
                    runs: &[(Some(54), 1)],
                }),
                resident(0x80, 3, "small", b"1234"),
            ],
        ),
    ));
    records.push((
        24,
        file_record(
            24,
            false,
            0,
            &[
                si(0x401000),
                filename(1, reference(5), "cloud.bin", 1),
                data(2, 999, 55, 1),
            ],
        ),
    ));
    for (id, name, tag) in [
        (25, "junction", 0xa000_0003),
        (26, "dir-link", 0xa000_000c),
        (27, "cloud-dir", 0x9000_001a),
    ] {
        records.push((
            id,
            file_record(
                id as u64,
                true,
                0,
                &[
                    si(0x410),
                    filename(1, reference(5), name, 1),
                    reparse(2, tag),
                ],
            ),
        ));
    }
    records.push((
        28,
        file_record(
            28,
            false,
            0,
            &[
                si(0),
                filename(1, reference(27), "nested.txt", 1),
                resident(0x80, 2, "", b"child"),
            ],
        ),
    ));
    records.push((
        29,
        file_record(
            29,
            false,
            0,
            &[
                si(6),
                filename(1, reference(11), "$UsnJrnl", 1),
                resident(0x80, 2, "$J", b"journal"),
            ],
        ),
    ));
    let list: Vec<u8> = [
        (0x10, 0, 30, 0),
        (0x30, 0, 30, 1),
        (0x80, 0, 30, 2),
        (0x80, 1, 31, 3),
    ]
    .into_iter()
    .flat_map(|(ty, low, id, instance)| list_entry(ty, "", low, reference(id), instance))
    .collect();
    let list_attr = if nonresident_list {
        image[100 * CLUSTER..100 * CLUSTER + list.len()].copy_from_slice(&list);
        nonresident(Stream {
            ty: 0x20,
            id: 4,
            name: "",
            low: 0,
            allocated: 4096,
            size: list.len() as u64,
            flags: 0,
            physical: 0,
            runs: &[(Some(100), 1)],
        })
    } else {
        resident(0x20, 4, "", &list)
    };
    records.push((
        30,
        file_record(
            30,
            false,
            0,
            &[
                si(0),
                list_attr,
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
    ));
    records.push((
        31,
        file_record(
            31,
            false,
            reference(30),
            &[nonresident(Stream {
                ty: 0x80,
                id: 3,
                name: "",
                low: 1,
                allocated: 0,
                size: 0,
                flags: 0,
                physical: 0,
                runs: &[(Some(61), 1)],
            })],
        ),
    ));
    records.push((
        40,
        file_record(
            40,
            false,
            0,
            &[
                si(0),
                filename(1, reference(5), "fragment-two.txt", 1),
                resident(0x80, 2, "", b"second"),
            ],
        ),
    ));
    for (id, record) in records {
        set_record(&mut image, id, &record);
    }
    // 清除的 bitmap 位对应的数据即使已损坏，也不应解析。
    let deleted = record_offset(32);
    image[deleted..deleted + RECORD].fill(0xfe);
    image
}

/// `$BadClus:$Bad` 不设置 SPARSE 标记，但未损坏的簇对应空洞；坏簇使用同号 VCN/LCN。
pub fn bad_cluster_image(physical: bool, record_id: usize, stream_name: &str) -> Vec<u8> {
    let mut image = small_image(false);
    let mut bitmap = vec![0; RECORDS / 8];
    for id in [0, 3, 5, 11]
        .into_iter()
        .chain(16..=31)
        .chain([40, record_id])
    {
        bitmap[id / 8] |= 1 << (id % 8);
    }
    set_record(&mut image, 0, &small_mft_record(&bitmap));
    let clusters = (VOLUME_BYTES / CLUSTER) as u64;
    let runs = if physical {
        vec![(None, 240), (Some(240), 1), (None, clusters - 241)]
    } else {
        vec![(None, clusters)]
    };
    let mut bad = nonresident(Stream {
        ty: 0x80,
        id: 3,
        name: stream_name,
        low: 0,
        allocated: VOLUME_BYTES as u64,
        size: VOLUME_BYTES as u64,
        flags: 0,
        physical: 0,
        runs: &runs,
    });
    put64(&mut bad, 56, 0);
    set_record(
        &mut image,
        record_id,
        &file_record(
            record_id as u64,
            false,
            0,
            &[
                si(6),
                filename(1, reference(5), "$BadClus", 1),
                resident(0x80, 2, "", &[]),
                bad,
            ],
        ),
    );
    image
}

/// MFT 第一段只在基记录中；第二段映射存放于第一段可达的扩展记录1。
pub fn bootstrap_image(unreachable_extension: bool) -> Vec<u8> {
    let mut image = small_image(false);
    let extension = if unreachable_extension { 32 } else { 1 };
    let mut bitmap = vec![0; RECORDS / 8];
    for id in [0, 3, 5, 11]
        .into_iter()
        .chain(16..=31)
        .chain([40, extension])
    {
        bitmap[id / 8] |= 1 << (id % 8);
    }
    let list: Vec<_> = [
        (0x10, 0, 0, 0),
        (0x30, 0, 0, 1),
        (0x80, 0, 0, 2),
        (0x80, 8, extension as u64, 5),
        (0xb0, 0, 0, 3),
    ]
    .into_iter()
    .flat_map(|(ty, low, id, instance)| list_entry(ty, "", low, reference(id), instance))
    .collect();
    set_record(
        &mut image,
        0,
        &file_record(
            0,
            false,
            0,
            &[
                si(6),
                resident(0x20, 4, "", &list),
                filename(1, reference(5), "$MFT", 1),
                nonresident(Stream {
                    ty: 0x80,
                    id: 2,
                    name: "",
                    low: 0,
                    allocated: (RECORDS * RECORD) as u64,
                    size: (RECORDS * RECORD) as u64,
                    flags: 0,
                    physical: 0,
                    runs: &[(Some(MFT_LCN), 8)],
                }),
                resident(0xb0, 3, "", &bitmap),
            ],
        ),
    );
    set_record(
        &mut image,
        extension,
        &file_record(
            extension as u64,
            false,
            reference(0),
            &[nonresident(Stream {
                ty: 0x80,
                id: 5,
                name: "",
                low: 8,
                allocated: 0,
                size: 0,
                flags: 0,
                physical: 0,
                runs: &[(Some(24), 8)],
            })],
        ),
    );
    image
}

pub enum MftAuxiliaryDamage {
    StaleBitmapReference,
    OmittedFileName,
    InvalidAttributeList,
}

/// MFT 的 DATA 映射有效，只有与映射无关的属性列表或 BITMAP 扩展损坏。
pub fn mft_auxiliary_damage_image(data_extension: bool, damage: MftAuxiliaryDamage) -> Vec<u8> {
    let mut image = if data_extension {
        bootstrap_image(false)
    } else {
        small_image(false)
    };
    // 无有效 BITMAP 时，明确全零的空槽不算额外的损坏记录。
    set_record(&mut image, 32, &[0; RECORD]);
    let stale_bitmap = matches!(damage, MftAuxiliaryDamage::StaleBitmapReference);
    let mut bitmap = vec![0; RECORDS / 8];
    for id in [0, 3, 5, 11].into_iter().chain(16..=31).chain([40]) {
        bitmap[id / 8] |= 1 << (id % 8);
    }
    if data_extension {
        bitmap[0] |= 1 << 1;
    }
    if stale_bitmap {
        bitmap[0] |= 1 << 2;
        let mut extension = file_record(2, false, reference(0), &[resident(0xb0, 6, "", &bitmap)]);
        put16(&mut extension, 16, 2);
        set_record(&mut image, 2, &extension);
    }
    let mut list = list_entry(0x10, "", 0, reference(0), 0);
    if !matches!(damage, MftAuxiliaryDamage::OmittedFileName) {
        list.extend(list_entry(0x30, "", 0, reference(0), 1));
    }
    list.extend(list_entry(0x80, "", 0, reference(0), 2));
    if data_extension {
        list.extend(list_entry(0x80, "", 8, reference(1), 5));
    }
    list.extend(if stale_bitmap {
        list_entry(0xb0, "", 0, reference(2), 6)
    } else {
        list_entry(0xb0, "", 0, reference(0), 3)
    });
    if matches!(damage, MftAuxiliaryDamage::InvalidAttributeList) {
        list = vec![0; 26];
    }
    let runs = if data_extension {
        vec![(Some(MFT_LCN), 8)]
    } else {
        vec![(Some(MFT_LCN), 8), (Some(24), 8)]
    };
    let mut attrs = vec![
        si(6),
        resident(0x20, 4, "", &list),
        filename(1, reference(5), "$MFT", 1),
        nonresident(Stream {
            ty: 0x80,
            id: 2,
            name: "",
            low: 0,
            allocated: (RECORDS * RECORD) as u64,
            size: (RECORDS * RECORD) as u64,
            flags: 0,
            physical: 0,
            runs: &runs,
        }),
    ];
    if !stale_bitmap {
        attrs.push(resident(0xb0, 3, "", &bitmap));
    }
    set_record(&mut image, 0, &file_record(0, false, 0, &attrs));
    image
}

/// 流式生成大镜像：1,000,000 个用户文件 + 根目录/MFT，占约 977 MiB，内存保持常量。
pub fn write_benchmark(path: &Path, files: usize) -> io::Result<()> {
    let records = files + 16;
    let mft_clusters = (records * RECORD).div_ceil(CLUSTER);
    let bitmap = {
        let mut bitmap = vec![0; records.div_ceil(8)];
        for id in [0, 5].into_iter().chain(16..records) {
            bitmap[id / 8] |= 1 << (id % 8);
        }
        bitmap
    };
    let bitmap_clusters = bitmap.len().div_ceil(CLUSTER);
    let bitmap_lcn = MFT_LCN + mft_clusters as u64;
    let volume_size = (bitmap_lcn as usize + bitmap_clusters + 1) * CLUSTER;
    let mut out = BufWriter::with_capacity(1024 * 1024, File::create(path)?);
    out.get_ref().set_len(volume_size as u64)?;
    out.write_all(&boot(volume_size))?;
    out.seek(SeekFrom::Start(MFT_LCN * CLUSTER as u64))?;
    let mft = file_record(
        0,
        false,
        0,
        &[
            si(6),
            filename(1, reference(5), "$MFT", 1),
            data(2, (records * RECORD) as u64, MFT_LCN, mft_clusters as u64),
            nonresident(Stream {
                ty: 0xb0,
                id: 3,
                name: "",
                low: 0,
                allocated: (bitmap_clusters * CLUSTER) as u64,
                size: bitmap.len() as u64,
                flags: 0,
                physical: 0,
                runs: &[(Some(bitmap_lcn), bitmap_clusters as u64)],
            }),
        ],
    );
    out.write_all(&mft)?;
    for id in 1..16 {
        if id == 5 {
            out.write_all(&file_record(
                5,
                true,
                0,
                &[si(0x10), filename(1, reference(5), ".", 1)],
            ))?;
        } else {
            out.write_all(&[0; RECORD])?;
        }
    }
    for id in 16..records {
        let name = format!("file-{id:07}.txt");
        out.write_all(&file_record(
            id as u64,
            false,
            0,
            &[
                si(0),
                filename(1, reference(5), &name, 1),
                resident(0x80, 2, "", b"123456789"),
            ],
        ))?;
    }
    out.seek(SeekFrom::Start(bitmap_lcn * CLUSTER as u64))?;
    out.write_all(&bitmap)?;
    out.flush()
}

/// 混合基准的索引预期值；文件数包含 $MFT，目录数不包含根目录，记录数只含位图中的活记录。
#[allow(dead_code)] // 此 fixture 还会被不运行混合基准的集成测试直接包含。
#[derive(Clone, Copy, Debug)]
pub struct MixedBenchmark {
    pub files: u64,
    pub dirs: u64,
    pub records: u64,
    pub allocated: u64,
    pub apparent: u64,
}

/// 流式生成多层目录、硬链接和属性扩展混合的大 MFT，不生成或读取普通文件的数据簇。
/// 目录数为用户文件数的 15%，按八叉树分布；5% 的文件有第二个名字，2% 的 DATA 全在尾部扩展记录。
/// 扩展记录按可逆的位翻转排列，使顺序扫描基础记录时需要在 MFT 尾部跳转定位。
#[allow(dead_code)] // 调用方是 scan/ntfs/benchmark.rs，其他 fixture 使用方不必调用。
pub fn write_mixed_benchmark(path: &Path, files: usize) -> io::Result<MixedBenchmark> {
    let invalid = || {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "混合基准规模过大 / Benchmark is too large",
        )
    };
    let dirs = files.checked_mul(15).ok_or_else(invalid)? / 100;
    let extensions = files / 50;
    let first_file = 16usize.checked_add(dirs).ok_or_else(invalid)?;
    let first_extension = first_file.checked_add(files).ok_or_else(invalid)?;
    let records = first_extension
        .checked_add(extensions)
        .ok_or_else(invalid)?;
    if records > u32::MAX as usize {
        return Err(invalid());
    }
    let mft_bytes = records.checked_mul(RECORD).ok_or_else(invalid)?;
    let mft_clusters = mft_bytes.div_ceil(CLUSTER);
    let mut bitmap = vec![0; records.div_ceil(8)];
    for id in [0, 5].into_iter().chain(16..records) {
        bitmap[id / 8] |= 1 << (id % 8);
    }
    let bitmap_clusters = bitmap.len().div_ceil(CLUSTER);
    let bitmap_lcn = (MFT_LCN as usize)
        .checked_add(mft_clusters)
        .ok_or_else(invalid)?;
    let volume_size = bitmap_lcn
        .checked_add(bitmap_clusters)
        .and_then(|n| n.checked_add(1))
        .and_then(|n| n.checked_mul(CLUSTER))
        .ok_or_else(invalid)?;
    let mut out = BufWriter::with_capacity(1024 * 1024, File::create(path)?);
    out.get_ref().set_len(volume_size as u64)?;
    out.write_all(&boot(volume_size))?;
    out.seek(SeekFrom::Start(MFT_LCN * CLUSTER as u64))?;
    out.write_all(&file_record(
        0,
        false,
        0,
        &[
            si(6),
            filename(1, reference(5), "$MFT", 1),
            data(2, mft_bytes as u64, MFT_LCN, mft_clusters as u64),
            nonresident(Stream {
                ty: 0xb0,
                id: 3,
                name: "",
                low: 0,
                allocated: (bitmap_clusters * CLUSTER) as u64,
                size: bitmap.len() as u64,
                flags: 0,
                physical: 0,
                runs: &[(Some(bitmap_lcn as u64), bitmap_clusters as u64)],
            }),
        ],
    ))?;
    for id in 1..16 {
        if id == 5 {
            out.write_all(&file_record(
                5,
                true,
                0,
                &[si(0x10), filename(1, reference(5), ".", 1)],
            ))?;
        } else {
            out.write_all(&[0; RECORD])?;
        }
    }
    for ordinal in 0..dirs {
        let id = 16 + ordinal;
        let parent = if ordinal < 8 {
            5
        } else {
            16 + (ordinal - 8) / 8
        };
        out.write_all(&file_record(
            id as u64,
            true,
            0,
            &[
                si(0x10),
                filename(1, reference(parent as u64), &format!("dir-{ordinal:07}"), 1),
            ],
        ))?;
    }
    let parent = |ordinal: usize| {
        if dirs == 0 {
            reference(5)
        } else {
            reference((16 + ordinal % dirs) as u64)
        }
    };
    for ordinal in 0..files {
        let id = first_file + ordinal;
        let linked = (ordinal + 1).is_multiple_of(20);
        let extended = (ordinal + 1).is_multiple_of(50);
        let mut attrs = vec![
            si(0),
            filename(1, parent(ordinal), &format!("file-{ordinal:07}.txt"), 1),
        ];
        if linked {
            attrs.push(filename(
                2,
                parent(ordinal + 1),
                &format!("hard-{ordinal:07}.txt"),
                1,
            ));
        }
        if extended {
            let extension_index = (ordinal + 1) / 50 - 1;
            let extension = first_extension + mixed_extension_slot(extension_index, extensions);
            let mut list = list_entry(0x10, "", 0, reference(id as u64), 0);
            list.extend(list_entry(0x30, "", 0, reference(id as u64), 1));
            if linked {
                list.extend(list_entry(0x30, "", 0, reference(id as u64), 2));
            }
            list.extend(list_entry(0x80, "", 0, reference(extension as u64), 4));
            attrs.push(resident(0x20, 3, "", &list));
        } else {
            attrs.push(resident(0x80, 4, "", b"123456789"));
        }
        out.write_all(&file_record(id as u64, false, 0, &attrs))?;
    }
    for slot in 0..extensions {
        // 位翻转排列是其自身的逆置换，可以顺序写出尾部，不保存每条记录的映射表。
        let extension_index = mixed_extension_slot(slot, extensions);
        let ordinal = (extension_index + 1) * 50 - 1;
        out.write_all(&file_record(
            (first_extension + slot) as u64,
            false,
            reference((first_file + ordinal) as u64),
            &[resident(0x80, 4, "", b"123456789")],
        ))?;
    }
    out.seek(SeekFrom::Start(bitmap_lcn as u64 * CLUSTER as u64))?;
    out.write_all(&bitmap)?;
    out.flush()?;
    Ok(MixedBenchmark {
        files: files as u64 + 1,
        dirs: dirs as u64,
        records: (files + dirs + extensions + 2) as u64,
        allocated: mft_clusters as u64 * CLUSTER as u64 + files as u64 * 16,
        apparent: mft_bytes as u64 + files as u64 * 9,
    })
}

/// 截断到 count 范围的位翻转置换：配对两端均在范围内才交换，否则保持原位。
fn mixed_extension_slot(index: usize, count: usize) -> usize {
    if count <= 1 {
        return index;
    }
    let bits = usize::BITS - (count - 1).leading_zeros();
    let reversed = index.reverse_bits() >> (usize::BITS - bits);
    if reversed < count { reversed } else { index }
}

/// 基记录只保存 SI、一个名字和属性列表；全部 DATA 和其余硬链接名字都在扩展记录中。
/// 扩展记录可位于基础记录之前，以覆盖顺序扫描先遇到扩展记录的布局。
pub fn data_in_extension_image(extension_first: bool, attributes: u32) -> Vec<u8> {
    let mut image = small_image(false);
    let base = 40;
    let extension = if extension_first { 14 } else { 41 };
    let mut bitmap = vec![0; RECORDS / 8];
    for id in [0, 3, 5, 11]
        .into_iter()
        .chain(16..=31)
        .chain([base, extension])
    {
        bitmap[id / 8] |= 1 << (id % 8);
    }
    set_record(&mut image, 0, &small_mft_record(&bitmap));
    let list: Vec<_> = [
        (0x10, base, 0),
        (0x30, base, 1),
        (0x30, extension, 3),
        (0x30, extension, 4),
        (0x80, extension, 5),
    ]
    .into_iter()
    .flat_map(|(ty, id, instance)| list_entry(ty, "", 0, reference(id as u64), instance))
    .collect();
    set_record(
        &mut image,
        base,
        &file_record(
            base as u64,
            false,
            0,
            &[
                si(attributes),
                resident(0x20, 2, "", &list),
                filename(1, reference(17), "late.bin", 1),
            ],
        ),
    );
    set_record(
        &mut image,
        extension,
        &file_record(
            extension as u64,
            false,
            reference(base as u64),
            &[
                filename(3, reference(16), "first.bin", 1),
                filename(4, reference(16), "second.bin", 1),
                data(5, 6000, 80, 2),
            ],
        ),
    );
    image
}

/// 按 Windows 验收报告5.2的六段VCN/flags布局构造压缩流，段顺序故意不是VCN顺序。
pub fn acceptance_split_compressed_image() -> Vec<u8> {
    let mut image = small_image(false);
    image.resize(8 * VOLUME_BYTES, 0);
    image[..SECTOR].copy_from_slice(&boot(8 * VOLUME_BYTES));
    let base = 0x24usize;
    let segments = [
        (base, 320, 438, 0),
        (12, 213, 319, 0),
        (13, 439, 773, 0),
        (14, 914, 1103, 0),
        (15, 0, 212, 1),
        (37, 774, 913, 1),
    ];
    let mut bitmap = vec![0; RECORDS / 8];
    for id in [0, 3, 5, 11]
        .into_iter()
        .chain(16..=31)
        .chain([40])
        .chain(segments.iter().map(|s| s.0))
    {
        bitmap[id / 8] |= 1 << (id % 8);
    }
    set_record(&mut image, 0, &small_mft_record(&bitmap));
    let mut list = list_entry(0x10, "", 0, reference(base as u64), 0);
    for &(id, low, _, _) in &segments {
        list.extend(list_entry(0x30, "", 0, reference(id as u64), 1));
        list.extend(list_entry(0x80, "", low, reference(id as u64), 2));
    }
    for (i, &(id, low, high, flags)) in segments.iter().enumerate() {
        let len = high - low + 1;
        let runs = match low {
            0 => vec![(Some(256), 40), (None, len - 40)],
            774 => vec![(Some(320), 8), (None, len - 8)],
            _ => vec![(None, len)],
        };
        let mut attrs = Vec::new();
        if id == base {
            attrs.push(si(0x820));
            attrs.push(resident(0x20, 3, "", &list));
        }
        attrs.push(filename(1, reference(5), &format!("seg{i}.log"), 1));
        attrs.push(nonresident(Stream {
            ty: 0x80,
            id: 2,
            name: "",
            low,
            allocated: if low == 0 { 4_521_984 } else { 0 },
            size: if low == 0 { 4_494_815 } else { 0 },
            flags,
            physical: if low == 0 { 48 * CLUSTER as u64 } else { 0 },
            runs: &runs,
        }));
        set_record(
            &mut image,
            id,
            &file_record(
                id as u64,
                false,
                if id == base {
                    0
                } else {
                    reference(base as u64)
                },
                &attrs,
            ),
        );
    }
    image
}

/// 同一流的普通段与稀疏段混合；后段标记为稀疏也必须影响先前未标记的段。
pub fn mixed_sparse_flags_image(sparse_flag_on_head: bool) -> Vec<u8> {
    let mut image = small_image(false);
    let mut bitmap = vec![0; RECORDS / 8];
    for id in [0, 3, 5, 11].into_iter().chain(16..=31).chain([40, 41]) {
        bitmap[id / 8] |= 1 << (id % 8);
    }
    set_record(&mut image, 0, &small_mft_record(&bitmap));
    let list: Vec<_> = [
        (0x10, 0, 40, 0),
        (0x30, 0, 40, 1),
        (0x80, 0, 40, 2),
        (0x80, 2, 41, 3),
    ]
    .into_iter()
    .flat_map(|(ty, low, id, instance)| list_entry(ty, "", low, reference(id), instance))
    .collect();
    set_record(
        &mut image,
        40,
        &file_record(
            40,
            false,
            0,
            &[
                si(0x200),
                resident(0x20, 4, "", &list),
                filename(1, reference(5), "mixed-sparse.bin", 1),
                nonresident(Stream {
                    ty: 0x80,
                    id: 2,
                    name: "",
                    low: 0,
                    allocated: 4 * CLUSTER as u64,
                    size: 4 * CLUSTER as u64,
                    flags: if sparse_flag_on_head { 0x8000 } else { 0 },
                    physical: 2 * CLUSTER as u64,
                    runs: &[(Some(80), 1), (None, 1)],
                }),
            ],
        ),
    );
    set_record(
        &mut image,
        41,
        &file_record(
            41,
            false,
            reference(40),
            &[nonresident(Stream {
                ty: 0x80,
                id: 3,
                name: "",
                low: 2,
                allocated: 0,
                size: 0,
                flags: if sparse_flag_on_head { 0 } else { 0x8000 },
                physical: 0,
                runs: &[(Some(81), 1), (None, 1)],
            })],
        ),
    );
    image
}

/// 固定读取延迟基准：沿用混合目录/硬链接布局，将 2% 文件改为外置属性列表和重解析数据。
/// 每个扩展记录与其基础记录相隔很远；一半交换前后位置，验证顺序收集不依赖遇到顺序。
/// 列表项带合法填充，列表长 4–5 KiB，重解析数据长 4 KiB；每 1024 组之间留 512 KiB 空洞。
#[allow(dead_code)]
pub fn write_io_benchmark(path: &Path, files: usize) -> io::Result<MixedBenchmark> {
    let expected = write_mixed_benchmark(path, files)?;
    let dirs = files * 15 / 100;
    let first_file = 16 + dirs;
    let first_extension = first_file + files;
    let extensions = files / 50;
    if extensions == 0 {
        return Ok(expected);
    }
    let mut out = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)?;
    let metadata_lcn = out.metadata()?.len().div_ceil(CLUSTER as u64);
    let cluster_for = |ordinal: usize| metadata_lcn + (ordinal * 3 + ordinal / 1024 * 128) as u64;
    let end_lcn = cluster_for(extensions - 1) + 3;
    let volume_size = end_lcn
        .checked_mul(CLUSTER as u64)
        .and_then(|n| usize::try_from(n).ok())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "NTFS 基准镜像过大 / Benchmark image is too large",
            )
        })?;
    out.set_len(volume_size as u64)?;
    out.seek(SeekFrom::Start(0))?;
    out.write_all(&boot(volume_size))?;
    for index in 0..extensions {
        let ordinal = (index + 1) * 50 - 1;
        let original_base = first_file + ordinal;
        let original_extension = first_extension + mixed_extension_slot(index, extensions);
        let (base, extension) = if index.is_multiple_of(2) {
            (original_base, original_extension)
        } else {
            (original_extension, original_base)
        };
        let parent = |n: usize| reference((16 + n % dirs) as u64);
        let mut attrs = vec![
            si(0x400),
            filename(1, parent(ordinal), &format!("file-{ordinal:07}.txt"), 1),
        ];
        let linked = (ordinal + 1).is_multiple_of(20);
        if linked {
            attrs.push(filename(
                2,
                parent(ordinal + 1),
                &format!("hard-{ordinal:07}.txt"),
                1,
            ));
        }
        let mut entries = vec![
            list_entry(0x10, "", 0, reference(base as u64), 0),
            list_entry(0x30, "", 0, reference(base as u64), 1),
        ];
        if linked {
            entries.push(list_entry(0x30, "", 0, reference(base as u64), 2));
        }
        entries.extend([
            list_entry(0x80, "", 0, reference(extension as u64), 4),
            list_entry(0xc0, "", 0, reference(extension as u64), 5),
        ]);
        let mut list = Vec::with_capacity(entries.len() * 1024);
        for mut entry in entries {
            entry.resize(1024, 0);
            put16(&mut entry, 4, 1024);
            list.extend(entry);
        }
        let lcn = cluster_for(index);
        attrs.push(nonresident(Stream {
            ty: 0x20,
            id: 3,
            name: "",
            low: 0,
            allocated: 2 * CLUSTER as u64,
            size: list.len() as u64,
            flags: 0,
            physical: 0,
            runs: &[(Some(lcn), 2)],
        }));
        let extension_attrs = [
            resident(0x80, 4, "", b"123456789"),
            nonresident(Stream {
                ty: 0xc0,
                id: 5,
                name: "",
                low: 0,
                allocated: CLUSTER as u64,
                size: CLUSTER as u64,
                flags: 0,
                physical: 0,
                runs: &[(Some(lcn + 2), 1)],
            }),
        ];
        for (id, bytes) in [
            (base, file_record(base as u64, false, 0, &attrs)),
            (
                extension,
                file_record(
                    extension as u64,
                    false,
                    reference(base as u64),
                    &extension_attrs,
                ),
            ),
        ] {
            out.seek(SeekFrom::Start(
                MFT_LCN * CLUSTER as u64 + id as u64 * RECORD as u64,
            ))?;
            out.write_all(&bytes)?;
        }
        out.seek(SeekFrom::Start(lcn * CLUSTER as u64))?;
        out.write_all(&list)?;
        let mut reparse = [0u8; CLUSTER];
        put32(&mut reparse, 0, 0x8000_0017); // WOF；既不是符号链接，也不是仅在线云占位。
        put16(&mut reparse, 4, (CLUSTER - 8) as u16);
        out.seek(SeekFrom::Start((lcn + 2) * CLUSTER as u64))?;
        out.write_all(&reparse)?;
    }
    out.flush()?;
    Ok(expected)
}
