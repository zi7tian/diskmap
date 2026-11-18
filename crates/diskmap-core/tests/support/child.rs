//! 测试子进程的有界等待与异常回收；只操作本测试创建的进程。

use std::io::{self, Read, Write};
use std::ops::{Deref, DerefMut};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub struct ChildGuard {
    child: Child,
    completed: bool,
    #[cfg(unix)]
    own_group: bool,
}

impl ChildGuard {
    pub fn spawn(command: &mut Command) -> io::Result<Self> {
        // 独立进程组使 Unix 上的超时回收也覆盖继承管道的后代。
        #[cfg(unix)]
        let own_group = {
            use std::os::unix::process::CommandExt;
            let own = std::env::var_os("DISKMAP_TEST_MANAGED_SESSION").is_none();
            if own {
                command.process_group(0);
            }
            own
        };
        command.spawn().map(|child| Self {
            child,
            completed: false,
            #[cfg(unix)]
            own_group,
        })
    }

    // 同一 helper 被多个独立测试 crate 引入；没有输入的调用者只使用 command_output。
    #[allow(dead_code)]
    pub fn wait_with_input(mut self, input: Vec<u8>, timeout: Duration) -> io::Result<Output> {
        let deadline = Instant::now() + timeout;
        let mut stdin = self
            .child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("测试子进程未配置 stdin 管道"))?;
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let result = stdin.write_all(&input);
            drop(stdin);
            let _ = sender.send(result);
        });
        let output = self.wait_with_output(timeout)?;
        receiver
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|error| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("测试输入管道等待失败：{error}"),
                )
            })??;
        Ok(output)
    }

    pub fn wait_with_output(mut self, timeout: Duration) -> io::Result<Output> {
        self.child.stdin.take();
        let deadline = Instant::now() + timeout;
        let stdout = drain(self.child.stdout.take());
        let stderr = drain(self.child.stderr.take());
        let status = loop {
            if let Some(status) = self.child.try_wait()? {
                break status;
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "测试子进程等待超时",
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let output = Output {
            status,
            stdout: receive(stdout, deadline)?,
            stderr: receive(stderr, deadline)?,
        };
        self.completed = true;
        Ok(output)
    }
}

impl Deref for ChildGuard {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.child
    }
}

impl DerefMut for ChildGuard {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.child
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        if !self.completed
            && self.own_group
            && let Ok(pid) = i32::try_from(self.child.id())
        {
            unsafe extern "C" {
                fn kill(pid: i32, signal: i32) -> i32;
            }
            // SAFETY: spawn 为此 Child 创建了独立进程组；负 PID 只指向该组，9 为 SIGKILL。
            unsafe {
                kill(-pid, 9);
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn drain(pipe: Option<impl Read + Send + 'static>) -> mpsc::Receiver<io::Result<Vec<u8>>> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let result = (|| {
            let mut bytes = Vec::new();
            if let Some(mut pipe) = pipe {
                pipe.read_to_end(&mut bytes)?;
            }
            Ok(bytes)
        })();
        let _ = sender.send(result);
    });
    receiver
}

fn receive(
    receiver: mpsc::Receiver<io::Result<Vec<u8>>>,
    deadline: Instant,
) -> io::Result<Vec<u8>> {
    receiver
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .map_err(|error| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                format!("测试子进程管道等待失败：{error}"),
            )
        })?
}

pub fn command_output(command: &mut Command, timeout: Duration) -> io::Result<Output> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    ChildGuard::spawn(command)?.wait_with_output(timeout)
}
