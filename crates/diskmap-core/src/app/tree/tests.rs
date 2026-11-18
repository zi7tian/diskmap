use super::*;

fn version(app: &App, state: ScanState) -> Version {
    Version {
        state,
        processes: app.procs().snapshot(),
        components: app.components(),
        settings: app.inner.settings_version.load(Ordering::Acquire),
    }
}

fn app() -> App {
    let db = Arc::new(PkgDb::default());
    App::with_components(
        db.clone(),
        Arc::new(Classifier::new(Some(db), &[])),
        ProcInfo::new(),
    )
}

fn key(number: u64) -> Key {
    Key {
        path: b"/tree".to_vec(),
        frac: number,
        lang: Lang::En,
    }
}

fn bytes(text: &str) -> Arc<[u8]> {
    text.as_bytes().into()
}

#[test]
fn response_cache_reuses_platform_equivalent_paths_only() {
    let app = app();
    let key = |os, path: &str| Key {
        path: platform::path::path_key(os, path.as_bytes()),
        frac: 1,
        lang: Lang::En,
    };
    let mut cache = TreeResponses::default();
    let first = cache
        .get_or_render(
            version(&app, ScanState::Done),
            key(Os::Windows, r"C:\ΟΣ\ı"),
            || Some(bytes("first")),
        )
        .unwrap();
    let alias = cache
        .get_or_render(
            version(&app, ScanState::Done),
            key(Os::Windows, "c:/οσ/I"),
            || panic!("等价路径应直接命中响应缓存"),
        )
        .unwrap();
    assert!(Arc::ptr_eq(&first, &alias));
    let distinct = cache
        .get_or_render(
            version(&app, ScanState::Done),
            key(Os::Windows, "c:/οσ/İ"),
            || Some(bytes("distinct")),
        )
        .unwrap();
    assert!(!Arc::ptr_eq(&first, &distinct));
    for os in [Os::Linux, Os::Macos] {
        let first = cache
            .get_or_render(version(&app, ScanState::Done), key(os, "/ΟΣ/ı"), || {
                Some(bytes("unix first"))
            })
            .unwrap();
        let distinct = cache
            .get_or_render(version(&app, ScanState::Done), key(os, "/οσ/I"), || {
                Some(bytes("unix distinct"))
            })
            .unwrap();
        assert!(!Arc::ptr_eq(&first, &distinct));
    }
}

#[test]
fn live_publications_and_all_terminal_transitions_never_reuse_old_bytes() {
    let app = app();
    let mut cache = TreeResponses::default();
    // 同一个正在扫描的会话先后发布目录（即使总字节数不变）也必须重新读取。
    for text in [
        "unlisted",
        "empty directory published",
        "hard link finalized",
    ] {
        let value = cache
            .get_or_render(version(&app, ScanState::Scanning), key(1), || {
                Some(bytes(text))
            })
            .unwrap();
        assert_eq!(&*value, text.as_bytes());
        assert!(cache.entries.is_empty());
    }
    // 完成、取消、失败都可能改变 state 和末尾计数，跨状态不能命中。
    for state in [ScanState::Done, ScanState::Cancelled, ScanState::Error] {
        let value = cache
            .get_or_render(version(&app, state), key(1), || Some(bytes(state.key())))
            .unwrap();
        assert_eq!(&*value, state.key().as_bytes());
        let hit = cache
            .get_or_render(version(&app, state), key(1), || panic!("must hit"))
            .unwrap();
        assert!(Arc::ptr_eq(&value, &hit));
    }
    let value = cache
        .get_or_render(version(&app, ScanState::Scanning), key(1), || {
            Some(bytes("live again"))
        })
        .unwrap();
    assert_eq!(&*value, b"live again");
    assert!(cache.entries.is_empty());
}

#[test]
fn version_changes_in_processes_settings_and_components_invalidate() {
    let app = app();
    let mut cache = TreeResponses::default();
    let mut last = cache
        .get_or_render(version(&app, ScanState::Done), key(1), || {
            Some(bytes("first"))
        })
        .unwrap();
    app.procs().install(std::iter::empty());
    for event in 0..3 {
        match event {
            0 => {}
            1 => app.invalidate_tree_responses(),
            _ => {
                let db = Arc::new(PkgDb::default());
                *app.inner.components.write().unwrap() = Arc::new(Components {
                    pkgdb: db.clone(),
                    classifier: Arc::new(Classifier::new(Some(db), &[])),
                });
            }
        }
        let value = cache
            .get_or_render(version(&app, ScanState::Done), key(1), || {
                Some(bytes("changed"))
            })
            .unwrap();
        assert!(!Arc::ptr_eq(&last, &value));
        last = value;
    }
}

#[test]
fn key_and_lru_bounds_preserve_recent_entries() {
    let app = app();
    let mut cache = TreeResponses::default();
    for n in 0..8 {
        cache
            .get_or_render(version(&app, ScanState::Done), key(n), || {
                Some(bytes("entry"))
            })
            .unwrap();
    }
    cache.get_or_render(version(&app, ScanState::Done), key(0), || {
        panic!("must hit")
    });
    cache.get_or_render(version(&app, ScanState::Done), key(8), || {
        Some(bytes("new"))
    });
    assert_eq!(cache.entries.len(), 8);
    assert!(cache.entries.iter().all(|(k, _)| k.frac != 1));
    for changed in [
        Key {
            lang: Lang::Zh,
            ..key(0)
        },
        Key {
            path: b"/other".to_vec(),
            ..key(0)
        },
    ] {
        let value = cache
            .get_or_render(version(&app, ScanState::Done), changed, || {
                Some(bytes("different key"))
            })
            .unwrap();
        assert_eq!(&*value, b"different key");
    }
    let big: Arc<[u8]> = vec![0; MAX_BYTES / 3 + 1].into();
    for n in 10..14 {
        cache.get_or_render(version(&app, ScanState::Done), key(n), || Some(big.clone()));
    }
    assert_eq!(cache.entries.len(), 2);
    assert!(cache.bytes <= MAX_BYTES);
    let oversized: Arc<[u8]> = vec![0; MAX_BYTES + 1].into();
    cache.get_or_render(version(&app, ScanState::Done), key(99), || Some(oversized));
    assert!(cache.entries.iter().all(|(k, _)| k.frac != 99));
    assert!(cache.bytes <= MAX_BYTES);
}

#[test]
fn late_render_cannot_restore_an_invalidated_version_or_duplicate_a_key() {
    let app = app();
    let mut cache = TreeResponses::default();
    let old = version(&app, ScanState::Done);
    assert!(cache.get(old.clone(), &key(1)).is_none());
    app.invalidate_tree_responses();
    let current = version(&app, ScanState::Done);
    let fresh = cache
        .get_or_render(current.clone(), key(1), || Some(bytes("fresh")))
        .unwrap();
    cache.insert(&old, key(1), bytes("stale"));
    assert!(Arc::ptr_eq(
        &fresh,
        &cache.get(current.clone(), &key(1)).unwrap()
    ));
    cache.insert(&current, key(1), bytes("same version"));
    assert_eq!(cache.entries.len(), 1);
    assert_eq!(cache.bytes, b"same version".len());
}
