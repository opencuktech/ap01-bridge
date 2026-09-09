//! 内核错误及统一的进程退出码。

use std::fmt;

/// 各类错误携带面向用户的中文说明。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KernelError {
    /// 命令参数不符合用法。
    Usage(String),
    /// 输入未通过校验。
    Rejected(String),
    /// 状态缺失、损坏或读写失败。
    State(String),
    /// 服务运行失败。
    Runtime(String),
    /// 未预期的内部错误。
    Internal(String),
}

impl KernelError {
    /// 返回此类错误约定的进程退出码。
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Usage(_) => 2,
            Self::Rejected(_) => 3,
            Self::State(_) => 4,
            Self::Runtime(_) => 5,
            Self::Internal(_) => 1,
        }
    }

    /// 返回创建错误时提供的中文说明。
    pub fn message(&self) -> &str {
        match self {
            Self::Usage(message)
            | Self::Rejected(message)
            | Self::State(message)
            | Self::Runtime(message)
            | Self::Internal(message) => message,
        }
    }
}

impl fmt::Display for KernelError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message())
    }
}

impl std::error::Error for KernelError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_codes_and_chinese_messages() {
        let cases = [
            (KernelError::Usage("参数不正确".into()), 2),
            (KernelError::Rejected("输入被拒绝".into()), 3),
            (KernelError::State("状态不可用".into()), 4),
            (KernelError::Runtime("运行失败".into()), 5),
            (KernelError::Internal("内部错误".into()), 1),
        ];
        for (error, code) in cases {
            assert_eq!(error.exit_code(), code);
            assert!(!error.message().is_ascii());
            assert_eq!(error.to_string(), error.message());
        }
    }
}
