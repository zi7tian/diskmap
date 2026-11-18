use super::*;
use crate::i18n::Lang;

fn linux(home: &str) -> Classifier {
    let mut options = ClassifierOptions::for_os(Os::Linux);
    options.homes = Some(vec![home.into()]);
    Classifier::with_options(None, &[], options)
}

#[test]
fn classifier_and_context_are_shareable() {
    fn assert_traits<T: Send + Sync>() {}
    assert_traits::<Classifier>();
    assert_traits::<DirContext>();
    assert!(std::mem::size_of::<DirContext>() <= 32);
}

#[test]
fn scan_order_and_parallel_queries() {
    let c = linux("/home/alice");
    let parent = Path::new("/home/alice");
    let context = c.enter_dir(c.root_class(parent), parent);
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let c = &c;
            let context = &context;
            scope.spawn(move || {
                let path = Path::new("/home/alice/.config");
                let class = c.dir_class(context, OsStr::new(".config"), path);
                assert_eq!(class.0, Category::Config);
                let cfg = c.enter_dir(class, path);
                let cached = c.dir_class(
                    &cfg,
                    OsStr::new("Cache"),
                    Path::new("/home/alice/.config/Cache"),
                );
                assert_eq!(cached.0, Category::Cache);
                assert_eq!(
                    c.file_class(context, OsStr::new(".bashrc")),
                    Category::Config
                );
            });
        }
    });
}

#[test]
fn all_static_texts_have_english() {
    for specs in [
        rules::LINUX_DIRS,
        rules::LINUX_FILES,
        rules::LINUX_HOME,
        rules::MAC_DIRS,
        rules::MAC_HOME,
        rules::WIN_DIRS,
        rules::WIN_FILES,
        rules::WIN_HOME,
        rules::COMMON_HOME,
    ] {
        for (_, _, text) in specs {
            assert!(!text.en.is_empty());
            assert!(!text.en.chars().any(|c| ('一'..='鿿').contains(&c)));
        }
    }
    for class in CLASSES {
        assert!(!class.label.en.is_empty());
        assert!(!class.desc.en.chars().any(|c| ('一'..='鿿').contains(&c)));
    }
}

#[test]
fn backup_beats_core_and_unowned_backup_has_no_effect() {
    let mut db = PkgDb::new(Os::Linux);
    db.insert_file(Path::new("/boot/settings"), "bootloader");
    db.insert_backup(Path::new("/boot/settings"));
    db.insert_backup(Path::new("/data/unowned"));
    let c = Classifier::with_options(
        Some(Arc::new(db)),
        &[],
        ClassifierOptions::for_os(Os::Linux),
    );
    assert_eq!(
        c.explain(Path::new("/boot/settings"), false).0,
        Category::Config
    );
    assert_eq!(
        c.explain(Path::new("/data/unowned"), false).0,
        Category::Other
    );
}

#[test]
fn app_rules_do_not_replace_fixed_rules() {
    let mut db = PkgDb::new(Os::Linux);
    db.insert_app_dir(Path::new("/usr"), "不要覆盖");
    db.insert_app_dir(Path::new("/custom/app"), "Demo");
    let c = Classifier::with_options(
        Some(Arc::new(db)),
        &[],
        ClassifierOptions::for_os(Os::Linux),
    );
    assert_eq!(
        c.explain(Path::new("/usr"), true).1.get(Lang::En),
        "System programs /usr"
    );
    let (_, id) = c.root_class(Path::new("/custom/app/bin"));
    assert_eq!(c.reason(id).get(Lang::En), "Installed program: Demo");
}

