//! 数据面：HTTP 入口 + 失败接力 + 流式转发。
//!
//! 只监听 127.0.0.1。控制面（增删厂商、读日志）不开放 HTTP，
//! 由宿主通过进程内调用拿到 —— 避免本机任意进程改配置。

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU16, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Local;
use freeport_catalog::{self as catalog, Catalog};
use freeport_core::cooldown::Cooldowns;
use freeport_core::dto::*;
use freeport_core::error::{body_error, classify, ErrorClass};
use freeport_core::model::*;
use freeport_core::quota;
use freeport_core::{affinity, presets, Affinity, Plan, Swrr};
use freeport_core::route::{cap_factor, plan_with, stick_to_front_if, Needs};
use freeport_auth::device::{self as device_flow, DeviceStart, DeviceStep};
use freeport_auth::copilot;
use freeport_auth::cookie::CookieJar;
use freeport_probe::patrol as patrol_calc;
use freeport_probe::{budget, health, probe};
use futures_util::stream::{BoxStream, StreamExt};
use serde_json::{json, Value};
use tokio::sync::{Notify, RwLock};
use tokio_stream::wrappers::ReceiverStream;
use tower_http::cors::CorsLayer;

use crate::anthropic;
use crate::cred::{self, Credential};
use crate::gemini;
use crate::hold::{self, HoldCfg, HoldOutcome};
use crate::keys;
use crate::license;
use crate::ollama;
use crate::providers;
use crate::responses;
use crate::sanitize::Redactor;
use crate::secrets;
use crate::store;
use crate::tokens::{Broker, Endpoints, OAuthBlob};
use crate::transfer;

const MAX_BODY: usize = 8 * 1024 * 1024;
const LOG_RING: usize = 300;
/// 巡检循环的节拍。真实探针按到期队列走，不需要跑得更勤。
const PATROL_TICK: Duration = Duration::from_secs(300);
/// 启动后先让用户用一会儿，别和第一个请求抢额度
const PATROL_WARMUP: Duration = Duration::from_secs(120);
/// 单轮巡检最多探这么多，避免长时间占着上游
const PATROL_BATCH: usize = 40;
/// feed 的检查节拍。真正是否发网络请求由 `checkMinutes` 决定，这里只是「到点看一眼」。
const FEED_TICK: Duration = Duration::from_secs(60);
/// 让目录先同步完，别和第一次 models.dev 拉取抢带宽
const FEED_WARMUP: Duration = Duration::from_secs(150);

pub type LogSink = Arc<dyn Fn(LogEntry) + Send + Sync>;

pub(crate) struct State_ {
    pub(crate) data: RwLock<AppData>,    catalog: RwLock<Catalog>,
    cooldowns: Cooldowns,
    /// SWRR 的累计权重。必须跨请求保留，否则「平滑」无从谈起。
    swrr: Mutex<Swrr>,
    /// 会话粘性：一段对话尽量回到上次答它的那家，保住 prompt cache。
    affinity: Mutex<Affinity>,
    logs: RwLock<Vec<LogEntry>>,
    client: reqwest::Client,
    started_at: RwLock<Option<Instant>>,
    bound_port: AtomicU16,
    running: AtomicBool,
    /// 暂停：只挡业务请求，不挡 /health 与 /v1/models。
    /// 内存态，不落盘——重启就是恢复，避免「上次暂停了这次忘了开」。
    paused: AtomicBool,
    /// 在途真实请求数：巡检看到非 0 就让路
    inflight: AtomicUsize,
    /// 上次落盘时间（毫秒）：模型健康每次请求都会变，不能每个请求都写盘
    last_save_ms: AtomicI64,
    on_log: RwLock<Option<LogSink>>,
    shutdown: Notify,
    /// OAuth 凭据的续期与并发去重
    broker: Broker,
    /// 进行中的设备码登录。用户点「登录」到浏览器里点完，中间隔了几十秒，
    /// 状态只能放这里。
    logins: Mutex<HashMap<String, PendingLogin>>,
}

struct PendingLogin {
    provider: String,
    start: DeviceStart,
    at: i64,
}

/// 发请求要用的凭据：OAuth 账号取 token（过期先续期），其余取 API key。
///
/// 放在锁外面调用——续期要发网络请求，拿着 `data` 读锁等网络会把整个网关卡住。
async fn credential(st: &Arc<State_>, account_id: &str) -> Option<Credential> {
    if secrets::has_oauth(account_id) {
        match st.broker.token(account_id).await {
            Ok(ts) => return Some(Credential::bearer(ts.access_token)),
            Err(e) => {
                // 续期失败不静默：这条凭据这轮就是不可用，要让日志说清楚
                tracing::warn!(account = %account_id, "OAuth 凭据不可用：{e}");
                return None;
            }
        }
    }
    // key 或 web 会话，由 Credential 决定发哪种头
    Credential::load(account_id)
}

/// 把「这条凭据是登录来的 / 是 web 会话」标出来，界面才不会把它当一把假 key 展示。
fn with_auth_meta(mut p: GwProvider) -> GwProvider {
    for a in p.accounts.iter_mut() {
        if let Some((kind, expires_at, _login)) = Broker::summary(&a.id) {
            a.auth = Some(kind);
            a.expires_at = expires_at;
        } else if secrets::has_cookie(&a.id) {
            // 会话没有「到期时间」这个概念（jar 里每条各不同），只标形态；
            // 是否还有效看 has_key——全过期时它会变 false
            a.auth = Some("cookie".into());
        }
    }
    p
}

pub struct Gateway {
    pub(crate) st: Arc<State_>,
}

impl Gateway {
    pub fn new() -> Self {
        Self::with_endpoints(Endpoints::default())
    }

    /// 换一套授权端点。只有测试会用——线上永远是官方地址。
    pub fn with_endpoints(endpoints: Endpoints) -> Self {
        let client = providers::client();
        Self {
            st: Arc::new(State_ {
                data: RwLock::new(AppData::default()),
                catalog: RwLock::new(Catalog::default()),
                cooldowns: Cooldowns::new(),
                swrr: Mutex::new(Swrr::new()),
                affinity: Mutex::new(Affinity::default()),
                logs: RwLock::new(Vec::new()),
                broker: Broker::new(client.clone(), endpoints),
                client,
                started_at: RwLock::new(None),
                bound_port: AtomicU16::new(0),
                running: AtomicBool::new(false),
                paused: AtomicBool::new(false),
                inflight: AtomicUsize::new(0),
                last_save_ms: AtomicI64::new(0),
                on_log: RwLock::new(None),
                shutdown: Notify::new(),
                logins: Mutex::new(HashMap::new()),
            }),
        }
    }

    pub async fn set_log_sink(&self, sink: LogSink) {
        *self.st.on_log.write().await = Some(sink);
    }

    /* ── 设备码登录（Tier C） ─────────────────────
     * 用户在自己的浏览器里登录自己的账号，我们只拿授权结果。
     * 不内嵌 client_secret，不伪造 UA，不读系统浏览器 Cookie。 */

    /// 发起一次登录：返回要展示给用户的设备码。
    pub async fn login_start(&self, provider_id: &str) -> Result<GwLoginStart, String> {
        let (exists, base_url) = {
            let d = self.st.data.read().await;
            match d.providers.iter().find(|p| p.id == provider_id) {
                Some(p) => (true, p.base_url.clone()),
                None => (false, String::new()),
            }
        };
        if !exists {
            return Err("请先添加这个厂商，再登录".into());
        }

        let cfg = self.device_cfg();
        let start = device_flow::start(&self.st.client, &cfg)
            .await
            .map_err(|e| e.to_string())?;

        let login_id = uuid::Uuid::new_v4().simple().to_string();
        {
            let mut map = self.st.logins.lock().unwrap_or_else(|e| e.into_inner());
            // 顺手清掉过期的，别让这张表随着时间长大
            let now = chrono::Utc::now().timestamp();
            map.retain(|_, v| now - v.at < v.start.expires_in as i64);
            map.insert(
                login_id.clone(),
                PendingLogin {
                    provider: provider_id.to_string(),
                    start: start.clone(),
                    at: now,
                },
            );
        }
        tracing::info!(provider = provider_id, base = %base_url, "已发起设备码登录");

        Ok(GwLoginStart {
            login_id,
            user_code: start.user_code.clone(),
            verification_uri: start.verification_uri.clone(),
            verification_uri_complete: start.verification_uri_complete.clone(),
            interval: start.interval,
            expires_in: start.expires_in,
            instruction: start.instruction(),
        })
    }

    /// 轮询一次登录状态。界面每 `interval` 秒调一次，直到不是 `pending`。
    pub async fn login_poll(&self, login_id: &str) -> GwLoginState {
        let pending = {
            let map = self.st.logins.lock().unwrap_or_else(|e| e.into_inner());
            map.get(login_id)
                .map(|p| (p.provider.clone(), p.start.clone(), p.at))
        };
        let Some((provider_id, start, at)) = pending else {
            return GwLoginState::error("这次登录已失效，请重新发起");
        };

        let now = chrono::Utc::now().timestamp();
        if now - at >= start.expires_in as i64 {
            self.forget_login(login_id);
            return GwLoginState {
                status: "expired".into(),
                message: Some("设备码已过期，请重新发起登录".into()),
                account_id: None,
                login: None,
            };
        }

        let cfg = self.device_cfg();
        let step = match device_flow::poll_once(&self.st.client, &cfg, &start.device_code, now).await {
            Ok(s) => s,
            Err(e) => return GwLoginState::error(e.to_string()),
        };

        match step {
            DeviceStep::Pending => GwLoginState::pending(),
            DeviceStep::SlowDown => GwLoginState {
                status: "pending".into(),
                message: Some("轮询过快，已自动放慢".into()),
                account_id: None,
                login: None,
            },
            DeviceStep::Denied => {
                self.forget_login(login_id);
                GwLoginState {
                    status: "denied".into(),
                    message: Some("你在浏览器里取消了授权".into()),
                    account_id: None,
                    login: None,
                }
            }
            DeviceStep::Expired => {
                self.forget_login(login_id);
                GwLoginState {
                    status: "expired".into(),
                    message: Some("设备码已过期，请重新发起登录".into()),
                    account_id: None,
                    login: None,
                }
            }
            DeviceStep::Ready(github) => {
                match self.finish_login(&provider_id, github).await {
                    Ok(state) => {
                        self.forget_login(login_id);
                        state
                    }
                    Err(e) => GwLoginState::error(e),
                }
            }
        }
    }

    /// 拿到 GitHub token 之后：换成 Copilot token、建凭据、顺手拉模型。
    async fn finish_login(
        &self,
        provider_id: &str,
        github: freeport_auth::TokenSet,
    ) -> Result<GwLoginState, String> {
        let now = chrono::Utc::now().timestamp();
        let login = copilot::whoami_at(
            &self.st.client,
            &self.st.broker.endpoints().github_user_url,
            &github.access_token,
        )
        .await;

        let mut tokens = copilot::exchange_at(
            &self.st.client,
            &self.st.broker.endpoints().copilot_token_url,
            &github.access_token,
            now,
        )
        .await
        .map_err(|e| e.to_string())?;
        tokens.account = login.clone();

        let acc = Account {
            id: format!("acct_{}", &uuid::Uuid::new_v4().simple().to_string()[..10]),
            name: login.clone().unwrap_or_else(|| "github".into()),
            key_mask: "gh-oauth".into(),
            enabled: true,
            added_at: chrono::Utc::now().to_rfc3339(),
            quota: None,
            health: None,
        };
        Broker::save(
            &acc.id,
            &OAuthBlob {
                kind: "copilot".into(),
                token_url: None,
                client_id: None,
                login: login.clone(),
                tokens: tokens.clone(),
            },
        )?;

        // 登录成功立刻拉一次模型列表：拉不到不影响「已登录」，但要留痕
        let provider_snapshot = {
            let d = self.st.data.read().await;
            d.providers.iter().find(|p| p.id == provider_id).cloned()
        };
        let mut models = Vec::new();
        if let Some(p) = provider_snapshot {
            let base = if p.base_url.trim().is_empty() {
                copilot::API_BASE.to_string()
            } else {
                p.base_url.clone()
            };
            let probe_provider = Provider {
                base_url: base.clone(),
                ..p.clone()
            };
            match providers::list_models(
                &self.st.client,
                &probe_provider,
                &Credential::bearer(tokens.access_token.clone()),
            )
            .await
            {
                Ok(m) => models = m,
                Err(e) => tracing::warn!("登录成功但拉模型失败：{}", e.body.chars().take(160).collect::<String>()),
            }

            let mut d = self.st.data.write().await;
            if let Some(p) = d.providers.iter_mut().find(|p| p.id == provider_id) {
                if p.base_url.trim().is_empty() {
                    p.base_url = base;
                }
                p.accounts.push(acc.clone());
                if !models.is_empty() {
                    p.models = models;
                }
            }
            store::save(&d);
        }

        tracing::info!(provider = provider_id, account = %acc.id, login = ?login, "OAuth 登录完成");
        Ok(GwLoginState {
            status: "ready".into(),
            message: None,
            account_id: Some(acc.id),
            login,
        })
    }

    fn forget_login(&self, login_id: &str) {
        let mut map = self.st.logins.lock().unwrap_or_else(|e| e.into_inner());
        map.remove(login_id);
    }

    /// 目前只有 Copilot 走设备码；将来 OpenCode Zen 之类换 client_id 即可。
    fn device_cfg(&self) -> device_flow::DeviceCfg {
        let ep = self.st.broker.endpoints();
        device_flow::DeviceCfg {
            device_code_url: ep.device_code_url.clone(),
            token_url: ep.github_token_url.clone(),
            client_id: copilot::CLIENT_ID.to_string(),
            scope: "read:user".to_string(),
        }
    }

    /// 取一条凭据真正发请求要用的东西：API key / OAuth token / web 会话。
    pub async fn credential(&self, account_id: &str) -> Option<Credential> {
        credential(&self.st, account_id).await
    }

