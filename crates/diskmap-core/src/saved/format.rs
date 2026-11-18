//! 存盘格式 v1。固定头的小端布局为标识 (8)、格式版本 (4)、元数据长度 (4)、
//! 索引存储长度 (8)、索引解码长度 (8)、索引 CRC32 (4)、文件头 CRC32 (4)。
//! 后面是 JSON 元数据及索引。文件头 CRC32 覆盖固定头的前 36 字节及元数据，
//! 索引 CRC32 覆盖实际存盘字节。列出时只读文件头，并用文件大小检查是否截断。

use std::ffi::OsString;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Seek, Write};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crc32fast::Hasher;
use lz4_flex::frame::{BlockSize, FrameDecoder, FrameEncoder, FrameInfo};
use serde::{Deserialize, Serialize};

use crate::VERSION;
use crate::i18n::Text;
use crate::index::Index;
use crate::platform::Os;
use crate::scan::{DENIED_SAMPLES, MAX_THREADS, MountEntry, Progress, ScanMethod, ScanState};

const MAGIC: &[u8; 8] = b"DMAPSAVE";
const FORMAT_VERSION: u32 = 1;
const PREFIX_LEN: usize = 40;
const CRC_OFFSET: usize = 36;
const MAX_METADATA: usize = 16 * 1024 * 1024;
const BUFFER_BYTES: usize = 256 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum Codec {
    Raw,
    Lz4,
}

pub(super) struct Header {
    pub version: String,
    pub os: Os,
    pub progress: Progress,
    pub reasons: Vec<Text>,
    codec: Codec,
    stored_len: u64,
    encoded_len: u64,
    body_crc: u32,
}

#[derive(Serialize, Deserialize)]
struct Metadata {
    version: String,
    host_os: String,
    index_os: String,
    codec: Codec,
    // serde 的 OsString 使用 Unix 原字节或 Windows UTF-16，避免路径有损转换。
    root: OsString,
    files: u64,
    dirs: u64,
    bytes: u64,
    denied: u64,
    denied_samples: Vec<OsString>,
    elapsed: Span,
    finished_at: Timestamp,
    apparent: bool,
    cross_fs: bool,
    fs_used: u64,
    mounts: Vec<Mount>,
    skipped: Vec<Mount>,
    threads: usize,
    method: String,
    fallback_reason: Option<Bilingual>,
    records: u64,
    anomalies: u64,
    reasons: Vec<Bilingual>,
}

impl Metadata {
    fn new(index: &Index, p: &Progress, reasons: &[Text], codec: Codec) -> io::Result<Self> {
        let finished_at = p.finished_at.ok_or_else(invalid_file)?;
        Ok(Self {
            version: VERSION.to_owned(),
            host_os: Os::current().key().to_owned(),
            index_os: index.os().key().to_owned(),
            codec,
            root: p.root.as_os_str().to_owned(),
            files: p.files,
            dirs: p.dirs,
            bytes: p.bytes,
            denied: p.denied,
            denied_samples: p
                .denied_samples
                .iter()
                .map(|path| path.as_os_str().to_owned())
                .collect(),
            elapsed: p.elapsed.into(),
            finished_at: finished_at.into(),
            apparent: p.apparent,
            cross_fs: p.cross_fs,
            fs_used: p.fs_used,
            mounts: p.mounts.iter().map(Mount::from).collect(),
            skipped: p.skipped.iter().map(Mount::from).collect(),
            threads: p.threads,
            method: p.method.key().to_owned(),
            fallback_reason: p.fallback_reason.as_ref().map(Bilingual::from),
            records: p.records,
            anomalies: p.anomalies,
            reasons: reasons.iter().map(Bilingual::from).collect(),
        })
    }

    fn into_progress(self) -> io::Result<Progress> {
        let method = [ScanMethod::Auto, ScanMethod::Generic, ScanMethod::Mft]
            .into_iter()
            .find(|method| method.key() == self.method)
            .ok_or_else(invalid_file)?;
        if !(1..=MAX_THREADS).contains(&self.threads)
            || self.denied_samples.len() > DENIED_SAMPLES
            || self.denied_samples.len() as u64 > self.denied
        {
            return Err(invalid_file());
        }
        Ok(Progress {
            state: ScanState::Done,
            error: None,
            root: self.root.into(),
            files: self.files,
            dirs: self.dirs,
            bytes: self.bytes,
            denied: self.denied,
            denied_samples: self.denied_samples.into_iter().map(Into::into).collect(),
            current: None,
            elapsed: self.elapsed.duration()?,
            finished_at: Some(self.finished_at.time()?),
            apparent: self.apparent,
            cross_fs: self.cross_fs,
            fs_used: self.fs_used,
            mounts: self.mounts.into_iter().map(Into::into).collect(),
            skipped: self.skipped.into_iter().map(Into::into).collect(),
            threads: self.threads,
            method,
            fallback_reason: self.fallback_reason.map(Text::from),
            records: self.records,
            anomalies: self.anomalies,
        })
    }
}

