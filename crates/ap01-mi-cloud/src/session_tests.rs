//! 登录边界完全通过假传输验证，不打开连接。
use super::*;
use crate::{
    testkit::{FakeTransport, fixtures::*},
    transport::TransportError,
};

#[test]
fn login_success_headers_and_cookie_chain() {
    let fake = FakeTransport::new([
        Ok(auth(SECURE_STS)),
        Ok(response(
            302,
            "",
            &[
                ("Set-Cookie", "serviceToken=synthetic-old; Path=/"),
                (
                    "Set-Cookie",
                    "tracking.invalid=synthetic value, ignored; Path=/",
                ),
                ("Set-Cookie", "malformed"),
                ("Set-Cookie", "extra_invalid=synthetic value, ignored"),
                ("Set-Cookie", "extra=synthetic-extra"),
                ("Location", SECURE_STS),
            ],
        )),
        Ok(response(
            200,
            "",
            &[
                ("set-cookie", "serviceToken=synthetic-middle"),
                ("SET-COOKIE", "serviceToken=synthetic-last; Path=/"),
            ],
        )),
    ]);
    let session = Session::login(&fake, &credentials(), &Endpoints::default()).unwrap();
    assert_eq!(session.service_token, "synthetic-last");
    let requests = fake.requests();
    assert_eq!(requests.len(), 3);
    let initial = cookie_header(&[
        ("userId".into(), credentials().user_id),
        ("passToken".into(), credentials().pass_token),
        ("deviceId".into(), credentials().device_id),
    ]);
    assert_eq!(requests[0].url, format!("{ACCOUNT_BASE}{LOGIN_PATH}"));
    for request in &requests {
        assert_eq!(request.method, "GET");
        assert_eq!(request.timeout, Duration::from_secs(20));
        assert!(request.body.is_none());
        assert_eq!(
            request.headers[0],
            ("User-Agent".into(), LOGIN_AGENT.into())
        );
        assert_eq!(request.headers.len(), 2);
    }
    assert_eq!(requests[0].headers[1], ("Cookie".into(), initial.clone()));
    assert_eq!(requests[1].headers[1].1, initial);
    assert_eq!(
        requests[2].headers[1].1,
        format!("{initial}; serviceToken=synthetic-old; extra=synthetic-extra")
    );
}

#[test]
fn login_non_success_status() {
    for responses in [
        vec![Ok(response(403, "私有响应", &[]))],
        vec![Ok(auth(SECURE_STS)), Ok(response(500, "私有响应", &[]))],
    ] {
        assert_runtime(
            Session::login(
                &FakeTransport::new(responses),
                &credentials(),
                &Endpoints::default(),
            )
            .unwrap_err(),
        );
    }
}
#[test]
fn login_rejected_code_omits_location() {
    let body =
        serde_json::json!({"code": -7, "location": SECURE_STS, "message":"私有响应"}).to_string();
    let fake = FakeTransport::new([Ok(response(200, &body, &[]))]);
    let error = Session::login(&fake, &credentials(), &Endpoints::default()).unwrap_err();
    assert!(error.message().contains("-7"));
    assert!(!error.message().contains("私有响应"));
    assert_eq!(fake.requests().len(), 1);
    assert_runtime(error);
}
#[test]
fn login_empty_location() {
    let error = Session::login(
        &FakeTransport::new([Ok(auth(""))]),
        &credentials(),
        &Endpoints::default(),
    )
    .unwrap_err();
    assert_eq!(error.message(), "登录响应缺少跳转地址，状态码 0");
    assert_runtime(error);
}

#[test]
fn login_missing_ssecurity_includes_code() {
    for key in [None, Some("")] {
        let mut body = serde_json::json!({"code":0,"location":SECURE_STS});
        if let Some(key) = key {
            body["ssecurity"] = key.into();
        }
        let error = Session::login(
            &FakeTransport::new([Ok(response(200, &body.to_string(), &[]))]),
            &credentials(),
            &Endpoints::default(),
        )
        .unwrap_err();
        assert_eq!(error.message(), "登录响应缺少会话密钥，状态码 0");
        assert_runtime(error);
    }
}

