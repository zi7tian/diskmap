use super::*;
use std::fs;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};

struct Temp(PathBuf);
impl Temp {
    fn new() -> io::Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "diskmap-pkgdb-{}-{stamp}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path)?;
        Ok(Self(path))
    }
    fn write(&self, path: &str, bytes: &[u8]) -> io::Result<()> {
        let file = self.0.join(path);
        if let Some(parent) = file.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(file, bytes)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn directory_lookup_and_backup_insertion_order() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<PkgDb>();
    let mut db = PkgDb::new(Os::Linux);
    db.insert_backup(Path::new("/etc/demo.conf"));
    db.insert_file(Path::new("/etc/demo.conf"), "demo");
    db.insert_file(Path::new("/etc/demo.conf"), "demo-new");
    db.insert_file(Path::new("/usr/bin/demo"), "demo");
    db.insert_backup(Path::new("/unowned"));
    assert_eq!(db.file_count(), 2);
    assert_eq!(db.owner(Path::new("/etc/demo.conf")), Some("demo-new"));
    assert!(db.is_backup(Path::new("/etc/demo.conf")));
    assert!(db.is_backup(Path::new("/unowned")));
    assert_eq!(db.owner(Path::new("/unowned")), None);
    assert_eq!(db.directory_id(Path::new("/missing")), None);
    let id = db.directory_id(Path::new("/etc/")).expect("目录已登记");
    assert_eq!(
        db.lookup_in_dir(id, OsStr::new("demo.conf")),
        Some(FileInfo {
            owner: Some("demo-new"),
            backup: true
        })
    );
    assert_eq!(db.lookup_in_dir(id, OsStr::new("Demo.conf")), None);
    assert_eq!(db.lookup_in_dir(u32::MAX, OsStr::new("demo.conf")), None);
}

#[test]
fn windows_paths_use_windows_semantics_on_every_host() {
    let mut db = PkgDb::new(Os::Windows);
    db.insert_file(Path::new("C:/Demo/İΣ.DAT"), "demo");
    db.insert_backup(Path::new("c:\\demo\\İΣ.DAT"));
    db.insert_file(Path::new("C:\\root.sys"), "kernel");
    let id = db.directory_id(Path::new("c:\\DEMO")).expect("目录已登记");
    assert_eq!(
        db.lookup_in_dir(id, OsStr::new("İΣ.DAT")),
        Some(FileInfo {
            owner: Some("demo"),
            backup: true
        })
    );
    assert_eq!(db.owner(Path::new("C:/ROOT.SYS")), Some("kernel"));
    assert_ne!(db.directory_id(Path::new("D:\\demo")), Some(id));
}

#[test]
fn pacman_sections_package_names_and_backups() -> io::Result<()> {
    let temp = Temp::new()?;
    temp.write("my-demo-1.2-3/files", b"%FILES%\nusr/\nusr/bin/demo\netc/demo.conf\n\n%BACKUP%\netc/demo.conf\tabcd\netc/orphan.conf\tffff\n\n%OTHER%\nnot/a/file\n")?;
    temp.write("no-files-1-1/desc", b"ignored")?;
    temp.write("not-a-directory", b"ignored")?;
    let db = PkgDb::load_pacman(&temp.0);
    assert_eq!(db.manager, Some("pacman"));
    assert_eq!(db.npkgs, 1);
    assert_eq!(db.file_count(), 2);
    assert_eq!(db.owner(Path::new("/usr/bin/demo")), Some("my-demo"));
    assert_eq!(db.owner(Path::new("/usr")), None);
    assert!(db.is_backup(Path::new("/etc/demo.conf")));
    assert!(db.is_backup(Path::new("/etc/orphan.conf")));
    assert!(db.error.is_none());
    Ok(())
}

