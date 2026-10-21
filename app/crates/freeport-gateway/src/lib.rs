//! 数据面。GUI 宿主与 headless daemon 共用这一份实现。

pub mod anthropic;
pub mod cache;
pub mod completions;
pub mod compress;
pub mod control;
pub mod cred;
pub mod exts;
pub mod gemini;
pub mod hold;
pub mod idem;
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
pub mod upwire;
pub mod usage;

pub use server::Gateway;
