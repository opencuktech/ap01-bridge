//! 发布后的尽力回收；无法确认当前引用时保守保留内容。

use super::{
    CurrentRecord, FallbackRecord,
    atomic::is_temporary,
    records::{is_digest, read_record},
    validate_name,
};
use std::{collections::HashSet, fs, path::Path, time::UNIX_EPOCH};

/// 清除无引用内容和三处目录中至少一小时前修改的临时文件，所有失败仅作为中文告警返回。
pub fn collect_garbage(data_dir: &Path, now: u64) -> Vec<String> {
    let mut warnings = Vec::new();
    let mut references = HashSet::new();
    let mut preserve_content = false;
    match read_record::<CurrentRecord>(&data_dir.join("current.json")) {
        Ok(Some(current)) => {
            references.insert(current.gif);
        }
        Ok(None) => (),
        Err(error) => {
            preserve_content = true;
            warnings.push(format!("无法确认引用关系，保留全部内容文件：{error}"));
        }
    }
    // 先收集全部槽引用；目录枚举失败时也保留内容，避免误删未知引用。
    let fallbacks = entries(&data_dir.join("fallbacks"), &mut warnings);
    if let Some(entries) = &fallbacks {
        for path in entries {
            let Some(name) = path
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.strip_suffix(".json"))
            else {
                continue;
            };
            if validate_name(name).is_ok() {
                match read_record::<FallbackRecord>(path) {
                    Ok(Some(record)) => {
                        references.insert(record.gif);
                    }
                    Ok(None) => (),
                    Err(error) => {
                        preserve_content = true;
                        warnings.push(format!("无法确认引用关系，保留全部内容文件：{error}"));
                    }
                }
            }
        }
    } else {
        preserve_content = true;
    }
    for directory in [
        data_dir.to_path_buf(),
        data_dir.join("store"),
        data_dir.join("fallbacks"),
    ] {
        let Some(paths) = entries(&directory, &mut warnings) else {
            continue;
        };
        for path in paths {
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let unreferenced = directory == data_dir.join("store")
                && !preserve_content
                && name
                    .strip_suffix(".gif")
                    .is_some_and(|hash| is_digest(hash) && !references.contains(hash));
            let residual = is_temporary(name) && is_stale(&path, now);
            if (residual || unreferenced) && fs::remove_file(&path).is_err() {
                warnings.push(format!("无法删除回收文件：{}", path.display()));
            }
        }
    }
    warnings
}

fn is_stale(path: &Path, now: u64) -> bool {
    // 无法读取修改时间、时间早于 epoch 或晚于注入时钟时，保守保留。
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .and_then(|modified| now.checked_sub(modified.as_secs()))
        .is_some_and(|age| age >= 3600)
}

fn entries(path: &Path, warnings: &mut Vec<String>) -> Option<Vec<std::path::PathBuf>> {
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Some(Vec::new()),
        Err(_) => {
            warnings.push(format!("无法读取回收目录：{}", path.display()));
            return None;
        }
    };
    match entries
        .map(|entry| entry.map(|entry| entry.path()))
        .collect()
    {
        Ok(paths) => Some(paths),
        Err(_) => {
            warnings.push(format!("无法读取回收目录条目：{}", path.display()));
            None
        }
    }
}
