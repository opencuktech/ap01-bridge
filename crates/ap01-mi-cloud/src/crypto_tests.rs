//! 使用源码内黄金向量覆盖边界，并在交接副本存在时校验漂移。

use super::*;
use crate::golden::vectors;
use serde_json::Value;
use std::{collections::HashSet, path::PathBuf};

fn reference(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs")
        .join("handoff/reference")
        .join(name)
}

fn verify_source(vector: &Value, source: &[u8]) -> bool {
    format!("{:x}", Sha256::digest(source)) == vector["source"]["sha256"].as_str().unwrap()
}

fn field<'a>(value: &'a Value, section: &str, name: &str) -> &'a str {
    value[section][name].as_str().unwrap()
}

fn key(v: &Value) -> &str {
    field(v, "signed_nonce", "value")
}

fn plain(v: &Value) -> String {
    let data = &v["inputs"]["data_json_obj"];
    format!(
        "{{\"getVirtualModel\":{},\"getHuamiDevices\":{}}}",
        data["getVirtualModel"], data["getHuamiDevices"]
    )
}

fn plain_hash(v: &Value) -> String {
    signature(
        "POST",
        field(v, "composed_request", "url"),
        &[("data".into(), plain(v))],
        key(v),
    )
}

#[test]
fn golden_matches_handoff_when_present() {
    let v = vectors();
    let golden_path = reference("golden/mi_cloud_signing_vectors.json");
    if golden_path.try_exists().expect("无法检查交接向量文件") {
        let handoff: Value =
            serde_json::from_slice(&std::fs::read(golden_path).expect("无法读取交接向量文件"))
                .expect("交接向量必须是有效的 JSON");
        assert_eq!(v, handoff, "源码内黄金向量与交接副本不一致");
    } else {
        eprintln!("交接向量文件不存在，跳过向量副本漂移校验");
    }

    let source_path = reference("legacy/mi_cloud.py");
    if !source_path.try_exists().expect("无法检查参考算法文件") {
        eprintln!("参考算法文件不存在，跳过摘要与单字节变异校验");
        return;
    }
    let mut copy = std::fs::read(source_path).expect("无法读取参考算法文件");
    assert!(verify_source(&v, &copy), "参考算法摘要不一致");
    copy[0] ^= 1;
    assert!(!verify_source(&v, &copy), "必须拒绝改动一个字节的内存副本");
}

#[test]
fn nonce_vector() {
    let v = vectors();
    let random = u64::from_str_radix(field(&v, "inputs", "getrandbits_64_hex"), 16).unwrap();
    let rand8 = random_u64_bytes(random);
    let hex: String = rand8.iter().map(|b| format!("{b:02x}")).collect();
    assert!(hex == field(&v, "nonce", "first8_signed_be_hex"));
    assert!(
        nonce(rand8, v["inputs"]["fixed_minute"].as_u64().unwrap() * 60)
            == field(&v, "nonce", "nonce_b64")
    );
}

#[test]
fn nonce_shape() {
    let mut seen = HashSet::new();
    for _ in 0..1000 {
        let mut random = [0; 8];
        getrandom::fill(&mut random).unwrap();
        let bytes = STANDARD.decode(nonce(random, 29_000_000 * 60)).unwrap();
        assert_eq!(bytes.len(), 12);
        assert!(seen.insert(bytes[..8].to_vec()));
    }
}

#[test]
fn nonce_minute_boundaries() {
    for (seconds, tail) in [
        (0, vec![]),
        (59, vec![]),
        (60, vec![1]),
        (255 * 60, vec![255]),
        (256 * 60, vec![1, 0]),
        (256 * 60 + 59, vec![1, 0]),
    ] {
        let bytes = STANDARD.decode(nonce([0; 8], seconds)).unwrap();
        assert_eq!(&bytes[8..], tail);
    }
    assert_eq!(random_u64_bytes(0), i64::MIN.to_be_bytes());
    assert_eq!(random_u64_bytes(u64::MAX), i64::MAX.to_be_bytes());
}

#[test]
fn signed_nonce_vector() {
    let v = vectors();
    let seed = field(&v, "inputs", "ssecurity_b64");
    let nonce = field(&v, "nonce", "nonce_b64");
    assert!(signed_nonce(seed, nonce).unwrap() == key(&v));
    assert!(STANDARD.encode(Sha256::digest(format!("{seed}{nonce}"))) != key(&v));
    assert!(signed_nonce("", "").unwrap() == STANDARD.encode(Sha256::digest([])));
    assert_eq!(signed_nonce("!", ""), Err(CryptoError::InvalidBase64));
    assert_eq!(signed_nonce("", "!"), Err(CryptoError::InvalidBase64));
}

