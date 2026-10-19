//! 数据面。GUI 宿主与 headless daemon 共用这一份实现。

pub mod providers;
pub mod secrets;
pub mod server;
pub mod store;

pub use server::Gateway;
