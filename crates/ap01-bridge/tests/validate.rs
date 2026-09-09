//! 校验命令的真实文件、标准输入、报告与退出码契约。

mod common;

use ap01_bridge_kernel::gif;
use ap01_gif::testkit::{Frame, GifBuilder, quota_gif};
use common::{TempDir, bridge};
use serde_json::Value;
use std::fs;
use std::io::Write;
use std::process::{Output, Stdio};

fn report(output: &Output, code: i32) -> Value {
    assert_eq!(output.status.code(), Some(code), "命令结果：{output:?}");
    assert!(output.stderr.is_empty());
    let value: Value =
        serde_json::from_slice(&output.stdout).expect("标准输出必须恰好为一个 JSON 对象");
    assert_eq!(value.as_object().unwrap().len(), 12);
    assert_eq!(String::from_utf8_lossy(&output.stdout).lines().count(), 1);
    value
}

#[test]
fn compliant_file_json_report_is_exact_and_read_only() {
    let temp = TempDir::new();
    let path = temp.0.join("额度 面板.gif");
    let data_dir = temp.0.join("unused");
    let bytes = quota_gif();
    fs::write(&path, &bytes).unwrap();
    let output = bridge()
        .args(["--json", "--data-dir"])
        .arg(&data_dir)
        .arg("validate")
        .arg(&path)
        .output()
        .unwrap();
    let value = report(&output, 0);
    assert_eq!(value, serde_json::to_value(gif::validate(&bytes)).unwrap());
    assert_eq!(value["ok"], true);
    assert_eq!(value["frames"], 6);
    assert_eq!(value["total_duration_ms"], 480000);
    assert_eq!(value["loop_count"], Value::Null);
    assert_eq!(fs::read(path).unwrap(), bytes);
    assert!(!data_dir.exists());
    assert_eq!(fs::read_dir(&temp.0).unwrap().count(), 1);
}

#[test]
fn rejected_stdin_json_is_report_and_exits_three() {
    let bytes = b"GIF87a";
    let mut child = bridge()
        .args(["validate", "-", "--json"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(bytes).unwrap();
    let value = report(&child.wait_with_output().unwrap(), 3);
    assert_eq!(value["ok"], false);
    assert!(!value["errors"].as_array().unwrap().is_empty());
    assert_eq!(value, serde_json::to_value(gif::validate(bytes)).unwrap());
    assert!(value.get("error").is_none());
}

#[test]
fn compliant_stdin_reads_all_bytes_until_eof() {
    let bytes = quota_gif();
    let mut child = bridge()
        .args(["validate", "-", "--json"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    for chunk in bytes.chunks(7) {
        stdin.write_all(chunk).unwrap();
    }
    drop(stdin);
    let value = report(&child.wait_with_output().unwrap(), 0);
    assert_eq!(value, serde_json::to_value(gif::validate(&bytes)).unwrap());
}

#[test]
fn human_summary_contains_size_frames_duration_bytes_and_digest() {
    let temp = TempDir::new();
    let path = temp.0.join("quota.gif");
    let bytes = quota_gif();
    fs::write(&path, &bytes).unwrap();
    let output = bridge().arg("validate").arg(&path).output().unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    let text = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        text,
        format!(
            "尺寸：320x240\n帧数：6\n总时长：480000 毫秒\n字节数：{}\nSHA-256：{}\n",
            bytes.len(),
            gif::validate(&bytes).sha256
        )
    );
}

#[test]
fn missing_file_is_state_error_without_validation_report() {
    let temp = TempDir::new();
    let missing = temp.0.join("missing.gif");
    for json_mode in [true, false] {
        let mut command = bridge();
        command.arg("validate").arg(&missing);
        if json_mode {
            command.arg("--json");
        }
        let output = command.output().unwrap();
        assert_eq!(output.status.code(), Some(4));
        if json_mode {
            assert!(output.stderr.is_empty());
            let value: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(value.as_object().unwrap().len(), 2);
            assert_eq!(value["ok"], false);
            assert_eq!(value["error"]["code"], 4);
            assert!(
                value["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("无法读取文件")
            );
            assert!(value.get("errors").is_none());
        } else {
            assert!(output.stdout.is_empty());
            assert!(String::from_utf8_lossy(&output.stderr).contains("无法读取文件"));
        }
    }
    assert_eq!(fs::read_dir(&temp.0).unwrap().count(), 0);
}

#[test]
fn human_rejection_keeps_summary_on_stdout_and_each_error_on_stderr() {
    let temp = TempDir::new();
    let path = temp.0.join("rejected.gif");
    let bytes = GifBuilder {
        trailing_bytes: vec![0x0a],
        ..GifBuilder::default().frame(Frame::default())
    }
    .build();
    fs::write(&path, &bytes).unwrap();
    let output = bridge().arg("validate").arg(&path).output().unwrap();
    assert_eq!(output.status.code(), Some(3));
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    let expected = gif::validate(&bytes);
    assert_eq!(
        stdout,
        format!(
            "尺寸：320x240\n帧数：1\n总时长：0 毫秒\n字节数：{}\nSHA-256：{}\n",
            bytes.len(),
            expected.sha256
        )
    );
    assert_eq!(
        stderr,
        expected
            .errors
            .iter()
            .map(|e| format!("拒绝：{}\n", e.message))
            .collect::<String>()
    );
    assert!(!stdout.contains("拒绝："));
    assert_eq!(fs::read(path).unwrap(), bytes);
    assert_eq!(fs::read_dir(&temp.0).unwrap().count(), 1);
}

#[test]
fn serialized_report_keeps_declaration_order() {
    let temp = TempDir::new();
    let path = temp.0.join("input.gif");
    let bytes = quota_gif();
    fs::write(&path, &bytes).unwrap();
    let output = bridge()
        .arg("validate")
        .arg(&path)
        .arg("--json")
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!(
            "{}\n",
            serde_json::to_string(&gif::validate(&bytes)).unwrap()
        )
    );
}

#[test]
fn oversized_file_and_stdin_get_error_object_instead_of_report() {
    let temp = TempDir::new();
    let path = temp.0.join("input.gif");
    // 结尾保留 0x3B，证明拒绝理由只有体积，而不是前缀最后一字节的误判。
    let mut bytes = quota_gif();
    bytes.resize(299_999, 0);
    bytes.push(0x3b);
    fs::write(&path, &bytes).unwrap();
    let output = bridge()
        .arg("validate")
        .arg(&path)
        .arg("--json")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    assert!(output.stderr.is_empty());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        value,
        serde_json::json!({"ok": false, "error": {"code": 3, "message": "体积大于 262144 字节：文件实际 300000 字节"}})
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).lines().count(), 1);
    // 不带 --json 时没有摘要，只有一行中文错误。
    let output = bridge().arg("validate").arg(&path).output().unwrap();
    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "体积大于 262144 字节：文件实际 300000 字节\n"
    );
    assert_eq!(fs::metadata(&path).unwrap().len(), 300_000);
    // 标准输入超限：消息不含大小，读取在超过上限后即停。
    let mut child = bridge()
        .args(["validate", "-", "--json"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // 子进程读满上限后会退出，管道关闭导致的写入失败是预期的。
    let _ = child.stdin.take().unwrap().write_all(&bytes);
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(3));
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        value,
        serde_json::json!({"ok": false, "error": {"code": 3, "message": "体积大于 262144 字节：输入超过上限"}})
    );
}
