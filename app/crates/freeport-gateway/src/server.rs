//! 数据面：HTTP 入口 + 失败接力 + 流式转发。
//!
//! 只监听 127.0.0.1。控制面（增删厂商、读日志）不开放 HTTP，
//! 由宿主通过进程内调用拿到 —— 避免本机任意进程改配置。

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use freeport_core::cooldown::Cooldowns;
use freeport_core::dto::*;
use freeport_core::error::classify;
use freeport_core::model::*;
use freeport_core::{plan, presets};
use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio::sync::{Notify, RwLock};
use tokio_stream::wrappers::ReceiverStream;
use tower_http::cors::CorsLayer;

use crate::providers;
use crate::secrets;
use crate::store;

const MAX_BODY: usize = 8 * 1024 * 1024;
const LOG_RING: usize = 300;

pub type LogSink = Arc<dyn Fn(LogEntry) + Send + Sync>;

struct State_ {
    data: RwLock<AppData>,
    cooldowns: Cooldowns,
    logs: RwLock<Vec<LogEntry>>,
    client: reqwest::Client,
    started_at: RwLock<Option<Instant>>,
    bound_port: AtomicU16,
    running: AtomicBool,
    on_log: RwLock<Option<LogSink>>,
    shutdown: Notify,
}

pub struct Gateway {
    st: Arc<State_>,
}

impl Gateway {
    pub fn new() -> Self {
        Self {
            st: Arc::new(State_ {
                data: RwLock::new(AppData::default()),
                cooldowns: Cooldowns::new(),
                logs: RwLock::new(Vec::new()),
                client: providers::client(),
                started_at: RwLock::new(None),
                bound_port: AtomicU16::new(0),
                running: AtomicBool::new(false),
                on_log: RwLock::new(None),
                shutdown: Notify::new(),
            }),
        }
    }

    pub async fn set_log_sink(&self, sink: LogSink) {
        *self.st.on_log.write().await = Some(sink);
    }

    /// 载入磁盘配置。宿主启动时调一次。
    pub async fn load(&self) {
        let data = store::load();
        *self.st.data.write().await = data;
    }

