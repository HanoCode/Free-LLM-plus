//! 扩展宿主与网关的接驳。
//!
//! 设计边界（扩展页那句脚注的代码化）：图片 / 音频这类非 chat 能力走扩展、
//! 不进主网关——所以这里**不碰** Gate / Order / Pick / 账本 / 冷却，
//! 只做「校验身份 → 检查开关 → 唤起 sidecar → 透明转发」。
//! sidecar 的故障域完全独立：它崩了，chat 主链路连一个字节都不会知道。

use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;
use serde_json::json;

use freeport_ext::SidecarCfg;

use crate::secrets;
use crate::server::{oai_error, resolve_key, save_here, Gateway, Inflight, State_};
use crate::store;

/// 扩展自己的凭据在密钥库里的槽位前缀。
/// 独立槽位 = 独立授权模型：扩展的 key 永远不会误发给 chat 候选。
fn ext_slot(id: &str) -> String {
    format!("ext:{id}")
}

/// 给控制面的扩展状态。端口故意不在这里——它只进内部路由表。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GwExt {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub version: String,
    pub author: String,
    pub domains: Vec<String>,
    pub paths: Vec<String>,
    pub trusted: bool,
    pub running: bool,
    pub enabled: bool,
    pub base_url: String,
    pub model: String,
    pub key_mask: String,
    pub has_key: bool,
    /// 调用方实际打哪个端点
    pub endpoint: &'static str,
}

const ASR_ENDPOINT: &str = "/v1/audio/transcriptions";
const IMAGE_ENDPOINT: &str = "/v1/images/generations";
const SPEECH_ENDPOINT: &str = "/v1/audio/speech";
const VIDEO_ENDPOINT: &str = "/v1/videos/generations";
const RERANK_ENDPOINT: &str = "/v1/rerank";

fn endpoint_of(kind: &str) -> &'static str {
    match kind {
        "asr" => ASR_ENDPOINT,
        "speech" => SPEECH_ENDPOINT,
        "video" => VIDEO_ENDPOINT,
        "rerank" => RERANK_ENDPOINT,
        _ => IMAGE_ENDPOINT,
    }
}

impl Gateway {
    /// 扩展清单 = 扩展目录里的可信 manifest（缺失时由内置份修复）+ 用户配置。
    pub async fn ext_list(&self) -> Vec<GwExt> {
        // 目录只解析一次：`extensions_dir()` 读的是进程级环境变量，
        // 解析两遍就可能「装进 A、却从 B 读」——并行测试里这是真的会发生的。
        let dir = freeport_ext::install::extensions_dir();
        // 目录可能被手动清掉：列表入口兜底装一次
        freeport_ext::install::materialize_builtin(&dir);

        let installed = freeport_ext::install::installed(&dir);
        let host = freeport_ext::shared();
        let d = self.st.data.read().await;

        let mut out = Vec::new();
        for m in &installed {
            let cfg = d.extensions.get(&m.id).cloned().unwrap_or_default();
            let has_key = secrets::get_key(&ext_slot(&m.id)).is_some();
            let running = host.status(&m.id).await;
            out.push(GwExt {
                running,
                endpoint: endpoint_of(m.kind.as_str()),
                has_key,
                id: m.id.clone(),
                name: m.name.clone(),
                kind: m.kind.as_str().to_string(),
                version: m.version.clone(),
                author: m.author.clone(),
                domains: m.permissions.domains.clone(),
                paths: m.permissions.paths.clone(),
                trusted: true, // installed() 只返回验签通过的
                enabled: cfg.enabled,
                base_url: cfg.base_url,
                model: cfg.model,
                key_mask: cfg.key_mask,
            });
        }
        out
    }

    /// 更新扩展配置。关闭时顺手停掉 sidecar——「关了还在跑」比崩溃更骗人。
    pub async fn ext_set_config(
        &self,
        id: &str,
        enabled: bool,
        base_url: String,
        model: String,
    ) -> Result<Vec<GwExt>, String> {
        if freeport_ext::install::builtin_by_id(id).is_none() {
            return Err(format!("未知扩展：{id}"));
        }
        let base_url = base_url.trim().trim_end_matches('/').to_string();
        if enabled && base_url.is_empty() {
            return Err("启用前先填上游地址".into());
        }
        if enabled {
            // 启用前先照一次门槛：上游域名必须在 manifest 的授权清单里
            let m = freeport_ext::install::builtin_by_id(id).expect("刚检查过");
            let host = reqwest::Url::parse(&base_url)
                .map_err(|e| format!("上游地址不合法：{e}"))?
                .host_str()
                .unwrap_or("")
                .to_ascii_lowercase();
            let ok = m
                .permissions
                .domains
                .iter()
                .any(|d| d.eq_ignore_ascii_case(&host) || {
                    d.strip_prefix("*.")
                        .map(|suffix| host.ends_with(suffix))
                        .unwrap_or(false)
                });
            if !ok {
                return Err(format!(
                    "上游 {host} 不在该扩展声明的域名清单里：权限说清楚才能开"
                ));
            }
        }

        let mut d = self.st.data.write().await;
        let entry = d.extensions.entry(id.to_string()).or_default();
        entry.enabled = enabled;
        entry.base_url = base_url;
        entry.model = model;
        let snapshot = d.clone();
        drop(d);
        if !save_here(&self.st, &snapshot) {
            tracing::error!("扩展配置落盘失败：{id}");
        }
        if !enabled {
            freeport_ext::shared().stop(id).await;
        }
        Ok(self.ext_list().await)
    }

