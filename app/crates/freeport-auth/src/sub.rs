//! 订阅账号登录（授权码 + PKCE）。
//!
//! 有些厂商的能力只跟**你的订阅**绑在一起，不单卖 API key。能做的只有一件事：
//! 让用户在自己浏览器里登录自己的账号，我们把回调里的授权码换成 token。
//! 这条线和设备码是同一条红线的两种走法，规矩更硬：
//!
//! 1. **公共客户端**：只用厂商公开的 client_id + PKCE，绝不内嵌 client_secret
//!    （有 secret 的客户端等于「我们替用户保管了别人的应用密钥」，不该存在）。
//! 2. **不代登录**：我们只打开厂商自己的授权页，密码、二次验证都在厂商那边。
//!    我们不读系统浏览器的 cookie，也不伪造 UA 去「像官方客户端」。
//! 3. **不隐藏事实**：走订阅额度可能不符合厂商条款，界面必须写明这一句，
//!    让用户自己决定。偷偷用比明着用更糟。
//!
//! 两个厂商的脾气不一样，都写在代码里了：
//!
//! | | Claude | Codex |
//! |---|---|---|
//! | 换 token 的编码 | **JSON** | **表单** |
//! | 回调地址 | 任意回环端口 `/callback` | **固定 1455** `/auth/callback` |
//! | 授权页额外参数 | `code=true` | 三个 `codex_cli_*` 风格开关 |
//! | 授权码形态 | 网页上显示 `code#state`，要拆 | 标准 query |
//!
//! 编码弄反的报错非常含糊（只回一个 400 + HTML），所以这里按厂商写死并各有单测。

use std::time::Duration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
use base64::Engine as _;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::pkce::{self, Pkce};
use crate::{AuthError, TokenSet};

/* ── 厂商常量 ─────────────────────────────────────── */

/// Claude Code 的公开客户端标识（公共客户端，无 secret）。
pub const CLAUDE_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
pub const CLAUDE_AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
pub const CLAUDE_TOKEN_URL: &str = "https://api.anthropic.com/v1/oauth/token";
pub const CLAUDE_SCOPES: &str = "org:create_api_key user:profile user:inference";
pub const CLAUDE_PROFILE_URL: &str = "https://api.anthropic.com/api/oauth/profile";
/// 订阅额度的权威值。账本里的「剩余」是从 429 反推的，这里是厂商自己说的。
pub const CLAUDE_USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
/// 回环端口挑一个不常见的：1455 留给 Codex，常见开发端口留给用户自己的服务。
pub const CLAUDE_PORT: u16 = 54545;

/// Codex CLI 的公开客户端标识。
pub const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const CODEX_AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
pub const CODEX_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
pub const CODEX_SCOPES: &str = "openid profile email offline_access";
/// OpenAI 只认这一个回调地址，端口写死。改了就 `redirect_uri_mismatch`。
pub const CODEX_PORT: u16 = 1455;
pub const CODEX_CALLBACK_PATH: &str = "/auth/callback";
pub const CODEX_USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vendor {
    Claude,
    Codex,
}

impl Vendor {
    /// 认厂商名。同时接受预设 id（`claude-sub` / `codex-sub`），
    /// 免得调用方各自再写一套映射。
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "claude" | "claude-sub" | "anthropic" => Some(Vendor::Claude),
            "codex" | "codex-sub" | "chatgpt" | "openai-codex" => Some(Vendor::Codex),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Vendor::Claude => "claude",
            Vendor::Codex => "codex",
        }
    }

    pub fn callback_path(&self) -> &'static str {
        match self {
            Vendor::Claude => "/callback",
            Vendor::Codex => CODEX_CALLBACK_PATH,
        }
    }

    /// 优先端口。拿不到就退到临时端口（见 [`pick_port`]）。
    pub fn preferred_port(&self) -> u16 {
        match self {
            Vendor::Claude => CLAUDE_PORT,
            Vendor::Codex => CODEX_PORT,
        }
    }

    pub fn cfg(&self) -> SubCfg {
        match self {
            Vendor::Claude => SubCfg {
                vendor: Vendor::Claude,
                client_id: CLAUDE_CLIENT_ID.into(),
                authorize_url: CLAUDE_AUTHORIZE_URL.into(),
                token_url: CLAUDE_TOKEN_URL.into(),
                scopes: CLAUDE_SCOPES.into(),
            },
            Vendor::Codex => SubCfg {
                vendor: Vendor::Codex,
                client_id: CODEX_CLIENT_ID.into(),
                authorize_url: CODEX_AUTHORIZE_URL.into(),
                token_url: CODEX_TOKEN_URL.into(),
                scopes: CODEX_SCOPES.into(),
            },
        }
    }
}

