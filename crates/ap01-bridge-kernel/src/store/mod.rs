//! 内容寻址存储、原子发布与具名回退槽。

pub mod atomic;
mod gc;
mod lock;
mod records;
mod status;

pub use gc::collect_garbage;
pub use records::{CurrentRecord, FallbackRecord};
pub use status::{CurrentStatus, FallbackStatus, Serving, ServingStatus, resolve_status};

use crate::{
    KernelError,
    gif::{self, ValidationReport},
};
use records::{is_digest, read_record};
use serde::Serialize;
use std::{
    fs,
    path::{Path, PathBuf},
};

/// 手写槽名检查，限制为可直接作为文件名使用的 ASCII 字符。
pub fn validate_name(name: &str) -> Result<(), KernelError> {
    let mut bytes = name.bytes();
    let first_valid = bytes
        .next()
        .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
    if name.len() > 64
        || !first_valid
        || !bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
    {
        return Err(KernelError::Usage(
            "回退槽名必须匹配 [a-z0-9][a-z0-9_-]{0,63}".into(),
        ));
    }
    let reserved = matches!(name, "con" | "prn" | "aux" | "nul")
        || name
            .strip_prefix("com")
            .or_else(|| name.strip_prefix("lpt"))
            .is_some_and(|suffix| matches!(suffix.as_bytes(), [b'1'..=b'9']));
    if reserved {
        return Err(KernelError::Usage(
            "回退槽名不能是 Windows 保留设备名：con、prn、aux、nul、com1–com9、lpt1–lpt9".into(),
        ));
    }
    Ok(())
}

/// 按已校验的哈希定位内容；调用方应使用发布结果或供应状态里的哈希。
pub fn content_path(data_dir: &Path, gif: &str) -> PathBuf {
    data_dir.join("store").join(format!("{gif}.gif"))
}

