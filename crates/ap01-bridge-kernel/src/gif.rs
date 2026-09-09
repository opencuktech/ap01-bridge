//! GIF 校验报告的摘要与序列化适配，不承担文件读取。

pub use ap01_gif::{ErrorCode, Report as GifReport, ValidationError, validate as validate_bytes};
use serde::ser::{SerializeSeq, SerializeStruct};
use serde::{Serialize, Serializer};
use sha2::{Digest, Sha256};

/// 面向脚本的十二字段校验报告，字段顺序固定。
#[derive(Debug, Serialize)]
pub struct ValidationReport {
    pub ok: bool,
    #[serde(serialize_with = "serialize_errors")]
    pub errors: Vec<ValidationError>,
    pub bytes: usize,
    pub sha256: String,
    pub version: String,
    pub width: u16,
    pub height: u16,
    pub frames: usize,
    pub delays_ms: Vec<u32>,
    pub total_duration_ms: u64,
    pub loop_count: Option<u16>,
    pub trailing_bytes: usize,
}

fn serialize_errors<S: Serializer>(
    errors: &[ValidationError],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    struct ErrorRef<'a>(&'a ValidationError);
    impl Serialize for ErrorRef<'_> {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            let mut item = serializer.serialize_struct("ValidationError", 2)?;
            item.serialize_field("code", self.0.code.as_str())?;
            item.serialize_field("message", &self.0.message)?;
            item.end()
        }
    }
    let mut sequence = serializer.serialize_seq(Some(errors.len()))?;
    for error in errors {
        sequence.serialize_element(&ErrorRef(error))?;
    }
    sequence.end()
}

/// 校验原始字节并计算全部输入的 SHA-256，不改写内容。
pub fn validate(bytes: &[u8]) -> ValidationReport {
    let report = validate_bytes(bytes);
    ValidationReport {
        ok: report.ok,
        errors: report.errors,
        bytes: report.bytes,
        sha256: format!("{:x}", Sha256::digest(bytes)),
        version: report.version,
        width: report.width,
        height: report.height,
        frames: report.frames,
        delays_ms: report.delays_ms,
        total_duration_ms: report.total_duration_ms,
        loop_count: report.loop_count,
        trailing_bytes: report.trailing_bytes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap01_gif::testkit::{Frame, GifBuilder, quota_gif};
    use serde_json::{Value, json};

    #[test]
    fn quota_json_has_exactly_twelve_fields_in_order() {
        let bytes = quota_gif();
        let report = validate(&bytes);
        let encoded = serde_json::to_string(&report).unwrap();
        let value: Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(value.as_object().unwrap().len(), 12);
        assert_eq!(
            value,
            json!({
                "ok": true, "errors": [], "bytes": bytes.len(),
                "sha256": format!("{:x}", Sha256::digest(&bytes)),
                "version": "GIF89a", "width": 320, "height": 240, "frames": 6,
                "delays_ms": [600, 600, 600, 600, 417600, 60000],
                "total_duration_ms": 480000, "loop_count": null, "trailing_bytes": 0
            })
        );
        let mut previous = 0;
        for key in [
            "ok",
            "errors",
            "bytes",
            "sha256",
            "version",
            "width",
            "height",
            "frames",
            "delays_ms",
            "total_duration_ms",
            "loop_count",
            "trailing_bytes",
        ] {
            let position = encoded.find(&format!("\"{key}\":")).unwrap();
            assert!(position > previous);
            previous = position;
        }
    }

    #[test]
    fn rejected_report_serializes_codes_messages_and_defaults() {
        let bytes = GifBuilder {
            version: "GIF87a".into(),
            ..GifBuilder::default().frame(Frame::default())
        }
        .build();
        let value = serde_json::to_value(validate(&bytes)).unwrap();
        assert_eq!(value.as_object().unwrap().len(), 12);
        assert_eq!(value["ok"], false);
        assert_eq!(value["version"], "GIF87a");
        assert_eq!(value["width"], 320);
        assert_eq!(value["height"], 240);
        assert_eq!(value["frames"], 0);
        assert_eq!(value["delays_ms"], json!([]));
        assert_eq!(value["total_duration_ms"], 0);
        assert_eq!(value["loop_count"], Value::Null);
        assert_eq!(value["trailing_bytes"], 0);
        assert_eq!(value["errors"].as_array().unwrap().len(), 1);
        let error = &value["errors"][0];
        assert_eq!(error.as_object().unwrap().len(), 2);
        assert_eq!(error["code"], "header_not_gif89a");
        assert!(error["message"].as_str().unwrap().contains("头不是 GIF89a"));
    }

    #[test]
    fn sha256_matches_known_vectors_even_for_rejected_input() {
        for (bytes, hash) in [
            (
                &b""[..],
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            ),
            (
                &b"abc"[..],
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            ),
        ] {
            let report = validate(bytes);
            assert!(!report.ok);
            assert_eq!(report.sha256, hash);
        }
    }

    #[test]
    fn loop_count_serializes_as_integer() {
        let bytes = GifBuilder {
            loop_count: Some(0),
            ..GifBuilder::default().frame(Frame::default())
        }
        .build();
        let value = serde_json::to_value(validate(&bytes)).unwrap();
        assert_eq!(value["loop_count"], 0);
        assert_eq!(value["ok"], true);
    }
}
