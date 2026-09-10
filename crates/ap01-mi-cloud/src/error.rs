//! 按首次网络请求是否已经发出区分失败阶段。

use std::fmt;

/// 只携带面向用户的中文说明，不保留底层错误或原始输入。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MiCloudError {
    /// 首次网络请求之前失败，对应状态错误。
    State(String),
    /// 已尝试网络请求后失败，对应运行错误。
    Runtime(String),
}

impl MiCloudError {
    /// 取得可输出的中文说明。
    pub fn message(&self) -> &str {
        match self {
            Self::State(message) | Self::Runtime(message) => message,
        }
    }

    /// 返回阶段对应的退出码。
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::State(_) => 4,
            Self::Runtime(_) => 5,
        }
    }
}

impl fmt::Display for MiCloudError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for MiCloudError {}
