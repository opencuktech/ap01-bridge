//! 设备选择、离线分支和字段缺省的离线回归。
use super::*;
use crate::{
    client::{Client, RequestSource},
    crypto,
    session::{Endpoints, Session},
    testkit::{FakeTransport, fixtures::*},
};
use serde_json::json;

fn list(entries: Value) -> crate::transport::HttpResponse {
    encrypted(&json!({"code":0,"result":{"list":entries}}).to_string())
}
fn entry() -> Value {
    json!({"did":"synthetic-target","model":MODEL,"isOnline":true,"fw_version":" 1.0.2_0041 "})
}

#[test]
fn device_list_finds_ap01_and_exact_payload() {
    let fake = FakeTransport::new([Ok(list(json!([entry()])))]);
    let client = Client::new(&fake, session(), Endpoints::default(), &FixedSource);
    let device = find_ap01(&client).unwrap();
    assert_eq!(device.did, "synthetic-target");
    assert_eq!(device.model, MODEL);
    assert_eq!(device.online, Some(true));
    assert_eq!(device.firmware_version.as_deref(), Some("1.0.2_0041"));
    let form = decode_form(fake.requests()[0].body.as_ref().unwrap());
    assert_eq!(
        crypto::rc4_decrypt(
            vectors()["signed_nonce"]["value"].as_str().unwrap(),
            &form[0].1
        )
        .unwrap(),
        DEVICE_LIST_DATA
    );
}
#[test]
fn device_list_empty_or_unmatched_is_private_runtime_error() {
    for entries in [
        json!([]),
        json!([{"model":"private-model","did":"private-target","name":"私有名字"}]),
    ] {
        let fake = FakeTransport::new([Ok(list(entries))]);
        let client = Client::new(&fake, session(), Endpoints::default(), &FixedSource);
        let error = find_ap01(&client).err().unwrap();
        for value in ["private-model", "private-target", "私有名字"] {
            assert!(!error.message().contains(value));
        }
        assert_runtime(error);
    }
}
#[test]
fn device_list_first_match_only() {
    let fake = FakeTransport::new([Ok(list(
        json!([{"model":"other","did":"other"},entry(),{"model":MODEL,"did":"second","isOnline":false}]),
    ))]);
    let client = Client::new(&fake, session(), Endpoints::default(), &FixedSource);
    assert_eq!(find_ap01(&client).unwrap().did, "synthetic-target");
}
#[test]
fn rpc_online_success_and_signature_target() {
    let fake = FakeTransport::new([
        Ok(list(json!([entry()]))),
        Ok(encrypted(
            r#"{"code":0,"result":{"life":123,"fw_ver":" 2 ","fw_version":"3","model":"njcuk.enstor.ap01"}}"#,
        )),
    ]);
    let client = Client::new(&fake, session(), Endpoints::default(), &FixedSource);
    let info = device_info(&client, &find_ap01(&client).unwrap()).unwrap();
    assert_eq!(info.uptime_seconds, Some(Number::from(123)));
    assert_eq!(info.firmware_version.as_deref(), Some("2"));
    assert_eq!(info.model.as_deref(), Some(MODEL));
    let request = &fake.requests()[1];
    assert!(request.url.ends_with("/app/home/rpc/synthetic-target"));
    let form = decode_form(request.body.as_ref().unwrap());
    let v = vectors();
    let key = v["signed_nonce"]["value"].as_str().unwrap();
    let plain = crypto::rc4_decrypt(key, &form[0].1).unwrap();
    let id = 1_000_000 + FixedSource.random_u64().unwrap() % 9_000_000;
    assert!((1_000_000..=9_999_999).contains(&id));
    assert_eq!(
        plain,
        format!(r#"{{"id":{id},"method":"miIO.info","params":[]}}"#)
    );
    assert_eq!(
        crypto::rc4_decrypt(key, &form[1].1).unwrap(),
        crypto::signature(
            "POST",
            "/home/rpc/synthetic-target",
            &[("data".into(), plain)],
            key
        )
    );
    assert_ne!(
        form[2].1,
        crypto::signature("POST", "/home/rpc/other", &form[..2], key)
    );
}
#[test]
fn rpc_offline_skips_request() {
    let mut item = entry();
    item["isOnline"] = json!(false);
    item["fw_version"] = json!(" ");
    let fake = FakeTransport::new([Ok(list(json!([item])))]);
    let client = Client::new(&fake, session(), Endpoints::default(), &FixedSource);
    let report = ap01(&client).unwrap();
    assert_eq!(report.online, Some(false));
    assert!(report.uptime_seconds.is_none());
    assert!(report.firmware_version.is_none());
    assert_eq!(fake.requests().len(), 1);
}
#[test]
fn rpc_failure_redacts_target() {
    let fake = FakeTransport::new([
        Ok(list(json!([entry()]))),
        Ok(encrypted(
            r#"{"code":-704010000,"message":"目标 synthetic-target 不可用"}"#,
        )),
    ]);
    let client = Client::new(&fake, session(), Endpoints::default(), &FixedSource);
    let error = ap01(&client).err().unwrap();
    assert!(error.message().contains("-704010000"));
    assert!(!error.message().contains("synthetic-target"));
    assert_runtime(error);
}
#[test]
fn device_unknown_online_rpc_shapes_and_version_precedence() {
    for (list_version, result, expected_version, expected_life) in [
        (
            json!(" list "),
            json!({"fw_ver":"first","fw_version":"second","life":42}),
            Some("list"),
            Some(42),
        ),
        (
            json!(" "),
            json!({"fw_ver":" first ","fw_version":"second","life":-1}),
            Some("first"),
            Some(-1),
        ),
        (
            Value::Null,
            json!({"fw_ver":" \t","fw_version":" second ","life":1.5}),
            Some("second"),
            None,
        ),
        (Value::Null, json!({"life":"42"}), None, None),
        (Value::Null, json!([]), None, None),
        (Value::Null, Value::Null, None, None),
    ] {
        let item = json!({"did":"synthetic-target","model":MODEL,"fw_version":list_version});
        let fake = FakeTransport::new([
            Ok(list(json!([item]))),
            Ok(encrypted(&json!({"code":0,"result":result}).to_string())),
        ]);
        let client = Client::new(&fake, session(), Endpoints::default(), &FixedSource);
        let report = ap01(&client).unwrap();
        assert_eq!(report.online, None);
        assert_eq!(report.firmware_version.as_deref(), expected_version);
        assert_eq!(report.uptime_seconds, expected_life.map(Number::from));
        assert_eq!(fake.requests().len(), 2);
        let object = serde_json::to_value(report).unwrap();
        assert_eq!(object.as_object().unwrap().len(), 5);
    }
}
#[test]
fn public_fields_reject_invalid_or_multiline_values() {
    for version in [
        "https://example.invalid/private",
        "1 2",
        "1\n2",
        &"a".repeat(129),
    ] {
        let fake = FakeTransport::new([Ok(list(
            json!([{"did":"synthetic-target","model":MODEL,"isOnline":false,"fw_version":version}]),
        ))]);
        assert_runtime(
            ap01(&Client::new(
                &fake,
                session(),
                Endpoints::default(),
                &FixedSource,
            ))
            .err()
            .unwrap(),
        );
    }
}

#[test]
fn short_login_cookie_does_not_reject_firmware_version() {
    for rpc_version in [false, true] {
        let mut responses = login_responses();
        responses[0]
            .as_mut()
            .unwrap()
            .headers
            .push(("Set-Cookie".into(), "flag=1".into()));
        let mut item = entry();
        if rpc_version {
            item["fw_version"] = Value::Null;
        }
        responses.extend([
            Ok(list(json!([item]))),
            Ok(encrypted(r#"{"code":0,"result":{"fw_ver":"1.0.2_0041"}}"#)),
        ]);
        let fake = FakeTransport::new(responses);
        let session = Session::login(&fake, &credentials(), &Endpoints::default()).unwrap();
        let report = ap01(&Client::new(
            &fake,
            session,
            Endpoints::default(),
            &FixedSource,
        ))
        .unwrap();
        let output = serde_json::to_value(report).unwrap();
        assert_eq!(output["firmware_version"], "1.0.2_0041");
        assert_eq!(output["model"], MODEL);
        assert!(fake.requests()[1].headers[1].1.ends_with("; flag=1"));
    }
}

#[test]
fn dotted_device_id_reaches_rpc_path() {
    let mut item = entry();
    item["did"] = json!("synthetic.target-1_2");
    let fake = FakeTransport::new([
        Ok(list(json!([item]))),
        Ok(encrypted(r#"{"code":0,"result":{}}"#)),
    ]);
    let client = Client::new(&fake, session(), Endpoints::default(), &FixedSource);
    ap01(&client).unwrap();
    assert!(
        fake.requests()[1]
            .url
            .ends_with("/home/rpc/synthetic.target-1_2")
    );
}

#[test]
fn device_id_rejects_path_delimiters_whitespace_and_controls() {
    for did in ["", "a/b", "a?b", "a#b", "a%b", "a b", "a\nb", "a\0b"] {
        let mut item = entry();
        item["did"] = json!(did);
        let fake = FakeTransport::new([Ok(list(json!([item])))]);
        let client = Client::new(&fake, session(), Endpoints::default(), &FixedSource);
        assert_runtime(ap01(&client).err().unwrap());
        assert_eq!(fake.requests().len(), 1);
    }
}
