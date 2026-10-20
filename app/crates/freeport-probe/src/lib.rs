//! 探针引擎与巡检调度。
//!
//! 三个独立的小模块，谁也不知道 HTTP 长什么样：
//! - `probe`：发请求，产出事实
//! - `health`：把事实折进状态机
//! - `budget` / `patrol`：决定什么时候、对谁发

pub mod budget;
pub mod health;
pub mod patrol;
pub mod probe;

pub use probe::{capability, liveness, CapsOutcome, ProbeOutcome};
