//! 凭据代理。只负责「怎么拿到一枚能用的 token、什么时候必须换掉」，
//! 不参与任何业务请求的拼装。
//!
//! 三条约束贯穿全模块：
//!
//! 1. **公共客户端**：只用 `client_id` + PKCE / 设备码，绝不内嵌 `client_secret`。
//! 2. **不伪造身份**：设备码是用户在自己浏览器里登录自己的账号，我们不碰密码、
//!    不复制别人的 Cookie。
//! 3. **时间由外部注入**：`now` 一律作为参数传进来，单测不依赖真实时钟。

pub mod cookie;
pub mod copilot;
pub mod device;
pub mod pkce;

use serde::{Deserialize, Serialize};

/// 提前多久就认为「该换了」。上游时钟和本机时钟常有几十秒偏差，
/// 留 5 分钟既够覆盖偏差，也不至于把 30 分钟的 token 换得太勤。
pub const REFRESH_SKEW_SECS: i64 = 300;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthError {
    /// 网络层失败（DNS、超时、TLS）
    Transport(String),
    /// 上游明确拒绝：4xx + 错误码
    Rejected { status: u16, code: String, detail: String },
    /// 用户点了拒绝，或设备码已过期
    Denied,
    Expired,
    /// 配置问题（缺 client_id、地址为空）
    Config(String),
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthError::Transport(e) => write!(f, "无法连接授权服务：{e}"),
            AuthError::Rejected { status, code, detail } => {
                write!(f, "授权被拒绝（HTTP {status} {code}）：{detail}")
            }
            AuthError::Denied => write!(f, "授权被拒绝：用户取消或设备码已失效"),
            AuthError::Expired => write!(f, "设备码已过期，请重新发起登录"),
            AuthError::Config(e) => write!(f, "授权配置有误：{e}"),
        }
    }
}

impl std::error::Error for AuthError {}

/// 一枚可用的凭据。
///
/// `expires_at` 是**绝对时间**（unix 秒）。上游不给过期时间时为 `None`——
/// 那种情况只能靠 401 兜底刷新，别假装知道它什么时候过期。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenSet {
    pub access_token: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub token_type: String,
    /// 续期用。标准 OAuth 是 refresh_token；Copilot 这类是「长期的上游 token」。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    /// 上游账号标识（登录名、用户名），用于界面上区分「这是谁的号」
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
}

impl TokenSet {
    pub fn new(access_token: impl Into<String>, expires_at: Option<i64>) -> Self {
        Self {
            access_token: access_token.into(),
            token_type: "bearer".into(),
            expires_at,
            ..Default::default()
        }
    }

    /// 是不是已经该换了。没有过期时间的一律返回 false——
    /// 换成「猜一个 1 小时」会让长任务在中途被自己掐断。
    pub fn needs_refresh(&self, now: i64, skew: i64) -> bool {
        match self.expires_at {
            Some(at) => at - skew <= now,
            None => false,
        }
    }

    /// 还剩多少秒。`None` 表示上游没给过期时间。
    pub fn expires_in(&self, now: i64) -> Option<i64> {
        self.expires_at.map(|at| (at - now).max(0))
    }

    pub fn is_empty(&self) -> bool {
        self.access_token.trim().is_empty()
    }

    /// 发请求用的头。GitHub / Copilot 一律 `Bearer`，不搞 `token` 前缀那套。
    pub fn authorization(&self) -> String {
        format!("Bearer {}", self.access_token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_window_is_inclusive_of_skew() {
        let ts = TokenSet::new("t", Some(1_000));
        assert!(!ts.needs_refresh(600, REFRESH_SKEW_SECS)); // 还早
        assert!(ts.needs_refresh(700, REFRESH_SKEW_SECS)); // 剩 300s，正好进窗口
        assert!(ts.needs_refresh(1_001, REFRESH_SKEW_SECS)); // 已过期
    }

    #[test]
    fn unknown_expiry_never_forces_refresh() {
        let ts = TokenSet::new("t", None);
        assert!(!ts.needs_refresh(i64::MAX / 2, REFRESH_SKEW_SECS));
        assert_eq!(ts.expires_in(1_000), None);
    }

    #[test]
    fn token_roundtrips_through_json() {
        let mut ts = TokenSet::new("at", Some(42));
        ts.refresh_token = Some("rt".into());
        ts.account = Some("octocat".into());
        let raw = serde_json::to_string(&ts).unwrap();
        assert!(raw.contains("\"expiresAt\":42"));
        let back: TokenSet = serde_json::from_str(&raw).unwrap();
        assert_eq!(back, ts);
    }
}
