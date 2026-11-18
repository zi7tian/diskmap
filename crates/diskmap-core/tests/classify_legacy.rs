//! 固定的 Python 旧版输出；测试时只需 Rust，无需 Python 或本机数据库。

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use diskmap_core::classify::{CLASSES, Classifier, ClassifierOptions, DirClass};
use diskmap_core::i18n::{self, Lang};
use diskmap_core::pkgdb::PkgDb;
use diskmap_core::platform::Os;
use serde::Deserialize;

#[derive(Deserialize)]
struct Fixture {
    schema_version: u8,
    case_columns: Vec<String>,
    expected_columns: [String; 2],
    reason_columns: [String; 2],
    reasons: Vec<[String; 2]>,
    classes: HashMap<String, serde_json::Value>,
    scenarios: Vec<Scenario>,
}

#[derive(Deserialize)]
struct Scenario {
    name: String,
    os: String,
    env: HashMap<String, String>,
    homes: Vec<PathBuf>,
    files: HashMap<String, String>,
    backup: Vec<PathBuf>,
    app_dirs: HashMap<String, String>,
    swapfiles: Vec<PathBuf>,
    cases: Vec<Case>,
}

// 紧凑 JSON 的列顺序由顶层 case_columns 声明；保持命名字段便于测试调用与报错。
type CaseRow = (
    PathBuf,
    bool,
    PathBuf,
    String,
    PathBuf,
    Expected,
    Expected,
    Option<Expected>,
    Option<usize>,
    Expected,
    Vec<String>,
);

#[derive(Deserialize)]
#[serde(from = "CaseRow")]
struct Case {
    path: PathBuf,
    is_dir: bool,
    parent: PathBuf,
    name: String,
    scan_path: PathBuf,
    root: Expected,
    parent_class: Expected,
    dir: Option<Expected>,
    file: Option<usize>,
    explain: Expected,
}

