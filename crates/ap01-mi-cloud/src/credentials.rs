//! 可注入环境、文件读取与子进程的凭据解析；命中一个来源即停止。

use crate::MiCloudError;
use serde_json::Value;
use std::{
    ffi::{OsStr, OsString},
    fmt,
    io::{self, Read},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

/// 协议规定的程序常量，与用户身份无关。
pub const DEFAULT_DEVICE_ID: &str = "B4C9D5BF5C4B6925";
const DEFAULT_PREFS: &str = "Library/Group Containers/group.com.xiaomi.mihome/Library/Preferences/group.com.xiaomi.mihome.plist";
const SOURCE_TIMEOUT: Duration = Duration::from_secs(10);
const KEYCHAIN_ERROR: &str = "无法从系统钥匙串读取登录态";
const PREFS_ERROR: &str = "无法从属性列表读取登录态";
const PAYLOAD_ERROR: &str = "登录凭据内容无效或缺少必要字段";

/// 仅在内存中保留的登录凭据，不提供序列化输出。
#[derive(Clone)]
pub struct Credentials {
    pub user_id: String,
    pub pass_token: String,
    pub device_id: String,
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("登录凭据")
            .field("令牌长度", &self.pass_token.len())
            .finish()
    }
}

fn state(message: &str) -> MiCloudError {
    MiCloudError::State(message.into())
}

impl Credentials {
    /// 解析对象，容忍字节序标记，优先采用驼峰命名字段。
    pub fn from_json(bytes: &[u8]) -> Result<Self, MiCloudError> {
        let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
        let value: Value = serde_json::from_slice(bytes).map_err(|_| state(PAYLOAD_ERROR))?;
        Self::from_value(&value)
    }

    fn from_value(value: &Value) -> Result<Self, MiCloudError> {
        let object = value.as_object().ok_or_else(|| state(PAYLOAD_ERROR))?;
        let field = |camel, snake| object.get(camel).or_else(|| object.get(snake));
        let user_id = match field("userId", "user_id") {
            Some(Value::String(value)) => value.trim().to_owned(),
            Some(Value::Number(value)) => value.to_string(),
            _ => return Err(state(PAYLOAD_ERROR)),
        };
        let pass_token = field("passToken", "pass_token")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| state(PAYLOAD_ERROR))?
            .to_owned();
        if user_id.is_empty() {
            return Err(state(PAYLOAD_ERROR));
        }
        let device_id = match field("deviceId", "device_id") {
            None | Some(Value::Null) => DEFAULT_DEVICE_ID.to_owned(),
            Some(Value::String(value)) if value.trim().is_empty() => DEFAULT_DEVICE_ID.to_owned(),
            Some(Value::String(value)) => value.trim().to_owned(),
            _ => return Err(state(PAYLOAD_ERROR)),
        };
        Ok(Self {
            user_id,
            pass_token,
            device_id,
        })
    }
}

/// 平台作为参数注入，便于在任一主机测试平台分支。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    MacOs,
    Windows,
    Other,
}
impl Platform {
    /// 生产调用使用当前编译目标。
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(windows) {
            Self::Windows
        } else {
            Self::Other
        }
    }
}

/// 子进程调用参数不含凭据正文，输出另行捕获。
pub struct ProcessRequest {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub timeout: Duration,
}

/// 子进程失败类别，不保留系统说明、标准输出或标准错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessError {
    Start,
    Failed,
    Timeout,
}
impl fmt::Display for ProcessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Start => "无法启动读取进程",
            Self::Failed => "读取进程失败",
            Self::Timeout => "读取进程超时",
        })
    }
}
impl std::error::Error for ProcessError {}

/// 文件读取接缝，不隐式读取宿主环境。
pub type ReadFile<'a> = dyn Fn(&Path) -> io::Result<Vec<u8>> + 'a;
/// 子进程接缝，成功时只返回标准输出。
pub type RunProcess<'a> = dyn Fn(&ProcessRequest) -> Result<Vec<u8>, ProcessError> + 'a;

