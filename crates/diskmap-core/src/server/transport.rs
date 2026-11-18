//! 有资源上限的同步 HTTP 传输。每连接只处理一个请求，响应后限时丢弃未读数据再关闭。

use std::io::{self, Cursor, IoSlice, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

const MAX_HEADERS: usize = 64 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(5);
const CLOSE_TIMEOUT: Duration = Duration::from_secs(1);
const MAX_DISCARD: usize = 1024 * 1024;

#[derive(Clone, PartialEq, Eq)]
pub(super) enum Method {
    Get,
    Post,
    Other,
}

pub(super) struct Header {
    pub name: String,
    pub value: String,
}

impl Header {
    pub fn from_bytes(name: &str, value: &str) -> io::Result<Self> {
        if name.contains(['\r', '\n']) || value.contains(['\r', '\n']) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid header",
            ));
        }
        Ok(Self {
            name: name.into(),
            value: value.into(),
        })
    }
}

pub(super) struct StatusCode(pub u16);

pub(super) struct Response {
    code: u16,
    headers: Vec<Header>,
    body: ResponseBody,
}

// 普通响应继续拥有原 Vec；缓存响应仅持有共享字节，发送时不克隆或重新序列化正文。
enum ResponseBody {
    Owned(Vec<u8>),
    Shared(Arc<[u8]>),
}

impl ResponseBody {
    fn bytes(&self) -> &[u8] {
        match self {
            Self::Owned(bytes) => bytes,
            Self::Shared(bytes) => bytes,
        }
    }
}

impl Response {
    pub fn from_data(body: Vec<u8>) -> Self {
        Self {
            code: 200,
            headers: Vec::new(),
            body: ResponseBody::Owned(body),
        }
    }

    /// 直接持有缓存字节；调用方通常仅需 Arc::clone，不复制正文缓冲区。
    pub fn from_shared(body: Arc<[u8]>) -> Self {
        Self {
            code: 200,
            headers: Vec::new(),
            body: ResponseBody::Shared(body),
        }
    }

    pub fn with_status_code(mut self, code: StatusCode) -> Self {
        self.code = code.0;
        self
    }

    pub fn with_header(mut self, header: Header) -> Self {
        self.headers.push(header);
        self
    }

    fn write(&self, stream: &mut TcpStream, deadline: Instant) -> io::Result<()> {
        let reason = match self.code {
            200 => "OK",
            204 => "No Content",
            400 => "Bad Request",
            403 => "Forbidden",
            404 => "Not Found",
            408 => "Request Timeout",
            431 => "Request Header Fields Too Large",
            500 => "Internal Server Error",
            501 => "Not Implemented",
            503 => "Service Unavailable",
            _ => "Error",
        };
        let body = self.body.bytes();
        let mut headers = format!(
            "HTTP/1.1 {} {reason}\r\nServer: DiskMap\r\nContent-Length: {}\r\nConnection: close\r\n",
            self.code,
            body.len()
        );
        for header in &self.headers {
            use std::fmt::Write;
            write!(headers, "{}: {}\r\n", header.name, header.value).expect("string write");
        }
        headers.push_str("\r\n");
        let mut parts = [IoSlice::new(headers.as_bytes()), IoSlice::new(body)];
        let mut pending = &mut parts[..];
        while !pending.is_empty() {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .filter(|remaining| !remaining.is_zero());
            if let Some(remaining) = remaining {
                stream.set_write_timeout(Some(remaining))?;
            } else {
                // 当前 I/O 期限已用完：只允许一次非阻塞发送，让读取超时的小型错误仍能送达。
                stream.set_nonblocking(true)?;
            }
            match stream.write_vectored(pending) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(count) => IoSlice::advance_slices(&mut pending, count),
                Err(error) if error.kind() == io::ErrorKind::Interrupted && remaining.is_some() => {
                    continue;
                }
                Err(error) => return Err(error),
            }
            if remaining.is_none() && !pending.is_empty() {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "response timeout"));
            }
        }
        Ok(())
    }
}

pub(super) struct Request {
    method: Method,
    url: String,
    headers: Vec<Header>,
    stream: TcpStream,
    buffered: Cursor<Vec<u8>>,
    body_deadline: Instant,
}