impl From<CaseRow> for Case {
    fn from(row: CaseRow) -> Self {
        let (path, is_dir, parent, name, scan_path, root, parent_class, dir, file, explain, _tags) =
            row;
        Self {
            path,
            is_dir,
            parent,
            name,
            scan_path,
            root,
            parent_class,
            dir,
            file,
            explain,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(from = "(usize, usize)")]
struct Expected {
    category: usize,
    reason: usize,
}

impl From<(usize, usize)> for Expected {
    fn from((category, reason): (usize, usize)) -> Self {
        Self { category, reason }
    }
}

fn assert_class(
    c: &Classifier,
    actual: DirClass,
    expected: &Expected,
    reasons: &[[String; 2]],
    label: &str,
) {
    let [zh, en] = &reasons[expected.reason];
    assert_eq!(actual.0.index(), expected.category, "{label}: category");
    assert_eq!(c.reason(actual.1).get(Lang::Zh), zh, "{label}: zh");
    assert_eq!(c.reason(actual.1).get(Lang::En), en, "{label}: en");
}

#[test]
fn every_rule_and_explanation_matches_legacy() {
    let fixture: Fixture = serde_json::from_str(include_str!("fixtures/classify/legacy.json"))
        .expect("有效的旧版 JSON");
    assert_eq!(fixture.schema_version, 2);
    assert_eq!(
        fixture.case_columns,
        [
            "path",
            "is_dir",
            "parent",
            "name",
            "scan_path",
            "root",
            "parent_class",
            "dir",
            "file",
            "explain",
            "tags"
        ]
    );
    assert_eq!(fixture.expected_columns, ["category", "reason"]);
    assert_eq!(fixture.reason_columns, ["zh", "en"]);
    for lang in [Lang::Zh, Lang::En] {
        let _guard = i18n::scope(lang);
        assert_eq!(
            serde_json::to_value(&CLASSES).expect("序列化类别"),
            fixture.classes[lang.code()]
        );
    }
    let mut count = 0;
    let mut approved_differences = 0;
    for scenario in fixture.scenarios {
        let os = match scenario.os.as_str() {
            "linux" => Os::Linux,
            "macos" => Os::Macos,
            "windows" => Os::Windows,
            other => panic!("未知系统：{other}"),
        };
        let mut db = PkgDb::new(os);
        for (path, owner) in scenario.files {
            db.insert_file(Path::new(&path), &owner);
        }
        for path in scenario.backup {
            db.insert_backup(&path);
        }
        for (path, name) in scenario.app_dirs {
            db.insert_app_dir(Path::new(&path), &name);
        }
        let c = Classifier::with_options(
            Some(Arc::new(db)),
            &scenario.swapfiles,
            ClassifierOptions {
                os,
                env: scenario.env,
                homes: Some(scenario.homes),
            },
        );
        for mut case in scenario.cases {
            count += 1;
            // T15 明确改为逐码元 simple uppercase；旧 lower() 会把 Kelvin 符号折叠为 k。
            // 保留旧版 fixture，只对以下五条受影响结果核验差异；继承系统类别的两条仍原样比较。
            if os == Os::Windows {
                match case.path.to_str().unwrap() {
                    "C:\\Users\\Bob\\.config\\Code\\Service WorKer"
                    | "C:\\data\\Service WorKer" => {
                        assert_eq!(case.root.category, 5);
                        assert_eq!(case.dir.as_ref().unwrap().category, 5);
                        assert_eq!(case.explain.category, 5);
                        case.root = case.parent_class.clone();
                        case.dir = Some(case.parent_class.clone());
                        case.explain = case.parent_class.clone();
                        approved_differences += 1;
                    }
                    "C:\\Users\\Bob\\download.baK"
                    | "C:\\data\\download.BAK"
                    | "C:\\data\\download.baK" => {
                        assert_eq!(case.file, Some(5));
                        assert_eq!(case.explain.category, 5);
                        case.file = Some(case.parent_class.category);
                        case.explain = case.parent_class.clone();
                        approved_differences += 1;
                    }
                    _ => {}
                }
            }
            let label = format!("{} {:?} is_dir={}", scenario.name, case.path, case.is_dir);
            assert_class(
                &c,
                c.root_class(&case.path),
                &case.root,
                &fixture.reasons,
                &format!("{label} root"),
            );
            let parent = c.root_class(&case.parent);
            assert_class(
                &c,
                parent,
                &case.parent_class,
                &fixture.reasons,
                &format!("{label} parent"),
            );
            let ctx = c.enter_dir(parent, &case.parent);
            assert_eq!(ctx.class(), parent);
            if let Some(expected) = &case.dir {
                assert_class(
                    &c,
                    c.dir_class(&ctx, OsStr::new(&case.name), &case.scan_path),
                    expected,
                    &fixture.reasons,
                    &format!("{label} dir"),
                );
            }
            if let Some(expected) = case.file {
                assert_eq!(
                    c.file_class(&ctx, OsStr::new(&case.name)).index(),
                    expected,
                    "{label} file"
                );
            }
            let (category, reason) = c.explain(&case.path, case.is_dir);
            assert_eq!(
                category.index(),
                case.explain.category,
                "{label} explain category"
            );
            let [zh, en] = &fixture.reasons[case.explain.reason];
            assert_eq!(reason.get(Lang::Zh), zh, "{label} explain zh");
            assert_eq!(reason.get(Lang::En), en, "{label} explain en");
            if !case.is_dir {
                assert_eq!(
                    c.explain_file(
                        parent,
                        Path::new(&case.parent),
                        OsStr::new(&case.name),
                        &case.scan_path
                    ),
                    (category, reason),
                    "{label} explain_file"
                );
            }
        }
    }
    assert_eq!(count, 1987, "改变样例时同时更新覆盖数量");
    assert_eq!(approved_differences, 5, "T15 与旧版的 Kelvin 符号差异");
}
