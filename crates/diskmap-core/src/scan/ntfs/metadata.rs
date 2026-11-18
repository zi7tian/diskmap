//! 有界元数据批读：先合并实际需要的区间，再将相近区间合为最多 1 MiB 的读取。
//! 缓存只保留请求的字节，不保留间隙，更不缓存整份 MFT。

use std::io;
use std::sync::atomic::AtomicBool;

use super::pipeline::ReadRange;
use super::source::{ReadAt, ReadKind};
use super::{check_cancel, invalid};

const BLOCK: usize = 1024 * 1024;
const MAX_GAP: u64 = 64 * 1024;

#[derive(Default)]
pub(super) struct Cache {
    spans: Vec<Span>,
    bytes: Vec<u8>,
}

struct Span {
    start: u64,
    end: u64,
    at: usize,
}

impl Cache {
    pub fn read(
        source: &mut dyn ReadAt,
        mut ranges: Vec<ReadRange>,
        kind: ReadKind,
        cancel: &AtomicBool,
    ) -> io::Result<Self> {
        check_cancel(cancel)?;
        ranges.sort_unstable_by_key(|r| r.offset);
        let mut spans: Vec<Span> = Vec::new();
        for range in ranges {
            if range.len == 0 {
                continue;
            }
            let end = range
                .offset
                .checked_add(range.len as u64)
                .ok_or_else(|| invalid("NTFS metadata range overflows"))?;
            if let Some(last) = spans.last_mut()
                && range.offset <= last.end
            {
                last.end = last.end.max(end);
            } else {
                spans.push(Span {
                    start: range.offset,
                    end,
                    at: 0,
                });
            }
        }
        let mut length = 0usize;
        for span in &mut spans {
            span.at = length;
            length = usize::try_from(span.end - span.start)
                .ok()
                .and_then(|n| length.checked_add(n))
                .ok_or_else(|| invalid("NTFS metadata cache overflows"))?;
        }
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(length)
            .map_err(|_| io::Error::from(io::ErrorKind::OutOfMemory))?;
        bytes.resize(length, 0);
        let mut buffer = if length == 0 {
            Vec::new()
        } else {
            vec![0; BLOCK]
        };
        let mut i = 0;
        let mut offset = spans.first().map_or(0, |s| s.start);
        while i < spans.len() {
            check_cancel(cancel)?;
            let limit = offset.saturating_add(BLOCK as u64);
            let mut end = spans[i].end.min(limit);
            let mut last = i;
            while end == spans[last].end
                && last + 1 < spans.len()
                && spans[last + 1].start.saturating_sub(end) <= MAX_GAP
                && spans[last + 1].start < limit
            {
                last += 1;
                end = spans[last].end.min(limit);
            }
            let len = (end - offset) as usize;
            // 合并后的间隙可能大于所有有效负载的总和，缓冲仍不超过 BLOCK。
            buffer.resize(len, 0);
            source.read_kind(kind, offset, &mut buffer)?;
            let read_offset = offset;
            while i <= last {
                let span = &spans[i];
                let from = span.start.max(read_offset);
                let to = span.end.min(end);
                let src = (from - read_offset) as usize;
                let dst = span.at + (from - span.start) as usize;
                let n = (to - from) as usize;
                bytes[dst..dst + n].copy_from_slice(&buffer[src..src + n]);
                if to < span.end {
                    offset = to;
                    break;
                }
                i += 1;
                offset = spans.get(i).map_or(0, |s| s.start);
            }
        }
        Ok(Self { spans, bytes })
    }

    pub fn copy(&self, offset: u64, out: &mut [u8]) -> io::Result<()> {
        if out.is_empty() {
            return Ok(());
        }
        let i = self.spans.partition_point(|s| s.start <= offset);
        let span = i
            .checked_sub(1)
            .and_then(|i| self.spans.get(i))
            .ok_or_else(missing)?;
        let end = offset
            .checked_add(out.len() as u64)
            .filter(|end| *end <= span.end)
            .ok_or_else(missing)?;
        let from = span.at + (offset - span.start) as usize;
        let to = span.at + (end - span.start) as usize;
        out.copy_from_slice(&self.bytes[from..to]);
        Ok(())
    }