/// 一套端点 + 客户端标识。做成可替换的结构，测试才能对着本地假 IdP 跑完整链路。
#[derive(Debug, Clone)]
pub struct SubCfg {
    pub vendor: Vendor,
    pub client_id: String,
    pub authorize_url: String,
    pub token_url: String,
    pub scopes: String,
}

impl SubCfg {
    /// 换 token 用 JSON 还是表单。Claude 只吃 JSON，Codex 只吃表单。
    fn json_body(&self) -> bool {
        matches!(self.vendor, Vendor::Claude)
    }
}

/* ── 发起 ─────────────────────────────────────────── */

/// 一次进行中的授权。`verifier` 绝不能离开进程（只在换 token 时用一次）。
#[derive(Debug, Clone)]
pub struct LoginStart {
    pub vendor: Vendor,
    pub state: String,
    pub verifier: String,
    pub redirect_uri: String,
    /// 给用户点开（或复制）的完整授权地址
    pub url: String,
    pub port: u16,
    /// 回环端口是否已就位。false 表示只能手工把回调地址粘回来。
    pub auto: bool,
}

/// 纯函数：给定端口就能算出整条授权请求。`state` / `verifier` 每次新生成。
pub fn begin(cfg: &SubCfg, port: u16, auto: bool) -> LoginStart {
    let Pkce { verifier, challenge } = Pkce::generate();
    let state = pkce::state();
    let redirect_uri = format!("http://127.0.0.1:{port}{}", cfg.vendor.callback_path());

    let mut pairs: Vec<(&str, String)> = vec![
        ("response_type", "code".into()),
        ("client_id", cfg.client_id.clone()),
        ("redirect_uri", redirect_uri.clone()),
        ("scope", cfg.scopes.clone()),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256".into()),
        ("state", state.clone()),
    ];

    // 厂商特有的开关。Claude 的 `code=true` 是「把 code 直接显示给我」的开关，
    // 少了它授权页会走一条我们收不到回调的分支。Codex 那三个是官方 CLI 的标记，
    // 少了会被当成第三方应用拒掉。
    match cfg.vendor {
        Vendor::Claude => pairs.push(("code", "true".into())),
        Vendor::Codex => {
            pairs.push(("id_token_add_organizations", "true".into()));
            pairs.push(("codex_cli_simplified_flow", "true".into()));
            pairs.push(("originator", "codex_cli_rs".into()));
        }
    }

    let query = pairs
        .iter()
        .map(|(k, v)| format!("{}={}", enc(k), enc(v)))
        .collect::<Vec<_>>()
        .join("&");
    let sep = if cfg.authorize_url.contains('?') { '&' } else { '?' };

    LoginStart {
        vendor: cfg.vendor,
        state,
        verifier,
        redirect_uri,
        url: format!("{}{sep}{query}", cfg.authorize_url),
        port,
        auto,
    }
}

/// 挑一个能用的回环端口。返回 `(端口, 能否自动收回调)`。
///
/// 先试厂商要求的首选端口，占用就退到临时端口——**但 Codex 不能退**：
/// OpenAI 那边登记的就是 1455，换端口只会换来一句 `redirect_uri_mismatch`，
/// 那还不如直接让用户手工粘贴。
pub async fn pick_port(vendor: Vendor) -> (u16, bool) {
    let preferred = vendor.preferred_port();
    if can_bind(preferred).await {
        return (preferred, true);
    }
    if vendor == Vendor::Codex {
        return (preferred, false);
    }
    match TcpListener::bind(("127.0.0.1", 0)).await {
        Ok(l) => {
            let p = l.local_addr().map(|a| a.port()).unwrap_or(preferred);
            drop(l);
            (p, true)
        }
        Err(_) => (preferred, false),
    }
}

