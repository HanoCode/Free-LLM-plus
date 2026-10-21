//! 上游调用。M1.5 只做 OpenAI 兼容协议；
//! Anthropic / Gemini 的 wire 转换在 M6 接 Canonical IR。
//!
//! 所有出门的请求体都过一遍 [`crate::sanitize`]：剥内部字段 + 抹掉本凭据的
//! 密钥字面量。上游回显 key 的错误体也在这里就地脱敏——这里是最靠外的
//! 一环，脱在这一层，日志、界面、返回给客户端的错误体就都干净了。

use std::time::Duration;

use freeport_core::error::parse_retry_after;
use freeport_core::model::{ModelInfo, Provider};
use serde::Deserialize;

use crate::cred::Credential;
use crate::sanitize::{strip_internal, Redactor};

#[derive(Debug, Clone)]
pub struct UpstreamError {
    pub status: u16,
    pub body: String,
    pub retry_after_ms: u64,
}

impl std::fmt::Display for UpstreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "upstream {}: {}", self.status, self.body.chars().take(200).collect::<String>())
    }
}

impl std::error::Error for UpstreamError {}

pub fn client() -> reqwest::Client {
    build(env_proxy().as_deref())
}

/// 厂商级代理客户端（§3.9「网络」：厂商级 > 环境变量）。
///
/// 同一代理串复用同一个 `Client`：`reqwest::Client` 里是连接池，
/// 每个请求重建一个等于每次重新 TCP + TLS 握手。缓存放在进程级，
/// 因为代理配置只有几十条，生命周期与进程一致。
pub fn client_of(p: &Provider) -> reqwest::Client {
    let Some(url) = p.proxy.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
        return client();
    };
    client_for_proxy(url)
}

/// 按代理串取客户端。巡检手上只有 `proxy` 字符串、没有 `Provider`，
/// 但连接池缓存必须与 `client_of` 共用，否则同一代理会出现两个池。
pub fn client_for_proxy(url: &str) -> reqwest::Client {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, reqwest::Client>>> =
        std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    let mut map = cache.lock().unwrap();
    if let Some(c) = map.get(url) {
        return c.clone();
    }
    let built = build(Some(url));
    map.insert(url.to_string(), built.clone());
    built
}

fn build(proxy_url: Option<&str>) -> reqwest::Client {
    let mut builder = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .pool_idle_timeout(Duration::from_secs(90))
        .user_agent("Freeport/0.1 (+https://freeport.dev)");

    // 系统代理照用（访问被墙厂商需要），但本机回环必须直连：
    // 否则 chat2api / 本地代理这类 127.0.0.1 上游会被代理直接掐断。
    if let Some(url) = proxy_url {
        if let Ok(proxy) = reqwest::Proxy::all(url) {
            builder = builder.proxy(proxy.no_proxy(reqwest::NoProxy::from_string(
                "127.0.0.1,localhost,::1",
            )));
        } else {
            tracing::warn!(proxy = %url, "厂商代理地址无效，本次回落到直连");
        }
    }

    builder.build().expect("reqwest client")
}

fn env_proxy() -> Option<String> {
    for name in [
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "http_proxy",
        "ALL_PROXY",
        "all_proxy",
    ] {
        if let Ok(value) = std::env::var(name) {
            if !value.trim().is_empty() {
                return Some(value);
            }
        }
    }
    None
}

fn join(base: &str, path: &str) -> String {
    format!("{}{}", base.trim_end_matches('/'), path)
}

