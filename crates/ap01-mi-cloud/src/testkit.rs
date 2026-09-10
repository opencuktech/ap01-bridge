//! 可供应用层复用的离线脚本传输，不打开网络连接。

use crate::transport::{HttpRequest, HttpResponse, Transport, TransportError};
use std::{cell::RefCell, collections::VecDeque};

/// 按顺序消费响应，保留请求供断言；不实现原始数据的调试格式化。
pub struct FakeTransport {
    responses: RefCell<VecDeque<Result<HttpResponse, TransportError>>>,
    requests: RefCell<Vec<HttpRequest>>,
}

impl FakeTransport {
    /// 创建确定性响应脚本。
    pub fn new(responses: impl IntoIterator<Item = Result<HttpResponse, TransportError>>) -> Self {
        Self {
            responses: RefCell::new(responses.into_iter().collect()),
            requests: RefCell::new(Vec::new()),
        }
    }

    /// 取回已发送请求的独立副本，调用方不应输出其原始内容。
    pub fn requests(&self) -> Vec<HttpRequest> {
        self.requests.borrow().clone()
    }
}

impl Transport for FakeTransport {
    fn send(&self, req: &HttpRequest) -> Result<HttpResponse, TransportError> {
        self.requests.borrow_mut().push(req.clone());
        self.responses
            .borrow_mut()
            .pop_front()
            .unwrap_or(Err(TransportError::ScriptExhausted))
    }
}

#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;
    use crate::{
        MiCloudError,
        client::RequestSource,
        credentials::Credentials,
        crypto,
        session::{Endpoints, Session},
    };
    use serde_json::{Value, json};

    pub const SECURE_STS: &str = "https://example.invalid/sts";

    pub fn vectors() -> Value {
        crate::golden::vectors()
    }
    pub struct FixedSource;
    impl RequestSource for FixedSource {
        fn random_u64(&self) -> Result<u64, MiCloudError> {
            Ok(u64::from_str_radix(
                vectors()["inputs"]["getrandbits_64_hex"].as_str().unwrap(),
                16,
            )
            .unwrap())
        }
        fn unix_seconds(&self) -> Result<u64, MiCloudError> {
            Ok(vectors()["inputs"]["fixed_minute"].as_u64().unwrap() * 60)
        }
    }
    pub fn credentials() -> Credentials {
        Credentials {
            user_id: vectors()["inputs"]["user_id"].as_str().unwrap().into(),
            pass_token: "synthetic-pass".into(),
            device_id: "synthetic-device".into(),
        }
    }
    pub fn response(status: u16, body: &str, headers: &[(&str, &str)]) -> HttpResponse {
        HttpResponse {
            status,
            body: body.as_bytes().to_vec(),
            headers: headers
                .iter()
                .map(|(k, v)| ((*k).into(), (*v).into()))
                .collect(),
        }
    }
    pub fn auth(location: &str) -> HttpResponse {
        response(
            200,
            &format!(
                "&&&START&&&{}",
                json!({"code":0,"ssecurity":vectors()["inputs"]["ssecurity_b64"],"location":location})
            ),
            &[],
        )
    }
    pub fn login_responses() -> Vec<Result<HttpResponse, TransportError>> {
        vec![
            Ok(auth(SECURE_STS)),
            Ok(response(
                200,
                "",
                &[(
                    "Set-Cookie",
                    &format!(
                        "serviceToken={}",
                        vectors()["inputs"]["service_token"].as_str().unwrap()
                    ),
                )],
            )),
        ]
    }
    pub fn session() -> Session {
        Session::login(
            &FakeTransport::new(login_responses()),
            &credentials(),
            &Endpoints::default(),
        )
        .unwrap()
    }
    pub fn encrypted(plain: &str) -> HttpResponse {
        response(
            200,
            &crypto::rc4_encrypt(vectors()["signed_nonce"]["value"].as_str().unwrap(), plain)
                .unwrap(),
            &[],
        )
    }
    pub fn assert_runtime(error: MiCloudError) {
        assert_eq!(error.exit_code(), 5);
        for forbidden in [
            "http://",
            "https://",
            "Xiaomi",
            "xiaomi",
            "mi.com",
            "passToken",
            "serviceToken",
            "ssecurity",
            "userId",
            "deviceId",
            "did=",
            "DID",
            "cookie",
            "Cookie",
            "ota_url",
            ".bin",
        ] {
            assert!(!error.message().contains(forbidden));
        }
    }

    #[test]
    #[should_panic(expected = "assertion failed")]
    fn runtime_guard_rejects_ssecurity() {
        assert_runtime(MiCloudError::Runtime("ssecurity".into()));
    }
    pub fn decode_form(body: &[u8]) -> Vec<(String, String)> {
        fn decode(value: &str) -> String {
            let mut bytes = Vec::new();
            let mut input = value.bytes();
            while let Some(byte) = input.next() {
                bytes.push(match byte {
                    b'+' => b' ',
                    b'%' => {
                        let digits = [input.next().unwrap(), input.next().unwrap()];
                        u8::from_str_radix(std::str::from_utf8(&digits).unwrap(), 16).unwrap()
                    }
                    _ => byte,
                });
            }
            String::from_utf8(bytes).unwrap()
        }
        std::str::from_utf8(body)
            .unwrap()
            .split('&')
            .map(|part| {
                let (k, v) = part.split_once('=').unwrap();
                (decode(k), decode(v))
            })
            .collect()
    }
}
