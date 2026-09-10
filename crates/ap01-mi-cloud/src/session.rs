//! 一次命令内的登录会话与逐跳传输策略。

use crate::{
    MiCloudError,
    credentials::Credentials,
    redact,
    transport::{HttpRequest, Transport},
};
use serde_json::Value;
use std::{fmt, time::Duration};

const ACCOUNT_BASE: &str = "https://account.xiaomi.com";
const API_BASE: &str = "https://api.io.mi.com";
const LOGIN_PATH: &str = "/pass/serviceLogin?sid=xiaomiio&_json=true";
const LOGIN_AGENT: &str = "APP/com.xiaomi.mihome APPV/9.1.200";

/// 地址策略在首次联网前校验，调试例外不进入发布构建。
#[derive(Clone)]
pub struct Endpoints {
    account: String,
    pub(crate) api: String,
    #[cfg(debug_assertions)]
    account_loopback: bool,
    #[cfg(debug_assertions)]
    api_loopback: bool,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            account: ACCOUNT_BASE.into(),
            api: API_BASE.into(),
            #[cfg(debug_assertions)]
            account_loopback: false,
            #[cfg(debug_assertions)]
            api_loopback: false,
        }
    }
}

fn valid_url(url: &str) -> bool {
    !url.chars().any(char::is_whitespace)
        && url.parse::<ureq::http::Uri>().ok().is_some_and(|uri| {
            uri.host().is_some() && uri.authority().is_some_and(|a| !a.as_str().contains('@'))
        })
}

#[cfg(debug_assertions)]
fn loopback(url: &str) -> bool {
    url.parse::<ureq::http::Uri>()
        .ok()
        .and_then(|uri| uri.host().map(str::to_ascii_lowercase))
        .is_some_and(|host| matches!(host.as_str(), "127.0.0.1" | "localhost" | "[::1]"))
}

impl Endpoints {
    /// 仅供调试命令注入基址；非回环明文在联网前拒绝。
    #[cfg(debug_assertions)]
    pub fn debug_overrides(
        account: Option<String>,
        api: Option<String>,
    ) -> Result<Self, MiCloudError> {
        let mut endpoints = Self::default();
        if let Some(url) = account {
            endpoints.account_loopback = loopback(&url);
            endpoints.account = url.trim_end_matches('/').into();
        }
        if let Some(url) = api {
            endpoints.api_loopback = loopback(&url);
            endpoints.api = url.trim_end_matches('/').into();
        }
        endpoints.validate()?;
        Ok(endpoints)
    }

    pub(crate) fn allows(&self, url: &str, account: bool) -> bool {
        if !valid_url(url) {
            return false;
        }
        if url.starts_with("https://") {
            return true;
        }
        #[cfg(debug_assertions)]
        if url.starts_with("http://")
            && loopback(url)
            && if account {
                self.account_loopback
            } else {
                self.api_loopback
            }
        {
            return true;
        }
        #[cfg(not(debug_assertions))]
        let _ = account;
        false
    }

    fn validate(&self) -> Result<(), MiCloudError> {
        if !self.allows(&self.account, true) || !self.allows(&self.api, false) {
            return Err(MiCloudError::State(redact::message(
                "连接地址不符合加密传输要求",
                &[],
            )));
        }
        Ok(())
    }
}

/// 会话只驻留内存，不自动续期，不序列化。
pub struct Session {
    pub(crate) user_id: String,
    pub(crate) ssecurity: String,
    pub(crate) service_token: String,
    secrets: Vec<String>,
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("登录会话")
            .field("密钥长度", &self.ssecurity.len())
            .field("令牌长度", &self.service_token.len())
            .finish()
    }
}

pub(crate) fn runtime(message: &str) -> MiCloudError {
    MiCloudError::Runtime(redact::message(message, &[]))
}

pub(crate) fn cookie_header(cookies: &[(String, String)]) -> String {
    cookies
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("; ")
}

fn safe_cookie(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_graphic() && !matches!(b, b';' | b','))
}

fn collect_cookies(
    headers: &[(String, String)],
    cookies: &mut Vec<(String, String)>,
    secrets: &mut Vec<String>,
) -> Result<(), MiCloudError> {
    for (_, header) in headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("set-cookie"))
    {
        if let Some((name, value)) = header.split(';').next().unwrap_or("").split_once('=') {
            let name = name.trim();
            let value = value.trim();
            if name.is_empty()
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
            {
                continue;
            }
            if value.is_empty() {
                cookies.retain(|(key, _)| key != name);
                continue;
            }
            if name == "serviceToken" && !safe_cookie(value) {
                return Err(runtime("登录响应会话字段无效"));
            }
            // 无法安全装入请求头的附加条目忽略，不影响有效的会话令牌。
            if !safe_cookie(value) {
                continue;
            }
            secrets.push(value.into());
            if let Some((_, old)) = cookies.iter_mut().find(|(key, _)| key == name) {
                *old = value.into();
            } else {
                cookies.push((name.into(), value.into()));
            }
        }
    }
    Ok(())
}

