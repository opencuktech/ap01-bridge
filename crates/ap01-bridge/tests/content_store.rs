//! 覆盖 content-store 的每个场景，所有文件与时间均由测试显式控制。

mod common;
use ap01_bridge_kernel::{
    KernelError, gif,
    store::{self, CurrentRecord, FallbackRecord, PublishOptions, Serving},
};
use ap01_gif::testkit::{Frame, GifBuilder, quota_gif};
use common::{TempDir, bridge};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

const T: u64 = 1_788_668_411;
fn after_residual_deadline() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 7200
}
fn a() -> Vec<u8> {
    quota_gif()
}
fn b() -> Vec<u8> {
    GifBuilder::default().frame(Frame::default()).build()
}
fn invalid() -> Vec<u8> {
    GifBuilder {
        trailing_bytes: vec![10],
        ..GifBuilder::default().frame(Frame::default())
    }
    .build()
}
fn command(dir: &Path, now: u64) -> Command {
    let mut command = bridge();
    command
        .arg("--data-dir")
        .arg(dir)
        .env("AP01_BRIDGE_FAKE_NOW", now.to_string());
    command
}
fn invoke(dir: &Path, now: u64, args: &[&str], input: Option<&[u8]>) -> Output {
    let mut cmd = command(dir, now);
    cmd.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    if let Some(bytes) = input {
        // 子进程可能在读取 stdin 之前就因用法错误退出（例如非法槽名），
        // 此时管道已关闭，写入得到 BrokenPipe 是预期行为，不算测试失败。
        match child.stdin.take().unwrap().write_all(bytes) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => {}
            Err(error) => panic!("写入子进程 stdin 失败：{error}"),
        }
    }
    child.wait_with_output().unwrap()
}
fn result(output: Output, code: i32) -> Value {
    assert_eq!(output.status.code(), Some(code), "命令结果：{output:?}");
    assert!(output.stderr.is_empty(), "标准错误：{output:?}");
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(value.is_object());
    assert_eq!(String::from_utf8_lossy(&output.stdout).lines().count(), 1);
    assert_eq!(value["ok"], code == 0);
    if code != 0 {
        assert_eq!(value["error"]["code"], code);
        assert!(!value["error"]["message"].as_str().unwrap().is_ascii());
    }
    value
}
fn publish(dir: &Path, bytes: &[u8], now: u64, extra: &[&str]) -> Value {
    let mut args = vec!["publish", "-", "--json"];
    args.extend(extra);
    result(invoke(dir, now, &args, Some(bytes)), 0)
}
fn set(dir: &Path, name: &str, bytes: &[u8]) -> Value {
    result(
        invoke(
            dir,
            T,
            &["fallback", "set", name, "-", "--json"],
            Some(bytes),
        ),
        0,
    )
}
fn status(dir: &Path, now: u64) -> Value {
    result(invoke(dir, now, &["status", "--json"], None), 0)
}
fn pointer(dir: &Path) -> Value {
    serde_json::from_slice(&fs::read(dir.join("current.json")).unwrap()).unwrap()
}
fn content(dir: &Path, value: &Value) -> std::path::PathBuf {
    store::content_path(dir, value["gif"].as_str().unwrap())
}
fn snapshot(dir: &Path) -> Vec<(std::path::PathBuf, Vec<u8>)> {
    fn walk(path: &Path, result: &mut Vec<(std::path::PathBuf, Vec<u8>)>) {
        if !path.exists() {
            return;
        }
        if path.is_dir() {
            result.push((path.into(), Vec::new()));
            for entry in fs::read_dir(path).unwrap() {
                walk(&entry.unwrap().path(), result);
            }
        } else {
            result.push((path.into(), fs::read(path).unwrap()));
        }
    }
    let mut result = Vec::new();
    walk(dir, &mut result);
    result.sort();
    result
}

#[test]
fn first_file_publish_creates_layout_and_exact_typed_result_in_order() {
    let temp = TempDir::new();
    let dir = temp.0.join("new/nested");
    let input = temp.0.join("输入.gif");
    let bytes = a();
    fs::write(&input, &bytes).unwrap();
    let output = command(&dir, T)
        .arg("publish")
        .arg(&input)
        .args(["--ttl", "420", "--json"])
        .output()
        .unwrap();
    let expected_report = serde_json::to_string(&gif::validate(&bytes)).unwrap();
    let encoded = String::from_utf8(output.stdout.clone()).unwrap();
    let value = result(output, 0);
    let hash = gif::validate(&bytes).sha256;
    assert_eq!(
        encoded,
        format!(
            "{{\"ok\":true,\"gif\":\"{hash}\",\"bytes\":{},\"published_at\":{T},\"ttl_seconds\":420,\"fallback\":null,\"stored\":true,\"warnings\":[],\"report\":{expected_report}}}\n",
            bytes.len()
        )
    );
    assert_eq!(value.as_object().unwrap().len(), 9);
    assert_eq!(fs::read(content(&dir, &value)).unwrap(), bytes);
    assert_eq!(
        pointer(&dir),
        json!({"schema":1,"gif":hash,"bytes":bytes.len(),"published_at":T,"ttl_seconds":420,"fallback":null})
    );
    for name in ["store", "fallbacks", "logs"] {
        assert!(dir.join(name).is_dir());
    }
    assert!(!dir.join("logs/access.log").exists());
}

