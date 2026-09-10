//! AP01 桥接命令行入口，负责环境注入和输出分流。

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering};

use ap01_bridge_kernel::KernelError;
use ap01_bridge_kernel::gif;
use ap01_bridge_kernel::paths::{Platform, resolve_data_dir};
use clap::{Parser, Subcommand, error::ErrorKind};
use serde::Serialize;
use serde_json::json;

mod mi_commands;
use mi_commands::MiCommands;
mod serve_commands;
mod store_commands;
use store_commands::FallbackCommands;

#[derive(Parser)]
#[command(name = "bridge", version = env!("CARGO_PKG_VERSION"), about = "AP01 桥接工具")]
struct Cli {
    /// 输出 JSON 结果；服务运行时逐行输出事件。
    #[arg(long, global = true)]
    json: bool,
    /// 指定数据目录。
    #[arg(long, global = true, value_name = "路径")]
    data_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// 查询云端设备信息。
    Mi {
        #[command(subcommand)]
        command: MiCommands,
    },
    /// 检查系统、版本和数据目录。
    Doctor,
    /// 只读校验 GIF 文件，使用 - 从标准输入读取到结束。
    Validate {
        #[arg(value_name = "文件|-")]
        file: PathBuf,
    },
    /// 发布已校验的 GIF 内容。
    Publish {
        #[arg(value_name = "文件|-")]
        file: PathBuf,
        /// 有效期秒数，至少为 1。
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
        ttl: Option<u64>,
        /// 已存在的回退槽名。
        #[arg(long)]
        fallback: Option<String>,
    },
    /// 设置、列出或删除回退槽。
    Fallback {
        #[command(subcommand)]
        command: FallbackCommands,
    },
    /// 查询当前供应状态。
    Status,
    /// 启动面向设备的 HTTP 服务。
    Serve {
        /// 监听 IP 地址，接受 IPv4 与 IPv6 字面量。
        #[arg(long, default_value = "0.0.0.0", value_name = "地址")]
        bind: IpAddr,
        /// 监听端口，0 表示由系统分配。
        #[arg(long, default_value_t = 8765, value_name = "端口")]
        port: u16,
    },
}

#[derive(Serialize)]
struct DoctorReport {
    ok: bool,
    os: &'static str,
    arch: &'static str,
    version: &'static str,
    data_dir: String,
    data_dir_exists: bool,
    data_dir_writable: bool,
}

struct Output {
    json_mode: bool,
    value: String,
    human: String,
    diagnostics: Vec<String>,
    exit_code: u8,
}

