//! 扫描前算好的挂载点处理方式：起点下的每个挂载点进入还是跳过。对应旧版 `Scan._run` 里的
//! `mounts`、`seen_devs`、`always_skip`。
//!
//! 规则与旧版相同：不跨分区扫描时全部跳过；不是真正占用磁盘的文件系统（proc、tmpfs、网络盘等）、
//! macOS 的系统内部卷跳过；同一个文件系统（设备号相同）只进入一次，起点所在的文件系统算已经进入。
//!
//! 旧版边扫边决定，同一个文件系统挂载多次时先遇到的那个进入。并行扫描时先后不固定，所以改为扫描前
//! 按"按名字排序的深度优先遍历"的顺序决定，结果固定。被跳过的目录下面的挂载点遇不到，不参与决定。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::platform::Os;
use crate::platform::mounts::{Mount, is_disk_fs};

/// 一个挂载点的处理方式。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Decision {
    /// 进入，目录加上 `MOUNT` 标志，计入"已进入的分区"。
    Include { fstype: String, source: String },
    /// 跳过：不计入目录树，计入"跳过的分区"。
    Skip { fstype: String },
}

/// 子目录的处理方式。
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Visit<'a> {
    /// 普通目录。
    Normal,
    /// 总是跳过的路径（macOS 的 `/System/Volumes`），不计入也不记录。
    Ignored,
    /// 挂载点。
    Mount(&'a Decision),
}

pub(super) struct MountPlan {
    os: Os,
    /// 起点所在文件系统的挂载点；挂载表里找不到时为 `None`。
    root_mount: Option<PathBuf>,
    /// 起点下（不含起点）可能遇到的挂载点，键见 [`key`]。
    nested: HashMap<Vec<u8>, Decision>,
    /// 总是跳过的路径，键见 [`key`]。
    ignored: Vec<Vec<u8>>,
}

impl MountPlan {
    /// `root` 是规范化后的扫描起点，`mounts` 来自 `platform::mounts::read_mounts`，
    /// `ignored` 来自 `platform::mounts::skip_paths`。
    pub fn new(
        os: Os,
        root: &Path,
        mounts: &[Mount],
        cross_fs: bool,
        ignored: &[PathBuf],
    ) -> MountPlan {
        let root_key = key(os, root);
        let keyed: Vec<(&Mount, Vec<u8>)> = mounts.iter().map(|m| (m, key(os, &m.path))).collect();

        // 起点所在的文件系统：起点本身或它的上级中最长的挂载点
        let root_mount = keyed
            .iter()
            .filter(|(_, k)| is_within(os, &root_key, k))
            .max_by_key(|(_, k)| k.len())
            .map(|(m, _)| *m);
        let mut seen_devs: HashSet<Vec<u8>> = root_mount
            .iter()
            .map(|m| crate::platform::path::path_key(os, m.dev.as_bytes()))
            .collect();

        let ignored: Vec<Vec<u8>> = ignored.iter().map(|p| key(os, p)).collect();
        let mut nested: Vec<&(&Mount, Vec<u8>)> = keyed
            .iter()
            .filter(|(_, k)| *k != root_key && is_within(os, k, &root_key))
            .collect();
        nested.sort_by(|a, b| components(os, &a.1).cmp(components(os, &b.1)));

        // 不会进入的目录：总是跳过的路径和被跳过的挂载点，它们下面的挂载点遇不到
        let mut blocked = ignored.clone();
        let mut decisions = HashMap::new();
        for (m, k) in nested {
            if blocked.iter().any(|b| is_within(os, k, b)) {
                continue;
            }
            let dev = crate::platform::path::path_key(os, m.dev.as_bytes());
            let include =
                cross_fs && is_disk_fs(&m.fstype) && !m.hidden && !seen_devs.contains(&dev);
            let decision = if include {
                seen_devs.insert(dev);
                Decision::Include {
                    fstype: m.fstype.clone(),
                    source: m.source.clone(),
                }
            } else {
                blocked.push(k.clone());
                Decision::Skip {
                    fstype: m.fstype.clone(),
                }
            };
            decisions.insert(k.clone(), decision);
        }
        MountPlan {
            os,
            root_mount: root_mount.map(|m| m.path.clone()),
            nested: decisions,
            ignored,
        }
    }

    /// 起点所在文件系统的挂载点。
    pub fn root_mount(&self) -> Option<&Path> {
        self.root_mount.as_deref()
    }

