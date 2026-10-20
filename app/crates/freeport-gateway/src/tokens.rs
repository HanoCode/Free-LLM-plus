//! OAuth 凭据的保管与续期。
//!
//! 两个容易写错的地方，这里各用一条机制钉住：
//!
//! 1. **并发刷新**：Copilot token 只有 30 分钟，一次冷启动可能有十几个请求
//!    同时撞上过期。每凭据一把异步锁 + 拿锁后二次检查，保证只换一次；
//!    否则会被上游当成刷接口，甚至把 refresh token 用废。
//! 2. **落盘时机**：换到新 token 必须立刻写回密钥库。晚一步崩了，下次
//!    启动还是旧 token，又白换一次。

use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};

use freeport_auth::device::{self, DeviceCfg};
use freeport_auth::{copilot, AuthError, TokenSet, REFRESH_SKEW_SECS};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::secrets;

/// 存进密钥库的那坨 JSON。把「怎么刷新」也一起存下来，
/// 这样续期不依赖内存状态，进程重启后照样能自己换。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthBlob {
    /// `copilot`（换 token 式）或 `oauth`（refresh_token 式）
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login: Option<String>,
    pub tokens: TokenSet,
}

#[derive(Debug, Clone)]
pub struct Endpoints {
    pub copilot_token_url: String,
    pub device_code_url: String,
    pub github_token_url: String,
    pub github_user_url: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            copilot_token_url: copilot::COPILOT_TOKEN_URL.into(),
            device_code_url: copilot::DEVICE_CODE_URL.into(),
            github_token_url: copilot::TOKEN_URL.into(),
            github_user_url: copilot::USER_URL.into(),
        }
    }
}

impl Endpoints {
    /// 测试用：全部指向同一个假 IdP。
    pub fn pointing_at(base: &str) -> Self {
        Self {
            copilot_token_url: format!("{base}/copilot/token"),
            device_code_url: format!("{base}/login/device/code"),
            github_token_url: format!("{base}/login/oauth/access_token"),
            github_user_url: format!("{base}/user"),
        }
    }
}

pub struct Broker {
    client: reqwest::Client,
    endpoints: Endpoints,
    /// 每凭据一把锁：同一时刻只允许一个刷新在飞
    locks: StdMutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl Broker {
    pub fn new(client: reqwest::Client, endpoints: Endpoints) -> Self {
        Self {
            client,
            endpoints,
            locks: StdMutex::new(HashMap::new()),
        }
    }

    pub fn endpoints(&self) -> &Endpoints {
        &self.endpoints
    }

    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

    /* ── 存取 ─────────────────────────────────── */

    pub fn load(account_id: &str) -> Option<OAuthBlob> {
        let raw = secrets::get_oauth(account_id)?;
        serde_json::from_str(&raw).ok()
    }

    pub fn save(account_id: &str, blob: &OAuthBlob) -> Result<(), String> {
        let raw = serde_json::to_string(blob).map_err(|e| format!("凭据序列化失败：{e}"))?;
        secrets::set_oauth(account_id, &raw)
    }

    /// 退出登录：只清 token，不动同一凭据下的 API key。
    pub fn clear(account_id: &str) {
        secrets::delete_oauth(account_id);
    }

    /// 界面上用来显示「还剩多久」「谁的号」。
    pub fn summary(account_id: &str) -> Option<(String, Option<i64>, Option<String>)> {
        let blob = Self::load(account_id)?;
        Some((
            blob.kind,
            blob.tokens.expires_at,
            blob.tokens.account.or(blob.login),
        ))
    }

    /* ── 续期 ─────────────────────────────────── */

    /// 拿一枚当前可用的 token。快过期就换，换完立刻落盘。
    pub async fn token(&self, account_id: &str) -> Result<TokenSet, String> {
        let blob = Self::load(account_id).ok_or_else(|| "这条凭据还没有授权，请先登录".to_string())?;
        let now = chrono::Utc::now().timestamp();
        if !blob.tokens.needs_refresh(now, REFRESH_SKEW_SECS) {
            return Ok(blob.tokens);
        }

        let gate = self.lock_for(account_id);
        let _guard = gate.lock().await;

        // 等锁期间可能已经被别人换好了，再看一眼
        let blob = Self::load(account_id).ok_or_else(|| "凭据在刷新期间被删除".to_string())?;
        let now = chrono::Utc::now().timestamp();
        if !blob.tokens.needs_refresh(now, REFRESH_SKEW_SECS) {
            return Ok(blob.tokens);
        }

        let next = self.refresh(account_id, &blob, now).await?;
        Self::save(
            account_id,
            &OAuthBlob {
                tokens: next.clone(),
                ..blob
            },
        )?;
        tracing::info!(
            account = %account_id,
            expires_at = ?next.expires_at,
            "OAuth 凭据已续期"
        );
        Ok(next)
    }

    async fn refresh(
        &self,
        account_id: &str,
        blob: &OAuthBlob,
        now: i64,
    ) -> Result<TokenSet, String> {
        let result = match blob.kind.as_str() {
            "copilot" => {
                copilot::refresh_at(
                    &self.client,
                    &self.endpoints.copilot_token_url,
                    &blob.tokens,
                    now,
                )
                .await
            }
            _ => {
                let token_url = blob
                    .token_url
                    .clone()
                    .ok_or_else(|| "这条凭据没记录刷新地址，只能重新登录".to_string())?;
                let refresh_token = blob
                    .tokens
                    .refresh_token
                    .clone()
                    .ok_or_else(|| "上游没发 refresh_token，只能重新登录".to_string())?;
                let cfg = DeviceCfg {
                    device_code_url: String::new(),
                    token_url,
                    client_id: blob.client_id.clone().unwrap_or_default(),
                    scope: String::new(),
                };
                device::refresh(&self.client, &cfg, &refresh_token, now).await
            }
        };

        result.map_err(|e| match e {
            AuthError::Denied => {
                // 刷新被拒 = 这枚 refresh token 废了，留着只会反复失败
                Self::clear(account_id);
                format!("{}（已清除本地凭据，请重新登录）", e)
            }
            other => other.to_string(),
        })
    }

    fn lock_for(&self, account_id: &str) -> Arc<Mutex<()>> {
        let mut map = self.locks.lock().unwrap_or_else(|e| e.into_inner());
        map.entry(account_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_roundtrips_through_json() {
        let mut tokens = TokenSet::new("at", Some(1_700_000_000));
        tokens.refresh_token = Some("rt".into());
        let blob = OAuthBlob {
            kind: "copilot".into(),
            token_url: None,
            client_id: None,
            login: Some("octocat".into()),
            tokens,
        };
        let raw = serde_json::to_string(&blob).unwrap();
        let back: OAuthBlob = serde_json::from_str(&raw).unwrap();
        assert_eq!(back.kind, "copilot");
        assert_eq!(back.login.as_deref(), Some("octocat"));
        assert_eq!(back.tokens, blob.tokens);
    }

    #[test]
    fn lock_is_stable_per_account() {
        let broker = Broker::new(reqwest::Client::new(), Endpoints::default());
        let a = broker.lock_for("acct_1");
        let b = broker.lock_for("acct_1");
        let c = broker.lock_for("acct_2");
        assert!(Arc::ptr_eq(&a, &b), "同一凭据必须拿到同一把锁");
        assert!(!Arc::ptr_eq(&a, &c));
    }
}
