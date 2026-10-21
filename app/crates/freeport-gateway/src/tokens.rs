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
use freeport_auth::sub::{self, SubCfg, Vendor};
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
    /// 把 `tokens.account_id` 发成哪个请求头。`None` = 不发。
    ///
    /// 存在数据里而不是按 `kind` 硬编码：`kind` 说的是「怎么刷新」，
    /// 这个是「账号标识叫什么头」，是两件事。将来多一家订阅厂商时，
    /// 只改登录时写进去的值，不改刷新路径。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_header: Option<String>,
    pub tokens: TokenSet,
}

/// 一条 OAuth 凭据「此刻能用」的样子。
pub struct Live {
    pub tokens: TokenSet,
    /// `copilot` / `claude` / `codex` / `oauth`
    pub kind: String,
    /// 账号标识该发成哪个头（Codex 是 `chatgpt-account-id`）
    pub account_header: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Endpoints {
    pub copilot_token_url: String,
    pub device_code_url: String,
    pub github_token_url: String,
    pub github_user_url: String,
    /// 订阅登录的四个地址。做成字段而不是写死常量，是为了让测试能对着
    /// 本地假 IdP 跑完整链路——线上永远指向官方地址。
    pub claude_authorize_url: String,
    pub claude_token_url: String,
    pub claude_profile_url: String,
    pub codex_authorize_url: String,
    pub codex_token_url: String,
    /// 官方额度端点。同样做成字段，测试才能对着假上游跑。
    pub claude_usage_url: String,
    pub codex_usage_url: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            copilot_token_url: copilot::COPILOT_TOKEN_URL.into(),
            device_code_url: copilot::DEVICE_CODE_URL.into(),
            github_token_url: copilot::TOKEN_URL.into(),
            github_user_url: copilot::USER_URL.into(),
            claude_authorize_url: sub::CLAUDE_AUTHORIZE_URL.into(),
            claude_token_url: sub::CLAUDE_TOKEN_URL.into(),
            claude_profile_url: sub::CLAUDE_PROFILE_URL.into(),
            codex_authorize_url: sub::CODEX_AUTHORIZE_URL.into(),
            codex_token_url: sub::CODEX_TOKEN_URL.into(),
            claude_usage_url: sub::CLAUDE_USAGE_URL.into(),
            codex_usage_url: sub::CODEX_USAGE_URL.into(),
        }
    }
}

impl Endpoints {
    /// 某个订阅厂商的端点。`client_id` 与 scopes 永远用官方常量：
    /// 那是厂商发给公共客户端的，不该跟部署环境一起变。
    pub fn sub_cfg(&self, vendor: Vendor) -> SubCfg {
        let mut cfg = vendor.cfg();
        match vendor {
            Vendor::Claude => {
                cfg.authorize_url = self.claude_authorize_url.clone();
                cfg.token_url = self.claude_token_url.clone();
            }
            Vendor::Codex => {
                cfg.authorize_url = self.codex_authorize_url.clone();
                cfg.token_url = self.codex_token_url.clone();
            }
        }
        cfg
    }

    /// 测试用：全部指向同一个假 IdP。
    pub fn pointing_at(base: &str) -> Self {
        Self {
            copilot_token_url: format!("{base}/copilot/token"),
            device_code_url: format!("{base}/login/device/code"),
            github_token_url: format!("{base}/login/oauth/access_token"),
            github_user_url: format!("{base}/user"),
            claude_authorize_url: format!("{base}/claude/authorize"),
            claude_token_url: format!("{base}/claude/token"),
            claude_profile_url: format!("{base}/claude/profile"),
            codex_authorize_url: format!("{base}/codex/authorize"),
            codex_token_url: format!("{base}/codex/token"),
            claude_usage_url: format!("{base}/claude/usage"),
            codex_usage_url: format!("{base}/codex/usage"),
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
    ///
    /// 连 `kind` / `account_header` 一起返回，调用方不用再读一遍密钥库——
    /// 那是解密，出现在每个请求的热路径上。
    pub async fn token(&self, account_id: &str) -> Result<Live, String> {
        let blob = Self::load(account_id).ok_or_else(|| "这条凭据还没有授权，请先登录".to_string())?;
        let now = chrono::Utc::now().timestamp();
        if !blob.tokens.needs_refresh(now, REFRESH_SKEW_SECS) {
            return Ok(live_of(&blob));
        }

        let gate = self.lock_for(account_id);
        let _guard = gate.lock().await;

        // 等锁期间可能已经被别人换好了，再看一眼
        let blob = Self::load(account_id).ok_or_else(|| "凭据在刷新期间被删除".to_string())?;
        let now = chrono::Utc::now().timestamp();
        if !blob.tokens.needs_refresh(now, REFRESH_SKEW_SECS) {
            return Ok(live_of(&blob));
        }

        let next = self.refresh(account_id, &blob, now).await?;
        // 先把要回传的元数据取出来：构造新 blob 时 `..blob` 会把整个 blob 移走
        let kind = blob.kind.clone();
        let account_header = blob.account_header.clone();
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
        Ok(Live {
            tokens: next,
            kind,
            account_header,
        })
    }

    async fn refresh(
        &self,
        account_id: &str,
        blob: &OAuthBlob,
        now: i64,
    ) -> Result<TokenSet, String> {
        let result = match blob.kind.as_str() {
            // 订阅号：按厂商自己的编码换 token（Claude 要 JSON，Codex 要表单）。
            // 编码弄反了上游只回一个含糊的 400，所以这条分派必须显式写出来。
            kind @ ("claude" | "codex") => match Vendor::parse(kind) {
                Some(vendor) => {
                    sub::refresh(&self.client, &self.endpoints.sub_cfg(vendor), &blob.tokens, now)
                        .await
                }
                None => Err(AuthError::Config(format!("不认识的订阅类型 {kind}"))),
            },
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

fn live_of(blob: &OAuthBlob) -> Live {
    Live {
        tokens: blob.tokens.clone(),
        kind: blob.kind.clone(),
        account_header: blob.account_header.clone(),
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
            account_header: None,
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