    pub async fn start(&self) -> Result<u16, String> {
        let (port, host) = {
            let d = self.st.data.read().await;
            (d.gateway.port, d.gateway.host.clone())
        };

        let listener = tokio::net::TcpListener::bind(format!("{host}:{port}"))
            .await
            .map_err(|e| format!("监听 {host}:{port} 失败：{e}"))?;
        let actual = listener.local_addr().map(|a| a.port()).unwrap_or(port);

        let app = Router::new()
            .route("/health", get(health))
            .route("/v1/models", get(list_models))
            .route("/v1/route", get(route_debug))
            .route("/v1/chat/completions", post(chat))
            .layer(DefaultBodyLimit::max(MAX_BODY))
            .layer(CorsLayer::permissive())
            .with_state(self.st.clone());

        self.st.bound_port.store(actual, Ordering::SeqCst);
        self.st.running.store(true, Ordering::SeqCst);
        *self.st.started_at.write().await = Some(Instant::now());

        let st = self.st.clone();
        tokio::spawn(async move {
            let shutdown = st.clone();
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    shutdown.shutdown.notified().await;
                })
                .await;
            st.running.store(false, Ordering::SeqCst);
        });

        tracing::info!("gateway listening on http://{host}:{actual}");
        Ok(actual)
    }

    pub async fn stop(&self) {
        self.st.shutdown.notify_waiters();
        self.st.running.store(false, Ordering::SeqCst);
        self.st.bound_port.store(0, Ordering::SeqCst);
    }

    /* ── 控制面 ───────────────────────────────────── */

    pub async fn status(&self) -> GwStatus {
        let d = self.st.data.read().await;
        GwStatus {
            running: self.st.running.load(Ordering::SeqCst),
            port: match self.st.bound_port.load(Ordering::SeqCst) {
                0 => None,
                p => Some(p),
            },
            bind: d.gateway.host.clone(),
            since: {
                let guard = self.st.started_at.read().await;
                guard.map(|t| {
                    let elapsed = chrono::Duration::from_std(t.elapsed()).unwrap_or_default();
                    (chrono::Utc::now() - elapsed).to_rfc3339()
                })
            },
            auth_token: d.gateway.auth_token.clone(),
            require_auth: d.gateway.require_auth,
            requests24h: d.stats.requests,
            tokens_in: d.stats.tokens_in,
            tokens_out: d.stats.tokens_out,
            errors: d.stats.errors,
            cooldowns: self.st.cooldowns.snapshot(),
            error: None,
        }
    }

    pub async fn providers(&self) -> Vec<GwProvider> {
        let d = self.st.data.read().await;
        d.providers
            .iter()
            .map(|p| GwProvider::from(p, secrets::get_key(&p.id).is_some()))
            .collect()
    }

    pub async fn models(&self) -> Vec<GwModel> {
        let d = self.st.data.read().await;
        aggregate_models(&d)
    }

    pub async fn routing(&self) -> GwRouting {
        let d = self.st.data.read().await;
        GwRouting {
            strategy: d.routing.strategy.clone(),
            chain: d.routing.chain.clone(),
            cooldowns: self.st.cooldowns.snapshot(),
        }
    }

    pub fn presets(&self) -> Vec<presets::ProviderPreset> {
        presets::all()
    }

    pub async fn logs(&self) -> Vec<LogEntry> {
        self.st.logs.read().await.clone()
    }

    pub async fn add_provider(&self, input: ProviderInput) -> Result<AddProviderResult, String> {
        let pid: String = input
            .id
            .trim()
            .to_lowercase()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
            .collect();
        if pid.is_empty() {
            return Err("缺少 provider id".into());
        }
        if input.base_url.trim().is_empty() {
            return Err("缺少 baseUrl".into());
        }
        if input.api_key.trim().is_empty() {
            return Err("缺少 API Key".into());
        }

        let preset = presets::get(&pid);
        let mut provider = Provider {
            id: pid.clone(),
            name: input
                .name
                .clone()
                .or_else(|| preset.as_ref().map(|p| p.name.clone()))
                .unwrap_or_else(|| pid.clone()),
            base_url: input.base_url.trim_end_matches('/').to_string(),
            tier: input
                .tier
                .clone()
                .or_else(|| preset.as_ref().map(|p| p.tier.clone()))
                .unwrap_or_else(|| "C".into()),
            homepage: preset.as_ref().and_then(|p| p.homepage.clone()),
            note: preset.as_ref().and_then(|p| p.note.clone()),
            key_mask: store::mask(&input.api_key),
            enabled: true,
            added_at: chrono::Utc::now().to_rfc3339(),
            models: Vec::new(),
            health: Health {
                status: HealthStatus::Probing,
                checked_at: Some(chrono::Utc::now().to_rfc3339()),
                ..Default::default()
            },
        };

        // 用真 key 拉一次模型；拉不到就不入库
        let t0 = Instant::now();
        match providers::list_models(&self.st.client, &provider, &input.api_key).await {
            Ok(models) => {
                provider.health = Health {
                    status: HealthStatus::Healthy,
                    latency_ms: Some(t0.elapsed().as_millis() as u64),
                    checked_at: Some(chrono::Utc::now().to_rfc3339()),
                    model_count: Some(models.len()),
                    error: None,
                };
                provider.models = models;
            }
            Err(e) => {
                let detail = format!("{} {}", if e.status > 0 { e.status.to_string() } else { String::new() }, e.body)
                    .trim()
                    .chars()
                    .take(200)
                    .collect::<String>();
                return Err(format!("校验失败：{}", detail));
            }
        }

        secrets::set_key(&pid, &input.api_key)?;

        let model_count = provider.models.len();
        let chain_entries: Vec<String> = provider
            .models
            .iter()
            .take(2)
            .map(|m| format!("{pid}:{}", m.id))
            .collect();

        let mut d = self.st.data.write().await;
        if let Some(existing) = d.providers.iter_mut().find(|p| p.id == pid) {
            *existing = provider;
        } else {
            d.providers.push(provider);
        }
        for entry in chain_entries {
            if !d.routing.chain.contains(&entry) {
                d.routing.chain.push(entry);
            }
        }
        store::save(&d);
        let saved = d.providers.iter().find(|p| p.id == pid).cloned();
        drop(d);

        let provider = saved
            .map(|p| GwProvider::from(&p, true))
            .ok_or_else(|| "保存失败".to_string())?;

        Ok(AddProviderResult {
            provider,
            model_count,
        })
    }

    pub async fn remove_provider(&self, id: &str) -> Vec<GwProvider> {
        {
            let mut d = self.st.data.write().await;
            d.providers.retain(|p| p.id != id);
            d.routing.chain.retain(|c| !c.starts_with(&format!("{id}:")));
            store::save(&d);
        }
        secrets::delete_key(id);
        self.providers().await
    }

    pub async fn toggle_provider(&self, id: &str, enabled: bool) -> Result<Vec<GwProvider>, String> {
        {
            let mut d = self.st.data.write().await;
            let p = d
                .providers
                .iter_mut()
                .find(|p| p.id == id)
                .ok_or_else(|| "provider 不存在".to_string())?;
            p.enabled = enabled;
            store::save(&d);
        }
        Ok(self.providers().await)
    }

    pub async fn probe(&self, id: &str) -> ProbeResult {
        let provider = {
            let d = self.st.data.read().await;
            d.providers.iter().find(|p| p.id == id).cloned()
        };
        let Some(mut p) = provider else {
            return ProbeResult {
                ok: false,
                models: None,
                latency_ms: None,
                error: Some("provider 不存在".into()),
            };
        };
        let Some(key) = secrets::get_key(id) else {
            return ProbeResult {
                ok: false,
                models: None,
                latency_ms: None,
                error: Some("系统钥匙串里没有这个 provider 的 key，请重新输入".into()),
            };
        };

        let t0 = Instant::now();
        match providers::list_models(&self.st.client, &p, &key).await {
            Ok(models) => {
                let latency = t0.elapsed().as_millis() as u64;
                p.health = Health {
                    status: HealthStatus::Healthy,
                    latency_ms: Some(latency),
                    checked_at: Some(chrono::Utc::now().to_rfc3339()),
                    model_count: Some(models.len()),
                    error: None,
                };
                p.models = models.clone();
                let mut d = self.st.data.write().await;
                if let Some(slot) = d.providers.iter_mut().find(|x| x.id == id) {
                    *slot = p;
                }
                store::save(&d);
                ProbeResult {
                    ok: true,
                    models: Some(models.len()),
                    latency_ms: Some(latency),
                    error: None,
                }
            }
            Err(e) => {
                let latency = t0.elapsed().as_millis() as u64;
                let detail = e.body.chars().take(240).collect::<String>();
                p.health = Health {
                    status: HealthStatus::Error,
                    latency_ms: Some(latency),
                    checked_at: Some(chrono::Utc::now().to_rfc3339()),
                    model_count: None,
                    error: Some(detail.clone()),
                };
                let mut d = self.st.data.write().await;
                if let Some(slot) = d.providers.iter_mut().find(|x| x.id == id) {
                    *slot = p;
                }
                store::save(&d);
                ProbeResult {
                    ok: false,
                    models: None,
                    latency_ms: Some(latency),
                    error: Some(detail),
                }
            }
        }
    }

    pub fn reveal_key(&self, id: &str) -> String {
        secrets::get_key(id).unwrap_or_default()
    }

    pub async fn set_routing(&self, patch: RoutingPatch) -> GwRouting {
        {
            let mut d = self.st.data.write().await;
            if let Some(s) = patch.strategy {
                d.routing.strategy = s;
            }
            if let Some(c) = patch.chain {
                d.routing.chain = c;
            }
            store::save(&d);
        }
        self.routing().await
    }

    pub async fn set_config(&self, patch: ConfigPatch) -> GwStatus {
        {
            let mut d = self.st.data.write().await;
            if let Some(v) = patch.require_auth {
                d.gateway.require_auth = v;
            }
            if let Some(p) = patch.port {
                d.gateway.port = p;
            }
            store::save(&d);
        }
        self.status().await
    }

    pub fn clear_cooldowns(&self) {
        self.st.cooldowns.clear();
    }
}

