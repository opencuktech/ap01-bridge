//! 按显式时间解析当前内容和回退槽，不维护后台状态。

use super::{CurrentRecord, FallbackRecord, content_path, records::read_record};
use serde::Serialize;
use std::{fs::File, path::Path};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Serving {
    Current,
    Fallback,
    None,
}

#[derive(Debug, Serialize)]
pub struct CurrentStatus {
    pub gif: String,
    pub bytes: u64,
    pub published_at: u64,
    pub ttl_seconds: Option<u64>,
    pub expires_at: Option<u64>,
    pub expired: bool,
}

#[derive(Debug, Serialize)]
pub struct FallbackStatus {
    pub name: String,
    pub gif: Option<String>,
    pub bytes: Option<u64>,
    pub missing: bool,
}

#[derive(Debug, Serialize)]
pub struct ServingStatus {
    pub serving: Serving,
    pub current: Option<CurrentStatus>,
    pub fallback: Option<FallbackStatus>,
    pub error: Option<String>,
}

impl ServingStatus {
    /// 返回当前应供应的哈希，供调用方定位内容文件。
    pub fn serving_gif(&self) -> Option<&str> {
        match self.serving {
            Serving::Current => self.current.as_ref().map(|current| current.gif.as_str()),
            Serving::Fallback => self
                .fallback
                .as_ref()
                .and_then(|fallback| fallback.gif.as_deref()),
            Serving::None => None,
        }
    }
}

fn readable(path: &Path, bytes: u64) -> Result<(), String> {
    File::open(path)
        .and_then(|file| file.metadata())
        .ok()
        .filter(|metadata| metadata.is_file())
        .ok_or_else(|| format!("内容文件缺失或不可读：{}", path.display()))
        .and_then(|metadata| {
            if metadata.len() == bytes {
                Ok(())
            } else {
                Err(format!("内容文件长度与记录不一致：{}", path.display()))
            }
        })
}

/// 查询异常通过 error 表达，查询本身永不失败。
pub fn resolve_status(data_dir: &Path, now: u64) -> ServingStatus {
    let mut status = ServingStatus {
        serving: Serving::None,
        current: None,
        fallback: None,
        error: None,
    };
    let record = match read_record::<CurrentRecord>(&data_dir.join("current.json")) {
        Ok(Some(record)) => record,
        Ok(None) => {
            status.error = Some("尚未发布任何内容".into());
            return status;
        }
        Err(error) => {
            status.error = Some(error.to_string());
            return status;
        }
    };
    // 用宽整数比较避免恶意或极端时间溢出；显示值无法用 u64 表示时留空。
    let deadline = record
        .ttl_seconds
        .map(|ttl| u128::from(record.published_at) + u128::from(ttl));
    let expired = deadline.is_some_and(|end| u128::from(now) >= end);
    let availability = readable(&content_path(data_dir, &record.gif), record.bytes);
    let available = availability.is_ok();
    if let Err(error) = availability {
        status.error = Some(format!("当前{error}"));
    }
    status.current = Some(CurrentStatus {
        gif: record.gif,
        bytes: record.bytes,
        published_at: record.published_at,
        ttl_seconds: record.ttl_seconds,
        expires_at: deadline.and_then(|end| u64::try_from(end).ok()),
        expired,
    });
    if let Some(name) = record.fallback {
        let path = data_dir.join("fallbacks").join(format!("{name}.json"));
        let pointer = read_record::<FallbackRecord>(&path).ok().flatten();
        let mut fallback = FallbackStatus {
            name,
            gif: None,
            bytes: None,
            missing: true,
        };
        if let Some(pointer) = pointer {
            fallback.missing =
                readable(&content_path(data_dir, &pointer.gif), pointer.bytes).is_err();
            fallback.gif = Some(pointer.gif);
            fallback.bytes = Some(pointer.bytes);
        }
        status.fallback = Some(fallback);
    }
    status.serving = if available && !expired {
        Serving::Current
    } else if status
        .fallback
        .as_ref()
        .is_some_and(|fallback| !fallback.missing)
    {
        Serving::Fallback
    } else {
        Serving::None
    };
    status
}