#[test]
fn login_relative_locations_use_previous_origin() {
    let fake = FakeTransport::new([
        Ok(auth("/sts")),
        Ok(response(
            302,
            "",
            &[("Location", "https://example.invalid:8443/next")],
        )),
        Ok(response(302, "", &[("Location", "/finish?step=2")])),
        Ok(response(
            200,
            "",
            &[("Set-Cookie", "serviceToken=synthetic-token")],
        )),
    ]);
    Session::login(&fake, &credentials(), &Endpoints::default()).unwrap();
    let requests = fake.requests();
    assert_eq!(requests[1].url, "https://account.xiaomi.com/sts");
    assert_eq!(
        requests[3].url,
        "https://example.invalid:8443/finish?step=2"
    );

    let fake = FakeTransport::new([
        Ok(auth(SECURE_STS)),
        Ok(response(302, "", &[("Location", "/invalid path")])),
    ]);
    assert_runtime(Session::login(&fake, &credentials(), &Endpoints::default()).unwrap_err());
    assert_eq!(fake.requests().len(), 2);
}

#[test]
fn login_invalid_service_token_is_rejected() {
    for value in ["synthetic value, invalid", "synthetic\r\ninvalid"] {
        let fake = FakeTransport::new([
            Ok(auth(SECURE_STS)),
            Ok(response(
                200,
                "",
                &[("Set-Cookie", &format!("serviceToken={value}"))],
            )),
        ]);
        let error = Session::login(&fake, &credentials(), &Endpoints::default()).unwrap_err();
        assert_eq!(error.message(), "登录响应会话字段无效");
        assert_runtime(error);
    }
}

#[test]
fn login_deleted_service_token_is_reissued() {
    let mut first = auth(SECURE_STS);
    first.headers = vec![("Set-Cookie".into(), "serviceToken=test-old".into())];
    let deletion = [
        ("Set-Cookie", "serviceToken=; Max-Age=0"),
        ("Location", SECURE_STS),
    ];
    let reissued = [("Set-Cookie", "serviceToken=synthetic-new")];
    let fake = FakeTransport::new([
        Ok(first),
        Ok(response(302, "", &deletion)),
        Ok(response(200, "", &reissued)),
    ]);
    let session = Session::login(&fake, &credentials(), &Endpoints::default()).unwrap();
    assert!(session.service_token == "synthetic-new");
    let requests = fake.requests();
    assert_eq!(requests.len(), 3);
    assert!(requests[1].headers[1].1.contains("serviceToken="));
    assert_eq!(requests[2].headers[1].0, "Cookie");
    assert!(!requests[2].headers[1].1.contains("serviceToken="));
}

#[test]
fn login_final_service_token_deletion_fails() {
    let mut first = auth(SECURE_STS);
    first.headers = vec![("Set-Cookie".into(), "serviceToken=test-old".into())];
    let deletion = [("Set-Cookie", "serviceToken=; Max-Age=0")];
    let fake = FakeTransport::new([Ok(first), Ok(response(200, "", &deletion))]);
    let error = Session::login(&fake, &credentials(), &Endpoints::default()).unwrap_err();
    assert_eq!(error.message(), "登录响应缺少会话令牌");
    assert!(matches!(error, MiCloudError::Runtime(_)));
    assert_runtime(error);
}