#[test]
fn writing_respects_explicit_and_environment_directory_priorities() {
    let temp = TempDir::new();
    let explicit = temp.0.join("explicit");
    let env_dir = temp.0.join("env");
    let defaults = temp.0.join("defaults");
    let file = temp.0.join("input.gif");
    fs::write(&file, a()).unwrap();
    let output = command(&explicit, T)
        .arg("publish")
        .arg(&file)
        .arg("--json")
        .env("AP01_BRIDGE_DATA_DIR", &env_dir)
        .output()
        .unwrap();
    result(output, 0);
    assert!(!env_dir.exists());
    let original = snapshot(&explicit);
    let output = bridge()
        .arg("publish")
        .arg(&file)
        .arg("--json")
        .env("AP01_BRIDGE_DATA_DIR", &env_dir)
        .env("HOME", &defaults)
        .env("XDG_DATA_HOME", &defaults)
        .env("LOCALAPPDATA", &defaults)
        .output()
        .unwrap();
    result(output, 0);
    assert!(env_dir.join("current.json").exists());
    assert!(!defaults.exists());
    assert_eq!(snapshot(&explicit), original);
}

#[test]
fn stdin_publish_null_options_and_republish_renew_without_rewriting_content() {
    let temp = TempDir::new();
    let first = publish(&temp.0, &a(), T, &[]);
    let path = content(&temp.0, &first);
    let modified = fs::metadata(&path).unwrap().modified().unwrap();
    let second = publish(&temp.0, &a(), T + 10, &[]);
    assert_eq!(first["gif"], second["gif"]);
    assert_eq!(second["stored"], false);
    assert_eq!(pointer(&temp.0)["published_at"], T + 10);
    assert_eq!(second["ttl_seconds"], Value::Null);
    assert_eq!(second["fallback"], Value::Null);
    assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), modified);
    assert_eq!(fs::read(&path).unwrap(), a());
    assert!(!store::store_content(&temp.0, &a(), first["gif"].as_str().unwrap()).unwrap());
    assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), modified);
    let status = status(&temp.0, u64::MAX);
    assert_eq!(status["current"]["expires_at"], Value::Null);
    assert_eq!(status["current"]["expired"], false);
}

#[test]
fn republish_repairs_content_with_the_wrong_length() {
    let temp = TempDir::new();
    let bytes = a();
    let first = publish(&temp.0, &bytes, T, &[]);
    let path = content(&temp.0, &first);
    fs::write(&path, b"garbage").unwrap();
    let repaired = publish(&temp.0, &bytes, T + 1, &[]);
    assert_eq!(repaired["stored"], true);
    assert_eq!(repaired["gif"], first["gif"]);
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert_eq!(pointer(&temp.0)["published_at"], T + 1);
}

