//! 仅连接测试自身监听的回环地址，检查实际线上字节。

use ap01_mi_cloud::transport::{HttpRequest, Transport, TransportError, UreqTransport};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

fn read_request(stream: &mut TcpStream) -> (String, Vec<(String, String)>, Vec<u8>) {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut data = Vec::new();
    while !data.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        stream.read_exact(&mut byte).unwrap();
        data.push(byte[0]);
        assert!(data.len() < 8192);
    }
    let text = String::from_utf8(data).unwrap();
    let mut lines = text.split("\r\n");
    let first = lines.next().unwrap().to_owned();
    let headers: Vec<_> = lines
        .filter(|line| !line.is_empty())
        .map(|line| {
            let (key, value) = line.split_once(':').unwrap();
            (key.to_ascii_lowercase(), value.trim().to_owned())
        })
        .collect();
    let length = headers
        .iter()
        .find(|(key, _)| key == "content-length")
        .map(|(_, value)| value.parse().unwrap())
        .unwrap_or(0);
    let mut body = vec![0; length];
    stream.read_exact(&mut body).unwrap();
    (first, headers, body)
}

fn request(url: String) -> HttpRequest {
    HttpRequest {
        method: "POST".into(),
        url,
        headers: vec![
            ("User-Agent".into(), "synthetic-agent".into()),
            ("Accept-Encoding".into(), "identity".into()),
            ("Content-Type".into(), "application/octet-stream".into()),
            ("X-Test".into(), "synthetic-value".into()),
        ],
        body: Some(b"synthetic-body".to_vec()),
        timeout: Duration::from_secs(2),
    }
}

#[test]
fn ureq_wire_headers_and_body() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let worker = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let data = read_request(&mut stream);
        stream.write_all(b"HTTP/1.1 418 Test\r\nContent-Length: 3\r\nX-Test: first\r\nX-Test: second\r\nConnection: close\r\n\r\nraw").unwrap();
        data
    });
    let req = request(format!("http://{address}/synthetic-path"));
    let response = UreqTransport::new()
        .send(&req)
        .unwrap_or_else(|_| panic!("回环请求失败"));
    assert_eq!(response.status, 418);
    assert!(response.body == b"raw");
    assert!(
        response
            .headers
            .iter()
            .filter(|(name, _)| name == "x-test")
            .map(|(_, value)| value.as_str())
            .collect::<Vec<_>>()
            == ["first", "second"]
    );
    let (line, mut headers, body) = worker.join().unwrap();
    assert_eq!(line, "POST /synthetic-path HTTP/1.1");
    assert!(Some(body) == req.body);
    assert!(!headers.iter().any(|(_, value)| value.contains("gzip")));
    let mut expected: Vec<_> = req
        .headers
        .iter()
        .map(|(k, v)| (k.to_ascii_lowercase(), v.clone()))
        .collect();
    // 客户端必须补充主机与正文长度；除此以外不允许隐式请求头。
    expected.push(("host".into(), address.to_string()));
    expected.push(("content-length".into(), "14".into()));
    headers.sort();
    expected.sort();
    assert!(headers == expected, "线上请求头集合不匹配");
}

#[test]
fn ureq_returns_redirect_without_following() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let worker = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        read_request(&mut stream);
        write!(stream, "HTTP/1.1 302 Found\r\nLocation: http://{address}/next\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        drop(stream);
        listener.set_nonblocking(true).unwrap();
        let until = Instant::now() + Duration::from_millis(150);
        while Instant::now() < until {
            assert!(
                matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
                "不应自动跟随跳转"
            );
            thread::sleep(Duration::from_millis(5));
        }
    });
    let response = UreqTransport::new()
        .send(&request(format!("http://{address}/start")))
        .unwrap_or_else(|_| panic!("回环请求失败"));
    assert_eq!(response.status, 302);
    assert!(
        response
            .headers
            .iter()
            .any(|(name, value)| name == "location" && value.ends_with("/next"))
    );
    worker.join().unwrap();
}

#[test]
fn ureq_request_timeout() {
    let transport = UreqTransport::new();
    for timeout_ms in [80, 200] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (finish, wait) = mpsc::channel();
        let worker = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            read_request(&mut stream);
            let _ = wait.recv_timeout(Duration::from_secs(2));
        });
        let mut req = request(format!("http://{address}/timeout"));
        req.timeout = Duration::from_millis(timeout_ms);
        let started = Instant::now();
        let result = transport.send(&req);
        assert!(matches!(result, Err(TransportError::Timeout)));
        assert!(started.elapsed() >= req.timeout);
        assert!(started.elapsed() < Duration::from_secs(1));
        let _ = finish.send(());
        worker.join().unwrap();
    }
}

#[test]
fn ureq_empty_get_has_only_host_header() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let worker = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_request(&mut stream);
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .unwrap();
        request
    });
    let request = HttpRequest {
        method: "GET".into(),
        url: format!("http://{address}/empty"),
        headers: vec![],
        body: None,
        timeout: Duration::from_secs(2),
    };
    assert!(UreqTransport::new().send(&request).is_ok());
    let (line, headers, body) = worker.join().unwrap();
    assert_eq!(line, "GET /empty HTTP/1.1");
    assert!(body.is_empty());
    assert!(headers == [("host".into(), address.to_string())]);
}