/// 拼上游请求头。认证部分交给 [`Credential`]——API key 发 `Bearer`，
/// web 会话发 `Cookie`，两者**绝不同时出现**（带着 Bearer 访问只认会话的
/// 端点会直接 401，还会让上游看到一把它根本不认识的 token）。
fn headers(p: &Provider, cred: &Credential, url: &str) -> Vec<(String, String)> {
    // 头名字用 String 而不是 &'static str：附加认证头是运行时才定下来的，
    // 为了塞进静态生命周期去 leak 内存等于每个请求漏一块。
    let mut h: Vec<(String, String)> = vec![
        ("content-type".into(), "application/json".into()),
        ("accept".into(), "application/json".into()),
    ];

    match cred {
        Credential::Bearer { token, .. } => {
            h.push(("authorization".into(), format!("Bearer {token}")));
            // 附加认证头来自 token 本身（如 Codex 的工作区 id）。
            // 它们不按厂商写死，因为同一家不同号可能属于不同工作区。
            for (k, v) in cred.extras() {
                h.push((k.clone(), v.clone()));
            }
        }
        Credential::Cookie { .. } => {
            if let Some(v) = cred.header(url) {
                h.push(("cookie".into(), v));
            }
        }
    }
    if p.id == "openrouter" {
        h.push(("http-referer".into(), "https://freeport.dev".into()));
        h.push(("x-title".into(), "Freeport".into()));
    }
    // Copilot 的网关会看这几个头，缺了直接 400。它们也是「这是谁在调用」的
    // 一部分，所以只在 copilot 这个厂商下加，别污染其他上游。
    if p.id == "copilot" {
        for (k, v) in freeport_auth::copilot::extra_headers() {
            h.push((k.to_string(), v));
        }
    }
    match p.id.as_str() {
        // 订阅号走 Anthropic 的 OAuth 通道：`anthropic-version` 是必须的，
        // `oauth-2025-04-20` 是「允许拿订阅 token 调 /v1/messages」的开关。
        // 只给订阅预设加——普通 API key 的号带上这个 beta 会被拒。
        "claude-sub" => {
            h.push(("anthropic-version".into(), "2023-06-01".into()));
            h.push(("anthropic-beta".into(), "oauth-2025-04-20".into()));
        }
        // Codex 的后端要求标明「这是谁在调」，官方 CLI 用固定值。
        // 少了它上游按第三方客户端处理，直接 400。
        "codex-sub" => {
            h.push(("originator".into(), "codex_cli_rs".into()));
            h.push(("openai-beta".into(), "responses=experimental".into()));
        }
        _ => {}
    }
    h
}

