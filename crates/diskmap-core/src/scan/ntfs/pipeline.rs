//! 有界双缓冲：读取线程只持有数据源，调用线程按顺序解析已完成的窗口。

use std::io;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::mpsc::sync_channel;
use std::time::{Duration, Instant};

use super::source::{ReadAt, ReadKind};
use super::{check_cancel, invalid};

/// 一个窗口可以跨越 MFT 的多个物理运行段；内容按 ranges 顺序紧密拼接。
pub(super) struct ReadRange {
    pub offset: u64,
    pub len: usize,
}

pub(super) struct Window {
    pub offset: u64,
    pub len: usize,
    pub ranges: Vec<ReadRange>,
}

#[derive(Default)]
pub(super) struct PipelineTiming {
    /// 主线程等待窗口或读线程退出的时间；与本线程的解析互斥。
    pub wait: Duration,
    pub elapsed: Duration,
    /// 读线程内 read_kind 的累计耗时；可以与消费回调重叠。
    pub read_elapsed: Duration,
}

/// 两个缓冲的所有权在两个线程和有界通道间移动，不复制窗口、不累积预读结果。
/// 回调错误或取消时先断开通道，再等待读线程；最多等待正在进行的一次底层读取。
pub(super) fn run<I>(
    source: &mut dyn ReadAt,
    windows: I,
    block_size: usize,
    cancel: &AtomicBool,
    mut consume: impl FnMut(&Window, &[u8]) -> io::Result<()>,
) -> io::Result<PipelineTiming>
where
    I: Iterator<Item = io::Result<Window>> + Send,
{
    if block_size == 0 || block_size > 4 * 1024 * 1024 {
        return Err(invalid("invalid NTFS pipeline buffer size"));
    }
    check_cancel(cancel)?;
    let started = Instant::now();
    let stopped = AtomicBool::new(false);
    std::thread::scope(|scope| {
        struct StopOnDrop<'a>(&'a AtomicBool);
        impl Drop for StopOnDrop<'_> {
            fn drop(&mut self) {
                self.0.store(true, Relaxed);
            }
        }
        // 回调出错或展开时也停止后续分片读取，不必等到整个窗口读完才发现通道断开。
        let stop_on_drop = StopOnDrop(&stopped);
        let stopped = &stopped;
        // 通道端点在 scope 回调中创建；即使消费回调 panic，先销毁端点也能唤醒读线程。
        let (free_tx, free_rx) = sync_channel::<Vec<u8>>(2);
        let (ready_tx, ready_rx) = sync_channel::<(Window, Vec<u8>)>(1);
        for _ in 0..2 {
            let mut buffer = Vec::new();
            buffer
                .try_reserve_exact(block_size)
                .map_err(|error| io::Error::new(io::ErrorKind::OutOfMemory, error))?;
            buffer.resize(block_size, 0);
            // 此时接收端仍由本线程持有，且容量为 2，不会失败或阻塞。
            free_tx
                .send(buffer)
                .map_err(|_| invalid("NTFS pipeline initialization failed"))?;
        }
        let worker = std::thread::Builder::new()
            .name("diskmap-mft-read".into())
            .spawn_scoped(scope, move || -> io::Result<Duration> {
                let mut read_elapsed = Duration::ZERO;
                for window in windows {
                    if stopped.load(Relaxed) {
                        return Ok(read_elapsed);
                    }
                    check_cancel(cancel)?;
                    let window = window?;
                    validate(&window, block_size)?;
                    let Ok(mut buffer) = free_rx.recv() else {
                        return Ok(read_elapsed);
                    };
                    let mut at = 0;
                    for range in &window.ranges {
                        if stopped.load(Relaxed) {
                            return Ok(read_elapsed);
                        }
                        check_cancel(cancel)?;
                        let reading = Instant::now();
                        let result = source.read_kind(
                            ReadKind::Sequential,
                            range.offset,
                            &mut buffer[at..at + range.len],
                        );
                        read_elapsed += reading.elapsed();
                        result?;
                        at += range.len;
                    }
                    if ready_tx.send((window, buffer)).is_err() {
                        return Ok(read_elapsed);
                    }
                }
                Ok(read_elapsed)
            })?;
        let mut timing = PipelineTiming::default();
        let consumed = (|| -> io::Result<()> {
            loop {
                check_cancel(cancel)?;
                let waiting = Instant::now();
                let ready = ready_rx.recv();
                timing.wait += waiting.elapsed();
                let Ok((window, buffer)) = ready else {
                    break;
                };
                check_cancel(cancel)?;
                consume(&window, &buffer[..window.len])?;
                // 读线程可能刚好已读完最后一个窗口，此时不再需要回收。
                let _ = free_tx.send(buffer);
            }
            Ok(())
        })();
        drop(stop_on_drop);
        drop(ready_rx);
        drop(free_tx);
        let waiting = Instant::now();
        let read = worker
            .join()
            .map_err(|_| io::Error::other("NTFS read worker panicked"))?;
        timing.wait += waiting.elapsed();
        // 不能让断开通道引起的正常退出覆盖消费方的实际错误。
        consumed?;
        timing.read_elapsed = read?;
        check_cancel(cancel)?;
        timing.elapsed = started.elapsed();
        Ok(timing)
    })
}

