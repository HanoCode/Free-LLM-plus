//! 数据面。GUI 宿主与 headless daemon 共用这一份实现。

pub mod anthropic;
pub mod cred;
pub mod gemini;
pub mod hold;
pub mod keys;
pub mod license;
pub mod ollama;
pub mod providers;
pub mod responses;
pub mod sanitize;
pub mod secrets;
pub mod server;
pub mod sig;
pub mod store;
pub mod tokens;
pub mod transfer;

pub use server::Gateway;
