//! 全部负载均为合成数据；断言不输出凭据原值。

use super::*;
use std::{cell::RefCell, collections::HashMap};

fn payload(source: usize) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({"userId": format!("synthetic-user-{source}"), "passToken": format!("synthetic-secret-{source}")})).unwrap()
}
fn missing(_: &Path) -> io::Result<Vec<u8>> {
    Err(io::ErrorKind::NotFound.into())
}
fn no_process(_: &ProcessRequest) -> Result<Vec<u8>, ProcessError> {
    panic!("不应运行子进程")
}
fn state_error(error: &MiCloudError) {
    assert!(matches!(error, MiCloudError::State(_)));
    assert_eq!(error.exit_code(), 4);
    assert!(!error.message().is_ascii());
    assert!(!error.message().contains("synthetic"));
    assert!(!error.message().contains('/'));
}

#[test]
fn credentials_json_rules() {
    for (value, expected_user) in [
        (serde_json::json!("synthetic-user"), "synthetic-user"),
        (serde_json::json!(12345), "12345"),
        (serde_json::json!(0), "0"),
        (serde_json::json!(-12), "-12"),
        (serde_json::json!(1.25), "1.25"),
    ] {
        for name in ["userId", "user_id"] {
            for token_name in ["passToken", "pass_token"] {
                for device_name in ["deviceId", "device_id"] {
                    let input = serde_json::json!({name: value, token_name: " synthetic-secret ", device_name: " synthetic-device "});
                    let result =
                        Credentials::from_json(&serde_json::to_vec(&input).unwrap()).unwrap();
                    assert!(result.user_id == expected_user);
                    assert!(result.pass_token == "synthetic-secret");
                    assert!(result.device_id == "synthetic-device");
                }
            }
        }
    }
    for device in [
        Value::Null,
        Value::String("".into()),
        Value::String(" \t ".into()),
    ] {
        let value = serde_json::json!({"userId": " synthetic-user ", "passToken": "synthetic-secret", "deviceId": device});
        let result = Credentials::from_value(&value).unwrap();
        assert!(result.device_id == DEFAULT_DEVICE_ID);
        assert!(result.user_id == "synthetic-user");
    }
    assert!(Credentials::from_json(&payload(1)).unwrap().device_id == DEFAULT_DEVICE_ID);
    let mut bom = vec![0xef, 0xbb, 0xbf];
    bom.extend(payload(1));
    assert!(Credentials::from_json(&bom).is_ok());
}

#[test]
fn credentials_camel_case_wins_without_fallback() {
    let mut value = serde_json::json!({"userId":"synthetic-primary", "user_id":"synthetic-secondary", "passToken":"synthetic-primary-token", "pass_token":"synthetic-secondary-token", "deviceId":"synthetic-primary-device", "device_id":"synthetic-secondary-device"});
    let parsed = Credentials::from_value(&value).unwrap();
    assert!(parsed.user_id == "synthetic-primary");
    assert!(parsed.pass_token == "synthetic-primary-token");
    assert!(parsed.device_id == "synthetic-primary-device");
    value["deviceId"] = Value::Null;
    assert!(Credentials::from_value(&value).unwrap().device_id == DEFAULT_DEVICE_ID);
    for name in ["userId", "passToken"] {
        let mut invalid = value.clone();
        invalid[name] = Value::String(" ".into());
        state_error(&Credentials::from_value(&invalid).unwrap_err());
        invalid[name] = Value::Null;
        state_error(&Credentials::from_value(&invalid).unwrap_err());
    }
}

#[test]
fn credentials_rejects_invalid_payloads() {
    for input in [
        b"".as_slice(),
        b"{",
        b"[]",
        b"null",
        b"{}",
        b"\xff",
        br#"{"userId":"synthetic-user"}"#,
        br#"{"passToken":"synthetic-secret"}"#,
        br#"{"userId":true,"passToken":"synthetic-secret"}"#,
        br#"{"userId":" ","passToken":"synthetic-secret"}"#,
        br#"{"userId":1,"passToken":" "}"#,
        br#"{"userId":1,"passToken":123}"#,
        br#"{"userId":1,"passToken":"synthetic-secret","deviceId":false}"#,
    ] {
        state_error(&Credentials::from_json(input).unwrap_err());
    }
}