fn validate(window: &Window, block_size: usize) -> io::Result<()> {
    if window.len == 0
        || window.len > block_size
        || window.offset.checked_add(window.len as u64).is_none()
    {
        return Err(invalid("invalid NTFS pipeline window"));
    }
    let mut total = 0usize;
    for range in &window.ranges {
        if range.len == 0 || range.offset.checked_add(range.len as u64).is_none() {
            return Err(invalid("invalid NTFS pipeline range"));
        }
        total = total
            .checked_add(range.len)
            .filter(|n| *n <= window.len)
            .ok_or_else(|| invalid("NTFS pipeline range exceeds window"))?;
    }
    if total != window.len {
        return Err(invalid("incomplete NTFS pipeline window"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Source<'a> {
        reads: &'a AtomicUsize,
        fail: Option<usize>,
    }

    impl ReadAt for Source<'_> {
        fn read_exact_at(&mut self, offset: u64, out: &mut [u8]) -> io::Result<()> {
            let n = self.reads.fetch_add(1, Ordering::Relaxed);
            if self.fail == Some(n) {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "test read failure",
                ));
            }
            for (i, byte) in out.iter_mut().enumerate() {
                *byte = (offset + i as u64) as u8;
            }
            Ok(())
        }
    }

    fn windows(n: usize) -> impl Iterator<Item = io::Result<Window>> + Send {
        (0..n).map(|i| {
            Ok(Window {
                offset: i as u64 * 8,
                len: 8,
                ranges: vec![
                    ReadRange {
                        offset: 100 + i as u64 * 8,
                        len: 3,
                    },
                    ReadRange {
                        offset: 200 + i as u64 * 8,
                        len: 5,
                    },
                ],
            })
        })
    }

    #[test]
    fn fragments_are_concatenated_in_order_using_two_buffers() {
        let reads = AtomicUsize::new(0);
        let mut source = Source {
            reads: &reads,
            fail: None,
        };
        let mut buffers = std::collections::BTreeSet::new();
        let mut seen = 0;
        let timing = run(
            &mut source,
            windows(200),
            8,
            &AtomicBool::new(false),
            |window, data| {
                assert_eq!(window.offset, seen * 8);
                let expected: Vec<_> = (100 + seen * 8..103 + seen * 8)
                    .chain(200 + seen * 8..205 + seen * 8)
                    .map(|n| n as u8)
                    .collect();
                assert_eq!(data, expected);
                buffers.insert(data.as_ptr() as usize);
                seen += 1;
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(seen, 200);
        assert_eq!(buffers.len(), 2);
        assert_eq!(reads.load(Ordering::Relaxed), 400);
        assert!(timing.wait <= timing.elapsed);
    }

    #[test]
    fn read_consumer_and_cancel_failures_join_worker() {
        for failure in 0..3 {
            let reads = AtomicUsize::new(0);
            let cancel = AtomicBool::new(false);
            let mut source = Source {
                reads: &reads,
                fail: (failure == 0).then_some(2),
            };
            let result = run(&mut source, windows(100), 8, &cancel, |_, _| {
                if failure == 1 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "test consumer failure",
                    ));
                }
                if failure == 2 {
                    cancel.store(true, Ordering::Relaxed);
                }
                Ok(())
            });
            assert_eq!(
                result.err().unwrap().kind(),
                match failure {
                    0 => io::ErrorKind::UnexpectedEof,
                    1 => io::ErrorKind::InvalidInput,
                    _ => io::ErrorKind::Interrupted,
                }
            );
            assert!(reads.load(Ordering::Relaxed) <= 4);
        }
    }

    #[test]
    fn malformed_schedule_does_not_read_or_panic() {
        for (len, ranges) in [
            (9, vec![ReadRange { offset: 0, len: 9 }]),
            (
                8,
                vec![ReadRange {
                    offset: u64::MAX,
                    len: 8,
                }],
            ),
            (8, vec![ReadRange { offset: 0, len: 7 }]),
            (
                8,
                vec![ReadRange {
                    offset: 0,
                    len: usize::MAX,
                }],
            ),
            (8, vec![ReadRange { offset: 0, len: 0 }]),
        ] {
            let reads = AtomicUsize::new(0);
            let mut source = Source {
                reads: &reads,
                fail: None,
            };
            let window = Ok(Window {
                offset: 0,
                len,
                ranges,
            });
            assert!(
                run(
                    &mut source,
                    std::iter::once(window),
                    8,
                    &AtomicBool::new(false),
                    |_, _| Ok(())
                )
                .is_err()
            );
            assert_eq!(reads.load(Ordering::Relaxed), 0);
        }
    }

    #[test]
    fn next_read_runs_while_current_window_is_consumed() {
        use std::sync::mpsc::{Receiver, Sender, channel};

        struct Handshake {
            consuming: Receiver<()>,
            reading: Sender<()>,
        }
        impl ReadAt for Handshake {
            fn read_exact_at(&mut self, offset: u64, out: &mut [u8]) -> io::Result<()> {
                if offset != 0 {
                    self.consuming
                        .recv_timeout(Duration::from_secs(2))
                        .map_err(io::Error::other)?;
                    self.reading.send(()).map_err(io::Error::other)?;
                }
                out.fill(offset as u8);
                Ok(())
            }
        }
        let (consume_tx, consume_rx) = channel();
        let (read_tx, read_rx) = channel();
        let mut source = Handshake {
            consuming: consume_rx,
            reading: read_tx,
        };
        let windows = (0..2).map(|i| {
            Ok(Window {
                offset: i * 8,
                len: 8,
                ranges: vec![ReadRange {
                    offset: i * 8,
                    len: 8,
                }],
            })
        });
        run(
            &mut source,
            windows,
            8,
            &AtomicBool::new(false),
            |window, data| {
                if window.offset == 0 {
                    consume_tx.send(()).unwrap();
                    // 若读取与消费串行，这个相互握手会超时，而不是靠不稳定的速度阈值判断。
                    read_rx.recv_timeout(Duration::from_secs(2)).unwrap();
                }
                assert!(data.iter().all(|b| u64::from(*b) == window.offset));
                Ok(())
            },
        )
        .unwrap();
    }

    #[test]
    fn consumer_unwind_disconnects_channels_before_scope_joins() {
        let reads = AtomicUsize::new(0);
        let mut source = Source {
            reads: &reads,
            fail: None,
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = run(
                &mut source,
                windows(100),
                8,
                &AtomicBool::new(false),
                |_, _| panic!("test consumer unwind"),
            );
        }));
        assert!(result.is_err());
        assert!(reads.load(Ordering::Relaxed) <= 4);
    }
}
