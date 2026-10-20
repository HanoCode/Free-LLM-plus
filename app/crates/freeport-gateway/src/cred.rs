//! 运行时凭据：把「库里存的那坨东西」变成「这次请求要发的头」。
//!
//! 三种形态统一到 [`Credential`]，上游请求只认它，不再到处 `if is_cookie`：
//!
//! | 形态 | 发什么 | 失效判定 |
//! |---|---|---|
//! | `Bearer`（API key） | `Authorization: Bearer …` | 401 |
//! | `OAuth`（登录来的 token） | `Authorization: Bearer …` | 401 / 刷新被拒 |
//! | `Cookie`（web 会话） | `Cookie: …` | 401/403，且**只提示重新登录，不自动续** |
//!
//! Cookie 形态刻意不自动续期：真实会话靠浏览行为刷新，程序去模拟就是在绕
//! 人家的风控。我们只做「记得住、发得对、失效说得清」。

use chrono::Utc;
use freeport_auth::cookie::CookieJar;

use crate::secrets;

#[derive(Debug, Clone)]
pub enum Credential {
    Bearer(String),
    Cookie { account_id: String, jar: CookieJar },
}

impl Credential {
    pub fn bearer(token: impl Into<String>) -> Self {
        Credential::Bearer(token.into())
    }

    /// 从库里取一条能发请求的凭据。
    ///
    /// 同一条凭据可能既有 key 又有 web 会话——**会话优先**：用户专门配了
    /// cookie，说明这次走的就是浏览器那条路（很多站点只有会话才有完整权限）。
    /// 但会话**全过期就退回去用 key**，否则会拿着一份发不出去的空会话把
    /// 这条本来能用的凭据拖死。
    pub fn load(account_id: &str) -> Option<Self> {
        if let Some(raw) = secrets::get_cookie(account_id) {
            if let Ok(jar) = serde_json::from_str::<CookieJar>(&raw) {
                if !jar.is_empty() && !jar.all_expired(Utc::now().timestamp()) {
                    return Some(Credential::Cookie {
                        account_id: account_id.to_string(),
                        jar,
                    });
                }
            }
        }
        secrets::get_key(account_id).map(Credential::Bearer)
    }

    /// 界面上的形态标签
    pub fn kind(&self) -> &'static str {
        match self {
            Credential::Bearer(_) => "key",
            Credential::Cookie { .. } => "cookie",
        }
    }

    pub fn is_cookie(&self) -> bool {
        matches!(self, Credential::Cookie { .. })
    }

    /// 这条请求该带哪个头、什么值。`Cookie` 形态按 URL 现算——不同 path 匹配
    /// 不同 cookie，提前拼好一个串等于把根路径的会话也发给子路径。
    pub fn auth_pair(&self, url: &str) -> Option<(&'static str, String)> {
        match self {
            Credential::Bearer(t) => Some(("authorization", format!("Bearer {t}"))),
            Credential::Cookie { jar, .. } => jar
                .header_for(url, Utc::now().timestamp())
                .map(|v| ("cookie", v)),
        }
    }

    /// 只要头部值，不要头名字。探针与日志用得上。
    pub fn header(&self, url: &str) -> Option<String> {
        self.auth_pair(url).map(|(_, v)| v)
    }

    /// 把上游 `Set-Cookie` 里轮换的新值学回来。不学的话用户过一阵就被踢出去，
    /// 而且日志上完全看不出原因。
    ///
    /// 返回是否真的写盘了（有变化才写，别每个响应都动一次密钥库）。
    pub fn absorb(&mut self, host: Option<&str>, set_cookie: &[String]) -> bool {
        let Some(host) = host else { return false };
        let Credential::Cookie { account_id, jar } = self else {
            return false;
        };
        if set_cookie.is_empty() {
            return false;
        }
        let before = jar.clone();
        jar.absorb(host, set_cookie, Utc::now().timestamp());
        if *jar == before {
            return false;
        }
        match serde_json::to_string(jar) {
            Ok(raw) => secrets::set_cookie(account_id, &raw).is_ok(),
            Err(_) => false,
        }
    }

    /// 这条凭据是不是已经彻底过期（只对 Cookie 有意义）
    pub fn is_expired(&self) -> bool {
        match self {
            Credential::Cookie { jar, .. } => jar.all_expired(Utc::now().timestamp()),
            _ => false,
        }
    }

    /// 界面/日志用的摘要。绝不回显 cookie 值。
    pub fn summary(&self) -> String {
        match self {
            Credential::Bearer(t) => crate::store::mask(t),
            Credential::Cookie { jar, .. } => format!("web 会话（{}）", jar.summary()),
        }
    }
}

/// 把一条 web 会话存进库。`raw` 是用户粘贴的原文，host 来自该厂商的 base_url。
pub fn save_cookie_jar(account_id: &str, raw: &str, host: &str) -> Result<usize, String> {
    let imported = CookieJar::parse_input(raw, host);
    if imported.jar.is_empty() {
        let why = imported
            .skipped
            .first()
            .cloned()
            .unwrap_or_else(|| "没识别出任何 cookie".into());
        return Err(format!("导入失败：{why}"));
    }
    let body = serde_json::to_string(&imported.jar).map_err(|e| e.to_string())?;
    secrets::set_cookie(account_id, &body)?;
    Ok(imported.jar.len())
}

/// 从 URL 里取 host。解析失败返回 `None`，调用方当「学不到」处理。
pub fn host_of(url: &str) -> Option<String> {
    let rest = url.split_once("://").map(|(_, r)| r)?;
    let authority = rest.split('/').next().unwrap_or(rest);
    let host_port = authority.rsplit('@').next().unwrap_or(authority);
    let host = if let Some(stripped) = host_port.strip_prefix('[') {
        stripped.split(']').next().unwrap_or("")
    } else {
        host_port.split(':').next().unwrap_or("")
    };
    if host.is_empty() {
        None
    } else {
        Some(host.to_ascii_lowercase())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_is_taken_from_authority_not_path() {
        assert_eq!(host_of("https://opencode.ai/zen/v1").as_deref(), Some("opencode.ai"));
        assert_eq!(host_of("http://127.0.0.1:8787/v1").as_deref(), Some("127.0.0.1"));
        assert_eq!(host_of("https://u:p@x.com:8443/a").as_deref(), Some("x.com"));
        assert_eq!(host_of("not a url"), None);
    }

    #[test]
    fn cookie_header_is_computed_per_url() {
        let jar = CookieJar::parse_input("sid=1", "opencode.ai").jar;
        let cred = Credential::Cookie {
            account_id: "a".into(),
            jar,
        };
        assert_eq!(
            cred.header("https://opencode.ai/zen/v1").as_deref(),
            Some("sid=1")
        );
        // 别的站点不发
        assert_eq!(cred.header("https://api.other.com/v1"), None);
    }

    #[test]
    fn bearer_and_cookie_never_mix() {
        let cred = Credential::Bearer("sk-1".into());
        assert_eq!(cred.header("https://x.com").as_deref(), Some("Bearer sk-1"));
        assert!(!cred.is_cookie());
    }
}
