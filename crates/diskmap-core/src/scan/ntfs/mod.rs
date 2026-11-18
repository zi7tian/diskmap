//! 只读 NTFS MFT 扫描。解析器与按偏移读取的数据源分开，镜像在三个系统上走同一条路径。
//! 活动卷不是一致性快照；普通异常记录局部跳过并计数，仅 MFT 无法读取时自动回退。

#[cfg(test)]
mod benchmark;
mod compact;
mod metadata;
mod parse;
mod pipeline;
mod reader;
#[cfg(test)]
mod round3_tests;
mod source;
mod tree;
#[cfg(windows)]
mod windows;

use std::collections::BTreeSet;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::time::Duration;

use crate::classify::Classifier;
use crate::i18n::Text;
use crate::index::Index;

use super::{Inner, ScanMethod, lock};
pub use source::{ReadCount, ReadStats};

/// 互不重叠的墙钟阶段；读盘只计未被解析覆盖的读取及等待，解析不含等待读盘。
#[derive(Clone, Copy, Debug, Default)]
pub struct Timings {
    pub io: Duration,
    pub parse: Duration,
    pub tree: Duration,
    pub classify: Duration,
    pub hardlinks: Duration,
    pub index: Duration,
}

/// 成功后一次交付完整索引；失败、取消时不交付半棵树。
pub struct ScanResult {
    pub index: Index,
    /// 位图标记为已分配且实际处理的记录数，包含扩展记录。
    pub records: u64,
    /// 被跳过或保守处理的异常记录数（同一记录多处异常只计一次）。
    pub anomalies: u64,
    pub parse_elapsed: Duration,
    pub build_elapsed: Duration,
    pub timings: Timings,
    pub read_bytes: u64,
    pub read_calls: u64,
    pub read_stats: ReadStats,
    /// 读线程执行与主线程解析重叠的时间；实际读取总耗时见 read_stats。
    pub io_overlap: Duration,
    /// 顺序扫描时暂存的基础/扩展记录收集表容量，不包含最终索引。
    pub collection_bytes: usize,
    pub extension_records: u64,
    pub deferred_records: u64,
    /// 建树前紧凑临时表、位图及读取缓冲区的堆容量估计；不是进程峰值 RSS。
    pub temporary_bytes: usize,
}

/// 读取以 NTFS 引导扇区开头的原始分区镜像（不解析 MBR/GPT/VHD 容器）。
/// 文件只读打开。`root` 是索引里显示的卷根（例如 `C:\`）；分类器应注入 Windows 规则。
/// `progress` 每批返回已处理的记录数；回调也可以设置 `cancel`。取消返回 `Interrupted`。
pub fn scan_image(
    image: &Path,
    root: &Path,
    apparent: bool,
    classifier: &Classifier,
    cancel: &AtomicBool,
    progress: impl FnMut(u64),
) -> io::Result<ScanResult> {
    scan_image_with_options(
        image,
        root,
        classifier,
        cancel,
        ImageOptions {
            apparent,
            read_delay: Duration::ZERO,
        },
        progress,
    )
}

/// 镜像诊断选项。延迟只作用于源读取请求，不作用于缓存命中，也不改变真实卷扫描。
#[derive(Clone, Copy, Debug, Default)]
pub struct ImageOptions {
    pub apparent: bool,
    pub read_delay: Duration,
}

pub fn scan_image_with_options(
    image: &Path,
    root: &Path,
    classifier: &Classifier,
    cancel: &AtomicBool,
    options: ImageOptions,
    mut progress: impl FnMut(u64),
) -> io::Result<ScanResult> {
    let mut source = source::Image::open(image)?.with_delay(options.read_delay);
    reader::scan(
        &mut source,
        root,
        options.apparent,
        classifier,
        cancel,
        |records, _| progress(records),
    )
}

/// 同步扫描整个 Windows NTFS 卷；与自动扫描使用相同实现，额外交付阶段计时。
/// 不提权、不回退；用于诊断和验收，子目录或其他系统返回双语错误。
pub fn scan_volume(
    root: &Path,
    apparent: bool,
    classifier: &Classifier,
    cancel: &AtomicBool,
    progress: impl FnMut(u64, u64),
) -> Result<ScanResult, Text> {
    #[cfg(windows)]
    {
        windows::scan_volume(root, apparent, classifier, cancel, progress)
    }
    #[cfg(not(windows))]
    {
        let _ = (root, apparent, classifier, cancel, progress);
        Err(Text::new(
            "MFT 卷扫描只支持 Windows；镜像可在所有系统上读取",
            "MFT volume scanning requires Windows; images can be read on all systems",
        ))
    }
}

/// 只有异常记录才占额外空间；按记录号去重，解析和建树共享同一计数。
#[derive(Default)]
pub(super) struct Anomalies {
    records: BTreeSet<u64>,
}

impl Anomalies {
    pub(super) fn note(&mut self, id: u64) {
        self.records.insert(id & 0x0000_ffff_ffff_ffff);
    }
    pub(super) fn count(&self) -> u64 {
        self.records.len() as u64
    }
}

pub(super) fn check_cancel(cancel: &AtomicBool) -> io::Result<()> {
    if cancel.load(Relaxed) {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "扫描已取消 / Scan cancelled",
        ))
    } else {
        Ok(())
    }
}

pub(super) fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

/// `true` 表示已经由 MFT 完成或取消；`false` 表示应继续通用扫描。
pub(super) fn selected(inner: &Inner) -> Result<bool, Text> {
    if inner.method == ScanMethod::Generic {
        return Ok(false);
    }
    let result = scan_volume(
        &inner.root,
        inner.apparent,
        &inner.classifier,
        &inner.cancelled,
        |records, anomalies| {
            lock(&inner.status).method = ScanMethod::Mft;
            inner.records.store(records, Relaxed);
            inner.anomalies.store(anomalies, Relaxed);
        },
    );
    match result {
        Ok(result) => {
            // 索引一直在本地构建；只有完整成功后才发布，回退不会混入先前读到的项目。
            let files = result.index.root().nfiles();
            let dirs = result.index.root().ndirs();
            let bytes = result.index.root().size();
            *super::write(&inner.index) = result.index;
            inner.files.store(files, Relaxed);
            inner.dirs.store(dirs, Relaxed);
            inner.bytes.store(bytes, Relaxed);
            inner.records.store(result.records, Relaxed);
            inner.anomalies.store(result.anomalies, Relaxed);
            let mut status = lock(&inner.status);
            status.method = ScanMethod::Mft;
            status.fs_used = crate::platform::mounts::fs_usage(&inner.root).map_or(0, |u| u.used);
            Ok(true)
        }
        Err(_) if inner.cancelled.load(Relaxed) => Ok(true),
        Err(reason) if inner.method == ScanMethod::Auto => {
            let mut status = lock(&inner.status);
            status.method = ScanMethod::Generic;
            status.fallback_reason = Some(reason);
            Ok(false)
        }
        Err(reason) => Err(reason),
    }
}