async fn can_bind(port: u16) -> bool {
    match TcpListener::bind(("127.0.0.1", port)).await {
        Ok(l) => {
            drop(l);
            true
        }
        Err(_) => false,
    }
}

/* ── 收回调 ───────────────────────────────────────── */

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Callback {
    pub code: String,
    pub state: Option<String>,
}

/// 在回环上等一次回调，最多等 `timeout_secs` 秒。
///
/// 只认 `path` 匹配的那一条请求并只回一次结果：浏览器很喜欢顺手发
/// `/favicon.ico`，把它当成回调会让整条流程拿着空 code 失败。
pub async fn wait_callback(
    port: u16,
    path: &str,
    timeout_secs: u64,
) -> Result<Callback, AuthError> {
    let listener = TcpListener::bind(("127.0.0.1", port))
        .await
        .map_err(|e| AuthError::Transport(format!("无法监听回环端口 {port}：{e}")))?;

    let work = async {
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(x) => x,
                Err(e) => return Err(AuthError::Transport(e.to_string())),
            };
            let mut buf = [0u8; 8192];
            let n = match sock.read(&mut buf).await {
                Ok(n) => n,
                Err(_) => continue,
            };
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let Some(target) = req.lines().next().and_then(request_target) else {
                let _ = sock.write_all(&reply(400, "请求格式不认识")).await;
                continue;
            };
            let (req_path, query) = match target.split_once('?') {
                Some((p, q)) => (p.to_string(), q.to_string()),
                None => (target, String::new()),
            };
            if req_path != path {
                let _ = sock.write_all(&reply(404, "这里不是回调地址")).await;
                continue;
            }

            let pairs = parse_query(&query);
            let get = |k: &str| {
                pairs
                    .iter()
                    .find(|(key, _)| key == k)
                    .map(|(_, v)| v.clone())
            };

            if get("error").is_some() {
                // 用户在授权页点了拒绝，或厂商拒了这个客户端。原因写进日志，
                // 但页面只说「没完成」——用户不需要看懂 OAuth 的 error 码。
                tracing::warn!(
                    error = ?get("error"),
                    detail = ?get("error_description"),
                    "订阅授权未完成"
                );
                let _ = sock.write_all(&reply(200, "授权没有完成，可以关掉这个页面")).await;
                return Err(AuthError::Denied);
            }

            match get("code") {
                Some(code) if !code.trim().is_empty() => {
                    let _ = sock
                        .write_all(&reply(200, "已收到授权，可以关掉这个页面回到 Freeport"))
                        .await;
                    return Ok(Callback {
                        code,
                        state: get("state"),
                    });
                }
                _ => {
                    let _ = sock.write_all(&reply(400, "回调里没有 code")).await;
                    continue;
                }
            }
        }
    };

    match tokio::time::timeout(Duration::from_secs(timeout_secs), work).await {
        Ok(r) => r,
        Err(_) => Err(AuthError::Expired),
    }
}

/// `GET /auth/callback?code=x HTTP/1.1` → `/auth/callback?code=x`
fn request_target(line: &str) -> Option<String> {
    let mut it = line.split_whitespace();
    let method = it.next()?;
    if !method.eq_ignore_ascii_case("GET") {
        return None;
    }
    it.next().map(|s| s.to_string())
}

fn reply(status: u16, msg: &str) -> Vec<u8> {
    let body = format!(
        "<!doctype html><meta charset=\"utf-8\"><title>Freeport</title>\
         <body style=\"font:16px/1.6 -apple-system,system-ui,sans-serif;padding:48px;color:#111\">\
         <p>{msg}</p></body>"
    );
    let head = format!(
        "HTTP/1.1 {status} OK\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.as_bytes().len()
    );
    format!("{head}{body}").into_bytes()
}

/* ── 换 token ─────────────────────────────────────── */

