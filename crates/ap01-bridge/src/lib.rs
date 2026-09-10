//! 二进制应用的跨库错误适配，不把云端依赖引入内核。

use ap01_bridge_kernel::KernelError;
use ap01_mi_cloud::MiCloudError;

/// 将云端失败阶段转换为命令行统一错误类别。
pub fn mi_cloud_error(error: MiCloudError) -> KernelError {
    match error {
        MiCloudError::State(message) => KernelError::State(message),
        MiCloudError::Runtime(message) => KernelError::Runtime(message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mi_cloud_error_stages_map_to_kernel() {
        for (error, code) in [
            (MiCloudError::State("凭据不可用".into()), 4),
            (MiCloudError::Runtime("连接失败".into()), 5),
        ] {
            assert_eq!(error.exit_code(), code);
            let mapped = mi_cloud_error(error.clone());
            assert_eq!(mapped.exit_code(), code);
            assert_eq!(mapped.message(), error.message());
            assert!(matches!(
                (code, mapped),
                (4, KernelError::State(_)) | (5, KernelError::Runtime(_))
            ));
        }
    }
}
