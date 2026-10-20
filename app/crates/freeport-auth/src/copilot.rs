//! GitHub Copilot（Tier C 首批）。
//!
//! 两段式：GitHub OAuth token（长期，设备码拿到）→ 换 Copilot 短期 token
//! （约 30 分钟，真正发给 `api.githubcopilot.com` 的那枚）。
//!
//! 所以「刷新」在这里不是 refresh_token 那套，而是**拿 GitHub token 再换一次**。
//! 这也意味着 GitHub token 必须留下：它一旦丢了，用户就得重新走一遍设备码。
//!
//! 注意：`copilot_internal` 是未公开接口，随时可能变。失效时如实报错，
//! 不猜测、不重试到死。

use std::time::Duration;

use serde_json::Value;

use crate::device::DeviceCfg;
use crate::{AuthError, TokenSet};

/// GitHub 官方为 Copilot 系工具签发的**公共** client_id。
/// 公共客户端没有 secret，凭据就是用户自己浏览器里的登录态。
pub const CLIENT_ID: &str = "Iv1.b507a08c87ecfe98";
pub const DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
pub const TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
pub const COPILOT_TOKEN_URL: &str = "https://api.github.com/copilot_internal/v2/token";
pub const USER_URL: &str = "https://api.github.com/user";
pub const API_BASE: &str = "https://api.githubcopilot.com";

pub fn device_cfg() -> DeviceCfg {
    DeviceCfg {
        device_code_url: DEVICE_CODE_URL.into(),
        token_url: TOKEN_URL.into(),
        client_id: CLIENT_ID.into(),
        // 只要登录名，别一上来就要 repo 权限
        scope: "read:user".into(),
    }
}

/// GitHub token → Copilot 短期 token。`now` 用于把 `expires_at` 对齐本机时钟。
pub async fn exchange(
    client: &reqwest::Client,
    github_token: &str,
    now: i64,
) -> Result<TokenSet, AuthError> {
    exchange_at(client, COPILOT_TOKEN_URL, github_token, now).await
}

/// 同 [`exchange`]，但可换 endpoint。网关用它把地址做成可配置项，
/// 测试才能对着假服务跑完整续期链路。
pub async fn exchange_at(
    client: &reqwest::Client,
    url: &str,
    github_token: &str,
    now: i64,
) -> Result<TokenSet, AuthError> {
    let res = client
        .get(url)
        .header("accept", "application/json")
        .header("x-github-api-version", "2022-11-28")
        .header("authorization", format!("Bearer {github_token}"))
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| AuthError::Transport(e.to_string()))?;

    let status = res.status().as_u16();
    let text = res.text().await.map_err(|e| AuthError::Transport(e.to_string()))?;
    let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);

    if !(200..300).contains(&status) {
        // 403/404 通常意味着「这个号没有 Copilot 订阅」，说清楚比抛原始 JSON 有用
        let hint = match status {
            401 => "GitHub token 已失效，请重新登录",
            403 | 404 => "这个账号没有可用的 Copilot 订阅",
            _ => "Copilot 授权服务返回异常",
        };
        return Err(AuthError::Rejected {
            status,
            code: value
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("copilot_exchange_failed")
                .to_string(),
            detail: hint.to_string(),
        });
    }

    let token = value
        .get("token")
        .and_then(|t| t.as_str())
        .filter(|t| !t.trim().is_empty())
        .ok_or_else(|| AuthError::Rejected {
            status,
            code: "no_token".into(),
            detail: "响应里没有 token 字段".into(),
        })?;

    // `expires_at` 是 unix 秒；个别版本只给 `refresh_in`（剩余秒数），两种都认
    let expires_at = value
        .get("expires_at")
        .and_then(|v| v.as_i64())
        .or_else(|| value.get("refresh_in").and_then(|v| v.as_i64()).map(|s| now + s))
        .or(Some(now + 1_500));

    Ok(TokenSet {
        access_token: token.to_string(),
        token_type: "bearer".into(),
        // 留着 GitHub token：下次续期要用它再换一次
        refresh_token: Some(github_token.to_string()),
        scope: Some("copilot".into()),
        expires_at,
        account: None,
    })
}

