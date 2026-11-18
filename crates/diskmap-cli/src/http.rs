//! 只连接 IPv4 回环地址的 HTTP 客户端；不使用代理、不跟随重定向。

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::time::{Duration, Instant};

use diskmap_core::i18n::{self, Text};
use serde_json::Value;

const MAX_HEADERS: usize = 64 * 1024;
const MAX_BODY: usize = 64 * 1024 * 1024;

#[derive(Clone)]
pub(crate) struct Endpoint {
    pub(crate) port: u16,
    pub(crate) token: String,
}

impl Endpoint {
    pub(crate) fn request(
        &self,
        method: &str,
        path: &str,
        params: &[(&str, String)],
        body: Option<&Value>,
        timeout: Duration,
    ) -> Result<Value, String> {
        let (status, bytes) = self.request_raw(method, path, params, body, timeout)?;
        let value: Result<Value, _> = serde_json::from_slice(&bytes);
        if !(200..300).contains(&status) {
            return Err(value
                .ok()
                .and_then(|v| v.get("error").and_then(Value::as_str).map(str::to_owned))
                .unwrap_or_else(|| {
                    Text::owned(
                        format!("DiskMap 接口出错（HTTP {status}）"),
                        format!("DiskMap API error (HTTP {status})"),
                    )
                    .to_string()
                }));
        }
        value.map_err(|error| error.to_string())
    }

    pub(crate) fn request_raw(
        &self,
        method: &str,
        path: &str,
        params: &[(&str, String)],
        body: Option<&Value>,
        timeout: Duration,
    ) -> Result<(u16, Vec<u8>), String> {
        self.exchange(method, path, params, body, timeout)
            .map_err(|error| error.to_string())
    }

    fn exchange(
        &self,
        method: &str,
        path: &str,
        params: &[(&str, String)],
        body: Option<&Value>,
        timeout: Duration,
    ) -> std::io::Result<(u16, Vec<u8>)> {
        // 实例文件可被同用户修改；禁止令牌换行注入，永远不把它当主机名或 URL。
        if self.token.bytes().any(|b| b < b' ' || b == 0x7f)
            || !matches!(method, "GET" | "POST")
            || !path.starts_with('/')
            || path.bytes().any(|b| b <= b' ' || b == 0x7f)
        {
            return Err(invalid("Invalid local HTTP request"));
        }
        let deadline = Instant::now() + timeout;
        let address = SocketAddr::from((Ipv4Addr::LOCALHOST, self.port));
        let mut stream = TcpStream::connect_timeout(&address, timeout)?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        let mut target = path.to_owned();
        for (index, (key, value)) in params.iter().enumerate() {
            target.push(if index == 0 && !path.contains('?') {
                '?'
            } else {
                '&'
            });
            target.push_str(&encode(key));
            target.push('=');
            target.push_str(&encode(value));
        }
        let data = body
            .map(serde_json::to_vec)
            .transpose()?
            .unwrap_or_default();
        write!(
            stream,
            "{method} {target} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nX-DiskMap-Token: {}\r\nX-DiskMap-Lang: {}\r\nConnection: close\r\n",
            self.port,
            self.token,
            i18n::current()
        )?;
        if body.is_some() {
            write!(
                stream,
                "Content-Type: application/json\r\nContent-Length: {}\r\n",
                data.len()
            )?;
        }
        stream.write_all(b"\r\n")?;
        stream.write_all(&data)?;
        let mut reader = BufReader::new(stream);
        let mut consumed = 0;
        let status_line = line(&mut reader, &mut consumed, deadline)?;
        let mut fields = status_line.split_whitespace();
        if !matches!(fields.next(), Some("HTTP/1.0" | "HTTP/1.1")) {
            return Err(invalid("Invalid HTTP response"));
        }
        let status = fields
            .next()
            .and_then(|s| s.parse::<u16>().ok())
            .ok_or_else(|| invalid("Invalid HTTP status"))?;
        let mut length = None;
        let mut chunked = false;
        loop {
            let header = line(&mut reader, &mut consumed, deadline)?;
            if header == "\r\n" || header == "\n" {
                break;
            }
            let (key, value) = header
                .split_once(':')
                .ok_or_else(|| invalid("Invalid HTTP header"))?;
            let value = value.trim();
            if key.eq_ignore_ascii_case("content-length") {
                let next = value
                    .parse::<usize>()
                    .map_err(|_| invalid("Invalid HTTP length"))?;
                if length.is_some_and(|old| old != next) || next > MAX_BODY {
                    return Err(invalid("Invalid HTTP length"));
                }
                length = Some(next);
            } else if key.eq_ignore_ascii_case("transfer-encoding") {
                if !value.eq_ignore_ascii_case("chunked") {
                    return Err(invalid("Unsupported HTTP transfer encoding"));
                }
                chunked = true;
            }
        }
        let mut output = Vec::new();
        if chunked {
            if length.is_some() {
                return Err(invalid("Ambiguous HTTP response length"));
            }
            loop {
                let chunk_line = line(&mut reader, &mut consumed, deadline)?;
                let size =
                    usize::from_str_radix(chunk_line.trim().split(';').next().unwrap_or(""), 16)
                        .map_err(|_| invalid("Invalid HTTP chunk"))?;
                if size == 0 {
                    break;
                }
                if size > MAX_BODY - output.len() {
                    return Err(invalid("HTTP response too large"));
                }
                read_exact(&mut reader, &mut output, size, deadline)?;
                let mut end = Vec::new();
                read_exact(&mut reader, &mut end, 2, deadline)?;
                if end != b"\r\n" {
                    return Err(invalid("Invalid HTTP chunk ending"));
                }
            }
        } else if let Some(length) = length {
            read_exact(&mut reader, &mut output, length, deadline)?;
        } else {
            let mut buffer = [0; 8192];
            loop {
                set_remaining(&reader, deadline)?;
                let read = reader.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                if read > MAX_BODY - output.len() {
                    return Err(invalid("HTTP response too large"));
                }
                output.extend_from_slice(&buffer[..read]);
            }
        }
        Ok((status, output))
    }
}

