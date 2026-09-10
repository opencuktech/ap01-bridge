//! 来源于 Python 原项目、由旧实现 `mi_cloud.py` 生成的黄金向量。
//! 逐字节复制自交接文档 `golden/mi_cloud_signing_vectors.json`。
//! 所有值均为合成测试数据，不含真实凭据。

/// 来源于 Python 原项目，由旧实现 `mi_cloud.py` 生成的黄金向量。
/// 逐字节复制自交接文档 `golden/mi_cloud_signing_vectors.json`，保留原格式与换行。
/// 所有值均为合成测试数据，不含真实凭据。
pub const MI_CLOUD_SIGNING_VECTORS: &str = r#"{
  "description": "从 reference/legacy/mi_cloud.py 生成的签名与加密黄金向量。所有值均为测试用假数据，不含真实凭据。Rust 实现在相同输入下必须得到相同输出。",
  "source": {
    "file": "reference/legacy/mi_cloud.py",
    "sha256": "577bd978e1d2d87b339385bd2a0e2131ce9c194bed859d30b3950e96ffec3729"
  },
  "inputs": {
    "ssecurity_raw_utf8": "0123456789abcdef",
    "ssecurity_b64": "MDEyMzQ1Njc4OWFiY2RlZg==",
    "getrandbits_64_hex": "0123456789ABCDEF",
    "fixed_minute": 29000000,
    "user_id": "10001",
    "service_token": "TEST_SERVICE_TOKEN",
    "path": "home/device_list",
    "data_json_obj": {
      "getVirtualModel": false,
      "getHuamiDevices": 0
    }
  },
  "nonce": {
    "first8_signed_be_hex": "8123456789abcdef",
    "minute_be_hex": "01ba8140",
    "nonce_b64": "gSNFZ4mrze8BuoFA",
    "rule": "nonce = b64( int64_be(getrandbits(64) - 2^63) || minimal_be_bytes(unix_time/60) )"
  },
  "signed_nonce": {
    "rule": "b64( sha256( b64decode(ssecurity) || b64decode(nonce) ) )",
    "value": "MsG+4jN8xwAnfFqqa6sruvVFFao7gaPbdxUmak3Af3c="
  },
  "rc4": {
    "rule": "key = b64decode(signed_nonce); RC4 keystream drop first 1024 bytes; encrypt: b64(plain_utf8 XOR ks); decrypt: (b64decode(cipher) XOR ks).utf8",
    "plain": "{\"hello\":\"世界\"}",
    "cipher_b64": "h4B9fBhQExjjX0S1pK/8XK75"
  },
  "signature": {
    "rule": "b64( sha1( METHOD & path_without_/app_prefix & k=v ... (dict insertion order) & signed_nonce ) )",
    "method": "POST",
    "url": "https://api.io.mi.com/app/home/device_list",
    "params_in_order": {
      "data": "{\"a\":1}",
      "rc4_hash__": "X"
    },
    "value": "Qjb+nVtN+EWMde+lO1qn0RlTBYk="
  },
  "composed_request": {
    "url": "https://api.io.mi.com/app/home/device_list",
    "timeout_seconds": 30,
    "headers": {
      "User-Agent": "Android-7.1.1-1.0.0-ONEPLUS A3010-136-ABCDEF1234567 APP/xiaomi.smarthome APPV/62830",
      "Accept-Encoding": "identity",
      "MIOT-ENCRYPT-ALGORITHM": "ENCRYPT-RC4"
    },
    "cookies": {
      "userId": "10001",
      "yetAnotherServiceToken": "TEST_SERVICE_TOKEN",
      "serviceToken": "TEST_SERVICE_TOKEN",
      "locale": "zh_CN",
      "timezone": "GMT+08:00",
      "channel": "MI_APP_STORE"
    },
    "form_fields_in_order": [
      "data",
      "rc4_hash__",
      "signature",
      "ssecurity",
      "_nonce"
    ],
    "form": {
      "data": "h4ByfABqFUitCMFhfycNteCm/EZFkKzMYkMP+GMAQACXTUo5byRtzLuXdkj0",
      "rc4_hash__": "sOFjfydeSWKaLfVvQAI7sf7g8BdXiObBDAU5oA==",
      "signature": "NBtjofBLeUowLaHG1GrcD6yskMo=",
      "ssecurity": "MDEyMzQ1Njc4OWFiY2RlZg==",
      "_nonce": "gSNFZ4mrze8BuoFA"
    },
    "notes": [
      "data 字段是 json.dumps(obj, ensure_ascii=False, separators=(',',':')) 后再 RC4 加密",
      "rc4_hash__ 是对明文 {data} 的签名，再被 RC4 加密",
      "signature 是对已加密的 {data, rc4_hash__} 的签名，不加密",
      "ssecurity 与 _nonce 以明文随表单发送"
    ]
  },
  "composed_response": {
    "plain_json": "{\"code\":0,\"message\":\"ok\",\"result\":{\"list\":[]}}",
    "body_text_b64": "h4B2dhBZXgDpUYJgVzsasevh5BoGk7SLYkMa+GQ9WRXYHnV+dSR93eqPFyX0TA==",
    "decoded": {
      "code": 0,
      "message": "ok",
      "result": {
        "list": []
      }
    }
  }
}
"#;

/// 解析源码内的合成黄金向量，供单元测试与集成测试复用。
pub fn vectors() -> serde_json::Value {
    serde_json::from_str(MI_CLOUD_SIGNING_VECTORS).expect("源码内黄金向量必须是有效的 JSON")
}
