//! 严格读取版本一指针，空值字段也必须显式存在。

use crate::KernelError;
use serde::{Deserialize, Deserializer, Serialize, de};
use std::{fs, io, path::Path};

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CurrentRecord {
    #[serde(deserialize_with = "schema_one")]
    pub schema: u32,
    #[serde(deserialize_with = "digest")]
    pub gif: String,
    pub bytes: u64,
    pub published_at: u64,
    #[serde(deserialize_with = "Option::deserialize")]
    pub ttl_seconds: Option<u64>,
    #[serde(deserialize_with = "fallback_name")]
    pub fallback: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FallbackRecord {
    #[serde(deserialize_with = "schema_one")]
    pub schema: u32,
    #[serde(deserialize_with = "digest")]
    pub gif: String,
    pub bytes: u64,
    pub set_at: u64,
}

fn schema_one<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    match u32::deserialize(d)? {
        1 => Ok(1),
        _ => Err(de::Error::custom("记录版本必须为 1")),
    }
}

pub(super) fn is_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn digest<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    let value = String::deserialize(d)?;
    if !is_digest(&value) {
        return Err(de::Error::custom("内容哈希必须为 64 位小写十六进制"));
    }
    Ok(value)
}

fn fallback_name<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    let value = Option::<String>::deserialize(d)?;
    if let Some(name) = &value {
        super::validate_name(name).map_err(de::Error::custom)?;
    }
    Ok(value)
}

pub(super) fn read_record<T: de::DeserializeOwned>(path: &Path) -> Result<Option<T>, KernelError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => {
            return Err(KernelError::State(format!(
                "无法读取记录：{}",
                path.display()
            )));
        }
    };
    serde_json::from_slice(&bytes).map(Some).map_err(|_| {
        KernelError::State(format!(
            "记录损坏：{}（字段、类型、版本或格式不正确）",
            path.display()
        ))
    })
}
