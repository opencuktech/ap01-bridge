//! 常驻文件上的跨进程排他锁，由操作系统在进程结束时释放。

use crate::KernelError;
use std::{
    fs::{self, File, OpenOptions, TryLockError},
    path::Path,
};

pub(super) struct WriteLock {
    file: File,
}

impl WriteLock {
    pub(super) fn acquire(data_dir: &Path) -> Result<Self, KernelError> {
        let path = data_dir.join("write.lock");
        let unavailable = || KernelError::State(format!("无法获取写锁：{}", path.display()));
        fs::create_dir_all(data_dir).map_err(|_| unavailable())?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|_| unavailable())?;
        match file.try_lock() {
            Ok(()) => Ok(Self { file }),
            Err(TryLockError::WouldBlock) => Err(KernelError::State(
                "另一个写命令正在运行，请稍后重试".into(),
            )),
            Err(_) => Err(unavailable()),
        }
    }
}

impl Drop for WriteLock {
    fn drop(&mut self) {
        // 显式解锁后由字段析构关闭句柄；永远保留同一个锁文件。
        let _ = self.file.unlock();
    }
}