#[test]
fn dpkg_arch_names_directories_and_conffiles() -> io::Result<()> {
    let temp = Temp::new()?;
    // NTFS 把冒号解释为 ADS；Windows 仍验证目录和 conffiles，Unix 保留真实架构后缀覆盖。
    let list_name = if cfg!(windows) {
        "editor.list"
    } else {
        "editor:amd64.list"
    };
    temp.write(
        list_name,
        b"/.\n/usr\n/usr/bin/editor\n/etc/editor.conf\n\n",
    )?;
    temp.write(
        "editor.conffiles",
        b"\xc2\xa0/etc/editor.conf\n/etc/orphan obsolete\n\n",
    )?;
    // 无法打开的 .list 仍计入包数，和旧版一致。
    fs::create_dir(temp.0.join("unreadable.list"))?;
    let db = PkgDb::load_dpkg(&temp.0);
    assert_eq!(db.manager, Some("dpkg"));
    assert_eq!(db.npkgs, 2);
    assert_eq!(db.file_count(), 3);
    assert_eq!(db.owner(Path::new("/usr")), Some("editor"));
    assert_eq!(db.owner(Path::new("/usr/bin/editor")), Some("editor"));
    assert!(db.is_backup(Path::new("/etc/editor.conf")));
    assert!(db.is_backup(Path::new("/etc/orphan")));
    assert_eq!(db.owner(Path::new("/.")), None);
    Ok(())
}

#[test]
fn rpm_first_owner_wins_and_config_flags_accumulate() {
    let db = PkgDb::load_rpm_output(b"/usr/bin/a\tpkg-one\t\n/etc/a\tpkg-one\tcn\n/usr/bin/a\tpkg-two\tc\ninvalid\n/etc/orphan\tpkg-three\n");
    assert_eq!(db.manager, Some("rpm"));
    assert_eq!(db.npkgs, 3);
    assert_eq!(db.file_count(), 3);
    assert_eq!(db.owner(Path::new("/usr/bin/a")), Some("pkg-one"));
    assert!(db.is_backup(Path::new("/usr/bin/a")));
    assert!(db.is_backup(Path::new("/etc/a")));
    assert!(!db.is_backup(Path::new("/etc/orphan")));
}

#[test]
fn homebrew_roots_and_missing_databases() -> io::Result<()> {
    let temp = Temp::new()?;
    fs::create_dir_all(temp.0.join("arm/Cellar/git/2.0"))?;
    fs::create_dir_all(temp.0.join("intel/Caskroom/editor"))?;
    temp.write("arm/Cellar/not-a-directory", b"ignored")?;
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        temp.0.join("arm/Cellar/git"),
        temp.0.join("arm/Cellar/link"),
    )?;
    let db = PkgDb::load_homebrew_roots(&[temp.0.join("arm"), temp.0.join("intel")]);
    assert_eq!(db.manager, Some("brew"));
    assert_eq!(db.npkgs, 2);
    assert_eq!(
        db.app_dirs().collect::<Vec<_>>(),
        vec![
            (temp.0.join("arm/Cellar/git").as_path(), "git（Homebrew）"),
            (
                temp.0.join("intel/Caskroom/editor").as_path(),
                "editor（Homebrew）"
            )
        ]
    );
    let missing = temp.0.join("missing");
    assert!(
        PkgDb::load_homebrew_roots(std::slice::from_ref(&missing))
            .manager
            .is_none()
    );
    for db in [PkgDb::load_pacman(&missing), PkgDb::load_dpkg(&missing)] {
        assert!(db.error.is_some());
        assert_eq!(db.file_count(), 0);
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn unix_database_paths_preserve_non_utf8() -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let temp = Temp::new()?;
    temp.write(
        "demo-1-1/files",
        b"%FILES%\nusr/share/\xffdata\n\n%BACKUP%\nusr/share/\xffdata\t123\n",
    )?;
    let db = PkgDb::load_pacman(&temp.0);
    let path = Path::new(OsStr::from_bytes(b"/usr/share/\xffdata"));
    assert_eq!(db.owner(path), Some("demo"));
    assert!(db.is_backup(path));
    let id = db
        .directory_id(Path::new("/usr/share"))
        .expect("目录已登记");
    assert_eq!(
        db.lookup_in_dir(id, OsStr::from_bytes(b"\xffdata"))
            .and_then(|v| v.owner),
        Some("demo")
    );
    Ok(())
}

