//! sidecar 进程本体：`<exe> --freeport-sidecar <id>` 走到这里。
//!
//! 职责只有三件事：
//! 1. 从 stdin 读一份配置（凭据不走 argv / 环境变量，避免被同机进程看见）；
//! 2. 绑 127.0.0.1:0，把拿到的端口用一行 `PORT <n>` 报给宿主——端口号
//!    从此只活在这对进程之间，不进配置、不进 UI、不落盘；
//! 3. 把 `/transcribe`、`/generate` 的请求转发到 manifest 授权过的上游域名。
//!
//! 出网域名白名单在这里强制执行：哪怕宿主进程被骗着传了别的地址，
//! sidecar 也会拒绝启动——权限清单是扩展自己的承诺，不是宿主的恩赐。

use std::collections::HashSet;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;

use crate::host::SidecarCfg;

struct Rt {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
    domains: HashSet<String>,
}

/// 出网校验：目标主机必须命中 manifest 声明的域名（支持 `*.example.com`）。
fn egress_allowed(domains: &HashSet<String>, host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    domains.iter().any(|d| {
        if let Some(suffix) = d.strip_prefix("*.") {
            // 必须是「.suffix」结尾：xgroq.com 不能蹭 *.groq.com
            host.len() > suffix.len()
                && host.ends_with(suffix)
                && host[..host.len() - suffix.len()].ends_with('.')
        } else {
            host == *d
        }
    })
}

impl Rt {
    fn from_cfg(cfg: SidecarCfg) -> Result<Self, String> {
        let host = reqwest::Url::parse(&cfg.base_url)
            .map_err(|e| format!("baseUrl 不合法：{e}"))?
            .host_str()
            .ok_or("baseUrl 缺少主机名")?
            .to_ascii_lowercase();
        let domains: HashSet<String> =
            cfg.domains.iter().map(|d| d.to_ascii_lowercase()).collect();
        if !egress_allowed(&domains, &host) {
            // 门槛一的进程级执行：上游不在 manifest 的授权清单里就不启动
            return Err(format!("上游 {host} 不在 manifest 授权的域名清单里，拒绝启动"));
        }
        Ok(Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(300))
                .build()
                .map_err(|e| e.to_string())?,
            base_url: cfg.base_url.trim_end_matches('/').to_string(),
            api_key: cfg.api_key,
            model: cfg.model,
            domains,
        })
    }

    /// 转发核心。body / content-type 原样透传，宿主到 sidecar 的这一跳
    /// 不解释协议——协议的归属方是上游。
    async fn forward(&self, path: &str, ct: Option<&str>, body: Bytes) -> Response {
        // 出网域再验一次：启动校验是承诺，请求校验是执行
        let url = format!("{}{path}", self.base_url);
        let host = match reqwest::Url::parse(&url) {
            Ok(u) => u.host_str().unwrap_or("").to_ascii_lowercase(),
            Err(e) => {
                return (
                    StatusCode::BAD_GATEWAY,
                    Json(json!({"error": {"message": format!("上游地址不合法：{e}"), "type": "upstream_error"}})),
                )
                    .into_response()
            }
        };
        if !egress_allowed(&self.domains, &host) {
            return (
                StatusCode::FORBIDDEN,
                Json(json!({"error": {"message": format!("上游 {host} 不在 manifest 授权的域名清单里"), "type": "egress_denied"}})),
            )
                .into_response();
        }

        let mut req = self
            .http
            .post(url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .body(body);
        if let Some(ct) = ct {
            req = req.header("Content-Type", ct);
        }
        match req.send().await {
            Ok(up) => {
                let status = StatusCode::from_u16(up.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
                let ct = up
                    .headers()
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .map(|s| s.to_string());
                match up.bytes().await {
                    Ok(bytes) => {
                        let mut resp = (status, bytes).into_response();
                        if let Some(ct) = ct {
                            if let Ok(v) = ct.parse() {
                                resp.headers_mut().insert("content-type", v);
                            }
                        }
                        resp
                    }
                    Err(e) => (
                        StatusCode::BAD_GATEWAY,
                        Json(json!({"error": {"message": format!("读上游响应失败：{e}"), "type": "upstream_error"}})),
                    )
                        .into_response(),
                }
            }
            Err(e) => (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error": {"message": format!("上游不可达：{e}"), "type": "upstream_error"}})),
            )
                .into_response(),
        }
    }
}

async fn healthz() -> &'static str {
    "ok"
}

/// ASR：OpenAI 兼容的 `/audio/transcriptions`。
async fn transcribe(State(rt): State<std::sync::Arc<Rt>>, headers: HeaderMap, body: Bytes) -> Response {
    let ct = headers.get("content-type").and_then(|v| v.to_str().ok()).map(str::to_string);
    rt.forward("/audio/transcriptions", ct.as_deref(), body).await
}

/// 语音合成：OpenAI 兼容的 `/audio/speech`。
async fn speech(State(rt): State<std::sync::Arc<Rt>>, headers: HeaderMap, body: Bytes) -> Response {
    let body = with_default_model(&rt, body);
    let ct = headers.get("content-type").and_then(|v| v.to_str().ok()).map(str::to_string);
    rt.forward("/audio/speech", ct.as_deref(), body).await
}

