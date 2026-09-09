//! 存储命令的环境时钟注入与人类可读输出。

use super::{Commands, Output, read_input};
use ap01_bridge_kernel::{
    KernelError,
    store::{self, FallbackEntry, PublishOptions, Serving, ServingStatus},
    time::format_rfc3339_utc,
};
use clap::Subcommand;
use serde::Serialize;
use std::{
    io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Subcommand)]
pub enum FallbackCommands {
    /// 设置或覆盖回退槽，使用 - 读取标准输入。
    Set { name: String, file: PathBuf },
    /// 按槽名列出回退槽。
    List,
    /// 删除未被当前发布引用的槽。
    Rm { name: String },
}

#[derive(Serialize)]
struct FallbackListResult {
    ok: bool,
    fallbacks: Vec<FallbackEntry>,
}
#[derive(Serialize)]
struct FallbackRemoveResult {
    ok: bool,
    name: String,
}
#[derive(Serialize)]
struct StatusResult {
    ok: bool,
    #[serde(flatten)]
    status: ServingStatus,
}

pub(super) fn now() -> Result<u64, KernelError> {
    // 仅在调试构建中供离线测试注入时钟；release 完全不读取此变量，无效值沿用系统时钟。
    #[cfg(debug_assertions)]
    if let Some(value) = std::env::var("AP01_BRIDGE_FAKE_NOW")
        .ok()
        .and_then(|value| value.parse().ok())
    {
        return Ok(value);
    }
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| KernelError::State("系统时间早于 Unix 起点".into()))
}

fn output(
    json_mode: bool,
    report: &impl Serialize,
    human: String,
    diagnostics: Vec<String>,
) -> Result<Output, KernelError> {
    Ok(Output {
        json_mode,
        value: serde_json::to_string(report)
            .map_err(|_| KernelError::Internal("无法序列化命令结果".into()))?,
        human,
        diagnostics,
        exit_code: 0,
    })
}

fn yes_no(value: bool) -> &'static str {
    if value { "是" } else { "否" }
}
fn optional<T: ToString>(value: Option<T>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "无".into())
}

pub(super) fn run(
    command: Commands,
    data_dir: &Path,
    json_mode: bool,
) -> Result<Output, KernelError> {
    match command {
        Commands::Publish {
            file,
            ttl,
            fallback,
        } => {
            let bytes = read_input(&file, &mut io::stdin().lock())?;
            let result = store::publish(
                data_dir,
                &bytes,
                now()?,
                PublishOptions {
                    ttl_seconds: ttl,
                    fallback,
                },
            )?;
            let human = format!(
                "gif：{}\nbytes：{}\npublished_at：{}\nttl：{}\nfallback：{}\nstored：{}",
                result.gif,
                result.bytes,
                format_rfc3339_utc(result.published_at),
                optional(result.ttl_seconds),
                optional(result.fallback.as_deref()),
                yes_no(result.stored)
            );
            output(json_mode, &result, human, result.warnings.clone())
        }
        Commands::Fallback { command } => match command {
            FallbackCommands::Set { name, file } => {
                store::validate_name(&name)?;
                let bytes = read_input(&file, &mut io::stdin().lock())?;
                let result = store::set_fallback(data_dir, &name, &bytes, now()?)?;
                let human = format!(
                    "name：{}\ngif：{}\nbytes：{}\nset_at：{}\nstored：{}",
                    result.name,
                    result.gif,
                    result.bytes,
                    format_rfc3339_utc(result.set_at),
                    yes_no(result.stored)
                );
                output(json_mode, &result, human, Vec::new())
            }
            FallbackCommands::List => {
                let result = FallbackListResult {
                    ok: true,
                    fallbacks: store::list_fallbacks(data_dir)?,
                };
                let human = if result.fallbacks.is_empty() {
                    "暂无回退槽".into()
                } else {
                    result
                        .fallbacks
                        .iter()
                        .map(|entry| {
                            format!(
                                "{} {} {} 字节 {}",
                                entry.name,
                                entry.gif,
                                entry.bytes,
                                format_rfc3339_utc(entry.set_at)
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                output(json_mode, &result, human, Vec::new())
            }
            FallbackCommands::Rm { name } => {
                store::remove_fallback(data_dir, &name)?;
                let human = format!("已删除回退槽：{name}");
                output(
                    json_mode,
                    &FallbackRemoveResult { ok: true, name },
                    human,
                    Vec::new(),
                )
            }
        },
        Commands::Status => {
            let status = store::resolve_status(data_dir, now()?);
            let mut lines = vec![format!(
                "serving：{}",
                match status.serving {
                    Serving::Current => "current",
                    Serving::Fallback => "fallback",
                    Serving::None => "none",
                }
            )];
            if let Some(current) = &status.current {
                lines.extend([
                    format!("current.gif：{}", current.gif),
                    format!("current.bytes：{}", current.bytes),
                    format!(
                        "current.published_at：{}",
                        format_rfc3339_utc(current.published_at)
                    ),
                    format!("current.ttl_seconds：{}", optional(current.ttl_seconds)),
                    format!(
                        "current.expires_at：{}",
                        optional(current.expires_at.map(format_rfc3339_utc))
                    ),
                    format!("current.expired：{}", yes_no(current.expired)),
                ]);
            } else {
                lines.push("current：无".into());
            }
            if let Some(fallback) = &status.fallback {
                lines.extend([
                    format!("fallback.name：{}", fallback.name),
                    format!("fallback.gif：{}", optional(fallback.gif.as_deref())),
                    format!("fallback.bytes：{}", optional(fallback.bytes)),
                    format!("fallback.missing：{}", yes_no(fallback.missing)),
                ]);
            } else {
                lines.push("fallback：无".into());
            }
            lines.push(format!("error：{}", optional(status.error.as_deref())));
            output(
                json_mode,
                &StatusResult { ok: true, status },
                lines.join("\n"),
                Vec::new(),
            )
        }
        _ => Err(KernelError::Internal("存储命令分发不正确".into())),
    }
}