#[test]
fn status_json_has_legacy_keys() {
    // 避免序列化 Text 时触发默认语言的真实设置文件读取。
    let _language = crate::i18n::scope(crate::i18n::Lang::En);
    let db = PkgDb::load_rpm_output(b"/demo\tdemo\t\n");
    let value = serde_json::to_value(db.status()).expect("状态可序列化");
    assert_eq!(value["manager"], "rpm");
    assert_eq!(value["packages"], 1);
    assert_eq!(value["files"], 1);
    assert_eq!(value["label"], "rpm");
    assert!(value["error"].is_null());
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "只读本机数据库的性能测量，普通测试不依赖真实系统"]
fn native_load_benchmark() {
    fn rss_kib() -> usize {
        fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|text| {
                text.lines()
                    .find(|line| line.starts_with("VmRSS:"))
                    .and_then(|line| line.split_whitespace().nth(1))
                    .and_then(|value| value.parse().ok())
            })
            .unwrap_or(0)
    }
    let before = rss_kib();
    let db = PkgDb::load();
    let after = rss_kib();
    println!(
        "manager={:?} packages={} files={} directories={} load_ms={:.3} rss_before_kib={} rss_after_kib={} rss_delta_kib={}",
        db.manager,
        db.npkgs,
        db.file_count(),
        db.directory_count(),
        db.load_time * 1000.0,
        before,
        after,
        after.saturating_sub(before)
    );
    assert!(db.error.is_none(), "{:?}", db.error);
    if let Some(id) = db.directory_id(Path::new("/usr/bin")) {
        let started = Instant::now();
        for _ in 0..1_000_000 {
            std::hint::black_box(db.lookup_in_dir(id, std::hint::black_box(OsStr::new("ls"))));
        }
        println!(
            "lookup_count=1000000 lookup_ms={:.3}",
            started.elapsed().as_secs_f64() * 1000.0
        );
    }
}

#[test]
fn loaders_follow_python_universal_newline_rules() -> io::Result<()> {
    let pacman = Temp::new()?;
    pacman.write(
        "demo-1-1/files",
        b"%FILES%\r\nusr/bin/demo\r\netc/demo\r\n\r\n%BACKUP%\retc/demo\thash\r",
    )?;
    let db = PkgDb::load_pacman(&pacman.0);
    assert_eq!(db.file_count(), 2);
    assert_eq!(db.owner(Path::new("/usr/bin/demo")), Some("demo"));
    assert!(db.is_backup(Path::new("/etc/demo")));
    let dpkg = Temp::new()?;
    dpkg.write("demo.list", b"/.\r\n/usr/bin/demo\r/etc/demo\r\n")?;
    dpkg.write("demo.conffiles", b"/etc/demo\r\n")?;
    let db = PkgDb::load_dpkg(&dpkg.0);
    assert_eq!(db.file_count(), 2);
    assert!(db.is_backup(Path::new("/etc/demo")));
    let db = PkgDb::load_rpm_output(b"/usr/bin/demo\tdemo\t\r\n/etc/demo\tdemo\tc\r");
    assert_eq!(db.file_count(), 2);
    assert!(db.is_backup(Path::new("/etc/demo")));
    Ok(())
}

#[test]
fn repeated_separators_and_relative_keys_are_distinct() {
    let mut db = PkgDb::new(Os::Linux);
    for path in ["/f", "//f", "/data/f", "/data//f", "f"] {
        db.insert_file(Path::new(path), path);
    }
    for (dir, path) in [
        ("/", "/f"),
        ("//", "//f"),
        ("/data", "/data/f"),
        ("/data//", "/data//f"),
        ("", "f"),
    ] {
        assert_eq!(db.owner(Path::new(path)), Some(path));
        let id = db.directory_id(Path::new(dir)).expect("父目录键已登记");
        assert_eq!(
            db.lookup_in_dir(id, OsStr::new("f")).and_then(|v| v.owner),
            Some(path)
        );
    }
    for path in ["/opt/demo", "/opt//demo", "/opt/./demo"] {
        db.insert_app_dir(Path::new(path), path);
    }
    assert_eq!(db.app_dirs().count(), 3);
}

#[test]
fn application_directory_aliases_share_one_entry() {
    let mut db = PkgDb::new(Os::Windows);
    db.insert_app_dir(Path::new("C:/Apps/ΟΣ"), "first");
    db.insert_app_dir(Path::new("c:\\apps\\οσ"), "second");
    db.insert_app_dir(Path::new("C:/Apps/ß"), "sharp s");
    db.insert_app_dir(Path::new("C:/Apps/ẞ"), "capital sharp s");
    assert_eq!(
        db.app_dirs().collect::<Vec<_>>(),
        vec![
            (Path::new("C:/Apps/ΟΣ"), "second"),
            (Path::new("C:/Apps/ß"), "sharp s"),
            (Path::new("C:/Apps/ẞ"), "capital sharp s"),
        ]
    );
}