impl Request {
    pub fn read(mut stream: TcpStream) -> io::Result<Self> {
        stream.set_nonblocking(false)?;
        stream.set_read_timeout(Some(IO_TIMEOUT))?;
        stream.set_write_timeout(Some(IO_TIMEOUT))?;
        let header_deadline = Instant::now() + IO_TIMEOUT;
        let mut data = Vec::with_capacity(4096);
        let header_end = loop {
            if let Some(end) = data.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                break end + 4;
            }
            if data.len() >= MAX_HEADERS {
                return reject(stream, header_deadline, 431, "headers too large");
            }
            let Some(remaining) = header_deadline
                .checked_duration_since(Instant::now())
                .filter(|remaining| !remaining.is_zero())
            else {
                return reject(stream, header_deadline, 408, "request timeout");
            };
            stream.set_read_timeout(Some(remaining))?;
            let mut chunk = [0_u8; 4096];
            let available = chunk.len().min(MAX_HEADERS - data.len());
            let count = match stream.read(&mut chunk[..available]) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "incomplete headers",
                    ));
                }
                Ok(count) => count,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    return reject(stream, header_deadline, 408, "request timeout");
                }
                Err(error) => return Err(error),
            };
            data.extend_from_slice(&chunk[..count]);
        };
        let mut header_space = [httparse::EMPTY_HEADER; 100];
        let mut parsed = httparse::Request::new(&mut header_space);
        if !matches!(
            parsed.parse(&data[..header_end]),
            Ok(httparse::Status::Complete(_))
        ) {
            return reject(stream, header_deadline, 400, "bad request");
        }
        // 关闭连接消除了请求走私的复用条件，仍拒绝有歧义的正文长度声明。
        let lengths: Vec<_> = parsed
            .headers
            .iter()
            .filter(|header| header.name.eq_ignore_ascii_case("Content-Length"))
            .collect();
        let transfer = parsed
            .headers
            .iter()
            .any(|header| header.name.eq_ignore_ascii_case("Transfer-Encoding"));
        if lengths.len() > 1 || (transfer && !lengths.is_empty()) {
            return reject(stream, header_deadline, 400, "ambiguous body length");
        }
        let method = match parsed.method {
            Some("GET") => Method::Get,
            Some("POST") => Method::Post,
            _ => Method::Other,
        };
        let url = parsed.path.unwrap_or("").to_owned();
        // httparse 只负责验证语法；它会裁剪尾空白，认证值必须保留 Python 的原样语义。
        let headers = data[..header_end]
            .split(|byte| *byte == b'\n')
            .skip(1)
            .filter_map(|line| {
                let line = line.strip_suffix(b"\r").unwrap_or(line);
                let colon = line.iter().position(|byte| *byte == b':')?;
                let mut value = &line[colon + 1..];
                while value
                    .first()
                    .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
                {
                    value = &value[1..];
                }
                Some(Header {
                    name: String::from_utf8_lossy(&line[..colon]).into_owned(),
                    // Python http.server 用 Latin-1 读取头值；保留尾空白，避免认证别名。
                    value: value.iter().map(|&byte| char::from(byte)).collect(),
                })
            })
            .collect();
        Ok(Self {
            method,
            url,
            headers,
            stream,
            buffered: Cursor::new(data[header_end..].to_vec()),
            body_deadline: header_deadline,
        })
    }

    pub fn method(&self) -> &Method {
        &self.method
    }
    pub fn url(&self) -> &str {
        &self.url
    }
    pub fn headers(&self) -> &[Header] {
        &self.headers
    }
    pub fn as_reader(&mut self) -> &mut dyn Read {
        self
    }

    pub fn respond(self, response: Response) -> io::Result<()> {
        // 应用计算不消耗发送预算；只在开始响应时领取一次，部分写入不能重置期限。
        send_response(self.stream, response, Instant::now() + IO_TIMEOUT)
    }
}

impl Read for Request {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.buffered.position() < self.buffered.get_ref().len() as u64 {
            self.buffered.read(buffer)
        } else {
            let remaining = self
                .body_deadline
                .checked_duration_since(Instant::now())
                .filter(|remaining| !remaining.is_zero())
                .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "request timeout"))?;
            self.stream.set_read_timeout(Some(remaining))?;
            self.stream.read(buffer)
        }
    }
}

