//! Freeport 的纯逻辑内核。
//!
//! 这一层不做任何 IO：没有文件、没有网络、没有时钟以外的副作用。
//! 路由、账本、错误分类都在这里，因此可以被完整单测，
//! 也能同时被 GUI 宿主与 headless daemon 复用。

pub mod affinity;
pub mod cooldown;
pub mod dto;
pub mod error;
pub mod model;
pub mod presets;
pub mod quota;
pub mod route;

pub use affinity::{Affinity, Stick};
pub use cooldown::{CooldownSnapshot, Cooldowns};
pub use error::{classify, ErrorClass, Scope};
pub use model::*;
pub use quota::{Ledger, WindowKind};
pub use route::{
    canary_factor, plan, stick_to_front, weight_factors, weight_of, Candidate, Plan, Rejected, Swrr,
};
