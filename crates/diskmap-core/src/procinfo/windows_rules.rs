//! Windows 映射的纯数据规则；不调用系统接口，可在 Linux 上覆盖 UTF-16 路径和区域边界。

use std::collections::{BTreeMap, HashSet};

use serde::Serialize;

use crate::platform::{Os, path::path_key};

use super::Kind;

pub(super) const MEM_COMMIT: u32 = 0x1000;
pub(super) const MEM_RESERVE: u32 = 0x2000;
pub(super) const MEM_MAPPED: u32 = 0x40000;
pub(super) const MEM_IMAGE: u32 = 0x1000000;

/// 地址遍历状态与逐区域取名统计；可见性只由地址空间是否完整遍历决定。
#[derive(Default, Serialize)]
pub(super) struct MappingStats {
    pub(super) virtual_query_calls: usize,
    pub(super) mapped_filename_calls: usize,
    pub(super) image_allocations: usize,
    pub(super) data_allocations: usize,
    pub(super) filename_failures: usize,
    pub(super) filename_winerrors: BTreeMap<i32, usize>,
    pub(super) unconverted_paths: usize,
    pub(super) address_walk_complete: bool,
    pub(super) timed_out: bool,
    pub(super) query_winerror: Option<i32>,
    pub(super) invalid_region: bool,
}

impl MappingStats {
    /// 与旧版 memory_maps 的口径一致，单个区域没有磁盘文件名不影响可见性。
    pub(super) fn visible(&self) -> bool {
        self.address_walk_complete
            && !self.timed_out
            && self.query_winerror.is_none()
            && !self.invalid_region
    }
}

/// 无损 UTF-16 → WTF-8；孤立代理项不经过替换字符，供公共 T15 比较规则使用。
pub(super) fn wide_bytes(path: &[u16]) -> Vec<u8> {
    let mut result = Vec::with_capacity(path.len());
    for decoded in char::decode_utf16(path.iter().copied()) {
        match decoded {
            Ok(ch) => {
                let mut bytes = [0; 4];
                result.extend_from_slice(ch.encode_utf8(&mut bytes).as_bytes());
            }
            Err(error) => {
                let unit = error.unpaired_surrogate();
                result.extend_from_slice(&[
                    0xe0 | (unit >> 12) as u8,
                    0x80 | ((unit >> 6) & 0x3f) as u8,
                    0x80 | (unit & 0x3f) as u8,
                ]);
            }
        }
    }
    result
}

fn wide_key(path: &[u16]) -> Vec<u8> {
    path_key(Os::Windows, &wide_bytes(path))
}

fn drive_path(path: &[u16]) -> bool {
    path.len() > 3
        && matches!(path[0], 65..=90 | 97..=122)
        && path[1] == u16::from(b':')
        && matches!(path[2], 47 | 92)
}

struct DeviceDrive {
    prefix_len: usize,
    key: Vec<u8>,
    drive: Vec<u16>,
}

/// 每轮只采集一次 QueryDosDevice 的当前映射；不采用 MULTI_SZ 中的历史映射。
#[derive(Default)]
pub(super) struct DevicePaths {
    entries: Vec<DeviceDrive>,
}

impl DevicePaths {
    pub(super) fn insert(&mut self, drive: &[u16], targets: &[u16]) {
        if drive.len() != 2
            || !matches!(drive[0], 65..=90 | 97..=122)
            || drive[1] != u16::from(b':')
        {
            return;
        }
        let Some(end) = targets.iter().position(|&ch| ch == 0) else {
            return;
        };
        if end == 0 {
            return;
        }
        self.entries.push(DeviceDrive {
            prefix_len: end,
            key: wide_key(&targets[..end]),
            drive: drive.to_vec(),
        });
        // 最长前缀优先；相同设备有多个盘符时固定选字母靠前的，避免刷新间路径跳动。
        self.entries.sort_by(|a, b| {
            b.prefix_len
                .cmp(&a.prefix_len)
                .then_with(|| a.drive.cmp(&b.drive))
        });
    }

