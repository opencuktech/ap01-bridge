//! 集成测试共享的独占临时目录与受控子进程环境。

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

pub struct TempDir(pub PathBuf);

impl TempDir {
    pub fn new() -> Self {
        static NEXT_DIR: AtomicU64 = AtomicU64::new(0);
        for _ in 0..128 {
            let sequence = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("ap01-cli-test-{}-{sequence}", std::process::id()));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => panic!("无法创建测试临时目录"),
            }
        }
        panic!("无法找到未占用的测试临时目录名称");
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub fn bridge() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_bridge"));
    command.env_clear();
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("AP01_BRIDGE_MI_") {
            command.env_remove(name);
        }
    }
    // Windows 的系统库加载可能依赖这两个变量，其余宿主环境不继承。
    #[cfg(windows)]
    for name in ["PATH", "SystemRoot"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command
}
