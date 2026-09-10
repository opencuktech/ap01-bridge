//! 通过真实子进程验证命令行契约，环境和临时目录由测试控制。

use std::fs;
use std::path::Path;
use std::process::Output;
mod common;
use common::{TempDir, bridge};

use serde_json::{Value, json};

fn assert_success(output: &Output) {
    assert_eq!(output.status.code(), Some(0), "命令失败：{output:?}");
    assert!(output.stderr.is_empty(), "标准错误不应包含内容：{output:?}");
}

fn assert_report(output: &Output, path: &Path, exists: bool, writable: bool) {
    assert_success(output);
    let value: Value =
        serde_json::from_slice(&output.stdout).expect("输出必须恰好是一个 JSON 对象");
    assert_eq!(
        value,
        json!({
            "ok": true,
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "version": "0.1.0",
            "data_dir": path.to_string_lossy(),
            "data_dir_exists": exists,
            "data_dir_writable": writable,
        })
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).lines().count(), 1);
}

#[test]
fn version_is_exactly_one_line() {
    let output = bridge().arg("--version").output().expect("无法运行命令");
    assert_success(&output);
    assert_eq!(output.stdout, b"bridge 0.1.0\n");
}

#[test]
fn help_and_version_keep_clap_success_behavior() {
    for args in [
        vec!["--help"],
        vec!["--json", "--help"],
        vec!["doctor", "--help"],
        vec!["--json", "--version"],
    ] {
        let output = bridge().args(args).output().expect("无法运行命令");
        assert_success(&output);
        assert!(!output.stdout.is_empty());
    }
}

#[test]
fn doctor_human_output_has_four_items_and_cleans_up() {
    let temp = TempDir::new();
    let output = bridge()
        .args(["doctor", "--data-dir"])
        .arg(&temp.0)
        .output()
        .expect("无法运行命令");
    assert_success(&output);
    let text = String::from_utf8(output.stdout).expect("输出必须为中文文本");
    assert_eq!(
        text,
        format!(
            "操作系统：{}\n架构：{}\n版本：0.1.0\n数据目录：{}（存在：是；可写：是）\n",
            std::env::consts::OS,
            std::env::consts::ARCH,
            temp.0.display()
        )
    );
    assert_eq!(fs::read_dir(&temp.0).unwrap().count(), 0);
}

#[test]
fn doctor_json_has_exact_fields_and_cleans_up() {
    let temp = TempDir::new();
    let path = temp.0.join("中文 空格目录");
    fs::create_dir(&path).unwrap();
    let output = bridge()
        .args(["doctor", "--json", "--data-dir"])
        .arg(&path)
        .output()
        .expect("无法运行命令");
    assert_report(&output, &path, true, true);
    assert_eq!(fs::read_dir(&path).unwrap().count(), 0);
}

#[test]
fn doctor_does_not_create_missing_directories() {
    let temp = TempDir::new();
    let path = temp.0.join("missing").join("nested");
    let output = bridge()
        .args(["doctor", "--json", "--data-dir"])
        .arg(&path)
        .output()
        .expect("无法运行命令");
    assert_report(&output, &path, false, false);
    assert!(!path.exists());
    assert_eq!(fs::read_dir(&temp.0).unwrap().count(), 0);
}

#[test]
fn explicit_directory_overrides_environment_and_flags_are_global() {
    let temp = TempDir::new();
    let ignored = temp.0.join("ignored");
    let output = bridge()
        .args(["--json", "--data-dir"])
        .arg(&temp.0)
        .arg("doctor")
        .env("AP01_BRIDGE_DATA_DIR", &ignored)
        .output()
        .expect("无法运行命令");
    assert_report(&output, &temp.0, true, true);
    assert!(!ignored.exists());
}