/// 已存在且长度一致的内容不重写，长度不一致则原子修复；调用方负责先校验 GIF 并计算对应哈希。
pub fn store_content(data_dir: &Path, bytes: &[u8], sha256_hex: &str) -> Result<bool, KernelError> {
    if !is_digest(sha256_hex) {
        return Err(KernelError::Usage(
            "内容哈希必须为 64 位小写十六进制".into(),
        ));
    }
    let path = content_path(data_dir, sha256_hex);
    match fs::metadata(&path) {
        Ok(metadata) if metadata.is_file() => {
            if metadata.len() == bytes.len() as u64 {
                return Ok(false);
            }
        }
        Ok(_) => {
            return Err(KernelError::State(format!(
                "内容路径不是文件：{}",
                path.display()
            )));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(_) => {
            return Err(KernelError::State(format!(
                "无法检查内容文件：{}",
                path.display()
            )));
        }
    }
    atomic::write_file(&path, bytes)
        .map_err(|_| KernelError::State(format!("无法写入内容文件：{}", path.display())))?;
    Ok(true)
}

fn validated(bytes: &[u8]) -> Result<ValidationReport, KernelError> {
    let report = gif::validate(bytes);
    if !report.ok {
        return Err(KernelError::Rejected(
            report
                .errors
                .iter()
                .map(|error| error.message.as_str())
                .collect::<Vec<_>>()
                .join("；"),
        ));
    }
    Ok(report)
}

fn write_record(path: &Path, record: &impl Serialize) -> Result<(), KernelError> {
    let bytes = serde_json::to_vec(record)
        .map_err(|_| KernelError::Internal("无法序列化存储记录".into()))?;
    atomic::write_file(path, &bytes)
        .map_err(|_| KernelError::State(format!("无法提交存储记录：{}", path.display())))
}

fn create_layout(data_dir: &Path) -> Result<(), KernelError> {
    for name in ["store", "fallbacks", "logs"] {
        let path = data_dir.join(name);
        fs::create_dir_all(&path)
            .map_err(|_| KernelError::State(format!("无法创建数据目录：{}", path.display())))?;
    }
    Ok(())
}

#[derive(Debug, Default)]
pub struct PublishOptions {
    pub ttl_seconds: Option<u64>,
    pub fallback: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PublishResult {
    pub ok: bool,
    pub gif: String,
    pub bytes: u64,
    pub published_at: u64,
    pub ttl_seconds: Option<u64>,
    pub fallback: Option<String>,
    pub stored: bool,
    pub warnings: Vec<String>,
    pub report: ValidationReport,
}

/// 先完成全部输入检查，再入库、提交指针并尽力回收。
pub fn publish(
    data_dir: &Path,
    bytes: &[u8],
    now: u64,
    options: PublishOptions,
) -> Result<PublishResult, KernelError> {
    let report = validated(bytes)?;
    if let Some(name) = &options.fallback {
        validate_name(name)?;
    }
    let _lock = lock::WriteLock::acquire(data_dir)?;
    if let Some(name) = &options.fallback {
        let path = data_dir.join("fallbacks").join(format!("{name}.json"));
        match path.try_exists() {
            Ok(true) => (),
            Ok(false) => return Err(KernelError::State(format!("回退槽 {name} 不存在"))),
            Err(_) => {
                return Err(KernelError::State(format!(
                    "无法检查回退槽：{}",
                    path.display()
                )));
            }
        }
    }
    create_layout(data_dir)?;
    let stored = store_content(data_dir, bytes, &report.sha256)?;
    let record = CurrentRecord {
        schema: 1,
        gif: report.sha256.clone(),
        bytes: bytes.len() as u64,
        published_at: now,
        ttl_seconds: options.ttl_seconds,
        fallback: options.fallback,
    };
    write_record(&data_dir.join("current.json"), &record)?;
    let warnings = collect_garbage(data_dir, now);
    Ok(PublishResult {
        ok: true,
        gif: record.gif,
        bytes: record.bytes,
        published_at: now,
        ttl_seconds: record.ttl_seconds,
        fallback: record.fallback,
        stored,
        warnings,
        report,
    })
}

#[derive(Debug, Serialize)]
pub struct FallbackSetResult {
    pub ok: bool,
    pub name: String,
    pub gif: String,
    pub bytes: u64,
    pub set_at: u64,
    pub stored: bool,
    pub report: ValidationReport,
}

/// 设置回退槽不触发回收，旧内容由下一次成功发布回收。
pub fn set_fallback(
    data_dir: &Path,
    name: &str,
    bytes: &[u8],
    now: u64,
) -> Result<FallbackSetResult, KernelError> {
    validate_name(name)?;
    let report = validated(bytes)?;
    let _lock = lock::WriteLock::acquire(data_dir)?;
    create_layout(data_dir)?;
    let stored = store_content(data_dir, bytes, &report.sha256)?;
    let record = FallbackRecord {
        schema: 1,
        gif: report.sha256.clone(),
        bytes: bytes.len() as u64,
        set_at: now,
    };
    write_record(
        &data_dir.join("fallbacks").join(format!("{name}.json")),
        &record,
    )?;
    Ok(FallbackSetResult {
        ok: true,
        name: name.into(),
        gif: record.gif,
        bytes: record.bytes,
        set_at: now,
        stored,
        report,
    })
}

#[derive(Debug, Serialize)]
pub struct FallbackEntry {
    pub name: String,
    pub gif: String,
    pub bytes: u64,
    pub set_at: u64,
}

/// 按槽名排序；跳过损坏指针，使一个坏槽不妨碍查看其它可用槽。
pub fn list_fallbacks(data_dir: &Path) -> Result<Vec<FallbackEntry>, KernelError> {
    let path = data_dir.join("fallbacks");
    let entries = match fs::read_dir(&path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => {
            return Err(KernelError::State(format!(
                "无法读取回退槽目录：{}",
                path.display()
            )));
        }
    };
    let mut result = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|_| {
            KernelError::State(format!("无法读取回退槽目录条目：{}", path.display()))
        })?;
        let filename = entry.file_name();
        let Some(name) = filename
            .to_str()
            .and_then(|name| name.strip_suffix(".json"))
        else {
            continue;
        };
        if validate_name(name).is_err() {
            continue;
        }
        if let Ok(Some(record)) = read_record::<FallbackRecord>(&entry.path()) {
            result.push(FallbackEntry {
                name: name.into(),
                gif: record.gif,
                bytes: record.bytes,
                set_at: record.set_at,
            });
        }
    }
    result.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(result)
}

/// 删除未被当前发布引用的槽，仅删除指针文件。
pub fn remove_fallback(data_dir: &Path, name: &str) -> Result<(), KernelError> {
    validate_name(name)?;
    let _lock = lock::WriteLock::acquire(data_dir)?;
    let current =
        read_record::<CurrentRecord>(&data_dir.join("current.json")).map_err(|error| {
            KernelError::State(format!(
                "当前发布记录损坏或不可读，无法确认引用关系：{error}"
            ))
        })?;
    if let Some(current) = current
        && current.fallback.as_deref() == Some(name)
    {
        return Err(KernelError::State(format!("当前发布仍引用回退槽 {name}")));
    }
    let path = data_dir.join("fallbacks").join(format!("{name}.json"));
    fs::remove_file(&path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            KernelError::State(format!("回退槽 {name} 不存在"))
        } else {
            KernelError::State(format!("无法删除回退槽：{}", path.display()))
        }
    })
}
