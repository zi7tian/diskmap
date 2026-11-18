//! Windows 卸载登记的安装目录；目录过滤逻辑可在其他系统上测试。

use std::collections::HashSet;
use std::path::Path;

use super::PkgDb;
use crate::platform::Os;
use crate::platform::path::{cmp_paths, path_key};

fn broad_directories(env: impl Fn(&str) -> Option<String>) -> HashSet<Vec<u8>> {
    let drive = env("SystemDrive").unwrap_or_else(|| "C:".to_owned());
    let value =
        |key: &str, fallback: String| env(key).filter(|s| !s.is_empty()).unwrap_or(fallback);
    [
        value("ProgramFiles", format!("{drive}\\Program Files")),
        value("ProgramFiles(x86)", format!("{drive}\\Program Files (x86)")),
        value("ProgramData", format!("{drive}\\ProgramData")),
        value("SystemRoot", format!("{drive}\\Windows")),
        value("LOCALAPPDATA", String::new()),
        value("APPDATA", String::new()),
        format!("{drive}\\Users"),
        value("USERPROFILE", String::new()),
    ]
    .into_iter()
    .filter(|s| !s.is_empty())
    .map(|s| path_key(Os::Windows, s.as_bytes()))
    .collect()
}

fn register_app(db: &mut PkgDb, name: &str, location: &str, broad: &HashSet<Vec<u8>>) -> bool {
    if name.is_empty() {
        return false;
    }
    let location = location
        .trim()
        .trim_matches('"')
        .trim_end_matches(['\\', '/']);
    let key = path_key(Os::Windows, location.as_bytes());
    // ntpath.isabs：盘符后的根分隔符或 UNC 路径；不能接受相对盘符路径 C:foo。
    let absolute =
        key.starts_with(b"\\\\") || (key.get(1) == Some(&b':') && key.get(2) == Some(&b'\\'));
    if location.chars().count() < 4 || !absolute || broad.contains(&key) {
        return false;
    }
    let p = Path::new(location);
    if !db.apps.iter().any(|(existing, _)| {
        cmp_paths(
            Os::Windows,
            existing.as_os_str().as_encoded_bytes(),
            p.as_os_str().as_encoded_bytes(),
        )
        .is_eq()
    }) {
        db.insert_app_dir(p, name);
    }
    true
}

#[cfg(windows)]
impl PkgDb {
    pub(super) fn read_windows(&mut self) {
        use crate::i18n::Text;
        use winreg::{
            RegKey,
            enums::{
                HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY,
                REG_DWORD, REG_DWORD_BIG_ENDIAN, REG_EXPAND_SZ, REG_QWORD, REG_SZ,
            },
            types::FromRegValue,
        };

        self.set_manager("windows");
        self.label = Some(Text::new("已安装的程序", "Installed programs"));
        let broad = broad_directories(|key| std::env::var(key).ok());
        let mut names = HashSet::new();
        for (hive, view) in [
            (HKEY_LOCAL_MACHINE, KEY_WOW64_64KEY),
            (HKEY_LOCAL_MACHINE, KEY_WOW64_32KEY),
            (HKEY_CURRENT_USER, 0),
        ] {
            let Ok(root) = RegKey::predef(hive).open_subkey_with_flags(
                r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall",
                KEY_READ | view,
            ) else {
                continue;
            };
            for sub in root.enum_keys().flatten() {
                let Ok(key) = root.open_subkey_with_flags(sub, KEY_READ | view) else {
                    continue;
                };
                let (Ok(raw_name), Ok(raw_location)) = (
                    key.get_raw_value("DisplayName"),
                    key.get_raw_value("InstallLocation"),
                ) else {
                    continue;
                };
                // Python 的 InstallLocation 必须是字符串；REG_MULTI_SZ 不能当作一个路径。
                if !matches!(raw_location.vtype, REG_SZ | REG_EXPAND_SZ) {
                    continue;
                }
                let Ok(location) = String::from_reg_value(&raw_location) else {
                    continue;
                };
                let name = match raw_name.vtype {
                    REG_SZ | REG_EXPAND_SZ => String::from_reg_value(&raw_name).ok(),
                    REG_DWORD | REG_DWORD_BIG_ENDIAN => u32::from_reg_value(&raw_name)
                        .ok()
                        .filter(|&v| v != 0)
                        .map(|v| v.to_string()),
                    REG_QWORD => u64::from_reg_value(&raw_name)
                        .ok()
                        .filter(|&v| v != 0)
                        .map(|v| v.to_string()),
                    _ => None,
                };
                let Some(name) = name else {
                    continue;
                };
                if register_app(self, &name, &location, &broad) {
                    names.insert(name);
                }
            }
        }
        self.npkgs = names.len();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_broad_and_invalid_install_locations() {
        let broad = broad_directories(|key| match key {
            "USERPROFILE" => Some(r"C:\Users\Alice".into()),
            _ => None,
        });
        let mut db = PkgDb::new(Os::Windows);
        for location in [
            "C:\\",
            "C:",
            "C:foo",
            "relative\\foo",
            r"C:\Program Files",
            r"c:\WINDOWS",
            r"C:\Users\Alice",
        ] {
            assert!(
                !register_app(&mut db, "bad", location, &broad),
                "{location}"
            );
        }
        assert!(register_app(
            &mut db,
            "Editor",
            "  \"C:\\Program Files\\Editor\\\"  ",
            &broad
        ));
        assert!(register_app(
            &mut db,
            "Ignored duplicate",
            r"c:/PROGRAM FILES/editor",
            &broad
        ));
        assert!(register_app(
            &mut db,
            "Remote",
            r"\\server\share\app",
            &broad
        ));
        assert_eq!(
            db.app_dirs().collect::<Vec<_>>(),
            vec![
                (Path::new(r"C:\Program Files\Editor"), "Editor"),
                (Path::new(r"\\server\share\app"), "Remote")
            ]
        );
    }
}