    /// 载入磁盘配置与目录。宿主启动时调一次。
    pub async fn load(&self) {
        let mut data = store::load();
        let cat = catalog::load();

        // 目录摘要回写到配置，UI 才能显示「上次同步」
        data.catalog = cat.meta.clone();

        // 预算按天滚动（跨天自动清零）
        let today = Local::now().format("%Y-%m-%d").to_string();
        let budget_now = budget_of(&cat, &data);
        budget::roll(&mut data.patrol, &today, budget_now);

        // 老版本只有一个 authToken：迁成 name="default" 的一条，老客户端不用改配置
        if keys::migrate_legacy(&mut data) {
            store::save(&data);
            tracing::info!("已把全局 authToken 迁移为虚拟 key `default`");
        }

        // 「一厂商一 key」→「一厂商多凭据」。老凭据沿用 provider id，密钥不用搬家。
        if secrets::migrate_accounts(&mut data) {
            store::save(&data);
            tracing::info!("已把厂商密钥升级为凭据视图");
        }

        *self.st.data.write().await = data;
        *self.st.catalog.write().await = cat;
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
            .route("/v1/models/:id", get(get_model))
            .route("/v1/route", get(route_debug))
            .route("/v1/chat/completions", post(chat))
            .route("/v1/responses", post(responses_entry))
            .route("/v1/messages", post(messages))
            .route("/v1/messages/count_tokens", post(count_tokens))
            .route("/v1beta/models", get(gemini_models))
            .route("/v1beta/models/:tail", post(gemini))
            .route("/api/tags", get(ollama_tags))
            .route("/api/version", get(ollama_version))
            .route("/api/chat", post(ollama_chat))
            .route("/api/generate", post(ollama_generate))
            .layer(DefaultBodyLimit::max(MAX_BODY))
            .layer(CorsLayer::permissive())
            .layer(axum::middleware::from_fn_with_state(
                self.st.clone(),
                pause_guard,
            ))
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
        // 副作用都在 start 里：load 只读状态，测试可以只 load 不起服务
        self.spawn_catalog_refresh();
        self.spawn_patrol();
        self.spawn_feed();
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
            paused: self.st.paused.load(Ordering::SeqCst),
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
            request_budget_ms: d.gateway.request_budget_ms,
            tokens_in: d.stats.tokens_in,
            tokens_out: d.stats.tokens_out,
            errors: d.stats.errors,
            cooldowns: self.st.cooldowns.snapshot(),
            error: None,
        }
    }

    pub async fn providers(&self) -> Vec<GwProvider> {
        let d = self.st.data.read().await;
        let cat = self.st.catalog.read().await;
        d.providers
            .iter()
            .map(|p| {
                let base = GwProvider::from(p, &|id: &str| secrets::has_credential(id));
                let base = with_auth_meta(base);
                match cat.provider(&p.id) {
                    Some(c) => base.with_free_tier(c.free_tier, c.free_note.clone()),
                    None => base,
                }
            })
            .collect()
    }

    pub async fn models(&self) -> Vec<GwModel> {
        let d = self.st.data.read().await;
        aggregate_models(&d)
    }

    /// 每条凭据的四个额度窗口。额度按凭据算，所以一个厂商可能出多行。
    /// 用户流量与巡检分列，UI 才能解释「我只发了 3 次请求，额度怎么没了」。
    pub async fn quota(&self) -> Vec<GwQuota> {
        let d = self.st.data.read().await;
        let now = freeport_core::cooldown::now_ms();
        let mut rows = Vec::new();
        for p in d.providers.iter() {
            for a in p.accounts.iter() {
                rows.push(GwQuota::from(p, a, &d.ledger, now));
            }
        }
        rows
    }

    pub async fn keys(&self) -> Vec<GwKey> {
        let d = self.st.data.read().await;
        d.api_keys.iter().map(GwKey::from).collect()
    }

    /// 新建。明文只在返回值里出现这一次，落盘只留 hash。
    pub async fn create_key(&self, input: KeyInput) -> Result<KeyCreated, String> {
        let name = input.name.trim();
        if name.is_empty() {
            return Err("key 名称不能为空".into());
        }
        let secret = keys::generate();
        let k = keys::new_key(name, &secret, input.rpm_limit, input.daily_limit);
        let view = GwKey::from(&k);
        {
            let mut d = self.st.data.write().await;
            if d.api_keys.iter().any(|x| x.name == name) {
                return Err(format!("已存在名为 {name} 的 key"));
            }
            d.api_keys.push(k);
            store::save(&d);
        }
        Ok(KeyCreated { key: view, secret })
    }

    pub async fn delete_key(&self, id: &str) -> Vec<GwKey> {
        let mut d = self.st.data.write().await;
        d.api_keys.retain(|k| k.id != id);
        store::save(&d);
        d.api_keys.iter().map(GwKey::from).collect()
    }

    pub async fn toggle_key(&self, id: &str, enabled: bool) -> Result<Vec<GwKey>, String> {
        let mut d = self.st.data.write().await;
        let Some(k) = d.api_keys.iter_mut().find(|k| k.id == id) else {
            return Err(format!("找不到 key {id}"));
        };
        k.enabled = enabled;
        store::save(&d);
        Ok(d.api_keys.iter().map(GwKey::from).collect())
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

    /// 预演一条链路：不推进 SWRR、不写粘性表。
    /// GUI 的「路由」页每刷新一次都会调它，绝不能带副作用。
    pub async fn explain_route(&self, model: &str, session: &str) -> serde_json::Value {
        explain_route_json(&self.st, model, session, false, Needs::default()).await
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

        let cat_p = { self.st.catalog.read().await.provider(&pid).cloned() };
        let preset = presets::get(&pid);

        let base_url = if !input.base_url.trim().is_empty() {
            input.base_url.trim().trim_end_matches('/').to_string()
        } else {
            cat_p
                .as_ref()
                .and_then(|c| c.api.clone())
                .or_else(|| preset.as_ref().map(|p| p.base_url.clone()))
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| "缺少 baseUrl".to_string())?
        };

        let tier = input
            .tier
            .clone()
            .or_else(|| cat_p.as_ref().map(|c| c.tier.clone()))
            .or_else(|| preset.as_ref().map(|p| p.tier.clone()))
            .unwrap_or_else(|| "C".into());

        // 匿名端点（Tier B）本来就没有 key；订阅类（Tier C）先接进来再走登录，其余必须给
        let anonymous = input.api_key.trim().is_empty();
        if anonymous && tier != "B" && tier != "C" {
            return Err("缺少 API Key".into());
        }
        // 订阅类此时还没有凭据：不建空账号，等登录成功由 finish_login 建
        let login_only = anonymous && tier == "C";
        let key = if anonymous {
            "anonymous".to_string()
        } else {
            input.api_key.clone()
        };

        let key_mask = if anonymous {
            "无需 Key".to_string()
        } else {
            store::mask(&input.api_key)
        };
        let now_rfc = chrono::Utc::now().to_rfc3339();
        let account = Account {
            id: format!("acct_{}", &uuid::Uuid::new_v4().simple().to_string()[..10]),
            name: "default".into(),
            key_mask: key_mask.clone(),
            enabled: true,
            added_at: now_rfc.clone(),
            quota: None,
            health: None,
        };

        let mut provider = Provider {
            id: pid.clone(),
            name: input
                .name
                .clone()
                .or_else(|| cat_p.as_ref().map(|c| c.name.clone()))
                .or_else(|| preset.as_ref().map(|p| p.name.clone()))
                .unwrap_or_else(|| pid.clone()),
            base_url,
            tier: tier.clone(),
            homepage: cat_p
                .as_ref()
                .and_then(|c| c.docs.clone())
                .or_else(|| preset.as_ref().and_then(|p| p.homepage.clone())),
            note: cat_p
                .as_ref()
                .and_then(|c| c.free_note.clone())
                .or_else(|| preset.as_ref().and_then(|p| p.note.clone())),
            key_mask: key_mask.clone(),
            enabled: true,
            added_at: now_rfc,
            models: Vec::new(),
            // 限额默认不填：免费额度的公开数字经常变，宁可只记账不误拦
            quota: None,
            accounts: if login_only { Vec::new() } else { vec![account.clone()] },
            health: Health {
                status: HealthStatus::Probing,
                checked_at: Some(chrono::Utc::now().to_rfc3339()),
                ..Default::default()
            },
        };

        if login_only {
            // 订阅类靠登录拿 token，接入时既没有 key 也没有模型：用目录兜底，
            // 真正的模型列表在 login 成功时按 token 拉一次。
            provider.models = self.st.catalog.read().await.models_of(&pid);
        }
        if !login_only {
            // 用真 key 拉一次模型；拉不到就不入库（匿名端点允许用目录兜底）
            let t0 = Instant::now();
            match providers::list_models(&self.st.client, &provider, &Credential::bearer(key.clone())).await {
                Ok(mut models) => {
                    // 目录补全：上下文长度、免费判定、能力声明。上游没给的才填。
                    self.st.catalog.read().await.enrich(&pid, &mut models);
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
                    let detail = format!(
                        "{} {}",
                        if e.status > 0 { e.status.to_string() } else { String::new() },
                        e.body
                    )
                    .trim()
                    .chars()
                    .take(200)
                    .collect::<String>();

                    // 匿名端点经常不实现 /models，用目录里的内置列表兜底；
                    // 状态标 degraded，第一次真实调用成功就会自动转 healthy。
                    let fallback = if tier == "B" {
                        self.st.catalog.read().await.models_of(&pid)
                    } else {
                        Vec::new()
                    };
                    if fallback.is_empty() {
                        return Err(format!("校验失败：{}", detail));
                    }
                    provider.health = Health {
                        status: HealthStatus::Degraded,
                        latency_ms: Some(t0.elapsed().as_millis() as u64),
                        checked_at: Some(chrono::Utc::now().to_rfc3339()),
                        model_count: Some(fallback.len()),
                        error: Some(format!("/models 不可用（{}），已用目录内置模型", detail)),
                    };
                    provider.models = fallback;
                }
            }

            secrets::set_key(&account.id, &key)?;
        }

        let model_count = provider.models.len();
        let chain_entries: Vec<String> = provider
            .models
            .iter()
            .take(2)
            .map(|m| format!("{pid}:{}", m.id))
            .collect();

        let mut d = self.st.data.write().await;
        // 显式添加就是「我要它」，把墓碑清掉——否则下次合并又会被判成「已删除」
        d.tombstones.revive_provider(&pid);
        if let Some(existing) = d.providers.iter_mut().find(|p| p.id == pid) {
            // 已有厂商：保留原凭据，把这次输入的 key 追加成一条新凭据。
            // 同一个 key 重复录入不算新账号，只更新掩码与开关，避免越点越多。
            let mut accounts = std::mem::take(&mut existing.accounts);
            match accounts.iter_mut().find(|a| a.key_mask == account.key_mask) {
                Some(a) => {
                    a.enabled = true;
                    a.key_mask = account.key_mask.clone();
                }
                None => accounts.push(account.clone()),
            }
            provider.accounts = accounts;
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

        let free = cat_p.as_ref().map(|c| c.free_tier).unwrap_or(false);
        let free_note = cat_p.as_ref().and_then(|c| c.free_note.clone());
        let provider = saved
            .map(|p| {
                GwProvider::from(&p, &|id: &str| secrets::has_credential(id))
                    .with_free_tier(free, free_note)
            })
            .ok_or_else(|| "保存失败".to_string())?;

        Ok(AddProviderResult {
            provider,
            model_count,
        })
    }

    /* ── 凭据（同一厂商多账号） ───────────────────
       额度、冷却、账本、粘性全部按凭据算：同一厂商的两个 key 在上游是两本独立的账。 */

    /// 给已有厂商再加一条 key。先用真 key 打一次 /models 验证，再落库。
    pub async fn add_account(&self, input: AccountInput) -> Result<Vec<GwProvider>, String> {
        let key = input.api_key.trim().to_string();
        if key.is_empty() {
            return Err("缺少 API Key".into());
        }
        let (provider, index) = {
            let d = self.st.data.read().await;
            let idx = d
                .providers
                .iter()
                .position(|p| p.id == input.provider_id)
                .ok_or_else(|| "provider 不存在".to_string())?;
            (d.providers[idx].clone(), idx)
        };
        let _ = index;

        // 重复录入同一个 key 不算新账号：直接返回现状，免得越点越多
        let mask = store::mask(&key);
        if provider.accounts.iter().any(|a| a.key_mask == mask) {
            return Ok(self.providers().await);
        }

        // 验证：能拉到模型才算这条凭据可用
        let t0 = Instant::now();
        let models = providers::list_models(&self.st.client, &provider, &Credential::bearer(key.clone())).await;
        let latency = t0.elapsed().as_millis() as u64;
        let (models, degraded) = match models {
            Ok(m) => (m, None),
            Err(e) => {
                // Tier B 匿名端点没有 /models；有 key 的厂商则必须验证通过
                if provider.tier == "B" {
                    (Vec::new(), Some(e.body.chars().take(200).collect::<String>()))
                } else {
                    return Err(format!(
                        "校验失败：{} {}",
                        if e.status > 0 { e.status.to_string() } else { String::new() },
                        e.body.chars().take(200).collect::<String>()
                    ));
                }
            }
        };

        let seq = provider.accounts.len() + 1;
        let acc = Account {
            id: format!("acct_{}", &uuid::Uuid::new_v4().simple().to_string()[..10]),
            name: input
                .name
                .map(|n| n.trim().to_string())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| format!("key-{seq}")),
            key_mask: mask,
            enabled: true,
            added_at: chrono::Utc::now().to_rfc3339(),
            quota: input.quota,
            // 刚验过就记一笔：新行不该显示「待探测」，用户一分钟前才看着它通过
            health: degraded.is_none().then(|| Health {
                status: HealthStatus::Healthy,
                latency_ms: Some(latency),
                checked_at: Some(chrono::Utc::now().to_rfc3339()),
                model_count: Some(models.len()),
                error: None,
            }),
        };
        secrets::set_key(&acc.id, &key)?;

        {
            let mut d = self.st.data.write().await;
            let Some(p) = d.providers.iter_mut().find(|p| p.id == provider.id) else {
                return Err("provider 已被删除".into());
            };
            p.accounts.push(acc.clone());
            if p.models.is_empty() && !models.is_empty() {
                p.models = models;
            }
            if let Some(err) = degraded {
                p.health.error = Some(format!("/models 不可用：{err}"));
            }
            store::save(&d);
        }
        tracing::info!(provider = %provider.id, account = %acc.id, "已追加凭据");
        Ok(self.providers().await)
    }

    /// 用粘贴进来的 web 会话新建一条凭据。
    ///
    /// 只存用户主动交出来的那几条 cookie：不读系统浏览器库、不伪造 UA、
    /// 不模拟登录。会话失效也不自动续——那是在绕人家的风控，只提示重新粘贴。
    pub async fn add_cookie_account(
        &self,
        provider_id: &str,
        name: Option<String>,
        raw: &str,
    ) -> Result<Vec<GwProvider>, String> {
        let provider = {
            let d = self.st.data.read().await;
            d.providers
                .iter()
                .find(|p| p.id == provider_id)
                .cloned()
                .ok_or_else(|| "provider 不存在".to_string())?
        };
        let host = cred::host_of(&provider.base_url)
            .ok_or_else(|| "这个厂商还没填基础地址，先填好再导入会话".to_string())?;

        let imported = CookieJar::parse_input(raw, &host);
        if imported.jar.is_empty() {
            let why = imported
                .skipped
                .first()
                .cloned()
                .unwrap_or_else(|| "没识别出任何 cookie".into());
            return Err(format!("导入失败：{why}"));
        }

        let id = format!("acct_{}", &uuid::Uuid::new_v4().simple().to_string()[..10]);
        let body = serde_json::to_string(&imported.jar).map_err(|e| e.to_string())?;
        secrets::set_cookie(&id, &body)?;

        // 用这条会话验一次：拿得到模型才算可用
        let cred_obj = Credential::Cookie {
            account_id: id.clone(),
            jar: imported.jar.clone(),
        };
        let t0 = Instant::now();
        let probed = providers::list_models(&self.st.client, &provider, &cred_obj).await;
        let latency = t0.elapsed().as_millis() as u64;

        let (models, degraded) = match probed {
            Ok(m) => (m, None),
            // 站点没实现 /models 不代表会话没用，别因为这个把人挡在门外
            Err(e) if matches!(e.status, 404 | 405) => (Vec::new(), Some("这个厂商没有 /models".to_string())),
            Err(e) => {
                secrets::delete_cookie(&id);
                return Err(format!(
                    "会话校验失败（{}）：{}",
                    if e.status > 0 { e.status.to_string() } else { "网络错误".into() },
                    e.body.chars().take(200).collect::<String>()
                ));
            }
        };

        let seq = provider.accounts.len() + 1;
        let acc = Account {
            id: id.clone(),
            name: name
                .map(|n| n.trim().to_string())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| format!("session-{seq}")),
            key_mask: "web-session".into(),
            enabled: true,
            added_at: chrono::Utc::now().to_rfc3339(),
            quota: None,
            health: degraded.is_none().then(|| Health {
                status: HealthStatus::Healthy,
                latency_ms: Some(latency),
                checked_at: Some(chrono::Utc::now().to_rfc3339()),
                model_count: Some(models.len()),
                error: None,
            }),
        };

        {
            let mut d = self.st.data.write().await;
            let Some(p) = d.providers.iter_mut().find(|p| p.id == provider_id) else {
                secrets::delete_cookie(&id);
                return Err("provider 已被删除".into());
            };
            p.accounts.push(acc.clone());
            if p.models.is_empty() && !models.is_empty() {
                p.models = models;
            }
            store::save(&d);
        }

        tracing::info!(
            provider = provider_id,
            account = %acc.id,
            cookies = imported.jar.len(),
            "已导入 web 会话凭据"
        );
        Ok(self.providers().await)
    }

    /// 给已有凭据换一份 web 会话（会话过期后重新粘贴走这里）。
    pub async fn set_account_cookie(
        &self,
        provider_id: &str,
        account_id: &str,
        raw: &str,
    ) -> Result<Vec<GwProvider>, String> {
        let base = {
            let d = self.st.data.read().await;
            let p = d
                .providers
                .iter()
                .find(|p| p.id == provider_id)
                .ok_or_else(|| "provider 不存在".to_string())?;
            if p.account(account_id).is_none() {
                return Err("这条凭据不存在".into());
            }
            p.base_url.clone()
        };
        let host = cred::host_of(&base).ok_or_else(|| "这个厂商还没填基础地址".to_string())?;
        let n = cred::save_cookie_jar(account_id, raw, &host)?;
        // 换了会话就把旧的失效记录清掉，否则界面还挂着一个红叉。
        // 写锁必须困在自己的块里：`providers()` 要拿读锁，同任务持写锁再读
        // 就是自锁，整个网关会永久卡住（不是超时，是等一个永远得不到的锁）。
        {
            let mut d = self.st.data.write().await;
            if let Some(p) = d.providers.iter_mut().find(|p| p.id == provider_id) {
                if let Some(a) = p.account_mut(account_id) {
                    a.health = None;
                    a.key_mask = "web-session".into();
                }
            }
            store::save(&d);
        }
        tracing::info!(account = %account_id, cookies = n, "已更新 web 会话");
        Ok(self.providers().await)
    }

    /// 删掉一条凭据：密文、账本窗口、key 级冷却一起清。
    pub async fn remove_account(&self, provider_id: &str, account_id: &str) -> Vec<GwProvider> {
        {
            let mut d = self.st.data.write().await;
            if let Some(p) = d.providers.iter_mut().find(|p| p.id == provider_id) {
                p.accounts.retain(|a| a.id != account_id);
            }
            // 凭据也要墓碑：跨机合并是按 id 补缺的，没有墓碑就会把删掉的 key 加回来
            d.tombstones.bury_account(account_id);
            d.ledger.forget(account_id);
            store::save(&d);
        }
        secrets::delete_credential(account_id);
        self.st.cooldowns.forget_account(account_id);
        self.providers().await
    }

    /// 改名字 / 开关 / 限额。`clearQuota` 用来把限额清空（JSON 表达不了 `Some(None)`）。
    pub async fn patch_account(
        &self,
        provider_id: &str,
        account_id: &str,
        patch: AccountPatch,
    ) -> Result<Vec<GwProvider>, String> {
        {
            let mut d = self.st.data.write().await;
            let p = d
                .providers
                .iter_mut()
                .find(|p| p.id == provider_id)
                .ok_or_else(|| "provider 不存在".to_string())?;
            let a = p
                .account_mut(account_id)
                .ok_or_else(|| "凭据不存在".to_string())?;
            if let Some(n) = patch.name {
                let n = n.trim().to_string();
                if !n.is_empty() {
                    a.name = n;
                }
            }
            if let Some(v) = patch.enabled {
                a.enabled = v;
            }
            if patch.clear_quota {
                a.quota = None;
            } else if let Some(q) = patch.quota {
                a.quota = Some(q);
            }
            store::save(&d);
        }
        Ok(self.providers().await)
    }

    /* ── 迁移（多机同步 v1） ─────────────────────────
       迁移码 = 口令派生的 AES-256-GCM 信封，里面是厂商/凭据/虚拟 key/选路配置。
       不含用量、冷却、模型目录——那些是「这台机器发生过什么」，搬过去只会骗人。 */

    /// 导出迁移码。返回加密信封原文，调用方负责交给用户（复制/存文件）。
    pub async fn export_bundle(&self, passphrase: &str) -> Result<String, String> {
        // Argon2 是 CPU-bound 的，几百毫秒到一秒；扔到阻塞池里算，
        // 别让它把网关的 async runtime 卡住——导出时可能正在转发对话。
        let data = { self.st.data.read().await.clone() };
        let pass = passphrase.to_string();
        let source = format!(
            "Freeport {} · {}",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS
        );
        tokio::task::spawn_blocking(move || transfer::export(&data, &pass, &source))
            .await
            .map_err(|e| format!("导出任务失败：{e}"))?
    }

    /// 导出到 `.fpk`：内容与迁移码一致，只是落成文件。返回实际写入的路径。
    ///
    /// 先算再写：加密失败的报错（口令太短）不该先弹一个「已保存」再反悔。
    pub async fn export_bundle_to(
        &self,
        path: std::path::PathBuf,
        passphrase: &str,
    ) -> Result<String, String> {
        let blob = self.export_bundle(passphrase).await?;
        let saved = tokio::task::spawn_blocking(move || {
            transfer::write_file(&path, &blob).map(|_| path)
        })
        .await
        .map_err(|e| format!("保存任务失败：{e}"))??;
        Ok(saved.display().to_string())
    }

    /// 从 `.fpk` 导入：读文件这一步也扔进阻塞池（可能是网络盘）。
    pub async fn import_bundle_from(
        &self,
        path: std::path::PathBuf,
        passphrase: &str,
        mode: &str,
    ) -> Result<transfer::Summary, String> {
        let blob = tokio::task::spawn_blocking(move || transfer::read_file(&path))
            .await
            .map_err(|e| format!("读取任务失败：{e}"))??;
        self.import_bundle(&blob, passphrase, mode).await
    }

    /// 导入迁移码：先解密校验（这一步不改任何状态），再落数据与密文。
    pub async fn import_bundle(
        &self,
        blob: &str,
        passphrase: &str,
        mode: &str,
    ) -> Result<transfer::Summary, String> {
        let mode = transfer::Mode::parse(mode)?;
        let body = {
            let blob = blob.to_string();
            let pass = passphrase.to_string();
            tokio::task::spawn_blocking(move || transfer::open(&blob, &pass))
                .await
                .map_err(|e| format!("解密任务失败：{e}"))??
        };
        let mut d = self.st.data.write().await;
        let sum = transfer::apply(body, &mut d, mode);
        store::save(&d);
        tracing::info!(
            target: "transfer",
            mode = sum.mode.as_str(),
            providers = sum.providers_added,
            accounts = sum.accounts_added,
            keys = sum.keys_added,
            "已导入迁移码"
        );
        Ok(sum)
    }

    pub async fn remove_provider(&self, id: &str) -> Vec<GwProvider> {
        let account_ids: Vec<String> = {
            let mut d = self.st.data.write().await;
            let ids: Vec<String> = d
                .providers
                .iter()
                .find(|p| p.id == id)
                .map(|p| p.accounts.iter().map(|a| a.id.clone()).collect())
                .unwrap_or_default();
            d.providers.retain(|p| p.id != id);
            // 墓碑：记下「是用户删的」。不带这个标记，一次目录回填或跨机合并
            // 就会把它复活，用户会以为删除没生效
            d.tombstones.bury_provider(id);
            d.routing.chain.retain(|c| !c.starts_with(&format!("{id}:")));
            let prefix = format!("{id}:");
            d.model_health.retain(|k, _| !k.starts_with(&prefix));
            store::save(&d);
            ids
        };
        for a in account_ids {
            secrets::delete_credential(&a);
        }
        secrets::delete_credential(id);
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

    /// 校验厂商：挑第一条「有凭据」的打一次 `/models`。
    ///
    /// 这是「整家体检」的语义。想单独验证某一把 key，走
    /// [`probe_account`](Self::probe_account)——多账号之后这两件事不一样了。
    /// 这里**不看 `enabled`**：手动体检不是路由，停用的凭据也该能验一次，
    /// 否则「全部停用」会被报成「没有 key」，让人以为密钥丢了。
    pub async fn probe(&self, id: &str) -> ProbeResult {
        let account_id = {
            let d = self.st.data.read().await;
            d.providers
                .iter()
                .find(|p| p.id == id)
                .and_then(|p| {
                    p.accounts
                        .iter()
                        .find(|a| secrets::has_credential(&a.id))
                        .map(|a| a.id.clone())
                })
        };
        let Some(account_id) = account_id else {
            return ProbeResult {
                ok: false,
                models: None,
                latency_ms: None,
                error: Some("本机密钥库里没有这个 provider 的 key，请重新输入".into()),
            };
        };
        self.probe_account(id, &account_id).await
    }

    /// 用**指定的一条凭据**校验厂商端点。
    ///
    /// 结果写进 `Account::health`，然后重新汇总厂商级健康：只要还有一条
    /// key 是好的，这家就是好的。之前的实现只写厂商级字段，于是「校验第二
    /// 把 key 失败」会把整家刷红——在多账号下这是直接误导。
    pub async fn probe_account(&self, provider_id: &str, account_id: &str) -> ProbeResult {
        let provider = {
            let d = self.st.data.read().await;
            d.providers.iter().find(|p| p.id == provider_id).cloned()
        };
        let Some(mut p) = provider else {
            return ProbeResult {
                ok: false,
                models: None,
                latency_ms: None,
                error: Some("provider 不存在".into()),
            };
        };
        if p.account(account_id).is_none() {
            return ProbeResult {
                ok: false,
                models: None,
                latency_ms: None,
                error: Some("凭据不存在，可能已被删除".into()),
            };
        }
        let Some(cred) = self.credential(account_id).await else {
            return ProbeResult {
                ok: false,
                models: None,
                latency_ms: None,
                error: Some("这条凭据取不到可用凭据，请重新输入或重新登录".into()),
            };
        };

        let t0 = Instant::now();
        let result = providers::list_models(&self.st.client, &p, &cred).await;
        let latency = t0.elapsed().as_millis() as u64;
        let checked_at = chrono::Utc::now().to_rfc3339();

        let out = match result {
            Ok(models) => {
                if let Some(a) = p.account_mut(account_id) {
                    a.health = Some(Health {
                        status: HealthStatus::Healthy,
                        latency_ms: Some(latency),
                        checked_at: Some(checked_at),
                        model_count: Some(models.len()),
                        error: None,
                    });
                }
                let mut enriched = models.clone();
                self.st.catalog.read().await.enrich(provider_id, &mut enriched);
                p.models = enriched;
                ProbeResult {
                    ok: true,
                    models: Some(models.len()),
                    latency_ms: Some(latency),
                    error: None,
                }
            }
            Err(e) => {
                let detail: String = e.body.chars().take(240).collect();
                if let Some(a) = p.account_mut(account_id) {
                    a.health = Some(Health {
                        status: HealthStatus::Error,
                        latency_ms: Some(latency),
                        checked_at: Some(checked_at),
                        model_count: None,
                        error: Some(detail.clone()),
                    });
                }
                ProbeResult {
                    ok: false,
                    models: None,
                    latency_ms: Some(latency),
                    error: Some(detail),
                }
            }
        };

        p.sync_health();
        let mut d = self.st.data.write().await;
        if let Some(slot) = d.providers.iter_mut().find(|x| x.id == provider_id) {
            *slot = p;
        }
        store::save(&d);
        out
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
            // 预算给个下限：写进 0 会让每一跳都立刻超时，等于把网关变成 504 发生器
            if let Some(ms) = patch.request_budget_ms {
                if (1_000..=600_000).contains(&ms) {
                    d.gateway.request_budget_ms = ms;
                    // hold 的总时长跟着预算走，否则「45s 墙钟」与「hold 总时长」会打架
                    d.gateway.hold_total_ms = ms;
                }
            }
            store::save(&d);
        }
        self.status().await
    }

    pub fn clear_cooldowns(&self) {
        self.st.cooldowns.clear();
    }

    /// 暂停 / 恢复业务转发。返回设置后的状态。
    pub fn set_paused(&self, v: bool) -> bool {
        self.st.paused.store(v, Ordering::SeqCst);
        v
    }

    /* ── 目录 ─────────────────────────────────────── */

    pub async fn catalog(&self) -> GwCatalog {
        let cat = self.st.catalog.read().await;
        let d = self.st.data.read().await;
        GwCatalog {
            meta: cat.meta.clone(),
            providers: cat
                .providers
                .iter()
                .map(|p| GwCatalogProvider {
                    id: p.id.clone(),
                    name: p.name.clone(),
                    api: p.api.clone(),
                    docs: p.docs.clone(),
                    tier: p.tier.clone(),
                    free_tier: p.free_tier,
                    free_note: p.free_note.clone(),
                    quota: p.quota.map(|q| CatalogQuota { rpm: q.rpm, rpd: q.rpd }),
                    models: p.models.len(),
                    free_models: p.models.iter().filter(|m| m.free).count(),
                    added: d.providers.iter().any(|x| x.id == p.id),
                    source: p.source.clone(),
                })
                .collect(),
        }
    }

    pub async fn refresh_catalog(&self) -> Result<GwCatalog, String> {
        match catalog::refresh(&self.st.client).await {
            Ok(cat) => {
                *self.st.catalog.write().await = cat;
                self.sync_catalog_meta(None).await;
                self.re_enrich().await;
                Ok(self.catalog().await)
            }
            Err(e) => {
                {
                    let mut d = self.st.data.write().await;
                    d.catalog.error = Some(e.clone());
                    store::save(&d);
                }
                Err(e)
            }
        }
    }

    async fn sync_catalog_meta(&self, error: Option<String>) {
        let meta = { self.st.catalog.read().await.meta.clone() };
        let mut d = self.st.data.write().await;
        d.catalog = meta;
        d.catalog.error = error;
        store::save(&d);
    }

    /// 目录更新后，把已有厂商的模型重新补一遍元数据。
    async fn re_enrich(&self) {
        let cat = self.st.catalog.read().await;
        let mut d = self.st.data.write().await;
        for p in d.providers.iter_mut() {
            cat.enrich(&p.id, &mut p.models);
        }
        store::save(&d);
    }

    fn spawn_catalog_refresh(&self) {
        let gw = Gateway { st: self.st.clone() };
        tokio::spawn(async move {
            let stale = {
                let c = gw.st.catalog.read().await;
                catalog::needs_refresh(&c, 24)
            };
            if !stale {
                return;
            }
            match catalog::refresh(&gw.st.client).await {
                Ok(cat) => {
                    *gw.st.catalog.write().await = cat;
                    gw.sync_catalog_meta(None).await;
                    gw.re_enrich().await;
                }
                Err(e) => {
                    tracing::warn!("catalog refresh failed: {e}");
                    gw.sync_catalog_meta(Some(e)).await;
                }
            }
        });
    }

    /* ── 签名目录 feed ─────────────────────────────── */

    pub async fn feed(&self) -> GwFeed {
        let d = self.st.data.read().await;
        let f = &d.feed;
        GwFeed {
            enabled: f.enabled,
            url: f.url.clone(),
            channel: f.channel.clone(),
            policy: f.policy.clone(),
            configured: f.configured(),
            has_pubkey: f
                .pubkey
                .as_deref()
                .map(|s| !s.trim().is_empty())
                .unwrap_or(false)
                || !catalog::feed::BUILTIN_PUBKEY.is_empty(),
            check_minutes: f.check_minutes,
            seq: f.seq,
            version: f.version.clone(),
            served_tier: f.served_tier.clone(),
            last_check_at: f.last_check_at.clone(),
            last_ok_at: f.last_ok_at.clone(),
            last_error: f.last_error.clone(),
            halted: f.halted.clone(),
            pending: f.pending.clone(),
            events: f.events.clone(),
            applied_at: f.applied_at.clone(),
            rejected: f.rejected,
            tombstones: f.tombstones.iter().cloned().collect(),
        }
    }

    pub async fn set_feed(&self, patch: FeedPatch) -> GwFeed {
        {
            let mut d = self.st.data.write().await;
            let f = &mut d.feed;
            if let Some(v) = patch.enabled {
                f.enabled = v;
            }
            if let Some(v) = &patch.url {
                f.url = v.trim().to_string();
                // 换了地址，上一个 feed 说的档位就不再适用于当前来源
                f.served_tier = None;
            }
            if let Some(v) = patch.channel {
                if v == catalog::feed::CHANNEL_STABLE || v == catalog::feed::CHANNEL_BETA {
                    f.channel = v;
                }
            }
            if let Some(v) = patch.policy {
                if [catalog::feed::POLICY_AUTO, catalog::feed::POLICY_NOTIFY, catalog::feed::POLICY_PINNED]
                    .contains(&v.as_str())
                {
                    f.policy = v;
                }
            }
            if let Some(v) = patch.pubkey {
                f.pubkey = if v.trim().is_empty() { None } else { Some(v.trim().to_string()) };
            }
            if let Some(v) = patch.check_minutes {
                f.check_minutes = v.clamp(1, 1440);
            }
            // 手动改了地址/开关，就把上一次的错误与待确认状态清掉：
            // 那是旧配置的结果，留着重置会让人以为新配置也失败了。
            if patch.enabled.is_some() || patch.url.is_some() {
                f.last_error = None;
                f.halted = None;
                f.pending = None;
            }
            store::save(&d);
        }
        self.feed().await
    }

    /// 拉一次 feed。`manual` 忽略轮询间隔；`force_apply` 跳过安全阀（用户已确认）。
    pub async fn feed_check(&self, manual: bool) -> GwFeed {
        self.feed_poll(manual, false).await
    }

    /// 用户看过差量后点「仍然应用」。
    pub async fn feed_apply(&self) -> GwFeed {
        self.feed_poll(true, true).await
    }

    async fn feed_poll(&self, manual: bool, force_apply: bool) -> GwFeed {
        let (url, channel, policy, pubkey, since, tier, token) = {
            let d = self.st.data.read().await;
            let f = &d.feed;
            if !f.accepts_feed() {
                return self.feed().await;
            }
            if !manual && !f.due(chrono::Utc::now().timestamp_millis()) {
                return self.feed().await;
            }
            // 身份取当前许可证现算：宽限期仍按 Pro 报，过期立刻退回 free。
            // 这里只决定「请求带什么」，不决定「给不给拉」。
            let id = license::identity(&d);
            (
                f.url.clone(),
                f.channel.clone(),
                if force_apply {
                    catalog::feed::POLICY_AUTO.to_string()
                } else {
                    f.policy.clone()
                },
                f.pubkey.clone().unwrap_or_default(),
                f.seq,
                id.tier.to_string(),
                id.token,
            )
        };

        // 目录是 557KB，克隆一次比「拿着读锁等网络」划算得多
        let current = { self.st.catalog.read().await.clone() };
        // 增量补丁的基线：上一份原样快照。没有它就只能下全量。
        let base = catalog::store::load_snapshot();
        let now = Local::now().to_rfc3339();

        let cfg = catalog::feed::PollCfg {
            url: &url,
            channel: &channel,
            policy: &policy,
            pubkey: &pubkey,
            since,
            force: force_apply,
            tier: &tier,
            token: token.as_deref(),
            base: base.as_ref(),
        };

        match catalog::feed::poll(&self.st.client, &cfg, &current).await {
            Ok(catalog::feed::Poll::Unchanged) => {
                let mut d = self.st.data.write().await;
                d.feed.last_check_at = Some(now.clone());
                d.feed.last_ok_at = Some(now);
                d.feed.last_error = None;
                store::save(&d);
            }
            Ok(catalog::feed::Poll::Pending(m)) => {
                let mut d = self.st.data.write().await;
                d.feed.last_check_at = Some(now.clone());
                d.feed.last_ok_at = Some(now.clone());
                d.feed.last_error = None;
                d.feed.halted = None;
                d.feed.pending = Some(FeedPending {
                    seq: m.seq,
                    version: m.version.clone(),
                    published_at: Some(m.published_at).filter(|s| !s.is_empty()),
                    at: Some(now),
                });
                store::save(&d);
            }
            Ok(catalog::feed::Poll::Applied(a)) => {
                let a = *a;
                // 先把新模型的元数据从新目录里取出来，避免同时持有两把锁
                let fresh: Vec<(String, ModelInfo)> = a
                    .added_keys
                    .iter()
                    .filter_map(|k| {
                        let (pid, mid) = k.split_once(':')?;
                        let m = a.catalog.find_model(pid, mid)?;
                        Some((
                            pid.to_string(),
                            ModelInfo {
                                id: mid.to_string(),
                                owned_by: pid.to_string(),
                                context: m.context,
                                created: None,
                                free: Some(m.free),
                                caps: m.caps(),
                                candidate: true,
                            },
                        ))
                    })
                    .collect();

                {
                    let mut cat = self.st.catalog.write().await;
                    catalog::store::save_snapshot(&a.snapshot);
                    *cat = a.catalog;
                    catalog::store::save(&cat);
                }
                {
                    let mut d = self.st.data.write().await;
                    // feed 说存在、上游 /models 又没列的模型，插到最前面：
                    // 兜底链只取前两个，放末尾等于永远拿不到流量，canary 也就永远转不了正。
                    for (pid, mi) in fresh {
                        let Some(p) = d.providers.iter_mut().find(|p| p.id == pid) else {
                            continue;
                        };
                        if p.models.iter().any(|m| m.id == mi.id) {
                            continue;
                        }
                        p.models.insert(0, mi);
                    }
                    let f = &mut d.feed;
                    f.seq = a.seq;
                    f.version = a.version.clone();
                    // 记下服务端说这份目录属于哪个档：界面据此显示「Pro 目录」，
                    // 但收不收、用不用，都只看签名——不看本机档位。
                    f.served_tier = a.tier.clone();
                    f.last_check_at = Some(now.clone());
                    f.last_ok_at = Some(now.clone());
                    f.last_error = None;
                    f.halted = None;
                    f.pending = None;
                    f.applied_at = Some(now);
                    // 墓碑累积；快照里明确还活着的模型撤掉本地墓碑
                    f.tombstones.extend(a.tombstones.iter().cloned());
                    for k in &a.active {
                        f.tombstones.remove(k);
                    }
                    f.push_events(a.events.clone());
                    store::save(&d);
                }
                self.sync_catalog_meta(None).await;
                self.re_enrich().await;
                tracing::info!(
                    seq = a.seq,
                    added = a.added,
                    deleted = a.deleted,
                    "catalog feed applied"
                );
            }
            Err(e) => {
                let mut d = self.st.data.write().await;
                d.feed.last_check_at = Some(now);
                d.feed.rejected += 1;
                if e.halted {
                    d.feed.halted = Some(e.message.clone());
                } else {
                    d.feed.last_error = Some(e.message.clone());
                }
                store::save(&d);
                tracing::warn!(halted = e.halted, "catalog feed rejected: {}", e.message);
            }
        }
        self.feed().await
    }

    fn spawn_feed(&self) {
        let gw = Gateway { st: self.st.clone() };
        tokio::spawn(async move {
            tokio::time::sleep(FEED_WARMUP).await;
            loop {
                if !gw.st.running.load(Ordering::SeqCst) {
                    break;
                }
                let _ = gw.feed_poll(false, false).await;
                tokio::time::sleep(FEED_TICK).await;
            }
        });
    }

    /* ── 探针 ─────────────────────────────────────── */

    /// 单模型探针。`deep` 时顺带跑一次能力探测（贵 20 倍，只在手动触发时开）。
    pub async fn probe_model(&self, provider_id: &str, model: &str, deep: bool) -> GwModelProbe {
        let provider = {
            let d = self.st.data.read().await;
            d.providers.iter().find(|p| p.id == provider_id).cloned()
        };
        let Some(p) = provider else {
            return GwModelProbe {
                ok: false,
                status: 0,
                latency_ms: 0,
                error: Some("provider 不存在".into()),
                health: "unknown".into(),
                caps: None,
            };
        };
        let account_id = p
            .accounts
            .iter()
            .find(|a| secrets::has_credential(&a.id))
            .map(|a| a.id.clone());
        let Some(account_id) = account_id else {
            return GwModelProbe {
                ok: false,
                status: 0,
                latency_ms: 0,
                error: Some("缺少 API Key，请先补录".into()),
                health: "unknown".into(),
                caps: None,
            };
        };
        let Some(cred) = self.credential(&account_id).await else {
            return GwModelProbe {
                ok: false,
                status: 0,
                latency_ms: 0,
                error: Some("凭据读取失败，请重新输入或重新登录".into()),
                health: "unknown".into(),
                caps: None,
            };
        };
        // 探针只认「哪个头、什么值」：key 是 Bearer，web 会话是 Cookie
        let Some((hn, hv)) = cred.auth_pair(&p.base_url) else {
            return GwModelProbe {
                ok: false,
                status: 0,
                latency_ms: 0,
                error: Some("这条凭据没有可用的会话或 key".into()),
                health: "unknown".into(),
                caps: None,
            };
        };
        let auth = (hn, hv.as_str());

        let out = probe::liveness(&self.st.client, &p.base_url, model, auth).await;
        let measured = if deep && out.ok {
            Some(probe::capability(&self.st.client, &p.base_url, model, auth).await.caps)
        } else {
            None
        };

        let now = chrono::Utc::now().to_rfc3339();
        let mut d = self.st.data.write().await;
        let k = format!("{provider_id}:{model}");
        let h = d.model_health.entry(k).or_default();
        if out.ok {
            health::apply_ok(h, out.latency_ms, &now);
        } else if out.no_free_tier {
            health::apply_no_free_tier(h, out.error.clone().unwrap_or_default(), &now);
        } else {
            health::apply_fail(h, out.error.clone().unwrap_or_default(), &now);
        }
        if let Some(c) = measured {
            health::apply_caps(h, c);
        }
        let status = h.status.as_str().to_string();
        let caps = if h.caps.is_empty() { None } else { Some(h.caps) };
        store::save(&d);

        GwModelProbe {
            ok: out.ok,
            status: out.status,
            latency_ms: out.latency_ms,
            error: out.error,
            health: status,
            caps,
        }
    }

    /// 把已知异常的模型重探一遍（最多 20 个，避免一次点爆额度）。
    pub async fn probe_failed(&self) -> PatrolReport {
        let targets: Vec<(String, String)> = {
            let d = self.st.data.read().await;
            d.model_health
                .iter()
                .filter(|(_, h)| {
                    matches!(h.status, HealthStatus::Unavailable | HealthStatus::Degraded)
                })
                .filter_map(|(k, _)| {
                    k.split_once(':')
                        .map(|(p, m)| (p.to_string(), m.to_string()))
                })
                .take(20)
                .collect()
        };

        let mut report = PatrolReport::default();
        for (provider, model) in targets {
            let r = self.probe_model(&provider, &model, false).await;
            report.ran += 1;
            if r.ok {
                report.ok += 1;
            } else {
                report.failed += 1;
            }
        }
        report
    }

    /* ── 巡检 ─────────────────────────────────────── */

    pub async fn patrol(&self) -> GwPatrol {
        let d = self.st.data.read().await;
        let due_now = patrol_calc::due(
            &d,
            chrono::Utc::now().timestamp_millis(),
            usize::MAX,
            &|id| secrets::has_credential(id),
        )
        .len() as u32;
        GwPatrol {
            enabled: d.patrol.enabled,
            hot_hours: d.patrol.hot_hours,
            cold_hours: d.patrol.cold_hours,
            sweep_at: d.patrol.sweep_at.clone(),
            budget_today: d.patrol.budget_today,
            used_today: d.patrol.used_today,
            remaining: budget::remaining(&d.patrol),
            due_now,
            last_run_at: d.patrol.last_run_at.clone(),
            last_sweep_at: d.patrol.last_sweep_at.clone(),
            probes_total: d.patrol.probes_total,
            probes_ok: d.patrol.probes_ok,
            inflight: self.st.inflight.load(Ordering::SeqCst) as u32,
        }
    }

    pub async fn set_patrol(&self, patch: PatrolPatch) -> GwPatrol {
        {
            let cat = self.st.catalog.read().await;
            let mut d = self.st.data.write().await;
            if let Some(v) = patch.enabled {
                d.patrol.enabled = v;
            }
            if let Some(v) = patch.hot_hours {
                d.patrol.hot_hours = v.max(1);
            }
            if let Some(v) = patch.cold_hours {
                d.patrol.cold_hours = v.max(1);
            }
            if let Some(v) = patch.sweep_at {
                if patrol_calc::parse_hhmm(&v).is_some() {
                    d.patrol.sweep_at = v;
                }
            }
            d.patrol.budget_cap = patch.budget_cap;
            let today = Local::now().format("%Y-%m-%d").to_string();
            let b = budget_of(&cat, &d);
            budget::roll(&mut d.patrol, &today, b);
            store::save(&d);
        }
        self.patrol().await
    }

    /// 跑一轮巡检。`force_all` 用于每日全量体检。
    pub async fn patrol_pass(&self, force_all: bool) -> PatrolReport {
        let now = chrono::Utc::now();
        let now_rfc = now.to_rfc3339();
        let mut report = PatrolReport::default();

        let items: Vec<patrol_calc::DueItem> = {
            let cat = self.st.catalog.read().await;
            let mut d = self.st.data.write().await;

            if !d.patrol.enabled && !force_all {
                report.skipped = Some("巡检已关闭".into());
                return report;
            }
            if self.st.inflight.load(Ordering::SeqCst) > 0 && !force_all {
                report.skipped = Some("有真实请求在途，巡检让路".into());
                return report;
            }

            let today = Local::now().format("%Y-%m-%d").to_string();
            let b = budget_of(&cat, &d);
            budget::roll(&mut d.patrol, &today, b);
            if budget::remaining(&d.patrol) == 0 {
                report.skipped = Some("今日探针预算已用完".into());
                return report;
            }

            let has_key = |id: &str| secrets::has_credential(id);
            if force_all {
                patrol_calc::all(&d, &has_key)
            } else {
                let limit = (budget::remaining(&d.patrol) as usize).min(PATROL_BATCH);
                patrol_calc::due(&d, now.timestamp_millis(), limit, &has_key)
            }
        };

        if items.is_empty() {
            report.skipped = Some("没有到期的模型".into());
            return report;
        }

        for item in items {
            // 巡检挑第一条有 key 的凭据：模型健康是厂商级事实，不必每条 key 都探
            let picked = {
                let d = self.st.data.read().await;
                d.providers.iter().find(|p| p.id == item.provider).and_then(|p| {
                    let base = p.base_url.clone();
                    p.live_accounts()
                        .find(|a| secrets::has_credential(&a.id))
                        .map(|a| (base, a.id.clone(), a.quota))
                })
            };
            let Some((base, account_id, account_quota)) = picked else {
                continue;
            };
            let Some(cred) = self.credential(&account_id).await else {
                continue;
            };
            let Some((hn, hv)) = cred.auth_pair(&base) else { continue };
            let auth = (hn, hv.as_str());

            {
                let mut d = self.st.data.write().await;
                if !budget::try_spend(&mut d.patrol, 1) {
                    break;
                }
            }

            // 巡检与用户流量分列记账。RPD 耗尽时用户流量该停，但一条 3 token 的
            // liveness 仍然值得跑——混记进 used_user 就做不出这个区分。
            let limits = quota::limits_of(account_quota.as_ref());
            const PROBE_RESERVE: f64 = 64.0;
            {
                let mut d = self.st.data.write().await;
                d.ledger
                    .reserve_now(&account_id, limits, PROBE_RESERVE, true);
            }

            let out = probe::liveness(&self.st.client, &base, &item.model, auth).await;
            report.ran += 1;
            if out.ok {
                report.ok += 1;
            } else {
                report.failed += 1;
            }

            let mut d = self.st.data.write().await;
            // 探针报了 token 就按实际结；没连上全退，其余认预扣
            let actual = match out.tokens {
                Some(t) => t as f64,
                None if out.status == 0 => 0.0,
                None => PROBE_RESERVE,
            };
            d.ledger
                .settle_now(&account_id, limits, PROBE_RESERVE, actual, true);

            let h = d.model_health.entry(item.key.clone()).or_default();
            if out.ok {
                health::apply_ok(h, out.latency_ms, &now_rfc);
                budget::note_ok(&mut d.patrol);
            } else if out.no_free_tier {
                health::apply_no_free_tier(h, out.error.clone().unwrap_or_default(), &now_rfc);
            } else {
                health::apply_fail(h, out.error.clone().unwrap_or_default(), &now_rfc);
            }
        }

        {
            let mut d = self.st.data.write().await;
            d.patrol.last_run_at = Some(now_rfc);
            if force_all {
                d.patrol.last_sweep_at = Some(chrono::Utc::now().to_rfc3339());
                d.patrol.last_sweep_day = Some(Local::now().format("%Y-%m-%d").to_string());
            }
            store::save(&d);
        }

        if report.ran > 0 {
            tracing::info!(ran = report.ran, ok = report.ok, "patrol pass");
        }
        report
    }

    fn spawn_patrol(&self) {
        // 后台任务只需要 State_，构造一个同源的轻量句柄即可
        let gw = Gateway { st: self.st.clone() };
        tokio::spawn(async move {
            tokio::time::sleep(PATROL_WARMUP).await;
            loop {
                if !gw.st.running.load(Ordering::SeqCst) {
                    break;
                }
                gw.patrol_tick().await;
                tokio::time::sleep(PATROL_TICK).await;
            }
        });
    }

    async fn patrol_tick(&self) {
        // 目录过期就顺手刷新，一天最多一次
        let stale = {
            let c = self.st.catalog.read().await;
            catalog::needs_refresh(&c, 24)
        };
        if stale {
            if let Ok(cat) = catalog::refresh(&self.st.client).await {
                *self.st.catalog.write().await = cat;
                self.sync_catalog_meta(None).await;
                self.re_enrich().await;
            }
        }

        let sweep = {
            let d = self.st.data.read().await;
            patrol_calc::sweep_due(&d.patrol, Local::now())
        };
        let _ = self.patrol_pass(sweep).await;
    }
}

