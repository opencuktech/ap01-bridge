//! 裸 TCP 设备模拟器，覆盖 screen-server 的全部场景；只连接 127.0.0.1。

mod common;

use ap01_bridge_kernel::{
    gif,
    serve::{ServeConfig, Server},
    store,
};
use ap01_gif::testkit::{Frame, GifBuilder, quota_gif};
use common::{TempDir, bridge};
use serde_json::{Value, json};
use std::{
    fs,
    io::{self, BufRead, BufReader, Read, Write},
    net::{Shutdown, SocketAddr, TcpListener, TcpStream},
    path::Path,
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const GET: &[u8] = b"GET /screen.gif HTTP/1.0\r\n\r\n";
const T: u64 = 1_788_668_411;
type RawResponse = (String, Vec<(String, String)>, Vec<u8>, bool);

struct ServeGuard {
    child: Child,
    lines: Receiver<String>,
    reader: Option<JoinHandle<()>>,
    port: u16,
    first: String,
}

impl ServeGuard {
    fn new(dir: &Path) -> Self {
        Self::start(dir, true, Some(T))
    }

    fn start(dir: &Path, json_mode: bool, now: Option<u64>) -> Self {
        let mut command = bridge();
        command.arg("--data-dir").arg(dir).arg("serve");
        command.args(["--bind", "127.0.0.1", "--port", "0"]);
        if json_mode {
            command.arg("--json");
        }
        if let Some(now) = now {
            command.env("AP01_BRIDGE_FAKE_NOW", now.to_string());
        }
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut child = command.spawn().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (sender, lines) = mpsc::channel();
        // 持续排空管道，避免测试请求较多时子进程因 stdout 背压阻塞。
        let reader = thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else {
                    break;
                };
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        // 在任何断言之前建立守卫，启动失败或超时时同样回收子进程。
        let mut guard = Self {
            child,
            lines,
            reader: Some(reader),
            port: 0,
            first: String::new(),
        };
        guard.first = guard.line();
        guard.port = if json_mode {
            let first: Value = serde_json::from_str(&guard.first).unwrap();
            assert_eq!(first["event"], "listening", "服务启动输出：{}", guard.first);
            assert_eq!(first["bind"], "127.0.0.1");
            assert_eq!(first["data_dir"], dir.to_string_lossy().as_ref());
            assert_eq!(first.as_object().unwrap().len(), 4);
            u16::try_from(first["port"].as_u64().unwrap()).unwrap()
        } else {
            assert!(guard.first.starts_with("listening 127.0.0.1:"));
            assert!(
                guard
                    .first
                    .ends_with(&format!(" data_dir={}", dir.display()))
            );
            guard
                .first
                .split_whitespace()
                .nth(1)
                .unwrap()
                .parse::<SocketAddr>()
                .unwrap()
                .port()
        };
        assert!(guard.port > 0);
        guard
    }

    fn line(&self) -> String {
        self.lines
            .recv_timeout(Duration::from_secs(10))
            .expect("服务应及时输出事件行")
    }
    fn event(&self) -> Value {
        let value: Value = serde_json::from_str(&self.line()).unwrap();
        assert!(value["event"].is_string());
        value
    }
    fn connect(&self) -> TcpStream {
        let stream = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        stream
    }
    fn raw(&self, request: &[u8]) -> RawResponse {
        let mut stream = self.connect();
        stream.write_all(request).unwrap();
        let mut response = Vec::new();
        let eof = match stream.read_to_end(&mut response) {
            Ok(_) => true,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) =>
            {
                false
            }
            Err(error) => panic!("读取响应失败：{error}"),
        };
        decode(&response, eof)
    }
    fn health(&self) -> Value {
        let response = self.raw(b"GET /health HTTP/1.0\r\n\r\n");
        assert_eq!(response.0, "HTTP/1.0 200 OK");
        assert_eq!(header(&response, "Content-Type"), "application/json");
        assert_eq!(
            header(&response, "Content-Length")
                .parse::<usize>()
                .unwrap(),
            response.2.len()
        );
        assert!(response.3);
        serde_json::from_slice(&response.2).unwrap()
    }
}