    /// 子目录 `path`（完整路径）的处理方式。没有需要特别处理的路径时不做任何查找。
    pub fn visit(&self, path: &Path) -> Visit<'_> {
        if self.nested.is_empty() && self.ignored.is_empty() {
            return Visit::Normal;
        }
        let raw = path.as_os_str().as_encoded_bytes();
        let folded;
        let k: &[u8] = if self.os.case_insensitive() {
            folded = key(self.os, path);
            &folded
        } else {
            raw
        };
        if self.ignored.iter().any(|i| i.as_slice() == k) {
            Visit::Ignored
        } else if let Some(d) = self.nested.get(k) {
            Visit::Mount(d)
        } else {
            Visit::Normal
        }
    }
}

/// 大小写和分隔符使用平台统一规则，保持挂载路径原有的其他字节。
fn key(os: Os, path: &Path) -> Vec<u8> {
    crate::platform::path::path_key(os, path.as_os_str().as_encoded_bytes())
}

fn is_sep(os: Os, b: u8) -> bool {
    b == b'/' || (os == Os::Windows && b == b'\\')
}

/// `path` 是否是 `dir` 本身或在 `dir` 下面（两者都是 [`key`]）。
fn is_within(os: Os, path: &[u8], dir: &[u8]) -> bool {
    if !path.starts_with(dir) {
        return false;
    }
    path.len() == dir.len()
        || dir.last().is_some_and(|&b| is_sep(os, b))
        || is_sep(os, path[dir.len()])
}