#[test]
fn truncated_current_is_unavailable_to_status_and_serving_entry() {
    let temp = TempDir::new();
    let published = publish(&temp.0, &a(), T, &[]);
    fs::OpenOptions::new()
        .write(true)
        .open(content(&temp.0, &published))
        .unwrap()
        .set_len(1)
        .unwrap();
    let value = status(&temp.0, T);
    assert_eq!(value["serving"], "none");
    assert!(
        value["error"]
            .as_str()
            .unwrap()
            .contains("长度与记录不一致")
    );
    assert_eq!(store::resolve_status(&temp.0, T).serving_gif(), None);
    let fallback = set(&temp.0, "boot", &b());
    // 只更新指针，保留截断文件以验证损坏当前内容能切到完整回退。
    let mut record = pointer(&temp.0);
    record["fallback"] = json!("boot");
    fs::write(
        temp.0.join("current.json"),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
    let resolved = store::resolve_status(&temp.0, T);
    assert_eq!(resolved.serving, Serving::Fallback);
    assert_eq!(resolved.serving_gif(), fallback["gif"].as_str());
    assert!(resolved.error.unwrap().contains("长度与记录不一致"));
}

#[test]
fn wrong_length_fallback_is_missing_without_current_error() {
    let temp = TempDir::new();
    let fallback = set(&temp.0, "boot", &a());
    let current = publish(&temp.0, &b(), T, &["--ttl", "420", "--fallback", "boot"]);
    fs::write(content(&temp.0, &fallback), b"x").unwrap();
    for now in [T, T + 420] {
        let value = status(&temp.0, now);
        assert_eq!(value["serving"], if now == T { "current" } else { "none" });
        assert_eq!(value["error"], Value::Null);
        assert_eq!(
            value["fallback"],
            json!({
                "name":"boot", "gif":fallback["gif"], "bytes":a().len(), "missing":true
            })
        );
        assert_eq!(
            store::resolve_status(&temp.0, now).serving_gif(),
            if now == T {
                current["gif"].as_str()
            } else {
                None
            }
        );
    }
}

#[test]
fn rejected_publish_and_missing_fallback_have_zero_side_effects() {
    for existing in [false, true] {
        let temp = TempDir::new();
        let dir = temp.0.join("data");
        if existing {
            set(&dir, "boot", &a());
            publish(&dir, &a(), T, &["--fallback", "boot"]);
        }
        let before = snapshot(&dir);
        let rejected = result(
            invoke(&dir, T, &["publish", "-", "--json"], Some(&invalid())),
            3,
        );
        let expected = gif::validate(&invalid())
            .errors
            .iter()
            .map(|error| error.message.as_str())
            .collect::<Vec<_>>()
            .join("；");
        assert_eq!(rejected["error"]["message"], expected);
        assert_eq!(snapshot(&dir), before);
        let error = result(
            invoke(
                &dir,
                T,
                &["publish", "-", "--fallback", "missing", "--json"],
                Some(&b()),
            ),
            4,
        );
        assert_eq!(error["error"]["message"], "回退槽 missing 不存在");
        // 槽存在性在取锁后检查；首次失败只允许创建数据目录和常驻空锁文件。
        let before = if existing {
            before
        } else {
            vec![
                (dir.clone(), Vec::new()),
                (dir.join("write.lock"), Vec::new()),
            ]
        };
        assert_eq!(snapshot(&dir), before);
        result(
            invoke(
                &dir,
                T,
                &["publish", "-", "--fallback", "Bad", "--json"],
                Some(&b()),
            ),
            2,
        );
        assert_eq!(snapshot(&dir), before);
        let human = invoke(&dir, T, &["publish", "-"], Some(&invalid()));
        assert_eq!(human.status.code(), Some(3));
        assert!(human.stdout.is_empty());
        assert!(String::from_utf8_lossy(&human.stderr).contains(&expected));
    }
}

#[test]
fn ttl_clap_errors_and_missing_input_or_environment_use_expected_codes() {
    let temp = TempDir::new();
    let dir = temp.0.join("data");
    for ttl in ["0", "-1", "abc", "18446744073709551616"] {
        let output = command(&dir, T)
            .args(["publish", "-", "--ttl", ttl, "--json"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["error"]["code"], 2);
        assert!(!dir.exists());
    }
    for args in [
        vec!["publish", "missing.gif", "--json"],
        vec!["fallback", "set", "boot", "missing.gif", "--json"],
    ] {
        result(invoke(&dir, T, &args, None), 4);
        assert!(!dir.exists());
    }
    for args in [
        vec!["status", "--json"],
        vec!["publish", "-", "--json"],
        vec!["fallback", "list", "--json"],
    ] {
        result(bridge().args(args).output().unwrap(), 4);
    }
    publish(&dir, &a(), T, &["--ttl", "18446744073709551615"]);
}

#[test]
fn fallback_set_list_overwrite_and_pointer_layout() {
    let temp = TempDir::new();
    let dir = temp.0.join("data");
    assert_eq!(
        result(invoke(&dir, T, &["fallback", "list", "--json"], None), 0),
        json!({"ok":true,"fallbacks":[]})
    );
    assert!(!dir.exists());
    let first = set(&dir, "disconnected", &a());
    set(&dir, "boot", &b());
    let record: Value =
        serde_json::from_slice(&fs::read(dir.join("fallbacks/disconnected.json")).unwrap())
            .unwrap();
    assert_eq!(
        record,
        json!({"schema":1,"gif":first["gif"],"bytes":a().len(),"set_at":T})
    );
    assert_eq!(
        first,
        json!({"ok":true,"name":"disconnected","gif":record["gif"],"bytes":a().len(),"set_at":T,"stored":true,"report":serde_json::to_value(gif::validate(&a())).unwrap()})
    );
    assert_eq!(fs::read(content(&dir, &first)).unwrap(), a());
    let list = result(invoke(&dir, T, &["fallback", "list", "--json"], None), 0);
    assert_eq!(list["fallbacks"].as_array().unwrap().len(), 2);
    assert_eq!(list["fallbacks"][0]["name"], "boot");
    assert_eq!(list["fallbacks"][1]["name"], "disconnected");
    for entry in list["fallbacks"].as_array().unwrap() {
        assert_eq!(entry.as_object().unwrap().len(), 4);
        assert!(entry["gif"].is_string());
        assert!(entry["bytes"].is_u64());
        assert!(entry["set_at"].is_u64());
    }
    let published = publish(
        &dir,
        &b(),
        T,
        &["--ttl", "420", "--fallback", "disconnected"],
    );
    assert_eq!(
        pointer(&dir),
        json!({"schema":1,"gif":published["gif"],"bytes":b().len(),"published_at":T,"ttl_seconds":420,"fallback":"disconnected"})
    );
    assert!(content(&dir, &first).exists());
    assert!(content(&dir, &published).exists());
    let replacement = set(&dir, "disconnected", &b());
    assert_eq!(replacement["stored"], false);
    assert!(content(&dir, &first).exists());
    assert_eq!(status(&dir, T + 420)["fallback"]["gif"], replacement["gif"]);
    fs::write(dir.join("fallbacks/broken.json"), b"{").unwrap();
    fs::write(
        dir.join("fallbacks/.ghost.json.tmp-1-2"),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
    assert_eq!(store::list_fallbacks(&dir).unwrap().len(), 2);
}

#[test]
fn fallback_invalid_names_and_rejected_input_do_not_mutate_state() {
    for existing in [false, true] {
        let temp = TempDir::new();
        let dir = temp.0.join("data");
        if existing {
            set(&dir, "boot", &a());
        }
        let original = snapshot(&dir);
        for name in [
            "Disconnected",
            "con",
            "prn",
            "aux",
            "nul",
            "com1",
            "com9",
            "lpt1",
            "lpt9",
            "_boot",
            "-boot",
            "../boot",
            "a/b",
            "中文",
            "",
            &"a".repeat(65),
        ] {
            // 前导连字符通过 -- 明确作为位置参数交给槽名校验。
            result(
                invoke(
                    &dir,
                    T,
                    &["--json", "fallback", "set", "--", name, "-"],
                    Some(&b()),
                ),
                2,
            );
            assert_eq!(snapshot(&dir), original);
            result(
                invoke(&dir, T, &["--json", "fallback", "rm", "--", name], None),
                2,
            );
            assert_eq!(snapshot(&dir), original);
            result(
                invoke(
                    &dir,
                    T,
                    &["publish", "-", "--json", &format!("--fallback={name}")],
                    Some(&b()),
                ),
                2,
            );
            assert_eq!(snapshot(&dir), original);
        }
        result(
            invoke(
                &dir,
                T,
                &["fallback", "set", "boot", "-", "--json"],
                Some(&invalid()),
            ),
            3,
        );
        assert_eq!(snapshot(&dir), original);
        result(
            invoke(
                &dir,
                T,
                &["fallback", "set", "new", "-", "--json"],
                Some(&invalid()),
            ),
            3,
        );
        assert_eq!(snapshot(&dir), original);
    }
    let temp = TempDir::new();
    for name in [
        "0",
        "a-b_c",
        "con1",
        "console",
        "com0",
        "com10",
        "lpt0",
        "lpt10",
        &"a".repeat(64),
    ] {
        set(&temp.0, name, &a());
    }
}

#[test]
fn fallback_removal_refuses_referenced_and_missing_slots_then_removes_only_pointer() {
    let temp = TempDir::new();
    let slot = set(&temp.0, "disconnected", &a());
    publish(&temp.0, &b(), T, &["--fallback", "disconnected"]);
    let before = snapshot(&temp.0);
    let error = result(
        invoke(
            &temp.0,
            T,
            &["fallback", "rm", "disconnected", "--json"],
            None,
        ),
        4,
    );
    assert_eq!(
        error["error"]["message"],
        "当前发布仍引用回退槽 disconnected"
    );
    assert_eq!(snapshot(&temp.0), before);
    result(
        invoke(&temp.0, T, &["fallback", "rm", "missing", "--json"], None),
        4,
    );
    assert_eq!(snapshot(&temp.0), before);
    publish(&temp.0, &b(), T + 1, &[]);
    assert_eq!(
        result(
            invoke(
                &temp.0,
                T,
                &["fallback", "rm", "disconnected", "--json"],
                None
            ),
            0
        ),
        json!({"ok":true,"name":"disconnected"})
    );
    assert!(!temp.0.join("fallbacks/disconnected.json").exists());
    assert!(content(&temp.0, &slot).exists());
    publish(&temp.0, &b(), T + 2, &[]);
    assert!(!content(&temp.0, &slot).exists());
}

#[test]
fn fallback_removal_refuses_corrupt_current_but_allows_absent_current() {
    let temp = TempDir::new();
    set(&temp.0, "boot", &a());
    fs::write(temp.0.join("current.json"), b"{").unwrap();
    let before = snapshot(&temp.0);
    let error = result(
        invoke(&temp.0, T, &["fallback", "rm", "boot", "--json"], None),
        4,
    );
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("记录损坏")
    );
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("无法确认引用关系")
    );
    assert_eq!(snapshot(&temp.0), before);
    fs::remove_file(temp.0.join("current.json")).unwrap();
    result(
        invoke(&temp.0, T, &["fallback", "rm", "boot", "--json"], None),
        0,
    );
    assert!(!temp.0.join("fallbacks/boot.json").exists());
}

#[test]
fn status_valid_current_and_exact_expiration_boundary_with_fallback() {
    let temp = TempDir::new();
    let fallback = set(&temp.0, "boot", &a());
    let current = publish(&temp.0, &b(), T, &["--ttl", "420", "--fallback", "boot"]);
    for now in [T, T + 419, T + 420] {
        let value = status(&temp.0, now);
        assert_eq!(
            value,
            json!({"ok":true,"serving":if now < T+420 {"current"} else {"fallback"},
            "current":{"gif":current["gif"],"bytes":b().len(),"published_at":T,"ttl_seconds":420,"expires_at":T+420,"expired":now>=T+420},
            "fallback":{"name":"boot","gif":fallback["gif"],"bytes":a().len(),"missing":false},"error":null})
        );
        let resolved = store::resolve_status(&temp.0, now);
        assert_eq!(
            resolved.serving_gif(),
            if now < T + 420 {
                current["gif"].as_str()
            } else {
                fallback["gif"].as_str()
            }
        );
    }
    // 当前文件缺失时仍可供应完整的回退内容，同时如实报告当前文件异常。
    fs::remove_file(content(&temp.0, &current)).unwrap();
    let value = status(&temp.0, T);
    assert_eq!(value["serving"], "fallback");
    assert!(value["error"].is_string());
}

#[test]
fn status_expired_without_fallback_or_with_missing_or_broken_pointer() {
    let temp = TempDir::new();
    publish(&temp.0, &b(), T, &["--ttl", "420"]);
    let value = status(&temp.0, T + 420);
    assert_eq!(value["serving"], "none");
    assert_eq!(value["fallback"], Value::Null);
    set(&temp.0, "boot", &a());
    publish(&temp.0, &b(), T, &["--ttl", "420", "--fallback", "boot"]);
    fs::remove_file(temp.0.join("fallbacks/boot.json")).unwrap();
    let value = status(&temp.0, T + 420);
    assert_eq!(value["serving"], "none");
    assert_eq!(
        value["fallback"],
        json!({"name":"boot","gif":null,"bytes":null,"missing":true})
    );
    assert_eq!(value["error"], Value::Null);
    for invalid in [
        json!({}),
        json!({"schema":2,"gif":gif::validate(&a()).sha256,"bytes":a().len(),"set_at":T}),
    ] {
        fs::write(
            temp.0.join("fallbacks/boot.json"),
            serde_json::to_vec(&invalid).unwrap(),
        )
        .unwrap();
        for now in [T, T + 420] {
            let value = status(&temp.0, now);
            assert_eq!(value["serving"], if now == T { "current" } else { "none" });
            assert_eq!(value["fallback"]["gif"], Value::Null);
            assert_eq!(value["fallback"]["missing"], true);
            assert_eq!(value["error"], Value::Null);
        }
    }
}

#[test]
fn status_missing_fallback_content_preserves_pointer_values() {
    let temp = TempDir::new();
    let fallback = set(&temp.0, "boot", &a());
    publish(&temp.0, &b(), T, &["--ttl", "420", "--fallback", "boot"]);
    fs::remove_file(content(&temp.0, &fallback)).unwrap();
    for now in [T, T + 420] {
        let value = status(&temp.0, now);
        assert_eq!(value["serving"], if now == T { "current" } else { "none" });
        assert_eq!(
            value["fallback"],
            json!({"name":"boot","gif":fallback["gif"],"bytes":a().len(),"missing":true})
        );
        assert_eq!(value["error"], Value::Null);
    }
}

#[test]
fn status_missing_corrupt_schema_unknown_or_missing_fields_and_missing_content() {
    let temp = TempDir::new();
    let dir = temp.0.join("data");
    let value = status(&dir, T);
    assert_eq!(
        value,
        json!({"ok":true,"serving":"none","current":null,"fallback":null,"error":"尚未发布任何内容"})
    );
    assert!(!dir.exists());
    let published = publish(&dir, &a(), T, &[]);
    let valid = pointer(&dir);
    let mut broken = vec![b"{".to_vec(), b"null".to_vec()];
    for field in valid.as_object().unwrap().keys() {
        let mut missing = valid.clone();
        missing.as_object_mut().unwrap().remove(field);
        broken.push(serde_json::to_vec(&missing).unwrap());
    }
    for (key, value) in [
        ("schema", json!(2)),
        ("extra", json!(true)),
        ("gif", json!("../escape")),
        ("gif", json!("A".repeat(64))),
        ("bytes", json!("10")),
        ("fallback", json!("../boot")),
    ] {
        let mut invalid = valid.clone();
        invalid[key] = value;
        broken.push(serde_json::to_vec(&invalid).unwrap());
    }
    for bytes in broken {
        assert!(serde_json::from_slice::<CurrentRecord>(&bytes).is_err());
        fs::write(dir.join("current.json"), &bytes).unwrap();
        let value = status(&dir, T);
        assert_eq!(value["serving"], "none");
        assert_eq!(value["current"], Value::Null);
        assert!(!value["error"].as_str().unwrap().is_ascii());
    }
    fs::write(
        dir.join("current.json"),
        serde_json::to_vec(&valid).unwrap(),
    )
    .unwrap();
    fs::remove_file(content(&dir, &published)).unwrap();
    let value = status(&dir, T);
    assert_eq!(value["serving"], "none");
    assert!(value["current"].is_object());
    assert!(
        value["error"]
            .as_str()
            .unwrap()
            .contains("当前内容文件缺失")
    );
    assert_eq!(store::resolve_status(&dir, T).serving_gif(), None);
}

#[test]
fn fallback_records_require_every_field_and_known_schema() {
    let temp = TempDir::new();
    let slot = set(&temp.0, "boot", &a());
    let valid = json!({"schema":1,"gif":slot["gif"],"bytes":a().len(),"set_at":T});
    for key in valid.as_object().unwrap().keys() {
        let mut value = valid.clone();
        value.as_object_mut().unwrap().remove(key);
        assert!(serde_json::from_value::<FallbackRecord>(value).is_err());
    }
    for (key, value) in [
        ("schema", json!(0)),
        ("extra", json!(0)),
        ("gif", json!("BAD")),
    ] {
        let mut invalid = valid.clone();
        invalid[key] = value;
        assert!(serde_json::from_value::<FallbackRecord>(invalid).is_err());
    }
}

#[test]
fn garbage_collection_preserves_references_and_ignores_non_content_names() {
    let temp = TempDir::new();
    let first = publish(&temp.0, &a(), T, &[]);
    let second = publish(&temp.0, &b(), T + 1, &[]);
    assert!(!content(&temp.0, &first).exists());
    assert!(content(&temp.0, &second).exists());
    let fallback = set(&temp.0, "boot", &a());
    publish(&temp.0, &b(), T + 2, &[]);
    assert!(content(&temp.0, &fallback).exists());
    let ignored = [
        "ABC.gif",
        "a.gif",
        ".unrelated",
        ".thing.tmp-not-a-number",
        "current.json.tmp-1-2",
    ];
    for name in ignored {
        fs::write(temp.0.join("store").join(name), "保留".as_bytes()).unwrap();
    }
    for directory in [&temp.0, &temp.0.join("store"), &temp.0.join("fallbacks")] {
        fs::write(directory.join(".interrupted.json.tmp-123-9"), b"{").unwrap();
    }
    let before = status(&temp.0, T + 3);
    assert_eq!(before["serving"], "current");
    assert!(store::collect_garbage(&temp.0, after_residual_deadline()).is_empty());
    assert_eq!(status(&temp.0, T + 3), before);
    for name in ignored {
        assert!(temp.0.join("store").join(name).exists());
    }
    for directory in [&temp.0, &temp.0.join("store"), &temp.0.join("fallbacks")] {
        assert!(!directory.join(".interrupted.json.tmp-123-9").exists());
    }
    // 损坏槽的引用无法确认，保留全部内容并告警。
    fs::write(temp.0.join("fallbacks/boot.json"), b"{").unwrap();
    let warnings = store::collect_garbage(&temp.0, after_residual_deadline());
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("无法确认引用关系"));
    assert!(content(&temp.0, &fallback).exists());
}

