//! 无状态的随机数编码、摘要、签名与流加密；随机源及时间由调用方提供。

use base64::{Engine, engine::general_purpose::STANDARD};
use sha1::Sha1;
use sha2::{Digest, Sha256};

/// 协议原语失败；不携带输入字节。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CryptoError {
    /// 输入编码不合法。
    InvalidBase64,
    /// 流加密密钥不能为空。
    EmptyKey,
    /// 解密结果不是合法文本。
    InvalidUtf8,
}

impl std::fmt::Display for CryptoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidBase64 => "编码内容无效",
            Self::EmptyKey => "加密密钥为空",
            Self::InvalidUtf8 => "解密文本无效",
        })
    }
}

impl std::error::Error for CryptoError {}

/// 把无符号随机整数减去有符号范围中点，编码为八字节大端补码。
pub fn random_u64_bytes(random: u64) -> [u8; 8] {
    (random ^ (1 << 63)).to_be_bytes()
}

/// 拼接已转换的随机字节与分钟数的最小大端编码。
///
/// 旧实现按位长计算编码长度，所以分钟数为零时追加空字节序列。
/// 真实调用可将系统随机源取得的八字节直接注入，分布保持均匀。
pub fn nonce(rand8: [u8; 8], unix_secs: u64) -> String {
    let minute = (unix_secs / 60).to_be_bytes();
    let start = minute.iter().position(|&byte| byte != 0).unwrap_or(8);
    let mut raw = Vec::with_capacity(16);
    raw.extend_from_slice(&rand8);
    raw.extend_from_slice(&minute[start..]);
    STANDARD.encode(raw)
}

fn decode(value: &str) -> Result<Vec<u8>, CryptoError> {
    STANDARD
        .decode(value)
        .map_err(|_| CryptoError::InvalidBase64)
}

/// 对两段编码各自解码，拼接原始字节后计算摘要。
pub fn signed_nonce(ssecurity_b64: &str, nonce_b64: &str) -> Result<String, CryptoError> {
    let mut digest = Sha256::new();
    digest.update(decode(ssecurity_b64)?);
    digest.update(decode(nonce_b64)?);
    Ok(STANDARD.encode(digest.finalize()))
}

fn rc4_bytes(key: &[u8], source: &[u8], drop_count: usize) -> Result<Vec<u8>, CryptoError> {
    if key.is_empty() {
        return Err(CryptoError::EmptyKey);
    }
    let mut state = std::array::from_fn::<_, 256, _>(|i| i as u8);
    let mut j = 0usize;
    for i in 0..256 {
        j = (j + usize::from(state[i]) + usize::from(key[i % key.len()])) & 255;
        state.swap(i, j);
    }
    let (mut i, mut j) = (0usize, 0usize);
    let mut next_byte = || {
        i = (i + 1) & 255;
        j = (j + usize::from(state[i])) & 255;
        state.swap(i, j);
        state[(usize::from(state[i]) + usize::from(state[j])) & 255]
    };
    for _ in 0..drop_count {
        next_byte();
    }
    Ok(source.iter().map(|byte| byte ^ next_byte()).collect())
}

/// 重新初始化密钥调度，丢弃前一千零二十四字节后加密并编码。
pub fn rc4_encrypt(key_b64: &str, plain: &str) -> Result<String, CryptoError> {
    Ok(STANDARD.encode(rc4_bytes(&decode(key_b64)?, plain.as_bytes(), 1024)?))
}

/// 独立初始化密钥调度，解密并校验文本编码。
pub fn rc4_decrypt(key_b64: &str, cipher_b64: &str) -> Result<String, CryptoError> {
    String::from_utf8(rc4_bytes(&decode(key_b64)?, &decode(cipher_b64)?, 1024)?)
        .map_err(|_| CryptoError::InvalidUtf8)
}

fn signature_text(method: &str, url: &str, params: &[(String, String)], key: &str) -> String {
    let path = if let Some((_, authority)) = url.split_once("://") {
        let end = authority.find(['/', '?', '#']).unwrap_or(authority.len());
        &authority[end..]
    } else {
        url
    };
    let path = path.split(['?', '#']).next().unwrap_or("");
    let path = if path.starts_with("/app/") {
        &path[4..]
    } else {
        path
    };
    let mut parts = vec![method.to_ascii_uppercase(), path.to_owned()];
    parts.extend(params.iter().map(|(name, value)| format!("{name}={value}")));
    parts.push(key.to_owned());
    parts.join("&")
}

/// 按参数切片顺序签名，只对指定路径前缀去掉应用段。
pub fn signature(method: &str, url: &str, params: &[(String, String)], key: &str) -> String {
    STANDARD.encode(Sha1::digest(
        signature_text(method, url, params, key).as_bytes(),
    ))
}

#[cfg(test)]
#[path = "crypto_tests.rs"]
mod tests;
