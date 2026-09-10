//! 同步传输边界；加密协议与会话策略由上层负责。

use std::{fmt, io::Read, time::Duration};
use ureq::{
    Agent, AsSendBody,
    config::AutoHeaderValue,
    tls::{RootCerts, TlsConfig, TlsProvider},
};

/// 请求数据按调用方给定的顺序保存，不提供原始内容的调试输出。
#[derive(Clone, PartialEq, Eq)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
    pub timeout: Duration,
}

/// 原始响应；重复响应头保留为独立条目。
#[derive(Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// 传输失败只记录类别，绝不保留底层错误中的请求信息。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportError {
    /// 请求字段不合法。
    InvalidRequest,
    /// 连接、加密协商或读写失败。
    Failed,
    /// 请求超过时限。
    Timeout,
    /// 测试预置响应已用尽。
    ScriptExhausted,
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidRequest => "请求格式无效",
            Self::Failed => "网络传输失败",
            Self::Timeout => "网络请求超时",
            Self::ScriptExhausted => "测试响应已用尽",
        })
    }
}
impl std::error::Error for TransportError {}

/// 上层唯一的网络调用接口，可由离线脚本替换。
pub trait Transport {
    fn send(&self, req: &HttpRequest) -> Result<HttpResponse, TransportError>;
}

/// 使用打包根证书的同步适配器；明文限制留给会话与请求层。
pub struct UreqTransport {
    agent: Agent,
}

impl Default for UreqTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl UreqTransport {
    /// 禁用自动跳转、自动头及环境代理，保持请求行为由调用方控制。
    pub fn new() -> Self {
        let config = Agent::config_builder()
            .tls_config(
                TlsConfig::builder()
                    .provider(TlsProvider::Rustls)
                    .root_certs(RootCerts::WebPki)
                    .build(),
            )
            .max_redirects(0)
            .http_status_as_error(false)
            .user_agent(AutoHeaderValue::None)
            .accept(AutoHeaderValue::None)
            .accept_encoding(AutoHeaderValue::None)
            .proxy(None)
            .build();
        Self {
            agent: config.new_agent(),
        }
    }
}

fn transport_error(error: ureq::Error) -> TransportError {
    match error {
        ureq::Error::Timeout(_) => TransportError::Timeout,
        ureq::Error::Io(error) if error.kind() == std::io::ErrorKind::TimedOut => {
            TransportError::Timeout
        }
        _ => TransportError::Failed,
    }
}

impl Transport for UreqTransport {
    fn send(&self, req: &HttpRequest) -> Result<HttpResponse, TransportError> {
        let mut builder = ureq::http::Request::builder()
            .method(req.method.as_str())
            .uri(&req.url);
        for (name, value) in &req.headers {
            builder = builder.header(name, value);
        }
        let mut bytes = req.body.as_deref().unwrap_or_default();
        let mut empty = ();
        let body = if req.body.is_some() {
            bytes.as_body()
        } else {
            empty.as_body()
        };
        let request = builder
            .body(body)
            .map_err(|_| TransportError::InvalidRequest)?;
        let request = self
            .agent
            .configure_request(request)
            .timeout_global(Some(req.timeout))
            .build();
        let mut response = self.agent.run(request).map_err(transport_error)?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(name, value)| {
                // 字节到字符的一一映射保留非文本响应头，不影响常见的 ASCII 头。
                (
                    name.to_string(),
                    value
                        .as_bytes()
                        .iter()
                        .map(|&byte| char::from(byte))
                        .collect(),
                )
            })
            .collect();
        let mut body = Vec::new();
        response
            .body_mut()
            .as_reader()
            .read_to_end(&mut body)
            .map_err(|error| transport_error(error.into()))?;
        Ok(HttpResponse {
            status,
            headers,
            body,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_error_preserves_wrapped_timeout() {
        let error = ureq::Error::Timeout(ureq::Timeout::Global).into_io();
        assert_eq!(transport_error(error.into()), TransportError::Timeout);
        assert_eq!(
            transport_error(std::io::Error::from(std::io::ErrorKind::TimedOut).into()),
            TransportError::Timeout
        );
        assert_eq!(
            transport_error(std::io::Error::from(std::io::ErrorKind::ConnectionReset).into()),
            TransportError::Failed
        );
    }

    #[test]
    fn transport_invalid_headers_fail_before_network() {
        let request = HttpRequest {
            method: "GET".into(),
            url: "/synthetic".into(),
            headers: vec![("invalid\r\nname".into(), "synthetic".into())],
            body: None,
            timeout: Duration::from_secs(1),
        };
        assert!(matches!(
            UreqTransport::new().send(&request),
            Err(TransportError::InvalidRequest)
        ));
    }
}