fn resolve_location(location: &str, previous: &str) -> Result<String, MiCloudError> {
    if location.starts_with('/') {
        let uri = previous
            .parse::<ureq::http::Uri>()
            .map_err(|_| runtime("登录跳转地址无效"))?;
        let scheme = uri
            .scheme_str()
            .ok_or_else(|| runtime("登录跳转地址无效"))?;
        let authority = uri.authority().ok_or_else(|| runtime("登录跳转地址无效"))?;
        return Ok(format!("{scheme}://{authority}{location}"));
    }
    Ok(location.into())
}

impl Session {
    pub(crate) fn sanitize(&self, message: &str, extra: &[&str]) -> String {
        let mut secrets: Vec<&str> = self.secrets.iter().map(String::as_str).collect();
        secrets.extend([
            self.user_id.as_str(),
            self.ssecurity.as_str(),
            self.service_token.as_str(),
        ]);
        secrets.extend_from_slice(extra);
        redact::message(message, &secrets)
    }

    /// 第一跳之后的所有失败均属运行失败，最多跟随五次跳转。
    pub fn login(
        transport: &dyn Transport,
        credentials: &Credentials,
        endpoints: &Endpoints,
    ) -> Result<Self, MiCloudError> {
        endpoints.validate()?;
        let mut cookies = vec![
            ("userId".into(), credentials.user_id.clone()),
            ("passToken".into(), credentials.pass_token.clone()),
            ("deviceId".into(), credentials.device_id.clone()),
        ];
        if cookies.iter().any(|(_, value)| !safe_cookie(value)) {
            return Err(MiCloudError::State(redact::message(
                "登录凭据格式无效",
                &[],
            )));
        }
        let get = |url: &str, cookies: &[(String, String)]| {
            transport
                .send(&HttpRequest {
                    method: "GET".into(),
                    url: url.into(),
                    headers: vec![
                        ("User-Agent".into(), LOGIN_AGENT.into()),
                        ("Cookie".into(), cookie_header(cookies)),
                    ],
                    body: None,
                    timeout: Duration::from_secs(20),
                })
                .map_err(|e| runtime(&e.to_string()))
        };
        let first_url = format!("{}{LOGIN_PATH}", endpoints.account);
        let first = get(&first_url, &cookies)?;
        if !(200..300).contains(&first.status) {
            return Err(runtime("登录响应状态异常"));
        }
        let text = std::str::from_utf8(&first.body)
            .map_err(|_| runtime("登录响应文本无效"))?
            .replace("&&&START&&&", "");
        let auth: Value = serde_json::from_str(&text).map_err(|_| runtime("登录响应格式无效"))?;
        let code = auth["code"]
            .as_number()
            .filter(|n| n.is_i64() || n.is_u64())
            .ok_or_else(|| runtime("登录响应缺少状态码"))?;
        if code.as_i64() != Some(0) {
            return Err(runtime(&format!("登录被拒绝，状态码 {code}，请刷新登录态")));
        }
        let mut location = auth["location"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| runtime(&format!("登录响应缺少跳转地址，状态码 {code}")))?
            .to_owned();
        let ssecurity = auth["ssecurity"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| runtime(&format!("登录响应缺少会话密钥，状态码 {code}")))?
            .to_owned();
        location = resolve_location(&location, &first_url)?;
        let mut secrets = cookies
            .iter()
            .map(|(_, value)| value.clone())
            .collect::<Vec<_>>();
        collect_cookies(&first.headers, &mut cookies, &mut secrets)?;
        let mut redirects = 0;
        loop {
            if !endpoints.allows(&location, true) {
                return Err(runtime("登录跳转地址不符合加密传输要求"));
            }
            secrets.push(location.clone());
            let response = get(&location, &cookies)?;
            collect_cookies(&response.headers, &mut cookies, &mut secrets)?;
            if matches!(response.status, 301 | 302 | 303 | 307 | 308) {
                if redirects == 5 {
                    return Err(runtime("登录跳转次数超过上限"));
                }
                let next_location = response
                    .headers
                    .iter()
                    .find(|(name, _)| name.eq_ignore_ascii_case("location"))
                    .map(|(_, value)| value.clone())
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| runtime("登录响应缺少跳转地址"))?;
                location = resolve_location(&next_location, &location)?;
                redirects += 1;
                continue;
            }
            if !(200..300).contains(&response.status) {
                return Err(runtime("登录响应状态异常"));
            }
            let service_token = cookies
                .iter()
                .find(|(name, _)| name == "serviceToken")
                .map(|(_, value)| value.clone())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| runtime("登录响应缺少会话令牌"))?;
            return Ok(Self {
                user_id: credentials.user_id.clone(),
                ssecurity,
                service_token,
                secrets,
            });
        }
    }
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;