    pub(super) fn translate(&self, path: &[u16]) -> Option<Vec<u16>> {
        if path.is_empty() || path.contains(&0) {
            return None;
        }
        // 仅去掉明确的 DOS 路径包装；不把卷 GUID、UNC、设备或相对路径猜成盘符路径。
        let unwrapped =
            if path.starts_with(&[92, 63, 63, 92]) || path.starts_with(&[92, 92, 63, 92]) {
                &path[4..]
            } else {
                path
            };
        if drive_path(unwrapped) {
            return Some(unwrapped.to_vec());
        }
        let key = wide_key(path);
        let entry = self.entries.iter().find(|entry| {
            key.starts_with(&entry.key)
                && key.get(entry.key.len()) == Some(&b'\\')
                && path.len() > entry.prefix_len + 1
        })?;
        let mut result = entry.drive.clone();
        result.extend_from_slice(&path[entry.prefix_len..]);
        Some(result)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum MappingKind {
    Image,
    Data,
}

impl MappingKind {
    pub(super) fn key(self) -> &'static str {
        match self {
            Self::Image => "MEM_IMAGE",
            Self::Data => "MEM_MAPPED",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Region {
    pub base: usize,
    pub allocation_base: usize,
    pub size: usize,
    pub state: u32,
    pub region_type: u32,
}

impl Region {
    /// 只能按区域大小前进；零长度、溢出和不包含查询地址的竞态结果不能造成死循环。
    pub(super) fn next_address(self, address: usize) -> Option<usize> {
        let end = self.base.checked_add(self.size)?;
        (self.size > 0 && self.base <= address && address < end).then_some(end)
    }

    fn mapping_kind(self) -> Option<MappingKind> {
        if !matches!(self.state, MEM_COMMIT | MEM_RESERVE)
            || self.allocation_base == 0
            || self.allocation_base > self.base
        {
            return None;
        }
        match self.region_type {
            MEM_IMAGE => Some(MappingKind::Image),
            MEM_MAPPED => Some(MappingKind::Data),
            _ => None,
        }
    }
}

/// 以 AllocationBase 合并保护属性不同的区域，每次分配最多查询一次路径。
#[derive(Default)]
pub(super) struct Allocations {
    seen: HashSet<usize>,
}

impl Allocations {
    pub(super) fn observe(&mut self, region: Region) -> Option<MappingKind> {
        let kind = region.mapping_kind()?;
        self.seen.insert(region.allocation_base).then_some(kind)
    }
}

/// 同一文件的多个视图只登记一次；映像和普通数据映射都使用旧版的 lib 类型。
pub(super) struct MappedFiles {
    seen: HashSet<Vec<u8>>,
    executable: Option<Vec<u8>>,
}

impl MappedFiles {
    pub(super) fn new(executable: Option<&[u16]>) -> Self {
        Self {
            seen: HashSet::new(),
            executable: executable.map(wide_key),
        }
    }

    pub(super) fn observe(&mut self, path: &[u16]) -> Option<Kind> {
        let key = wide_key(path);
        let kind = if self.executable.as_ref() == Some(&key) {
            Kind::Exe
        } else {
            Kind::Lib
        };
        self.seen.insert(key).then_some(kind)
    }
}

/// 诊断中的集合差异也使用 T15 路径键，保留原始路径以显示 WOW64 的实际差别。
pub(super) fn path_difference(left: &[Vec<u16>], right: &[Vec<u16>]) -> Vec<Vec<u16>> {
    let right: HashSet<_> = right.iter().map(|path| wide_key(path)).collect();
    left.iter()
        .filter_map(|path| {
            let key = wide_key(path);
            (!right.contains(&key)).then(|| (key, path.clone()))
        })
        .collect::<BTreeMap<_, _>>()
        .into_values()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().collect()
    }

    fn target(value: &str) -> Vec<u16> {
        wide(value).into_iter().chain([0, 0]).collect()
    }

    #[test]
    fn visibility_requires_a_valid_complete_address_walk() {
        for address_walk_complete in [false, true] {
            for timed_out in [false, true] {
                for query_winerror in [None, Some(5)] {
                    for invalid_region in [false, true] {
                        let stats = MappingStats {
                            address_walk_complete,
                            timed_out,
                            query_winerror,
                            invalid_region,
                            ..MappingStats::default()
                        };
                        // 四个状态的所有组合中，只有正常到达末尾的一组算可见。
                        assert_eq!(
                            stats.visible(),
                            (
                                address_walk_complete,
                                timed_out,
                                query_winerror,
                                invalid_region
                            ) == (true, false, None, false)
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn visibility_ignores_filename_failures_and_unconverted_paths() {
        // Windows 实测 wps.exe 有 200 个分配，其中 59 个匿名 section 返回 1006。
        let mut stats = MappingStats {
            virtual_query_calls: 1340,
            mapped_filename_calls: 200,
            filename_failures: 59,
            filename_winerrors: BTreeMap::from([(1006, 59)]),
            address_walk_complete: true,
            ..MappingStats::default()
        };
        assert!(stats.visible());
        // 即使每个区域都没有可登记的盘符路径，成功读取映射表仍算可见。
        for (filename_failures, unconverted_paths) in [(59, 141), (0, 200), (200, 0)] {
            stats.filename_failures = filename_failures;
            stats.unconverted_paths = unconverted_paths;
            stats.filename_winerrors.clear();
            if filename_failures > 0 {
                stats.filename_winerrors.insert(1006, filename_failures);
            }
            assert!(stats.visible());
        }
        stats.address_walk_complete = false;
        assert!(!stats.visible());
    }

    #[test]
    fn device_paths_use_current_mapping_and_component_boundaries() {
        let mut paths = DevicePaths::default();
        paths.insert(&wide("C:"), &target(r"\Device\HarddiskVolume1"));
        paths.insert(&wide("D:"), &target(r"\Device\HarddiskVolume10"));
        paths.insert(&wide("E:"), &target(r"\Device\HarddiskVolume1\mounted"));
        assert_eq!(
            paths.translate(&wide(r"\device\harddiskvolume1\Windows\a.mui")),
            Some(wide(r"C:\Windows\a.mui"))
        );
        assert_eq!(
            paths.translate(&wide(r"\Device\HarddiskVolume10\app.dll")),
            Some(wide(r"D:\app.dll"))
        );
        assert_eq!(
            paths.translate(&wide(r"\Device\HarddiskVolume1\mounted\db")),
            Some(wide(r"E:\db"))
        );
        assert_eq!(paths.translate(&wide(r"\Device\HarddiskVolume100\x")), None);
        assert_eq!(paths.translate(&wide(r"\Device\HarddiskVolume1")), None);
        // 后续 MULTI_SZ 元素是尚未删除的旧映射，不能把它当作当前设备。
        paths.insert(&wide("F:"), &wide("\\Device\\Current\0\\Device\\Old\0\0"));
        assert_eq!(paths.translate(&wide(r"\Device\Old\x")), None);
        assert_eq!(
            paths.translate(&wide(r"\Device\Current\x")),
            Some(wide(r"F:\x"))
        );
    }

    #[test]
    fn paths_preserve_unicode_surrogates_spaces_and_real_wow64_directories() {
        let mut paths = DevicePaths::default();
        paths.insert(&wide("Z:"), &target(r"\Device\Volume"));
        paths.insert(&wide("C:"), &target(r"\Device\Volume"));
        let mut raw = wide(r"\Device\Volume\中文 🦀\ ");
        raw.extend([0xd800, 0xdc80, 0xd801]);
        let mut expected = wide(r"C:\中文 🦀\ ");
        expected.extend([0xd800, 0xdc80, 0xd801]);
        assert_eq!(paths.translate(&raw), Some(expected));
        for directory in ["System32", "SysWOW64"] {
            assert_eq!(
                paths.translate(&wide(&format!(r"\Device\Volume\Windows\{directory}\x.dll"))),
                Some(wide(&format!(r"C:\Windows\{directory}\x.dll")))
            );
        }
        assert_eq!(wide_bytes(&[0xd800]), vec![0xed, 0xa0, 0x80]);
        assert_eq!(wide_bytes(&[0xdc80]), vec![0xed, 0xb2, 0x80]);
        assert_eq!(wide_bytes(&wide("🦀")), "🦀".as_bytes());
    }

    #[test]
    fn only_absolute_drive_paths_are_accepted_without_a_device_mapping() {
        let paths = DevicePaths::default();
        for value in [r"C:\x", "c:/x", r"\??\C:\x", r"\\?\C:\x"] {
            assert!(paths.translate(&wide(value)).is_some(), "{value}");
        }
        for value in [
            "",
            "C:",
            "C:x",
            r"C:\",
            r"\\server\share\x",
            r"\Device\Unknown\x",
            r"\\?\Volume{guid}\x",
            r"\??\UNC\server\share\x",
            "C:\\a\0b",
        ] {
            assert_eq!(paths.translate(&wide(value)), None, "{value:?}");
        }
    }

    fn region(base: usize, allocation_base: usize, region_type: u32) -> Region {
        Region {
            base,
            allocation_base,
            size: 0x1000,
            state: MEM_COMMIT,
            region_type,
        }
    }

    #[test]
    fn protection_regions_merge_by_allocation_not_by_file_extension() {
        let mut allocations = Allocations::default();
        assert_eq!(
            allocations.observe(region(0x1000, 0x1000, MEM_IMAGE)),
            Some(MappingKind::Image)
        );
        assert_eq!(allocations.observe(region(0x2000, 0x1000, MEM_IMAGE)), None);
        assert_eq!(allocations.observe(region(0x3000, 0x1000, MEM_IMAGE)), None);
        assert_eq!(
            allocations.observe(region(0x4000, 0x4000, MEM_MAPPED)),
            Some(MappingKind::Data)
        );
        assert_eq!(
            allocations.observe(region(0x5000, 0x4000, MEM_MAPPED)),
            None
        );
        // 私有页、空闲页不取路径。保留的映射仍可能有文件，与 psutil 一样尝试查询。
        assert_eq!(allocations.observe(region(0x6000, 0x6000, 0x20000)), None);
        assert_eq!(
            allocations.observe(Region {
                state: 0x10000,
                ..region(0x7000, 0x7000, MEM_IMAGE)
            }),
            None
        );
        assert_eq!(
            allocations.observe(Region {
                state: MEM_RESERVE,
                ..region(0x7000, 0x7000, MEM_IMAGE)
            }),
            Some(MappingKind::Image)
        );
        assert_eq!(allocations.observe(region(0x8000, 0x7000, MEM_IMAGE)), None);
        assert_eq!(allocations.observe(region(0x9000, 0, MEM_IMAGE)), None);
        assert_eq!(allocations.observe(region(0xa000, 0xb000, MEM_IMAGE)), None);
        assert_eq!(MappingKind::Image.key(), "MEM_IMAGE");
        assert_eq!(MappingKind::Data.key(), "MEM_MAPPED");
    }

    #[test]
    fn invalid_regions_cannot_loop_or_wrap_the_address_walk() {
        let valid = region(0x1000, 0x1000, MEM_IMAGE);
        assert_eq!(valid.next_address(0x1234), Some(0x2000));
        assert_eq!(valid.next_address(0x2000), None);
        assert_eq!(valid.next_address(0xfff), None);
        assert_eq!(Region { size: 0, ..valid }.next_address(0x1000), None);
        assert_eq!(
            Region {
                base: usize::MAX - 1,
                size: 2,
                ..valid
            }
            .next_address(usize::MAX - 1),
            None
        );
    }

    #[test]
    fn file_views_deduplicate_with_t15_keys_and_keep_exe_priority() {
        let exe = wide(r"C:\Apps\Σ.exe");
        let mut files = MappedFiles::new(Some(&exe));
        assert_eq!(files.observe(&wide("c:/apps/ς.EXE")), Some(Kind::Exe));
        assert_eq!(files.observe(&exe), None);
        for value in [r"C:\resource.mui", r"C:\database.db", r"C:\assembly.winmd"] {
            assert_eq!(files.observe(&wide(value)), Some(Kind::Lib));
            assert_eq!(files.observe(&wide(&value.to_ascii_lowercase())), None);
        }
        let mut files = MappedFiles::new(None);
        for value in [r"C:\ß", r"C:\ẞ", r"C:\🦀", r"C:\�"] {
            assert_eq!(files.observe(&wide(value)), Some(Kind::Lib));
        }
        assert_eq!(files.observe(&[67, 58, 92, 0xd800]), Some(Kind::Lib));
        assert_eq!(files.observe(&[67, 58, 92, 0xdc80]), Some(Kind::Lib));
    }

    #[test]
    fn diagnostics_keep_system32_and_syswow64_distinct() {
        let modules = [wide(r"C:\Windows\System32\x.dll"), wide(r"C:\Σ.dll")];
        let maps = [
            wide(r"C:\Windows\SysWOW64\x.dll"),
            wide("c:/ς.DLL"),
            wide(r"C:\resource.mui"),
        ];
        assert_eq!(path_difference(&modules, &maps), vec![modules[0].clone()]);
        assert_eq!(path_difference(&maps, &modules).len(), 2);
        assert_eq!(
            path_difference(&[modules[0].clone(), modules[0].clone()], &[]).len(),
            1
        );
    }
}