#[test]
fn publish_keeps_fresh_temporary_files_and_removes_old_residuals() {
    let temp = TempDir::new();
    publish(&temp.0, &a(), T, &[]);
    let residuals = [
        temp.0.join(".current.json.tmp-123-1"),
        temp.0.join("store/.content.gif.tmp-123-2"),
        temp.0.join("fallbacks/.boot.json.tmp-123-3"),
    ];
    for path in &residuals {
        fs::write(path, b"{").unwrap();
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    publish(&temp.0, &a(), now, &[]);
    for path in &residuals {
        assert!(path.exists());
    }
    publish(&temp.0, &a(), after_residual_deadline(), &[]);
    for path in &residuals {
        assert!(!path.exists());
    }
}

#[test]
fn temporary_file_collection_obeys_exact_age_boundary_and_clock_rollback() {
    let temp = TempDir::new();
    let path = temp.0.join(".current.json.tmp-123-0");
    fs::write(&path, b"{").unwrap();
    let modified = fs::metadata(&path)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    for now in [modified - 1, modified, modified + 3599] {
        assert!(store::collect_garbage(&temp.0, now).is_empty());
        assert!(path.exists());
    }
    assert!(store::collect_garbage(&temp.0, modified + 3600).is_empty());
    assert!(!path.exists());
}

#[test]
fn publish_with_corrupt_slot_preserves_all_content_but_cleans_old_temporary_files() {
    let temp = TempDir::new();
    let old = publish(&temp.0, &a(), T, &[]);
    fs::write(temp.0.join("fallbacks/broken.json"), b"{").unwrap();
    let residual = temp.0.join("store/.orphan.gif.tmp-123-0");
    fs::write(&residual, b"{").unwrap();
    let next = publish(&temp.0, &b(), after_residual_deadline(), &[]);
    assert!(content(&temp.0, &old).exists());
    assert!(content(&temp.0, &next).exists());
    assert!(!residual.exists());
    let warnings = next["warnings"].as_array().unwrap();
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].as_str().unwrap().contains("无法确认引用关系"));
}

