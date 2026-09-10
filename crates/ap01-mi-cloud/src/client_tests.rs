//! 请求和信封向量锁定，以及会话复用回归。
use super::*;
use crate::testkit::{FakeTransport, fixtures::*};
use base64::{Engine, engine::general_purpose::STANDARD};

#[test]
fn composed_request_vector() {
    let vectors = vectors();
    let input = &vectors["inputs"];
    let expected = &vectors["composed_request"];
    let fake = FakeTransport::new([Ok(response(
        200,
        vectors["composed_response"]["body_text_b64"]
            .as_str()
            .unwrap(),
        &[],
    ))]);
    let client = Client::new(&fake, session(), Endpoints::default(), &FixedSource);
    let data = format!(
        r#"{{"getVirtualModel":{},"getHuamiDevices":{}}}"#,
        input["data_json_obj"]["getVirtualModel"], input["data_json_obj"]["getHuamiDevices"]
    );
    let decoded = client
        .request(input["path"].as_str().unwrap(), &data)
        .unwrap();
    assert_eq!(decoded, vectors["composed_response"]["decoded"]);
    let request = &fake.requests()[0];
    assert_eq!(request.method, "POST");
    assert_eq!(request.url, expected["url"]);
    assert_eq!(request.timeout.as_secs(), expected["timeout_seconds"]);
    assert_eq!(request.headers.len(), 5);
    for (name, value) in expected["headers"].as_object().unwrap() {
        assert_eq!(request.headers.iter().filter(|(k, _)| k == name).count(), 1);
        assert_eq!(
            request.headers.iter().find(|(k, _)| k == name).unwrap().1,
            value.as_str().unwrap()
        );
    }
    assert_eq!(
        request.headers[3],
        (
            "Content-Type".into(),
            "application/x-www-form-urlencoded".into()
        )
    );
    let cookies: Vec<_> = request.headers[4]
        .1
        .split("; ")
        .map(|part| part.split_once('=').unwrap())
        .collect();
    assert_eq!(request.headers[4].0, "Cookie");
    assert_eq!(cookies.len(), 6);
    for ((name, value), expected_name) in cookies.iter().zip([
        "userId",
        "yetAnotherServiceToken",
        "serviceToken",
        "locale",
        "timezone",
        "channel",
    ]) {
        assert_eq!(*name, expected_name);
        assert_eq!(*value, expected["cookies"][expected_name].as_str().unwrap());
    }
    let form = decode_form(request.body.as_ref().unwrap());
    assert_eq!(form.len(), 5);
    for ((name, value), expected_name) in form
        .iter()
        .zip(expected["form_fields_in_order"].as_array().unwrap())
    {
        assert_eq!(name, expected_name.as_str().unwrap());
        assert_eq!(value, expected["form"][name].as_str().unwrap());
    }
}
#[test]
fn composed_response_vector() {
    let v = vectors();
    let actual = decode_response(
        &response(
            200,
            v["composed_response"]["body_text_b64"].as_str().unwrap(),
            &[],
        ),
        v["signed_nonce"]["value"].as_str().unwrap(),
        str::to_owned,
    )
    .unwrap();
    assert_eq!(actual, v["composed_response"]["decoded"]);
}

#[test]
fn composed_response_vector_accepts_trailing_newline() {
    let v = vectors();
    let body = format!(
        "{}\n",
        v["composed_response"]["body_text_b64"].as_str().unwrap()
    );
    let fake = FakeTransport::new([Ok(response(200, &body, &[]))]);
    let client = Client::new(&fake, session(), Endpoints::default(), &FixedSource);
    assert_eq!(
        client.request("home/device_list", "{}").unwrap(),
        v["composed_response"]["decoded"]
    );
}