impl Default for Gateway {
    fn default() -> Self {
        Self::new()
    }
}

/* ── handlers ─────────────────────────────────────── */

fn oai_error(message: &str, kind: &str, code: &str, extra: Value) -> Value {
    let mut err = json!({ "message": message, "type": kind, "code": code });
    if let Some(obj) = extra.as_object() {
        for (k, v) in obj {
            err[k] = v.clone();
        }
    }
    json!({ "error": err })
}

async fn health(State(st): State<Arc<State_>>) -> impl IntoResponse {
    let d = st.data.read().await;
    let uptime = st
        .started_at
        .read()
        .await
        .map(|t| t.elapsed().as_secs())
        .unwrap_or(0);
    Json(json!({
        "ok": true,
        "name": "freeport",
        "version": env!("CARGO_PKG_VERSION"),
        "uptimeSec": uptime,
        "probe": true,
        "providers": d.providers.iter().filter(|p| p.enabled).count(),
    }))
}

async fn list_models(State(st): State<Arc<State_>>, headers: HeaderMap) -> Response {
    if !authorized(&st, &headers).await {
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
    }
    let models = {
        let d = st.data.read().await;
        aggregate_models(&d)
    };
    Json(json!({ "object": "list", "data": models })).into_response()
}

async fn route_debug(
    State(st): State<Arc<State_>>,
    Query(q): Query<std::collections::HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    if !authorized(&st, &headers).await {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let requested = q.get("model").cloned().unwrap_or_else(|| "auto".into());
    let d = st.data.read().await;
    let plan = plan(&d, &requested, &st.cooldowns, &|p: &Provider| {
        secrets::get_key(&p.id).is_some()
    });
    Json(json!({
        "chain": plan.candidates.iter().map(|c| format!("{}:{}", d.providers[c.provider].id, c.model_id)).collect::<Vec<_>>(),
        "rejected": plan.rejected.iter().map(|r| format!("{}:{} ({})", r.provider, r.model, r.reason)).collect::<Vec<_>>(),
        "cooldowns": st.cooldowns.snapshot(),
        "strategy": d.routing.strategy,
    }))
    .into_response()
}

async fn authorized(st: &Arc<State_>, headers: &HeaderMap) -> bool {
    let require = st.data.read().await.gateway.require_auth;
    if !require {
        return true;
    }
    let expected = st.data.read().await.gateway.auth_token.clone();
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim_start_matches("Bearer ").trim().to_string())
        .unwrap_or_default();
    !token.is_empty() && !expected.is_empty() && constant_time_eq(token.as_bytes(), expected.as_bytes())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

async fn chat(State(st): State<Arc<State_>>, headers: HeaderMap, body: Bytes) -> Response {
    if !authorized(&st, &headers).await {
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
    }

    let started = Instant::now();
    let payload: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(oai_error("请求体不是合法 JSON", "invalid_request_error", "bad_json", json!({}))),
            )
                .into_response()
        }
    };

    let stream = payload.get("stream").and_then(|v| v.as_bool()).unwrap_or(false);
    let requested = payload
        .get("model")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "auto".into());

    let (budget_ms, plan, has_any_key) = {
        let d = st.data.read().await;
        let plan = plan(&d, &requested, &st.cooldowns, &|p: &Provider| {
            secrets::get_key(&p.id).is_some()
        });
        let has_any = d.providers.iter().any(|p| secrets::get_key(&p.id).is_some());
        (d.gateway.request_budget_ms, plan, has_any)
    };

    let mut entry = LogEntry {
        id: format!("req_{}", &uuid::Uuid::new_v4().simple().to_string()[..12]),
        at: chrono::Utc::now().to_rfc3339(),
        path: "/v1/chat/completions".into(),
        requested_model: requested.clone(),
        model: None,
        provider: None,
        stream,
        ok: false,
        status: 0,
        ms: 0,
        usage: None,
        attempts: Vec::new(),
        error: None,
    };

    if plan.is_empty() {
        let msg = if has_any_key {
            "所有候选都在冷却中，或没有可用模型"
        } else {
            "还没有接入任何厂商，请先在「厂商」页添加 API Key"
        };
        entry.status = 503;
        entry.ms = started.elapsed().as_millis() as u64;
        entry.error = Some(msg.into());
        record(&st, entry).await;
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(oai_error(
                msg,
                "freeport_no_route",
                "no_candidate",
                json!({ "cooling": st.cooldowns.snapshot().into_iter().take(10).collect::<Vec<_>>() }),
            )),
        )
            .into_response();
    }

    let mut skipped: HashSet<(usize, String)> = HashSet::new();

    let queue = plan.candidates.clone();
    for cand in queue {
        if started.elapsed() > Duration::from_millis(budget_ms) && entry.attempts.len() >= 2 {
            break;
        }
        if skipped.contains(&(cand.provider, cand.model_id.clone())) {
            continue;
        }

        let (provider, key) = {
            let d = st.data.read().await;
            let p = d.providers[cand.provider].clone();
            let key = secrets::get_key(&p.id);
            (p, key)
        };
        let Some(key) = key else { continue };

        let mut body_for_upstream = payload.clone();
        body_for_upstream["model"] = json!(cand.model_id);
        if stream && body_for_upstream.get("stream_options").is_none() {
            body_for_upstream["stream_options"] = json!({ "include_usage": true });
        }

        let remaining = Duration::from_millis(
            budget_ms.saturating_sub(started.elapsed().as_millis() as u64).max(1000),
        );
        let t0 = Instant::now();

        match providers::create_chat(&st.client, &provider, &key, &body_for_upstream, remaining).await {
            Ok(upstream) => {
                let status = upstream.status().as_u16();
                entry.attempts.push(Attempt {
                    provider: provider.id.clone(),
                    model: cand.model_id.clone(),
                    ms: t0.elapsed().as_millis() as u64,
                    status,
                    kind: "ok".into(),
                    scope: None,
                    cooled_ms: None,
                    detail: None,
                    skip_provider: None,
                });
                entry.model = Some(cand.model_id.clone());
                entry.provider = Some(provider.id.clone());
                entry.ok = true;
                entry.status = 200;
                st.cooldowns
                    .note_success(freeport_core::Scope::Model, &provider.id, Some(&cand.model_id));
                if cand.half_open {
                    tracing::info!("half-open probe succeeded: {}", provider.id);
                }

                let trail = trail_of(&entry);
                let provider_id = provider.id.clone();
                let model_id = cand.model_id.clone();

                if stream {
                    return stream_response(
                        st.clone(),
                        upstream,
                        entry,
                        started,
                        provider_id,
                        model_id,
                        trail,
                    );
                }

                let text = upstream.text().await.unwrap_or_default();
                let json: Value = serde_json::from_str(&text).unwrap_or_else(|_| json!({ "raw": text }));
                entry.usage = json.get("usage").and_then(parse_usage);
                entry.ms = started.elapsed().as_millis() as u64;
                bump_stats(&st, &entry.usage, false).await;
                record(&st, entry).await;

                let mut res = Response::new(Body::from(json.to_string()));
                *res.status_mut() = StatusCode::OK;
                let h = res.headers_mut();
                h.insert("content-type", HeaderValue::from_static("application/json; charset=utf-8"));
                h.insert("x-freeport-provider", HeaderValue::from_str(&provider_id).unwrap_or(HeaderValue::from_static("")));
                h.insert("x-freeport-model", HeaderValue::from_str(&model_id).unwrap_or(HeaderValue::from_static("")));
                h.insert("x-freeport-trail", HeaderValue::from_str(&trail).unwrap_or(HeaderValue::from_static("")));
                return res;
            }
            Err(e) => {
                let cls = classify(e.status, &e.body, e.retry_after_ms);
                let cooled = st.cooldowns.hit(
                    cls.scope,
                    &provider.id,
                    Some(&cand.model_id),
                    cls.cooldown_ms,
                    &cls.kind,
                );
                entry.attempts.push(Attempt {
                    provider: provider.id.clone(),
                    model: cand.model_id.clone(),
                    ms: t0.elapsed().as_millis() as u64,
                    status: e.status,
                    kind: cls.kind.clone(),
                    scope: Some(cls.scope.as_str().to_string()),
                    cooled_ms: Some(cooled),
                    detail: Some(e.body.chars().take(240).collect()),
                    skip_provider: matches!(cls.scope, freeport_core::Scope::Provider | freeport_core::Scope::Key)
                        .then_some(true),
                });
                if matches!(cls.scope, freeport_core::Scope::Provider | freeport_core::Scope::Key) {
                    // 该 provider 整体不可用，本轮直接跳过它的其余模型
                    for c in plan.candidates.iter() {
                        if c.provider == cand.provider {
                            skipped.insert((c.provider, c.model_id.clone()));
                        }
                    }
                }
            }
        }
    }

    let status = failure_status(&entry.attempts);
    entry.status = status;
    entry.ms = started.elapsed().as_millis() as u64;
    entry.error = Some(
        entry
            .attempts
            .iter()
            .map(|a| format!("{}:{}", a.provider, a.kind))
            .collect::<Vec<_>>()
            .join(", "),
    );
    let trail = entry.error.clone().unwrap_or_default();
    bump_stats(&st, &None, true).await;
    record(&st, entry.clone()).await;

    let kinds: Vec<String> = entry.attempts.iter().map(|a| a.kind.clone()).collect();
    let first_kind = kinds.first().cloned().unwrap_or_else(|| "no_candidate".into());
    let body = oai_error(
        &format!(
            "全部候选失败（{}）：共尝试 {} 次",
            kinds.join(" / "),
            entry.attempts.len()
        ),
        "freeport_all_failed",
        &first_kind,
        json!({ "attempts": entry.attempts }),
    );

    let mut res = (StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY), Json(body)).into_response();
    if let Ok(v) = HeaderValue::from_str(&trail) {
        res.headers_mut().insert("x-freeport-trail", v);
    }
    res
}

