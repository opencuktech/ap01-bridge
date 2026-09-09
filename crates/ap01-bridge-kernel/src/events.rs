//! 服务运行事件及其人类可读输出。

use crate::time::format_rfc3339_utc;
use serde::Serialize;

#[derive(Debug, Serialize)]
#[serde(tag = "event", rename_all = "lowercase")]
pub enum Event {
    Listening {
        bind: String,
        port: u16,
        data_dir: String,
    },
    Request {
        ts: u64,
        client: String,
        method: String,
        path: String,
        version: String,
        status: u16,
        bytes: u64,
        serving: String,
        gif: Option<String>,
    },
}

impl Event {
    pub fn human_line(&self) -> String {
        match self {
            Self::Listening {
                bind,
                port,
                data_dir,
            } => {
                let bind = if bind.contains(':') {
                    format!("[{bind}]")
                } else {
                    bind.clone()
                };
                format!("listening {bind}:{port} data_dir={data_dir}")
            }
            Self::Request {
                ts,
                client,
                method,
                path,
                version,
                status,
                bytes,
                serving,
                ..
            } => format!(
                "{} {client} \"{method} {path} {version}\" {status} {bytes} {serving}",
                format_rfc3339_utc(*ts)
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_field_order_and_human_timestamp_and_original_version() {
        let event = Event::Request {
            ts: 1_788_668_411,
            client: "192.168.1.23".into(),
            method: "GET".into(),
            path: "/screen.gif".into(),
            version: "HTTP/1.0".into(),
            status: 200,
            bytes: 87342,
            serving: "current".into(),
            gif: Some("abc".into()),
        };
        assert_eq!(
            serde_json::to_string(&event).unwrap(),
            "{\"event\":\"request\",\"ts\":1788668411,\"client\":\"192.168.1.23\",\"method\":\"GET\",\"path\":\"/screen.gif\",\"version\":\"HTTP/1.0\",\"status\":200,\"bytes\":87342,\"serving\":\"current\",\"gif\":\"abc\"}"
        );
        assert_eq!(
            event.human_line(),
            "2026-09-06T04:20:11Z 192.168.1.23 \"GET /screen.gif HTTP/1.0\" 200 87342 current"
        );
        let event = Event::Listening {
            bind: "0.0.0.0".into(),
            port: 8765,
            data_dir: "/path".into(),
        };
        assert_eq!(
            serde_json::to_string(&event).unwrap(),
            "{\"event\":\"listening\",\"bind\":\"0.0.0.0\",\"port\":8765,\"data_dir\":\"/path\"}"
        );
        assert_eq!(event.human_line(), "listening 0.0.0.0:8765 data_dir=/path");
    }
}