/// 按已接入厂商的日配额算今日探针预算。
fn budget_of(cat: &Catalog, data: &AppData) -> u32 {
    let quotas: Vec<u32> = data
        .providers
        .iter()
        .filter(|p| p.enabled)
        .flat_map(|p| {
            p.live_accounts()
                .filter(|a| secrets::has_credential(&a.id))
                // 每多一条凭据就多一份日额度，探针预算才跟着涨
                .map(move |_| {
                    cat.provider(&p.id)
                        .and_then(|c| c.quota)
                        .map(|q| q.rpd.or_else(|| p.quota.and_then(|x| x.rpd)).unwrap_or(0))
                        .unwrap_or(0)
                })
        })
        .collect();
    budget::total_budget(&quotas, data.patrol.budget_cap)
}

impl Default for Gateway {
    fn default() -> Self {
        Self::new()
    }
}

/* ── handlers ─────────────────────────────────────── */

/// 在途请求守卫。Drop 时自动减一，panic 也不会漏计。
struct Inflight<'a>(&'a AtomicUsize);

impl<'a> Inflight<'a> {
    fn new(c: &'a AtomicUsize) -> Self {
        c.fetch_add(1, Ordering::SeqCst);
        Self(c)
    }
}

impl Drop for Inflight<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// 模型健康每次请求都在变，但磁盘不该每个请求都写。
/// 30 秒最多落一次盘；崩溃丢了也只是重探一遍的事。
async fn persist_throttled(st: &Arc<State_>) {
    let now = chrono::Utc::now().timestamp_millis();
    let last = st.last_save_ms.load(Ordering::Relaxed);
    if now - last < 30_000 {
        return;
    }
    if st
        .last_save_ms
        .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
        .is_err()
    {
        return;
    }
    let d = st.data.read().await;
    store::save(&d);
}