#[test]
fn credentials_debug_only_token_length() {
    let result = Credentials::from_json(&payload(2)).unwrap();
    let debug = format!("{result:?}");
    for secret in [&result.user_id, &result.pass_token, &result.device_id] {
        assert!(!debug.contains(secret));
    }
    assert_eq!(
        debug,
        format!("登录凭据 {{ 令牌长度: {} }}", result.pass_token.len())
    );
}

#[test]
fn credentials_priority_matrix() {
    // 五个单独来源加十组两两竞争，记录全部副作用以验证短路。
    for first in 0..5 {
        for second in first..5 {
            let enabled = |index| index == first || index == second;
            let mut vars = HashMap::<&str, OsString>::from([("HOME", "synthetic-home".into())]);
            if enabled(1) {
                vars.insert("AP01_BRIDGE_MI_USER_ID", " synthetic-user-1 ".into());
                vars.insert("AP01_BRIDGE_MI_PASS_TOKEN", " synthetic-secret-1 ".into());
            }
            if enabled(2) {
                vars.insert("AP01_BRIDGE_MI_CREDENTIALS", "synthetic-file".into());
            }
            if enabled(3) {
                vars.insert(
                    "AP01_BRIDGE_MI_KEYCHAIN_SERVICE",
                    "synthetic-service".into(),
                );
            }
            let calls = RefCell::new(Vec::new());
            let read = |path: &Path| {
                if path == Path::new("synthetic-explicit") {
                    calls.borrow_mut().push(0);
                    Ok(payload(0))
                } else if path == Path::new("synthetic-file") {
                    calls.borrow_mut().push(2);
                    Ok(payload(2))
                } else {
                    assert!(path == Path::new("synthetic-home").join(DEFAULT_PREFS));
                    calls.borrow_mut().push(4);
                    Ok(vec![])
                }
            };
            let run = |request: &ProcessRequest| {
                assert_eq!(request.timeout, Duration::from_secs(10));
                if request.program == OsStr::new("/usr/bin/security") {
                    calls.borrow_mut().push(3);
                    Ok(payload(3))
                } else {
                    calls.borrow_mut().push(5);
                    Ok(payload(4))
                }
            };
            let result = load_credentials(
                enabled(0).then_some(Path::new("synthetic-explicit")),
                None,
                &|name| vars.get(name).cloned(),
                &read,
                &run,
                Platform::MacOs,
            )
            .unwrap();
            assert!(result.user_id == format!("synthetic-user-{first}"));
            assert!(result.pass_token == format!("synthetic-secret-{first}"));
            assert!(result.device_id == DEFAULT_DEVICE_ID);
            let expected = match first {
                0 => vec![0],
                1 => vec![],
                2 => vec![2],
                3 => vec![3],
                _ => vec![4, 5],
            };
            assert_eq!(*calls.borrow(), expected);
        }
    }
}