fn components(os: Os, key: &[u8]) -> impl Iterator<Item = &[u8]> {
    key.split(move |&b| is_sep(os, b)).filter(|c| !c.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mount(path: &str, fstype: &str, dev: &str) -> Mount {
        Mount {
            path: PathBuf::from(path),
            dev: dev.to_owned(),
            fstype: fstype.to_owned(),
            source: format!("/dev/{dev}"),
            hidden: false,
            removable: false,
        }
    }

    fn kind(plan: &MountPlan, path: &str) -> &'static str {
        match plan.visit(Path::new(path)) {
            Visit::Normal => "normal",
            Visit::Ignored => "ignored",
            Visit::Mount(Decision::Include { .. }) => "include",
            Visit::Mount(Decision::Skip { .. }) => "skip",
        }
    }

    fn linux_mounts() -> Vec<Mount> {
        vec![
            mount("/", "ext4", "8:1:/"),
            mount("/proc", "proc", "0:20:/"),
            mount("/proc/sys/fs/binfmt_misc", "ext4", "0:44:/"),
            mount("/home", "ext4", "8:2:/"),
            mount("/mnt/bind", "ext4", "8:2:/"),
            mount("/boot", "vfat", "8:3:/"),
            mount("/home/u/nested", "xfs", "8:4:/"),
            mount("/run/user/1000/doc", "fuse.portal", "0:50:/"),
            mount("/srv/again", "ext4", "8:1:/"),
        ]
    }

    #[test]
    fn linux_root_scan_matches_legacy_rules() {
        let plan = MountPlan::new(Os::Linux, Path::new("/"), &linux_mounts(), true, &[]);
        assert_eq!(plan.root_mount(), Some(Path::new("/")));
        assert_eq!(kind(&plan, "/home"), "include");
        assert_eq!(kind(&plan, "/boot"), "include");
        assert_eq!(kind(&plan, "/home/u/nested"), "include");
        // 同一个文件系统第二次出现、起点所在的文件系统再次出现
        assert_eq!(kind(&plan, "/mnt/bind"), "skip");
        assert_eq!(kind(&plan, "/srv/again"), "skip");
        // 不是磁盘文件系统
        assert_eq!(kind(&plan, "/proc"), "skip");
        assert_eq!(kind(&plan, "/run/user/1000/doc"), "skip");
        // 在跳过的目录下面，遇不到，也不占用设备号
        assert!(
            !plan
                .nested
                .contains_key(b"/proc/sys/fs/binfmt_misc".as_slice())
        );
        assert_eq!(kind(&plan, "/usr"), "normal");
        assert_eq!(kind(&plan, "/home/u"), "normal");
    }

    #[test]
    fn same_filesystem_goes_to_the_first_path_in_traversal_order() {
        let mounts = vec![
            mount("/", "ext4", "a"),
            mount("/a-b", "ext4", "x"),
            mount("/a/b", "ext4", "x"),
        ];
        let plan = MountPlan::new(Os::Linux, Path::new("/"), &mounts, true, &[]);
        // 深度优先时 /a/b 先于 /a-b（按字节比较则相反）
        assert_eq!(kind(&plan, "/a/b"), "include");
        assert_eq!(kind(&plan, "/a-b"), "skip");
    }

    #[test]
    fn single_filesystem_scan_skips_everything_reachable() {
        let plan = MountPlan::new(Os::Linux, Path::new("/"), &linux_mounts(), false, &[]);
        for p in ["/home", "/boot", "/mnt/bind", "/proc"] {
            assert_eq!(kind(&plan, p), "skip", "{p}");
        }
        // /home 被跳过，它下面的挂载点遇不到
        assert!(!plan.nested.contains_key(b"/home/u/nested".as_slice()));
    }

    #[test]
    fn root_inside_a_mount() {
        let mounts = vec![
            mount("/", "ext4", "8:1:/"),
            mount("/home", "ext4", "8:2:/"),
            mount("/home/u/again", "ext4", "8:2:/"),
            mount("/home/u/usb", "exfat", "8:9:/"),
            mount("/home/uu", "ext4", "8:7:/"),
        ];
        let plan = MountPlan::new(Os::Linux, Path::new("/home/u"), &mounts, true, &[]);
        assert_eq!(plan.root_mount(), Some(Path::new("/home")));
        assert_eq!(kind(&plan, "/home/u/again"), "skip");
        assert_eq!(kind(&plan, "/home/u/usb"), "include");
        // /home/uu 不在 /home/u 下面
        assert_eq!(plan.nested.len(), 2);
        let none = MountPlan::new(Os::Linux, Path::new("/x"), &[], true, &[]);
        assert_eq!(none.root_mount(), None);
        assert_eq!(kind(&none, "/x/y"), "normal");
    }

    #[test]
    fn macos_system_volumes_are_ignored() {
        let mut snapshot = mount(
            "/Volumes/com.apple.TimeMachine.localsnapshots",
            "apfs",
            "disk3s6",
        );
        snapshot.hidden = true;
        let mounts = vec![
            mount("/", "apfs", "disk3s1"),
            mount("/System/Volumes/Data", "apfs", "disk3s5"),
            snapshot,
            mount("/Volumes/USB", "exfat", "disk4s1"),
        ];
        let ignored = [PathBuf::from("/System/Volumes")];
        let plan = MountPlan::new(Os::Macos, Path::new("/"), &mounts, true, &ignored);
        assert_eq!(kind(&plan, "/System/Volumes"), "ignored");
        assert!(!plan.nested.contains_key(b"/System/Volumes/Data".as_slice()));
        assert_eq!(kind(&plan, "/Volumes/USB"), "include");
        assert_eq!(
            kind(&plan, "/Volumes/com.apple.TimeMachine.localsnapshots"),
            "skip"
        );
        assert_eq!(kind(&plan, "/System"), "normal");
    }

    #[test]
    fn windows_unicode_mount_names_and_device_aliases_share_keys() {
        let mounts = vec![
            mount("C:\\", "ntfs", "C:"),
            mount("C:\\Σ", "ntfs", "D:\\Ꭰ"),
            mount("C:\\ﬀ", "ntfs", "d:/ꭰ"),
            mount("C:\\ı", "ntfs", "E:"),
            mount("C:\\ß", "ntfs", "F:"),
        ];
        let plan = MountPlan::new(Os::Windows, Path::new("c:/"), &mounts, true, &[]);
        assert_eq!(kind(&plan, "c:/ς"), "include");
        assert_eq!(kind(&plan, "c:/σ"), "include");
        assert_eq!(kind(&plan, "c:/ﬀ"), "skip");
        assert_eq!(kind(&plan, "c:/FF"), "normal");
        assert_eq!(kind(&plan, "c:/I"), "include");
        assert_eq!(kind(&plan, "c:/İ"), "normal");
        assert_eq!(kind(&plan, "c:/ẞ"), "normal");
        assert_eq!(kind(&plan, "c:/ß"), "include");
    }

    #[test]
    fn windows_keys_ignore_case_and_separators() {
        let mounts = vec![
            mount("C:\\", "ntfs", "C:"),
            mount("D:\\", "ntfs", "D:"),
            mount("C:\\Mount\\Disk", "ntfs", "E:"),
        ];
        let plan = MountPlan::new(Os::Windows, Path::new("c:\\"), &mounts, true, &[]);
        assert_eq!(plan.root_mount(), Some(Path::new("C:\\")));
        assert_eq!(kind(&plan, "c:\\MOUNT\\disk"), "include");
        assert_eq!(kind(&plan, "C:/Mount/Disk"), "include");
        assert_eq!(kind(&plan, "C:\\Windows"), "normal");
        let users = MountPlan::new(Os::Windows, Path::new("C:\\Users"), &mounts, true, &[]);
        assert_eq!(users.root_mount(), Some(Path::new("C:\\")));
        assert!(users.nested.is_empty());
    }
}
