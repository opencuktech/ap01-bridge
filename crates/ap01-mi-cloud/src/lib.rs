//! 云端客户端的独立边界、凭据来源与协议原语。

pub mod client;
pub mod credentials;
pub mod crypto;
pub mod device;
mod error;
pub mod redact;
pub mod session;
pub mod transport;
pub use error::MiCloudError;

#[cfg(any(test, feature = "testkit"))]
pub mod golden;

#[cfg(any(test, feature = "testkit"))]
pub mod testkit;