#[cfg(unix)]
#[test]
fn non_utf8_home_file_package_and_swap_paths_are_lossless() {
    use std::os::unix::ffi::OsStrExt;
    let home = Path::new(OsStr::from_bytes(b"/home/\xffalice"));
    let file = Path::new(OsStr::from_bytes(b"/home/\xffalice/\xfepackage"));
    let swap = PathBuf::from(OsStr::from_bytes(b"/data/\xffswap"));
    let mut db = PkgDb::new(Os::Linux);
    db.insert_file(file, "demo");
    let mut options = ClassifierOptions::for_os(Os::Linux);
    options.homes = Some(vec![home.to_path_buf()]);
    let c = Classifier::with_options(Some(Arc::new(db)), std::slice::from_ref(&swap), options);
    let ctx = c.enter_dir(c.root_class(home), home);
    assert_eq!(
        c.file_class(&ctx, OsStr::from_bytes(b"\xfepackage")),
        Category::System
    );
    assert_eq!(
        c.file_class(&ctx, OsStr::from_bytes(b"\xfe.log")),
        Category::Cache
    );
    assert_eq!(
        c.file_class(&ctx, OsStr::from_bytes(b".\xfe")),
        Category::Config
    );
    let config = Path::new(OsStr::from_bytes(b"/home/\xffalice/.config"));
    assert_eq!(c.root_class(config).0, Category::Config);
    assert_eq!(
        c.explain(file, false).1.get(Lang::En),
        "Installed by package demo"
    );
    let parent = Path::new("/data");
    assert_eq!(
        c.file_class(
            &c.enter_dir(c.root_class(parent), parent),
            OsStr::from_bytes(b"\xffswap")
        ),
        Category::Core
    );
    assert_eq!(c.explain(&swap, false).0, Category::Core);
}

#[test]
fn direct_windows_calls_normalize_separators_and_unicode() {
    let options = ClassifierOptions::for_os(Os::Windows);
    let c = Classifier::with_options(None, &[], options);
    let root = c.enter_dir(c.root_class(Path::new("C:/")), Path::new("C:/"));
    assert_eq!(
        c.dir_class(&root, OsStr::new("WINDOWS"), Path::new("c:/WINDOWS"))
            .0,
        Category::System
    );
    let data = Path::new("C:/data");
    let ctx = c.enter_dir(c.root_class(data), data);
    assert_eq!(
        c.file_class(&ctx, OsStr::new("download.baK")),
        Category::Other
    );
    assert_eq!(
        c.dir_class(
            &ctx,
            OsStr::new("Service WorKer"),
            Path::new("C:/data/Service WorKer")
        )
        .0,
        Category::Other
    );
    // 与旧版 lower() 有意不同：Kelvin 符号不转 K，长 s 和无点 i 则映射到 S / I。
    assert_eq!(
        c.file_class(&ctx, OsStr::new("download.ſWp")),
        Category::Cache
    );
    assert_eq!(
        c.file_class(&ctx, OsStr::new("download.LoG")),
        Category::Cache
    );
    assert_eq!(c.file_class(&ctx, OsStr::new("download~")), Category::Cache);
    assert_eq!(
        c.dir_class(
            &ctx,
            OsStr::new("Servıce Worker"),
            Path::new("C:/data/Servıce Worker")
        )
        .0,
        Category::Cache
    );
}

#[test]
fn registry_prefix_uses_the_same_windows_name_equivalence() {
    let mut options = ClassifierOptions::for_os(Os::Windows);
    options.homes = Some(vec![PathBuf::from("C:/Users/Alice")]);
    let c = Classifier::with_options(None, &[], options);
    let home = Path::new("c:/users/ALICE");
    let ctx = c.enter_dir(c.root_class(home), home);
    for name in ["NTUSER.DAT", "NTUſER.DaT"] {
        assert_eq!(c.file_class(&ctx, OsStr::new(name)), Category::Config);
        assert_eq!(
            c.explain(Path::new(&format!("C:/Users/Alice/{name}")), false)
                .0,
            Category::Config
        );
    }
    assert_eq!(c.file_class(&ctx, OsStr::new("NTUSERDAT")), Category::User);
}

#[test]
fn injected_unicode_drive_uses_character_boundaries() {
    let mut options = ClassifierOptions::for_os(Os::Windows);
    options.env.insert("SystemDrive".into(), "Å:".into());
    let c = Classifier::with_options(None, &[], options);
    assert_eq!(c.root_class(Path::new("Å:/Windows")).0, Category::System);
    assert_eq!(
        c.explain(Path::new("Å:/pagefile.sys"), false).0,
        Category::Core
    );
    assert_eq!(drive_end(Os::Windows, "Å:relative".as_bytes()), 3);
}