fn oai_error(message: &str, kind: &str, code: &str, extra: Value) -> Value {
    let mut err = json!({ "message": message, "type": kind, "code": code });
    if let Some(obj) = extra.as_object() {
        for (k, v) in obj {
            err[k] = v.clone();
        }
    }
    json!({ "error": err })
}

/// 暂停闸。
///
/// 只挡业务 POST：`/health` 与 `/v1/models` 必须继续可用——否则「暂停」会变成
/// 「进程看起来死了」，客户端只会重试到超时，排障时更容易误判。
async fn pause_guard(
    State(st): State<Arc<State_>>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let business = req.method() == axum::http::Method::POST;
    if business && st.paused.load(Ordering::Relaxed) {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(oai_error(
                "Freeport 已暂停：控制面还在，业务请求不转发。恢复后自动继续。",
                "service_unavailable",
                "paused",
                json!({}),
            )),
        )
            .into_response();
    }
    next.run(req).await
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

#[derive(serde::Deserialize)]
struct ModelsQuery {
    /// Anthropic 的发现端点带 `?limit=`，不认就当成没给
    limit: Option<usize>,
}

/// 单条模型行：同一份响应里给两种形状。
///
/// OpenAI 客户端只读 `object` / `data`；Claude Code 的发现还要
/// `type` / `display_name` / `created_at`，缺一个就把这条丢掉。
fn model_json(m: &GwModel) -> Value {
    let mut v = serde_json::to_value(m).unwrap_or_else(|_| json!({}));
    if let Some(o) = v.as_object_mut() {
        o.insert("type".into(), json!("model"));
        o.insert("display_name".into(), json!(m.id));
        o.insert("created_at".into(), json!("1970-01-01T00:00:00Z"));
    }
    v
}

