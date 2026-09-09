//! 固定响应头与正文分别写入，写失败仅影响实际写出字节数。

use std::{
    io::{self, Write},
    sync::Arc,
};

pub struct Response {
    pub status: u16,
    pub content_type: &'static str,
    pub body: Arc<Vec<u8>>,
    pub allow: bool,
}

impl Response {
    pub(super) fn text(status: u16) -> Self {
        let message = match status {
            400 => "无法解析请求",
            405 => "仅支持 GET 与 HEAD 请求",
            _ => "没有可供应的内容",
        };
        Self {
            status,
            content_type: "text/plain",
            body: Arc::new(message.as_bytes().to_vec()),
            allow: status == 405,
        }
    }
}

/// 返回实际写出的头与正文字节总数；HEAD 保留对应正文的长度。
pub fn write_response(
    stream: &mut dyn Write,
    response: &Response,
    head_only: bool,
    version: &str,
) -> usize {
    let reason = match response.status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Unknown",
    };
    let mut buffer = format!(
        "HTTP/1.0 {} {reason}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nServer: ap01-bridge/{version}\r\n",
        response.status, response.content_type, response.body.len()
    ).into_bytes();
    if response.status == 405 && response.allow {
        buffer.extend_from_slice(b"Allow: GET, HEAD\r\n");
    }
    buffer.extend_from_slice(b"\r\n");
    let mut written = 0;
    // 正文直接借用缓存，不复制到头缓冲；短写时继续写余下部分并精确计数。
    for mut pending in [
        buffer.as_slice(),
        if head_only {
            &[]
        } else {
            response.body.as_slice()
        },
    ] {
        while !pending.is_empty() {
            match stream.write(pending) {
                Ok(0) => return written,
                Ok(count) => {
                    written += count;
                    pending = &pending[count..];
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => return written,
            }
        }
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_five_headers_order_body_and_head_length() {
        let response = Response {
            status: 200,
            content_type: "image/gif",
            body: Arc::new(b"GIF89a".to_vec()),
            allow: false,
        };
        let headers = b"HTTP/1.0 200 OK\r\nContent-Type: image/gif\r\nContent-Length: 6\r\nConnection: close\r\nCache-Control: no-store\r\nServer: ap01-bridge/0.1.0\r\n\r\n";
        for head in [false, true] {
            let mut output = Vec::new();
            let count = write_response(&mut output, &response, head, "0.1.0");
            let mut expected = headers.to_vec();
            if !head {
                expected.extend(b"GIF89a");
            }
            assert_eq!(output, expected);
            assert_eq!(count, expected.len());
        }
    }

    #[test]
    fn error_responses_only_add_allow_for_405() {
        for (status, reason) in [
            (400, "Bad Request"),
            (404, "Not Found"),
            (405, "Method Not Allowed"),
        ] {
            let response = Response::text(status);
            let mut output = Vec::new();
            write_response(&mut output, &response, false, "0.1.0");
            let expected = format!(
                "HTTP/1.0 {status} {reason}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nServer: ap01-bridge/0.1.0\r\n{}\r\n{}",
                response.body.len(),
                if status == 405 {
                    "Allow: GET, HEAD\r\n"
                } else {
                    ""
                },
                String::from_utf8_lossy(&response.body)
            );
            assert_eq!(output, expected.as_bytes());
        }
    }

    #[test]
    fn partial_failure_zero_write_and_interrupted_write_are_tolerated() {
        struct Partial {
            count: usize,
            limit: usize,
            zero: bool,
            interrupted: bool,
        }
        impl Write for Partial {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                if !self.interrupted {
                    self.interrupted = true;
                    return Err(io::ErrorKind::Interrupted.into());
                }
                if self.count == self.limit {
                    return if self.zero {
                        Ok(0)
                    } else {
                        Err(io::Error::new(io::ErrorKind::BrokenPipe, "模拟对端断开"))
                    };
                }
                let count = bytes.len().min(self.limit - self.count).min(3);
                self.count += count;
                Ok(count)
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let response = Response::text(404);
        let header_len = write_response(&mut Vec::new(), &response, true, "0.1.0");
        for limit in [17, header_len, header_len + 3] {
            for zero in [false, true] {
                assert_eq!(
                    write_response(
                        &mut Partial {
                            count: 0,
                            limit,
                            zero,
                            interrupted: false
                        },
                        &response,
                        false,
                        "0.1.0"
                    ),
                    limit
                );
            }
        }
    }
}