#[derive(Serialize, Deserialize)]
struct Mount {
    path: OsString,
    fstype: String,
    source: Option<String>,
}

impl From<&MountEntry> for Mount {
    fn from(entry: &MountEntry) -> Self {
        Self {
            path: entry.path.as_os_str().to_owned(),
            fstype: entry.fstype.clone(),
            source: entry.source.clone(),
        }
    }
}

impl From<Mount> for MountEntry {
    fn from(mount: Mount) -> Self {
        Self {
            path: mount.path.into(),
            fstype: mount.fstype,
            source: mount.source,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Bilingual {
    zh: String,
    en: String,
}

impl From<&Text> for Bilingual {
    fn from(text: &Text) -> Self {
        Self {
            zh: text.zh.to_string(),
            en: text.en.to_string(),
        }
    }
}

impl From<Bilingual> for Text {
    fn from(text: Bilingual) -> Self {
        Text::owned(text.zh, text.en)
    }
}

#[derive(Serialize, Deserialize)]
struct Span {
    seconds: u64,
    nanos: u32,
}

impl From<Duration> for Span {
    fn from(duration: Duration) -> Self {
        Self {
            seconds: duration.as_secs(),
            nanos: duration.subsec_nanos(),
        }
    }
}

impl Span {
    fn duration(self) -> io::Result<Duration> {
        if self.nanos >= 1_000_000_000 {
            return Err(invalid_file());
        }
        Ok(Duration::new(self.seconds, self.nanos))
    }
}

#[derive(Serialize, Deserialize)]
struct Timestamp {
    before_epoch: bool,
    offset: Span,
}

impl From<SystemTime> for Timestamp {
    fn from(time: SystemTime) -> Self {
        let (before_epoch, offset) = match time.duration_since(UNIX_EPOCH) {
            Ok(duration) => (false, duration),
            Err(error) => (true, error.duration()),
        };
        Self {
            before_epoch,
            offset: offset.into(),
        }
    }
}

impl Timestamp {
    fn time(self) -> io::Result<SystemTime> {
        let offset = self.offset.duration()?;
        if self.before_epoch {
            UNIX_EPOCH.checked_sub(offset)
        } else {
            UNIX_EPOCH.checked_add(offset)
        }
        .ok_or_else(invalid_file)
    }
}

pub(super) fn write(
    file: &mut File,
    index: &Index,
    progress: &Progress,
    reasons: &[Text],
    codec: Codec,
) -> io::Result<()> {
    let metadata = serde_json::to_vec(&Metadata::new(index, progress, reasons, codec)?)?;
    if metadata.len() > MAX_METADATA {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "保存的扫描信息过长 / Saved scan metadata is too large",
        ));
    }
    file.write_all(&[0; PREFIX_LEN])?;
    file.write_all(&metadata)?;
    let encoded_len = index.encoded_len();
    let (stored_len, body_crc) = {
        let mut payload = Payload::new(BufWriter::with_capacity(BUFFER_BYTES, &mut *file));
        match codec {
            Codec::Raw => index.write_encoded(&mut payload)?,
            Codec::Lz4 => {
                let info = FrameInfo::new()
                    .block_size(BlockSize::Max256KB)
                    .content_size(Some(encoded_len))
                    .block_checksums(true);
                let mut encoder = FrameEncoder::with_frame_info(info, &mut payload);
                index.write_encoded(&mut encoder)?;
                encoder.finish()?;
            }
        }
        payload.flush()?;
        (payload.len, payload.crc.finalize())
    };
    let mut prefix = [0; PREFIX_LEN];
    prefix[..8].copy_from_slice(MAGIC);
    prefix[8..12].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
    prefix[12..16].copy_from_slice(&(metadata.len() as u32).to_le_bytes());
    prefix[16..24].copy_from_slice(&stored_len.to_le_bytes());
    prefix[24..32].copy_from_slice(&encoded_len.to_le_bytes());
    prefix[32..CRC_OFFSET].copy_from_slice(&body_crc.to_le_bytes());
    let checksum = header_crc(&prefix, &metadata);
    prefix[CRC_OFFSET..].copy_from_slice(&checksum.to_le_bytes());
    file.rewind()?;
    file.write_all(&prefix)
}

pub(super) fn read_header(file: &mut File) -> io::Result<Header> {
    let file_len = file.metadata()?.len();
    let mut prefix = [0; PREFIX_LEN];
    file.read_exact(&mut prefix)?;
    if &prefix[..8] != MAGIC || u32_at(&prefix, 8) != FORMAT_VERSION {
        return Err(invalid_file());
    }
    let metadata_len = u32_at(&prefix, 12) as usize;
    let stored_len = u64_at(&prefix, 16);
    let encoded_len = u64_at(&prefix, 24);
    let body_crc = u32_at(&prefix, 32);
    if metadata_len > MAX_METADATA
        || !(Index::MIN_ENCODED_LEN..=Index::MAX_ENCODED_LEN).contains(&encoded_len)
        || (PREFIX_LEN as u64 + metadata_len as u64).checked_add(stored_len) != Some(file_len)
    {
        return Err(invalid_file());
    }
    let mut metadata = vec![0; metadata_len];
    file.read_exact(&mut metadata)?;
    if header_crc(&prefix, &metadata) != u32_at(&prefix, CRC_OFFSET) {
        return Err(invalid_file());
    }
    let mut metadata: Metadata = serde_json::from_slice(&metadata).map_err(|_| invalid_file())?;
    if metadata.host_os != Os::current().key()
        || metadata.version.is_empty()
        || metadata.reasons.is_empty()
    {
        return Err(invalid_file());
    }
    let os = [Os::Linux, Os::Macos, Os::Windows]
        .into_iter()
        .find(|os| os.key() == metadata.index_os)
        .ok_or_else(invalid_file)?;
    let codec = metadata.codec;
    let valid_len = match codec {
        Codec::Raw => encoded_len == stored_len,
        // LZ4 的每个匹配长度扩展字节最多表示 255 个解码字节。
        Codec::Lz4 => encoded_len <= stored_len.saturating_mul(255),
    };
    if !valid_len {
        return Err(invalid_file());
    }
    let reasons = std::mem::take(&mut metadata.reasons)
        .into_iter()
        .map(Text::from)
        .collect();
    Ok(Header {
        version: metadata.version.clone(),
        os,
        progress: metadata.into_progress()?,
        reasons,
        codec,
        stored_len,
        encoded_len,
        body_crc,
    })
}

pub(super) fn read_index(file: &mut File, header: &Header) -> io::Result<Index> {
    let mut payload = Payload::new(BufReader::with_capacity(
        BUFFER_BYTES,
        file.take(header.stored_len),
    ));
    let index = match header.codec {
        Codec::Raw => decode_index(&mut payload, header)?,
        Codec::Lz4 => {
            let mut decoder = FrameDecoder::new(&mut payload);
            let index = decode_index(&mut decoder, header).map_err(lz4_read_error)?;
            // 读完帧尾，同时拒绝额外的解码数据。帧尾缺失时底层会读到 EOF。
            if decoder.read(&mut [0]).map_err(lz4_read_error)? != 0 {
                return Err(invalid_file());
            }
            index
        }
    };
    if payload.len != header.stored_len || payload.eof || payload.crc.finalize() != header.body_crc
    {
        return Err(invalid_file());
    }
    Ok(index)
}

fn decode_index(input: impl Read, header: &Header) -> io::Result<Index> {
    Index::read_encoded(
        input,
        header.encoded_len,
        header.os,
        header.progress.root.clone(),
    )
}

fn lz4_read_error(error: io::Error) -> io::Error {
    if error
        .get_ref()
        .is_some_and(|cause| cause.is::<lz4_flex::frame::Error>())
    {
        return invalid_file();
    }
    error
}

fn header_crc(prefix: &[u8; PREFIX_LEN], metadata: &[u8]) -> u32 {
    let mut crc = Hasher::new();
    crc.update(&prefix[..CRC_OFFSET]);
    crc.update(metadata);
    crc.finalize()
}

struct Payload<T> {
    inner: T,
    crc: Hasher,
    len: u64,
    eof: bool,
}

impl<T> Payload<T> {
    fn new(inner: T) -> Self {
        Self {
            inner,
            crc: Hasher::new(),
            len: 0,
            eof: false,
        }
    }

    fn update(&mut self, bytes: &[u8]) {
        self.crc.update(bytes);
        self.len += bytes.len() as u64;
    }
}

impl<R: Read> Read for Payload<R> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let len = self.inner.read(bytes)?;
        self.eof |= len == 0 && !bytes.is_empty();
        self.update(&bytes[..len]);
        Ok(len)
    }
}

impl<W: Write> Write for Payload<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let len = self.inner.write(bytes)?;
        self.update(&bytes[..len]);
        Ok(len)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

fn u32_at(prefix: &[u8; PREFIX_LEN], at: usize) -> u32 {
    u32::from_le_bytes(prefix[at..at + 4].try_into().expect("固定字段宽度"))
}

fn u64_at(prefix: &[u8; PREFIX_LEN], at: usize) -> u64 {
    u64::from_le_bytes(prefix[at..at + 8].try_into().expect("固定字段宽度"))
}

fn invalid_file() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "保存的扫描文件格式或校验和无效 / Invalid saved scan format or checksum",
    )
}