#[test]
fn environment_overrides_platform_defaults() {
    let temp = TempDir::new();
    let defaults = temp.0.join("defaults");
    let output = bridge()
        .args(["doctor", "--json"])
        .env("AP01_BRIDGE_DATA_DIR", &temp.0)
        .env("HOME", &defaults)
        .env("XDG_DATA_HOME", &defaults)
        .env("LOCALAPPDATA", &defaults)
        .output()
        .expect("无法运行命令");
    assert_report(&output, &temp.0, true, true);
    assert!(!defaults.exists());
}

#[test]
fn platform_defaults_are_resolved_from_controlled_environment() {
    let temp = TempDir::new();
    for use_xdg in [false, true] {
        let home = temp.0.join("home");
        let xdg = temp.0.join("xdg");
        let local = temp.0.join("local");
        let mut command = bridge();
        command
            .args(["doctor", "--json"])
            .env("HOME", &home)
            .env("LOCALAPPDATA", &local);
        if use_xdg {
            command.env("XDG_DATA_HOME", &xdg);
        }
        let base = if cfg!(target_os = "macos") {
            home.join("Library").join("Application Support")
        } else if cfg!(windows) {
            local
        } else if use_xdg {
            xdg
        } else {
            home.join(".local").join("share")
        };
        let output = command.output().expect("无法运行命令");
        assert_report(&output, &base.join("cuktech-ap01-bridge"), false, false);
    }
    assert_eq!(fs::read_dir(&temp.0).unwrap().count(), 0);
}

#[test]
fn missing_environment_is_reported_without_failing_doctor() {
    let output = bridge()
        .args(["doctor", "--json"])
        .output()
        .expect("无法运行命令");
    assert_report(&output, Path::new(""), false, false);
    let output = bridge().arg("doctor").output().expect("无法运行命令");
    assert_success(&output);
    assert!(String::from_utf8_lossy(&output.stdout).contains("无法确定数据目录"));
}

#[test]
fn file_instead_of_directory_is_not_writable_and_is_preserved() {
    let temp = TempDir::new();
    let path = temp.0.join("file");
    fs::write(&path, b"original").unwrap();
    let output = bridge()
        .args(["doctor", "--json", "--data-dir"])
        .arg(&path)
        .output()
        .expect("无法运行命令");
    assert_report(&output, &path, false, false);
    assert_eq!(fs::read(path).unwrap(), b"original");
}

#[test]
fn probe_collisions_do_not_overwrite_existing_files() {
    let temp = TempDir::new();
    // 使用子进程自身之外的文件名验证检查不会清理其它内容。
    let existing = temp.0.join(".ap01-doctor-existing.tmp");
    fs::write(&existing, b"original").unwrap();
    let output = bridge()
        .args(["doctor", "--json", "--data-dir"])
        .arg(&temp.0)
        .output()
        .expect("无法运行命令");
    assert_report(&output, &temp.0, true, true);
    assert_eq!(fs::read(existing).unwrap(), b"original");
    assert_eq!(fs::read_dir(&temp.0).unwrap().count(), 1);
}

#[test]
fn usage_errors_exit_two_and_diagnostics_go_to_stderr() {
    for args in [
        vec!["unknown"],
        vec![],
        vec!["doctor", "--data-dir"],
        vec!["validate"],
        vec!["publish"],
        vec!["serve", "--port"],
    ] {
        let output = bridge().args(args).output().expect("无法运行命令");
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("命令用法不正确"));
    }
}

#[test]
fn json_usage_errors_are_single_objects_even_after_unknown_command() {
    for args in [
        vec!["--json", "unknown"],
        vec!["unknown", "--json"],
        vec!["--json"],
        vec!["doctor", "--json", "--data-dir"],
    ] {
        let output = bridge().args(args).output().expect("无法运行命令");
        assert_eq!(output.status.code(), Some(2));
        let value: Value =
            serde_json::from_slice(&output.stdout).expect("输出必须恰好是一个 JSON 对象");
        assert_eq!(
            value,
            json!({"ok": false, "error": {"code": 2, "message": "命令用法不正确，请使用 --help 查看帮助"}})
        );
        assert!(!output.stderr.is_empty());
        assert_eq!(String::from_utf8_lossy(&output.stdout).lines().count(), 1);
    }
}