fn env_text(env: &dyn Fn(&str) -> Option<OsString>, name: &str) -> Option<String> {
    env(name)
        .and_then(|s| s.into_string().ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

fn home(env: &dyn Fn(&str) -> Option<OsString>, platform: Platform) -> Option<PathBuf> {
    let variable = if platform == Platform::Windows {
        "USERPROFILE"
    } else {
        "HOME"
    };
    env(variable).filter(|s| !s.is_empty()).map(PathBuf::from)
}

fn expand(
    path: &Path,
    env: &dyn Fn(&str) -> Option<OsString>,
    platform: Platform,
) -> Result<PathBuf, MiCloudError> {
    if let Ok(rest) = path.strip_prefix("~") {
        return Ok(home(env, platform)
            .ok_or_else(|| state("无法确定登录凭据目录"))?
            .join(rest));
    }
    Ok(path.to_owned())
}

/// 按显式文件、环境变量对、环境文件、钥匙串、属性列表顺序读取。
/// 任一已选来源失败都立即返回，不合并字段，也不回落。
pub fn load_credentials(
    explicit: Option<&Path>,
    prefs: Option<&Path>,
    env: &dyn Fn(&str) -> Option<OsString>,
    read_file: &ReadFile<'_>,
    run_process: &RunProcess<'_>,
    platform: Platform,
) -> Result<Credentials, MiCloudError> {
    let file = |path: &Path| {
        let bytes = read_file(path).map_err(|_| state("无法读取登录凭据文件"))?;
        Credentials::from_json(&bytes)
    };
    if let Some(path) = explicit {
        return file(path);
    }
    let user = env_text(env, "AP01_BRIDGE_MI_USER_ID");
    let token = env_text(env, "AP01_BRIDGE_MI_PASS_TOKEN");
    if let (Some(user_id), Some(pass_token)) = (user, token) {
        return Ok(Credentials {
            user_id,
            pass_token,
            device_id: env_text(env, "AP01_BRIDGE_MI_DEVICE_ID")
                .unwrap_or_else(|| DEFAULT_DEVICE_ID.into()),
        });
    }
    if let Some(path) = env_text(env, "AP01_BRIDGE_MI_CREDENTIALS") {
        return file(&expand(Path::new(&path), env, platform)?);
    }
    if let Some(service) = env_text(env, "AP01_BRIDGE_MI_KEYCHAIN_SERVICE") {
        let account =
            env_text(env, "AP01_BRIDGE_MI_KEYCHAIN_ACCOUNT").unwrap_or_else(|| "relay".into());
        return load_keychain(&service, &account, run_process, platform);
    }
    let path = match prefs {
        Some(path) => expand(path, env, platform)?,
        None => home(env, platform)
            .ok_or_else(|| state("没有找到可用的登录凭据"))?
            .join(DEFAULT_PREFS),
    };
    load_prefs(&path, read_file, run_process, platform)
}

fn load_keychain(
    service: &str,
    account: &str,
    run: &RunProcess<'_>,
    platform: Platform,
) -> Result<Credentials, MiCloudError> {
    if platform != Platform::MacOs {
        return Err(state("系统钥匙串来源仅 macOS 可用"));
    }
    let request = ProcessRequest {
        program: "/usr/bin/security".into(),
        args: ["find-generic-password", "-s", service, "-a", account, "-w"]
            .map(OsString::from)
            .to_vec(),
        timeout: SOURCE_TIMEOUT,
    };
    run(&request)
        .map_err(|_| state(KEYCHAIN_ERROR))
        .and_then(|bytes| Credentials::from_json(&bytes).map_err(|_| state(KEYCHAIN_ERROR)))
}

fn load_prefs(
    path: &Path,
    read: &ReadFile<'_>,
    run: &RunProcess<'_>,
    platform: Platform,
) -> Result<Credentials, MiCloudError> {
    if platform != Platform::MacOs {
        return Err(state("属性列表来源仅 macOS 可用"));
    }
    read(path).map_err(|_| state(PREFS_ERROR))?;
    let request = ProcessRequest {
        program: "/usr/bin/plutil".into(),
        args: [
            OsStr::new("-extract"),
            OsStr::new("GroupShareAccountInfo"),
            OsStr::new("json"),
            OsStr::new("-o"),
            OsStr::new("-"),
            path.as_os_str(),
        ]
        .map(OsString::from)
        .to_vec(),
        timeout: SOURCE_TIMEOUT,
    };
    let bytes = run(&request).map_err(|_| state(PREFS_ERROR))?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| state(PREFS_ERROR))?;
    Credentials::from_value(&value).map_err(|_| state(PREFS_ERROR))
}

fn receive_output(
    receiver: &mpsc::Receiver<Result<Vec<u8>, ProcessError>>,
    remaining: Duration,
) -> Result<Vec<u8>, ProcessError> {
    // 进程成功退出后，至少给读取线程 50 毫秒交付输出；后代持有管道时仍有界等待。
    receiver
        .recv_timeout(remaining.max(Duration::from_millis(50)))
        .map_err(|_| ProcessError::Timeout)?
}

/// 通用超时执行器：读取线程持续排空输出，主线程轮询并在超时后杀死及回收进程。
/// 标准错误丢弃，子进程不从终端读取输入；不依赖平台专有进程接口。
pub fn run_process(request: &ProcessRequest) -> Result<Vec<u8>, ProcessError> {
    let started = Instant::now();
    let mut child = Command::new(&request.program)
        .args(&request.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| ProcessError::Start)?;
    let mut stdout = child.stdout.take().ok_or(ProcessError::Failed)?;
    let (sender, receiver) = mpsc::channel();
    let reader = thread::Builder::new()
        .name("凭据输出读取".into())
        .spawn(move || {
            let mut bytes = Vec::new();
            let result = stdout
                .read_to_end(&mut bytes)
                .map(|_| bytes)
                .map_err(|_| ProcessError::Failed);
            let _ = sender.send(result);
        });
    if reader.is_err() {
        let _ = child.kill();
        let _ = child.wait();
        return Err(ProcessError::Failed);
    }
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return Err(ProcessError::Failed);
                }
                return receive_output(
                    &receiver,
                    request.timeout.saturating_sub(started.elapsed()),
                );
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ProcessError::Failed);
            }
            Ok(None) => {}
        }
        let remaining = request.timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(ProcessError::Timeout);
        }
        thread::sleep(remaining.min(Duration::from_millis(5)));
    }
}

#[cfg(test)]
#[path = "credentials_tests.rs"]
mod tests;
