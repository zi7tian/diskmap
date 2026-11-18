use super::*;
use std::io::Read;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "diskmap-t16-instance-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn held_reader_allows_atomic_replacement_and_keeps_its_original_snapshot() {
    let fixture = Fixture::new();
    let path = fixture.0.join("instance.json");
    write_at(&path, 1234, "old", "test").unwrap();
    let mut reader = open_shared(&path).unwrap();
    write_at(&path, 5678, "new", "test").unwrap();
    let mut original = String::new();
    reader.read_to_string(&mut original).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&original).unwrap()["token"],
        "old"
    );
    assert_eq!(read_at(&path).unwrap()["token"], "new");
}

#[test]
fn retries_only_transient_windows_errors_and_stops_at_its_budget() {
    let mut attempts = 0;
    replace_with_retry(Duration::from_secs(1), || {
        attempts += 1;
        match attempts {
            1 => Err(io::Error::from_raw_os_error(5)),
            2 => Err(io::Error::from_raw_os_error(32)),
            3 => Err(io::Error::from_raw_os_error(33)),
            _ => Ok(()),
        }
    })
    .unwrap();
    assert_eq!(attempts, 4);
    attempts = 0;
    let error = replace_with_retry(Duration::from_secs(1), || {
        attempts += 1;
        Err(io::Error::from_raw_os_error(2))
    })
    .unwrap_err();
    assert_eq!(error.raw_os_error(), Some(2));
    assert_eq!(attempts, 1);
    let started = Instant::now();
    let error = replace_with_retry(Duration::from_millis(25), || {
        Err(io::Error::from_raw_os_error(32))
    })
    .unwrap_err();
    assert_eq!(error.raw_os_error(), Some(32));
    assert!(started.elapsed() >= Duration::from_millis(25));
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[cfg(windows)]
fn restrictive_reader(path: &Path) -> fs::File {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};
    OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .open(path)
        .unwrap()
}

#[cfg(windows)]
#[test]
fn replacement_waits_for_a_brief_non_delete_shared_reader() {
    let fixture = Fixture::new();
    let path = fixture.0.join("instance.json");
    write_at(&path, 1234, "old", "test").unwrap();
    let reader = restrictive_reader(&path);
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(80));
        drop(reader);
    });
    write_at(&path, 5678, "new", "test").unwrap();
    release.join().unwrap();
    assert_eq!(read_at(&path).unwrap()["token"], "new");
}

#[cfg(windows)]
#[test]
fn persistent_sharing_conflict_preserves_the_registration_and_removes_temporary_file() {
    let fixture = Fixture::new();
    let path = fixture.0.join("instance.json");
    write_at(&path, 1234, "old", "test").unwrap();
    let reader = restrictive_reader(&path);
    let started = Instant::now();
    let error = write_at(&path, 5678, "new", "test").unwrap_err();
    assert!(matches!(error.raw_os_error(), Some(5 | 32)), "{error}");
    assert!(started.elapsed() >= Duration::from_millis(950));
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(read_at(&path).unwrap()["token"], "old");
    assert_eq!(fs::read_dir(&fixture.0).unwrap().count(), 1);
    drop(reader);
    write_at(&path, 5678, "new", "test").unwrap();
    assert_eq!(read_at(&path).unwrap()["token"], "new");
}