fn send_response(
    mut stream: TcpStream,
    response: Response,
    io_deadline: Instant,
) -> io::Result<()> {
    let result = response.write(&mut stream, io_deadline);
    // 先发 FIN，让客户端立即读到完整响应；保留读方向，避免未读正文使关闭变成 RST。
    let _ = stream.shutdown(Shutdown::Write);
    let deadline = io_deadline.min(Instant::now() + CLOSE_TIMEOUT);
    let mut discarded = 0;
    let mut buffer = [0_u8; 8192];
    while discarded < MAX_DISCARD {
        let Some(remaining) = deadline
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
        else {
            break;
        };
        if stream.set_read_timeout(Some(remaining)).is_err() {
            break;
        }
        // 不解析声明长度，也不增长缓冲区；慢速或无限发送不能延长本轮 I/O 期限。
        let available = buffer.len().min(MAX_DISCARD - discarded);
        match stream.read(&mut buffer[..available]) {
            Ok(0) => break,
            Ok(count) => discarded += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    result
}

fn reject<T>(
    stream: TcpStream,
    request_deadline: Instant,
    code: u16,
    message: &str,
) -> io::Result<T> {
    let body =
        serde_json::to_vec(&serde_json::json!({"error": message})).expect("serializable error");
    let _ = send_response(
        stream,
        super::bytes_response(code, body, super::JSON_TYPE),
        request_deadline,
    );
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        message.to_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tcp_pair() -> (TcpStream, TcpStream) {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        client
            .set_write_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let (server, _) = listener.accept().unwrap();
        (client, server)
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn unread_body_reset_makes_the_response_fail_without_delayed_close() {
        // Linux 直接关闭未排空的 TCP 复现 Windows 上的 RST 症状；Linux 的旧 shutdown(Both)
        // 往往先暴露 EOF，不能原样稳定重现。RST 会令 read_to_end 失败，客户端丢弃已到达的回复。
        for delayed_close in [false, true] {
            let (mut client, mut server) = tcp_pair();
            client.write_all(&[b'x'; 64 * 1024]).unwrap();
            client.shutdown(Shutdown::Write).unwrap();
            assert_eq!(server.peek(&mut [0_u8; 1]).unwrap(), 1);
            let worker = std::thread::spawn(move || {
                let response = Response::from_data(br#"{"error":"bad token"}"#.to_vec())
                    .with_status_code(StatusCode(403));
                if delayed_close {
                    send_response(server, response, Instant::now() + IO_TIMEOUT).unwrap();
                } else {
                    server.shutdown(Shutdown::Read).unwrap();
                    response
                        .write(&mut server, Instant::now() + IO_TIMEOUT)
                        .unwrap();
                    drop(server);
                }
            });
            worker.join().unwrap();
            let mut reply = Vec::new();
            let read = client.read_to_end(&mut reply);
            if delayed_close {
                read.expect("完整响应后应是 FIN，不能是 ConnectionReset");
                assert!(reply.starts_with(b"HTTP/1.1 403 Forbidden\r\n"));
                assert!(reply.ends_with(br#"{"error":"bad token"}"#));
            } else {
                assert_eq!(read.unwrap_err().kind(), io::ErrorKind::ConnectionReset);
            }
        }
    }

    #[test]
    fn expired_deadline_still_sends_a_small_error_without_blocking() {
        let (mut client, server) = tcp_pair();
        send_response(
            server,
            Response::from_data(br#"{"error":"request timeout"}"#.to_vec())
                .with_status_code(StatusCode(408)),
            Instant::now() - Duration::from_secs(1),
        )
        .unwrap();
        let mut reply = Vec::new();
        client.read_to_end(&mut reply).unwrap();
        assert!(reply.starts_with(b"HTTP/1.1 408 Request Timeout\r\n"));
        assert!(reply.ends_with(br#"{"error":"request timeout"}"#));
    }

    #[test]
    fn normal_response_sends_eight_mib_after_the_request_deadline_has_expired() {
        const BODY_LENGTH: usize = 8 * 1024 * 1024;
        let (mut client, server) = tcp_pair();
        client
            .write_all(b"GET /api/tree HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
            .unwrap();
        client.shutdown(Shutdown::Write).unwrap();
        let (finished, completion) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut request = Request::read(server).unwrap();
            // 模拟应用计算用完了读取期限；无需让测试真的等待五秒。
            request.body_deadline = Instant::now() - Duration::from_secs(1);
            let response = Response::from_data(vec![b'x'; BODY_LENGTH]);
            finished.send(request.respond(response)).unwrap();
        });
        // 短暂延后读取，避免 Linux 在旧版单次非阻塞写入时碰巧吸收整个响应。
        let early_result = completion.recv_timeout(Duration::from_millis(50)).ok();
        let mut reply = Vec::new();
        client.read_to_end(&mut reply).unwrap();
        let result = early_result
            .unwrap_or_else(|| completion.recv_timeout(IO_TIMEOUT + CLOSE_TIMEOUT).unwrap());
        worker.join().unwrap();
        assert!(
            result.is_ok(),
            "正常响应应有独立写入期限：respond={result:?}，收到 {} 字节",
            reply.len()
        );
        let split = reply
            .windows(4)
            .position(|part| part == b"\r\n\r\n")
            .unwrap();
        let headers = std::str::from_utf8(&reply[..split]).unwrap();
        assert!(headers.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(headers.contains(&format!("Content-Length: {BODY_LENGTH}\r\n")));
        let body = &reply[split + 4..];
        assert_eq!(body.len(), BODY_LENGTH);
        assert!(body.iter().all(|byte| *byte == b'x'));
    }

    #[test]
    fn slow_response_reader_stays_within_write_and_close_budgets() {
        const WRITE_BUDGET: Duration = Duration::from_millis(150);
        const SCHEDULING_MARGIN: Duration = Duration::from_millis(500);
        let (_client, server) = tcp_pair();
        let (finished, completion) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let response = Response::from_data(vec![b'x'; 16 * 1024 * 1024]);
            let start = Instant::now();
            let result = send_response(server, response, start + WRITE_BUDGET);
            finished
                .send((result.map_err(|error| error.kind()), start.elapsed()))
                .unwrap();
        });
        // Windows 可能接纳全部 16 MiB；允许写入成功，也允许超时，不能假定发送缓冲区大小。
        // 对方始终不读且不关闭，写入和延迟关闭的总耗时仍必须有上限。
        let maximum = WRITE_BUDGET + CLOSE_TIMEOUT + SCHEDULING_MARGIN;
        let (result, elapsed) = completion.recv_timeout(maximum).unwrap();
        worker.join().unwrap();
        if let Err(error) = result {
            assert!(matches!(
                error,
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            ));
        }
        assert!(elapsed <= maximum, "写入和关闭耗时 {elapsed:?}");
    }

    #[test]
    fn delayed_close_keeps_the_original_deadline_and_does_not_delay_the_reply() {
        let (mut client, server) = tcp_pair();
        let deadline = Instant::now() + Duration::from_millis(150);
        let (finished, completion) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            send_response(server, Response::from_data(b"ok".to_vec()), deadline).unwrap();
            finished.send(()).unwrap();
        });
        let mut reply = Vec::new();
        client.read_to_end(&mut reply).unwrap();
        assert!(reply.ends_with(b"ok"));
        // 客户端没有关闭写方向：回复已经结束，服务端仍在等待尾部，但最多用原期限的余额。
        assert!(completion.try_recv().is_err());
        completion
            .recv_timeout(Duration::from_millis(750))
            .expect("延迟关闭不能重新领取完整一秒");
        worker.join().unwrap();
    }

    #[test]
    fn delayed_close_stops_after_one_second_without_peer_eof() {
        let (mut client, server) = tcp_pair();
        let (finished, completion) = std::sync::mpsc::channel();
        let start = Instant::now();
        let worker = std::thread::spawn(move || {
            send_response(
                server,
                Response::from_data(b"ok".to_vec()),
                Instant::now() + IO_TIMEOUT,
            )
            .unwrap();
            finished.send(()).unwrap();
        });
        let mut reply = Vec::new();
        client.read_to_end(&mut reply).unwrap();
        assert!(reply.ends_with(b"ok"));
        completion.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(start.elapsed() >= Duration::from_millis(900));
        worker.join().unwrap();
    }

    #[test]
    fn delayed_close_stops_after_one_mib_without_peer_eof() {
        let (mut client, server) = tcp_pair();
        let (finished, completion) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            send_response(
                server,
                Response::from_data(b"ok".to_vec()),
                Instant::now() + IO_TIMEOUT,
            )
            .unwrap();
            finished.send(()).unwrap();
        });
        let mut reply = Vec::new();
        client.read_to_end(&mut reply).unwrap();
        // 收到 FIN 后仍可发送尾部；固定小块正好填满丢弃额度，不发送客户端 FIN。
        let chunk = [b'x'; 8192];
        for _ in 0..MAX_DISCARD / chunk.len() {
            client.write_all(&chunk).unwrap();
        }
        completion
            .recv_timeout(Duration::from_millis(750))
            .expect("读够 1 MiB 后应立即释放连接，不再等超时");
        worker.join().unwrap();
    }

    #[test]
    fn shared_and_owned_responses_keep_the_original_body_allocation() {
        // 使用验收场景规模的正文，直接证明缓存命中不会再次分配/复制约 1.2 MB。
        let shared: Arc<[u8]> = vec![b'x'; 1_200_000].into();
        let response = Response::from_shared(Arc::clone(&shared));
        assert_eq!(response.body.bytes().as_ptr(), shared.as_ptr());
        assert_eq!(response.body.bytes().len(), shared.len());
        assert_eq!(Arc::strong_count(&shared), 2);
        drop(response);
        assert_eq!(Arc::strong_count(&shared), 1);

        let owned = vec![b'y'; 1_200_000];
        let original = owned.as_ptr();
        let response = Response::from_data(owned);
        assert_eq!(response.body.bytes().as_ptr(), original);
        assert_eq!(response.body.bytes().len(), 1_200_000);
    }
}
