//! 上游调用。M1.5 只做 OpenAI 兼容协议；
//! Anthropic / Gemini 的 wire 转换在 M6 接 Canonical IR。

use std::time::Duration;

use freeport_core::error::parse_retry_after;
use freeport_core::model::{ModelInfo, Provider};
use serde::Deserialize;

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
    let mut builder = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .pool_idle_timeout(Duration::from_secs(90))
        .user_agent("Freeport/0.1 (+https://freeport.dev)");

    // 系统代理照用（访问被墙厂商需要），但本机回环必须直连：
    // 否则 chat2api / 本地代理这类 127.0.0.1 上游会被代理直接掐断。
    if let Some(url) = env_proxy() {
        if let Ok(proxy) = reqwest::Proxy::all(&url) {
            builder = builder.proxy(proxy.no_proxy(reqwest::NoProxy::from_string(
                "127.0.0.1,localhost,::1",
            )));
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

fn headers(p: &Provider, key: &str) -> Vec<(&'static str, String)> {
    let mut h = vec![
        ("content-type", "application/json".to_string()),
        ("accept", "application/json".to_string()),
        ("authorization", format!("Bearer {key}")),
    ];
    if p.id == "openrouter" {
        h.push(("http-referer", "https://freeport.dev".to_string()));
        h.push(("x-title", "Freeport".to_string()));
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
    key: &str,
) -> Result<Vec<ModelInfo>, UpstreamError> {
    let mut req = client.get(join(&p.base_url, "/models"));
    for (k, v) in headers(p, key) {
        req = req.header(k, v);
    }
    let res = req
        .timeout(Duration::from_secs(12))
        .send()
        .await
        .map_err(|e| UpstreamError {
            status: 0,
            body: e.to_string(),
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
            retry_after_ms: parse_retry_after(retry_after.as_deref(), &text),
            body: text.chars().take(800).collect(),
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
            }),
        })
        .filter(|m| !m.id.is_empty())
        .collect();

    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

/// 发起一次 chat。只等到响应头返回，body 交给上层流式转发。
pub async fn create_chat(
    client: &reqwest::Client,
    p: &Provider,
    key: &str,
    payload: &serde_json::Value,
    timeout: Duration,
) -> Result<reqwest::Response, UpstreamError> {
    let mut req = client.post(join(&p.base_url, "/chat/completions"));
    for (k, v) in headers(p, key) {
        req = req.header(k, v);
    }
    let res = tokio::time::timeout(timeout, req.json(payload).send())
        .await
        .map_err(|_| UpstreamError {
            status: 504,
            body: "上游在预算内没有响应".into(),
            retry_after_ms: 0,
        })?
        .map_err(|e| UpstreamError {
            status: if e.is_timeout() { 504 } else { 0 },
            body: e.to_string(),
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
        body,
    })
}
