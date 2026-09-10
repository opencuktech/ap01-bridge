//! 只读取设备契约允许的字段，设备标识只用于请求路径。

use crate::{MiCloudError, client::Client, session::runtime};
use serde::{Serialize, Serializer, ser::SerializeStruct};
use serde_json::{Number, Value};

pub const MODEL: &str = "njcuk.enstor.ap01";
const DEVICE_LIST_DATA: &str = r#"{"getVirtualModel":true,"getHuamiDevices":1,"get_split_device":false,"support_smart_home":true}"#;

/// 内部设备条目不实现调试或序列化，避免暴露路径标识。
pub struct Device {
    did: String,
    pub model: String,
    pub online: Option<bool>,
    pub firmware_version: Option<String>,
}

/// 设备信息只保留契约字段。
#[derive(Default)]
pub struct DeviceInfo {
    pub uptime_seconds: Option<Number>,
    pub firmware_version: Option<String>,
    pub model: Option<String>,
}

/// 命令公开结果的五字段白名单。
pub struct Ap01Report {
    pub ok: bool,
    pub model: String,
    pub firmware_version: Option<String>,
    pub online: Option<bool>,
    pub uptime_seconds: Option<Number>,
}

impl Serialize for Ap01Report {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut report = serializer.serialize_struct("设备查询结果", 5)?;
        report.serialize_field("ok", &self.ok)?;
        report.serialize_field("model", &self.model)?;
        report.serialize_field("firmware_version", &self.firmware_version)?;
        report.serialize_field("online", &self.online)?;
        report.serialize_field("uptime_seconds", &self.uptime_seconds)?;
        report.end()
    }
}

fn text(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

pub fn find_ap01(client: &Client<'_>) -> Result<Device, MiCloudError> {
    let envelope = client.request("home/device_list", DEVICE_LIST_DATA)?;
    let entry = envelope["result"]["list"]
        .as_array()
        .and_then(|list| list.iter().find(|entry| entry["model"] == MODEL))
        .ok_or_else(|| runtime("账号中未找到目标设备，请检查账号区域"))?;
    let did = entry["did"]
        .as_str()
        .filter(|s| {
            !s.is_empty()
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        })
        .ok_or_else(|| runtime("目标设备缺少有效的请求标识"))?
        .to_owned();
    Ok(Device {
        did,
        model: MODEL.into(),
        online: entry["isOnline"].as_bool(),
        firmware_version: text(&entry["fw_version"]),
    })
}

pub fn device_info(client: &Client<'_>, device: &Device) -> Result<DeviceInfo, MiCloudError> {
    if device.online == Some(false) {
        return Ok(DeviceInfo::default());
    }
    let id = client.rpc_id()?;
    let envelope = client.request(
        &format!("home/rpc/{}", device.did),
        &format!(r#"{{"id":{id},"method":"miIO.info","params":[]}}"#),
    )?;
    let result = &envelope["result"];
    if !result.is_object() {
        return Ok(DeviceInfo::default());
    }
    let uptime_seconds = result["life"]
        .as_number()
        .filter(|n| n.is_i64() || n.is_u64())
        .cloned();
    Ok(DeviceInfo {
        uptime_seconds,
        firmware_version: text(&result["fw_ver"]).or_else(|| text(&result["fw_version"])),
        model: text(&result["model"]),
    })
}

pub fn ap01(client: &Client<'_>) -> Result<Ap01Report, MiCloudError> {
    let device = find_ap01(client)?;
    let info = device_info(client, &device)?;
    let firmware_version = device.firmware_version.or(info.firmware_version);
    Client::check_public(&device.model)?;
    if let Some(version) = &firmware_version {
        Client::check_public(version)?;
    }
    Ok(Ap01Report {
        ok: true,
        model: device.model,
        firmware_version,
        online: device.online,
        uptime_seconds: info.uptime_seconds,
    })
}

#[cfg(test)]
#[path = "device_tests.rs"]
mod tests;