#[test]
fn crash_before_rename_and_after_rename_before_gc_recovers_on_next_publish() {
    let temp = TempDir::new();
    let current = publish(&temp.0, &a(), T, &[]);
    let stable = status(&temp.0, T);
    let hash = gif::validate(&b()).sha256;
    let residuals = [
        temp.0.join(".current.json.tmp-1-0"),
        temp.0.join("fallbacks/.boot.json.tmp-1-1"),
        temp.0.join("store").join(format!(".{hash}.gif.tmp-1-2")),
    ];
    for path in &residuals {
        fs::write(path, b"{").unwrap();
    }
    assert_eq!(status(&temp.0, T), stable);
    // 模拟内容已入库但指针尚未提交；正式供应保持原状。
    assert!(store::store_content(&temp.0, &b(), &hash).unwrap());
    assert_eq!(status(&temp.0, T), stable);
    // 模拟指针 rename 完成、回收尚未开始。
    let record = CurrentRecord {
        schema: 1,
        gif: hash.clone(),
        bytes: b().len() as u64,
        published_at: T + 1,
        ttl_seconds: None,
        fallback: None,
    };
    store::atomic::write_file(
        &temp.0.join("current.json"),
        &serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
    assert_eq!(status(&temp.0, T + 1)["current"]["gif"], hash);
    assert!(content(&temp.0, &current).exists());
    // 损坏当前指针时保守保留全部内容，但仍清理残留。
    fs::write(temp.0.join("current.json"), b"{").unwrap();
    assert_eq!(status(&temp.0, T + 1)["serving"], "none");
    assert!(!store::collect_garbage(&temp.0, after_residual_deadline()).is_empty());
    assert!(content(&temp.0, &current).exists());
    assert!(store::content_path(&temp.0, &hash).exists());
    for path in &residuals {
        assert!(!path.exists());
        fs::write(path, b"{").unwrap();
    }
    let next = publish(&temp.0, &b(), after_residual_deadline(), &[]);
    assert_eq!(next["stored"], false);
    for path in &residuals {
        assert!(!path.exists());
    }
    assert!(!content(&temp.0, &current).exists());
    assert_eq!(status(&temp.0, T + 2)["serving"], "current");
}

#[test]
fn concurrent_publication_readers_observe_complete_records_and_gifs() {
    let temp = TempDir::new();
    // 两份内容都有槽引用，排除设计 D4 已说明的回收与打开竞争，专门断言提交原子性。
    set(&temp.0, "a", &a());
    set(&temp.0, "b", &b());
    publish(&temp.0, &a(), T, &[]);
    let done = Arc::new(AtomicBool::new(false));
    let reader_done = done.clone();
    let dir = temp.0.clone();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let reader = thread::spawn(move || {
        let mut reads = 0;
        loop {
            let record: CurrentRecord =
                serde_json::from_slice(&fs::read(dir.join("current.json")).unwrap()).unwrap();
            let bytes = fs::read(store::content_path(&dir, &record.gif)).unwrap();
            assert_eq!(record.bytes, bytes.len() as u64);
            assert_eq!(gif::validate(&bytes).sha256, record.gif);
            assert!(bytes == a() || bytes == b());
            reads += 1;
            if reads == 1 {
                ready_tx.send(()).unwrap();
            }
            if reader_done.load(Ordering::Acquire) {
                break;
            }
        }
    });
    ready_rx.recv().unwrap();
    let (bytes_a, bytes_b) = (a(), b());
    for i in 0..40 {
        store::publish(
            &temp.0,
            if i % 2 == 0 { &bytes_a } else { &bytes_b },
            T + i,
            PublishOptions::default(),
        )
        .unwrap();
    }
    done.store(true, Ordering::Release);
    reader.join().unwrap();
}

#[cfg(unix)]
#[test]
fn gc_permission_failure_warns_and_publish_succeeds_in_json_and_human_modes() {
    use std::os::unix::fs::PermissionsExt;
    let temp = TempDir::new();
    let old = publish(&temp.0, &a(), T, &[]);
    let hash = gif::validate(&b()).sha256;
    store::store_content(&temp.0, &b(), &hash).unwrap();
    let dir = temp.0.join("store");
    let permissions = fs::metadata(&dir).unwrap().permissions();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).unwrap();
    let json_output = invoke(&temp.0, T + 1, &["publish", "-", "--json"], Some(&b()));
    let human_output = invoke(&temp.0, T + 2, &["publish", "-"], Some(&b()));
    let warnings = store::collect_garbage(&temp.0, T + 2);
    fs::set_permissions(&dir, permissions).unwrap();
    let value = result(json_output, 0);
    assert_eq!(value["stored"], false);
    assert_eq!(value["warnings"].as_array().unwrap().len(), 1);
    let path = content(&temp.0, &old);
    let warning = format!("无法删除回收文件：{}", path.display());
    assert_eq!(value["warnings"][0], warning);
    assert_eq!(warnings.as_slice(), std::slice::from_ref(&warning));
    assert!(path.exists());
    assert_eq!(human_output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(human_output.stderr).unwrap(),
        format!("{warning}\n")
    );
    assert!(!String::from_utf8_lossy(&human_output.stdout).contains("无法删除"));
}