#[test]
fn public_fields_require_bounded_ascii_identifiers() {
    for valid in [
        "a",
        "njcuk.enstor.ap01",
        "1.0.2_0041",
        "V1-beta_2",
        &"a".repeat(128),
    ] {
        assert!(Client::check_public(valid).is_ok());
    }
    for invalid in [
        "",
        " ",
        "中文",
        "a/b",
        "a?b",
        "a#b",
        "a%b",
        "a=b",
        "a,b",
        "a\nb",
        "a\0b",
        &"a".repeat(129),
    ] {
        assert_runtime(Client::check_public(invalid).unwrap_err());
    }
}
fn fails(response: HttpResponse) {
    let fake = FakeTransport::new([Ok(response)]);
    assert_runtime(
        Client::new(&fake, session(), Endpoints::default(), &FixedSource)
            .request("home/device_list", "{}")
            .unwrap_err(),
    );
}
#[test]
fn envelope_non_success_status() {
    fails(response(503, "私有响应", &[]));
}
#[test]
fn envelope_invalid_base64() {
    fails(response(200, "!", &[]));
}
#[test]
fn envelope_invalid_utf8() {
    let key = vectors()["signed_nonce"]["value"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut bytes = STANDARD
        .decode(crypto::rc4_encrypt(&key, "a").unwrap())
        .unwrap();
    bytes[0] ^= b'a' ^ 0xff;
    fails(response(200, &STANDARD.encode(bytes), &[]));
}
#[test]
fn envelope_not_object() {
    fails(encrypted("[]"));
}
#[test]
fn envelope_invalid_json_or_code() {
    for text in ["bad", "{}", r#"{"code":false}"#, r#"{"code":0.5}"#] {
        fails(encrypted(text));
    }
}
#[test]
fn envelope_nonzero_code_redacts_message() {
    let fake = FakeTransport::new([Ok(encrypted(
        r#"{"code":-42,"message":"请访问 https://example.invalid/a?secret=synthetic 完成验证 cookie ssecurity TEST_SERVICE_TOKEN synthetic-pass","private":"原文不得输出"}"#,
    ))]);
    let error = Client::new(&fake, session(), Endpoints::default(), &FixedSource)
        .request("home/device_list", "{}")
        .unwrap_err();
    assert!(error.message().contains("-42"));
    assert!(error.message().contains("请访问 <url> 完成验证"));
    for secret in ["TEST_SERVICE_TOKEN", "synthetic-pass", "原文不得输出"] {
        assert!(!error.message().contains(secret));
    }
    assert_runtime(error);
}
#[test]
fn two_business_requests_reuse_single_login() {
    let mut responses = login_responses();
    responses.extend([
        Ok(encrypted(r#"{"code":0}"#)),
        Ok(encrypted(r#"{"code":0}"#)),
    ]);
    let fake = FakeTransport::new(responses);
    let session = Session::login(&fake, &credentials(), &Endpoints::default()).unwrap();
    let client = Client::new(&fake, session, Endpoints::default(), &FixedSource);
    client.request("home/device_list", "{}").unwrap();
    client.request("home/device_list", "{}").unwrap();
    assert_eq!(
        fake.requests()
            .iter()
            .map(|r| r.method.clone())
            .collect::<Vec<_>>(),
        ["GET", "GET", "POST", "POST"]
    );
}
#[test]
fn request_preserves_non_ascii_and_form_encoding() {
    let fake = FakeTransport::new([Ok(encrypted(r#"{"code":0}"#))]);
    Client::new(&fake, session(), Endpoints::default(), &FixedSource)
        .request("home/device_list", r#"{"中文":"世界"}"#)
        .unwrap();
    let form = decode_form(fake.requests()[0].body.as_ref().unwrap());
    assert_eq!(
        crypto::rc4_decrypt(
            vectors()["signed_nonce"]["value"].as_str().unwrap(),
            &form[0].1
        )
        .unwrap(),
        r#"{"中文":"世界"}"#
    );
    assert_eq!(encode(" +/世界"), "+%2B%2F%E4%B8%96%E7%95%8C");
}
