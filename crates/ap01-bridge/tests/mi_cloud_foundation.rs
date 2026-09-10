//! 在应用 crate 验证测试工具可复用，不增加任何命令行子命令。

use ap01_mi_cloud::{
    testkit::FakeTransport,
    transport::{HttpRequest, HttpResponse, Transport, TransportError},
};
use std::time::Duration;

#[test]
fn fake_transport_records_requests_and_script_order() {
    let response = HttpResponse {
        status: 201,
        headers: vec![
            ("X-Test".into(), "first".into()),
            ("X-Test".into(), "second".into()),
        ],
        body: b"synthetic-response".to_vec(),
    };
    let fake = FakeTransport::new([Ok(response.clone()), Err(TransportError::Timeout)]);
    let first = HttpRequest {
        method: "GET".into(),
        url: "/synthetic-first".into(),
        headers: vec![("X-Test".into(), "synthetic-first".into())],
        body: None,
        timeout: Duration::from_secs(1),
    };
    let second = HttpRequest {
        method: "POST".into(),
        url: "/synthetic-second".into(),
        headers: vec![
            ("X-Test".into(), "synthetic-second".into()),
            ("X-Order".into(), "last".into()),
        ],
        body: Some(b"synthetic-request".to_vec()),
        timeout: Duration::from_secs(2),
    };
    assert!(fake.send(&first).is_ok_and(|actual| actual == response));
    assert!(matches!(fake.send(&second), Err(TransportError::Timeout)));
    assert!(fake.requests() == [first.clone(), second]);
    assert!(matches!(
        fake.send(&first),
        Err(TransportError::ScriptExhausted)
    ));
    assert_eq!(fake.requests().len(), 3);
}
