//! OAuth 2.0 设备码流程（RFC 8628）。
//!
//! 为什么首选设备码而不是 PKCE 回调：桌面端没有稳定的 redirect_uri，
//! 起本地端口容易被防火墙挡、被别的进程占。设备码让用户在浏览器里登录，
//! 我们只负责「拿到 code 之后轮询」，链路最短、失败模式最少。
//!
//! 轮询必须守规矩：`slow_down` 就把间隔加 5 秒（RFC 要求），
//! 否则会被上游限流甚至封设备码。

use serde::Deserialize;
use serde_json::Value;

use crate::{AuthError, TokenSet};

#[derive(Debug, Clone)]
pub struct DeviceCfg {
    pub device_code_url: String,
    pub token_url: String,
    pub client_id: String,
    /// 多个 scope 用空格分隔；上游不给就留空
    pub scope: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceStart {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    /// 带 code 的完整地址（GitHub 会回这个），有就直接打开，省得用户手抄
    pub verification_uri_complete: Option<String>,
    /// 建议轮询间隔（秒）
    pub interval: u64,
    pub expires_in: u64,
}

impl DeviceStart {
    /// 给用户看的「请在浏览器打开 xxx 并输入 ABCD-EFGH」。
    pub fn instruction(&self) -> String {
        match &self.verification_uri_complete {
            Some(url) => format!("在浏览器打开 {url} 完成授权"),
            None => format!("在浏览器打开 {} 并输入代码 {}", self.verification_uri, self.user_code),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceStep {
    /// 用户还没点完
    Pending,
    /// 轮询太快，下次要拉长间隔
    SlowDown,
    Ready(TokenSet),
    Denied,
    Expired,
}

#[derive(Deserialize)]
struct StartResponse {
    device_code: String,
    user_code: String,
    #[serde(default)]
    verification_uri: String,
    #[serde(default)]
    verification_uri_complete: Option<String>,
    #[serde(default)]
    interval: Option<u64>,
    #[serde(default)]
    expires_in: Option<u64>,
}

#[derive(Deserialize)]
struct TokenResponse {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    token_type: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

/// 申领设备码。用户拿 `user_code` 去浏览器里输入。
pub async fn start(client: &reqwest::Client, cfg: &DeviceCfg) -> Result<DeviceStart, AuthError> {
    if cfg.client_id.trim().is_empty() {
        return Err(AuthError::Config("缺少 client_id".into()));
    }
    let body = [
        ("client_id", cfg.client_id.as_str()),
        ("scope", cfg.scope.as_str()),
    ];
    let value = post_form(client, &cfg.device_code_url, &body).await?;

    let parsed: StartResponse = serde_json::from_value(value).map_err(|e| {
        AuthError::Rejected {
            status: 200,
            code: "bad_response".into(),
            detail: format!("设备码响应无法解析：{e}"),
        }
    })?;

    Ok(DeviceStart {
        device_code: parsed.device_code,
        user_code: parsed.user_code,
        verification_uri: parsed.verification_uri,
        verification_uri_complete: parsed.verification_uri_complete,
        // RFC 要求缺省 5 秒；不能更短，否则第一批轮询就会吃 slow_down
        interval: parsed.interval.unwrap_or(5).max(1),
        expires_in: parsed.expires_in.unwrap_or(900),
    })
}

/// 轮询一次。`now` 用来把上游的 `expires_in` 换算成绝对时间。
pub async fn poll_once(
    client: &reqwest::Client,
    cfg: &DeviceCfg,
    device_code: &str,
    now: i64,
) -> Result<DeviceStep, AuthError> {
    let body = [
        ("client_id", cfg.client_id.as_str()),
        ("device_code", device_code),
        ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
    ];
    let value = post_form(client, &cfg.token_url, &body).await?;
    Ok(step_from(value, now))
}

/// 一直轮询到出结果。`deadline` 之后放弃，避免界面卡在「等待授权」。
pub async fn wait(
    client: &reqwest::Client,
    cfg: &DeviceCfg,
    start: &DeviceStart,
    deadline_secs: u64,
) -> Result<TokenSet, AuthError> {
    let mut interval = start.interval;
    let mut waited = 0u64;
    let limit = deadline_secs.min(start.expires_in.max(30));

    loop {
        if waited >= limit {
            return Err(AuthError::Expired);
        }
        tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
        waited += interval;

        let now = chrono::Utc::now().timestamp();
        match poll_once(client, cfg, &start.device_code, now).await? {
            DeviceStep::Ready(ts) => return Ok(ts),
            DeviceStep::Denied => return Err(AuthError::Denied),
            DeviceStep::Expired => return Err(AuthError::Expired),
            DeviceStep::SlowDown => interval += 5,
            DeviceStep::Pending => {}
        }
    }
}

/// 标准 OAuth 刷新（有 refresh_token 的厂商走这条）。
pub async fn refresh(
    client: &reqwest::Client,
    cfg: &DeviceCfg,
    refresh_token: &str,
    now: i64,
) -> Result<TokenSet, AuthError> {
    let body = [
        ("client_id", cfg.client_id.as_str()),
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
    ];
    let value = post_form(client, &cfg.token_url, &body).await?;
    match step_from(value, now) {
        DeviceStep::Ready(ts) => Ok(ts),
        DeviceStep::Denied | DeviceStep::Expired => Err(AuthError::Denied),
        // 刷新接口回 pending/slow_down 属于上游异常，别当成「再等等」
        _ => Err(AuthError::Rejected {
            status: 200,
            code: "unexpected_pending".into(),
            detail: "刷新接口返回了等待状态".into(),
        }),
    }
}

fn step_from(value: Value, now: i64) -> DeviceStep {
    let parsed: TokenResponse = match serde_json::from_value(value) {
        Ok(v) => v,
        Err(_) => {
            return DeviceStep::Pending;
        }
    };

    if let Some(err) = parsed.error.as_deref() {
        return match err {
            "authorization_pending" => DeviceStep::Pending,
            "slow_down" => DeviceStep::SlowDown,
            "expired_token" => DeviceStep::Expired,
            "access_denied" => DeviceStep::Denied,
            other => {
                tracing::warn!(error = other, detail = ?parsed.error_description, "设备码轮询失败");
                DeviceStep::Denied
            }
        };
    }

    match parsed.access_token {
        Some(token) if !token.trim().is_empty() => DeviceStep::Ready(TokenSet {
            access_token: token,
            token_type: parsed.token_type.unwrap_or_else(|| "bearer".into()),
            refresh_token: parsed.refresh_token.filter(|s| !s.trim().is_empty()),
            scope: parsed.scope,
            expires_at: parsed.expires_in.map(|secs| now + secs),
            account: None,
        }),
        _ => DeviceStep::Pending,
    }
}

/// 表单 POST + JSON 解析。GitHub 默认回 `application/x-www-form-urlencoded`，
/// 必须显式要 JSON，否则解析会静默失败成 pending。
async fn post_form(
    client: &reqwest::Client,
    url: &str,
    body: &[(&str, &str)],
) -> Result<Value, AuthError> {
    let res = client
        .post(url)
        .header("accept", "application/json")
        .form(body)
        .send()
        .await
        .map_err(|e| AuthError::Transport(e.to_string()))?;

    let status = res.status().as_u16();
    let text = res.text().await.map_err(|e| AuthError::Transport(e.to_string()))?;

    match serde_json::from_str::<Value>(&text) {
        Ok(value) => {
            // 4xx 且带 error 字段：上游的明确拒绝，不要当网络错误重试
            if !(200..300).contains(&status) && value.get("error").is_none() {
                return Err(AuthError::Rejected {
                    status,
                    code: "http_error".into(),
                    detail: text.chars().take(300).collect(),
                });
            }
            Ok(value)
        }
        Err(_) => {
            if (200..300).contains(&status) {
                // 200 但不是 JSON：多半是上游改了协议，如实报出来
                Err(AuthError::Rejected {
                    status,
                    code: "not_json".into(),
                    detail: text.chars().take(300).collect(),
                })
            } else {
                Err(AuthError::Rejected {
                    status,
                    code: "http_error".into(),
                    detail: text.chars().take(300).collect(),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn pending_and_slow_down_are_not_terminal() {
        assert_eq!(step_from(json!({ "error": "authorization_pending" }), 0), DeviceStep::Pending);
        assert_eq!(step_from(json!({ "error": "slow_down" }), 0), DeviceStep::SlowDown);
        assert_eq!(step_from(json!({ "error": "access_denied" }), 0), DeviceStep::Denied);
        assert_eq!(step_from(json!({ "error": "expired_token" }), 0), DeviceStep::Expired);
    }

    #[test]
    fn expires_in_becomes_absolute_time() {
        let step = step_from(
            json!({ "access_token": "at", "token_type": "bearer", "expires_in": 3600 }),
            1_000,
        );
        match step {
            DeviceStep::Ready(ts) => {
                assert_eq!(ts.expires_at, Some(4_600));
                assert_eq!(ts.token_type, "bearer");
            }
            other => panic!("期望 Ready，实际 {other:?}"),
        }
    }

    #[test]
    fn empty_token_is_treated_as_pending() {
        assert_eq!(step_from(json!({ "access_token": "  " }), 0), DeviceStep::Pending);
    }

    #[test]
    fn instruction_prefers_complete_uri() {
        let start = DeviceStart {
            device_code: "dc".into(),
            user_code: "ABCD-EFGH".into(),
            verification_uri: "https://github.com/login/device".into(),
            verification_uri_complete: Some("https://github.com/login/device?code=ABCD-EFGH".into()),
            interval: 5,
            expires_in: 900,
        };
        assert!(start.instruction().contains("?code=ABCD-EFGH"));

        let plain = DeviceStart {
            verification_uri_complete: None,
            ..start
        };
        assert!(plain.instruction().contains("ABCD-EFGH"));
    }
}
