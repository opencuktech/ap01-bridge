//! 只读取请求行，不解释请求头的任何语义。

use std::{
    fmt,
    io::{self, Read},
    time::{Duration, Instant},
};

#[derive(Debug, PartialEq, Eq)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub version: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum RequestError {
    Invalid,
    Timeout,
}

impl fmt::Display for RequestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Invalid => "请求头不完整、过长或请求行无法解析",
            Self::Timeout => "请求头读取超时",
        })
    }
}

impl std::error::Error for RequestError {}

/// 从接受连接起使用同一个截止时刻，按块读取且累计最多 8192 字节。
/// 每次读取前通过注入函数设置剩余超时，便于使用模拟 Read 验证截止行为。
pub fn read_request<R: Read + ?Sized>(
    reader: &mut R,
    deadline: Instant,
    mut set_timeout: impl FnMut(&R, Duration) -> io::Result<()>,
) -> Result<Request, RequestError> {
    let mut header = Vec::new();
    let mut line_start = 0;
    let mut buffer = [0; 1024];
    while header.len() < 8192 {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(RequestError::Timeout);
        }
        set_timeout(reader, remaining).map_err(|_| RequestError::Invalid)?;
        let limit = buffer.len().min(8192 - header.len());
        let count = match reader.read(&mut buffer[..limit]) {
            Ok(0) => return Err(RequestError::Invalid),
            Ok(count) => count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) =>
            {
                return Err(RequestError::Timeout);
            }
            Err(_) => return Err(RequestError::Invalid),
        };
        if Instant::now() >= deadline {
            return Err(RequestError::Timeout);
        }
        let previous_len = header.len();
        header.extend_from_slice(&buffer[..count]);
        // 保留跨块的行起点，同时容忍 CRLF、裸 LF 与混合行尾。
        for end in previous_len..header.len() {
            if header[end] != b'\n' {
                continue;
            }
            let line = &header[line_start..end];
            if line.is_empty() || line == b"\r" {
                let first_end = header
                    .iter()
                    .position(|&b| b == b'\n')
                    .ok_or(RequestError::Invalid)?;
                let first =
                    std::str::from_utf8(&header[..first_end]).map_err(|_| RequestError::Invalid)?;
                let first = first.strip_suffix('\r').unwrap_or(first);
                if first.chars().any(char::is_control) {
                    return Err(RequestError::Invalid);
                }
                let parts: Vec<_> = first.split(' ').filter(|s| !s.is_empty()).collect();
                let [method, path, version] = parts.as_slice() else {
                    return Err(RequestError::Invalid);
                };
                return Ok(Request {
                    method: (*method).into(),
                    path: path.split('?').next().unwrap_or_default().into(),
                    version: (*version).into(),
                });
            }
            line_start = end + 1;
        }
    }
    Err(RequestError::Invalid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io, thread};

    fn read(reader: &mut dyn Read) -> Result<Request, RequestError> {
        read_request(reader, Instant::now() + Duration::from_secs(5), |_, _| {
            Ok(())
        })
    }

    #[test]
    fn valid_crlf_lf_mixed_query_and_methods() {
        for ending in ["\r\n\r\n", "\n\n", "\r\n\n", "\n\r\n"] {
            for method in ["GET", "HEAD", "POST", "get"] {
                for path in ["/screen.gif", "/screen.gif?ts=12345"] {
                    let input = format!("{method}   {path}  HTTP/1.0{ending}");
                    assert_eq!(
                        read(&mut input.as_bytes()).unwrap(),
                        Request {
                            method: method.into(),
                            path: "/screen.gif".into(),
                            version: "HTTP/1.0".into(),
                        }
                    );
                }
            }
        }
        let mut input = &b"GET /health HTTP/1.1\r\nHost: foo\nX: \xff\r\n\nBODY"[..];
        assert_eq!(read(&mut input).unwrap().version, "HTTP/1.1");
        // 按块读取可能包含正文；正文不参与解析，连接也不会复用。
    }

    #[test]
    fn garbage_empty_eof_and_missing_version_are_errors() {
        for input in [
            b"".as_slice(),
            b"garbage\n\n",
            b"GET /screen.gif\n\n",
            b"GET / HTTP/1.0 extra\n\n",
            b"GET / HTTP/1.0\r\n",
            b"GET\t/ HTTP/1.0\n\n",
            b"\xff / HTTP/1.0\n\n",
        ] {
            assert!(read(&mut &*input).is_err());
        }
    }

    #[test]
    fn limit_accepts_complete_header_at_8192_and_never_reads_beyond_it() {
        let mut input = b"GET / HTTP/1.0\nX: ".to_vec();
        input.resize(8190, b'x');
        input.extend(b"\n\n");
        assert!(read(&mut input.as_slice()).is_ok());
        let oversized = vec![b'x'; 9000];
        let mut reader = oversized.as_slice();
        assert!(read(&mut reader).is_err());
        assert_eq!(reader.len(), 9000 - 8192);
    }

    #[test]
    fn timeout_is_a_request_error() {
        struct Timeout;
        impl Read for Timeout {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::TimedOut, "模拟读取超时"))
            }
        }
        assert_eq!(read(&mut Timeout), Err(RequestError::Timeout));
    }

    struct Drip {
        reads: usize,
    }

    impl Read for Drip {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            thread::sleep(Duration::from_millis(10));
            self.reads += 1;
            buffer[0] = b'x';
            Ok(1)
        }
    }

    #[test]
    fn expired_deadline_rejects_drip_without_reading() {
        let mut reader = Drip { reads: 0 };
        let started = Instant::now();
        assert_eq!(
            read_request(&mut reader, started - Duration::from_secs(5), |_, _| {
                panic!("已过截止时刻，不应配置或读取连接")
            }),
            Err(RequestError::Timeout)
        );
        assert_eq!(reader.reads, 0);
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn remaining_timeout_decreases_across_drip_reads() {
        let mut reader = Drip { reads: 0 };
        let mut timeouts = Vec::new();
        let deadline = Instant::now() + Duration::from_millis(50);
        assert_eq!(
            read_request(&mut reader, deadline, |_, timeout| {
                timeouts.push(timeout);
                Ok(())
            }),
            Err(RequestError::Timeout)
        );
        assert!(reader.reads < 8192);
        assert!(timeouts.windows(2).all(|pair| pair[1] < pair[0]));
    }

    #[test]
    fn header_terminator_is_recognized_across_block_boundaries() {
        for ending in [b"\r\n\r\n".as_slice(), b"\n\n", b"\r\n\n", b"\n\r\n"] {
            for split in 1..ending.len() {
                let mut input = b"GET /screen.gif HTTP/1.0\r\nX: ".to_vec();
                input.resize(1024 - split, b'x');
                input.extend_from_slice(ending);
                assert_eq!(read(&mut input.as_slice()).unwrap().path, "/screen.gif");
            }
        }
    }
}
