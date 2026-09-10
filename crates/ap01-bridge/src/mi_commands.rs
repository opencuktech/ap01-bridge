//! 云端只读查询命令；凭据和会话只在本次调用中存在。

use super::Output;
use ap01_bridge::mi_cloud_error;
use ap01_bridge_kernel::KernelError;
use ap01_mi_cloud::{
    client::{Client, SystemSource},
    credentials::{self, Platform},
    device::{self, Ap01Report},
    session::{Endpoints, Session},
    transport::UreqTransport,
};
use clap::Subcommand;
use std::path::PathBuf;

#[derive(Subcommand)]
pub enum MiCommands {
    /// 读取 AP01 的型号、固件、在线状态和运行秒数。
    Ap01 {
        /// 从指定文件读取登录凭据。
        #[arg(long, value_name = "路径")]
        credentials: Option<PathBuf>,
        /// 指定登录态属性列表文件。
        #[arg(long, value_name = "路径")]
        prefs: Option<PathBuf>,
    },
}

fn report_output(report: Ap01Report, json_mode: bool) -> Result<Output, KernelError> {
    let online = report
        .online
        .map(|online| if online { "是" } else { "否" })
        .unwrap_or("未知");
    let human = format!(
        "型号: {}\n固件版本: {}\n在线: {}\n运行秒数: {}",
        report.model,
        report.firmware_version.as_deref().unwrap_or("未知"),
        online,
        report
            .uptime_seconds
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_else(|| "未知".into())
    );
    Ok(Output {
        json_mode,
        value: serde_json::to_string(&report)
            .map_err(|_| KernelError::Runtime("无法生成设备查询结果".into()))?,
        human,
        diagnostics: Vec::new(),
        exit_code: 0,
    })
}

pub fn run(command: MiCommands, json_mode: bool) -> Result<Output, KernelError> {
    let MiCommands::Ap01 {
        credentials: explicit,
        prefs,
    } = command;
    let credentials = credentials::load_credentials(
        explicit.as_deref(),
        prefs.as_deref(),
        &|name| std::env::var_os(name),
        &|path| std::fs::read(path),
        &credentials::run_process,
        Platform::current(),
    )
    .map_err(mi_cloud_error)?;
    #[cfg(debug_assertions)]
    let endpoints = Endpoints::debug_overrides(
        std::env::var("AP01_BRIDGE_MI_ACCOUNT_URL").ok(),
        std::env::var("AP01_BRIDGE_MI_API_URL").ok(),
    )
    .map_err(mi_cloud_error)?;
    #[cfg(not(debug_assertions))]
    let endpoints = Endpoints::default();
    let transport = UreqTransport::new();
    let session = Session::login(&transport, &credentials, &endpoints).map_err(mi_cloud_error)?;
    let client = Client::new(&transport, session, endpoints, &SystemSource);
    report_output(device::ap01(&client).map_err(mi_cloud_error)?, json_mode)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mi_output_exact_keys_and_four_lines() {
        for online in [None, Some(false), Some(true)] {
            let output = report_output(
                Ap01Report {
                    ok: true,
                    model: device::MODEL.into(),
                    firmware_version: None,
                    online,
                    uptime_seconds: None,
                },
                true,
            )
            .unwrap();
            assert_eq!(
                output.human,
                format!(
                    "型号: njcuk.enstor.ap01\n固件版本: 未知\n在线: {}\n运行秒数: 未知",
                    match online {
                        None => "未知",
                        Some(false) => "否",
                        Some(true) => "是",
                    }
                )
            );
            assert_eq!(output.human.lines().count(), 4);
            let value: serde_json::Value = serde_json::from_str(&output.value).unwrap();
            assert_eq!(
                value,
                serde_json::json!({"ok":true,"model":device::MODEL,"firmware_version":null,"online":online,"uptime_seconds":null})
            );
        }
    }
}
