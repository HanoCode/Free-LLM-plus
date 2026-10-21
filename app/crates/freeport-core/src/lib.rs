//! Freeport 的纯逻辑内核。
//!
//! 这一层不做任何 IO：没有文件、没有网络、没有时钟以外的副作用。
//! 路由、账本、错误分类都在这里，因此可以被完整单测，
//! 也能同时被 GUI 宿主与 headless daemon 复用。

pub mod affinity;
pub mod cooldown;
pub mod digest;
pub mod dto;
pub mod error;
pub mod model;
pub mod observe;
pub mod presets;
pub mod quota;
pub mod route;
pub mod score;

pub use affinity::{Affinity, Stick};
pub use cooldown::{CooldownSnapshot, Cooldowns};
pub use error::{classify, ErrorClass, Scope};
pub use model::*;
pub use quota::{Ledger, WindowKind};
pub use route::{
    blend_score, canary_factor, intelligence_factor, plan, reliability_factor, speed_factor,
    stick_to_front, weight_factors, weight_of, weight_of_model, Candidate, Plan, Rejected,
    Strategy, Swrr,
};