/// 视频生成：OpenAI 兼容的 `/videos/generations`。
async fn video(State(rt): State<std::sync::Arc<Rt>>, headers: HeaderMap, body: Bytes) -> Response {
    let body = with_default_model(&rt, body);
    let ct = headers.get("content-type").and_then(|v| v.to_str().ok()).map(str::to_string);
    rt.forward("/videos/generations", ct.as_deref(), body).await
}

/// 检索重排：`/rerank`（Jina / Cohere / Voyage 都是这个形状）。
async fn rerank(State(rt): State<std::sync::Arc<Rt>>, headers: HeaderMap, body: Bytes) -> Response {
    let body = with_default_model(&rt, body);
    let ct = headers.get("content-type").and_then(|v| v.to_str().ok()).map(str::to_string);
    rt.forward("/rerank", ct.as_deref(), body).await
}

/// 请求没带 `model` 时补上扩展配置里的默认模型。
/// 不是 JSON 就原样转发，让上游自己报错。
fn with_default_model(rt: &Rt, body: Bytes) -> Bytes {
    let Ok(mut obj) = serde_json::from_slice::<serde_json::Map<String, serde_json::Value>>(&body)
    else {
        return body;
    };
    if !rt.model.is_empty() && !obj.contains_key("model") {
        obj.insert("model".into(), serde_json::Value::String(rt.model.clone()));
    }
    Bytes::from(serde_json::to_vec(&obj).unwrap_or_default())
}

/// 图片生成：OpenAI 兼容的 `/images/generations`。
///
/// 请求没带 model 时补上扩展配置里的默认模型——配置了默认值就不该让
/// 每个调用方都背一遍模型名。
async fn generate(State(rt): State<std::sync::Arc<Rt>>, headers: HeaderMap, body: Bytes) -> Response {
    let body = with_default_model(&rt, body);
    let ct = headers.get("content-type").and_then(|v| v.to_str().ok()).map(str::to_string);
    rt.forward("/images/generations", ct.as_deref(), body).await
}

/// sidecar 进程入口。出错就带非零码退出，宿主会在下一次请求时重试拉起。
pub fn main_entry(id: &str) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("sidecar 建不出 tokio runtime");

    if let Err(e) = rt.block_on(run(id)) {
        eprintln!("sidecar {id} 启动失败：{e}");
        tracing::error!("sidecar {id} 启动失败：{e}");
        std::process::exit(1);
    }
}

async fn run(id: &str) -> Result<(), String> {
    // 配置：stdin 一行 JSON。读不到就退出，宿主会看到非零码。
    let mut line = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut line)
        .map_err(|e| format!("读配置失败：{e}"))?;
    let cfg: SidecarCfg =
        serde_json::from_str(line.trim()).map_err(|e| format!("配置解析失败：{e}"))?;
    if cfg.id != id {
        return Err(format!("配置 id（{}）与启动参数（{id}）不一致", cfg.id));
    }

    let rt_state = std::sync::Arc::new(Rt::from_cfg(cfg)?);
    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/transcribe", post(transcribe))
        .route("/generate", post(generate))
        .route("/speech", post(speech))
        .route("/video", post(video))
        .route("/rerank", post(rerank))
        .with_state(rt_state);

    // 只绑回环。动态端口由内核分配，永远不写死。
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| format!("绑端口失败：{e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();

    // 端口握手：宿主就等这一行
    println!("PORT {port}");
    use std::io::Write;
    std::io::stdout().flush().map_err(|e| e.to_string())?;

    tracing::info!("sidecar {id} listening on 127.0.0.1:{port}");
    axum::serve(listener, app).await.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn set(items: &[&str]) -> HashSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn egress_matches_exact_host() {
        let d = set(&["api.groq.com"]);
        assert!(egress_allowed(&d, "api.groq.com"));
        assert!(egress_allowed(&d, "API.GROQ.COM"));
        assert!(!egress_allowed(&d, "evil.api.groq.com.attacker.io"));
        assert!(!egress_allowed(&d, "groq.com"));
        assert!(!egress_allowed(&d, ""));
    }

    #[test]
    fn egress_matches_wildcard() {
        let d = set(&["*.groq.com"]);
        assert!(egress_allowed(&d, "api.groq.com"));
        assert!(!egress_allowed(&d, "groq.com"));
        assert!(!egress_allowed(&d, "xgroq.com"));
    }

    #[tokio::test]
    async fn rejects_base_url_outside_permissions() {
        let cfg = SidecarCfg {
            id: "asr".into(),
            base_url: "https://attacker.io/v1".into(),
            api_key: String::new(),
            model: String::new(),
            domains: vec!["api.groq.com".into()],
        };
        assert!(Rt::from_cfg(cfg).is_err());
    }

    #[tokio::test]
    async fn accepts_base_url_inside_permissions() {
        let cfg = SidecarCfg {
            id: "asr".into(),
            base_url: "https://api.groq.com/openai/v1".into(),
            api_key: String::new(),
            model: String::new(),
            domains: vec!["api.groq.com".into()],
        };
        assert!(Rt::from_cfg(cfg).is_ok());
    }
}