    /// 换扩展自己的凭据。明文进密钥库，配置里只留掩码。
    pub async fn ext_set_key(&self, id: &str, key: &str) -> Result<Vec<GwExt>, String> {
        if freeport_ext::install::builtin_by_id(id).is_none() {
            return Err(format!("未知扩展：{id}"));
        }
        let key = key.trim();
        if key.is_empty() {
            return Err("key 不能为空（要清掉请直接停用扩展）".into());
        }
        secrets::set_key(&ext_slot(id), key)?;
        let mask = store::mask(key);
        let mut d = self.st.data.write().await;
        d.extensions.entry(id.to_string()).or_default().key_mask = mask;
        let snapshot = d.clone();
        drop(d);
        if !save_here(&self.st, &snapshot) {
            tracing::error!("扩展配置落盘失败：{id}");
        }
        Ok(self.ext_list().await)
    }

    /// 手动停掉 sidecar（下个请求会按需再拉起）。
    pub async fn ext_stop(&self, id: &str) -> Vec<GwExt> {
        freeport_ext::shared().stop(id).await;
        self.ext_list().await
    }
}

/* ── 数据面入口 ─────────────────────────────────────── */

pub(crate) async fn asr_entry(
    State(st): State<Arc<State_>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    ext_proxy(&st, &headers, body, "asr", "transcribe").await
}

pub(crate) async fn image_entry(
    State(st): State<Arc<State_>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    ext_proxy(&st, &headers, body, "image", "generate").await
}

pub(crate) async fn speech_entry(
    State(st): State<Arc<State_>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    ext_proxy(&st, &headers, body, "speech", "speech").await
}

pub(crate) async fn video_entry(
    State(st): State<Arc<State_>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    ext_proxy(&st, &headers, body, "video", "video").await
}

pub(crate) async fn rerank_entry(
    State(st): State<Arc<State_>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    ext_proxy(&st, &headers, body, "rerank", "rerank").await
}

fn ext_unavailable(code: &str, msg: &str) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(oai_error(msg, "service_unavailable", code, json!({}))),
    )
        .into_response()
}

/// 共同路径：鉴权 → 开关 → 配置 → sidecar → 透传。
async fn ext_proxy(
    st: &Arc<State_>,
    headers: &HeaderMap,
    body: Bytes,
    id: &str,
    sidecar_path: &str,
) -> Response {
    // 与 chat 同一套虚拟 key：鉴权与归因一致，但不记 chat 账本
    let Some(_identity) = resolve_key(st, headers).await else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(oai_error(
                "未授权：请带上 Authorization: Bearer <Freeport Key>",
                "invalid_request_error",
                "unauthorized",
                json!({}),
            )),
        )
            .into_response();
    };

    let _inflight = Inflight::new(&st.inflight);

    let (enabled, base_url, model) = {
        let d = st.data.read().await;
        match d.extensions.get(id) {
            Some(c) => (c.enabled, c.base_url.clone(), c.model.clone()),
            None => (false, String::new(), String::new()),
        }
    };
    if !enabled {
        return ext_unavailable(
            "ext_disabled",
            "扩展未启用：在「扩展」页打开并配好上游后再试",
        );
    }
    if base_url.is_empty() {
        return ext_unavailable("ext_unconfigured", "扩展还没配上游地址");
    }
    let Some(api_key) = secrets::get_key(&ext_slot(id)) else {
        return ext_unavailable("ext_no_key", "扩展还没有自己的凭据：凭据独立于 chat 厂商单独配");
    };

    let domains = freeport_ext::install::builtin_by_id(id)
        .map(|m| m.permissions.domains.clone())
        .unwrap_or_default();
    let port = match freeport_ext::shared()
        .ensure_running(&SidecarCfg {
            id: id.to_string(),
            base_url: base_url.clone(),
            api_key,
            model,
            domains,
        })
        .await
    {
        Ok(p) => p,
        Err(e) => {
            // sidecar 起不来是它自己的故障域，主网关照常活着
            tracing::error!("sidecar {id} 不可用：{e}");
            return (
                StatusCode::BAD_GATEWAY,
                Json(oai_error(&format!("sidecar 启动失败：{e}"), "upstream_error", "ext_sidecar", json!({}))),
            )
                .into_response();
        }
    };

    // 内部一跳：body 原样透传，协议的解释权归上游
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(330))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(oai_error(&format!("构造内部客户端失败：{e}"), "api_error", "ext_internal", json!({}))),
            )
                .into_response()
        }
    };
    let mut req = client
        .post(format!("http://127.0.0.1:{port}/{sidecar_path}"))
        .body(body);
    if let Some(ct) = headers.get("content-type").and_then(|v| v.to_str().ok()) {
        req = req.header("Content-Type", ct);
    }
    match req.send().await {
        Ok(up) => {
            let status =
                StatusCode::from_u16(up.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
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
                    Json(oai_error(&format!("读 sidecar 响应失败：{e}"), "upstream_error", "ext_sidecar", json!({}))),
                )
                    .into_response(),
            }
        }
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(oai_error(
                &format!("sidecar 进程无响应（可能已崩溃，下一次请求会自动重拉）：{e}"),
                "upstream_error",
                "ext_sidecar_down",
                json!({}),
            )),
        )
            .into_response(),
    }
}

