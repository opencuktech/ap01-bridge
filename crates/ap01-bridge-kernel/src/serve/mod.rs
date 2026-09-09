//! 面向设备的同步 HTTP/1.0 服务，时钟与事件出口由调用方注入。

pub mod http;
pub mod response;
pub mod server;

pub use server::{ServeConfig, Server};