fn invalid(message: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message)
}

fn set_remaining(reader: &BufReader<TcpStream>, deadline: Instant) -> std::io::Result<()> {
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::TimedOut, "Local HTTP request timed out")
        })?;
    reader.get_ref().set_read_timeout(Some(remaining))
}

fn line(
    reader: &mut BufReader<TcpStream>,
    consumed: &mut usize,
    deadline: Instant,
) -> std::io::Result<String> {
    let mut bytes = Vec::new();
    loop {
        set_remaining(reader, deadline)?;
        let buffered = reader.fill_buf()?;
        if buffered.is_empty() {
            return Err(invalid("Incomplete HTTP headers"));
        }
        let newline = buffered.iter().position(|&byte| byte == b'\n');
        let count = newline.map_or(buffered.len(), |index| index + 1);
        if count > MAX_HEADERS - *consumed {
            return Err(invalid("Oversized HTTP headers"));
        }
        bytes.extend_from_slice(&buffered[..count]);
        *consumed += count;
        reader.consume(count);
        if newline.is_some() {
            break;
        }
    }
    String::from_utf8(bytes).map_err(|_| invalid("Invalid HTTP header encoding"))
}

fn read_exact(
    reader: &mut BufReader<TcpStream>,
    output: &mut Vec<u8>,
    length: usize,
    deadline: Instant,
) -> std::io::Result<()> {
    let end = output.len() + length;
    while output.len() < end {
        set_remaining(reader, deadline)?;
        let mut buffer = [0; 8192];
        let limit = buffer.len().min(end - output.len());
        let read = reader.read(&mut buffer[..limit])?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "Incomplete HTTP response",
            ));
        }
        output.extend_from_slice(&buffer[..read]);
    }
    Ok(())
}

fn encode(value: &str) -> String {
    const HEX: &[u8] = b"0123456789ABCDEF";
    let mut output = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                output.push(char::from(byte))
            }
            b' ' => output.push('+'),
            _ => {
                output.push('%');
                output.push(char::from(HEX[(byte >> 4) as usize]));
                output.push(char::from(HEX[(byte & 15) as usize]));
            }
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn response(bytes: &'static [u8]) -> Result<Value, String> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let endpoint = Endpoint {
            port: listener.local_addr().unwrap().port(),
            token: "test-token".into(),
        };
        let worker = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream);
            let mut request = String::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let end = line == "\r\n";
                request.push_str(&line);
                if end {
                    break;
                }
            }
            assert!(request.contains("X-DiskMap-Token: test-token\r\n"));
            reader.get_mut().write_all(bytes).unwrap();
        });
        let result = endpoint.request(
            "GET",
            "/api/agent/status",
            &[],
            None,
            Duration::from_secs(2),
        );
        worker.join().unwrap();
        result
    }

    #[test]
    fn reads_lengths_chunks_and_json_errors() {
        assert_eq!(
            response(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}").unwrap(),
            serde_json::json!({})
        );
        assert_eq!(
            response(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\n{}\r\n0\r\n\r\n")
                .unwrap(),
            serde_json::json!({})
        );
        assert_eq!(
            response(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 15\r\n\r\n{\"error\":\"bad\"}")
                .unwrap_err(),
            "bad"
        );
        assert!(response(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\n{}").is_err());
        assert!(
            response(
                b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nTransfer-Encoding: chunked\r\n\r\n{}"
            )
            .is_err()
        );
    }

    #[test]
    fn query_encoding_preserves_unicode_and_separators() {
        assert_eq!(
            encode("C:\\a b/中文&?#"),
            "C%3A%5Ca+b%2F%E4%B8%AD%E6%96%87%26%3F%23"
        );
    }

    #[test]
    fn rejects_header_injection_before_connecting() {
        let ep = Endpoint {
            port: 1,
            token: "token\r\nX: evil".into(),
        };
        assert!(
            ep.request(
                "GET",
                "/api/agent/status",
                &[],
                None,
                Duration::from_secs(1)
            )
            .unwrap_err()
            .contains("Invalid")
        );
    }
}