#[cfg(windows)]
#[test]
fn gc_sharing_failure_warns_and_publish_succeeds_in_json_and_human_modes() {
    use std::os::windows::fs::OpenOptionsExt;
    let temp = TempDir::new();
    let old = publish(&temp.0, &a(), T, &[]);
    let path = content(&temp.0, &old);
    // 禁止共享删除，模拟其它进程持有内容句柄。
    let held = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&path)
        .unwrap();
    let value = publish(&temp.0, &b(), T + 1, &[]);
    let warning = format!("无法删除回收文件：{}", path.display());
    assert_eq!(value["warnings"], json!([warning]));
    assert!(path.exists());
    let human_output = invoke(&temp.0, T + 2, &["publish", "-"], Some(&b()));
    assert_eq!(human_output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(human_output.stderr).unwrap(),
        format!("{warning}\n")
    );
    assert!(!String::from_utf8_lossy(&human_output.stdout).contains("无法删除"));
    drop(held);
    let next = publish(&temp.0, &b(), T + 3, &[]);
    assert_eq!(next["warnings"], json!([]));
    assert!(!path.exists());
}

#[test]
fn io_failures_map_to_state_and_status_remains_queryable() {
    let temp = TempDir::new();
    let blocked = temp.0.join("blocked");
    fs::write(&blocked, b"x").unwrap();
    assert!(matches!(
        store::publish(&blocked, &a(), T, PublishOptions::default()),
        Err(KernelError::State(_))
    ));
    assert!(matches!(
        store::store_content(&blocked, &a(), &gif::validate(&a()).sha256),
        Err(KernelError::State(_))
    ));
    assert!(matches!(
        store::set_fallback(&blocked, "boot", &a(), T),
        Err(KernelError::State(_))
    ));
    fs::write(temp.0.join("fallbacks"), b"x").unwrap();
    assert!(matches!(
        store::list_fallbacks(&temp.0),
        Err(KernelError::State(_))
    ));
    fs::create_dir(temp.0.join("current.json")).unwrap();
    let value = status(&temp.0, T);
    assert_eq!(value["serving"], "none");
    assert!(value["error"].is_string());
}

