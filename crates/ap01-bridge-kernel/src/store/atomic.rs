//! 同目录临时文件提交，正式路径永远只暴露完整内容。

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

/// 写完、同步并关闭临时文件后，原子替换正式文件。
pub fn write_file(final_path: &Path, bytes: &[u8]) -> io::Result<()> {
    static NEXT_FILE: AtomicU64 = AtomicU64::new(0);
    let parent = final_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = final_path
        .file_name()
        .ok_or_else(|| io::Error::other("写入目标缺少文件名"))?;
    fs::create_dir_all(parent)?;
    loop {
        let sequence = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
        let mut temporary = std::ffi::OsString::from(".");
        temporary.push(name);
        temporary.push(format!(".tmp-{}-{sequence}", std::process::id()));
        let temporary = parent.join(temporary);
        let mut file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        };
        let result = file.write_all(bytes).and_then(|()| file.sync_all());
        // Windows 上替换或删除之前必须释放文件句柄。
        drop(file);
        let result = result.and_then(|()| fs::rename(&temporary, final_path));
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        return result;
    }
}

pub(super) fn is_temporary(name: &str) -> bool {
    let Some((base, suffix)) = name.strip_prefix('.').and_then(|s| s.rsplit_once(".tmp-")) else {
        return false;
    };
    let Some((pid, sequence)) = suffix.split_once('-') else {
        return false;
    };
    !base.is_empty()
        && !pid.is_empty()
        && !sequence.is_empty()
        && pid.bytes().all(|b| b.is_ascii_digit())
        && sequence.bytes().all(|b| b.is_ascii_digit())
}