/// 别名行。`display_name` 带厂商名，否则选择器里全是 `claude-freeport-xxx`。
fn alias_json(a: &freeport_core::route::AliasEntry) -> Value {
    if a.is_virtual() {
        return json!({
            "id": a.alias,
            "object": "model",
            "type": "model",
            "display_name": "Freeport · 自动选择",
            "created_at": "1970-01-01T00:00:00Z",
            "owned_by": "freeport",
            "created": 0,
            "freeport": { "virtual": true },
        });
    }
    json!({
        "id": a.alias,
        "object": "model",
        "type": "model",
        "display_name": format!("Freeport · {} · {}", a.provider, a.model),
        "created_at": "1970-01-01T00:00:00Z",
        "owned_by": a.provider,
        "created": 0,
        "freeport": {
            "virtual": true,
            "provider": a.provider,
            "upstreamModel": a.model,
        },
    })
}

async fn list_models(
    State(st): State<Arc<State_>>,
    Query(q): Query<ModelsQuery>,
    headers: HeaderMap,
) -> Response {
    if resolve_key(&st, &headers).await.is_none() {
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
    let (models, alias_list) = {
        let d = st.data.read().await;
        (
            aggregate_models(&d),
            freeport_core::route::aliases(&d, &|a: &Account| secrets::has_credential(&a.id)),
        )
    };

    // 别名打头：Claude Code 只认 `claude*` 前缀，头一条会被它当成默认选中项。
    // 之所以要逐模型铺开而不是只给一条 `auto`：选择器里能看见具体模型，
    // 用户才可能主动挑，而不是永远被动接受自动路由。
    let mut data: Vec<Value> = alias_list.iter().map(alias_json).collect();
    data.extend(models.iter().map(model_json));

    let total = data.len();
    let limit = q.limit.unwrap_or(1000).clamp(1, 1000);
    data.truncate(limit);

    let first = data.first().and_then(|m| m["id"].as_str()).unwrap_or_default();
    let last = data.last().and_then(|m| m["id"].as_str()).unwrap_or_default();
    Json(json!({
        "object": "list",
        "type": "list",
        "data": data,
        "has_more": total > limit,
        "first_id": first,
        "last_id": last,
    }))
    .into_response()
}

/// `GET /v1/models/:id` —— Anthropic 与 OpenAI 都有这个端点，
/// 别名和真实模型 id 都要能查到，否则客户端发现完再去核实会 404。
async fn get_model(
    State(st): State<Arc<State_>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if resolve_key(&st, &headers).await.is_none() {
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
    let (models, alias_list) = {
        let d = st.data.read().await;
        (
            aggregate_models(&d),
            freeport_core::route::aliases(&d, &|a: &Account| secrets::has_credential(&a.id)),
        )
    };
    if let Some(a) = alias_list.iter().find(|a| a.alias == id) {
        return Json(alias_json(a)).into_response();
    }
    if let Some(m) = models.iter().find(|m| m.id == id) {
        return Json(model_json(m)).into_response();
    }
    (
        StatusCode::NOT_FOUND,
        Json(oai_error(
            &format!("模型不存在：{id}"),
            "invalid_request_error",
            "model_not_found",
            json!({}),
        )),
    )
        .into_response()
}

/// 会话标识：优先用调用方显式给的 header，没有就按对话内容推指纹。
///
/// 显式 header 让「同一段对话但不重发开头」的客户端（很少见）也能粘住；
/// 指纹让不改代码的 agent 也能享受粘性——这才是默认路径。
fn session_key(headers: &HeaderMap, model: &str, payload: &Value) -> String {
    // 前两个是我们的约定，第三个是 Claude Code 自己会带的官方头——
    // 白送的会话信号，不用它就得退回内容指纹（换一句 system 就全丢粘性）
    for name in [
        "x-freeport-session",
        "x-session-id",
        "x-claude-code-session-id",
    ] {
        if let Some(v) = headers.get(name).and_then(|v| v.to_str().ok()) {
            let v = v.trim();
            if !v.is_empty() {
                return format!("h:{v}");
            }
        }
    }
    format!("f:{}", affinity::Affinity::fingerprint(model, payload))
}

/// Gate → Order → Pick 一条完整链路。`/v1/route` 与真实请求共用它，
/// 否则调试看到的顺序和实际发出去的不一样，等于没调试。
fn build_plan(
    st: &Arc<State_>,
    d: &AppData,
    requested: &str,
    session: &str,
    needs: Needs,
) -> (Plan, bool) {
    build_plan_opts(st, d, requested, session, true, needs)
}

/// `commit = false` 是「预演」：SWRR 用状态副本算顺序、不写回粘性表。
///
/// 只有真实请求和 /v1/route 之外的调用才需要预演——UI 每刷新一次就
/// 推进一次轮转，会让用户看到的链路和实际流量互相干扰。
fn build_plan_opts(
    st: &Arc<State_>,
    d: &AppData,
    requested: &str,
    session: &str,
    commit: bool,
    needs: Needs,
) -> (Plan, bool) {
    // 对外露的是 `claude-freeport-*`，进来先脱壳，否则裸 id 找不到任何厂商
    let requested = freeport_core::route::resolve_alias(requested);
    let mut p = plan_with(d, requested, &st.cooldowns, &|a: &Account| {
        secrets::has_credential(&a.id)
    }, &d.ledger, needs);

    let now = freeport_core::cooldown::now_ms();

    // ③ Pick：先试会话粘性。
    // 粘住了就**不消耗轮转预算** —— 否则每粘一次，另一家就白捡一次加权，
    // SWRR 的比例会被粘性慢慢拉偏（实测会退化成连续三次同一家）。
    let mut sticky = false;
    if !session.is_empty() && p.candidates.len() > 1 {
        let prev = {
            let af = st.affinity.lock().unwrap_or_else(|e| e.into_inner());
            af.peek(session, now).cloned()
        };
        if let Some(s) = prev {
            // 余量见底就主动解粘：粘在最后一滴额度上，比换家贵得多
            if d.ledger.remaining_ratio(&s.account, now) < affinity::UNSTICK_RATIO {
                if commit {
                    st.affinity
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .forget(session);
                }
            } else {
                // 请求要图像而上次那家没测过视觉：宁可不粘，也不冒一次必定的 400
                let has_confirmed = p.candidates.iter().any(|c| !c.cap_unknown);
                let allow_unknown = !needs.any() || !has_confirmed;
                sticky = stick_to_front_if(&mut p, &s.provider, &s.account, &s.model, d, &|c| {
                    allow_unknown || !c.cap_unknown
                });
            }
        }
    }

    // ② Order：没粘住才轮转
    if !sticky && p.candidates.len() > 1 {
        let cands = std::mem::take(&mut p.candidates);
        p.candidates = {
            let mut swrr = st.swrr.lock().unwrap_or_else(|e| e.into_inner());
            if commit {
                swrr.order(cands, d, &d.ledger, now)
            } else {
                // 副本推进：既不写回，也不影响下一次真实请求
                let mut ghost = swrr.clone();
                ghost.order(cands, d, &d.ledger, now)
            }
        };
    }

    (p, sticky)
}

async fn route_debug(
    State(st): State<Arc<State_>>,
    Query(q): Query<std::collections::HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    if resolve_key(&st, &headers).await.is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let requested = q.get("model").cloned().unwrap_or_else(|| "auto".into());
    let session = q.get("session").cloned().unwrap_or_default();
    // 调试能力门禁：`?vision=1` 或 `?caps=vision,tools,json`
    let caps = q.get("caps").cloned().unwrap_or_default();
    let on = |k: &str| {
        q.get(k).map(|v| v == "1" || v == "true").unwrap_or(false)
            || caps.split(',').any(|c| c.trim() == k)
    };
    let needs = Needs {
        vision: on("vision"),
        tools: on("tools"),
        json: on("json"),
    };
    // 调试接口用预演模式：curl 一下不该改变下一单真实请求的落点
    Json(explain_route_json(&st, &requested, &session, false, needs).await).into_response()
}

/// 把一条链路解释成 JSON。`/v1/route` 与 GUI 的「路由」页共用这一份，
/// 保证界面看到的顺序就是 curl 出来的顺序。
async fn explain_route_json(
    st: &Arc<State_>,
    model: &str,
    session: &str,
    commit: bool,
    needs: Needs,
) -> serde_json::Value {
    let d = st.data.read().await;
    let now = freeport_core::cooldown::now_ms();
    let (p, sticky) = build_plan_opts(st, &d, model, session, commit, needs);

    let hops = p
        .candidates
        .iter()
        .map(|c| {
            let prov = &d.providers[c.provider];
            let h = d.model_health.get(&format!("{}:{}", prov.id, c.model_id));
            let (tier, headroom, health) =
                freeport_core::weight_factors(prov, &c.account, &d.ledger, h, now);
            let r3 = |x: f64| (x * 1000.0).round() / 1000.0;
            let base = freeport_core::weight_of(prov, &c.account, &d.ledger, h, now);
            json!({
                "provider": prov.id,
                "account": c.account,
                "model": c.model_id,
                "tier": prov.tier,
                "weight": r3(base * cap_factor(c)),
                "remaining": r3(d.ledger.remaining_ratio(&c.account, now)),
                "health": h.map(|x| x.status.as_str()).unwrap_or("unknown"),
                // 三因子单独给出，界面才能写「1.00 × 0.86 × 0.55 = 0.47」
                "factors": {
                    "tier": r3(tier),
                    "headroom": r3(headroom),
                    "health": r3(health),
                    "caps": r3(cap_factor(c)),
                },
                // 请求要的能力这个模型没测过：仍可用，但排在确认支持的后面
                "capsUnknown": c.cap_unknown,
            })
        })
        .collect::<Vec<_>>();

    let rejected = p
        .rejected
        .iter()
        .map(|r| {
            json!({
                "provider": r.provider,
                "account": r.account,
                "model": r.model,
                "reason": r.reason,
                "kind": r.kind,
            })
        })
        .collect::<Vec<_>>();

    json!({
        "model": model,
        "session": session,
        "needs": { "vision": needs.vision, "tools": needs.tools, "json": needs.json },
        "sticky": sticky,
        "strategy": d.routing.strategy,
        "budgetMs": d.gateway.request_budget_ms,
        "chain": hops,
        "rejected": rejected,
        "cooldowns": st.cooldowns.snapshot(),
    })
}

/// 解析调用方身份。`None` 表示未授权。
///
/// `require_auth = false` 时不校验，但仍尝试识别 key 以便归因（本地开发常见）。
async fn resolve_key(st: &Arc<State_>, headers: &HeaderMap) -> Option<keys::Identity> {
    let (require, token) = {
        let d = st.data.read().await;
        let token = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.trim_start_matches("Bearer ").trim().to_string())
            .unwrap_or_default();
        (d.gateway.require_auth, token)
    };
    if !require && token.is_empty() {
        return Some(keys::Identity::Legacy);
    }
    let d = st.data.read().await;
    let id = keys::resolve(&d, &token);
    if id.is_none() && !require {
        // 没开鉴权时，带了无效 token 也放行，只是无法归因
        return Some(keys::Identity::Legacy);
    }
    id
}

async fn chat(State(st): State<Arc<State_>>, headers: HeaderMap, body: Bytes) -> Response {
    let Some(identity) = resolve_key(&st, &headers).await else {
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

    // 日志归因：id 决定用量归属，name 是当次快照（key 以后被删也不影响这条记录）
    let key_label = identity.label();

    // 虚拟 key 的配额在这里判：任一维度打满就本地拒，不打上游
    {
        let blocked = {
            let mut d = st.data.write().await;
            keys::check_and_spend(&mut d, &key_label, "").err()
        };
        if let Some(msg) = blocked {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                Json(oai_error(&msg, "rate_limit_error", "key_quota", json!({}))),
            )
                .into_response();
        }
        persist_throttled(&st).await;
    }

    let started = Instant::now();
    // 在途计数：巡检靠它给真实流量让路；函数返回时自动减回去
    let _inflight = Inflight::new(&st.inflight);

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

    // 会话指纹：agent 不会主动带 session id，默认从「第一条 user 消息」推
    let session = session_key(&headers, &requested, &payload);
    // 能力要求从归一后的请求体里读：四个协议入口都汇到这里，只判一次
    let needs = Needs::of_chat(&payload);

    let (budget_ms, plan, has_any_key, sticky, key_name) = {
        let d = st.data.read().await;
        let (p, sticky) = build_plan(&st, &d, &requested, &session, needs);
        let has_any = d.providers.iter().any(|p| secrets::has_credential(&p.id));
        let name = d
            .api_keys
            .iter()
            .find(|k| k.id == key_label)
            .map(|k| k.name.clone())
            // 老 authToken 不在 key 列表里，但日志里也得看得出「这条是老客户端打的」
            .or_else(|| (key_label == "default").then(|| "legacy token".to_string()));
        (d.gateway.request_budget_ms, p, has_any, sticky, name)
    };

    // 各协议入口都汇到 chat()，日志里的 path 由调用方用内部头标注（只用于展示，不转发上游）
    let logged_path: String = headers
        .get("x-freeport-path")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("/v1/chat/completions")
        .chars()
        .take(48)
        .collect();

    let mut entry = LogEntry {
        id: format!("req_{}", &uuid::Uuid::new_v4().simple().to_string()[..12]),
        at: chrono::Utc::now().to_rfc3339(),
        path: logged_path,
        requested_model: requested.clone(),
        model: None,
        provider: None,
        account_id: None,
        account_name: None,
        stream,
        ok: false,
        status: 0,
        ms: 0,
        usage: None,
        attempts: Vec::new(),
        sticky,
        key_id: Some(key_label.clone()),
        key_name,
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

    // (provider, account, model)：多账号下「这条 key 挂了」不能连坐同一家的另一条 key
    let mut skipped: HashSet<(usize, String, String)> = HashSet::new();

    let queue = plan.candidates.clone();
    let queue_len = queue.len();
    for (qi, cand) in queue.into_iter().enumerate() {
        let has_next = qi + 1 < queue_len;
        if started.elapsed() > Duration::from_millis(budget_ms) && entry.attempts.len() >= 2 {
            break;
        }
        if skipped.contains(&(cand.provider, cand.account.clone(), cand.model_id.clone())) {
            continue;
        }

        // 候选的粒度是凭据：换的是这条 key，不是整个厂商
        let (provider, account_quota) = {
            let d = st.data.read().await;
            let p = d.providers[cand.provider].clone();
            let quota = p.account(&cand.account).and_then(|a| a.quota);
            (p, quota)
        };
        // 续期要发网络请求，必须放在读锁外面
        let Some(mut cred) = credential(&st, &cand.account).await else { continue };
        let account_id = cand.account.clone();

        let mut body_for_upstream = payload.clone();
        body_for_upstream["model"] = json!(cand.model_id);
        if stream && body_for_upstream.get("stream_options").is_none() {
            body_for_upstream["stream_options"] = json!({ "include_usage": true });
        }
        // 这条凭据的字面量：上游在错误体里回显 key 是常态，落盘前先抹掉
        let red = Redactor::for_credential(&cred);

        let remaining = Duration::from_millis(
            budget_ms.saturating_sub(started.elapsed().as_millis() as u64).max(1000),
        );
        let t0 = Instant::now();

        // 先记账再发请求：并发下不会超卖，代价是偏保守
        let limits = quota::limits_of(account_quota.as_ref());
        let (est_input, est_total) = estimate_tokens(&payload);
        {
            let mut d = st.data.write().await;
            d.ledger.reserve_now(&account_id, limits, est_total, false);
        }

        match providers::create_chat(&st.client, &provider, &cred, &body_for_upstream, remaining).await {
            Ok(upstream) => {
                // web 会话轮换（服务端换 session）必须当场学回来，否则用户
                // 过一阵就被踢出去，日志上还看不出原因
                let set_cookie: Vec<String> = upstream
                    .headers()
                    .get_all("set-cookie")
                    .iter()
                    .filter_map(|v| v.to_str().ok())
                    .map(str::to_string)
                    .collect();
                if !set_cookie.is_empty() {
                    let host = upstream.url().host_str().map(str::to_string);
                    if cred.absorb(host.as_deref(), &set_cookie) {
                        tracing::debug!(account = %cand.account, "web 会话已更新");
                    }
                }
                let provider_id = provider.id.clone();
                let model_id = cand.model_id.clone();

                if stream {
                    // 最后一跳没得换，扣住只是白等；有下家才值得等首个有效语义块
                    let committed = if has_next {
                        let cfg = {
                            let d = st.data.read().await;
                            let cap = remaining.as_millis() as u64;
                            HoldCfg {
                                idle_ms: d.gateway.hold_idle_ms.min(cap),
                                total_ms: d.gateway.hold_total_ms.min(cap),
                            }
                        };
                        match hold::wait_commit(upstream, &cfg).await {
                            HoldOutcome::Committed { head, rest } => Ok((head, rest)),
                            HoldOutcome::Rejected { kind, detail } => {
                                // 这段 detail 会被写进日志并回给客户端，先抹掉 key
                                Err((kind.to_string(), red.redact(&detail)))
                            }
                        }
                    } else {
                        Ok((Vec::new(), upstream.bytes_stream().boxed()))
                    };

                    match committed {
                        Ok((head, rest)) => {
                            mark_success(&st, &mut entry, &provider_id, &account_id, &model_id, t0.elapsed().as_millis() as u64, cand.half_open, &session).await;
                            let trail = trail_of(&entry);
                            return stream_response(st.clone(), head, rest, entry, started, provider_id, account_id, model_id, trail, limits, est_total);
                        }
                        Err((kind, detail)) => {
                            let cls = if kind == "upstream_body" {
                                classify(400, &detail, 0)
                            } else {
                                ErrorClass::soft(&kind)
                            };
                            note_fail(&mut entry, &st, &provider_id, &account_id, &model_id, &cls, 0, t0.elapsed().as_millis() as u64, &detail).await;
                            // 上游已受理过（200 但内容无效），输入被计费，只回吐输出的预扣
                            settle_quota(&st, &account_id, limits, est_total, est_input).await;
                            continue;
                        }
                    }
                }

                // 非流式：读全 body 再判，200 里藏 error 是免费厂商的常见形态
                let text = upstream.text().await.unwrap_or_default();
                if let Some(cls) = body_error(&text) {
                    // 200 里藏的 error 也可能带 key 回显，进日志前抹掉
                    let clean = red.redact(&text);
                    note_fail(&mut entry, &st, &provider_id, &account_id, &model_id, &cls, 0, t0.elapsed().as_millis() as u64, &clean).await;
                    continue;
                }

                mark_success(&st, &mut entry, &provider_id, &account_id, &model_id, t0.elapsed().as_millis() as u64, cand.half_open, &session).await;

                let json: Value = serde_json::from_str(&text).unwrap_or_else(|_| json!({ "raw": text }));
                entry.usage = json.get("usage").and_then(parse_usage);
                entry.ms = started.elapsed().as_millis() as u64;
                bump_stats(&st, &entry.usage, false).await;
                // 拿到真实 usage 就回吐多扣的部分；拿不到（流式未带 include_usage）就认预扣
                settle_quota(
                    &st,
                    &account_id,
                    limits,
                    est_total,
                    usage_tokens(&entry.usage).unwrap_or(est_total),
                )
                .await;
                let trail = trail_of(&entry);
                record(&st, entry.clone()).await;
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
                // web 会话失效与「key 无效」是两回事：前者只能靠人重新登录，
                // 说成 key 无效会让用户去翻一把根本不存在的 key
                let detail = if cred.is_cookie() && matches!(e.status, 401 | 403) {
                    format!("web 会话已失效，请到凭据页重新粘贴会话（{}）", e.body.chars().take(120).collect::<String>())
                } else {
                    e.body.clone()
                };
                if note_fail(&mut entry, &st, &provider.id, &account_id, &cand.model_id, &cls, e.status, t0.elapsed().as_millis() as u64, &detail).await {
                    // 作用域决定跳过谁：
                    // - Provider（5xx / 连不上）：这一家所有凭据本轮都别再撞
                    // - Key（限流 / 欠费 / key 失效）：只跳过这一条凭据，同一家的另一条照用
                    let whole_provider = matches!(cls.scope, freeport_core::Scope::Provider);
                    for c in plan.candidates.iter() {
                        if c.provider == cand.provider
                            && (whole_provider || c.account == account_id)
                        {
                            skipped.insert((c.provider, c.account.clone(), c.model_id.clone()));
                        }
                    }
                }
                // status == 0 表示没连上，上游没计费，全退；否则输入已送达，退到只算输入
                let actual = if e.status == 0 { 0.0 } else { est_input };
                settle_quota(&st, &account_id, limits, est_total, actual).await;
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

/* ── Anthropic Messages 入口 ─────────────────────
   请求转成 OpenAI 形态后走**同一条**接力链，响应再转回来。
   这样 failover / Hold / 账本 / 虚拟 key 全部复用，不维护第二套循环。 */

async fn messages(State(st): State<Arc<State_>>, mut headers: HeaderMap, body: Bytes) -> Response {
    // Claude Code 用 `x-api-key`，内部统一成 Authorization
    if !headers.contains_key("authorization") {
        if let Some(k) = headers.get("x-api-key").and_then(|v| v.to_str().ok()) {
            if let Ok(hv) = HeaderValue::from_str(&format!("Bearer {k}")) {
                headers.insert("authorization", hv);
            }
        }
    }

    let v: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return anth_error(400, "invalid_request_error", "请求体不是合法 JSON"),
    };
    let stream = v.get("stream").and_then(|x| x.as_bool()).unwrap_or(false);
    let oa = anthropic::to_openai(&v);

    headers.insert("x-freeport-path", HeaderValue::from_static("/v1/messages"));

    let res = chat(State(st), headers, Bytes::from(oa.to_string())).await;
    let status = res.status();
    let model = res
        .headers()
        .get("x-freeport-model")
        .and_then(|x| x.to_str().ok())
        .unwrap_or("freeport")
        .to_string();
    let diag: Vec<(String, String)> = res
        .headers()
        .iter()
        .filter(|(k, _)| k.as_str().starts_with("x-freeport-"))
        .filter_map(|(k, v)| v.to_str().ok().map(|s| (k.to_string(), s.to_string())))
        .collect();
    let provider = res
        .headers()
        .get("x-freeport-provider")
        .and_then(|x| x.to_str().ok())
        .unwrap_or("freeport")
        .to_string();

    if stream && status.is_success() {
        let mut out = Response::new(anthropic::sse_transform(res.into_body(), model));
        *out.status_mut() = StatusCode::OK;
        let h = out.headers_mut();
        h.insert(
            "content-type",
            HeaderValue::from_static("text/event-stream; charset=utf-8"),
        );
        h.insert("cache-control", HeaderValue::from_static("no-cache"));
        h.insert("connection", HeaderValue::from_static("keep-alive"));
        for (k, v) in diag {
            if let (Ok(name), Ok(val)) = (HeaderName::from_bytes(k.as_bytes()), HeaderValue::from_str(&v)) {
                h.insert(name, val);
            }
        }
        return out;
    }

    let raw = axum::body::to_bytes(res.into_body(), MAX_BODY)
        .await
        .unwrap_or_default();
    let parsed: Value = serde_json::from_slice(&raw).unwrap_or(Value::Null);

    let mut out = if let Some(err) = parsed.get("error") {
        let msg = err
            .get("message")
            .and_then(|x| x.as_str())
            .unwrap_or("上游全部候选失败");
        let kind = anth_kind(err.get("type").and_then(|x| x.as_str()).unwrap_or("api_error"));
        let mut r = (
            anth_status(status),
            Json(anthropic::error_body(kind, msg)),
        )
            .into_response();
        if let Some(trail) = diag.iter().find(|(k, _)| k == "x-freeport-trail") {
            if let Ok(val) = HeaderValue::from_str(&trail.1) {
                r.headers_mut().insert("x-freeport-trail", val);
            }
        }
        r
    } else if parsed.is_null() {
        anth_error(502, "api_error", "上游返回了无法解析的响应")
    } else {
        (StatusCode::OK, Json(anthropic::from_openai(&parsed, &provider))).into_response()
    };

    if let Some((_, trail)) = diag.iter().find(|(k, _)| k == "x-freeport-trail") {
        if let Ok(val) = HeaderValue::from_str(trail) {
            out.headers_mut().insert("x-freeport-trail", val);
        }
    }
    out
}

/// Claude Code 启动前会探一次 token 数；给个估算就够，不打上游。
async fn count_tokens(State(st): State<Arc<State_>>, headers: HeaderMap, body: Bytes) -> Response {
    if resolve_key(&st, &headers).await.is_none() {
        return anth_error(401, "authentication_error", "未授权");
    }
    let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let (input, _) = estimate_tokens(&anthropic::to_openai(&v));
    Json(json!({ "input_tokens": input.ceil() as u64 })).into_response()
}

/* ── OpenAI Responses 入口（Codex 原生协议） ───────
   和 messages / gemini 一样：请求转成 chat 形态走同一条接力链，
   响应再组装回 `output` 数组与语义化 SSE 事件。 */

async fn responses_entry(
    State(st): State<Arc<State_>>,
    mut headers: HeaderMap,
    body: Bytes,
) -> Response {
    let v: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(responses::error_body("请求体不是合法 JSON", "bad_json")),
            )
                .into_response()
        }
    };
    let stream = v.get("stream").and_then(|x| x.as_bool()).unwrap_or(false);

    // 让请求日志按真实入口记账，而不是一律写成 chat/completions
    headers.insert("x-freeport-path", HeaderValue::from_static("/v1/responses"));

    let chat_body = responses::to_chat(&v);
    let res = chat(State(st), headers, Bytes::from(chat_body.to_string())).await;

    let status = res.status();
    let model = res
        .headers()
        .get("x-freeport-model")
        .and_then(|x| x.to_str().ok())
        .unwrap_or("freeport")
        .to_string();
    let diag: Vec<(String, String)> = res
        .headers()
        .iter()
        .filter(|(k, _)| k.as_str().starts_with("x-freeport-"))
        .filter_map(|(k, v)| v.to_str().ok().map(|s| (k.to_string(), s.to_string())))
        .collect();

    if stream && status.is_success() {
        let mut out = Response::new(responses::sse_transform(res.into_body(), model));
        *out.status_mut() = StatusCode::OK;
        let h = out.headers_mut();
        h.insert(
            "content-type",
            HeaderValue::from_static("text/event-stream; charset=utf-8"),
        );
        h.insert("cache-control", HeaderValue::from_static("no-cache"));
        h.insert("connection", HeaderValue::from_static("keep-alive"));
        for (k, v) in &diag {
            if let (Ok(name), Ok(val)) =
                (HeaderName::from_bytes(k.as_bytes()), HeaderValue::from_str(v))
            {
                h.insert(name, val);
            }
        }
        return out;
    }

    let raw = axum::body::to_bytes(res.into_body(), MAX_BODY)
        .await
        .unwrap_or_default();
    let parsed: Value = serde_json::from_slice(&raw).unwrap_or(Value::Null);

    let mut out = if parsed.get("error").is_some() {
        // chat() 的错误体已经是 `{error:{message,type,code}}`，Responses 客户端认这个形态
        (status, Json(parsed)).into_response()
    } else if parsed.is_null() {
        (
            StatusCode::BAD_GATEWAY,
            Json(responses::error_body("上游返回了无法解析的响应", "bad_upstream")),
        )
            .into_response()
    } else {
        (
            StatusCode::OK,
            Json(responses::from_chat(&parsed, &model)),
        )
            .into_response()
    };

    if let Some((_, trail)) = diag.iter().find(|(k, _)| k == "x-freeport-trail") {
        if let Ok(val) = HeaderValue::from_str(trail) {
            out.headers_mut().insert("x-freeport-trail", val);
        }
    }
    out
}

/* ── Gemini 原生协议（/v1beta） ─────────────────── */

/// `GET /v1beta/models`。Gemini CLI 启动时先拉这个，没有就报「无可用模型」。
async fn gemini_models(State(st): State<Arc<State_>>, headers: HeaderMap) -> Response {
    if resolve_key(&st, &headers).await.is_none() {
        return (
            StatusCode::UNAUTHORIZED,
            Json(gemini::error_body(401, "未授权：缺少有效的 Freeport Key")),
        )
            .into_response();
    }
    let models: Vec<Value> = {
        let d = st.data.read().await;
        aggregate_models(&d)
    }
    .into_iter()
    .map(|m| {
            json!({
                "name": format!("models/{}", m.id),
                "displayName": m.id,
                "description": format!("via {}", m.owned_by),
                "inputTokenLimit": m.context_length.unwrap_or(32_768),
                "outputTokenLimit": 8_192u64,
                "supportedGenerationMethods": ["generateContent", "streamGenerateContent", "countTokens"]
            })
        })
        .collect();
    Json(json!({ "models": models })).into_response()
}

/// 形如 `gemini-2.0-flash:generateContent` / `models/gemini-2.0-flash:streamGenerateContent`。
fn split_tail(tail: &str) -> (String, String) {
    let tail = tail.strip_prefix("models/").unwrap_or(tail);
    match tail.rsplit_once(':') {
        Some((m, a)) => (m.to_string(), a.to_string()),
        None => (tail.to_string(), "generateContent".to_string()),
    }
}

async fn gemini(
    State(st): State<Arc<State_>>,
    Path(tail): Path<String>,
    mut headers: HeaderMap,
    Query(q): Query<std::collections::HashMap<String, String>>,
    body: Bytes,
) -> Response {
    // Gemini 用 `x-goog-api-key`，部分 SDK 还会把 key 放 query
    if !headers.contains_key("authorization") {
        let key = headers
            .get("x-goog-api-key")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
            .or_else(|| q.get("key").cloned());
        if let Some(k) = key {
            if let Ok(hv) = HeaderValue::from_str(&format!("Bearer {k}")) {
                headers.insert("authorization", hv);
            }
        }
    }

    let (model, action) = split_tail(&tail);

    if resolve_key(&st, &headers).await.is_none() {
        return (
            StatusCode::UNAUTHORIZED,
            Json(gemini::error_body(401, "未授权：缺少有效的 Freeport Key")),
        )
            .into_response();
    }

    let v: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(gemini::error_body(400, "请求体不是合法 JSON")),
            )
                .into_response()
        }
    };

    // countTokens 不打上游：估算就够，Gemini CLI 用它做上下文预算
    if action == "countTokens" {
        let (input, _) = estimate_tokens(&gemini::to_openai(&v, &model));
        return Json(json!({ "totalTokens": input.ceil() as u64 })).into_response();
    }

    let stream = action == "streamGenerateContent"
        || q.get("alt").map(|a| a == "sse").unwrap_or(false);
    if action != "generateContent" && action != "streamGenerateContent" {
        return (
            StatusCode::NOT_FOUND,
            Json(gemini::error_body(404, &format!("不支持的方法：{action}"))),
        )
            .into_response();
    }

    headers.insert("x-freeport-path", HeaderValue::from_static("/v1beta"));

    let mut oa = gemini::to_openai(&v, &model);
    oa["stream"] = json!(stream);

    let res = chat(State(st), headers, Bytes::from(oa.to_string())).await;
    let status = res.status();
    let used_model = res
        .headers()
        .get("x-freeport-model")
        .and_then(|x| x.to_str().ok())
        .unwrap_or(&model)
        .to_string();
    let diag: Vec<(String, String)> = res
        .headers()
        .iter()
        .filter(|(k, _)| k.as_str().starts_with("x-freeport-"))
        .filter_map(|(k, v)| v.to_str().ok().map(|s| (k.to_string(), s.to_string())))
        .collect();

    if stream && status.is_success() {
        let mut out = Response::new(gemini::sse_transform(res.into_body(), used_model));
        *out.status_mut() = StatusCode::OK;
        let h = out.headers_mut();
        h.insert(
            "content-type",
            HeaderValue::from_static("text/event-stream; charset=utf-8"),
        );
        h.insert("cache-control", HeaderValue::from_static("no-cache"));
        for (k, v) in &diag {
            if let (Ok(name), Ok(val)) =
                (HeaderName::from_bytes(k.as_bytes()), HeaderValue::from_str(v))
            {
                h.insert(name, val);
            }
        }
        return out;
    }

    let raw = axum::body::to_bytes(res.into_body(), MAX_BODY)
        .await
        .unwrap_or_default();
    let parsed: Value = serde_json::from_slice(&raw).unwrap_or(Value::Null);

    let mut out = if let Some(err) = parsed.get("error") {
        let msg = err
            .get("message")
            .and_then(|x| x.as_str())
            .unwrap_or("上游全部候选失败");
        let code = status.as_u16();
        (status, Json(gemini::error_body(code, msg))).into_response()
    } else if parsed.is_null() {
        (
            StatusCode::BAD_GATEWAY,
            Json(gemini::error_body(502, "上游返回了无法解析的响应")),
        )
            .into_response()
    } else {
        (StatusCode::OK, Json(gemini::from_openai(&parsed, &used_model))).into_response()
    };

    for (k, v) in diag {
        if k == "x-freeport-trail" {
            if let Ok(val) = HeaderValue::from_str(&v) {
                out.headers_mut().insert("x-freeport-trail", val);
            }
        }
    }
    out
}