/// 把「用户粘回来的东西」解析成授权码。
///
/// 三种形态都要认，因为用户手上可能拿到的是其中任何一种：
///
/// - 完整回调地址：`http://127.0.0.1:1455/auth/callback?code=…&state=…`
/// - Claude 网页上显示的 `code#state`（它把两段拼在一起给用户复制）
/// - 光秃秃一个 code
pub fn parse_code(input: &str) -> (String, Option<String>) {
    let s = input.trim();
    if let Some((_, q)) = s.split_once('?') {
        let q = q.split('#').next().unwrap_or(q);
        let pairs = parse_query(q);
        let find = |k: &str| {
            pairs
                .iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.clone())
        };
        if let Some(code) = find("code").filter(|c| !c.trim().is_empty()) {
            return (code, find("state"));
        }
    }
    if let Some((code, state)) = s.split_once('#') {
        let state = state.trim();
        return (
            code.trim().to_string(),
            if state.is_empty() {
                None
            } else {
                Some(state.to_string())
            },
        );
    }
    (s.to_string(), None)
}

/// 授权码换 token。`state` 必须与发起时逐字一致，否则就是别人塞进来的链接。
pub async fn exchange_code(
    client: &reqwest::Client,
    cfg: &SubCfg,
    flow: &LoginStart,
    input: &str,
    now: i64,
) -> Result<TokenSet, AuthError> {
    let (code, state) = parse_code(input);
    if code.is_empty() {
        return Err(AuthError::Config("没有拿到授权码".into()));
    }
    if let Some(got) = state {
        if got != flow.state {
            return Err(AuthError::Rejected {
                status: 400,
                code: "state_mismatch".into(),
                detail: "回调里的 state 与本次登录不一致，请重新发起登录（不要用别人给的链接）"
                    .into(),
            });
        }
    }

    let value = if cfg.json_body() {
        // Claude：JSON。`state` 也要放进 body，缺了会被拒。
        let body = json!({
            "code": code,
            "state": flow.state,
            "grant_type": "authorization_code",
            "client_id": cfg.client_id,
            "redirect_uri": flow.redirect_uri,
            "code_verifier": flow.verifier,
        });
        post_json(client, &cfg.token_url, &body).await?
    } else {
        post_form(
            client,
            &cfg.token_url,
            &[
                ("grant_type", "authorization_code"),
                ("client_id", &cfg.client_id),
                ("code", &code),
                ("redirect_uri", &flow.redirect_uri),
                ("code_verifier", &flow.verifier),
            ],
        )
        .await?
    };

    tokens_from(value, now)
}

/// 续期。Codex 每次都会轮换 refresh_token 并把旧的作废；
/// Claude 也会轮换。所以**换完必须落盘**，落盘失败等于下次登录。
pub async fn refresh(
    client: &reqwest::Client,
    cfg: &SubCfg,
    current: &TokenSet,
    now: i64,
) -> Result<TokenSet, AuthError> {
    let rt = current
        .refresh_token
        .as_deref()
        .filter(|t| !t.trim().is_empty())
        .ok_or(AuthError::Denied)?;

    let value = if cfg.json_body() {
        let body = json!({
            "grant_type": "refresh_token",
            "client_id": cfg.client_id,
            "refresh_token": rt,
        });
        post_json(client, &cfg.token_url, &body).await?
    } else {
        post_form(
            client,
            &cfg.token_url,
            &[
                ("grant_type", "refresh_token"),
                ("client_id", &cfg.client_id),
                ("refresh_token", rt),
                ("scope", &cfg.scopes),
            ],
        )
        .await?
    };

    let mut next = tokens_from(value, now)?;
    // 上游没回新的 refresh_token 时沿用旧的：清了就再也换不出来
    if next.refresh_token.is_none() {
        next.refresh_token = current.refresh_token.clone();
    }
    // 账号名沿用：这类接口的刷新响应里通常不带身份信息
    if next.account.is_none() {
        next.account = current.account.clone();
    }
    if next.account_id.is_none() {
        next.account_id = current.account_id.clone();
    }
    Ok(next)
}