#[derive(Deserialize)]
struct ModelsResponse {
    data: Option<Vec<RawModel>>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawModel {
    Id(String),
    Obj {
        id: String,
        #[serde(default)]
        owned_by: Option<String>,
        #[serde(default)]
        context_length: Option<u64>,
        #[serde(default)]
        created: Option<i64>,
    },
}

/// 用真 key 拉模型列表。拉不到就说明这条配置不可用，不允许入库。
pub async fn list_models(
    client: &reqwest::Client,
    p: &Provider,
    cred: &Credential,
) -> Result<Vec<ModelInfo>, UpstreamError> {
    let url = join(&p.base_url, "/models");
    let red = Redactor::for_credential(cred);
    let mut req = client.get(&url);
    for (k, v) in headers(p, cred, &url) {
        req = req.header(k, v);
    }
    let res = req
        .timeout(Duration::from_secs(12))
        .send()
        .await
        .map_err(|e| UpstreamError {
            status: 0,
            body: red.redact(&e.to_string()),
            retry_after_ms: 0,
        })?;

    let status = res.status().as_u16();
    let retry_after = res
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let text = res.text().await.unwrap_or_default();

    if !(200..300).contains(&status) {
        return Err(UpstreamError {
            status,
            // 先算 retry-after 再脱敏：脱敏只影响展示，不该影响配额判定
            retry_after_ms: parse_retry_after(retry_after.as_deref(), &text),
            body: red.redact(&text).chars().take(800).collect(),
        });
    }

    let raw: Vec<RawModel> = if let Ok(parsed) = serde_json::from_str::<ModelsResponse>(&text) {
        parsed.data.unwrap_or_default()
    } else {
        serde_json::from_str::<Vec<RawModel>>(&text).unwrap_or_default()
    };

    let mut out: Vec<ModelInfo> = raw
        .into_iter()
        .filter_map(|m| match m {
            RawModel::Id(id) => Some(ModelInfo {
                id,
                owned_by: p.id.clone(),
                context: None,
                created: None,
                free: None,
                caps: Default::default(),
                candidate: false,
            }),
            RawModel::Obj {
                id,
                owned_by,
                context_length,
                created,
            } => Some(ModelInfo {
                id,
                owned_by: owned_by.unwrap_or_else(|| p.id.clone()),
                context: context_length,
                created,
                free: None,
                caps: Default::default(),
                candidate: false,
            }),
        })
        .filter(|m| !m.id.is_empty())
        .collect();

    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

/// 发起一次 chat。只等到响应头返回，body 交给上层流式转发。
///
/// 请求体在这里复制一份再清洗（剥内部字段 + 抹掉本凭据的字面量），
/// 调用方传进来的 payload 不会被改动——接力链里同一个 payload 要发给
/// 下一跳，改坏了就是二次伤害。
pub async fn create_chat(
    client: &reqwest::Client,
    p: &Provider,
    cred: &Credential,
    payload: &serde_json::Value,
    timeout: Duration,
) -> Result<reqwest::Response, UpstreamError> {
    // 上游讲什么协议就发什么形状。绝大多数厂商是 OpenAI 兼容（原样发），
    // 订阅号只有原生端点，必须在**这一层**翻译——再往上一切都是 OpenAI 形态的。
    let wire = crate::upwire::wire_of_provider(&p.id);
    let url = join(&p.base_url, wire.path());
    let red = Redactor::for_credential(cred);
    let mut body = crate::upwire::request(wire, payload);
    strip_internal(&mut body);
    red.redact_value(&mut body);

    let mut req = client.post(&url);
    for (k, v) in headers(p, cred, &url) {
        req = req.header(k, v);
    }
    let res = tokio::time::timeout(timeout, req.json(&body).send())
        .await
        .map_err(|_| UpstreamError {
            status: 504,
            body: "上游在预算内没有响应".into(),
            retry_after_ms: 0,
        })?
        .map_err(|e| UpstreamError {
            status: if e.is_timeout() { 504 } else { 0 },
            body: red.redact(&e.to_string()),
            retry_after_ms: 0,
        })?;

    let status = res.status().as_u16();
    if (200..300).contains(&status) {
        return Ok(res);
    }

    let retry_after = res
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let body = res.text().await.unwrap_or_default();
    Err(UpstreamError {
        status,
        retry_after_ms: parse_retry_after(retry_after.as_deref(), &body),
        body: red.redact(&body),
    })
}

/// `POST {base}/embeddings`。只对讲 OpenAI 兼容线的厂商有意义——
/// 订阅号的原生端点没有 embeddings，调用方要在拼请求前把这类候选筛掉。
///
/// embeddings 不是 chat：没有接力链里那套协议翻译的必要（请求/响应原样
/// 就是 OpenAI 形状），只需要同样的认证、超时与错误归一。
pub async fn create_embeddings(
    client: &reqwest::Client,
    p: &Provider,
    cred: &Credential,
    payload: &serde_json::Value,
    timeout: Duration,
) -> Result<reqwest::Response, UpstreamError> {
    let url = join(&p.base_url, "/embeddings");
    let red = Redactor::for_credential(cred);
    let mut body = payload.clone();
    strip_internal(&mut body);
    red.redact_value(&mut body);

    let mut req = client.post(&url);
    for (k, v) in headers(p, cred, &url) {
        req = req.header(k, v);
    }
    let res = tokio::time::timeout(timeout, req.json(&body).send())
        .await
        .map_err(|_| UpstreamError {
            status: 504,
            body: "上游在预算内没有响应".into(),
            retry_after_ms: 0,
        })?
        .map_err(|e| UpstreamError {
            status: if e.is_timeout() { 504 } else { 0 },
            body: red.redact(&e.to_string()),
            retry_after_ms: 0,
        })?;

    let status = res.status().as_u16();
    if (200..300).contains(&status) {
        return Ok(res);
    }
    let retry_after = res
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let body = res.text().await.unwrap_or_default();
    Err(UpstreamError {
        status,
        retry_after_ms: parse_retry_after(retry_after.as_deref(), &body),
        body: red.redact(&body),
    })
}