const MI_CREDENTIAL_ENV: [&str; 6] = [
    "AP01_BRIDGE_MI_USER_ID",
    "AP01_BRIDGE_MI_PASS_TOKEN",
    "AP01_BRIDGE_MI_DEVICE_ID",
    "AP01_BRIDGE_MI_CREDENTIALS",
    "AP01_BRIDGE_MI_KEYCHAIN_SERVICE",
    "AP01_BRIDGE_MI_KEYCHAIN_ACCOUNT",
];

fn mi_bridge() -> std::process::Command {
    let mut command = bridge();
    remove_mi_credentials(&mut command);
    command
}

fn remove_mi_credentials(command: &mut std::process::Command) {
    for name in MI_CREDENTIAL_ENV {
        command.env_remove(name);
    }
}

#[test]
fn mi_commands_explicitly_remove_host_credentials() {
    let mut command = bridge();
    for name in MI_CREDENTIAL_ENV {
        command.env(name, "synthetic-value");
    }
    remove_mi_credentials(&mut command);
    for name in MI_CREDENTIAL_ENV {
        assert!(
            command
                .get_envs()
                .all(|(key, value)| key != name || value.is_none())
        );
    }
}

fn assert_mi_state_error(output: &Output) -> Value {
    assert_eq!(output.status.code(), Some(4));
    assert!(output.stderr.is_empty());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["error", "ok"]
    );
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], 4);
    assert!(value["error"]["message"].is_string());
    value
}

#[test]
fn mi_without_credentials_is_state_error_and_does_not_touch_data_dir() {
    let data = TempDir::new();
    let missing = data.0.join("missing-prefs");
    let output = mi_bridge()
        .args(["mi", "ap01", "--json", "--prefs"])
        .arg(&missing)
        .arg("--data-dir")
        .arg(&data.0)
        .output()
        .unwrap();
    assert_mi_state_error(&output);
    assert_eq!(fs::read_dir(&data.0).unwrap().count(), 0);
    let absent_data = data.0.join("not-created");
    let output = mi_bridge()
        .args(["mi", "ap01", "--json", "--prefs"])
        .arg(&missing)
        .arg("--data-dir")
        .arg(&absent_data)
        .output()
        .unwrap();
    assert_mi_state_error(&output);
    assert!(!absent_data.exists());
}