/* ── Ollama 兼容（/api 三个入口） ─────────────────── */

fn ollama_error(status: StatusCode, msg: &str) -> Response {
    (status, Json(ollama::error_body(msg))).into_response()
}

async fn ollama_version() -> Response {
    Json(json!({ "version": "freeport" })).into_response()
}

async fn ollama_tags(State(st): State<Arc<State_>>, headers: HeaderMap) -> Response {
    if resolve_key(&st, &headers).await.is_none() {
        return ollama_error(StatusCode::UNAUTHORIZED, "未授权：缺少有效的 Freeport Key");
    }
    let models = {
        let d = st.data.read().await;
        aggregate_models(&d)
    };
    let list: Vec<Value> = models
        .iter()
        .map(|m| ollama::tag(&m.id, &m.owned_by, m.context_length))
        .collect();
    Json(json!({ "models": list })).into_response()
}

async fn ollama_chat(
    State(st): State<Arc<State_>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    ollama_dispatch(st, headers, body, false).await
}

async fn ollama_generate(
    State(st): State<Arc<State_>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    ollama_dispatch(st, headers, body, true).await
}

/// 两个入口只差请求/响应的外壳，中间那条路完全一样。
async fn ollama_dispatch(
    st: Arc<State_>,
    mut headers: HeaderMap,
    body: Bytes,
    generate: bool,
) -> Response {
    let v: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return ollama_error(StatusCode::BAD_REQUEST, "请求体不是合法 JSON"),
    };
    let model = v
        .get("model")
        .and_then(|x| x.as_str())
        .unwrap_or("auto")
        .to_string();

    // Ollama 的默认是**流式**，和 OpenAI 相反
    let stream = v.get("stream").and_then(|x| x.as_bool()).unwrap_or(true);

    let mut oa = if generate {
        ollama::to_openai_generate(&v, &model)
    } else {
        ollama::to_openai_chat(&v, &model)
    };
    oa["stream"] = json!(stream);

    headers.insert(
        "x-freeport-path",
        HeaderValue::from_static(if generate { "/api/generate" } else { "/api/chat" }),
    );

    let res = chat(State(st), headers, Bytes::from(oa.to_string())).await;
    let status = res.status();
    let used = res
        .headers()
        .get("x-freeport-model")
        .and_then(|x| x.to_str().ok())
        .unwrap_or(&model)
        .to_string();

    if stream && status.is_success() {
        let mut out = Response::new(ollama::ndjson(res.into_body(), used, generate));
        *out.status_mut() = StatusCode::OK;
        out.headers_mut().insert(
            "content-type",
            HeaderValue::from_static("application/x-ndjson"),
        );
        return out;
    }

    let raw = axum::body::to_bytes(res.into_body(), MAX_BODY)
        .await
        .unwrap_or_default();
    let parsed: Value = serde_json::from_slice(&raw).unwrap_or(Value::Null);

    if let Some(err) = parsed.get("error") {
        let msg = err
            .get("message")
            .and_then(|x| x.as_str())
            .unwrap_or("上游全部候选失败");
        return ollama_error(status, msg);
    }
    if parsed.is_null() {
        return ollama_error(StatusCode::BAD_GATEWAY, "上游返回了无法解析的响应");
    }

    let out = if generate {
        ollama::generate_response(&parsed, &used)
    } else {
        ollama::chat_response(&parsed, &used)
    };
    (StatusCode::OK, Json(out)).into_response()
}