/// 从 token 响应里抠出 [`TokenSet`]。
///
/// 三个字段形态各异，只能挨个认：Claude 给 `account.email_address`，
/// Codex 给 `id_token`（JWT，里面才有邮箱和 workspace）。
fn tokens_from(value: Value, now: i64) -> Result<TokenSet, AuthError> {
    let token = value
        .get("access_token")
        .and_then(|t| t.as_str())
        .filter(|t| !t.trim().is_empty())
        .ok_or_else(|| AuthError::Rejected {
            status: 200,
            code: value
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("no_access_token")
                .to_string(),
            detail: value
                .get("error_description")
                .and_then(|d| d.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| "授权服务没有返回 access_token".into()),
        })?;

    let claims = value.get("id_token").and_then(|t| t.as_str()).and_then(jwt_payload);

    let account = value
        .get("account")
        .and_then(|a| a.get("email_address").or_else(|| a.get("email")))
        .and_then(|e| e.as_str())
        .map(|s| s.to_string())
        .or_else(|| claims.as_ref().and_then(|c| str_at(c, "email")))
        .or_else(|| value.get("email").and_then(|e| e.as_str()).map(|s| s.to_string()));

    // Codex 的请求必须带 `chatgpt-account-id`，值是 id_token 里的 workspace id。
    // 拿不到就留空——那说明这个号的结构和我们预期的不一样，宁可让它 401 报出来，
    // 也不要猜一个填进去。
    let account_id = claims
        .as_ref()
        .and_then(|c| c.get("https://api.openai.com/auth"))
        .and_then(|a| a.get("chatgpt_account_id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| {
            value
                .get("account")
                .and_then(|a| a.get("uuid"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        });

    Ok(TokenSet {
        access_token: token.to_string(),
        token_type: value
            .get("token_type")
            .and_then(|t| t.as_str())
            .unwrap_or("bearer")
            .to_string(),
        refresh_token: value
            .get("refresh_token")
            .and_then(|t| t.as_str())
            .filter(|t| !t.trim().is_empty())
            .map(|s| s.to_string()),
        scope: value
            .get("scope")
            .and_then(|s| s.as_str())
            .map(|s| s.to_string()),
        expires_at: value.get("expires_in").and_then(|e| e.as_i64()).map(|s| now + s),
        account,
        account_id,
    })
}

/// 取 JWT 的 payload（中间那段）。只是读，不验签——签名是给上游验的，
/// 我们要的只是「这个 token 说是谁的号」。
fn jwt_payload(token: &str) -> Option<Value> {
    let body = token.split('.').nth(1)?;
    let raw = B64URL.decode(body).ok()?;
    serde_json::from_slice(&raw).ok()
}

fn str_at(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| x.as_str()).map(|s| s.to_string())
}

/* ── 身份 ─────────────────────────────────────────── */

/// 查这个 token 是谁的号，用于界面上显示「已登录为 a@b.com」。
/// 失败不影响可用性，返回 `None` 就行。
pub async fn whoami(client: &reqwest::Client, kind: &str, token: &str) -> Option<String> {
    whoami_at(client, kind, CLAUDE_PROFILE_URL, token).await
}

pub async fn whoami_at(
    client: &reqwest::Client,
    kind: &str,
    url: &str,
    token: &str,
) -> Option<String> {
    // Codex 没有「查我是谁」的端点，身份只存在于 id_token 里（已由 tokens_from 解出）
    if kind != Vendor::Claude.as_str() {
        return None;
    }
    let res = client
        .get(url)
        .header("accept", "application/json")
        .header("authorization", format!("Bearer {token}"))
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .ok()?;
    if !res.status().is_success() {
        return None;
    }
    let v: Value = res.json().await.ok()?;
    v.get("account")
        .and_then(|a| a.get("email_address").or_else(|| a.get("email")))
        .and_then(|e| e.as_str())
        .map(|s| s.to_string())
}

/* ── HTTP 小工具 ──────────────────────────────────── */

async fn post_json(
    client: &reqwest::Client,
    url: &str,
    body: &Value,
) -> Result<Value, AuthError> {
    let res = client
        .post(url)
        .header("accept", "application/json")
        .json(body)
        .send()
        .await
        .map_err(|e| AuthError::Transport(e.to_string()))?;
    read(res).await
}

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
    read(res).await
}