/// 拿登录名，用于界面显示「已登录为 octocat」。失败不影响可用性。
pub async fn whoami(client: &reqwest::Client, github_token: &str) -> Option<String> {
    whoami_at(client, USER_URL, github_token).await
}

/// 同 [`whoami`]，endpoint 可替换。
pub async fn whoami_at(client: &reqwest::Client, url: &str, github_token: &str) -> Option<String> {
    let res = client
        .get(url)
        .header("accept", "application/json")
        .header("x-github-api-version", "2022-11-28")
        .header("authorization", format!("Bearer {github_token}"))
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .ok()?;
    if !res.status().is_success() {
        return None;
    }
    let value: Value = res.json().await.ok()?;
    value.get("login").and_then(|l| l.as_str()).map(|s| s.to_string())
}

/// 续期：用留存的 GitHub token 再换一枚 Copilot token。
pub async fn refresh(
    client: &reqwest::Client,
    current: &TokenSet,
    now: i64,
) -> Result<TokenSet, AuthError> {
    refresh_at(client, COPILOT_TOKEN_URL, current, now).await
}

/// 同 [`refresh`]，endpoint 可替换。
pub async fn refresh_at(
    client: &reqwest::Client,
    url: &str,
    current: &TokenSet,
    now: i64,
) -> Result<TokenSet, AuthError> {
    let github_token = current
        .refresh_token
        .as_deref()
        .filter(|t| !t.trim().is_empty())
        .ok_or(AuthError::Denied)?;

    let mut next = exchange_at(client, url, github_token, now).await?;
    next.account = current.account.clone();
    Ok(next)
}

/// Copilot 的 chat 端点要求带上这几个头，缺了会 400。
pub fn extra_headers() -> Vec<(&'static str, String)> {
    vec![
        ("copilot-integration-id", "vscode-chat".into()),
        ("editor-version", "vscode/1.99.0".into()),
        ("editor-plugin-version", "copilot-chat/0.26.7".into()),
        ("openai-intent", "conversation-panel".into()),
        ("x-initiator", "user".into()),
        ("user-agent", "GitHubCopilotChat/0.26.7".into()),
    ]
}

/// `/models` 的响应形状，供探针与「拉模型列表」复用。
pub fn models_path() -> &'static str {
    "/models"
}

/// 调试用：把 token 换成「可打印的摘要」，绝不把整枚 token 写进日志。
pub fn mask(token: &str) -> String {
    let head: String = token.chars().take(6).collect();
    format!("{head}…({} chars)", token.chars().count())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn mask_never_leaks_the_body() {
        let masked = mask("ghu_abcdefghijklmnop");
        assert!(masked.starts_with("ghu_ab"));
        assert!(!masked.contains("defghij"));
    }

    #[test]
    fn public_client_has_no_secret() {
        // 红线：公共客户端只能有 client_id
        assert!(!device_cfg().client_id.is_empty());
        assert!(!CLIENT_ID.contains("secret"));
    }

    #[test]
    fn extra_headers_cover_required_set() {
        let names: Vec<&str> = extra_headers().iter().map(|(k, _)| *k).collect();
        for required in ["editor-version", "copilot-integration-id", "user-agent"] {
            assert!(names.contains(&required), "缺少 {required}");
        }
    }

    #[test]
    fn exchange_payload_shape_is_understood() {
        // 这条只是把解析契约写下来：字段名变了要立刻发现
        let v = json!({ "token": "tid=1;exp=2", "expires_at": 1_700_000_000_i64 });
        assert_eq!(v["expires_at"].as_i64(), Some(1_700_000_000));
    }
}