fn stream_response(
    st: Arc<State_>,
    upstream: reqwest::Response,
    mut entry: LogEntry,
    started: Instant,
    provider_id: String,
    model_id: String,
    trail: String,
) -> Response {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(32);

    tokio::spawn(async move {
        let mut tail = String::new();
        let mut stream = upstream.bytes_stream();
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(bytes) => {
                    // 只保留尾部用于抓 usage，最多 8KB
                    let piece = String::from_utf8_lossy(&bytes);
                    tail.push_str(&piece);
                    if tail.len() > 8192 {
                        let cut = tail.len() - 8192;
                        let cut = tail.char_indices().nth(cut).map(|(i, _)| i).unwrap_or(cut);
                        tail = tail[cut..].to_string();
                    }
                    if tx.send(Ok(bytes)).await.is_err() {
                        break; // 客户端断开
                    }
                }
                Err(e) => {
                    let msg = format!(
                        "data: {}\n\n",
                        oai_error(
                            &format!("上游流中断: {e}"),
                            "freeport_stream_error",
                            "stream_broken",
                            json!({})
                        )
                    );
                    let _ = tx.send(Ok(Bytes::from(msg))).await;
                    break;
                }
            }
        }
        entry.usage = extract_usage(&tail);
        entry.ms = started.elapsed().as_millis() as u64;
        bump_stats(&st, &entry.usage, false).await;
        record(&st, entry).await;
    });

    let body = Body::from_stream(ReceiverStream::new(rx));
    let mut res = Response::new(body);
    let h = res.headers_mut();
    h.insert("content-type", HeaderValue::from_static("text/event-stream; charset=utf-8"));
    h.insert("cache-control", HeaderValue::from_static("no-cache, no-transform"));
    h.insert("connection", HeaderValue::from_static("keep-alive"));
    h.insert("x-accel-buffering", HeaderValue::from_static("no"));
    h.insert("x-freeport-provider", HeaderValue::from_str(&provider_id).unwrap_or(HeaderValue::from_static("")));
    h.insert("x-freeport-model", HeaderValue::from_str(&model_id).unwrap_or(HeaderValue::from_static("")));
    h.insert("x-freeport-trail", HeaderValue::from_str(&trail).unwrap_or(HeaderValue::from_static("")));
    res
}