/// 给测试和宿主用：网关关闭时把 sidecar 一起带走。
pub async fn shutdown_all() {
    freeport_ext::shared().stop_all().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    /// `FREEPORT_DATA_DIR` 是**进程级**的，而各模块的用例都要把它指到自己的临时目录。
    /// 并行跑时会互相覆盖：A 刚设好，B 又改指别处，A 随后读到的就是一个空目录
    /// （表现为「内置扩展凭空消失」）。
    ///
    /// 锁放在 `store` 里而不是这个模块里：secrets 的用例也在改同一个变量，
    /// 模块私有的锁彼此看不见，等于没锁——真实踩过一次，症状是扩展用例随机挂。
    fn env_guard() -> std::sync::MutexGuard<'static, ()> {
        crate::store::env_lock()
    }

    #[tokio::test]
    async fn disabled_ext_is_503_not_fake_success() {
        // 这条不碰环境变量，但它会走到 `ext_list()` → 往「当前」扩展目录落 manifest。
        // 不拿锁就会在别的用例改 env 的窗口里插进去，把对方的目录当成自己的写。
        let _env = env_guard();
        let gw = Gateway::new();
        let resp = ext_proxy(&gw.st, &HeaderMap::new(), Bytes::new(), "asr", "transcribe").await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn unknown_ext_rejected() {
        let gw = Gateway::new();
        assert!(gw.ext_set_config("nope", true, "https://api.groq.com/openai/v1".into(), "m".into()).await.is_err());
    }

    #[tokio::test]
    async fn enable_requires_base_url() {
        let gw = Gateway::new();
        let err = gw.ext_set_config("asr", true, String::new(), String::new()).await.unwrap_err();
        assert!(err.contains("上游地址"));
    }

    #[tokio::test]
    async fn enable_rejects_domain_outside_permissions() {
        // 门槛一在控制面的执行：manifest 没声明的上游，UI 里也开不了
        let _env = env_guard();
        let gw = Gateway::new();
        let err = gw
            .ext_set_config("asr", true, "https://attacker.io/v1".into(), "m".into())
            .await
            .unwrap_err();
        assert!(err.contains("域名清单"));
    }

    #[tokio::test]
    async fn enable_with_permitted_domain_persists() {
        // 落盘走数据目录，测试里指到临时目录，别碰真实配置
        let _env = env_guard();
        let dir = std::env::temp_dir().join(format!("fp-gw-ext-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("FREEPORT_DATA_DIR", &dir);

        let gw = Gateway::new();
        let list = gw
            .ext_set_config("asr", true, "https://api.groq.com/openai/v1".into(), "whisper-large-v3-turbo".into())
            .await
            .unwrap();
        let asr = list.iter().find(|e| e.id == "asr").unwrap();
        assert!(asr.enabled);
        assert_eq!(asr.base_url, "https://api.groq.com/openai/v1");
        assert!(asr.has_key == false);
        assert_eq!(asr.endpoint, "/v1/audio/transcriptions");
        // 权限来自 manifest，不由配置决定
        assert!(asr.domains.contains(&"api.groq.com".to_string()));

        // 关掉：配置生效且 sidecar 停止指令发出（本来就没在跑，幂等）
        let list = gw.ext_set_config("asr", false, "https://api.groq.com/openai/v1".into(), String::new()).await.unwrap();
        let asr = list
            .iter()
            .find(|e| e.id == "asr")
            .expect("关掉之后 asr 仍应出现在列表里，缺了说明 manifest 没读出来");
        assert!(!asr.enabled, "关闭必须立刻在返回的列表里生效");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 未授权（require_auth 默认关）时扩展入口放行；开关打开后未带 key 应 401。
    #[tokio::test]
    async fn auth_gate_on_ext_entry() {
        let _env = env_guard();
        let dir = std::env::temp_dir().join(format!("fp-gw-ext-auth-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("FREEPORT_DATA_DIR", &dir);

        let gw = Gateway::new();
        gw.st.data.write().await.gateway.require_auth = true;
        let mut headers = HeaderMap::new();
        headers.insert("authorization", HeaderValue::from_static("Bearer wrong"));
        let resp = ext_proxy(&gw.st, &headers, Bytes::new(), "asr", "transcribe").await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