fn anth_status(s: StatusCode) -> StatusCode {
    // Anthropic 用 529 表示过载，客户端会照着退避
    if s == StatusCode::SERVICE_UNAVAILABLE {
        StatusCode::from_u16(529).unwrap_or(StatusCode::BAD_GATEWAY)
    } else {
        s
    }
}

fn anth_kind(openai_kind: &str) -> &'static str {
    match openai_kind {
        "invalid_request_error" => "invalid_request_error",
        "authentication_error" => "authentication_error",
        "permission_error" => "permission_error",
        "rate_limit_error" => "rate_limit_error",
        "not_found_error" => "not_found_error",
        _ => "api_error",
    }
}

fn anth_error(status: u16, kind: &str, msg: &str) -> Response {
    (
        StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_REQUEST),
        Json(anthropic::error_body(kind, msg)),
    )
        .into_response()
}

/// 判定为成功：写 trail、清冷却、更新模型健康。
/// 只有真正拿到内容后才调用——响应头不算成功。
async fn mark_success(
    st: &Arc<State_>,
    entry: &mut LogEntry,
    provider_id: &str,
    account_id: &str,
    model_id: &str,
    latency_ms: u64,
    half_open: bool,
    session: &str,
) {
    entry.attempts.push(Attempt {
        provider: provider_id.to_string(),
        account: Some(account_id.to_string()),
        model: model_id.to_string(),
        ms: latency_ms,
        status: 200,
        kind: "ok".into(),
        scope: None,
        cooled_ms: None,
        detail: None,
        skip_provider: None,
    });
    entry.model = Some(model_id.to_string());
    entry.provider = Some(provider_id.to_string());
    entry.account_id = Some(account_id.to_string());
    entry.ok = true;
    entry.status = 200;

    // 名字存快照：凭据以后被删掉，这条日志仍然说得清是哪个账号答的
    entry.account_name = {
        let d = st.data.read().await;
        d.find_account(account_id).map(|(_, _, a)| a.name.clone())
    };

    // 记住这段对话是谁答的：下一轮同一个会话会优先回到它
    if !session.is_empty() {
        st.affinity
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .note(session, provider_id, account_id, model_id, freeport_core::cooldown::now_ms());
    }

    st.cooldowns.note_success(provider_id, account_id, Some(model_id));

    // 真实流量是最高优先级的探针：成功一次就够把模型拉回 healthy
    {
        let now = chrono::Utc::now().to_rfc3339();
        let key = format!("{provider_id}:{model_id}");
        let mut d = st.data.write().await;
        let h = d.model_health.entry(key).or_default();
        health::note_used(h, &now);
        health::apply_ok(h, latency_ms, &now);
    }
    persist_throttled(st).await;

    if half_open {
        tracing::info!("half-open probe succeeded: {provider_id}");
    }
}