fn main() -> ExitCode {
    let json_requested = std::env::args_os().skip(1).any(|arg| arg == "--json");
    let result = run();
    match write_output(
        result,
        json_requested,
        &mut io::stdout().lock(),
        &mut io::stderr().lock(),
    ) {
        Ok(code) => ExitCode::from(code),
        Err(_) => {
            let _ = writeln!(io::stderr(), "无法写入命令输出");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<Output, KernelError> {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) {
                error.exit();
            }
            // 云端用法错误不回显原始参数，避免路径或凭据进入诊断。
            if !std::env::args_os().skip(1).any(|arg| arg == "mi") {
                let _ = error.print();
            }
            return Err(KernelError::Usage(
                "命令用法不正确，请使用 --help 查看帮助".into(),
            ));
        }
    };
    match cli.command {
        Commands::Mi { command } => mi_commands::run(command, cli.json),
        Commands::Doctor => {
            let resolved = resolve_data_dir(
                cli.data_dir.as_deref(),
                &|name| std::env::var_os(name),
                Platform::current(),
            );
            let (data_dir, exists, writable, description) = match resolved {
                Ok(path) => {
                    let exists = path.is_dir();
                    let writable = exists && directory_is_writable(&path);
                    let display = path.to_string_lossy().into_owned();
                    (display.clone(), exists, writable, display)
                }
                // doctor 报告检查结论，缺少环境变量也不使检查命令失败。
                Err(error) => (String::new(), false, false, error.to_string()),
            };
            let report = DoctorReport {
                ok: true,
                os: std::env::consts::OS,
                arch: std::env::consts::ARCH,
                version: env!("CARGO_PKG_VERSION"),
                data_dir,
                data_dir_exists: exists,
                data_dir_writable: writable,
            };
            let yes_no = |value| if value { "是" } else { "否" };
            let human = format!(
                "操作系统：{}\n架构：{}\n版本：{}\n数据目录：{}（存在：{}；可写：{}）",
                report.os,
                report.arch,
                report.version,
                description,
                yes_no(exists),
                yes_no(writable)
            );
            let value = serde_json::to_string(&report)
                .map_err(|_| KernelError::Internal("无法序列化检查结果".into()))?;
            Ok(Output {
                json_mode: cli.json,
                value,
                human,
                diagnostics: Vec::new(),
                exit_code: 0,
            })
        }
        Commands::Validate { file } => {
            let bytes = read_input(&file, &mut io::stdin().lock())?;
            let report = gif::validate(&bytes);
            let human = format!(
                "尺寸：{}x{}\n帧数：{}\n总时长：{} 毫秒\n字节数：{}\nSHA-256：{}",
                report.width,
                report.height,
                report.frames,
                report.total_duration_ms,
                report.bytes,
                report.sha256
            );
            let diagnostics = report
                .errors
                .iter()
                .map(|error| format!("拒绝：{}", error.message))
                .collect();
            let exit_code = if report.ok { 0 } else { 3 };
            let value = serde_json::to_string(&report)
                .map_err(|_| KernelError::Internal("无法序列化校验报告".into()))?;
            Ok(Output {
                json_mode: cli.json,
                value,
                human,
                diagnostics,
                exit_code,
            })
        }
        command => {
            let data_dir = resolve_data_dir(
                cli.data_dir.as_deref(),
                &|name| std::env::var_os(name),
                Platform::current(),
            )?;
            if let Commands::Serve { bind, port } = command {
                // 长任务先输出启动事件，再阻塞服务；只有失败才回到一次性错误出口。
                return Err(serve_commands::run(bind, port, data_dir, cli.json));
            }
            store_commands::run(command, &data_dir, cli.json)
        }
    }
}

fn read_input(path: &Path, stdin: &mut dyn Read) -> Result<Vec<u8>, KernelError> {
    // 比设备上限多读一个字节即可判定超限，避免无限输入占用内存或阻塞到结束。
    // 超限时只读到了前缀，字节数、摘要与结尾判断都不可信，因此不生成校验报告，直接拒绝。
    let mut bytes = Vec::new();
    let actual_size = if path == Path::new("-") {
        stdin
            .take(262_145)
            .read_to_end(&mut bytes)
            .map_err(|_| KernelError::State("无法读取标准输入".into()))?;
        None
    } else {
        let file = fs::File::open(path)
            .map_err(|_| KernelError::State(format!("无法读取文件：{}", path.display())))?;
        // 管道与设备的元数据长度不代表输入总量，只有普通文件才把大小写进消息。
        let size = file
            .metadata()
            .ok()
            .filter(|metadata| metadata.is_file())
            .map(|metadata| metadata.len());
        file.take(262_145)
            .read_to_end(&mut bytes)
            .map_err(|_| KernelError::State(format!("无法读取文件：{}", path.display())))?;
        size
    };
    if bytes.len() > 262_144 {
        let detail = match actual_size {
            Some(size) => format!("文件实际 {size} 字节"),
            None => "输入超过上限".into(),
        };
        return Err(KernelError::Rejected(format!(
            "体积大于 262144 字节：{detail}"
        )));
    }
    Ok(bytes)
}