#[test]
fn human_outputs_use_utc_and_each_status_field_and_boolean() {
    let temp = TempDir::new();
    let file = temp.0.join("input.gif");
    fs::write(&file, a()).unwrap();
    let set_output = command(&temp.0, T)
        .args(["fallback", "set", "boot"])
        .arg(&file)
        .output()
        .unwrap();
    assert!(set_output.status.success());
    assert!(String::from_utf8_lossy(&set_output.stdout).contains("set_at：2026-09-06T04:20:11Z"));
    let list = invoke(&temp.0, T, &["fallback", "list"], None);
    assert_eq!(String::from_utf8_lossy(&list.stdout).lines().count(), 1);
    assert!(String::from_utf8_lossy(&list.stdout).contains("2026-09-06T04:20:11Z"));
    let published = invoke(
        &temp.0,
        T,
        &["publish", "-", "--ttl", "420", "--fallback", "boot"],
        Some(&b()),
    );
    assert!(published.status.success());
    assert!(published.stderr.is_empty());
    let text = String::from_utf8(published.stdout).unwrap();
    assert_eq!(text.lines().count(), 6);
    assert!(text.contains("published_at：2026-09-06T04:20:11Z"));
    for (now, expired) in [(T, "否"), (T + 420, "是")] {
        let output = invoke(&temp.0, now, &["status"], None);
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains(&format!("current.expired：{expired}")));
        assert!(text.contains("current.expires_at：2026-09-06T04:27:11Z"));
        assert!(text.contains("fallback.missing：否"));
    }
}

#[test]
fn extreme_timestamps_do_not_overflow_or_expire_early() {
    let temp = TempDir::new();
    store::publish(
        &temp.0,
        &a(),
        u64::MAX - 1,
        PublishOptions {
            ttl_seconds: Some(10),
            fallback: None,
        },
    )
    .unwrap();
    let resolved = store::resolve_status(&temp.0, u64::MAX);
    assert_eq!(resolved.serving, Serving::Current);
    let current = resolved.current.unwrap();
    assert!(!current.expired);
    assert_eq!(current.expires_at, None);
    assert_eq!(
        status(&temp.0, u64::MAX)["current"]["expires_at"],
        Value::Null
    );
}