async fn read(res: reqwest::Response) -> Result<Value, AuthError> {
    let status = res.status().as_u16();
    let text = res.text().await.map_err(|e| AuthError::Transport(e.to_string()))?;
    let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);

    // 有的端点失败时回 200 + error 字段，所以两个条件都要看
    let has_error = value.get("error").is_some_and(|e| !e.is_null());
    if !(200..300).contains(&status) || has_error {
        if status == 400 || has_error {
            // 400 里最值钱的是 error 字段；没有就顺手把正文带出来，别只报「失败了」
            let code = value
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("token_request_failed")
                .to_string();
            let detail = value
                .get("error_description")
                .and_then(|d| d.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| text.chars().take(300).collect());
            return Err(AuthError::Rejected {
                status,
                code,
                detail,
            });
        }
        return Err(AuthError::Rejected {
            status,
            code: "http_error".into(),
            detail: text.chars().take(300).collect(),
        });
    }
    Ok(value)
}

/* ── 编码 ─────────────────────────────────────────── */

/// `application/x-www-form-urlencoded` 的值编码。空格编成 `%20` 而不是 `+`：
/// Codex 的授权页把 `+` 当字面量，scope 里的空格会变成模型名的一部分。
fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn dec(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                if let (Some(h), Some(l)) = (hi, lo) {
                    out.push((h * 16 + l) as u8);
                    i += 3;
                } else {
                    out.push(b'%');
                    i += 1;
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_query(q: &str) -> Vec<(String, String)> {
    q.split('&')
        .filter(|p| !p.is_empty())
        .map(|p| match p.split_once('=') {
            Some((k, v)) => (dec(k), dec(v)),
            None => (dec(p), String::new()),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_url_carries_the_show_me_the_code_switch() {
        let cfg = Vendor::Claude.cfg();
        let flow = begin(&cfg, 54545, true);
        assert!(flow.url.starts_with(CLAUDE_AUTHORIZE_URL));
        assert!(flow.url.contains("code=true"));
        assert!(flow.url.contains("code_challenge_method=S256"));
        assert!(flow.url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A54545%2Fcallback"));
        // scope 里的空格必须编成 %20
        assert!(flow.url.contains("org%3Acreate_api_key%20user%3Aprofile"));
        assert!(!flow.url.contains('+'));
    }

    #[test]
    fn codex_url_is_pinned_to_1455_and_auth_callback() {
        let cfg = Vendor::Codex.cfg();
        let flow = begin(&cfg, 1455, true);
        assert!(flow.url.starts_with(CODEX_AUTHORIZE_URL));
        assert!(flow.url.contains("originator=codex_cli_rs"));
        assert!(flow.url.contains("codex_cli_simplified_flow=true"));
        assert!(flow.url.contains("auth%2Fcallback"));
        assert_eq!(flow.redirect_uri, "http://127.0.0.1:1455/auth/callback");
    }

    #[test]
    fn state_and_verifier_are_fresh_every_time() {
        let cfg = Vendor::Claude.cfg();
        let a = begin(&cfg, 54545, true);
        let b = begin(&cfg, 54545, true);
        assert_ne!(a.state, b.state);
        assert_ne!(a.verifier, b.verifier);
        assert!(a.url.contains(&a.state));
    }

    #[test]
    fn code_is_parsed_from_all_three_shapes() {
        let url = "http://127.0.0.1:1455/auth/callback?code=abc123&state=xyz";
        assert_eq!(parse_code(url), ("abc123".into(), Some("xyz".into())));

        // Claude 网页上给的是 code#state
        assert_eq!(
            parse_code("abc123#xyz"),
            ("abc123".into(), Some("xyz".into()))
        );

        assert_eq!(parse_code("  abc123 \n"), ("abc123".into(), None));
        // 带 fragment 的 URL 也要能吃
        assert_eq!(
            parse_code("http://x/cb?code=abc&state=s#frag"),
            ("abc".into(), Some("s".into()))
        );
    }

    #[test]
    fn percent_encoding_roundtrips() {
        let raw = "a b/c:d?e=f&g";
        assert_eq!(dec(&enc(raw)), raw);
        assert_eq!(enc("a b"), "a%20b");
        // 坏掉的序列不 panic，原样保留
        assert_eq!(dec("100%"), "100%");
        assert_eq!(dec("%zz"), "%zz");
    }

    #[test]
    fn request_target_ignores_non_get() {
        assert_eq!(
            request_target("GET /callback?code=1 HTTP/1.1").as_deref(),
            Some("/callback?code=1")
        );
        assert_eq!(request_target("POST /callback HTTP/1.1"), None);
        assert_eq!(request_target("garbage"), None);
    }

    #[test]
    fn claude_response_yields_email_and_keeps_refresh_token() {
        let v = json!({
            "access_token": "at-1",
            "refresh_token": "rt-1",
            "expires_in": 3600,
            "token_type": "Bearer",
            "scope": "user:inference",
            "account": { "uuid": "u-1", "email_address": "a@b.com" }
        });
        let ts = tokens_from(v, 1_000).unwrap();
        assert_eq!(ts.access_token, "at-1");
        assert_eq!(ts.refresh_token.as_deref(), Some("rt-1"));
        assert_eq!(ts.expires_at, Some(4_600));
        assert_eq!(ts.account.as_deref(), Some("a@b.com"));
        assert_eq!(ts.account_id.as_deref(), Some("u-1"));
    }

    #[test]
    fn codex_id_token_supplies_account_and_workspace() {
        // 造一个只有 payload 有意义的 JWT
        let payload = json!({
            "email": "c@d.com",
            "https://api.openai.com/auth": { "chatgpt_account_id": "ws-9" }
        });
        let body = B64URL.encode(serde_json::to_vec(&payload).unwrap());
        let v = json!({
            "access_token": "at-2",
            "refresh_token": "rt-2",
            "id_token": format!("header.{body}.sig"),
            "expires_in": 600
        });
        let ts = tokens_from(v, 0).unwrap();
        assert_eq!(ts.account.as_deref(), Some("c@d.com"));
        assert_eq!(ts.account_id.as_deref(), Some("ws-9"));
        assert_eq!(ts.expires_at, Some(600));
    }

    #[test]
    fn missing_access_token_is_an_error_not_an_empty_token() {
        let err = tokens_from(json!({ "error": "invalid_grant" }), 0).unwrap_err();
        match err {
            AuthError::Rejected { code, .. } => assert_eq!(code, "invalid_grant"),
            other => panic!("期望 Rejected，实际 {other:?}"),
        }
    }

    #[test]
    fn refresh_without_new_refresh_token_keeps_the_old_one() {
        // tokens_from 单独看不出「沿用」，这里把 refresh 的合并逻辑抄一遍做契约锁：
        // 上游只回 access_token 是常态（Claude 就会这样），丢了 rt 等于让用户重新登录。
        let current = TokenSet {
            access_token: "old".into(),
            refresh_token: Some("rt".into()),
            account: Some("a@b.com".into()),
            ..Default::default()
        };
        let mut next = tokens_from(json!({ "access_token": "new", "expires_in": 60 }), 0).unwrap();
        if next.refresh_token.is_none() {
            next.refresh_token = current.refresh_token.clone();
        }
        assert_eq!(next.refresh_token.as_deref(), Some("rt"));
        assert_eq!(next.account, None); // 身份由 refresh() 补，tokens_from 不管
    }

    #[test]
    fn codex_pick_port_never_falls_back_to_a_random_port() {
        // 端口被占时 Codex 只能走手工粘贴：换端口必然 redirect_uri_mismatch。
        // 这里只锁「首选端口就是 OpenAI 登记的那个」，避免有人顺手改成 1456。
        assert_eq!(Vendor::Codex.preferred_port(), 1455);
        assert_eq!(Vendor::Codex.callback_path(), "/auth/callback");
    }
}