    pub fn heap_bytes(&self) -> usize {
        self.spans.capacity() * std::mem::size_of::<Span>() + self.bytes.capacity()
    }
}

/// 仅缓存查询会返回此内部信号；调用方可先完成记录前置校验，再决定是否安排外置读取。
fn missing() -> io::Error {
    io::Error::new(
        io::ErrorKind::WouldBlock,
        "NTFS metadata has not been collected",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[derive(Default)]
    struct Trace<'a> {
        reads: Vec<(ReadKind, u64, usize)>,
        end: Option<u64>,
        cancel_after_read: Option<&'a AtomicBool>,
    }

    fn byte(offset: u64) -> u8 {
        (offset.wrapping_mul(37) ^ (offset >> 8)) as u8
    }

    impl Trace<'_> {
        fn perform(&mut self, kind: ReadKind, offset: u64, out: &mut [u8]) -> io::Result<()> {
            self.reads.push((kind, offset, out.len()));
            if self
                .end
                .is_some_and(|end| offset.checked_add(out.len() as u64).is_none_or(|n| n > end))
            {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "test source EOF",
                ));
            }
            for (i, value) in out.iter_mut().enumerate() {
                *value = byte(offset + i as u64);
            }
            if let Some(cancel) = self.cancel_after_read {
                cancel.store(true, Ordering::Relaxed);
            }
            Ok(())
        }
    }

    impl ReadAt for Trace<'_> {
        fn read_exact_at(&mut self, offset: u64, out: &mut [u8]) -> io::Result<()> {
            self.perform(ReadKind::Other, offset, out)
        }

        fn read_kind(&mut self, kind: ReadKind, offset: u64, out: &mut [u8]) -> io::Result<()> {
            self.perform(kind, offset, out)
        }
    }

    fn check(cache: &Cache, start: u64, len: usize) {
        let mut out = vec![0; len];
        cache.copy(start, &mut out).unwrap();
        assert!(
            out.into_iter()
                .enumerate()
                .all(|(i, value)| value == byte(start + i as u64))
        );
    }

    #[test]
    fn overlapping_adjacent_and_duplicate_ranges_are_stored_once() {
        let mut source = Trace::default();
        let cache = Cache::read(
            &mut source,
            vec![
                ReadRange {
                    offset: 20,
                    len: 10,
                },
                ReadRange { offset: 0, len: 12 },
                ReadRange { offset: 5, len: 20 },
                ReadRange {
                    offset: 30,
                    len: 10,
                },
                ReadRange { offset: 0, len: 12 },
                ReadRange {
                    offset: u64::MAX,
                    len: 0,
                },
            ],
            ReadKind::AttributeList,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(source.reads.len(), 1);
        assert_eq!((source.reads[0].1, source.reads[0].2), (0, 40));
        assert!(matches!(source.reads[0].0, ReadKind::AttributeList));
        assert_eq!(cache.spans.len(), 1);
        assert_eq!(cache.bytes.len(), 40);
        check(&cache, 0, 40);
        check(&cache, 8, 29);
        assert!(cache.copy(39, &mut [0; 2]).is_err());
        assert!(cache.copy(u64::MAX, &mut [0; 1]).is_err());
    }

    #[test]
    fn nearby_gaps_are_read_but_not_retained_or_served_as_requested_data() {
        let mut source = Trace::default();
        let cache = Cache::read(
            &mut source,
            vec![
                ReadRange {
                    offset: 200_000,
                    len: 8,
                },
                ReadRange {
                    offset: 100,
                    len: 20,
                },
                ReadRange {
                    offset: 200,
                    len: 20,
                },
            ],
            ReadKind::Reparse,
            &AtomicBool::new(false),
        )
        .unwrap();
        let reads: Vec<_> = source
            .reads
            .iter()
            .map(|(_, offset, len)| (*offset, *len))
            .collect();
        assert_eq!(reads, [(100, 120), (200_000, 8)]);
        assert!(
            source
                .reads
                .iter()
                .all(|(kind, _, _)| matches!(kind, ReadKind::Reparse))
        );
        assert_eq!(cache.bytes.len(), 48);
        assert_eq!(cache.bytes.capacity(), 48);
        assert_eq!(cache.spans.len(), 3);
        check(&cache, 100, 20);
        check(&cache, 200, 20);
        check(&cache, 200_000, 8);
        assert!(cache.copy(150, &mut [0; 1]).is_err());
        assert!(cache.copy(119, &mut [0; 82]).is_err());
        assert_eq!(source.reads.len(), 2);
    }

    #[test]
    fn partially_read_spans_cross_block_boundaries_without_losing_bytes() {
        let first = 500;
        let second = first + BLOCK as u64 + 100 + MAX_GAP;
        let mut source = Trace::default();
        let cache = Cache::read(
            &mut source,
            vec![
                ReadRange {
                    offset: second,
                    len: BLOCK + 200,
                },
                ReadRange {
                    offset: first,
                    len: BLOCK + 100,
                },
            ],
            ReadKind::AttributeList,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(source.reads.len(), 3);
        assert!(source.reads.iter().all(|(_, _, len)| *len <= BLOCK));
        assert_eq!(
            source.reads.iter().map(|(_, _, len)| *len).sum::<usize>(),
            2 * BLOCK + 300 + MAX_GAP as usize
        );
        assert_eq!(cache.bytes.len(), 2 * BLOCK + 300);
        check(&cache, first, BLOCK + 100);
        check(&cache, second, BLOCK + 200);
        check(&cache, first + BLOCK as u64 - 10, 110);
    }

    #[test]
    fn truncated_source_overflow_and_cancellation_propagate_without_partial_cache() {
        let mut short = Trace {
            end: Some(7),
            ..Default::default()
        };
        let result = Cache::read(
            &mut short,
            vec![ReadRange { offset: 0, len: 8 }],
            ReadKind::Reparse,
            &AtomicBool::new(false),
        );
        assert_eq!(result.err().unwrap().kind(), io::ErrorKind::UnexpectedEof);
        let mut source = Trace::default();
        let result = Cache::read(
            &mut source,
            vec![ReadRange {
                offset: u64::MAX,
                len: 1,
            }],
            ReadKind::Reparse,
            &AtomicBool::new(false),
        );
        assert_eq!(result.err().unwrap().kind(), io::ErrorKind::InvalidData);
        assert!(source.reads.is_empty());
        let cancel = AtomicBool::new(true);
        let result = Cache::read(
            &mut source,
            vec![ReadRange { offset: 0, len: 1 }],
            ReadKind::Reparse,
            &cancel,
        );
        assert_eq!(result.err().unwrap().kind(), io::ErrorKind::Interrupted);
        assert!(source.reads.is_empty());
        let cancel = AtomicBool::new(false);
        let mut source = Trace {
            cancel_after_read: Some(&cancel),
            ..Default::default()
        };
        let result = Cache::read(
            &mut source,
            vec![ReadRange {
                offset: 0,
                len: BLOCK * 2 + 1,
            }],
            ReadKind::Reparse,
            &cancel,
        );
        assert_eq!(result.err().unwrap().kind(), io::ErrorKind::Interrupted);
        assert_eq!(source.reads.len(), 1);
    }

    #[test]
    fn shuffled_ranges_match_an_independent_byte_coverage_map() {
        let mut state = 0x3141_5926u64;
        let mut coverage = vec![false; 5 * BLOCK];
        let mut ranges = Vec::new();
        let mut requests = Vec::new();
        for _ in 0..2_000 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let len = (state % 300 + 1) as usize;
            let offset = ((state >> 12) as usize % (coverage.len() - len)) as u64;
            coverage[offset as usize..offset as usize + len].fill(true);
            ranges.push(ReadRange { offset, len });
            requests.push((offset, len));
        }
        let mut source = Trace::default();
        let cache = Cache::read(
            &mut source,
            ranges,
            ReadKind::Reparse,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(source.reads.iter().all(|(_, _, len)| *len <= BLOCK));
        assert_eq!(
            cache.bytes.len(),
            coverage.iter().filter(|used| **used).count()
        );
        for (offset, len) in requests {
            check(&cache, offset, len);
        }
        for offset in (0..coverage.len()).step_by(101) {
            let result = cache.copy(offset as u64, &mut [0; 1]);
            assert_eq!(result.is_ok(), coverage[offset]);
        }
    }
}