fn directory_is_writable(path: &Path) -> bool {
    static NEXT_PROBE: AtomicU64 = AtomicU64::new(0);
    // 独占创建避免覆盖既有文件；残留文件重名时尝试下一个序号。
    for _ in 0..128 {
        let sequence = NEXT_PROBE.fetch_add(1, Ordering::Relaxed);
        let probe = path.join(format!(
            ".ap01-doctor-{}-{sequence}.tmp",
            std::process::id()
        ));
        match OpenOptions::new().write(true).create_new(true).open(&probe) {
            Ok(file) => {
                // Windows 删除前必须先关闭句柄。
                drop(file);
                return fs::remove_file(probe).is_ok();
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(_) => return false,
        }
    }
    false
}

fn write_output(
    result: Result<Output, KernelError>,
    json_requested: bool,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> io::Result<u8> {
    match result {
        Ok(output) => {
            if output.json_mode {
                writeln!(stdout, "{}", output.value)?;
            } else {
                writeln!(stdout, "{}", output.human)?;
                for diagnostic in output.diagnostics {
                    writeln!(stderr, "{diagnostic}")?;
                }
            }
            Ok(output.exit_code)
        }
        Err(error) => {
            let code = error.exit_code();
            if json_requested {
                writeln!(
                    stdout,
                    "{}",
                    serde_json::to_string(
                        &json!({"ok": false, "error": {"code": code, "message": error.message()}})
                    )?
                )?;
            } else {
                writeln!(stderr, "{error}")?;
            }
            Ok(code)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn serve_defaults_and_ipv6_literal_and_port_zero() {
        for (args, expected_bind, expected_port) in [
            (vec!["bridge", "serve"], "0.0.0.0", 8765),
            (
                vec!["bridge", "serve", "--bind", "::1", "--port", "0"],
                "::1",
                0,
            ),
        ] {
            let Commands::Serve { bind, port } = Cli::try_parse_from(args).unwrap().command else {
                panic!("应解析为服务命令")
            };
            assert_eq!(bind.to_string(), expected_bind);
            assert_eq!(port, expected_port);
        }
    }

    #[test]
    fn stdin_input_stops_after_one_byte_over_the_limit() {
        struct EndlessReader {
            read: usize,
        }
        impl Read for EndlessReader {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                assert!(
                    self.read + buffer.len() <= 262_145,
                    "不应读取判定上限之外的字节"
                );
                buffer.fill(0);
                self.read += buffer.len();
                Ok(buffer.len())
            }
        }
        let mut reader = EndlessReader { read: 0 };
        let error = read_input(Path::new("-"), &mut reader).unwrap_err();
        assert_eq!(reader.read, 262_145);
        assert!(matches!(error, KernelError::Rejected(_)));
        assert_eq!(error.exit_code(), 3);
        assert_eq!(error.message(), "体积大于 262144 字节：输入超过上限");
    }

    #[cfg(unix)]
    #[test]
    fn non_regular_file_path_does_not_report_metadata_length() {
        // 字符设备的元数据长度为 0，消息不能把它当成实际大小。
        let error = read_input(Path::new("/dev/zero"), &mut io::empty()).unwrap_err();
        assert!(matches!(error, KernelError::Rejected(_)));
        assert_eq!(error.message(), "体积大于 262144 字节：输入超过上限");
    }

    #[test]
    fn stdin_read_failure_uses_unified_state_error_exit() {
        struct FailingReader;
        impl Read for FailingReader {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::other("模拟标准输入读取失败"))
            }
        }
        let error = read_input(Path::new("-"), &mut FailingReader).unwrap_err();
        assert!(matches!(error, KernelError::State(_)));
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        assert_eq!(
            write_output(Err(error), true, &mut stdout, &mut stderr).unwrap(),
            4
        );
        assert!(stderr.is_empty());
        assert_eq!(
            serde_json::from_slice::<Value>(&stdout).unwrap(),
            json!({"ok": false, "error": {"code": 4, "message": "无法读取标准输入"}})
        );
    }

    #[test]
    fn unified_error_exit_routes_all_codes() {
        for error in [
            KernelError::Usage("用法错误".into()),
            KernelError::Rejected("输入被拒绝".into()),
            KernelError::State("状态错误".into()),
            KernelError::Runtime("运行失败".into()),
            KernelError::Internal("内部错误".into()),
        ] {
            for json_mode in [false, true] {
                let mut stdout = Vec::new();
                let mut stderr = Vec::new();
                let code =
                    write_output(Err(error.clone()), json_mode, &mut stdout, &mut stderr).unwrap();
                assert_eq!(code, error.exit_code());
                if json_mode {
                    assert!(stderr.is_empty());
                    assert_eq!(
                        serde_json::from_slice::<Value>(&stdout).unwrap(),
                        json!({"ok": false, "error": {"code": code, "message": error.message()}})
                    );
                } else {
                    assert!(stdout.is_empty());
                    assert_eq!(String::from_utf8(stderr).unwrap(), format!("{error}\n"));
                }
            }
        }
    }
}
