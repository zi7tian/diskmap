//! 数据源只暴露读操作；解析代码不拥有任何写入接口。

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Default)]
pub(super) enum ReadKind {
    Sequential,
    Extension,
    AttributeList,
    Reparse,
    #[default]
    Other,
}

/// 实际提交给数据源的请求，包括失败请求；字节数是请求量，不是物理设备吞吐计数。
#[derive(Clone, Copy, Debug, Default)]
pub struct ReadCount {
    pub calls: u64,
    pub bytes: u64,
    /// 数据源执行时间；流水线中可以与主线程解析重叠。
    pub elapsed: Duration,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ReadStats {
    pub sequential: ReadCount,
    pub extension: ReadCount,
    pub attribute_list: ReadCount,
    pub reparse: ReadCount,
    pub other: ReadCount,
}

impl ReadStats {
    fn get_mut(&mut self, kind: ReadKind) -> &mut ReadCount {
        match kind {
            ReadKind::Sequential => &mut self.sequential,
            ReadKind::Extension => &mut self.extension,
            ReadKind::AttributeList => &mut self.attribute_list,
            ReadKind::Reparse => &mut self.reparse,
            ReadKind::Other => &mut self.other,
        }
    }
}

pub(super) trait ReadAt: Send {
    fn read_exact_at(&mut self, offset: u64, buffer: &mut [u8]) -> io::Result<()>;

    fn read_kind(&mut self, _kind: ReadKind, offset: u64, buffer: &mut [u8]) -> io::Result<()> {
        self.read_exact_at(offset, buffer)
    }
}

/// 只在每次实际读取时计时，不在逐记录热路径调用时钟。
pub(super) struct Measured<'a> {
    pub source: &'a mut dyn ReadAt,
    pub elapsed: Duration,
    pub bytes: u64,
    pub calls: u64,
    pub stats: ReadStats,
}

impl ReadAt for Measured<'_> {
    fn read_exact_at(&mut self, offset: u64, buffer: &mut [u8]) -> io::Result<()> {
        self.read_kind(ReadKind::Other, offset, buffer)
    }

    fn read_kind(&mut self, kind: ReadKind, offset: u64, buffer: &mut [u8]) -> io::Result<()> {
        let started = Instant::now();
        let result = self.source.read_kind(kind, offset, buffer);
        let elapsed = started.elapsed();
        self.elapsed += elapsed;
        self.calls += 1;
        self.bytes += buffer.len() as u64;
        let stats = self.stats.get_mut(kind);
        stats.calls += 1;
        stats.bytes += buffer.len() as u64;
        stats.elapsed += elapsed;
        result
    }
}

pub(super) struct Image {
    file: File,
    length: u64,
    read_delay: Duration,
}

impl Image {
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(super::invalid(
                "需要普通镜像文件 / Expected a regular image file",
            ));
        }
        Ok(Self {
            file,
            length: metadata.len(),
            read_delay: Duration::ZERO,
        })
    }

    pub fn with_delay(mut self, delay: Duration) -> Self {
        self.read_delay = delay;
        self
    }
}

impl ReadAt for Image {
    fn read_exact_at(&mut self, offset: u64, buffer: &mut [u8]) -> io::Result<()> {
        // 仅用于显式镜像测量。sleep 的实际时间包含调度误差，由 Measured 如实计入。
        if !self.read_delay.is_zero() {
            std::thread::sleep(self.read_delay);
        }
        if offset
            .checked_add(buffer.len() as u64)
            .is_none_or(|end| end > self.length)
        {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "镜像被截断 / Truncated image",
            ));
        }
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.read_exact(buffer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    struct Fail;
    impl ReadAt for Fail {
        fn read_exact_at(&mut self, _offset: u64, _buffer: &mut [u8]) -> io::Result<()> {
            Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "test source error",
            ))
        }
    }

    #[test]
    fn measured_categories_include_failed_requests_exactly_once() {
        let mut source = Fail;
        let mut measured = Measured {
            source: &mut source,
            elapsed: Duration::ZERO,
            bytes: 0,
            calls: 0,
            stats: ReadStats::default(),
        };
        for (i, kind) in [
            ReadKind::Sequential,
            ReadKind::Extension,
            ReadKind::AttributeList,
            ReadKind::Reparse,
            ReadKind::Other,
        ]
        .into_iter()
        .enumerate()
        {
            let mut out = vec![0; i + 1];
            assert_eq!(
                measured.read_kind(kind, 0, &mut out).unwrap_err().kind(),
                io::ErrorKind::UnexpectedEof
            );
            let count = measured.stats.get_mut(kind);
            assert_eq!(count.calls, 1);
            assert_eq!(count.bytes, (i + 1) as u64);
        }
        assert_eq!(measured.calls, 5);
        assert_eq!(measured.bytes, 15);
        assert_eq!(
            measured.elapsed,
            measured.stats.sequential.elapsed
                + measured.stats.extension.elapsed
                + measured.stats.attribute_list.elapsed
                + measured.stats.reparse.elapsed
                + measured.stats.other.elapsed
        );
        assert!(measured.read_exact_at(0, &mut [0; 2]).is_err());
        assert_eq!(measured.stats.other.calls, 2);
        assert_eq!(measured.stats.other.bytes, 7);
    }

    #[test]
    fn image_delay_is_per_request_and_reads_leave_fixture_unchanged() {
        let dir = std::env::temp_dir().join(format!(
            "diskmap-read-delay-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("test.img");
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .unwrap();
        file.write_all(&[1, 2, 3, 4]).unwrap();
        drop(file);
        let delay = Duration::from_millis(2);
        let mut image = Image::open(&path).unwrap().with_delay(delay);
        let started = Instant::now();
        let mut out = [0; 2];
        image.read_exact_at(1, &mut out).unwrap();
        assert_eq!(out, [2, 3]);
        assert_eq!(
            image.read_exact_at(4, &mut out).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
        assert!(started.elapsed() >= delay * 2);
        drop(image);
        assert_eq!(std::fs::read(&path).unwrap(), [1, 2, 3, 4]);
        // 只删除本测试用 create_dir 新建的私有目录与自己的文件。
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }
}