#[test]
fn rc4_encrypt_data_vector() {
    let v = vectors();
    assert!(
        rc4_encrypt(key(&v), field(&v, "rc4", "plain")).unwrap() == field(&v, "rc4", "cipher_b64")
    );
    assert!(rc4_encrypt(key(&v), &plain(&v)).unwrap() == v["composed_request"]["form"]["data"]);
}

#[test]
fn rc4_encrypt_hash_vector() {
    let v = vectors();
    assert!(
        rc4_encrypt(key(&v), &plain_hash(&v)).unwrap()
            == v["composed_request"]["form"]["rc4_hash__"]
    );
}

#[test]
fn rc4_roundtrip() {
    let v = vectors();
    assert!(
        rc4_decrypt(key(&v), field(&v, "rc4", "cipher_b64")).unwrap() == field(&v, "rc4", "plain")
    );
    for text in ["", "中文文本", "\0\n", "纯合成测试"] {
        assert!(rc4_decrypt(key(&v), &rc4_encrypt(key(&v), text).unwrap()).unwrap() == text);
    }
}

#[test]
fn rc4_discards_1024_bytes() {
    let v = vectors();
    let encrypted = rc4_bytes(
        &decode(key(&v)).unwrap(),
        field(&v, "rc4", "plain").as_bytes(),
        0,
    )
    .unwrap();
    assert!(STANDARD.encode(encrypted) != field(&v, "rc4", "cipher_b64"));
    // 标准未丢弃流的已知样例，独立检查密钥调度。
    assert_eq!(
        rc4_bytes(b"Key", b"Plaintext", 0).unwrap(),
        [0xbb, 0xf3, 0x16, 0xe8, 0xd9, 0x40, 0xaf, 0x0a, 0xd3]
    );
}

#[test]
fn rc4_rejects_invalid_utf8_and_encoding() {
    let key = STANDARD.encode(b"synthetic-key");
    let encrypted = STANDARD.encode(rc4_bytes(&decode(&key).unwrap(), &[255], 1024).unwrap());
    assert_eq!(rc4_decrypt(&key, &encrypted), Err(CryptoError::InvalidUtf8));
    assert_eq!(rc4_decrypt(&key, "!"), Err(CryptoError::InvalidBase64));
    assert_eq!(rc4_encrypt("!", ""), Err(CryptoError::InvalidBase64));
    assert_eq!(rc4_encrypt("", ""), Err(CryptoError::EmptyKey));
    assert_eq!(rc4_decrypt("", ""), Err(CryptoError::EmptyKey));
}

#[test]
fn signature_generic_vector() {
    let v = vectors();
    let params = &v["signature"]["params_in_order"];
    let ordered =
        ["data", "rc4_hash__"].map(|name| (name.into(), params[name].as_str().unwrap().into()));
    assert!(
        signature(
            field(&v, "signature", "method"),
            field(&v, "signature", "url"),
            &ordered,
            key(&v)
        ) == field(&v, "signature", "value")
    );
}

#[test]
fn signature_plain_vector() {
    let v = vectors();
    let expected = rc4_decrypt(
        key(&v),
        v["composed_request"]["form"]["rc4_hash__"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert!(plain_hash(&v) == expected);
}

#[test]
fn signature_encrypted_vector() {
    let v = vectors();
    let ordered = [
        ("data".into(), rc4_encrypt(key(&v), &plain(&v)).unwrap()),
        (
            "rc4_hash__".into(),
            rc4_encrypt(key(&v), &plain_hash(&v)).unwrap(),
        ),
    ];
    assert!(
        signature(
            "POST",
            field(&v, "composed_request", "url"),
            &ordered,
            key(&v)
        ) == v["composed_request"]["form"]["signature"]
    );
    let reversed = [ordered[1].clone(), ordered[0].clone()];
    assert!(
        signature(
            "POST",
            field(&v, "composed_request", "url"),
            &reversed,
            key(&v)
        ) != v["composed_request"]["form"]["signature"]
    );
}

#[test]
fn signature_strips_app_prefix() {
    assert!(
        signature_text(
            "post",
            "/app/home/rpc/synthetic-target?ignored=1#fragment",
            &[],
            "synthetic-key"
        ) == "POST&/home/rpc/synthetic-target&synthetic-key"
    );
    assert!(signature_text("get", "/apple/path", &[], "") == "GET&/apple/path&");
    assert!(signature_text("get", "/app", &[], "") == "GET&/app&");
}