#[test]
fn mi_credentials_missing_fields_is_state_error() {
    let temp = TempDir::new();
    let file = temp.0.join("synthetic-credentials.json");
    fs::write(&file, br#"{"user_id":"synthetic-user"}"#).unwrap();
    let output = mi_bridge()
        .args(["mi", "ap01", "--json", "--credentials"])
        .arg(&file)
        .arg("--prefs")
        .arg(temp.0.join("missing-prefs"))
        .output()
        .unwrap();
    assert_mi_state_error(&output);
}

#[test]
fn mi_missing_subcommand_or_option_value_is_usage_error() {
    let temp = TempDir::new();
    for args in [
        vec!["mi"],
        vec!["mi", "unknown"],
        vec![
            "mi",
            "ap01",
            "--prefs",
            temp.0.to_str().unwrap(),
            "--credentials",
        ],
    ] {
        let output = mi_bridge().args(args).output().unwrap();
        assert_eq!(output.status.code(), Some(2));
    }
}

#[test]
fn mi_platform_sources_have_identical_error_structure() {
    let temp = TempDir::new();
    for keychain in [false, true] {
        let mut command = mi_bridge();
        command
            .args(["mi", "ap01", "--json", "--prefs"])
            .arg(temp.0.join("missing-prefs"));
        if keychain {
            command.env(
                "AP01_BRIDGE_MI_KEYCHAIN_SERVICE",
                format!(
                    "ap01-synthetic-missing-{}",
                    temp.0.file_name().unwrap().to_string_lossy()
                ),
            );
        }
        let output = command.output().unwrap();
        let value = assert_mi_state_error(&output);
        #[cfg(not(target_os = "macos"))]
        assert!(
            value["error"]["message"]
                .as_str()
                .unwrap()
                .contains("仅 macOS 可用")
        );
        #[cfg(target_os = "macos")]
        assert!(
            value["error"]["message"]
                .as_str()
                .unwrap()
                .contains("无法从")
        );
    }
}

#[test]
fn mi_help_omits_debug_environment_seams() {
    for args in [vec!["--help"], vec!["mi", "ap01", "--help"]] {
        let output = mi_bridge().args(args).output().unwrap();
        assert_success(&output);
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(!text.contains("AP01_BRIDGE_MI_ACCOUNT_URL"));
        assert!(!text.contains("AP01_BRIDGE_MI_API_URL"));
    }
}

#[cfg(debug_assertions)]
#[test]
fn mi_invalid_endpoint_override_fails_before_network() {
    let temp = TempDir::new();
    let output = mi_bridge()
        .args(["mi", "ap01", "--json", "--prefs"])
        .arg(temp.0.join("missing-prefs"))
        .env("AP01_BRIDGE_MI_USER_ID", "synthetic-user")
        .env("AP01_BRIDGE_MI_PASS_TOKEN", "synthetic-pass")
        .env("AP01_BRIDGE_MI_ACCOUNT_URL", "http://example.invalid")
        .output()
        .unwrap();
    assert_mi_state_error(&output);
}

#[test]
fn mi_usage_errors_do_not_echo_raw_arguments() {
    let temp = TempDir::new();
    for json_mode in [false, true] {
        let mut command = mi_bridge();
        command
            .args(["mi", "ap01", "--prefs"])
            .arg(temp.0.join("missing-prefs"))
            .arg("synthetic-passToken");
        if json_mode {
            command.arg("--json");
        }
        let output = command.output().unwrap();
        assert_eq!(output.status.code(), Some(2));
        for bytes in [&output.stdout, &output.stderr] {
            assert!(!String::from_utf8_lossy(bytes).contains("synthetic-passToken"));
        }
    }
}

#[cfg(debug_assertions)]
#[test]
fn mi_first_connection_failure_is_runtime_error() {
    // 先取得独占回环端口再关闭监听，整个用例只连接本机。
    let listener =
        std::net::TcpListener::bind(("127.0.0.1", 0)).expect("需要可绑定回环端口的本地环境");
    let base = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    for json_mode in [false, true] {
        let temp = TempDir::new();
        let mut command = mi_bridge();
        command
            .args(["mi", "ap01", "--prefs"])
            .arg(temp.0.join("missing-prefs"))
            .arg("--data-dir")
            .arg(&temp.0)
            .env("AP01_BRIDGE_MI_USER_ID", "synthetic-user")
            .env("AP01_BRIDGE_MI_PASS_TOKEN", "synthetic-pass")
            .env("AP01_BRIDGE_MI_ACCOUNT_URL", &base)
            .env("AP01_BRIDGE_MI_API_URL", &base);
        if json_mode {
            command.arg("--json");
        }
        let output = command.output().unwrap();
        assert_eq!(output.status.code(), Some(5));
        if json_mode {
            assert!(output.stderr.is_empty());
            let value: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(value["ok"], false);
            assert_eq!(value["error"]["code"], 5);
        }
        for bytes in [&output.stdout, &output.stderr] {
            let text = String::from_utf8_lossy(bytes);
            assert!(!text.contains("synthetic-"));
            assert!(!text.contains(&base));
        }
        assert_eq!(fs::read_dir(&temp.0).unwrap().count(), 0);
    }
}
