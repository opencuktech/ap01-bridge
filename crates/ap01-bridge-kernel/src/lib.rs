//! AP01 桥接内核，环境变量和时间由调用方注入。

pub mod error;
pub mod events;
pub mod gif;
pub mod paths;
pub mod serve;
pub mod store;
pub mod time;

pub use error::KernelError;
