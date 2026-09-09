//! 注入读取结果，确定性覆盖文件回收竞争与内存边界。

use super::*;

#[test]
fn oversized_headers_receive_bad_request_and_eof_without_reset() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let mut client = TcpStream::connect(addr).unwrap();
    client
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    // 在服务端开始解析前发送完整的超长头，确保解析停止后仍有未读数据。
    client.write_all(&[b'x'; 16384]).unwrap();
    let (stream, _) = listener.accept().unwrap();
    let accepted_at = Instant::now();
    let worker = thread::spawn(move || {
        let config = ServeConfig {
            data_dir: std::env::temp_dir().join(format!(
                "ap01-oversized-headers-{}-{}",
                std::process::id(),
                addr.port()
            )),
            version: "0.1.0".into(),
        };
        let clock: Clock = Arc::new(|| 1000);
        let sink: EventSink = Arc::new(Mutex::new(|_: &Event| {}));
        handle(
            stream,
            accepted_at,
            &config,
            &ServeState::new(1000),
            &clock,
            &sink,
        );
    });
    let mut response = Vec::new();
    let result = client.read_to_end(&mut response);
    // 读到服务端 FIN 后关闭客户端，让服务端排空读取获得 EOF，再等待工作线程。
    drop(client);
    worker.join().unwrap();
    result.expect("超长请求头必须收到响应和 EOF，不能发生连接重置");
    assert!(response.starts_with(b"HTTP/1.0 400"));
}

fn current(hash: &str, bytes: u64, expired: bool, missing: bool) -> ServingStatus {
    ServingStatus {
        serving: if missing || expired {
            Serving::None
        } else {
            Serving::Current
        },
        current: Some(CurrentStatus {
            gif: hash.into(),
            bytes,
            published_at: 1000,
            ttl_seconds: Some(420),
            expires_at: Some(1420),
            expired,
        }),
        fallback: None,
        error: missing.then(|| "当前内容文件缺失".into()),
    }
}

#[test]
fn missing_reference_rereads_new_record_and_sends_new_bytes() {
    let state = ServeState::new(0);
    let mut status = current("a", 1, false, false);
    let mut reads = Vec::new();
    let mut resolutions = 0;
    let snapshot = state
        .screen(
            &mut status,
            || {
                resolutions += 1;
                current("b", 2, false, false)
            },
            |hash| {
                reads.push(hash.to_owned());
                match hash {
                    "a" => Err(io::ErrorKind::NotFound.into()),
                    "b" => Ok(vec![2, 3]),
                    _ => panic!("意外读取"),
                }
            },
        )
        .unwrap();
    assert_eq!(resolutions, 1);
    assert_eq!(reads, ["a", "b"]);
    assert_eq!(snapshot.0, "b");
    assert_eq!(*snapshot.1, [2, 3]);
    assert_eq!(status.serving_gif(), Some("b"));
}

#[test]
fn two_missing_reads_reuse_successful_snapshot_and_keep_disk_status() {
    let state = ServeState::new(0);
    state
        .screen(
            &mut current("a", 1, false, false),
            || panic!("不应重读"),
            |_| Ok(vec![1]),
        )
        .unwrap();
    let mut status = current("b", 1, false, true);
    let mut reads = 0;
    let snapshot = state
        .screen(
            &mut status,
            || current("b", 1, false, true),
            |_| {
                reads += 1;
                Err(io::ErrorKind::NotFound.into())
            },
        )
        .unwrap();
    // 当前内容不可用时第一轮不读文件，直接重读记录；重读后仍缺失才尝试读取一次。
    assert_eq!(reads, 1);
    assert_eq!(snapshot.0, "a");
    assert_eq!(*snapshot.1, [1]);
    assert_eq!(status.serving, Serving::None);
    assert!(status.error.is_some());
}

fn fallback_available(hash: &str) -> ServingStatus {
    let mut status = current(hash, 1, false, true);
    status.serving = Serving::Fallback;
    status.fallback = Some(FallbackStatus {
        name: "boot".into(),
        gif: Some("f".into()),
        bytes: Some(1),
        missing: false,
    });
    status
}

#[test]
fn unavailable_current_rereads_before_serving_available_fallback() {
    let state = ServeState::new(0);
    let mut status = fallback_available("a");
    let mut reads = Vec::new();
    let mut resolutions = 0;
    let snapshot = state
        .screen(
            &mut status,
            || {
                resolutions += 1;
                current("b", 2, false, false)
            },
            |hash| {
                reads.push(hash.to_owned());
                match hash {
                    "b" => Ok(vec![2, 3]),
                    _ => panic!("重读之前不应读取回退或旧内容"),
                }
            },
        )
        .unwrap();
    assert_eq!(resolutions, 1);
    assert_eq!(reads, ["b"]);
    assert_eq!(snapshot.0, "b");
    assert_eq!(*snapshot.1, [2, 3]);
    assert_eq!(status.serving_gif(), Some("b"));
    // 重读后当前内容仍不可用，才按新状态供应回退。
    let mut status = fallback_available("a");
    let snapshot = state
        .screen(
            &mut status,
            || fallback_available("a"),
            |hash| match hash {
                "f" => Ok(vec![9]),
                _ => Err(io::ErrorKind::NotFound.into()),
            },
        )
        .unwrap();
    assert_eq!(snapshot.0, "f");
    assert_eq!(status.serving, Serving::Fallback);
}