/// 记录一次失败尝试。返回 true 表示 provider / key 整体不可用，本轮跳过它其余模型。
async fn note_fail(
    entry: &mut LogEntry,
    st: &Arc<State_>,
    provider_id: &str,
    account_id: &str,
    model_id: &str,
    cls: &ErrorClass,
    status: u16,
    ms: u64,
    detail: &str,
) -> bool {
    // 冷却 0 的失败（安全拒答 / 空响应 / 模型慢）只换下一跳，不写冷却
    let cooled = if cls.cooldown_ms > 0 {
        st.cooldowns.hit(
            cls.scope,
            provider_id,
            account_id,
            Some(model_id),
            cls.cooldown_ms,
            &cls.kind,
        )
    } else {
        0
    };

    // 上游在 429 里写明了额度上限时回填账本：Gate 从此按真值算余量，
    // 不用等用户手工把 rpd 填进配置。只收紧，见 `Ledger::learn`。
    if let Some(l) = &cls.learned {
        let now = freeport_core::cooldown::now_ms();
        let mut d = st.data.write().await;
        if let Some(rpd) = l.daily_limit {
            d.ledger.learn(account_id, quota::WindowKind::Rpd, rpd as f64, now);
        }
        if let Some(rpm) = l.minute_limit {
            d.ledger.learn(account_id, quota::WindowKind::Rpm, rpm as f64, now);
        }
    }

    // 上游说「这个模型不支持 X」时，把它记成实测能力。
    // 这是能力门禁唯一的自我修正入口：声明可能过期，实测不会。
    // 冷却为 0 意味着不判死任何东西，只是让下一单同类请求不再撞同一堵墙。
    if let Some(cap) = &cls.capability {
        let mut d = st.data.write().await;
        let h = d
            .model_health
            .entry(format!("{provider_id}:{model_id}"))
            .or_default();
        health::deny_cap(h, cap);
        drop(d);
        persist_throttled(st).await;
    }

    let skip = matches!(
        cls.scope,
        freeport_core::Scope::Provider | freeport_core::Scope::Key
    );
    entry.attempts.push(Attempt {
        provider: provider_id.to_string(),
        account: Some(account_id.to_string()),
        model: model_id.to_string(),
        ms,
        status,
        kind: cls.kind.clone(),
        scope: Some(cls.scope.as_str().to_string()),
        cooled_ms: (cooled > 0).then_some(cooled),
        detail: Some(detail.chars().take(240).collect()),
        skip_provider: skip.then_some(true),
    });
    skip
}

fn stream_response(
    st: Arc<State_>,
    head: Vec<Bytes>,
    rest: BoxStream<'static, Result<Bytes, reqwest::Error>>,
    mut entry: LogEntry,
    started: Instant,
    provider_id: String,
    account_id: String,
    model_id: String,
    trail: String,
    limits: [f64; 4],
    reserved: f64,
) -> Response {
    // header 需要 provider 名，spawn 内部结算也要用，留一份
    let provider_id_header = provider_id.clone();
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(32);

    tokio::spawn(async move {
        let mut tail = String::new();
        let mut broken = false;
        // head 是 Hold 阶段已经读掉的字节，必须先补发给客户端
        let mut stream = futures_util::stream::iter(
            head.into_iter().map(|b| Ok::<Bytes, reqwest::Error>(b)),
        )
        .chain(rest)
        .boxed();
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
                    broken = true;
                    break;
                }
            }
        }
        // STOP：已经交出去的内容收不回来，但「没答完」必须说出来。
        // 静默闭合最坏——客户端当成正常结束，用户看到一段被截断的回答，
        // 还会在此基础上继续追问。
        if !broken && !stream_complete(&tail) {
            entry.error = Some("上游流在结束标记前断开".into());
            let msg = format!(
                "data: {}\n\n",
                oai_error(
                    "上游连接在结束标记前断开，本次回复可能不完整",
                    "freeport_stream_truncated",
                    "stream_truncated",
                    json!({})
                )
            );
            let _ = tx.send(Ok(Bytes::from(msg))).await;
        }
        entry.usage = extract_usage(&tail);
        entry.ms = started.elapsed().as_millis() as u64;
        bump_stats(&st, &entry.usage, false).await;
        settle_quota(
            &st,
            &account_id,
            limits,
            reserved,
            usage_tokens(&entry.usage).unwrap_or(reserved),
        )
        .await;
        record(&st, entry).await;
    });

    let body = Body::from_stream(ReceiverStream::new(rx));
    let mut res = Response::new(body);
    let h = res.headers_mut();
    h.insert("content-type", HeaderValue::from_static("text/event-stream; charset=utf-8"));
    h.insert("cache-control", HeaderValue::from_static("no-cache, no-transform"));
    h.insert("connection", HeaderValue::from_static("keep-alive"));
    h.insert("x-accel-buffering", HeaderValue::from_static("no"));
    h.insert("x-freeport-provider", HeaderValue::from_str(&provider_id_header).unwrap_or(HeaderValue::from_static("")));
    h.insert("x-freeport-model", HeaderValue::from_str(&model_id).unwrap_or(HeaderValue::from_static("")));
    h.insert("x-freeport-trail", HeaderValue::from_str(&trail).unwrap_or(HeaderValue::from_static("")));
    res
}

/// 上游流是不是正常收尾。
///
/// OpenAI 兼容流以 `data: [DONE]` 结束；有的厂商不发 DONE，但最后一个 chunk
/// 带非空的 `finish_reason`。两者都没有 = 半截断流：连接干净地关了，内容却
/// 停在句子中间。这种情况**必须自己发一帧 error**，否则客户端会当成正常结束。
fn stream_complete(tail: &str) -> bool {
    if tail.contains("[DONE]") {
        return true;
    }
    let mut rest = tail;
    while let Some(i) = rest.find("\"finish_reason\"") {
        let after = &rest[i + "\"finish_reason\"".len()..];
        let after = after.trim_start();
        let after = after.strip_prefix(':').unwrap_or(after).trim_start();
        // `"finish_reason":null` 是「还在生成」，不是收尾
        if after.starts_with('"') && !after.starts_with("\"null\"") {
            return true;
        }
        rest = after;
    }
    false
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

/// 预扣用的 token 估算：返回 `(输入估算, 输入 + max_tokens)`。
///
/// 刻意偏保守 —— 预扣多了只是暂时少算余量，预扣少了会超卖上游额度。
/// 真实值由 `settle` 在拿到 usage 后回吐。中文大约 1.5 字/token，
/// 混排取 3 字符/token 作为折中。
fn estimate_tokens(payload: &Value) -> (f64, f64) {
    fn chars_of(v: &Value, out: &mut usize) {
        match v {
            Value::String(s) => *out += s.chars().count(),
            Value::Array(a) => a.iter().for_each(|x| chars_of(x, out)),
            Value::Object(o) => o.values().for_each(|x| chars_of(x, out)),
            _ => {}
        }
    }
    let mut chars = 0usize;
    chars_of(&payload["messages"], &mut chars);
    chars_of(&payload["system"], &mut chars);
    chars_of(&payload["tools"], &mut chars);

    let input = (chars as f64 / 3.0).ceil().max(1.0);
    let max_out = payload
        .get("max_completion_tokens")
        .or_else(|| payload.get("max_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(1024) as f64;
    (input, input + max_out)
}

/// 从 usage 里取一个可结算的总量。
fn usage_tokens(u: &Option<Usage>) -> Option<f64> {
    let u = u.as_ref()?;
    if let Some(t) = u.total_tokens {
        return Some(t as f64);
    }
    match (u.prompt_tokens, u.completion_tokens) {
        (Some(p), Some(c)) => Some((p + c) as f64),
        (Some(p), None) => Some(p as f64),
        _ => None,
    }
}

/// 把预扣的 token 修正成实际值。请求数不回滚——上游确实收了这一次。
/// `account` 是凭据 id：额度按凭据算，不是按厂商。
async fn settle_quota(
    st: &Arc<State_>,
    account: &str,
    limits: [f64; 4],
    reserved: f64,
    actual: f64,
) {
    let mut d = st.data.write().await;
    d.ledger.settle_now(account, limits, reserved, actual, false);
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

#[cfg(test)]
mod tests {
    use super::stream_complete;

    #[test]
    fn done_marker_counts_as_finished() {
        assert!(stream_complete("data: {\"choices\":[]}\n\ndata: [DONE]\n\n"));
    }

    #[test]
    fn finish_reason_counts_as_finished_when_done_is_missing() {
        // 有的厂商不发 [DONE]，只在最后一帧带 finish_reason
        assert!(stream_complete(r#"data: {"choices":[{"delta":{},"finish_reason":"stop"}]}"#));
        assert!(stream_complete(r#"data:{"choices":[{"finish_reason" : "length"}]}"#));
    }

    #[test]
    fn null_finish_reason_is_still_generating() {
        assert!(!stream_complete(
            r#"data: {"choices":[{"delta":{"content":"半截"},"finish_reason":null}]}"#
        ));
    }

    #[test]
    fn clean_close_without_marker_is_truncation() {
        // 连接干净地关了，但内容停在句子中间——这是最容易骗过客户端的一种
        assert!(!stream_complete("data: {\"choices\":[{\"delta\":{\"content\":\"答\"}}]}"));
        assert!(!stream_complete(""));
    }

    #[test]
    fn later_null_does_not_mask_an_earlier_finish() {
        assert!(stream_complete(
            r#"{"finish_reason":"stop"} {"finish_reason":null}"#
        ));
    }
}