#[test]
fn login_default_device_id_reaches_cookie_header() {
    let credentials =
        Credentials::from_json(br#"{"userId":"synthetic-user","passToken":"synthetic-pass"}"#)
            .unwrap();
    let fake = FakeTransport::new(login_responses());
    Session::login(&fake, &credentials, &Endpoints::default()).unwrap();
    for request in fake.requests() {
        let header = &request
            .headers
            .iter()
            .find(|(name, _)| name == "Cookie")
            .unwrap()
            .1;
        assert!(
            header
                .split("; ")
                .any(|part| part == format!("deviceId={}", crate::credentials::DEFAULT_DEVICE_ID))
        );
    }
}
#[test]
fn login_five_redirects_without_token() {
    let mut responses = vec![Ok(auth(SECURE_STS))];
    for status in [301, 302, 303, 307, 308] {
        responses.push(Ok(response(status, "", &[("Location", SECURE_STS)])));
    }
    responses.push(Ok(response(200, "", &[])));
    let fake = FakeTransport::new(responses);
    let error = Session::login(&fake, &credentials(), &Endpoints::default()).unwrap_err();
    assert!(error.message().contains("缺少会话令牌"));
    assert_eq!(fake.requests().len(), 7);
    assert_runtime(error);
}
#[test]
fn login_five_redirects_succeed_and_six_fail() {
    for count in [5, 6] {
        let mut responses = vec![Ok(auth(SECURE_STS))];
        responses.extend((0..count).map(|_| {
            Ok(response(
                302,
                "",
                &[
                    ("Location", SECURE_STS),
                    ("Set-Cookie", "serviceToken=synthetic-token"),
                ],
            ))
        }));
        responses.push(Ok(response(200, "", &[])));
        let fake = FakeTransport::new(responses);
        let result = Session::login(&fake, &credentials(), &Endpoints::default());
        if count == 5 {
            assert!(result.is_ok());
        } else {
            assert_runtime(result.unwrap_err());
        }
        assert_eq!(fake.requests().len(), 7);
    }
}
#[test]
fn login_rejects_plaintext_initial_and_redirect_locations() {
    const PLAIN: &str = "http://example.invalid/sts";
    for responses in [
        vec![Ok(auth(PLAIN))],
        vec![
            Ok(auth(SECURE_STS)),
            Ok(response(302, "", &[("Location", PLAIN)])),
        ],
    ] {
        let count = responses.len();
        let fake = FakeTransport::new(responses);
        assert_runtime(Session::login(&fake, &credentials(), &Endpoints::default()).unwrap_err());
        assert_eq!(fake.requests().len(), count);
    }
}
#[test]
fn login_global_marker_removal_and_invalid_responses() {
    let mut responses = login_responses();
    responses[0] = Ok(response(
        200,
        &format!(
            "&&&START&&&{}&&&START&&&",
            String::from_utf8(auth(SECURE_STS).body).unwrap()
        ),
        &[],
    ));
    assert!(
        Session::login(
            &FakeTransport::new(responses),
            &credentials(),
            &Endpoints::default()
        )
        .is_ok()
    );
    for body in [
        "[]",
        "bad",
        r#"{"code":"0"}"#,
        r#"{"code":0,"location":"x"}"#,
    ] {
        assert_runtime(
            Session::login(
                &FakeTransport::new([Ok(response(200, body, &[]))]),
                &credentials(),
                &Endpoints::default(),
            )
            .unwrap_err(),
        );
    }
    assert_runtime(
        Session::login(
            &FakeTransport::new([Err(TransportError::Timeout)]),
            &credentials(),
            &Endpoints::default(),
        )
        .unwrap_err(),
    );
}
#[test]
fn session_debug_only_lengths() {
    let session = session();
    let debug = format!("{session:?}");
    assert_eq!(
        debug,
        format!(
            "登录会话 {{ 密钥长度: {}, 令牌长度: {} }}",
            session.ssecurity.len(),
            session.service_token.len()
        )
    );
}
#[cfg(debug_assertions)]
#[test]
fn debug_plaintext_requires_override_and_exact_loopback_host() {
    const LOCAL: &str = "http://127.0.0.1:12345";
    let endpoints = Endpoints::debug_overrides(Some(LOCAL.into()), None).unwrap();
    for url in [
        LOCAL,
        "http://localhost/a",
        "http://LOCALHOST/a",
        "http://[::1]:80/a",
    ] {
        assert!(endpoints.allows(url, true));
        assert!(!Endpoints::default().allows(url, true));
    }
    for url in [
        "http://example.invalid/sts",
        "http://127.0.0.1.example.invalid/",
        "http://127.0.0.1@example.invalid/",
        "http://127.0.0.2/",
        "http://localhost\\@example.invalid/",
    ] {
        assert!(!endpoints.allows(url, true));
        assert!(matches!(
            Endpoints::debug_overrides(Some(url.into()), None),
            Err(MiCloudError::State(_))
        ));
    }
    assert!(!endpoints.allows(LOCAL, false));
    let endpoints =
        Endpoints::debug_overrides(Some("http://LOCALHOST:12345".into()), None).unwrap();
    let fake = FakeTransport::new([
        Ok(auth("/sts")),
        Ok(response(
            200,
            "",
            &[("Set-Cookie", "serviceToken=synthetic-token")],
        )),
    ]);
    Session::login(&fake, &credentials(), &endpoints).unwrap();
    assert_eq!(fake.requests()[1].url, "http://LOCALHOST:12345/sts");
}

#[test]
fn login_first_response_cookies_reach_second_hop() {
    let mut first = auth(SECURE_STS);
    first
        .headers
        .push(("Set-Cookie".into(), "first=synthetic-first; Path=/".into()));
    let fake = FakeTransport::new([
        Ok(first),
        Ok(response(
            200,
            "",
            &[("Set-Cookie", "serviceToken=synthetic-token")],
        )),
    ]);
    Session::login(&fake, &credentials(), &Endpoints::default()).unwrap();
    assert!(
        fake.requests()[1].headers[1]
            .1
            .ends_with("; first=synthetic-first")
    );
}

#[test]
fn login_invalid_credentials_fail_before_transport() {
    let mut credentials = credentials();
    credentials.pass_token = "synthetic\r\ninvalid".into();
    let fake = FakeTransport::new([]);
    assert_eq!(
        Session::login(&fake, &credentials, &Endpoints::default())
            .unwrap_err()
            .exit_code(),
        4
    );
    assert!(fake.requests().is_empty());
}
