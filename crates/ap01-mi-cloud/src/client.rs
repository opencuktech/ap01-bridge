//! 有序表单装配与响应信封解析，复用调用方已建立的会话。

use crate::{
    MiCloudError, crypto,
    session::{Endpoints, Session, cookie_header, runtime},
    transport::{HttpRequest, HttpResponse, Transport},
};
use serde_json::Value;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const API_AGENT: &str =
    "Android-7.1.1-1.0.0-ONEPLUS A3010-136-ABCDEF1234567 APP/xiaomi.smarthome APPV/62830";

/// 随机值与时钟显式注入，便于复现完整协议请求。
pub trait RequestSource {
    fn random_u64(&self) -> Result<u64, MiCloudError>;
    fn unix_seconds(&self) -> Result<u64, MiCloudError>;
}

/// 生产环境只使用系统随机源和系统时钟。
pub struct SystemSource;
impl RequestSource for SystemSource {
    fn random_u64(&self) -> Result<u64, MiCloudError> {
        let mut bytes = [0; 8];
        getrandom::fill(&mut bytes).map_err(|_| runtime("无法生成请求随机数"))?;
        Ok(u64::from_be_bytes(bytes))
    }
    fn unix_seconds(&self) -> Result<u64, MiCloudError> {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .map_err(|_| runtime("无法读取请求时间"))
    }
}

/// 每个客户端只接受现成会话，业务请求没有重新登录的路径。
pub struct Client<'a> {
    transport: &'a dyn Transport,
    session: Session,
    endpoints: Endpoints,
    source: &'a dyn RequestSource,
}

fn encode(value: &str) -> String {
    let mut output = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'*' | b'-' | b'.' | b'_' => {
                output.push(char::from(byte))
            }
            b' ' => output.push('+'),
            _ => {
                use std::fmt::Write;
                let _ = write!(output, "%{byte:02X}");
            }
        }
    }
    output
}

fn decode_response(
    response: &HttpResponse,
    key: &str,
    sanitize: impl Fn(&str) -> String,
) -> Result<Value, MiCloudError> {
    if !(200..300).contains(&response.status) {
        return Err(runtime("业务响应状态异常"));
    }
    let body = std::str::from_utf8(&response.body).map_err(|_| runtime("业务响应编码无效"))?;
    let plain = crypto::rc4_decrypt(key, body.trim()).map_err(|_| runtime("业务响应解密失败"))?;
    let value: Value = serde_json::from_str(&plain).map_err(|_| runtime("业务响应格式无效"))?;
    if !value.is_object() {
        return Err(runtime("业务响应不是对象"));
    }
    let code = value["code"]
        .as_number()
        .filter(|n| n.is_i64() || n.is_u64())
        .ok_or_else(|| runtime("业务响应缺少整数状态码"))?;
    if code.as_i64() != Some(0) {
        let message = sanitize(value["message"].as_str().unwrap_or("未提供说明"));
        return Err(runtime(&format!("业务请求失败，状态码 {code}：{message}")));
    }
    Ok(value)
}

impl<'a> Client<'a> {
    pub fn new(
        transport: &'a dyn Transport,
        session: Session,
        endpoints: Endpoints,
        source: &'a dyn RequestSource,
    ) -> Self {
        Self {
            transport,
            session,
            endpoints,
            source,
        }
    }

    /// 接口负载由调用方按协议键序生成；不重新序列化。
    pub fn request(&self, path: &str, data: &str) -> Result<Value, MiCloudError> {
        let url = format!(
            "{}/app/{}",
            self.endpoints.api,
            path.trim_start_matches('/')
        );
        if !self.endpoints.allows(&url, false) {
            return Err(runtime("业务地址不符合加密传输要求"));
        }
        let random = self
            .source
            .random_u64()
            .map_err(|_| runtime("无法生成请求随机数"))?;
        let seconds = self
            .source
            .unix_seconds()
            .map_err(|_| runtime("无法读取请求时间"))?;
        let nonce = crypto::nonce(crypto::random_u64_bytes(random), seconds);
        let key = crypto::signed_nonce(&self.session.ssecurity, &nonce)
            .map_err(|_| runtime("会话密钥编码无效"))?;
        let plain = [("data".into(), data.into())];
        let hash = crypto::signature("POST", &url, &plain, &key);
        let encrypt =
            |value: &str| crypto::rc4_encrypt(&key, value).map_err(|_| runtime("请求加密失败"));
        let mut form = vec![
            ("data".into(), encrypt(data)?),
            ("rc4_hash__".into(), encrypt(&hash)?),
        ];
        let signature = crypto::signature("POST", &url, &form, &key);
        form.extend([
            ("signature".into(), signature),
            ("ssecurity".into(), self.session.ssecurity.clone()),
            ("_nonce".into(), nonce),
        ]);
        let cookies = [
            ("userId".into(), self.session.user_id.clone()),
            (
                "yetAnotherServiceToken".into(),
                self.session.service_token.clone(),
            ),
            ("serviceToken".into(), self.session.service_token.clone()),
            ("locale".into(), "zh_CN".into()),
            ("timezone".into(), "GMT+08:00".into()),
            ("channel".into(), "MI_APP_STORE".into()),
        ];
        let request = HttpRequest {
            method: "POST".into(),
            url,
            headers: vec![
                ("User-Agent".into(), API_AGENT.into()),
                ("Accept-Encoding".into(), "identity".into()),
                ("MIOT-ENCRYPT-ALGORITHM".into(), "ENCRYPT-RC4".into()),
                (
                    "Content-Type".into(),
                    "application/x-www-form-urlencoded".into(),
                ),
                ("Cookie".into(), cookie_header(&cookies)),
            ],
            body: Some(
                form.iter()
                    .map(|(name, value)| format!("{}={}", encode(name), encode(value)))
                    .collect::<Vec<_>>()
                    .join("&")
                    .into_bytes(),
            ),
            timeout: Duration::from_secs(30),
        };
        let response = self
            .transport
            .send(&request)
            .map_err(|e| runtime(&e.to_string()))?;
        let target = path.strip_prefix("home/rpc/").unwrap_or("");
        decode_response(&response, &key, |message| {
            self.session.sanitize(message, &[target])
        })
    }

    pub(crate) fn rpc_id(&self) -> Result<u64, MiCloudError> {
        // 拒绝尾部余数，避免取模偏差。
        const RANGE: u64 = 9_000_000;
        const LIMIT: u64 = u64::MAX - u64::MAX % RANGE;
        loop {
            let value = self
                .source
                .random_u64()
                .map_err(|_| runtime("无法生成请求随机数"))?;
            if value < LIMIT {
                return Ok(1_000_000 + value % RANGE);
            }
        }
    }

    pub(crate) fn check_public(text: &str) -> Result<(), MiCloudError> {
        // 型号与版本号最多 128 个 ASCII 字符，不受会话中短值的影响。
        if text.is_empty()
            || text.len() > 128
            || !text
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        {
            return Err(runtime("设备公开字段包含不可输出的内容"));
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;