#[test]
fn cache_hits_skip_reads_and_third_hash_clears_cache() {
    let state = ServeState::new(0);
    for hash in ["a", "b", "c"] {
        let first = state
            .screen(
                &mut current(hash, 1, false, false),
                || panic!("不应重读"),
                |_| Ok(vec![1]),
            )
            .unwrap();
        // 当前内容不可用会先重读记录；记录未变时仍按哈希命中缓存，不读内容文件。
        let second = state
            .screen(
                &mut current(hash, 1, false, true),
                || current(hash, 1, false, true),
                |_| panic!("缓存命中不应读取内容"),
            )
            .unwrap();
        assert!(Arc::ptr_eq(&first.1, &second.1));
        assert!(
            state
                .cache
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .len()
                <= 2
        );
    }
    assert_eq!(
        state
            .cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .len(),
        1
    );
    assert!(
        state
            .cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .contains_key("c")
    );
}

#[test]
fn mismatched_lengths_retry_and_cannot_populate_cache() {
    let state = ServeState::new(0);
    let mut reads = 0;
    assert!(
        state
            .screen(
                &mut current("a", 2, false, false),
                || current("a", 2, false, true),
                |_| {
                    reads += 1;
                    Ok(vec![1])
                }
            )
            .is_none()
    );
    assert_eq!(reads, 2);
    assert!(
        state
            .cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_empty()
    );
}

#[test]
fn expired_without_available_fallback_never_uses_old_snapshot_even_after_retry() {
    let state = ServeState::new(0);
    state
        .screen(
            &mut current("a", 1, false, false),
            || panic!("不应重读"),
            |_| Ok(vec![1]),
        )
        .unwrap();
    for after_retry in [false, true] {
        let mut status = current("b", 1, !after_retry, after_retry);
        assert!(
            state
                .screen(
                    &mut status,
                    || current("b", 1, true, false),
                    |_| Err(io::ErrorKind::NotFound.into())
                )
                .is_none()
        );
    }
    let mut status = current("b", 1, true, false);
    status.fallback = Some(FallbackStatus {
        name: "boot".into(),
        gif: Some("a".into()),
        bytes: Some(1),
        missing: true,
    });
    assert!(
        state
            .screen(
                &mut status,
                || panic!("不应重读"),
                |_| panic!("过期且回退缺失不应读取缓存")
            )
            .is_none()
    );
}

#[test]
fn expired_with_unreadable_fallback_never_reuses_old_snapshot() {
    let state = ServeState::new(0);
    state
        .screen(
            &mut current("a", 1, false, false),
            || panic!("不应重读"),
            |_| Ok(vec![1]),
        )
        .unwrap();
    // 回退通过了存在性与长度检查（missing 为 false），但真正读取时失败。
    let expired_fallback = || {
        let mut status = current("a", 1, true, false);
        status.serving = Serving::Fallback;
        status.fallback = Some(FallbackStatus {
            name: "boot".into(),
            gif: Some("f".into()),
            bytes: Some(1),
            missing: false,
        });
        status
    };
    let mut status = expired_fallback();
    let mut reads = 0;
    assert!(
        state
            .screen(&mut status, expired_fallback, |hash| {
                assert_eq!(hash, "f", "过期后不应再读取当前内容");
                reads += 1;
                Err(io::ErrorKind::PermissionDenied.into())
            })
            .is_none()
    );
    assert_eq!(reads, 2);
    assert_eq!(status.serving, Serving::Fallback);
}

#[test]
fn health_order_and_clock_rollback_and_disk_error_with_fallback() {
    let state = ServeState::new(2000);
    let mut status = current("a", 1, false, true);
    status.serving = Serving::Fallback;
    status.fallback = Some(FallbackStatus {
        name: "boot".into(),
        gif: Some("b".into()),
        bytes: Some(2),
        missing: false,
    });
    let response = state.health(
        &ServeConfig {
            data_dir: PathBuf::from("data"),
            version: "0.1.0".into(),
        },
        &status,
        1000,
    );
    let encoded = String::from_utf8(response.body.as_ref().clone()).unwrap();
    let fields = [
        "ok",
        "serving",
        "now",
        "uptime_seconds",
        "version",
        "data_dir",
        "current",
        "fallback",
        "error",
        "requests",
    ];
    let positions: Vec<_> = fields
        .iter()
        .map(|field| encoded.find(&format!("\"{field}\":")).unwrap())
        .collect();
    assert!(positions.windows(2).all(|p| p[0] < p[1]));
    let health: serde_json::Value = serde_json::from_str(&encoded).unwrap();
    assert_eq!(health["ok"], false);
    assert_eq!(health["serving"], "fallback");
    assert_eq!(health["uptime_seconds"], 0);
    assert!(encoded.contains("\"requests\":{\"screen_total\":0,\"screen_last_at\":null,\"screen_last_status\":null,\"screen_last_client\":null}"));
}

#[test]
fn resource_exhaustion_is_retried_but_invalid_listener_is_fatal() {
    for code in [23, 24, 10024] {
        assert!(!fatal_accept_error(&io::Error::from_raw_os_error(code)));
    }
    assert!(!fatal_accept_error(&io::ErrorKind::Interrupted.into()));
    assert!(fatal_accept_error(&io::ErrorKind::InvalidInput.into()));
}

#[test]
fn poisoned_cache_after_read_panic_can_serve_next_request() {
    let state = ServeState::new(0);
    let failed = catch_unwind(AssertUnwindSafe(|| {
        state.screen(
            &mut current("a", 1, false, false),
            || panic!("不应重读"),
            |_| panic!("模拟持有缓存锁时发生读取异常"),
        )
    }));
    assert!(failed.is_err());
    assert!(state.cache.is_poisoned());
    let snapshot = state
        .screen(
            &mut current("b", 1, false, false),
            || panic!("不应重读"),
            |_| Ok(vec![2]),
        )
        .unwrap();
    assert_eq!(snapshot.0, "b");
    assert_eq!(*snapshot.1, [2]);
}