fn trail_of(entry: &LogEntry) -> String {
    entry
        .attempts
        .iter()
        .map(|a| {
            if a.status != 0 && a.status != 200 {
                format!("{}/{}:{}/{}", a.provider, a.model, a.kind, a.status)
            } else {
                format!("{}/{}:{}", a.provider, a.model, a.kind)
            }
        })
        .collect::<Vec<_>>()
        .join(" > ")
}

fn parse_usage(v: &Value) -> Option<Usage> {
    if !v.is_object() {
        return None;
    }
    Some(Usage {
        prompt_tokens: v.get("prompt_tokens").and_then(|x| x.as_u64()),
        completion_tokens: v.get("completion_tokens").and_then(|x| x.as_u64()),
        total_tokens: v.get("total_tokens").and_then(|x| x.as_u64()),
    })
}

/// 从 SSE 尾部找最后一个 usage 对象。
fn extract_usage(text: &str) -> Option<Usage> {
    if text.is_empty() {
        return None;
    }
    let mut best: Option<Usage> = None;
    for line in text.lines() {
        let payload = line.strip_prefix("data:").map(str::trim).unwrap_or(line.trim());
        if !payload.starts_with('{') || !payload.contains("\"usage\"") {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<Value>(payload) {
            if let Some(u) = v.get("usage").and_then(parse_usage) {
                if u.total_tokens.unwrap_or(0) > 0 || u.prompt_tokens.unwrap_or(0) > 0 {
                    best = Some(u);
                }
            }
        }
    }
    best
}

fn failure_status(attempts: &[Attempt]) -> u16 {
    if attempts.is_empty() {
        return 503;
    }
    let kinds: HashSet<&str> = attempts.iter().map(|a| a.kind.as_str()).collect();
    if kinds.len() == 1 {
        match attempts[0].kind.as_str() {
            "rate_limit" => return 429,
            "context" => return 413,
            "invalid_key" | "no_credit" | "quota" => return 502,
            _ => {}
        }
    }
    502
}

async fn bump_stats(st: &Arc<State_>, usage: &Option<Usage>, error: bool) {
    let mut d = st.data.write().await;
    d.stats.bump(
        usage.as_ref().map(|u| u.tokens_in()).unwrap_or(0),
        usage.as_ref().map(|u| u.tokens_out()).unwrap_or(0),
        error,
    );
}

async fn record(st: &Arc<State_>, entry: LogEntry) {
    {
        let mut logs = st.logs.write().await;
        logs.insert(0, entry.clone());
        if logs.len() > LOG_RING {
            logs.truncate(LOG_RING);
        }
    }
    let sink = st.on_log.read().await.clone();
    if let Some(f) = sink {
        f(entry);
    }
}
