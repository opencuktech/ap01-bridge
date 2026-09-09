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