impl Drop for ServeGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

fn decode(bytes: &[u8], eof: bool) -> RawResponse {
    let end = bytes
        .windows(4)
        .position(|p| p == b"\r\n\r\n")
        .expect("响应应有头结束标记");
    let text = std::str::from_utf8(&bytes[..end]).unwrap();
    let mut lines = text.split("\r\n");
    let status = lines.next().unwrap().to_owned();
    let headers = lines
        .map(|line| {
            let (k, v) = line.split_once(": ").unwrap();
            (k.into(), v.into())
        })
        .collect();
    (status, headers, bytes[end + 4..].to_vec(), eof)
}
fn header<'a>(response: &'a RawResponse, name: &str) -> &'a str {
    &response.1.iter().find(|(key, _)| key == name).unwrap().1
}
fn wire_len(response: &RawResponse) -> usize {
    response.0.len()
        + 2
        + response
            .1
            .iter()
            .map(|(k, v)| k.len() + v.len() + 4)
            .sum::<usize>()
        + 2
        + response.2.len()
}
fn assert_headers(response: &RawResponse, status: u16, content_type: &str, body_len: usize) {
    assert!(response.0.starts_with(&format!("HTTP/1.0 {status} ")));
    let mut expected = vec![
        ("Content-Type".into(), content_type.into()),
        ("Content-Length".into(), body_len.to_string()),
        ("Connection".into(), "close".into()),
        ("Cache-Control".into(), "no-store".into()),
        ("Server".into(), "ap01-bridge/0.1.0".into()),
    ];
    if status == 405 {
        expected.push(("Allow".into(), "GET, HEAD".into()));
    }
    assert_eq!(response.1, expected);
    for forbidden in [
        "Date",
        "ETag",
        "Transfer-Encoding",
        "Content-Encoding",
        "Location",
    ] {
        assert!(
            !response
                .1
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case(forbidden))
        );
    }
    assert!(response.3, "必须由服务端关闭连接");
}
fn command(dir: &Path) -> Command {
    let mut cmd = bridge();
    cmd.arg("--data-dir")
        .arg(dir)
        .env("AP01_BRIDGE_FAKE_NOW", "1000");
    cmd
}
fn publish(dir: &Path, bytes: &[u8], extra: &[&str]) -> Value {
    let mut child = command(dir)
        .args(["publish", "-", "--json"])
        .args(extra)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(bytes).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "发布结果：{output:?}");
    assert!(output.stderr.is_empty());
    serde_json::from_slice(&output.stdout).unwrap()
}
fn b() -> Vec<u8> {
    GifBuilder::default().frame(Frame::default()).build()
}
fn log(dir: &Path) -> Vec<Value> {
    fs::read_to_string(dir.join("logs/access.log"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
fn content(dir: &Path, record: &Value) -> std::path::PathBuf {
    store::content_path(dir, record["gif"].as_str().unwrap())
}

#[test]
fn minimal_tcp_exact_response_conditional_headers_lf_query_and_head() {
    let temp = TempDir::new();
    let bytes = quota_gif();
    publish(&temp.0, &bytes, &[]);
    let server = ServeGuard::new(&temp.0);
    let get = server.raw(GET);
    assert_headers(&get, 200, "image/gif", bytes.len());
    assert_eq!(get.2, bytes);
    assert_eq!(&get.2[..10], b"GIF89a\x40\x01\xf0\x00");
    assert_eq!(get.2.last(), Some(&0x3b));
    for request in [
        b"GET /screen.gif HTTP/1.0\r\nIf-None-Match: \"x\"\r\nHost: foo\r\n\r\n".as_slice(),
        b"GET /screen.gif HTTP/1.0\n\n",
        b"GET /screen.gif?ts=1 HTTP/1.0\r\n\r\n",
        b"GET /screen.gif HTTP/1.0\r\n\n",
        b"GET /screen.gif HTTP/1.0\nHost: anything\r\n\r\n",
    ] {
        assert_eq!(server.raw(request), get);
    }
    let head = server.raw(b"HEAD /screen.gif HTTP/1.0\r\n\r\n");
    assert_eq!(head.0, get.0);
    assert_eq!(head.1, get.1);
    assert_headers(&head, 200, "image/gif", bytes.len());
    assert!(head.2.is_empty());
    let version = bridge().arg("--version").output().unwrap();
    assert_eq!(String::from_utf8(version.stdout).unwrap(), "bridge 0.1.0\n");
}

#[test]
fn no_content_unknown_path_unsupported_method_garbage_and_oversized_headers() {
    let temp = TempDir::new();
    let server = ServeGuard::new(&temp.0);
    for (request, status) in [
        (GET.to_vec(), 404),
        (b"GET /foo HTTP/1.0\r\n\r\n".to_vec(), 404),
        (b"POST /screen.gif HTTP/1.0\r\n\r\n".to_vec(), 405),
        (b"garbage\r\n\r\n".to_vec(), 400),
        (vec![b'x'; 16384], 400),
    ] {
        let response = server.raw(&request);
        assert_headers(&response, status, "text/plain", response.2.len());
        assert!(!std::str::from_utf8(&response.2).unwrap().is_ascii());
        let event = server.event();
        assert_eq!(event["status"], status);
        assert!(event["gif"].is_null());
        assert_eq!(event["bytes"], wire_len(&response));
    }
    let get = server.raw(GET);
    let head = server.raw(b"HEAD /screen.gif HTTP/1.0\n\n");
    assert_eq!(head.1, get.1);
    assert!(head.2.is_empty());
    let health = server.health();
    assert_eq!(health["requests"]["screen_total"], 3);
    assert_eq!(health["requests"]["screen_last_status"], 404);
    assert_eq!(log(&temp.0).len(), 7);
}

#[test]
fn publish_immediately_replaces_cache_missing_file_uses_cache_and_new_record_wins() {
    let temp = TempDir::new();
    let first = publish(&temp.0, &quota_gif(), &[]);
    let server = ServeGuard::new(&temp.0);
    assert_eq!(server.raw(GET).2, quota_gif());
    fs::remove_file(content(&temp.0, &first)).unwrap();
    assert_eq!(server.raw(GET).2, quota_gif());
    let next = publish(&temp.0, &b(), &[]);
    assert_ne!(first["gif"], next["gif"]);
    assert_eq!(server.raw(GET).2, b());
    assert_eq!(server.health()["current"]["gif"], next["gif"]);
}

#[test]
fn two_missing_reads_keep_last_success_and_health_exposes_disk_failure() {
    let temp = TempDir::new();
    let first = publish(&temp.0, &quota_gif(), &[]);
    let server = ServeGuard::new(&temp.0);
    server.raw(GET);
    server.event();
    let mut record: Value =
        serde_json::from_slice(&fs::read(temp.0.join("current.json")).unwrap()).unwrap();
    record["gif"] = json!("a".repeat(64));
    store::atomic::write_file(
        &temp.0.join("current.json"),
        &serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
    let response = server.raw(GET);
    assert_headers(&response, 200, "image/gif", quota_gif().len());
    assert_eq!(response.2, quota_gif());
    let event = server.event();
    assert_eq!(event["serving"], "none");
    assert_eq!(event["gif"], first["gif"]);
    let health = server.health();
    assert_eq!(health["serving"], "none");
    assert_eq!(health["ok"], false);
    assert!(health["error"].is_string());
    publish(&temp.0, &b(), &[]);
    assert_eq!(server.raw(GET).2, b());
}

#[test]
fn expired_current_uses_fallback_and_missing_fallback_file_or_pointer_returns_404() {
    for missing in ["available", "content", "pointer", "corrupt"] {
        let temp = TempDir::new();
        let fallback = store::set_fallback(&temp.0, "boot", &quota_gif(), 1000).unwrap();
        publish(&temp.0, &b(), &["--ttl", "420", "--fallback", "boot"]);
        match missing {
            "content" => fs::remove_file(store::content_path(&temp.0, &fallback.gif)).unwrap(),
            "pointer" => fs::remove_file(temp.0.join("fallbacks/boot.json")).unwrap(),
            "corrupt" => fs::write(temp.0.join("fallbacks/boot.json"), b"{").unwrap(),
            _ => (),
        }
        let server = ServeGuard::start(&temp.0, true, None);
        let response = server.raw(GET);
        if missing == "available" {
            assert_headers(&response, 200, "image/gif", quota_gif().len());
            assert_eq!(response.2, quota_gif());
        } else {
            assert_headers(&response, 404, "text/plain", response.2.len());
        }
        let health = server.health();
        assert_eq!(
            health["serving"],
            if missing == "available" {
                "fallback"
            } else {
                "none"
            }
        );
        assert_eq!(health["fallback"]["missing"], missing != "available");
        assert_eq!(health["current"]["expired"], true);
        assert_eq!(health["error"], Value::Null);
        assert!(health["now"].as_u64().unwrap() >= 1420);
        if missing == "available" {
            assert_eq!(
                health["fallback"],
                json!({"name":"boot", "gif":fallback.gif,"bytes":quota_gif().len(),"missing":false})
            );
        }
    }
}

#[test]
fn expired_without_fallback_never_serves_previous_cached_content() {
    let temp = TempDir::new();
    publish(&temp.0, &quota_gif(), &[]);
    let server = ServeGuard::start(&temp.0, true, None);
    assert_eq!(server.raw(GET).2, quota_gif());
    publish(&temp.0, &quota_gif(), &["--ttl", "420"]);
    let response = server.raw(GET);
    assert_headers(&response, 404, "text/plain", response.2.len());
    assert_eq!(server.health()["serving"], "none");
}

#[test]
fn health_has_exact_types_counts_three_requests_and_restart_resets_memory() {
    let temp = TempDir::new();
    let published = publish(&temp.0, &quota_gif(), &[]);
    let server = ServeGuard::new(&temp.0);
    let health = server.health();
    assert_eq!(health.as_object().unwrap().len(), 10);
    assert_eq!(health["ok"], true);
    assert_eq!(health["serving"], "current");
    assert_eq!(health["now"], T);
    assert_eq!(health["uptime_seconds"], 0);
    assert_eq!(health["version"], "0.1.0");
    assert_eq!(health["data_dir"], temp.0.to_string_lossy().as_ref());
    assert_eq!(
        health["current"],
        json!({"gif":published["gif"],"bytes":quota_gif().len(),"published_at":1000,"ttl_seconds":null,"expires_at":null,"expired":false})
    );
    assert!(health["fallback"].is_null());
    assert!(health["error"].is_null());
    let empty = json!({"screen_total":0,"screen_last_at":null,"screen_last_status":null,"screen_last_client":null});
    assert_eq!(health["requests"], empty);
    server.raw(GET);
    server.raw(b"HEAD /screen.gif HTTP/1.0\n\n");
    server.raw(b"GET /screen.gif?ts=1 HTTP/1.1\n\n");
    let counted = server.health();
    assert_eq!(
        counted["requests"],
        json!({"screen_total":3,"screen_last_at":T,"screen_last_status":200,"screen_last_client":"127.0.0.1"})
    );
    let before = log(&temp.0);
    assert_eq!(before.len(), 3);
    let head = server.raw(b"HEAD /health HTTP/1.0\r\n\r\n");
    assert!(head.2.is_empty());
    assert_eq!(
        header(&head, "Content-Length").parse::<usize>().unwrap(),
        serde_json::to_vec(&counted).unwrap().len()
    );
    for _ in 0..3 {
        server.health();
    }
    assert_eq!(log(&temp.0), before);
    drop(server);
    let restarted = ServeGuard::new(&temp.0);
    assert_eq!(restarted.health()["requests"], empty);
    assert_eq!(log(&temp.0), before);
}

#[test]
fn empty_and_corrupt_health_remain_200_with_chinese_error() {
    let temp = TempDir::new();
    let server = ServeGuard::new(&temp.0);
    for corrupt in [false, true] {
        if corrupt {
            fs::write(temp.0.join("current.json"), b"{").unwrap();
        }
        let health = server.health();
        assert_eq!(health["ok"], false);
        assert_eq!(health["serving"], "none");
        assert!(health["current"].is_null());
        assert!(health["fallback"].is_null());
        assert!(!health["error"].as_str().unwrap().is_ascii());
        assert!(health["now"].is_u64());
        assert!(health["uptime_seconds"].is_u64());
    }
    assert!(fs::read(temp.0.join("logs/access.log")).unwrap().is_empty());
}

#[test]
fn json_stdout_and_access_log_have_one_complete_event_per_non_health_request() {
    let temp = TempDir::new();
    let published = publish(&temp.0, &quota_gif(), &[]);
    let server = ServeGuard::new(&temp.0);
    assert!(temp.0.join("logs").is_dir());
    assert!(fs::read(temp.0.join("logs/access.log")).unwrap().is_empty());
    let response = server.raw(GET);
    let event = server.event();
    assert_eq!(
        event,
        json!({"event":"request", "ts":T, "client":"127.0.0.1", "method":"GET", "path":"/screen.gif", "version":"HTTP/1.0", "status":200, "bytes":wire_len(&response), "serving":"current", "gif":published["gif"]})
    );
    assert_eq!(log(&temp.0), vec![event.clone()]);
    server.health();
    server.raw(b"GET /foo HTTP/1.1\n\n");
    let other = server.event();
    assert_eq!(other["path"], "/foo");
    assert_eq!(other["version"], "HTTP/1.1");
    assert_eq!(other["status"], 404);
    assert!(other["gif"].is_null());
    let post = server.raw(b"POST /health?probe=1 HTTP/1.0\r\n\r\n");
    assert_headers(&post, 405, "text/plain", post.2.len());
    let rejected_health = server.event();
    assert_eq!(rejected_health["event"], "request");
    assert_eq!(rejected_health["method"], "POST");
    assert_eq!(rejected_health["path"], "/health");
    assert_eq!(rejected_health["status"], 405);
    assert_eq!(rejected_health["bytes"], wire_len(&post));
    assert_eq!(log(&temp.0), vec![event, other, rejected_health]);
    assert!(matches!(
        server.lines.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    assert!(
        log(&temp.0)
            .iter()
            .all(|event| event["event"] != "listening")
    );
}

#[test]
fn human_stdout_uses_utc_timestamp_client_and_original_request_version() {
    let temp = TempDir::new();
    publish(&temp.0, &b(), &[]);
    let server = ServeGuard::start(&temp.0, false, Some(T));
    server.raw(GET);
    let line = server.line();
    assert!(line.starts_with("2026-09-06T04:20:11Z 127.0.0.1 "));
    assert!(line.contains("\"GET /screen.gif HTTP/1.0\" 200"));
    server.raw(b"GET /screen.gif HTTP/1.1\n\n");
    assert!(server.line().contains("\"GET /screen.gif HTTP/1.1\" 200"));
    assert_eq!(log(&temp.0).len(), 2);
}

#[test]
fn early_disconnect_logs_actual_bytes_and_server_keeps_serving() {
    let temp = TempDir::new();
    let bytes = GifBuilder::default()
        .frame(Frame {
            data_bytes: 250_000,
            ..Frame::default()
        })
        .build();
    publish(&temp.0, &bytes, &[]);
    let server = ServeGuard::new(&temp.0);
    let full = server.raw(GET);
    server.event();
    let mut stream = server.connect();
    stream.write_all(GET).unwrap();
    stream.read_exact(&mut [0; 10]).unwrap();
    let _ = stream.shutdown(Shutdown::Both);
    drop(stream);
    let early = server.event();
    assert_eq!(early["event"], "request");
    assert_eq!(early["status"], 200);
    assert!(early["bytes"].as_u64().unwrap() <= wire_len(&full) as u64);
    assert!(early["bytes"].as_u64().unwrap() >= 10);
    assert_eq!(server.raw(GET), full);
    let entries = log(&temp.0);
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[1], early);
}

#[test]
fn slow_client_times_out_after_five_seconds_without_blocking_other_connections() {
    let temp = TempDir::new();
    publish(&temp.0, &b(), &[]);
    let server = ServeGuard::new(&temp.0);
    let mut slow = server.connect();
    let started = Instant::now();
    let quick_started = Instant::now();
    assert_eq!(server.raw(GET).2, b());
    assert!(quick_started.elapsed() < Duration::from_secs(3));
    let mut output = Vec::new();
    let result = slow.read_to_end(&mut output);
    assert!(
        result.is_ok()
            || matches!(
                result.as_ref().unwrap_err().kind(),
                std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
            )
    );
    assert!(started.elapsed() >= Duration::from_millis(4500));
    assert!(started.elapsed() < Duration::from_secs(9));
    if !output.is_empty() {
        assert!(
            decode(&output, result.is_ok())
                .0
                .starts_with("HTTP/1.0 400")
        );
    }
    assert_eq!(server.raw(GET).2, b());
}

#[test]
fn drip_client_deadline_closes_connection_while_normal_get_succeeds() {
    let temp = TempDir::new();
    publish(&temp.0, &b(), &[]);
    let server = ServeGuard::new(&temp.0);
    let mut drip = server.connect();
    let started = Instant::now();
    let mut writer = drip.try_clone().unwrap();
    let (stop, stopped) = mpsc::channel();
    let sender = thread::spawn(move || {
        // 请求永远不完整，每秒一个字节足以绕过旧版单次读取超时。
        for byte in GET {
            if writer.write_all(&[*byte]).is_err() {
                break;
            }
            match stopped.recv_timeout(Duration::from_secs(1)) {
                Err(mpsc::RecvTimeoutError::Timeout) => (),
                _ => break,
            }
        }
    });
    let quick_started = Instant::now();
    let normal = server.raw(GET);
    let quick_elapsed = quick_started.elapsed();
    let mut output = Vec::new();
    let result = drip.read_to_end(&mut output);
    let elapsed = started.elapsed();
    let _ = stop.send(());
    sender.join().unwrap();
    assert_headers(&normal, 200, "image/gif", b().len());
    assert_eq!(normal.2, b());
    assert!(
        quick_elapsed < Duration::from_secs(6),
        "正常请求耗时：{quick_elapsed:?}"
    );
    assert!(
        result.is_ok()
            || matches!(
                result.as_ref().unwrap_err().kind(),
                io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted
            ),
        "滴流连接必须关闭：{result:?}"
    );
    // 为操作系统调度留出一秒容差，不能因持续收到字节而延长截止时间。
    assert!(
        elapsed >= Duration::from_millis(4500),
        "滴流连接过早关闭：{elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(6),
        "滴流连接未及时关闭：{elapsed:?}"
    );
    assert!(output.is_empty(), "超时连接应直接关闭，不写响应");
    assert_eq!(server.raw(GET).2, b());
}

#[test]
fn port_zero_kernel_binding_is_connectable_and_logs_directory_is_created() {
    let temp = TempDir::new();
    let server = Server::bind(
        "127.0.0.1:0".parse().unwrap(),
        ServeConfig {
            data_dir: temp.0.join("new/nested"),
            version: "0.1.0".into(),
        },
    )
    .unwrap();
    let addr = server.local_addr().unwrap();
    assert!(addr.port() > 0);
    let _stream = TcpStream::connect(addr).unwrap();
    assert!(temp.0.join("new/nested/logs").is_dir());
    assert!(
        fs::read(temp.0.join("new/nested/logs/access.log"))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn occupied_port_returns_exit_five_in_json_and_chinese_stderr_in_human_mode() {
    let temp = TempDir::new();
    let occupied = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = occupied.local_addr().unwrap().port();
    for json_mode in [true, false] {
        let mut cmd = command(&temp.0);
        cmd.args(["serve", "--bind", "127.0.0.1", "--port", &port.to_string()]);
        if json_mode {
            cmd.arg("--json");
        }
        let output = cmd.output().unwrap();
        assert_eq!(output.status.code(), Some(5));
        let message = if json_mode {
            assert!(output.stderr.is_empty());
            let value: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(value["ok"], false);
            assert_eq!(value["error"]["code"], 5);
            assert_eq!(String::from_utf8_lossy(&output.stdout).lines().count(), 1);
            value["error"]["message"].as_str().unwrap().to_owned()
        } else {
            assert!(output.stdout.is_empty());
            String::from_utf8(output.stderr).unwrap()
        };
        assert!(message.contains("无法绑定监听地址"));
        assert!(message.contains(&format!("127.0.0.1:{port}")));
    }
}

#[test]
fn directory_resolution_and_log_creation_failures_use_state_code() {
    let output = bridge()
        .args(["serve", "--bind", "127.0.0.1", "--port", "0", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(4));
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["error"]["code"], 4);
    let temp = TempDir::new();
    fs::write(temp.0.join("logs"), b"x").unwrap();
    let output = command(&temp.0)
        .args(["serve", "--bind", "127.0.0.1", "--port", "0", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(4));
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["error"]["code"], 4);
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("无法创建日志目录")
    );
}

#[test]
fn unopenable_access_log_fails_startup_with_state_code_and_no_listening_event() {
    let temp = TempDir::new();
    publish(&temp.0, &b(), &[]);
    fs::create_dir(temp.0.join("logs/access.log")).unwrap();
    let output = command(&temp.0)
        .args(["serve", "--bind", "127.0.0.1", "--port", "0", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(4));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(stdout.lines().count(), 1);
    let value: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], 4);
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("无法打开访问日志文件")
    );
    assert!(!stdout.contains("listening"));
}

#[test]
fn log_write_failure_after_startup_is_ignored_and_next_request_still_succeeds() {
    let temp = TempDir::new();
    publish(&temp.0, &b(), &[]);
    let server = ServeGuard::new(&temp.0);
    // 启动时已成功打开过日志文件；之后把它换成同名目录，写入失败只能被忽略。
    fs::remove_file(temp.0.join("logs/access.log")).unwrap();
    fs::create_dir(temp.0.join("logs/access.log")).unwrap();
    for _ in 0..2 {
        assert_eq!(server.raw(GET).2, b());
        assert_eq!(server.event()["status"], 200);
    }
    assert!(temp.0.join("logs/access.log").is_dir());
}

#[test]
fn wrong_length_content_is_unavailable_and_missing_current_can_use_valid_fallback() {
    let temp = TempDir::new();
    let published = publish(&temp.0, &b(), &[]);
    fs::write(content(&temp.0, &published), b"x").unwrap();
    let server = ServeGuard::new(&temp.0);
    assert!(server.raw(GET).0.starts_with("HTTP/1.0 404"));
    let fallback = store::set_fallback(&temp.0, "boot", &quota_gif(), 1000).unwrap();
    let mut record: Value =
        serde_json::from_slice(&fs::read(temp.0.join("current.json")).unwrap()).unwrap();
    record["fallback"] = json!("boot");
    store::atomic::write_file(
        &temp.0.join("current.json"),
        &serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
    assert_eq!(server.raw(GET).2, quota_gif());
    let health = server.health();
    assert_eq!(health["serving"], "fallback");
    assert_eq!(health["ok"], false);
    assert_eq!(health["fallback"]["gif"], fallback.gif);
    assert!(health["error"].is_string());
    assert_eq!(gif::validate(&quota_gif()).sha256, fallback.gif);
}