#[test]
fn credentials_empty_sources_and_no_merging() {
    for platform in [Platform::MacOs, Platform::Windows, Platform::Other] {
        state_error(
            &load_credentials(None, None, &|_| None, &missing, &no_process, platform).unwrap_err(),
        );
        state_error(
            &load_credentials(
                None,
                None,
                &|name| {
                    (name
                        == if platform == Platform::Windows {
                            "USERPROFILE"
                        } else {
                            "HOME"
                        })
                    .then(|| "synthetic-home".into())
                },
                &missing,
                &no_process,
                platform,
            )
            .unwrap_err(),
        );
    }
    let vars = |name: &str| match name {
        "AP01_BRIDGE_MI_USER_ID" => Some("synthetic-user".into()),
        "AP01_BRIDGE_MI_PASS_TOKEN" => Some("synthetic-secret".into()),
        "AP01_BRIDGE_MI_DEVICE_ID" => Some("synthetic-device".into()),
        _ => None,
    };
    let loaded = load_credentials(
        Some(Path::new("synthetic-file")),
        None,
        &vars,
        &|_| Ok(payload(0)),
        &no_process,
        Platform::MacOs,
    )
    .unwrap();
    assert!(loaded.device_id == DEFAULT_DEVICE_ID);
    state_error(
        &load_credentials(
            Some(Path::new("synthetic-file")),
            None,
            &vars,
            &|_| Ok(br#"{"userId":1}"#.to_vec()),
            &no_process,
            Platform::MacOs,
        )
        .unwrap_err(),
    );
}

#[test]
fn credentials_environment_trimming_and_partial_pair() {
    for (user, token) in [
        ("synthetic-user", ""),
        ("", "synthetic-secret"),
        ("synthetic-user", " \t "),
        ("", ""),
    ] {
        let env = |name: &str| match name {
            "AP01_BRIDGE_MI_USER_ID" => Some(user.into()),
            "AP01_BRIDGE_MI_PASS_TOKEN" => Some(token.into()),
            "AP01_BRIDGE_MI_CREDENTIALS" => Some("synthetic-file".into()),
            _ => None,
        };
        let result = load_credentials(
            None,
            None,
            &env,
            &|_| Ok(payload(2)),
            &no_process,
            Platform::MacOs,
        )
        .unwrap();
        assert!(result.user_id == "synthetic-user-2");
    }
    for device in [None, Some(" \t "), Some(" synthetic-device ")] {
        let env = |name: &str| match name {
            "AP01_BRIDGE_MI_USER_ID" => Some(" synthetic-user ".into()),
            "AP01_BRIDGE_MI_PASS_TOKEN" => Some(" synthetic-secret ".into()),
            "AP01_BRIDGE_MI_DEVICE_ID" => device.map(OsString::from),
            _ => None,
        };
        let result =
            load_credentials(None, None, &env, &missing, &no_process, Platform::MacOs).unwrap();
        assert!(result.user_id == "synthetic-user" && result.pass_token == "synthetic-secret");
        assert!(
            result.device_id
                == if device == Some(" synthetic-device ") {
                    "synthetic-device"
                } else {
                    DEFAULT_DEVICE_ID
                }
        );
    }
}

#[test]
fn credentials_environment_file_trims_and_skips_blank() {
    for filename in [" ", " synthetic-file "] {
        let env = |name: &str| match name {
            "AP01_BRIDGE_MI_CREDENTIALS" => Some(filename.into()),
            "AP01_BRIDGE_MI_KEYCHAIN_SERVICE" => Some("synthetic-service".into()),
            _ => None,
        };
        let read = |path: &Path| {
            assert_eq!(filename, " synthetic-file ");
            assert_eq!(path, Path::new("synthetic-file"));
            Ok(payload(2))
        };
        let run = |request: &ProcessRequest| {
            assert_eq!(filename, " ");
            assert_eq!(request.program, OsStr::new("/usr/bin/security"));
            Ok(payload(3))
        };
        let result = load_credentials(None, None, &env, &read, &run, Platform::MacOs).unwrap();
        let expected = if filename == " " { 3 } else { 2 };
        assert!(result.user_id == format!("synthetic-user-{expected}"));
    }
}

#[test]
fn home_and_expansion_follow_injected_platform() {
    let env = |name: &str| match name {
        "HOME" => Some("synthetic-unix-home".into()),
        "USERPROFILE" => Some("synthetic-windows-home".into()),
        _ => None,
    };
    for (platform, expected) in [
        (Platform::MacOs, "synthetic-unix-home"),
        (Platform::Other, "synthetic-unix-home"),
        (Platform::Windows, "synthetic-windows-home"),
    ] {
        assert_eq!(home(&env, platform), Some(PathBuf::from(expected)));
        assert_eq!(
            expand(Path::new("~/synthetic-file"), &env, platform).unwrap(),
            Path::new(expected).join("synthetic-file")
        );
        assert!(home(&|_| None, platform).is_none());
    }
}

#[test]
fn credentials_file_expansion_and_no_fallback() {
    for (platform, home_name) in [
        (Platform::MacOs, "HOME"),
        (Platform::Windows, "USERPROFILE"),
        (Platform::Other, "HOME"),
    ] {
        for filename in ["~/synthetic-file", "~"] {
            let env = |name: &str| {
                if name == home_name {
                    Some("synthetic-home".into())
                } else if name == "AP01_BRIDGE_MI_CREDENTIALS" {
                    Some(filename.into())
                } else if name == "AP01_BRIDGE_MI_KEYCHAIN_SERVICE" {
                    Some("synthetic-service".into())
                } else {
                    None
                }
            };
            let read = |path: &Path| {
                let expected = if filename == "~" {
                    PathBuf::from("synthetic-home")
                } else {
                    Path::new("synthetic-home").join("synthetic-file")
                };
                assert!(path == expected);
                Ok(payload(2))
            };
            assert!(load_credentials(None, None, &env, &read, &no_process, platform).is_ok());
            state_error(
                &load_credentials(None, None, &env, &missing, &no_process, platform).unwrap_err(),
            );
        }
    }
    state_error(
        &load_credentials(
            None,
            None,
            &|name| (name == "AP01_BRIDGE_MI_CREDENTIALS").then(|| "~/synthetic-file".into()),
            &missing,
            &no_process,
            Platform::MacOs,
        )
        .unwrap_err(),
    );
}

#[test]
fn keychain_argv_and_timeout() {
    for account in [None, Some(""), Some(" \t "), Some(" synthetic-account ")] {
        let env = |name: &str| match name {
            "AP01_BRIDGE_MI_KEYCHAIN_SERVICE" => Some(" synthetic-service ".into()),
            "AP01_BRIDGE_MI_KEYCHAIN_ACCOUNT" => account.map(OsString::from),
            _ => None,
        };
        let run = |request: &ProcessRequest| {
            assert!(request.program == OsStr::new("/usr/bin/security"));
            let expected_account = if account == Some(" synthetic-account ") {
                "synthetic-account"
            } else {
                "relay"
            };
            let expected = [
                "find-generic-password",
                "-s",
                "synthetic-service",
                "-a",
                expected_account,
                "-w",
            ]
            .map(OsString::from);
            assert!(request.args == expected);
            assert!(
                !request
                    .args
                    .iter()
                    .any(|arg| arg.to_string_lossy().contains("synthetic-secret"))
            );
            assert_eq!(request.timeout, Duration::from_secs(10));
            Ok(payload(3))
        };
        assert!(load_credentials(None, None, &env, &missing, &run, Platform::MacOs).is_ok());
    }
}

#[test]
fn keychain_failures_fold() {
    for outcome in [
        Err(ProcessError::Start),
        Err(ProcessError::Failed),
        Err(ProcessError::Timeout),
        Ok(b"invalid synthetic output".to_vec()),
        Ok(b"{}".to_vec()),
    ] {
        let error = load_keychain(
            "synthetic-service",
            "relay",
            &|_| outcome.clone(),
            Platform::MacOs,
        )
        .unwrap_err();
        state_error(&error);
        assert_eq!(error.message(), KEYCHAIN_ERROR);
    }
}

#[test]
fn prefs_argv_override_and_failures() {
    let path = Path::new("synthetic-override");
    let run = |request: &ProcessRequest| {
        assert!(request.program == OsStr::new("/usr/bin/plutil"));
        let expected = ["-extract", "GroupShareAccountInfo", "json", "-o", "-"];
        assert!(request.args[..5] == expected.map(OsString::from));
        assert!(request.args[5] == path.as_os_str());
        assert_eq!(request.timeout, Duration::from_secs(10));
        Ok(payload(4))
    };
    let read = |actual: &Path| {
        assert!(actual == path);
        Ok(vec![])
    };
    assert!(load_credentials(None, Some(path), &|_| None, &read, &run, Platform::MacOs).is_ok());
    state_error(&load_prefs(path, &missing, &no_process, Platform::MacOs).unwrap_err());
    for outcome in [
        Err(ProcessError::Start),
        Err(ProcessError::Failed),
        Err(ProcessError::Timeout),
        Ok(b"{invalid".to_vec()),
        Ok(b"{}".to_vec()),
        Ok("缺少 GroupShareAccountInfo 键".as_bytes().to_vec()),
        Ok(b"[]".to_vec()),
    ] {
        let error = load_prefs(path, &read, &|_| outcome.clone(), Platform::MacOs).unwrap_err();
        state_error(&error);
        assert_eq!(error.message(), PREFS_ERROR);
    }
}

#[test]
fn platform_sources_return_same_error_stage() {
    let errors = [
        load_keychain("synthetic-service", "relay", &no_process, Platform::Other).unwrap_err(),
        load_prefs(
            Path::new("synthetic-file"),
            &|_| panic!("不应读取文件"),
            &no_process,
            Platform::Other,
        )
        .unwrap_err(),
    ];
    for error in errors {
        state_error(&error);
        assert!(error.message().contains("仅 macOS 可用"));
    }
}

#[cfg(not(target_os = "macos"))]
#[test]
fn native_platform_sources_unavailable() {
    assert_eq!(
        Platform::current(),
        if cfg!(windows) {
            Platform::Windows
        } else {
            Platform::Other
        }
    );
    let native = load_keychain(
        "synthetic-service",
        "relay",
        &no_process,
        Platform::current(),
    )
    .unwrap_err();
    let simulated = load_keychain(
        "synthetic-service",
        "relay",
        &|_| Err(ProcessError::Failed),
        Platform::MacOs,
    )
    .unwrap_err();
    assert_eq!(
        std::mem::discriminant(&native),
        std::mem::discriminant(&simulated)
    );
    assert_eq!(native.exit_code(), simulated.exit_code());
    let native = load_prefs(
        Path::new("synthetic-file"),
        &missing,
        &no_process,
        Platform::current(),
    )
    .unwrap_err();
    assert!(native.message().contains("仅 macOS 可用"));
    state_error(&native);
}

#[cfg(target_os = "macos")]
#[test]
fn prefs_real_binary_and_xml_fixture() {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Temp(PathBuf);
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let directory = loop {
        let path = std::env::temp_dir().join(format!(
            "ap01-cloud-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        match std::fs::create_dir(&path) {
            Ok(()) => break Temp(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(_) => panic!("无法创建合成夹具目录"),
        }
    };
    let path = directory.0.join("synthetic.plist");
    for format in ["binary1", "xml1"] {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict>
<key>synthetic-date</key><date>2026-01-01T00:00:00Z</date>
<key>synthetic-data</key><data>c3ludGhldGlj</data>
<key>GroupShareAccountInfo</key><dict>
<key>userId</key><string>synthetic-user-4</string>
<key>passToken</key><string>synthetic-secret-4</string>
</dict></dict></plist>"#;
        std::fs::write(&path, xml).unwrap();
        run_process(&ProcessRequest {
            program: "/usr/bin/plutil".into(),
            args: vec![
                "-convert".into(),
                format.into(),
                path.as_os_str().to_owned(),
            ],
            timeout: SOURCE_TIMEOUT,
        })
        .unwrap();
        if format == "binary1" {
            assert!(std::fs::read(&path).unwrap().starts_with(b"bplist00"));
        }
        let parsed = load_credentials(
            None,
            Some(&path),
            &|_| None,
            &|p| std::fs::read(p),
            &run_process,
            Platform::current(),
        )
        .unwrap();
        assert!(parsed.user_id == "synthetic-user-4");
        assert!(parsed.pass_token == "synthetic-secret-4");
    }
}

#[cfg(unix)]
#[test]
fn subprocess_timeout_kills_and_reaps() {
    let started = Instant::now();
    let result = run_process(&ProcessRequest {
        program: "/bin/sleep".into(),
        args: vec!["5".into()],
        timeout: Duration::from_millis(200),
    });
    assert_eq!(result, Err(ProcessError::Timeout));
    assert!(started.elapsed() >= Duration::from_millis(200));
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[cfg(unix)]
#[test]
fn subprocess_drains_output_and_folds_exit_failure() {
    let run = |script: &str| {
        run_process(&ProcessRequest {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
            timeout: Duration::from_secs(2),
        })
    };
    let bytes = run("i=0; while [ $i -lt 20000 ]; do printf synthetic; i=$((i+1)); done").unwrap();
    assert_eq!(bytes.len(), 180000);
    assert_eq!(run("exit 7"), Err(ProcessError::Failed));
    assert_eq!(
        run_process(&ProcessRequest {
            program: "synthetic-nonexistent-program".into(),
            args: vec![],
            timeout: Duration::from_millis(200)
        }),
        Err(ProcessError::Start)
    );
}

#[test]
fn subprocess_output_gets_grace_after_deadline() {
    for remaining in [Duration::ZERO, Duration::from_millis(1)] {
        let (sender, receiver) = mpsc::channel();
        let worker = thread::spawn(move || {
            thread::sleep(Duration::from_millis(5));
            sender.send(Ok(b"synthetic-output".to_vec())).unwrap();
        });
        assert_eq!(
            receive_output(&receiver, remaining).unwrap(),
            b"synthetic-output"
        );
        worker.join().unwrap();
    }
}

#[test]
fn subprocess_output_grace_remains_bounded() {
    let (_sender, receiver) = mpsc::channel();
    let started = Instant::now();
    assert_eq!(
        receive_output(&receiver, Duration::ZERO),
        Err(ProcessError::Timeout)
    );
    assert!(started.elapsed() >= Duration::from_millis(50));
    assert!(started.elapsed() < Duration::from_secs(1));
}
