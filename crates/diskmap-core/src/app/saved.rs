//! 应用扫描的后台保存，以及把存盘结果安装为新的扫描会话。

use std::io;
#[cfg(feature = "server")]
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::classify::Classifier;
use crate::i18n::Text;
use crate::saved::{SavedScan, Store};
use crate::scan::{Scan, ScanOptions};

use super::{App, Session, lock};

pub(super) enum Location {
    Disabled,
    Default(Store),
    Explicit(Store),
}

impl App {
    /// 为嵌入调用和测试指定扫描缓存。应在启动扫描和共享 App 前配置。
    pub fn with_saved_store(self, store: Store) -> Self {
        *lock(&self.inner.saved) = Location::Explicit(store);
        self
    }

    /// 实例登记路径被调用方覆盖时，同样隔离默认扫描缓存。
    #[cfg(feature = "server")]
    pub(crate) fn use_instance_cache_dir(&self, cache_dir: &Path) {
        let mut location = lock(&self.inner.saved);
        if matches!(*location, Location::Default(_)) {
            *location = Location::Explicit(Store::with_cache_dir(cache_dir));
        }
    }

    pub(super) fn saved_store(&self) -> Option<Store> {
        match &*lock(&self.inner.saved) {
            Location::Disabled => None,
            Location::Default(store) | Location::Explicit(store) => Some(store.clone()),
        }
    }

    pub(super) fn start_managed_scan(
        &self,
        options: ScanOptions,
        classifier: Arc<Classifier>,
    ) -> Scan {
        if let Some(store) = self.saved_store() {
            Scan::start_with_done(options, classifier, move |scan| {
                if let Err(error) = store.save(&scan) {
                    eprintln!(
                        "{}",
                        Text::owned(
                            format!("无法保存扫描结果：{error}"),
                            format!("Could not save the scan result: {error}"),
                        )
                    );
                }
            })
        } else {
            Scan::start(options, classifier)
        }
    }

    /// 只读存盘结果的文件头。未配置缓存时返回空列表。
    pub fn saved_scans(&self) -> io::Result<Vec<SavedScan>> {
        self.saved_store()
            .map_or_else(|| Ok(Vec::new()), |store| store.list())
    }

    /// 在读入并校验后替换当前扫描。文件已被移除或无效时保留原扫描。
    pub fn open_saved_scan(&self, id: &str) -> io::Result<Option<Value>> {
        let Some(store) = self.saved_store() else {
            return Ok(None);
        };
        self.wait_ready();
        let Some(scan) = store.load(id, self.components().classifier.clone())? else {
            return Ok(None);
        };
        {
            let mut slot = lock(&self.inner.scan);
            if self.inner.closed.load(Ordering::Acquire) {
                return Err(io::ErrorKind::NotConnected.into());
            }
            if let Some(previous) = slot.as_ref() {
                previous.scan.cancel();
            }
            *slot = Some(Arc::new(Session::new(scan, self.procs().snapshot())));
        }
        Ok(Some(self.status()))
    }

    /// 移除缓存文件。已经打开的索引可继续查看。
    pub fn delete_saved_scan(&self, id: &str) -> io::Result<bool> {
        self.saved_store()
            .map_or(Ok(false), |store| store.delete(id))
    }
}

pub(crate) fn status_marker(scan: &Scan) -> Option<Value> {
    scan.saved_finished_at()
        .map(|time| json!({"finished_at": unix_seconds(time)}))
}

pub(crate) fn unix_seconds(time: SystemTime) -> f64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_secs_f64(),
        Err(error) => -error.duration().as_secs_f64(),
    }
}
