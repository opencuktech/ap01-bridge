//! 捕获日常命令与服务日志，回归敏感数据禁令。

mod common;

use ap01_gif::testkit::{Frame, GifBuilder};
use common::{TempDir, bridge};
use serde_json::Value;
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    path::Path,
    process::{Child, Stdio},
    sync::mpsc,
    thread::{self, JoinHandle},
    time::Duration,
};

fn capture_commands(dir: &Path) -> String {
    let file = dir.join("frame.gif");
    fs::write(&file, GifBuilder::default().frame(Frame::default()).build()).unwrap();
    let file = file.to_str().expect("测试夹具路径应为有效文本");
    let missing_prefs = dir.join("missing-prefs");
    let missing_prefs = missing_prefs.to_str().unwrap();
    let mut haystack = String::new();
    for (args, code) in [
        (vec!["doctor"], 0),
        (vec!["mi", "ap01", "--prefs", missing_prefs], 4),
        (vec!["validate", file], 0),
        (vec!["fallback", "set", "boot", file], 0),
        (
            vec!["publish", file, "--ttl", "60", "--fallback", "boot"],
            0,
        ),
        (vec!["fallback", "list"], 0),
        (vec!["status"], 0),
        (vec!["fallback", "rm", "boot"], 4),
    ] {
        for json_mode in [false, true] {
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
            command.arg("--data-dir").arg(dir).args(&args);
            command.env("AP01_BRIDGE_FAKE_NOW", "1000");
            if json_mode {
                command.arg("--json");
            }
            let output = command.output().unwrap();
            haystack.push_str(std::str::from_utf8(&output.stdout).unwrap());
            haystack.push_str(std::str::from_utf8(&output.stderr).unwrap());
            assert_eq!(
                output.status.code(),
                Some(code),
                "命令 {args:?}（JSON：{json_mode}）输出：{output:?}"
            );
        }
    }
    haystack
}

fn assert_no_sensitive_data(haystack: &str) {
    assert!(!haystack.is_empty(), "必须捕获实际输出");
    assert!(haystack.contains("\"ok\""), "必须捕获命令结果对象");
    for forbidden in [
        "cookie",
        "Cookie",
        "serviceToken",
        "ssecurity",
        "passToken",
        "userId",
        "deviceId",
        "did=",
        "DID",
        "https://",
        "http://",
        "ota_url",
        "OTA_URL",
        ".bin",
        "Xiaomi",
        "xiaomi",
        "mi.com",
    ] {
        assert!(
            !haystack.contains(forbidden),
            "捕获输出包含禁用子串：{forbidden}"
        );
    }
}

#[test]
fn command_outputs_do_not_contain_sensitive_data() {
    let temp = TempDir::new();
    let haystack = capture_commands(&temp.0);
    assert_no_sensitive_data(&haystack);
}

// 沿用 serve 测试的守卫思路：断言前建立守卫，并持续排空输出管道。
struct ServeGuard {
    child: Child,
    stdout: Option<JoinHandle<String>>,
    stderr: Option<JoinHandle<String>>,
    port: u16,
}

impl ServeGuard {
    fn start(dir: &Path) -> Self {
        let mut child = bridge()
            .arg("--data-dir")
            .arg(dir)
            .args(["serve", "--bind", "127.0.0.1", "--port", "0", "--json"])
            .env("AP01_BRIDGE_FAKE_NOW", "1000")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let mut stderr = child.stderr.take().unwrap();
        let mut guard = Self {
            child,
            stdout: None,
            stderr: None,
            port: 0,
        };
        let (sender, lines) = mpsc::channel();
        guard.stdout = Some(thread::spawn(move || {
            let mut output = String::new();
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap() == 0 {
                    break;
                }
                output.push_str(&line);
                let _ = sender.send(line);
            }
            output
        }));
        guard.stderr = Some(thread::spawn(move || {
            let mut output = String::new();
            stderr.read_to_string(&mut output).unwrap();
            output
        }));
        let first = lines
            .recv_timeout(Duration::from_secs(10))
            .expect("服务应及时输出启动结果");
        let event: Value = serde_json::from_str(&first).unwrap();
        assert_eq!(event["event"], "listening", "服务启动输出：{first}");
        assert_eq!(event["bind"], "127.0.0.1");
        guard.port = u16::try_from(event["port"].as_u64().unwrap()).unwrap();
        assert!(guard.port > 0);
        guard
    }

    fn get(&self, path: &str) {
        let mut stream = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        write!(stream, "GET {path} HTTP/1.0\r\n\r\n").unwrap();
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .expect("服务端必须关闭连接，使客户端读到 EOF");
        assert!(response.starts_with(b"HTTP/1.0 200 OK\r\n"));
    }

    fn finish(mut self) -> String {
        self.stop();
        let mut output = self.stdout.take().unwrap().join().unwrap();
        output.push_str(&self.stderr.take().unwrap().join().unwrap());
        output
    }

    fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for ServeGuard {
    fn drop(&mut self) {
        self.stop();
        for reader in [&mut self.stdout, &mut self.stderr] {
            if let Some(reader) = reader.take() {
                let _ = reader.join();
            }
        }
    }
}

#[test]
fn serve_outputs_and_access_log_do_not_contain_sensitive_data() {
    let temp = TempDir::new();
    let mut haystack = capture_commands(&temp.0);
    let server = ServeGuard::start(&temp.0);
    server.get("/screen.gif");
    server.get("/health");
    haystack.push_str(&server.finish());
    let log = fs::read_to_string(temp.0.join("logs/access.log")).unwrap();
    assert_eq!(log.lines().count(), 1, "画面请求应产生一条访问日志");
    haystack.push_str(&log);
    assert_no_sensitive_data(&haystack);
}

#[test]
#[should_panic(expected = "捕获输出包含禁用子串：ssecurity")]
fn sensitive_guard_rejects_ssecurity() {
    assert_no_sensitive_data(r#"{"ok":false,"message":"ssecurity"}"#);
}
