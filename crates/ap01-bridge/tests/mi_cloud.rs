//! 明文回环桩的端到端验证，只在带调试接缝的构建中运行。
#![cfg(debug_assertions)]

mod common;
use ap01_mi_cloud::{crypto, golden::vectors};
use common::{TempDir, bridge};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

const REJECTED_LOCATION: &str = "http://example.invalid/sts";
const SYNTHETIC_TARGET: &str = "synthetic-target";

fn decode(value: &str) -> String {
    let mut bytes = Vec::new();
    let mut input = value.bytes();
    while let Some(byte) = input.next() {
        bytes.push(match byte {
            b'+' => b' ',
            b'%' => {
                let digits = [input.next().unwrap(), input.next().unwrap()];
                u8::from_str_radix(std::str::from_utf8(&digits).unwrap(), 16).unwrap()
            }
            _ => byte,
        });
    }
    String::from_utf8(bytes).unwrap()
}

fn handle(stream: TcpStream, base: &str, reject_at: Option<usize>, index: usize) -> bool {
    // 部分平台接受的连接会继承监听器的非阻塞模式，读取前显式恢复阻塞。
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut reader = BufReader::new(stream);
    let mut start = String::new();
    if reader.read_line(&mut start).unwrap() == 0 {
        // 未发送请求便关闭的连接不消耗脚本步骤，也不计入请求数。
        return false;
    }
    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        assert!(reader.read_line(&mut line).unwrap() > 0);
        if line == "\r\n" {
            break;
        }
        let (name, value) = line.trim_end().split_once(':').unwrap();
        headers.push((name.to_ascii_lowercase(), value.trim().to_owned()));
    }
    let header = |name: &str| {
        headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, v)| v.as_str())
            .unwrap_or("")
    };
    let length: usize = header("content-length").parse().unwrap_or(0);
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    let vector = vectors();
    let (status, extra, body) = match index {
        0 => {
            assert!(start.starts_with("GET /pass/serviceLogin?sid=xiaomiio&_json=true "));
            assert_eq!(
                header("cookie"),
                "userId=synthetic-user; passToken=synthetic-pass; deviceId=synthetic-device"
            );
            assert_eq!(header("user-agent"), "APP/com.xiaomi.mihome APPV/9.1.200");
            let location = if reject_at == Some(0) {
                REJECTED_LOCATION.into()
            } else {
                format!("{base}/sts")
            };
            (
                200,
                String::new(),
                format!(
                    "&&&START&&&{}",
                    json!({"code":0,"ssecurity":vector["inputs"]["ssecurity_b64"],"location":location})
                ),
            )
        }
        1 => {
            assert!(start.starts_with("GET /sts "));
            let location = if reject_at == Some(1) {
                REJECTED_LOCATION.into()
            } else {
                format!("{base}/finish")
            };
            (
                302,
                format!(
                    "Location: {location}\r\nSet-Cookie: serviceToken=synthetic-old; Path=/\r\nSet-Cookie: extra=synthetic-extra\r\n"
                ),
                String::new(),
            )
        }
        2 => {
            assert!(start.starts_with("GET /finish "));
            assert_eq!(
                header("cookie"),
                "userId=synthetic-user; passToken=synthetic-pass; deviceId=synthetic-device; serviceToken=synthetic-old; extra=synthetic-extra"
            );
            (
                200,
                "Set-Cookie: serviceToken=synthetic-final; Path=/\r\n".into(),
                String::new(),
            )
        }
        3 | 4 => {
            let form: Vec<_> = std::str::from_utf8(&body)
                .unwrap()
                .split('&')
                .map(|part| {
                    let (k, v) = part.split_once('=').unwrap();
                    (decode(k), decode(v))
                })
                .collect();
            assert_eq!(
                form.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(),
                ["data", "rc4_hash__", "signature", "ssecurity", "_nonce"]
            );
            assert_eq!(form[3].1, vector["inputs"]["ssecurity_b64"]);
            let key = crypto::signed_nonce(&form[3].1, &form[4].1).unwrap();
            let plain = crypto::rc4_decrypt(&key, &form[0].1).unwrap();
            let path = if index == 3 {
                "/app/home/device_list".to_owned()
            } else {
                format!("/app/home/rpc/{SYNTHETIC_TARGET}")
            };
            assert!(start.starts_with(&format!("POST {path} ")));
            assert_eq!(
                form[2].1,
                crypto::signature("POST", &path, &form[..2], &key)
            );
            assert_eq!(
                crypto::rc4_decrypt(&key, &form[1].1).unwrap(),
                crypto::signature("POST", &path, &[("data".into(), plain.clone())], &key)
            );
            assert_eq!(header("accept-encoding"), "identity");
            assert_eq!(header("content-type"), "application/x-www-form-urlencoded");
            assert_eq!(header("miot-encrypt-algorithm"), "ENCRYPT-RC4");
            assert_eq!(header("x-xiaomi-protocal-flag-cli"), "");
            assert_eq!(
                header("cookie"),
                "userId=synthetic-user; yetAnotherServiceToken=synthetic-final; serviceToken=synthetic-final; locale=zh_CN; timezone=GMT+08:00; channel=MI_APP_STORE"
            );
            let result = if index == 3 {
                assert_eq!(
                    plain,
                    r#"{"getVirtualModel":true,"getHuamiDevices":1,"get_split_device":false,"support_smart_home":true}"#
                );
                json!({"list":[{"model":"other","did":"synthetic-other"},{"did":SYNTHETIC_TARGET,"model":"njcuk.enstor.ap01","isOnline":true,"fw_version":" \t "}]})
            } else {
                let data: Value = serde_json::from_str(&plain).unwrap();
                let id = data["id"].as_u64().unwrap();
                assert!((1_000_000..=9_999_999).contains(&id));
                assert_eq!(
                    plain,
                    format!(r#"{{"id":{id},"method":"miIO.info","params":[]}}"#)
                );
                json!({"life":12345,"fw_ver":" 1.0.2_0041 ","fw_version":"unused","model":"njcuk.enstor.ap01"})
            };
            (
                200,
                String::new(),
                crypto::rc4_encrypt(&key, &json!({"code":0,"result":result}).to_string()).unwrap(),
            )
        }
        _ => panic!("不得重新登录或追加请求"),
    };
    write!(
        reader.get_mut(),
        "HTTP/1.1 {status} OK\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    true
}

struct Stub {
    base: String,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<usize>>,
}
impl Stub {
    fn start(reject_at: Option<usize>) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("需要可绑定回环端口的本地环境");
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker_base = base.clone();
        let worker = thread::spawn(move || {
            let mut count = 0;
            while !worker_stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        if handle(stream, &worker_base, reject_at, count) {
                            count += 1;
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(_) => panic!("桩无法接收连接"),
                }
            }
            count
        });
        Self {
            base,
            stop,
            worker: Some(worker),
        }
    }
    fn finish(mut self) -> usize {
        self.stop.store(true, Ordering::SeqCst);
        self.worker.take().unwrap().join().expect("桩请求验证失败")
    }
}
impl Drop for Stub {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn command(stub: &Stub, data: &TempDir) -> std::process::Command {
    let mut command = bridge();
    for name in [
        "AP01_BRIDGE_MI_USER_ID",
        "AP01_BRIDGE_MI_PASS_TOKEN",
        "AP01_BRIDGE_MI_DEVICE_ID",
        "AP01_BRIDGE_MI_CREDENTIALS",
        "AP01_BRIDGE_MI_KEYCHAIN_SERVICE",
        "AP01_BRIDGE_MI_KEYCHAIN_ACCOUNT",
    ] {
        command.env_remove(name);
    }
    command
        .args(["mi", "ap01", "--prefs"])
        .arg(data.0.join("missing-prefs"))
        .arg("--data-dir")
        .arg(&data.0)
        .env("AP01_BRIDGE_MI_ACCOUNT_URL", &stub.base)
        .env("AP01_BRIDGE_MI_API_URL", &stub.base)
        .env("AP01_BRIDGE_MI_USER_ID", "synthetic-user")
        .env("AP01_BRIDGE_MI_PASS_TOKEN", "synthetic-pass")
        .env("AP01_BRIDGE_MI_DEVICE_ID", "synthetic-device");
    command
}

#[test]
fn mi_loopback_json_and_human_success_leave_data_dir_empty() {
    for json_mode in [true, false] {
        let data = TempDir::new();
        let stub = Stub::start(None);
        let mut command = command(&stub, &data);
        if json_mode {
            command.arg("--json");
        }
        let output = command.output().unwrap();
        assert_eq!(output.status.code(), Some(0));
        assert!(output.stderr.is_empty());
        if json_mode {
            assert_eq!(
                serde_json::from_slice::<Value>(&output.stdout).unwrap(),
                json!({"ok":true,"model":"njcuk.enstor.ap01","firmware_version":"1.0.2_0041","online":true,"uptime_seconds":12345})
            );
        } else {
            assert_eq!(
                String::from_utf8(output.stdout).unwrap(),
                "型号: njcuk.enstor.ap01\n固件版本: 1.0.2_0041\n在线: 是\n运行秒数: 12345\n"
            );
        }
        assert_eq!(fs::read_dir(&data.0).unwrap().count(), 0);
        assert_eq!(stub.finish(), 5);
    }
}

#[test]
fn mi_loopback_rejects_nonloopback_plaintext_location() {
    for reject_at in [0, 1] {
        for json_mode in [true, false] {
            let data = TempDir::new();
            let stub = Stub::start(Some(reject_at));
            let mut command = command(&stub, &data);
            if json_mode {
                command.arg("--json");
            }
            let output = command.output().unwrap();
            assert_eq!(output.status.code(), Some(5));
            if json_mode {
                let value: Value = serde_json::from_slice(&output.stdout).unwrap();
                assert_eq!(value["error"]["code"], 5);
                assert_eq!(value.as_object().unwrap().len(), 2);
            }
            for bytes in [&output.stdout, &output.stderr] {
                let text = std::str::from_utf8(bytes).unwrap();
                assert!(!text.contains(REJECTED_LOCATION));
                assert!(!text.contains("http://"));
                assert!(!text.contains("synthetic-"));
            }
            assert_eq!(fs::read_dir(&data.0).unwrap().count(), 0);
            assert_eq!(stub.finish(), reject_at + 1);
        }
    }
}
