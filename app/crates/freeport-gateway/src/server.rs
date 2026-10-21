//! 数据面：HTTP 入口 + 失败接力 + 流式转发。
//!
//! 只监听 127.0.0.1。控制面（增删厂商、读日志）不开放 HTTP，
//! 由宿主通过进程内调用拿到 —— 避免本机任意进程改配置。

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU16, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::extract::{ConnectInfo, DefaultBodyLimit, Path, Query, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Local;
use freeport_catalog::{self as catalog, Catalog};
use crate::cache;
use crate::idem;
use freeport_core::cooldown::Cooldowns;
use freeport_core::dto::*;
use freeport_core::error::{body_error, classify, ErrorClass};
use freeport_core::model::*;
use freeport_core::quota;
use freeport_core::{affinity, presets, Affinity, Plan, Strategy, Swrr};
use freeport_core::route::{cap_factor, plan_with, stick_to_front_if, Needs};
use freeport_auth::device::{self as device_flow, DeviceStart, DeviceStep};
use freeport_auth::sub;
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
use crate::upwire;
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
/// hedge 切片下限：再短就只是在给上游制造重试噪声（TLS 握手都不够）
const HEDGE_MIN_MS: u64 = 8_000;
/// hedge 切片上限：慢模型也有 20s 把首个语义块吐出来
const HEDGE_MAX_MS: u64 = 20_000;
/// feed 的检查节拍。真正是否发网络请求由 `checkMinutes` 决定，这里只是「到点看一眼」。
const FEED_TICK: Duration = Duration::from_secs(60);
/// 让目录先同步完，别和第一次 models.dev 拉取抢带宽
const FEED_WARMUP: Duration = Duration::from_secs(150);
/// 余量低于这个比例就值得在预演里点出来。
///
/// 比粘性解粘线（`affinity::UNSTICK_RATIO` = 0.10）高一档：解粘是「已经
/// 确实要换家了」，而预演是提醒「再跑几单就要换家」，得留出提前量。
const TIGHT_HEADROOM: f64 = 0.15;

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
    pub(crate) inflight: AtomicUsize,
    /// 上次落盘时间（毫秒）：模型健康每次请求都会变，不能每个请求都写盘
    last_save_ms: AtomicI64,
    on_log: RwLock<Option<LogSink>>,
    shutdown: Notify,
    /// OAuth 凭据的续期与并发去重
    broker: Broker,
    /// 进行中的设备码登录。用户点「登录」到浏览器里点完，中间隔了几十秒，
    /// 状态只能放这里。
    logins: Mutex<HashMap<String, PendingLogin>>,
    /// `Idempotency-Key` 响应缓存。内存态：重启后重放一次上游，好过把响应体落盘。
    idem: idem::IdemStore,
    /// 这次加载用的数据目录。MCP 是独立进程，得靠它找到 `control.key`。
    ///
    /// 用 std 的锁而不是 tokio 的：落盘（`save_here`）是同步调用，
    /// 而它恰恰在最热的路径上（每次请求的配额/统计都会写一次）。
    dir: std::sync::RwLock<std::path::PathBuf>,
    /// 本地控制凭据（`/v1/freeport/inspect`）。启动时生成，与数据面 key 完全分开。
    control_token: RwLock<String>,
    /// 按调用方标识的分钟窗口：`identifier -> (unix 分钟, 本分钟已用)`。
    /// 内存态即可：它限的是「刷得太快」，重启清零不会造成实际损失。
    idents: Mutex<HashMap<String, (i64, u32)>>,
    /// 精确匹配响应缓存。与 `idem` 分开：一个管重试，一个管省钱。
    resp_cache: Mutex<cache::ResponseCache>,
    /// 同一目标连续「200 但没内容」的次数。前三次不算故障（reasoning 模型吃满
    /// max_tokens 是常态），第四次才开始冷却；成功一次就清零。
    empty_streak: Mutex<HashMap<String, u32>>,
    /// 每个凭据当前在途请求数，配合 `gateway.maxInflightPerAccount` 用。
    /// 0 表示不限，默认不限——单机场景限并发只会让用户以为软件卡了。
    acct_inflight: Mutex<HashMap<String, usize>>,
}

/// 进行中的登录。两条路子的状态完全不通，只能分开存——
/// 硬塞进一个结构会让「哪个字段对哪条路有意义」变成靠注释维持的约定。
enum PendingLogin {
    /// 设备码：等用户在浏览器里输码，我们轮询
    Device {
        provider: String,
        start: DeviceStart,
        at: i64,
    },
    /// 授权码：等浏览器跳回本地回环，或用户把回调地址粘回来
    Sub {
        provider: String,
        flow: sub::LoginStart,
        at: i64,
    },
}

impl PendingLogin {
    /// 取出授权码流程的状态。设备码流程返回 `None`——
    /// 用 `Option` 而不是造一个假 flow，这样「走错接口」和「已经完成」
    /// 在类型上就分得开。
    fn sub_flow(&self) -> Option<(String, sub::LoginStart, i64)> {
        match self {
            PendingLogin::Sub {
                provider, flow, at, ..
            } => Some((provider.clone(), flow.clone(), *at)),
            PendingLogin::Device { .. } => None,
        }
    }

    fn expired(&self, now: i64) -> bool {
        match self {
            PendingLogin::Device { start, at, .. } => now - at >= start.expires_in as i64,
            // 授权码十分钟内必须用完：留太久等于给了一条长期有效的偷渡链接
            PendingLogin::Sub { at, .. } => now - at >= SUB_LOGIN_TTL,
        }
    }
}

/// 授权码的有效期。上游通常给 5 分钟，我们给 10 分钟余量让人慢慢登录。
const SUB_LOGIN_TTL: i64 = 600;

/// 设备码 → 界面 DTO。构造放在这里而不是 core：`DeviceStart` 是 auth crate 的
/// 类型，而 core 是「纯逻辑、无 IO」的内核，不该被拖进来。
fn device_start_dto(login_id: String, start: &DeviceStart) -> GwLoginStart {
    GwLoginStart {
        kind: "device".into(),
        login_id,
        expires_in: start.expires_in,
        instruction: start.instruction(),
        user_code: Some(start.user_code.clone()),
        verification_uri: Some(start.verification_uri.clone()),
        verification_uri_complete: start.verification_uri_complete.clone(),
        interval: Some(start.interval),
        url: None,
        redirect_uri: None,
        auto: true,
    }
}

/// 授权码 → 界面 DTO。`auto=false` 时文案要换成「手工粘贴」，
/// 因为那时浏览器跳回本地端口必然是打不开的，让用户以为失败了最糟。
fn pkce_start_dto(login_id: String, flow: &sub::LoginStart) -> GwLoginStart {
    let instruction = if flow.auto {
        "已在浏览器里打开授权页。登录并确认后会自动完成，不用回来点任何东西。".to_string()
    } else {
        format!(
            "请在浏览器打开授权页并登录。完成后浏览器会跳到 {}（打不开是正常的），按提示把地址栏那一整条地址复制回来。",
            flow.redirect_uri
        )
    };
    GwLoginStart {
        kind: "pkce".into(),
        login_id,
        expires_in: SUB_LOGIN_TTL as u64,
        instruction,
        user_code: None,
        verification_uri: None,
        verification_uri_complete: None,
        interval: None,
        url: Some(flow.url.clone()),
        redirect_uri: Some(flow.redirect_uri.clone()),
        auto: flow.auto,
    }
}

/// 发请求要用的凭据：OAuth 账号取 token（过期先续期），其余取 API key。
///
/// 放在锁外面调用——续期要发网络请求，拿着 `data` 读锁等网络会把整个网关卡住。
async fn credential(st: &Arc<State_>, account_id: &str) -> Option<Credential> {
    if secrets::has_oauth(account_id) {
        match st.broker.token(account_id).await {
            Ok(live) => {
                let mut cred = Credential::bearer(live.tokens.access_token.clone());
                // 有些厂商把「哪个工作区的额度」编进 token，请求里必须一起带上。
                // 头名是登录时写进凭据的，不在这里按厂商硬编码。
                if let (Some(name), Some(value)) =
                    (live.account_header.as_deref(), live.tokens.account_id.as_deref())
                {
                    if !value.is_empty() {
                        cred = cred.with_extra(name, value);
                    }
                }
                return Some(cred);
            }
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
                idem: idem::IdemStore::new(),
                dir: std::sync::RwLock::new(store::data_dir()),
                control_token: RwLock::new(String::new()),
                idents: Mutex::new(HashMap::new()),
                resp_cache: Mutex::new(cache::ResponseCache::new()),
                empty_streak: Mutex::new(HashMap::new()),
                acct_inflight: Mutex::new(HashMap::new()),
            }),
        }
    }

    pub async fn set_log_sink(&self, sink: LogSink) {
        *self.st.on_log.write().await = Some(sink);
    }

    /* ── 设备码登录（Tier C） ─────────────────────
     * 用户在自己的浏览器里登录自己的账号，我们只拿授权结果。
     * 不内嵌 client_secret，不伪造 UA，不读系统浏览器 Cookie。 */

    /// 发起一次登录。按厂商分两条路：
    ///
    /// - **订阅厂商**（`claude-sub` / `codex-sub`）走授权码 + PKCE，
    ///   回环端口能绑就自动收回调，绑不上就转人工粘贴。
    /// - **其余**走设备码：给一个短码，用户在浏览器里输。
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

        let login_id = uuid::Uuid::new_v4().simple().to_string();
        let now = chrono::Utc::now().timestamp();

        if let Some(vendor) = sub::Vendor::parse(provider_id) {
            let (port, auto) = sub::pick_port(vendor).await;
            let flow = sub::begin(&self.st.broker.endpoints().sub_cfg(vendor), port, auto);
            let dto = pkce_start_dto(login_id.clone(), &flow);
            {
                let mut map = self.st.logins.lock().unwrap_or_else(|e| e.into_inner());
                map.retain(|_, v| !v.expired(now));
                map.insert(
                    login_id,
                    PendingLogin::Sub {
                        provider: provider_id.to_string(),
                        flow,
                        at: now,
                    },
                );
            }
            tracing::info!(
                provider = provider_id,
                base = %base_url,
                port,
                auto,
                "已发起订阅授权（授权码 + PKCE）"
            );
            return Ok(dto);
        }

        let cfg = self.device_cfg();
        let start = device_flow::start(&self.st.client, &cfg)
            .await
            .map_err(|e| e.to_string())?;
        {
            let mut map = self.st.logins.lock().unwrap_or_else(|e| e.into_inner());
            map.retain(|_, v| !v.expired(now));
            map.insert(
                login_id.clone(),
                PendingLogin::Device {
                    provider: provider_id.to_string(),
                    start: start.clone(),
                    at: now,
                },
            );
        }
        tracing::info!(provider = provider_id, base = %base_url, "已发起设备码登录");
        Ok(device_start_dto(login_id, &start))
    }

    /// 轮询一次登录状态（**只对设备码有意义**）。
    ///
    /// 授权码流程没有可轮询的东西——回调是一次性的，用 [`Self::login_wait`]。
    pub async fn login_poll(&self, login_id: &str) -> GwLoginState {
        // 三层 Option：外层「有没有这条登录」、内层「是不是设备码那条路」。
        // 用 `return` 从闭包里出去是错的——那只会跳出闭包，不会跳出函数。
        let pending = {
            let map = self.st.logins.lock().unwrap_or_else(|e| e.into_inner());
            map.get(login_id).map(|p| match p {
                PendingLogin::Device {
                    provider, start, at, ..
                } => Some((provider.clone(), start.clone(), *at)),
                PendingLogin::Sub { .. } => None,
            })
        };
        let (provider_id, start, at) = match pending {
            None => return GwLoginState::error("这次登录已失效，请重新发起"),
            // 轮询接口问到授权码流程：说实话「还在等」，别假装失败
            Some(None) => return GwLoginState::pending(),
            Some(Some(x)) => x,
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
            DeviceStep::Ready(github) => match self.finish_login(&provider_id, github).await {
                Ok(state) => {
                    self.forget_login(login_id);
                    state
                }
                Err(e) => GwLoginState::error(e),
            },
        }
    }

    /// 等授权码流程的回调，等到就立刻换 token 建凭据。
    ///
    /// 一条命令把「监听 → 收回调 → 换 token → 落盘」串完，因为它中间
    /// 任何一步拆出去都得把 verifier 再传一遍，多一次泄露机会。
    /// `wait_secs` 是**这一次调用**最多阻塞多久，超时返回 pending，
    /// 界面可以再调一次（监听会重新绑同一个端口）。
    pub async fn login_wait(&self, login_id: &str, wait_secs: u64) -> GwLoginState {
        let (provider_id, flow, at) = match self.peek_sub(login_id) {
            Ok(x) => x,
            Err(state) => return state,
        };
        let _ = at;
        if !flow.auto {
            return GwLoginState {
                status: "manual".into(),
                message: Some("本机端口不可用，请把浏览器地址栏里的回调地址粘贴回来".into()),
                account_id: None,
                login: None,
            };
        }

        let cb = match sub::wait_callback(flow.port, flow.vendor.callback_path(), wait_secs).await {
            Ok(cb) => cb,
            // 超时不是错误：用户可能还没登录完，界面接着等就是了
            Err(freeport_auth::AuthError::Expired) => return GwLoginState::pending(),
            Err(e) => return GwLoginState::error(e.to_string()),
        };

        let cfg = self.st.broker.endpoints().sub_cfg(flow.vendor);
        let input = match &cb.state {
            Some(st) => format!("{}#{}", cb.code, st),
            None => cb.code.clone(),
        };
        self.complete_sub_login(login_id, &provider_id, &cfg, &flow, &input)
            .await
    }

    /// 人工粘贴的回调地址（或 `code#state`、或裸 code）。
    pub async fn login_complete(&self, login_id: &str, input: &str) -> GwLoginState {
        let (provider_id, flow, _) = match self.peek_sub(login_id) {
            Ok(x) => x,
            Err(state) => return state,
        };
        let cfg = self.st.broker.endpoints().sub_cfg(flow.vendor);
        self.complete_sub_login(login_id, &provider_id, &cfg, &flow, input)
            .await
    }

    /// 取一条授权码登录的进行中状态，顺手把「不存在 / 走错接口 / 已超时」
    /// 这三种失败都翻译成界面能直接显示的 `GwLoginState`。
    fn peek_sub(
        &self,
        login_id: &str,
    ) -> Result<(String, sub::LoginStart, i64), GwLoginState> {
        let found = {
            let map = self.st.logins.lock().unwrap_or_else(|e| e.into_inner());
            map.get(login_id).map(|p| p.sub_flow())
        };
        let Some(found) = found else {
            return Err(GwLoginState::error("这次登录已失效，请重新发起"));
        };
        let Some((provider, flow, at)) = found else {
            return Err(GwLoginState::error("这条登录要走设备码，请用轮询接口"));
        };
        if chrono::Utc::now().timestamp() - at >= SUB_LOGIN_TTL {
            self.forget_login(login_id);
            return Err(GwLoginState {
                status: "expired".into(),
                message: Some("授权超时，请重新发起登录".into()),
                account_id: None,
                login: None,
            });
        }
        Ok((provider, flow, at))
    }

    async fn complete_sub_login(
        &self,
        login_id: &str,
        provider_id: &str,
        cfg: &sub::SubCfg,
        flow: &sub::LoginStart,
        input: &str,
    ) -> GwLoginState {
        let now = chrono::Utc::now().timestamp();
        let mut tokens = match sub::exchange_code(&self.st.client, cfg, flow, input, now).await {
            Ok(t) => t,
            Err(freeport_auth::AuthError::Denied) => {
                self.forget_login(login_id);
                return GwLoginState {
                    status: "denied".into(),
                    message: Some("你在授权页取消了授权".into()),
                    account_id: None,
                    login: None,
                };
            }
            Err(e) => return GwLoginState::error(e.to_string()),
        };

        // 换 token 的响应不一定带身份。Claude 一般给 account.email_address，
        // 但企业号/新版流程里可能没有——那界面上就只剩「claude」这个笼统名字，
        // 多个号分不清。补一次 profile 查询，失败就算了，不影响能不能用。
        if tokens.account.is_none() && flow.vendor == sub::Vendor::Claude {
            tokens.account = sub::whoami_at(
                &self.st.client,
                flow.vendor.as_str(),
                &self.st.broker.endpoints().claude_profile_url,
                &tokens.access_token,
            )
            .await;
        }

        let account_header = match flow.vendor {
            // Codex 的每个请求都要带工作区 id，缺了上游直接 400
            sub::Vendor::Codex => Some("chatgpt-account-id".to_string()),
            // Claude 的 account id 只是给界面看的，不该出现在请求里
            sub::Vendor::Claude => None,
        };
        let blob = OAuthBlob {
            kind: flow.vendor.as_str().to_string(),
            token_url: Some(cfg.token_url.clone()),
            client_id: Some(cfg.client_id.clone()),
            login: tokens.account.clone(),
            account_header,
            tokens: tokens.clone(),
        };
        let key_mask = match flow.vendor {
            sub::Vendor::Claude => "cc-oauth",
            sub::Vendor::Codex => "cx-oauth",
        };
        match self
            .install_oauth_account(provider_id, tokens, blob, key_mask, flow.vendor.as_str())
            .await
        {
            Ok(state) => {
                self.forget_login(login_id);
                state
            }
            Err(e) => GwLoginState::error(e),
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

        let blob = OAuthBlob {
            kind: "copilot".into(),
            token_url: None,
            client_id: None,
            login: login.clone(),
            // Copilot 的 token 里没有「工作区」概念，不用发额外头
            account_header: None,
            tokens: tokens.clone(),
        };
        self.install_oauth_account(provider_id, tokens, blob, "gh-oauth", "github")
            .await
    }

    /// 登录成功之后共用的收尾：建凭据、存 token、拉一次模型列表。
    ///
    /// 做成一个函数是因为三条路（设备码 / Copilot / 订阅）收尾完全一样，
    /// 分开写迟早有一处忘了 `save_here`，然后就变成「登录成功了但重启就没了」。
    async fn install_oauth_account(
        &self,
        provider_id: &str,
        tokens: freeport_auth::TokenSet,
        blob: OAuthBlob,
        key_mask: &str,
        name_hint: &str,
    ) -> Result<GwLoginState, String> {
        let login = tokens.account.clone().or(blob.login.clone());
        let acc = Account {
            id: format!("acct_{}", &uuid::Uuid::new_v4().simple().to_string()[..10]),
            name: login.clone().unwrap_or_else(|| name_hint.into()),
            key_mask: key_mask.into(),
            enabled: true,
            added_at: chrono::Utc::now().to_rfc3339(),
            quota: None,
            health: None,
            purposes: Vec::new(),
        };
        Broker::save(&acc.id, &blob)?;

        // 登录成功立刻拉一次模型列表：拉不到不影响「已登录」，但要留痕
        let provider_snapshot = {
            let d = self.st.data.read().await;
            d.providers.iter().find(|p| p.id == provider_id).cloned()
        };
        let mut models = Vec::new();
        if let Some(p) = provider_snapshot {
            // 附加认证头（Codex 的工作区 id）必须一起带上，否则拉列表就被拒。
            // 用 blob 里记下的头名而不是按厂商猜：Claude 的 token 里也有 account_id
            // （那是它自己的账号 uuid），当成工作区头发出去等于给上游塞垃圾。
            let mut cred = Credential::bearer(tokens.access_token.clone());
            if let (Some(name), Some(value)) = (
                blob.account_header.as_deref(),
                tokens.account_id.as_deref().filter(|s| !s.is_empty()),
            ) {
                cred = cred.with_extra(name, value);
            }
            match providers::list_models(&self.st.client, &p, &cred).await {
                Ok(m) => models = m,
                Err(e) => tracing::warn!(
                    provider = provider_id,
                    "登录成功但拉模型失败：{}",
                    e.body.chars().take(160).collect::<String>()
                ),
            }

            let mut d = self.st.data.write().await;
            if let Some(p) = d.providers.iter_mut().find(|p| p.id == provider_id) {
                p.accounts.push(acc.clone());
                if !models.is_empty() {
                    p.models = models;
                }
            }
            save_here(&self.st, &d);
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
        self.load_from(&store::data_dir()).await;
    }

    /// 从指定目录载入。抽出来是为了让 MCP / 单测能拿一份受控的配置，
    /// 而不必去动进程级的 `FREEPORT_DATA_DIR`（那会让同进程的测试互相踩）。
    pub async fn load_from(&self, dir: &std::path::Path) {
        *self.st.dir.write().unwrap_or_else(|e| e.into_inner()) = dir.to_path_buf();
        let mut data = store::load_from(dir);
        let cat = catalog::load();

        // 目录摘要回写到配置，UI 才能显示「上次同步」
        data.catalog = cat.meta.clone();

        // 预算按天滚动（跨天自动清零）
        let today = Local::now().format("%Y-%m-%d").to_string();
        let budget_now = budget_of(&cat, &data);
        budget::roll(&mut data.patrol, &today, budget_now);

        // 老版本只有一个 authToken：迁成 name="default" 的一条，老客户端不用改配置
        if keys::migrate_legacy(&mut data) {
            store::save_to(dir, &data);
            tracing::info!("已把全局 authToken 迁移为虚拟 key `default`");
        }

        // 「一厂商一 key」→「一厂商多凭据」。老凭据沿用 provider id，密钥不用搬家。
        if secrets::migrate_accounts(&mut data) {
            store::save_to(dir, &data);
            tracing::info!("已把厂商密钥升级为凭据视图");
        }

        *self.st.data.write().await = data;
        *self.st.catalog.write().await = cat;
    }

    /// 改监听地址。**只给无界面宿主用**（容器里必须绑 `0.0.0.0` 才能被
    /// publish 出去，再靠外层 `-p 127.0.0.1:8787:8787` 收回来）。
    ///
    /// 刻意不放进 `ConfigPatch`：那会让界面上多出一个「绑到哪」的输入框，
    /// 而「网关只对内」是这条产品线的边界，不该变成一个可点错的开关。
    /// 谁能调这个方法，谁就负责判断地址合不合适。
    pub async fn set_bind(&self, host: &str, port: u16) {
        let mut d = self.st.data.write().await;
        if !host.trim().is_empty() {
            d.gateway.host = host.trim().to_string();
        }
        if port > 0 {
            d.gateway.port = port;
        }
        save_here(&self.st, &d);
    }

    /// 当前配置里的监听地址（`host`, `port`）。start 之前也能问。
    pub async fn bind_of(&self) -> (String, u16) {
        let d = self.st.data.read().await;
        (d.gateway.host.clone(), d.gateway.port)
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
            .route("/v1/freeport/inspect", get(inspect))
            .route("/v1/models", get(list_models))
            .route("/v1/models/:id", get(get_model))
            .route("/v1/route", get(route_debug))
            .route("/v1/chat/completions", post(chat))
            .route("/v1/completions", post(completions_entry))
            .route("/v1/embeddings", post(embeddings_entry))
            .route("/v1/responses", post(responses_entry))
            .route("/v1/audio/transcriptions", post(crate::exts::asr_entry))
            .route("/v1/audio/speech", post(crate::exts::speech_entry))
            .route("/v1/images/generations", post(crate::exts::image_entry))
            .route("/v1/videos/generations", post(crate::exts::video_entry))
            .route("/v1/rerank", post(crate::exts::rerank_entry))
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
        // 控制凭据：只在真正起服务时准备。MCP 进程只读这份文件，不自己生成——
        // 否则两个进程各写一把，谁都不认对方，表现为「MCP 永远连不上」。
        let dir = self.st.dir.read().unwrap_or_else(|e| e.into_inner()).clone();
        *self.st.control_token.write().await = crate::control::ensure_token(&dir);

        let st = self.st.clone();
        tokio::spawn(async move {
            let shutdown = st.clone();
            let _ = axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>())
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
        // 扩展目录 materialize + sidecar 空闲清扫。没用到扩展就零开销。
        freeport_ext::install::materialize_builtin(&freeport_ext::install::extensions_dir());
        freeport_ext::start_reaper(freeport_ext::shared());
        Ok(actual)
    }

    pub async fn stop(&self) {
        self.st.shutdown.notify_waiters();
        self.st.running.store(false, Ordering::SeqCst);
        self.st.bound_port.store(0, Ordering::SeqCst);
        // sidecar 是独立进程，宿主退出时显式带走，不留孤儿
        crate::exts::shutdown_all().await;
    }

    /* ── 控制面 ───────────────────────────────────── */

    /// 配置里写的监听地址（不是实际绑定的端口）。
    ///
    /// 给「另一个进程」用：MCP server 是一个独立进程，它看不到这里的内存状态，
    /// 但可以拿这个地址去探一下网关在不在。
    pub async fn configured_addr(&self) -> (String, u16) {
        let d = self.st.data.read().await;
        (d.gateway.host.clone(), d.gateway.port)
    }

    /// 这次加载用的数据目录。MCP 靠它去读 `control.key`。
    pub async fn data_dir(&self) -> std::path::PathBuf {
        self.st.dir.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// 本地控制凭据。没起过服务就是空串（MCP 那边读文件即可）。
    pub async fn control_token(&self) -> String {
        self.st.control_token.read().await.clone()
    }

    /// 问一次「那边的网关现在到底在做什么」。
    ///
    /// 返回 `None` 就说明**连不上**（没起、换了端口、或凭据还没生成）。
    /// 调用方必须把 `None` 与「在线但没日志」分开处理——把前者说成后者，
    /// 是自省工具最常见的撒谎方式。
    pub async fn inspect_live(&self) -> Option<Value> {
        let (host, port) = self.configured_addr().await;
        let port = match self.st.bound_port.load(Ordering::SeqCst) {
            0 => port,
            p => p,
        };
        if port == 0 {
            return None;
        }
        let host = match host.trim() {
            "" | "0.0.0.0" | "::" => "127.0.0.1".to_string(),
            h => h.to_string(),
        };
        let token = self.control_token().await;
        let token = if token.is_empty() {
            crate::control::read_token(&self.data_dir().await).unwrap_or_default()
        } else {
            token
        };
        if token.is_empty() {
            return None;
        }
        let res = self
            .st
            .client
            .get(format!("http://{host}:{port}/v1/freeport/inspect?limit=200"))
            .header(crate::control::HEADER, token)
            .timeout(Duration::from_millis(1_500))
            .send()
            .await
            .ok()?;
        if !res.status().is_success() {
            return None;
        }
        res.json::<Value>().await.ok()
    }

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
            compress: d.gateway.compress.enabled,
            compress_max_tool_chars: d.gateway.compress.max_tool_chars,
            compress_smart: d.gateway.compress.smart_filters,
            identifier_limit: d.gateway.identifier_limit,
            cache: d.gateway.cache.enabled,
            cache_ttl_secs: d.gateway.cache.ttl_secs,
            cooldowns: self.st.cooldowns.snapshot(),
            error: None,
        }
    }

    /// 查一条凭据的**官方**订阅额度。
    ///
    /// 和账本不是一回事：账本记的是「我们发出去多少」，这里问的是
    /// 「厂商认为你还剩多少」。两个数字可以不一致——账本漏记的部分
    /// 就是差额，而用户要的是后者。
    ///
    /// 永远返回一个可渲染的 `GwUsage`：查不到就说为什么，不抛错。
    pub async fn usage(&self, account_id: &str) -> freeport_core::dto::GwUsage {
        let now = chrono::Utc::now().to_rfc3339();
        let live = match self.st.broker.token(account_id).await {
            Ok(l) => l,
            Err(e) => {
                return freeport_core::dto::GwUsage {
                    kind: String::new(),
                    ok: false,
                    windows: Vec::new(),
                    credits: None,
                    note: Some(e),
                    checked_at: now,
                }
            }
        };
        crate::usage::fetch(
            &self.st.client,
            &self.st.broker.endpoints(),
            &live.kind,
            &live.tokens.access_token,
            live.tokens.account_id.as_deref(),
        )
        .await
    }

    /// 每日摘要：一行结论 + 最多三条变更 + 需要处理的事（§3.5.1）。
    /// 纯内存聚合，不发任何上游请求，可以随便刷。
    pub async fn digest(&self) -> freeport_core::digest::Digest {
        // 池战绩只能从日志算，而日志只在内存里。本进程没跑网关时（MCP 进程、
        // 或第二个实例）本地环是空的——直接算会得出「今天没有任何请求」，
        // 而空数组最容易被读成这个意思。所以空的时候先问活着的那个网关。
        let mut logs = self.logs().await;
        if logs.is_empty() {
            if let Some(l) = self.live_logs().await {
                logs = l;
            }
        }
        let d = self.st.data.read().await;
        freeport_core::digest::build_with_logs(&d, freeport_core::cooldown::now_ms(), &logs)
    }

    /// 从正在运行的网关取实时日志（同进程时就是它自己）。连不上返回 `None`。
    async fn live_logs(&self) -> Option<Vec<LogEntry>> {
        let v = self.inspect_live().await?;
        let arr = v.get("logs")?.clone();
        serde_json::from_value(arr).ok()
    }

    /// 近 `hours` 小时观测：p50/p95、token、按目录价折算的美元、按厂商/用途拆。
    ///
    /// 与 `/v1/freeport/inspect` 的 `window` 段同源（`observed_of`），
    /// 日志取法与 `digest` 一致：本进程环空时问活着的那个网关，
    /// 否则第二个实例会一本正经地报告「今天一条请求都没有」。
    pub async fn observed(&self, hours: i64) -> GwObserved {
        let mut logs = self.logs().await;
        if logs.is_empty() {
            if let Some(l) = self.live_logs().await {
                logs = l;
            }
        }
        // 上限一个月：日志环只有 300 条，再大的窗口也只是回到「全量」
        let since = freeport_core::cooldown::now_ms() - hours.clamp(1, 24 * 30) * 3_600_000;
        observed_of(&self.st, &logs, since).await
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
        let mut k = keys::new_key(name, &secret, input.rpm_limit, input.daily_limit);
        k.daily_token_limit = input.daily_token_limit;
        k.allowed_models = input
            .allowed_models
            .into_iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        k.expires_at = input
            .expires_at
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let view = GwKey::from(&k);
        {
            let mut d = self.st.data.write().await;
            if d.api_keys.iter().any(|x| x.name == name) {
                return Err(format!("已存在名为 {name} 的 key"));
            }
            d.api_keys.push(k);
            save_here(&self.st, &d);
        }
        Ok(KeyCreated { key: view, secret })
    }

    pub async fn delete_key(&self, id: &str) -> Vec<GwKey> {
        let mut d = self.st.data.write().await;
        d.api_keys.retain(|k| k.id != id);
        save_here(&self.st, &d);
        d.api_keys.iter().map(GwKey::from).collect()
    }

    pub async fn toggle_key(&self, id: &str, enabled: bool) -> Result<Vec<GwKey>, String> {
        let mut d = self.st.data.write().await;
        let Some(k) = d.api_keys.iter_mut().find(|k| k.id == id) else {
            return Err(format!("找不到 key {id}"));
        };
        k.enabled = enabled;
        save_here(&self.st, &d);
        Ok(d.api_keys.iter().map(GwKey::from).collect())
    }

    pub async fn routing(&self) -> GwRouting {
        let d = self.st.data.read().await;
        GwRouting {
            strategy: d.routing.strategy.clone(),
            chain: d.routing.chain.clone(),
            chains: d.routing.chains.clone(),
            purpose_chain: d.routing.purpose_chain.clone(),
            weights: d.routing.weights,
            cooldowns: self.st.cooldowns.snapshot(),
        }
    }

    /// 「可接入」预设 = 内置硬编码表 + 目录里的 Tier A 免费厂商。
    /// 内置表只是离线兜底，目录（基线 + models.dev + feed）才是主来源。
    pub async fn presets(&self) -> Vec<presets::ProviderPreset> {
        let mut out = presets::all();
        let cat = self.st.catalog.read().await;
        for p in cat.providers.iter() {
            if p.tier != "A" || !p.free_tier {
                continue;
            }
            if out.iter().any(|x| x.id == p.id) {
                continue;
            }
            let Some(api) = &p.api else { continue };
            // baseUrl 带 {占位符}（如 Cloudflare 的 account_id）时接入弹窗改不了 baseUrl，先不进列表
            if api.contains('{') {
                continue;
            }
            out.push(presets::ProviderPreset {
                id: p.id.clone(),
                name: p.name.clone(),
                base_url: api.clone(),
                tier: p.tier.clone(),
                homepage: p.docs.clone(),
                note: p.free_note.clone(),
            });
        }
        out
    }

    /// 预演一条链路：不推进 SWRR、不写粘性表。
    /// GUI 的「路由」页每刷新一次都会调它，绝不能带副作用。
    ///
    /// 池维度取 chat：这个探针回答的是「一条普通请求会怎么走」，
    /// 要按别的用途看，用 [`Gateway::explain_route_for`] 或 `GET /v1/route?purpose=`。
    pub async fn explain_route(&self, model: &str, session: &str) -> serde_json::Value {
        self.explain_route_for(model, session, Purpose::Chat).await
    }

    /// 按指定用途预演。分池出问题时，先看 `purpose` 判得对不对，再看链。
    pub async fn explain_route_for(
        &self,
        model: &str,
        session: &str,
        purpose: Purpose,
    ) -> serde_json::Value {
        explain_route_json(&self.st, model, session, false, Needs::default(), purpose, None).await
    }

    /// 带成本估算的预演（MCP 的 `freeport_route?tokens=` 走这条）。
    ///
    /// `cost` 是这单预计吃掉多少 token。给了才判「装不装得下」——没给就
    /// 保持 `null`，因为拿一个猜出来的数字去报红，比不报更糟。
    pub async fn explain_route_cost(
        &self,
        model: &str,
        session: &str,
        purpose: Purpose,
        cost: Option<f64>,
    ) -> serde_json::Value {
        explain_route_json(&self.st, model, session, false, Needs::default(), purpose, cost).await
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
            purposes: Vec::new(),
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
            proxy: None,
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
            match providers::list_models(&providers::client_of(&provider), &provider, &Credential::bearer(key.clone())).await {
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
        // 自动入链只取非付费的：auto 只花免费额度，把付费模型塞进链只会多一条
        // 「被拦下」的记录，还占掉原本留给免费模型的两个位置。
        let chain_entries: Vec<String> = provider
            .models
            .iter()
            .filter(|m| m.free != Some(false))
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
        save_here(&self.st, &d);
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
        let models = providers::list_models(&providers::client_of(&provider), &provider, &Credential::bearer(key.clone())).await;
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
            purposes: input.purposes,
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
            save_here(&self.st, &d);
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
        let probed = providers::list_models(&providers::client_of(&provider), &provider, &cred_obj).await;
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
            purposes: Vec::new(),
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
            save_here(&self.st, &d);
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
            save_here(&self.st, &d);
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
            save_here(&self.st, &d);
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
            if let Some(ps) = patch.purposes {
                // 去重但保留用户勾选顺序：界面上的标签就按这个顺序出现，
                // 每次保存都重排一遍会让人以为自己点错了。
                let mut seen = Vec::with_capacity(ps.len());
                for p in ps {
                    if !seen.contains(&p) {
                        seen.push(p);
                    }
                }
                a.purposes = seen;
            }
            save_here(&self.st, &d);
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
        save_here(&self.st, &d);
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
            save_here(&self.st, &d);
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
            save_here(&self.st, &d);
        }
        Ok(self.providers().await)
    }

    /// 设厂商级代理。空串等于清掉（回落到环境变量）。
    ///
    /// 只影响这一个厂商的上游请求（聊天、拉模型、探针），不影响目录同步——
    /// 目录是我们自己的公共资源，走系统链路。
    pub async fn set_provider_proxy(
        &self,
        id: &str,
        proxy: Option<String>,
    ) -> Result<Vec<GwProvider>, String> {
        let clean = proxy
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        if let Some(url) = clean.as_deref() {
            // 早失败：地址写错就让用户当场看到，而不是等第一次请求超时
            if reqwest::Proxy::all(url).is_err() {
                return Err(format!("代理地址不合法：{url}"));
            }
        }
        {
            let mut d = self.st.data.write().await;
            let p = d
                .providers
                .iter_mut()
                .find(|p| p.id == id)
                .ok_or_else(|| "provider 不存在".to_string())?;
            p.proxy = clean;
            save_here(&self.st, &d);
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
        let result = providers::list_models(&providers::client_of(&p), &p, &cred).await;
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
        save_here(&self.st, &d);
        out
    }

    pub fn reveal_key(&self, id: &str) -> String {
        secrets::get_key(id).unwrap_or_default()
    }

    pub async fn set_routing(&self, patch: RoutingPatch) -> GwRouting {
        {
            let mut d = self.st.data.write().await;
            if let Some(s) = patch.strategy {
                // 只收认识的模式：写进一个拼错的词会让排序静默退回 balanced，
                // 而用户以为自己的选择生效了。
                d.routing.strategy = Strategy::parse(&s).as_str().to_string();
            }
            if let Some(c) = patch.chain {
                d.routing.chain = c;
            }
            if let Some(c) = patch.chains {
                // 空 profile 名与空链都丢掉：它们只会让选择器多出一堆点了没用的项
                d.routing.chains = c
                    .into_iter()
                    .filter(|(k, v)| !k.trim().is_empty() && !v.is_empty())
                    .collect();
            }
            if let Some(m) = patch.purpose_chain {
                d.routing.purpose_chain = m
                    .into_iter()
                    .filter(|(_, v)| !v.trim().is_empty())
                    .collect();
            }
            if let Some(mut w) = patch.weights {
                // 夹到 0..=100：界面上是滑杆，但 IPC 是明文接口。
                // 三个都为 0 等于「不分先后」，那样 SWRR 会退化成一模一样的轮转，
                // 而用户以为自己调了个策略——所以兜一个 100 在可靠性上。
                w.reliability = w.reliability.min(100);
                w.speed = w.speed.min(100);
                w.intelligence = w.intelligence.min(100);
                if w.reliability as u32 + w.speed as u32 + w.intelligence as u32 == 0 {
                    w.reliability = 100;
                }
                d.routing.weights = w;
            }
            save_here(&self.st, &d);
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
            if let Some(v) = patch.compress {
                d.gateway.compress.enabled = v;
            }
            if let Some(n) = patch.compress_max_tool_chars {
                // 太低会把正常输出也剪掉，太高等于没开。1000..200000 是个能用的窗口。
                if (1_000..=200_000).contains(&n) {
                    d.gateway.compress.max_tool_chars = n;
                    // 留头留尾按阈值等比缩放，用户只填一个数就够了
                    d.gateway.compress.keep_head = n * 3 / 10;
                    d.gateway.compress.keep_tail = n * 15 / 100;
                }
            }
            if let Some(v) = patch.identifier_limit {
                // 上限给个天花板：把它写成一百万等于关掉，但界面上会显示「已开启」
                d.gateway.identifier_limit = v.min(10_000);
            }
            if let Some(v) = patch.cache {
                d.gateway.cache.enabled = v;
            }
            if let Some(secs) = patch.cache_ttl_secs {
                // 上限一天：再长就不是「复用答案」而是「喂陈旧信息」了
                d.gateway.cache.ttl_secs = secs.clamp(10, 86_400);
            }
            save_here(&self.st, &d);
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
                    save_here(&self.st, &d);
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
        save_here(&self.st, &d);
    }

    /// 目录更新后，把已有厂商的模型重新补一遍元数据。
    async fn re_enrich(&self) {
        let cat = self.st.catalog.read().await;
        let mut d = self.st.data.write().await;
        for p in d.providers.iter_mut() {
            cat.enrich(&p.id, &mut p.models);
        }
        save_here(&self.st, &d);
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
            served_feats: f.served_feats.clone(),
            feats_skipped: f.feats_skipped.clone(),
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
            save_here(&self.st, &d);
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
            // 身份取当前许可证现算：宽限期仍按 Pro 报，过期立刻退回 free。
            // 这里只决定「请求带什么」，不决定「给不给拉」。
            let id = license::identity(&d);
            // 轮询节奏按档位：免费档问得再勤，服务端给的还是月度快照
            if !manual && !f.due_for(chrono::Utc::now().timestamp_millis(), id.tier) {
                return self.feed().await;
            }
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
                save_here(&self.st, &d);
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
                save_here(&self.st, &d);
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
                    // 字段包同理：服务端按许可证的 feat 名单挑了哪几项，如实记下来
                    f.served_feats = a.feats.clone();
                    f.feats_skipped = a.feats_skipped.clone();
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
                    save_here(&self.st, &d);
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
                save_here(&self.st, &d);
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

        let probe_client = providers::client_of(&p);
        let out = probe::liveness(&probe_client, &p.base_url, model, auth).await;
        let measured = if deep && out.ok {
            Some(probe::capability(&probe_client, &p.base_url, model, auth).await.caps)
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
        save_here(&self.st, &d);

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
            save_here(&self.st, &d);
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
            // 巡检挑第一条有 key 且接巡检这活儿的凭据：模型健康是厂商级事实，
            // 不必每条 key 都探。标成 coding-only 的号不该被探针烧额度。
            let picked = {
                let d = self.st.data.read().await;
                d.providers.iter().find(|p| p.id == item.provider).and_then(|p| {
                    let base = p.base_url.clone();
                    let proxy = p.proxy.clone();
                    p.live_accounts()
                        .find(|a| a.admits(Purpose::Probe) && secrets::has_credential(&a.id))
                        .map(|a| (base, a.id.clone(), a.quota, proxy))
                })
            };
            let Some((base, account_id, account_quota, proxy)) = picked else {
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
                // 模型级窗口分列记进 used_probe：探针打的是哪一个模型，哪本账就该动
                d.ledger
                    .reserve_model_now(&account_id, &item.model, PROBE_RESERVE, true);
            }

            let patrol_client = match proxy.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                Some(u) => providers::client_for_proxy(u),
                None => self.st.client.clone(),
            };
            let out = probe::liveness(&patrol_client, &base, &item.model, auth).await;
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
                // 探针成功同时证明了厂商可达、凭据能用、模型有响应：
                // 把「猜出来的」冷却提前解除。上游明说的重置时刻不动。
                let n = self.st.cooldowns.recover_after_probe(
                    &item.provider,
                    &account_id,
                    Some(&item.model),
                    freeport_core::cooldown::now_ms(),
                );
                if n > 0 {
                    tracing::info!(
                        provider = %item.provider,
                        model = %item.model,
                        cleared = n,
                        "probe cleared heuristic cooldown early"
                    );
                }
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
                // 每日全量体检之后把那一行结论写进日志：用户翻日志能对上「今天变了吗」
                let dg = freeport_core::digest::build(&d, chrono::Utc::now().timestamp_millis());
                tracing::info!(digest = %dg.headline, "daily digest");
            }
            save_here(&self.st, &d);
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

        // unavailable 满 7 天的转 retired：移出路由与巡检队列，保留历史
        let newly_retired = {
            let mut d = self.st.data.write().await;
            let keys = patrol_calc::retire_pass(&mut d, chrono::Utc::now().timestamp_millis());
            if !keys.is_empty() {
                save_here(&self.st, &d);
            }
            keys
        };
        for k in &newly_retired {
            tracing::info!(model = %k, "model retired after 7 days unavailable");
        }

        let sweep = {
            let d = self.st.data.read().await;
            // ±90 分钟抖动：几百台机器同一个 03:00 打上游，等于自己造尖峰
            let jitter = patrol_calc::sweep_jitter_min(&d.install_id);
            patrol_calc::sweep_due_at(&d.patrol, Local::now(), jitter)
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
pub(crate) struct Inflight<'a>(&'a AtomicUsize);

impl<'a> Inflight<'a> {
    pub(crate) fn new(c: &'a AtomicUsize) -> Self {
        c.fetch_add(1, Ordering::SeqCst);
        Self(c)
    }
}

impl Drop for Inflight<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// 把配置写回**本实例的数据目录**，而不是进程级的 `store::data_dir()`。
///
/// 为什么非这样不可：`store::save()` 写的是 `FREEPORT_DATA_DIR` 或系统默认目录。
/// 一个用 `load_from(dir)` 载入的实例（测试、将来的多配置档/便携模式）只要走到
/// 任何落盘路径——配额记账、统计、健康度回写都会——就会把那份数据写进**真实
/// 用户目录**。真机上的表现是「跑一次测试，用户的厂商和 key 全没了」，而且
/// 不报错：保存成功，只是存错了地方。
pub(crate) fn save_here(st: &Arc<State_>, data: &AppData) -> bool {
    let dir = st.dir.read().unwrap_or_else(|e| e.into_inner()).clone();
    store::save_to(&dir, data)
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
    save_here(st, &d);
}

pub(crate) fn oai_error(message: &str, kind: &str, code: &str, extra: Value) -> Value {
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

#[derive(serde::Deserialize)]
struct InspectQuery {
    limit: Option<usize>,
}

/// 本地自省：把**只有网关进程知道**的事实时说出来。
///
/// MCP server 是独立进程，它能读磁盘上的配置、健康与账本，但读不到三样东西：
/// 实时日志、内存里的冷却、此刻的在途数。少了这三样，「为什么刚才失败了」
/// 就只能靠猜——而猜错比不说更糟。
///
/// 两道闸：只接受回环来源，且要带 `x-freeport-control`。凭据在数据目录的
/// `control.key`（0600），**不是** `authToken`——那把能发请求烧额度，
/// 为了看日志把它交出去是不划算的。
async fn inspect(
    State(st): State<Arc<State_>>,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    Query(q): Query<InspectQuery>,
    headers: HeaderMap,
) -> Response {
    if !peer.ip().is_loopback() {
        return (
            StatusCode::FORBIDDEN,
            Json(oai_error(
                "本地控制接口只接受本机回环访问",
                "invalid_request_error",
                "not_loopback",
                json!({ "peer": peer.ip().to_string() }),
            )),
        )
            .into_response();
    }

    let expected = st.control_token.read().await.clone();
    let got = headers
        .get(crate::control::HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .trim();
    if !crate::control::verify(&expected, got) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(oai_error(
                "缺少本地控制凭据：需带 x-freeport-control，值见数据目录下的 control.key",
                "invalid_request_error",
                "control_unauthorized",
                json!({}),
            )),
        )
            .into_response();
    }

    let limit = q.limit.unwrap_or(200).clamp(1, 1000);
    let (log_total, logs) = {
        let buf = st.logs.read().await;
        // 日志环形缓冲是「新的在后」。截最近 N 条时反过来取，再翻回时间序，
        // 这样调用方读到的仍是「按发生顺序」。
        let start = buf.len().saturating_sub(limit);
        (buf.len(), buf[start..].to_vec())
    };
    // 观测口径固定在 24h：与下面 `stats.requests` 的累计计数同名不同源，
    // 但「最近一天」是用户唯一能自己核对的时间窗。
    let since = freeport_core::cooldown::now_ms() - 24 * 3_600_000;
    let observed = observed_of(&st, &logs, since).await;
    let d = st.data.read().await;
    let uptime = st
        .started_at
        .read()
        .await
        .map(|t| t.elapsed().as_secs())
        .unwrap_or(0);

    Json(json!({
        "ok": true,
        "running": st.running.load(Ordering::SeqCst),
        "paused": st.paused.load(Ordering::SeqCst),
        "uptimeSec": uptime,
        "inflight": st.inflight.load(Ordering::SeqCst),
        "port": match st.bound_port.load(Ordering::SeqCst) {
            0 => None,
            p => Some(p),
        },
        "configuredPort": d.gateway.port,
        "host": d.gateway.host,
        "budgetMs": d.gateway.request_budget_ms,
        "logTotal": log_total,
        "logs": logs,
        "cooldowns": st.cooldowns.snapshot(),
        "stats": {
            "requests24h": d.stats.requests,
            "tokensIn": d.stats.tokens_in,
            "tokensOut": d.stats.tokens_out,
            "errors": d.stats.errors,
        },
        // 下面这块来自内存日志环（≤300 条），是「最近一天」的样本，
        // 与上面的累计计数不是一个口径：一个是全生命周期，一个是近期样本。
        "window": observed,
    }))
    .into_response()
}

/// 观测口径的唯一实现。`inspect` 与桌面端 `gw_window` 都走这里——
/// 两边各写一遍迟早会分叉，然后用户就会看到两个不同的 p95。
async fn observed_of(st: &Arc<State_>, logs: &[LogEntry], since: i64) -> GwObserved {
    let summary = {
        let cat = st.catalog.read().await;
        freeport_core::observe::summarize(logs, since, |p, m| {
            cat.find_model(p, m).map(|x| (x.cost_in, x.cost_out))
        })
    };
    let buckets = |v: Vec<(String, usize, u64)>| -> Vec<GwBucket> {
        v.into_iter()
            .map(|(key, requests, avg_ok_ms)| GwBucket {
                key,
                requests,
                avg_ok_ms,
            })
            .collect()
    };
    GwObserved {
        since_ms: since,
        requests: summary.requests,
        ok: summary.ok,
        errors: summary.errors,
        p50_ms: summary.p50_ms,
        p95_ms: summary.p95_ms,
        max_ms: summary.max_ms,
        tokens_in: summary.tokens_in,
        tokens_out: summary.tokens_out,
        // 六位小数足够：目录价本身就是每百万 token 的美元数
        cost_usd: (summary.cost_usd * 1e6).round() / 1e6,
        cost_unknown: summary.cost_unknown,
        by_provider: buckets(freeport_core::observe::bucket(logs, since, |e| {
            e.provider.as_deref()
        })),
        by_purpose: buckets(freeport_core::observe::bucket(logs, since, |e| {
            Some(e.purpose.as_str())
        })),
    }
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
///
/// 三种行：虚拟项（`auto`）、**模型组**（同名模型在多家都有）、单厂商模型。
/// 组的 `display_name` 要写明「几家」，否则用户看不出它与单厂商那条的区别。
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
    if a.is_group() {
        return json!({
            "id": a.alias,
            "object": "model",
            "type": "model",
            "display_name": format!("Freeport · {} · {} 家可用", a.model, a.members.len()),
            "created_at": "1970-01-01T00:00:00Z",
            "owned_by": "freeport",
            "created": 0,
            // `group` + `providers` 是给界面看的：这条名字背后是几条通道，
            // 而不是某一家。客户端不需要懂它们的含义。
            "freeport": {
                "virtual": true,
                "group": true,
                "providers": a.members,
                "upstreamModel": a.model,
            },
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

/// 这次请求该进哪个账号池。
///
/// 三个信号按可靠性排序，判定的顺序见 [`Purpose::infer`]：
/// - `x-freeport-purpose` 是调用方的明确声明，给需要精确控制的脚本用；
/// - 请求里带图是客观事实，不需要任何配置；
/// - Agent 身份来自我们自己注入的 `agent:<id>` key —— 这是最有价值的一条，
///   因为它让「IDE 里的 Claude Code」和「手动 curl」在**零配置**下就分开了。
///
/// 认不出来就 chat：绝大多数未标注的请求确实是普通对话。
///
/// 返回依据（`PurposeSource`）是为了写进日志：池过滤是静默的，
/// 不记依据的话，「为什么没走那把更快的 key」只能靠猜。
fn purpose_of(headers: &HeaderMap, needs: Needs, key_name: Option<&str>) -> (Purpose, PurposeSource) {
    let declared = headers
        .get("x-freeport-purpose")
        .and_then(|v| v.to_str().ok())
        .and_then(Purpose::parse);
    if declared.is_none() {
        // 写错的值不该让请求失败，但要留痕：否则用户会以为标签生效了
        if let Some(raw) = headers
            .get("x-freeport-purpose")
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            tracing::warn!(value = %raw, "x-freeport-purpose 取值无法识别，已改用自动判定");
        }
    }
    let from_agent = key_name.map(|n| n.starts_with("agent:")).unwrap_or(false);
    Purpose::infer_with_source(declared, needs.vision, from_agent)
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
    purpose: Purpose,
) -> (Plan, bool) {
    build_plan_opts(st, d, requested, session, true, needs, purpose)
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
    purpose: Purpose,
) -> (Plan, bool) {
    // 对外露的是 `claude-freeport-*`，进来先脱壳，否则裸 id 找不到任何厂商
    let requested = freeport_core::route::resolve_alias(requested);
    let mut p = plan_with(d, requested, &st.cooldowns, &|a: &Account| {
        secrets::has_credential(&a.id)
    }, &d.ledger, needs, purpose);

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
        let strategy = Strategy::parse(&d.routing.strategy);
        let cands = std::mem::take(&mut p.candidates);
        p.candidates = {
            let mut swrr = st.swrr.lock().unwrap_or_else(|e| e.into_inner());
            if commit {
                swrr.order_with(strategy, cands, d, &d.ledger, now)
            } else {
                // 副本推进：既不写回，也不影响下一次真实请求
                let mut ghost = swrr.clone();
                ghost.order_with(strategy, cands, d, &d.ledger, now)
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
    // 调试时没有请求体可看，所以 `?caps=vision` 同时当作「这是一条图片请求」，
    // 否则想验证 vision 池的人还得另外拼个 header。
    let purpose = q
        .get("purpose")
        .and_then(|v| Purpose::parse(v))
        .unwrap_or_else(|| Purpose::infer(None, needs.vision, false));
    // 这单大概要吃多少 token。给了才判「账面上有额度、实际装不下」——
    // 不给就留空，别拿一个猜的数字去报红。
    let cost = q
        .get("tokens")
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v > 0.0 && *v <= 10_000_000.0);
    // 调试接口用预演模式：curl 一下不该改变下一单真实请求的落点
    Json(explain_route_json(&st, &requested, &session, false, needs, purpose, cost).await)
        .into_response()
}

/// 把一条链路解释成 JSON。`/v1/route` 与 GUI 的「路由」页共用这一份，
/// 保证界面看到的顺序就是 curl 出来的顺序。
///
/// `cost` 只是「这单要吃多少 token」的估算，`None` 表示不判装不装得下。
async fn explain_route_json(
    st: &Arc<State_>,
    model: &str,
    session: &str,
    commit: bool,
    needs: Needs,
    purpose: Purpose,
    cost: Option<f64>,
) -> serde_json::Value {
    let d = st.data.read().await;
    let now = freeport_core::cooldown::now_ms();
    let (p, sticky) = build_plan_opts(st, &d, model, session, commit, needs, purpose);
    let weights = d.routing.weights.active();

    let hops = p
        .candidates
        .iter()
        .map(|c| {
            let prov = &d.providers[c.provider];
            let h = d.model_health.get(&format!("{}:{}", prov.id, c.model_id));
            let (tier, headroom, health) =
                freeport_core::weight_factors(prov, &c.account, &d.ledger, h, now);
            let r3 = |x: f64| (x * 1000.0).round() / 1000.0;
            let base =
                freeport_core::weight_of_model(prov, &c.account, &c.model_id, &d.ledger, h, now, weights);
            // 启用三维权重后，第三个因子被合成评分顶替。两个都发：
            // 界面要能同时说清「原始健康」与「实际使用的评分」。
            let blend = weights.map(|w| {
                freeport_core::blend_score(
                    w,
                    h,
                    prov.models.iter().find(|m| m.id == c.model_id),
                    now,
                )
            });
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
                    "blend": blend.map(r3),
                    // 长期可靠性后验；样本不足时是 null，界面据此说明「样本不足」
                    "reliability": h.and_then(|x| x.reliability(now)).map(r3),
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
        // 这一次请求被归到哪个池。分池出问题时第一个要看的就是它：
        // 用户以为是 coding，网关判成 chat，链上自然全是另一批账号。
        "purpose": purpose.as_str(),
        "needs": { "vision": needs.vision, "tools": needs.tools, "json": needs.json },
        "sticky": sticky,
        "strategy": d.routing.strategy,
        "budgetMs": d.gateway.request_budget_ms,
        "chain": hops,
        "rejected": rejected,
        "cooldowns": st.cooldowns.snapshot(),
        "pool": pool_forecast(st, &d, purpose, &p.candidates, cost, now).await,
    })
}

/// 「这条请求现在会落在谁身上、那个池还够不够」——一句话结论，附证据。
///
/// 光有 `chain` 回答不了用户真正会问的两件事：
///
/// 1. **这个用途到底有几个号接着。** 勾错标签是最常见的现场：链路全空，
///    而「厂商」页一片绿，用户只会觉得网关坏了。
/// 2. **排第一那个号余量够不够这单跑完。** Gate 只判「窗口有没有耗尽」，
///    日窗口还剩 300 token 时一条 8k 的请求照样放行，然后在上面撞 429。
///
/// 池战绩复用 `digest::pool_stat`，不另算一份——否则摘要说 78%、预演说
/// 100%，用户没法判断该信哪个。
async fn pool_forecast(
    st: &Arc<State_>,
    d: &AppData,
    purpose: Purpose,
    chain: &[freeport_core::Candidate],
    cost: Option<f64>,
    now: i64,
) -> serde_json::Value {
    let mut total = 0usize;
    let mut admits = 0usize;
    for p in d.providers.iter().filter(|p| p.enabled) {
        for a in p.live_accounts() {
            total += 1;
            if a.admits(purpose) {
                admits += 1;
            }
        }
    }

    let recent = {
        let logs = st.logs.read().await;
        freeport_core::digest::pool_stat(&logs, purpose, now)
    };

    let r3 = |x: f64| (x * 1000.0).round() / 1000.0;
    let headroom = chain
        .first()
        .map(|c| d.ledger.remaining_ratio(&c.account, now))
        .unwrap_or(0.0);

    let lands_on = chain.first().map(|c| {
        json!({
            "provider": d.providers[c.provider].id,
            "account": c.account,
            "model": c.model_id,
            "headroom": r3(headroom),
        })
    });

    // 链上最紧的一跳。第一跳最紧是常态，但 hedge 与失败换家会用到后面的跳，
    // 只盯第一跳会漏掉「换到第二家正好是最满的那个号」。
    let tightest = chain
        .iter()
        .filter_map(|c| d.ledger.tightest(&c.account, now).map(|t| (c, t)))
        .min_by(|a, b| {
            a.1.ratio
                .partial_cmp(&b.1.ratio)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(c, t)| {
            json!({
                "provider": d.providers[c.provider].id,
                "account": c.account,
                // token / rpm / rpd / tpd：光看「还剩 12%」不知道是什么见底了
                "window": t.kind.as_str(),
                "left": t.remaining,
                "headroom": r3(t.ratio),
                "resetAt": t.reset_at,
            })
        });

    // 装不下的是**第一跳**：第二跳装不下无所谓，本来就排后面
    let short = cost.and_then(|c| {
        chain
            .first()
            .and_then(|f| d.ledger.shortfall(&f.account, c, now))
    });

    let failing = recent
        .as_ref()
        .map(|r| r.requests >= 3 && r.errors * 2 >= r.requests)
        .unwrap_or(false);

    let verdict = if admits == 0 {
        "empty-pool"
    } else if chain.is_empty() {
        "no-candidate"
    } else if short.is_some() {
        "too-big"
    } else if failing {
        "pool-failing"
    } else if headroom < TIGHT_HEADROOM {
        "tight"
    } else {
        "ok"
    };

    json!({
        "purpose": purpose.as_str(),
        // 空池和「有号但全被拦下」是两回事，分开报用户才知道该去改标签还是等冷却
        "accounts": { "total": total, "admits": admits, "blocked": total - admits },
        "recent": recent.as_ref().map(|r| json!({
            "requests": r.requests,
            "errors": r.errors,
            "okRate": r.ok_rate,
        })),
        "landsOn": lands_on,
        "tightest": tightest,
        // 传了 `?tokens=` 才有值。`fits: false` 时 `shortfall` 说明是哪个窗口不够
        "cost": cost,
        "fits": cost.map(|_| short.is_none()),
        "shortfall": short.map(|(kind, left)| json!({ "window": kind.as_str(), "left": left })),
        // 结论码。措辞留给各端自己写（GUI 要中英双语，MCP 只要一句中文）
        "verdict": verdict,
    })
}

/// 解析调用方身份。`None` 表示未授权。
///
/// `require_auth = false` 时不校验，但仍尝试识别 key 以便归因（本地开发常见）。
pub(crate) async fn resolve_key(st: &Arc<State_>, headers: &HeaderMap) -> Option<keys::Identity> {
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

    let started = Instant::now();
    // 在途计数：巡检靠它给真实流量让路；函数返回时自动减回去
    let _inflight = Inflight::new(&st.inflight);

    // 可变是因为工具输出压缩会改写 `messages`。幂等指纹在下面按「客户端真正发来的
    // 那份 body」算，压缩放在判完幂等之后——否则同一条重试会算出两个指纹。
    let mut payload: Value = match serde_json::from_slice(&body) {
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

    // 幂等重放：同键同指纹直接返回上次的响应，同键不同体一律 409。
    // 只对非流式生效——流式已经把 token 发出去，没法重放。
    // 重放放在虚拟 key 记账之前：重试一次不该再扣一遍用户配额。
    let idem_ctx = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty() && !stream)
        .map(|k| (idem::sha256_hex(k), idem::fingerprint(&payload)));
    if let Some((k, fp)) = idem_ctx.as_ref() {
        match st.idem.check(k, fp, freeport_core::cooldown::now_ms()) {
            idem::Lookup::Hit {
                body: cached,
                provider,
                model,
            } => {
                tracing::debug!(provider = %provider, model = %model, "idempotent replay");
                let mut res = Response::new(Body::from(cached));
                *res.status_mut() = StatusCode::OK;
                let h = res.headers_mut();
                h.insert(
                    "content-type",
                    HeaderValue::from_static("application/json; charset=utf-8"),
                );
                h.insert("x-freeport-idempotent-replay", HeaderValue::from_static("1"));
                if let Ok(v) = HeaderValue::from_str(&provider) {
                    h.insert("x-freeport-provider", v);
                }
                if let Ok(v) = HeaderValue::from_str(&model) {
                    h.insert("x-freeport-model", v);
                }
                return res;
            }
            idem::Lookup::Conflict => {
                return (
                    StatusCode::CONFLICT,
                    Json(oai_error(
                        "同一个 Idempotency-Key 被用于了另一份请求体；换一个 key 或原样重发",
                        "invalid_request_error",
                        "idempotency_conflict",
                        json!({}),
                    )),
                )
                    .into_response();
            }
            idem::Lookup::Miss => {}
        }
    }

    // 工具输出压缩：只动 tool / tool_result，默认关（见 `compress` 模块头部的四条约束）。
    // 放在这里是因为幂等指纹已经算完了，而 `payload` 之后只被 clone 出去发上游。
    let squash = {
        let cfg = {
            let d = st.data.read().await;
            d.gateway.compress.clone()
        };
        crate::compress::apply(&mut payload, &cfg)
    };
    if squash.changed() {
        tracing::info!(
            blocks = squash.blocks,
            saved = squash.saved(),
            before = squash.chars_before,
            "工具输出已压缩"
        );
    }

    // 虚拟 key 的配额在这里判：任一维度打满就本地拒，不打上游
    {
        let blocked = {
            let mut d = st.data.write().await;
            keys::check_and_spend(&mut d, &key_label, &requested).err()
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

    // 调用方标识的分钟窗口：同一把 key 下区分终端用户。
    // 只有在设置里配了上限才生效，默认关闭——本机自用没有这个维度。
    if let Err(retry) = ident_touch(&st, &headers).await {
        let mut r = (
            StatusCode::TOO_MANY_REQUESTS,
            Json(oai_error(
                &format!("该调用方已达每分钟上限，请 {retry} 秒后重试"),
                "rate_limit_error",
                "identifier_rate_limited",
                json!({}),
            )),
        )
            .into_response();
        r.headers_mut()
            .insert("retry-after", HeaderValue::from_str(&retry.to_string()).unwrap_or(HeaderValue::from_static("60")));
        return r;
    }

    // 会话指纹：agent 不会主动带 session id，默认从「第一条 user 消息」推
    let session = session_key(&headers, &requested, &payload);
    // 能力要求从归一后的请求体里读：四个协议入口都汇到这里，只判一次
    let needs = Needs::of_chat(&payload);

    let (budget_ms, plan, has_any_key, sticky, key_name, purpose, purpose_source) = {
        let d = st.data.read().await;
        let name = d
            .api_keys
            .iter()
            .find(|k| k.id == key_label)
            .map(|k| k.name.clone())
            // 老 authToken 不在 key 列表里，但日志里也得看得出「这条是老客户端打的」
            .or_else(|| (key_label == "default").then(|| "legacy token".to_string()));
        // 用途要在 plan 之前定：它参与候选过滤，不是事后标注
        let (purpose, source) = purpose_of(&headers, needs, name.as_deref());
        let (mut p, sticky) = build_plan(&st, &d, &requested, &session, needs, purpose);
        // key 的模型白名单在计划之后过滤：候选（provider + model）要到这一步才存在。
        // 过滤掉的不静默消失，写进 rejected，用户才知道是 key 白名单挡的。
        if let Some(k) = d.api_keys.iter().find(|k| k.id == key_label) {
            if !k.allowed_models.is_empty() {
                p.candidates.retain(|c| {
                    let pid = &d.providers[c.provider].id;
                    keys::model_allowed(&k.allowed_models, pid, &c.model_id)
                });
                if p.candidates.is_empty() {
                    p.rejected.push(freeport_core::route::Rejected {
                        provider: "*".into(),
                        account: None,
                        model: requested.clone(),
                        reason: format!(
                            "not in this key's allowlist ({})",
                            k.allowed_models.join(", ")
                        ),
                        kind: "key",
                    });
                }
            }
        }
        let has_any = d
            .providers
            .iter()
            .any(|p| p.accounts.iter().any(|a| secrets::has_credential(&a.id)));
        (
            d.gateway.request_budget_ms,
            p,
            has_any,
            sticky,
            name,
            purpose,
            source,
        )
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
        purpose,
        purpose_source: Some(purpose_source),
        key_id: Some(key_label.clone()),
        key_name,
        error: None,
    };

    // 精确匹配缓存：同样的请求体已经答过，直接复用。
    // 只对非流式生效——流式要边生成边发，拿不出一个「完整的答案」来存。
    let cache_key = if !stream {
        let d = st.data.read().await;
        d.gateway.cache.enabled.then(|| cache::key(&payload))
    } else {
        None
    };
    if let Some(k) = cache_key.as_deref() {
        let hit = {
            let mut c = st.resp_cache.lock().unwrap_or_else(|e| e.into_inner());
            c.get(k, freeport_core::cooldown::now_ms())
        };
        if let Some(hit) = hit {
            entry.ok = true;
            entry.status = 200;
            entry.provider = Some(hit.provider.clone());
            entry.model = Some(hit.model.clone());
            entry.ms = started.elapsed().as_millis() as u64;
            record(&st, entry).await;
            let mut res = Response::new(Body::from(hit.body));
            *res.status_mut() = StatusCode::OK;
            let h = res.headers_mut();
            h.insert(
                "content-type",
                HeaderValue::from_static("application/json; charset=utf-8"),
            );
            h.insert("x-freeport-cache-hit", HeaderValue::from_static("1"));
            h.insert(
                "x-freeport-provider",
                HeaderValue::from_str(&hit.provider).unwrap_or(HeaderValue::from_static("")),
            );
            h.insert(
                "x-freeport-model",
                HeaderValue::from_str(&hit.model).unwrap_or(HeaderValue::from_static("")),
            );
            return res;
        }
    }

    if plan.is_empty() {
        // 用途标签挡掉的候选要单独点名：否则用户看到「都在冷却」会去查额度，
        // 而真相是他把所有号都标成了别的用途。
        let pooled_out = plan.rejected.iter().filter(|r| r.kind == "purpose").count();
        // 点名了一个模型却无路可走：这时**不换别的模型顶上**，所以必须说清
        // 为什么没有，否则用户只会看到「所有候选都在冷却」去查错方向。
        let named = !freeport_core::route::is_auto(&requested);
        let msg = if !has_any_key {
            "还没有接入任何厂商，请先在「厂商」页添加 API Key".to_string()
        } else if named && pooled_out > 0 {
            format!(
                "模型 {requested} 的凭据都因用途标签被排除（本次请求按 {} 处理）。去「账号」页调整标签，或留空表示不限",
                purpose.as_str()
            )
        } else if named {
            format!(
                "模型 {requested} 没有可用通道（未接入 / 已下架 / 凭据不可用 / 额度耗尽）。没有拿别的模型顶替：去掉模型名让网关自己选，或换一个模型"
            )
        } else if pooled_out > 0 {
            format!(
                "没有可用候选：{pooled_out} 条凭据因用途标签被排除（本次请求按 {} 处理）。去「账号」页调整标签，或留空表示不限",
                purpose.as_str()
            )
        } else {
            "所有候选都在冷却中，或没有可用模型".to_string()
        };
        entry.status = 503;
        entry.ms = started.elapsed().as_millis() as u64;
        entry.error = Some(msg.clone());
        record(&st, entry).await;
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(oai_error(
                &msg,
                "freeport_no_route",
                "no_candidate",
                json!({
                    "requested": requested,
                    "cooling": st.cooldowns.snapshot().into_iter().take(10).collect::<Vec<_>>(),
                    "purpose": purpose.as_str(),
                }),
            )),
        )
            .into_response();
    }

    // (provider, account, model)：多账号下「这条 key 挂了」不能连坐同一家的另一条 key
    let mut skipped: HashSet<(usize, String, String)> = HashSet::new();

    let queue = plan.candidates.clone();
    let queue_len = queue.len();
    // 只有明确要了 JSON 才判「格式违约」，否则一半编程对话都会被误杀
    let wants_json = payload.get("response_format").is_some();
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

        // 并发闸：拿不到名额就换下一条，不排队——排队会把 45s 预算耗在等上
        let cap = { st.data.read().await.gateway.max_inflight_per_account };
        let Some(slot) = try_slot(&st, &account_id, cap) else {
            tracing::debug!(account = %account_id, "凭据并发已满，跳过");
            continue;
        };

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
        // hedge（§3.6.8）：还有下家时，这一跳只分到一段切片。
        // 只控「是否开始下一次重试」是不够的——上游最常见的是「头回来了、体不发」，
        // 不掐这一跳就会把 45s 全耗在它身上，后面的候选连尝试的机会都没有。
        // 最后一跳拿满剩余预算：后面没人了，掐掉只是白等。
        let attempt_ms = if has_next {
            let left = budget_ms.saturating_sub(started.elapsed().as_millis() as u64);
            let slice = left / ((queue_len - qi) as u64).max(1);
            remaining.min(Duration::from_millis(
                slice.clamp(HEDGE_MIN_MS, HEDGE_MAX_MS),
            ))
        } else {
            remaining
        };
        let t0 = Instant::now();

        // 先记账再发请求：并发下不会超卖，代价是偏保守
        let limits = quota::limits_of(account_quota.as_ref());
        let (est_input, est_total) = estimate_tokens(&payload);
        {
            let mut d = st.data.write().await;
            d.ledger.reserve_now(&account_id, limits, est_total, false);
            // 上游学过的模型级窗口跟着记一笔：没学过时是 no-op。
            // 不记的话学到的模型限额永远不会被填满，Gate 的检查形同虚设。
            d.ledger.reserve_model_now(&account_id, &cand.model_id, est_total, false);
        }

        match providers::create_chat(&providers::client_of(&provider), &provider, &cred, &body_for_upstream, attempt_ms).await {
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
                            // 扣住的时间不能超过这一跳的切片，否则 hedge 无从谈起
                            let cap = attempt_ms.as_millis() as u64;
                            HoldCfg {
                                idle_ms: d.gateway.hold_idle_ms.min(cap),
                                total_ms: d.gateway.hold_total_ms.min(cap),
                            }
                        };
                        match hold::wait_commit(upstream, &cfg, upwire::wire_of_provider(&provider_id)).await {
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
                            let wire = upwire::wire_of_provider(&provider_id);
                            return stream_response(st.clone(), head, rest, entry, started, provider_id, account_id, model_id, trail, limits, est_total, wire, slot);
                        }
                        Err((kind, detail)) => {
                            let cls = if kind == "upstream_body" {
                                classify(400, &detail, 0)
                            } else {
                                ErrorClass::soft(&kind)
                            };
                            note_fail(&mut entry, &st, &provider_id, &account_id, &model_id, &cls, 0, t0.elapsed().as_millis() as u64, &detail).await;
                            // 上游已受理过（200 但内容无效），输入被计费，只回吐输出的预扣
                            settle_quota(&st, &account_id, &model_id, limits, est_total, est_input).await;
                            continue;
                        }
                    }
                }

                // 非流式：读全 body 再判，200 里藏 error 是免费厂商的常见形态。
                // reqwest 的 timeout 只覆盖到响应头，体卡住得自己掐——
                // 否则「头回来了、体不发」会让整条链停在这一跳。
                let read_budget = attempt_ms
                    .saturating_sub(t0.elapsed())
                    .max(Duration::from_millis(500));
                let text = match tokio::time::timeout(read_budget, upstream.text()).await {
                    Ok(Ok(t)) => t,
                    Ok(Err(e)) => {
                        let detail = red.redact(&e.to_string());
                        let cls = ErrorClass::soft("truncated");
                        note_fail(&mut entry, &st, &provider_id, &account_id, &model_id, &cls, 0, t0.elapsed().as_millis() as u64, &detail).await;
                        // 头已回、体断了：不确定上游是否计费，按只算输入退
                        settle_quota(&st, &account_id, &model_id, limits, est_total, est_input).await;
                        continue;
                    }
                    Err(_) => {
                        let cls = ErrorClass::soft("timeout");
                        note_fail(&mut entry, &st, &provider_id, &account_id, &model_id, &cls, 504, t0.elapsed().as_millis() as u64, "响应体在这一跳的预算内没有读完").await;
                        settle_quota(&st, &account_id, &model_id, limits, est_total, est_input).await;
                        continue;
                    }
                };
                if let Some(cls) = body_error(&text) {
                    // 200 里藏的 error 也可能带 key 回显，进日志前抹掉
                    let clean = red.redact(&text);
                    note_fail(&mut entry, &st, &provider_id, &account_id, &model_id, &cls, 0, t0.elapsed().as_millis() as u64, &clean).await;
                    // 上游已经受理（200），输出没有产出：只认输入
                    settle_quota(&st, &account_id, &model_id, limits, est_total, est_input).await;
                    continue;
                }

                let mut json: Value =
                    serde_json::from_str(&text).unwrap_or_else(|_| json!({ "raw": text }));
                // 上游讲原生协议时，从这里开始一切必须回到 OpenAI 形态：
                // 记账、幂等、缓存、回给客户端的都是这个形状
                let wire = upwire::wire_of_provider(&provider_id);
                if !wire.is_openai() {
                    json = upwire::response(wire, &json, &model_id);
                }

                // 200 却拿不到内容：reasoning 吃满 max_tokens 很常见，值得再试同模型，
                // 但不能记成健康。连续 3 次之后由 note_fail 升级成冷却。
                if let Some(kind) = freeport_core::error::empty_completion(&json) {
                    let cls = ErrorClass::soft(kind);
                    note_fail(&mut entry, &st, &provider_id, &account_id, &model_id, &cls, 200, t0.elapsed().as_millis() as u64, "上游 200 但没有可用内容").await;
                    settle_quota(&st, &account_id, &model_id, limits, est_total, est_input).await;
                    continue;
                }
                // 要了 JSON 却回了一段话：模型没遵守契约。只影响这一单的选路，
                // 不改能力门禁——同一模型在别的请求里可能照常工作。
                if freeport_core::error::format_ignored(&json, wants_json) {
                    let cls = ErrorClass::soft("format_ignored");
                    note_fail(&mut entry, &st, &provider_id, &account_id, &model_id, &cls, 200, t0.elapsed().as_millis() as u64, "要求 JSON 但返回的不是 JSON").await;
                    settle_quota(&st, &account_id, &model_id, limits, est_total, est_input).await;
                    continue;
                }

                mark_success(&st, &mut entry, &provider_id, &account_id, &model_id, t0.elapsed().as_millis() as u64, cand.half_open, &session).await;

                entry.usage = json.get("usage").and_then(parse_usage);
                entry.ms = started.elapsed().as_millis() as u64;
                bump_stats(&st, &entry.usage, false).await;
                // 拿到真实 usage 就回吐多扣的部分；拿不到（流式未带 include_usage）就认预扣
                settle_quota(
                    &st,
                    &account_id,
                    &model_id,
                    limits,
                    est_total,
                    usage_tokens(&entry.usage).unwrap_or(est_total),
                )
                .await;
                let trail = trail_of(&entry);
                record(&st, entry.clone()).await;
                if let Some((k, fp)) = idem_ctx.as_ref() {
                    if idem::cacheable(&json) {
                        st.idem.put(
                            k,
                            fp,
                            json.to_string(),
                            provider_id.clone(),
                            model_id.clone(),
                            freeport_core::cooldown::now_ms(),
                        );
                    }
                }
                // 响应缓存：只存正常收尾、且不带工具调用的回答。
                // 带 tool_calls 的复用了会跳过工具执行，比不缓存更危险。
                if let Some(k) = cache_key.as_ref() {
                    if cache::cacheable(&json) {
                        let (ttl, max) = {
                            let d = st.data.read().await;
                            (d.gateway.cache.ttl_secs, d.gateway.cache.max_entries)
                        };
                        let mut c = st.resp_cache.lock().unwrap_or_else(|e| e.into_inner());
                        c.put(
                            k.clone(),
                            cache::Hit {
                                body: json.to_string(),
                                provider: provider_id.clone(),
                                model: model_id.clone(),
                                at: freeport_core::cooldown::now_ms(),
                            },
                            freeport_core::cooldown::now_ms(),
                            ttl,
                            max,
                        );
                    }
                }
                let mut res = Response::new(Body::from(json.to_string()));
                *res.status_mut() = StatusCode::OK;
                let h = res.headers_mut();
                h.insert("content-type", HeaderValue::from_static("application/json; charset=utf-8"));
                h.insert("x-freeport-provider", HeaderValue::from_str(&provider_id).unwrap_or(HeaderValue::from_static("")));
                h.insert("x-freeport-model", HeaderValue::from_str(&model_id).unwrap_or(HeaderValue::from_static("")));
                h.insert("x-freeport-trail", HeaderValue::from_str(&trail).unwrap_or(HeaderValue::from_static("")));
                // 压了就明说：用户看到账单变小、或者模型说「没看到中间那截」，
                // 都得能立刻对上原因，而不是翻日志猜
                if squash.changed() {
                    if let Ok(v) = HeaderValue::from_str(&squash.saved().to_string()) {
                        h.insert("x-freeport-compressed", v);
                    }
                }
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
                settle_quota(&st, &account_id, &cand.model_id, limits, est_total, actual).await;
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

/* ── OpenAI 旧版补全入口（`/v1/completions`） ───────
   和 messages / responses 一样：请求转成 chat 形态走同一条接力链，
   响应再改回 `choices[].text` 的旧形状。一堆老 SDK 与脚本还在说这个协议，
   而免费厂商只认 chat —— 这层适配就是「能不能用」的差别。

   拒绝而不是猜的三种输入见 `completions::to_chat`：批量 prompt、token id、suffix。 */

async fn completions_entry(
    State(st): State<Arc<State_>>,
    mut headers: HeaderMap,
    body: Bytes,
) -> Response {
    let v: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(oai_error(
                    "请求体不是合法 JSON",
                    "invalid_request_error",
                    "bad_json",
                    json!({}),
                )),
            )
                .into_response()
        }
    };
    let stream = v.get("stream").and_then(|x| x.as_bool()).unwrap_or(false);
    // echo 是本地实现的（拼回 text），所以在这里剥掉，不能漏给上游
    let echo = v.get("echo").and_then(|x| x.as_bool()).unwrap_or(false);

    // 让请求日志按真实入口记账，而不是一律写成 chat/completions
    headers.insert("x-freeport-path", HeaderValue::from_static("/v1/completions"));

    let (chat_body, prompt) = match crate::completions::to_chat(&v) {
        Ok(x) => x,
        Err(msg) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(oai_error(
                    &msg,
                    "invalid_request_error",
                    "unsupported_parameter",
                    json!({}),
                )),
            )
                .into_response()
        }
    };

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
        let mut out = Response::new(crate::completions::sse_transform(
            res.into_body(),
            model,
            echo.then(|| prompt.clone()),
        ));
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

    let mut out = if parsed.get("error").is_some() {
        // 网关自己的错误本来就是 OpenAI 形状，旧客户端认，不必再包一层
        (status, Json(parsed)).into_response()
    } else if parsed.is_null() {
        (
            StatusCode::BAD_GATEWAY,
            Json(oai_error(
                "上游返回了无法解析的响应",
                "api_error",
                "bad_upstream",
                json!({}),
            )),
        )
            .into_response()
    } else {
        (
            StatusCode::OK,
            Json(crate::completions::from_chat(
                &parsed,
                &model,
                echo.then(|| prompt.as_str()),
            )),
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

/* ── OpenAI Embeddings 入口 ───────────────────────
   embeddings 不是 chat：没有消息、没有流、没有工具调用，所以不走
   chat() 的接力链，只复用它的 Gate（build_plan）、账本与失败分类。
   三条边界都是刻意的：
   - 只转发给讲 OpenAI 兼容线的厂商（订阅号的原生端点没有 embeddings）；
   - 不做「auto 帮你挑个 embedding 模型」——目录没把 embedding 单列，
     让网关猜一个等于猜错也能跑通，宁可选不上；
   - 目录里没有声明该模型时如实 503，不假装。 */
async fn embeddings_entry(State(st): State<Arc<State_>>, headers: HeaderMap, body: Bytes) -> Response {
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
    let key_label = identity.label();
    let started = Instant::now();
    let _inflight = Inflight::new(&st.inflight);

    let v: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(oai_error("请求体不是合法 JSON", "invalid_request_error", "bad_json", json!({}))),
            )
                .into_response()
        }
    };

    let requested = v
        .get("model")
        .and_then(|x| x.as_str())
        .map(|s| freeport_core::route::resolve_alias(s.trim()))
        .unwrap_or("")
        .to_string();
    if requested.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(oai_error("embeddings 请求必须带 model", "invalid_request_error", "missing_model", json!({}))),
        )
            .into_response();
    }

    // input：字符串 / 字符串数组 / token id 数组，三者都合法且都能原样转发；
    // 空的没有语义，其它形状（对象、混合数组）转发只会换来一次必然的 400
    let input = v.get("input");
    let input_ok = match input {
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => {
            !a.is_empty()
                && a.iter().all(|x| {
                    x.is_string() || x.is_i64() || x.is_u64()
                })
        }
        _ => false,
    };
    if !input_ok {
        return (
            StatusCode::BAD_REQUEST,
            Json(oai_error(
                "input 必须是非空字符串，或非空的字符串 / token id 数组",
                "invalid_request_error",
                "unsupported_parameter",
                json!({}),
            )),
        )
            .into_response()
    }

    // 上游体只带认识且必要的字段。多一个上游不认识的键就是一次 400 换家，
    // 和 completions / responses 入口同一条纪律。
    let mut up = json!({ "model": requested, "input": input });
    if let Some(d) = v.get("dimensions").and_then(|x| x.as_u64()) {
        up["dimensions"] = json!(d);
    }
    if let Some(f) = v.get("encoding_format").and_then(|x| x.as_str()) {
        up["encoding_format"] = json!(f);
    }

    // token 预估：embeddings 没有输出，预扣只算输入（字符数 ÷ 3，与 chat 同口径）
    let mut chars = 0usize;
    fn chars_of(v: &Value, out: &mut usize) {
        match v {
            Value::String(s) => *out += s.chars().count(),
            Value::Array(a) => a.iter().for_each(|x| chars_of(x, out)),
            _ => {}
        }
    }
    if let Some(i) = input {
        chars_of(i, &mut chars);
    }
    let est = (chars as f64 / 3.0).ceil().max(1.0);

    let mut entry = LogEntry {
        id: format!("req_{}", &uuid::Uuid::new_v4().simple().to_string()[..12]),
        at: chrono::Utc::now().to_rfc3339(),
        path: "/v1/embeddings".into(),
        requested_model: requested.clone(),
        model: None,
        provider: None,
        account_id: None,
        account_name: None,
        stream: false,
        ok: false,
        status: 0,
        ms: 0,
        usage: None,
        attempts: Vec::new(),
        sticky: false,
        purpose: Purpose::Chat,
        purpose_source: Some(freeport_core::model::PurposeSource::Default),
        key_id: Some(key_label.clone()),
        key_name: None,
        error: None,
    };

    let (budget_ms, plan, has_any_key) = {
        let d = st.data.read().await;
        let (mut p, _sticky) =
            build_plan(&st, &d, &requested, "", Needs::default(), Purpose::Chat);
        // 虚拟 key 的白名单同样要过：embeddings 不该成为绕过白名单的侧门
        if let Some(k) = d.api_keys.iter().find(|k| k.id == key_label) {
            if !k.allowed_models.is_empty() {
                p.candidates.retain(|c| {
                    let pid = &d.providers[c.provider].id;
                    keys::model_allowed(&k.allowed_models, pid, &c.model_id)
                });
            }
        }
        let has_any = d
            .providers
            .iter()
            .any(|p| p.accounts.iter().any(|a| secrets::has_credential(&a.id)));
        (d.gateway.request_budget_ms, p, has_any)
    };

    if plan.is_empty() {
        let msg = if !has_any_key {
            "还没有接入任何厂商，请先在「厂商」页添加 API Key".to_string()
        } else {
            format!(
                "模型 {requested} 没有可用的 embeddings 通道（目录里没有这个模型，或凭据不可用）。embeddings 不会由网关代选模型"
            )
        };
        entry.status = 503;
        entry.error = Some(msg.clone());
        record(&st, entry).await;
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(oai_error(&msg, "freeport_no_route", "no_candidate", json!({ "requested": requested }))),
        )
            .into_response();
    }

    let mut skipped: HashSet<(usize, String, String)> = HashSet::new();
    for cand in plan.candidates.clone() {
        if started.elapsed() > Duration::from_millis(budget_ms) && !entry.attempts.is_empty() {
            break;
        }
        if skipped.contains(&(cand.provider, cand.account.clone(), cand.model_id.clone())) {
            continue;
        }
        let (provider, account_quota) = {
            let d = st.data.read().await;
            let p = d.providers[cand.provider].clone();
            let quota = p.account(&cand.account).and_then(|a| a.quota);
            (p, quota)
        };
        // 订阅号的原生端点没有 embeddings：不是失败，是这单从一开始就不该来
        if !upwire::wire_of_provider(&provider.id).is_openai() {
            entry.attempts.push(Attempt {
                provider: provider.id.clone(),
                account: Some(cand.account.clone()),
                model: cand.model_id.clone(),
                ms: 0,
                status: 0,
                kind: "no_embeddings_endpoint".into(),
                scope: Some("provider".into()),
                cooled_ms: None,
                detail: Some("该厂商只有原生协议端点，不支持 embeddings".into()),
                skip_provider: Some(true),
            });
            continue;
        }
        let Some(cred) = credential(&st, &cand.account).await else { continue };
        let account_id = cand.account.clone();
        let model_id = cand.model_id.clone();

        let limits = quota::limits_of(account_quota.as_ref());
        {
            let mut d = st.data.write().await;
            d.ledger.reserve_now(&account_id, limits, est, false);
            d.ledger.reserve_model_now(&account_id, &model_id, est, false);
        }
        let t0 = Instant::now();
        let remaining = Duration::from_millis(
            budget_ms.saturating_sub(started.elapsed().as_millis() as u64).max(1_000),
        );

        // 组里可能是同名模型的另一家：model 要换成这一候选的
        let mut body_for_upstream = up.clone();
        body_for_upstream["model"] = json!(model_id);

        match providers::create_embeddings(&providers::client_of(&provider), &provider, &cred, &body_for_upstream, remaining).await {
            Ok(res) => {
                let text = res.text().await.unwrap_or_default();
                let json: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
                let shapes_ok = json.get("data").and_then(|x| x.as_array())
                    .map(|a| !a.is_empty() && a.iter().all(|d| d.get("embedding").map(|e| e.is_array()).unwrap_or(false)))
                    .unwrap_or(false);
                if !shapes_ok {
                    // 200 但不是 embeddings 形状：透明代理到了错误端点。如实记失败
                    let cls = ErrorClass::soft("bad_upstream");
                    note_fail(&mut entry, &st, &provider.id, &account_id, &model_id, &cls, 200, t0.elapsed().as_millis() as u64, "上游 200 但不是 embeddings 形状").await;
                    settle_quota(&st, &account_id, &model_id, limits, est, 0.0).await;
                    continue;
                }
                entry.ok = true;
                entry.status = 200;
                entry.provider = Some(provider.id.clone());
                entry.account_id = Some(account_id.clone());
                entry.account_name = provider.account(&account_id).map(|a| a.name.clone());
                entry.model = Some(model_id.clone());
                entry.usage = json.get("usage").and_then(parse_usage);
                entry.ms = started.elapsed().as_millis() as u64;
                bump_stats(&st, &entry.usage, false).await;
                let actual = usage_tokens(&entry.usage).unwrap_or(est);
                settle_quota(&st, &account_id, &model_id, limits, est, actual).await;
                mark_success(&st, &mut entry, &provider.id, &account_id, &model_id, t0.elapsed().as_millis() as u64, cand.half_open, "").await;

                let mut res = (StatusCode::OK, Json(json)).into_response();
                let h = res.headers_mut();
                for (k, v) in [
                    ("x-freeport-provider", provider.id.as_str()),
                    ("x-freeport-model", model_id.as_str()),
                ] {
                    if let Ok(val) = HeaderValue::from_str(v) {
                        h.insert(k, val);
                    }
                }
                let trail = trail_of(&entry);
                if let Ok(val) = HeaderValue::from_str(&trail) {
                    h.insert("x-freeport-trail", val);
                }
                record(&st, entry).await;
                return res;
            }
            Err(e) => {
                let cls = freeport_core::error::classify(e.status, &e.body, e.retry_after_ms);
                let detail = e.body.clone();
                if note_fail(&mut entry, &st, &provider.id, &account_id, &model_id, &cls, e.status, t0.elapsed().as_millis() as u64, &detail).await {
                    let whole_provider = matches!(cls.scope, freeport_core::Scope::Provider);
                    for c in plan.candidates.iter() {
                        if c.provider == cand.provider && (whole_provider || c.account == account_id) {
                            skipped.insert((c.provider, c.account.clone(), c.model_id.clone()));
                        }
                    }
                }
                let actual = if e.status == 0 { 0.0 } else { est };
                settle_quota(&st, &account_id, &model_id, limits, est, actual).await;
            }
        }
    }

    let status = failure_status(&entry.attempts);
    entry.status = status;
    entry.ms = started.elapsed().as_millis() as u64;
    let kinds: Vec<String> = entry.attempts.iter().map(|a| a.kind.clone()).collect();
    entry.error = Some(kinds.join(", "));
    bump_stats(&st, &None, true).await;
    record(&st, entry.clone()).await;
    (
        StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY),
        Json(oai_error(
            "全部 embeddings 候选失败",
            "freeport_all_failed",
            entry.attempts.first().map(|a| a.kind.as_str()).unwrap_or("no_candidate"),
            json!({ "attempts": entry.attempts }),
        )),
    )
        .into_response()
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

/// 「200 但没内容」在前几次不判故障：reasoning 模型吃满 `max_tokens` 是常态，
/// 换一个模型重发通常就好了。超过这个次数才当成「这条通道真的答不出东西」。
const EMPTY_STREAK_EXEMPT: u32 = 3;

/// 一条凭据的在途名额。Drop 时归还，流式响应由后台任务持有到流结束。
struct AcctSlot {
    st: Arc<State_>,
    id: String,
}

impl Drop for AcctSlot {
    fn drop(&mut self) {
        let mut m = self
            .st
            .acct_inflight
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(n) = m.get_mut(&self.id) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                m.remove(&self.id);
            }
        }
    }
}

/// 抢一个名额。`cap == 0` 表示不限，只记账不拦。
///
/// 抢不到时**不排队**：45 秒预算里等一个空位，比换下一家更慢，
/// 而且排队期间用户看到的是「没反应」。
fn try_slot(st: &Arc<State_>, id: &str, cap: u32) -> Option<AcctSlot> {
    let mut m = st.acct_inflight.lock().unwrap_or_else(|e| e.into_inner());
    let n = m.entry(id.to_string()).or_insert(0);
    if cap > 0 && *n >= cap as usize {
        return None;
    }
    *n += 1;
    Some(AcctSlot {
        st: st.clone(),
        id: id.to_string(),
    })
}

fn empty_key(provider_id: &str, account_id: &str, model_id: &str) -> String {
    format!("{provider_id}:{account_id}:{model_id}")
}

fn bump_empty(st: &Arc<State_>, provider_id: &str, account_id: &str, model_id: &str) -> u32 {
    let mut m = st.empty_streak.lock().unwrap_or_else(|e| e.into_inner());
    let n = {
        let e = m.entry(empty_key(provider_id, account_id, model_id)).or_insert(0);
        *e = e.saturating_add(1);
        *e
    };
    // 进程长期运行，别让这张表无限长
    if m.len() > 512 {
        m.retain(|_, v| *v > 0);
    }
    n
}

fn reset_empty(st: &Arc<State_>, provider_id: &str, account_id: &str, model_id: &str) {
    let mut m = st.empty_streak.lock().unwrap_or_else(|e| e.into_inner());
    m.remove(&empty_key(provider_id, account_id, model_id));
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
    reset_empty(st, provider_id, account_id, model_id);

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
    let mut cls = cls.clone();
    // 空回复前三次只换下一跳；连续第四次才当成通道故障，冷却 90 秒。
    // 这条豁免很关键：reasoning 模型把 max_tokens 全花在思考上是正常现象，
    // 一看到空就冷却会让用户「换个问题就好了」——那是最难查的一类 bug。
    if matches!(
        cls.kind.as_str(),
        "empty_completion" | "empty_reasoning" | "empty_stream"
    ) {
        let n = bump_empty(st, provider_id, account_id, model_id);
        if n > EMPTY_STREAK_EXEMPT {
            cls.scope = freeport_core::Scope::Key;
            cls.cooldown_ms = 90_000;
            cls.source = freeport_core::cooldown::Source::Heuristic;
        }
    }
    // 冷却 0 的失败（安全拒答 / 空响应 / 模型慢）只换下一跳，不写冷却
    let cooled = if cls.cooldown_ms > 0 {
        st.cooldowns.hit_with(
            cls.source,
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
    //
    // 文案点名了模型（`Rate limit reached for model \`x\``）→ 这是**模型级**
    // 限额，记进 `account|model` 的窗口：同一条 key 上的别的模型不受影响。
    // 点名的模型与本次请求不一致时不学——宁可漏记，不能把别的模型的账记错。
    if let Some(l) = &cls.learned {
        let now = freeport_core::cooldown::now_ms();
        let mut d = st.data.write().await;
        match l.model.as_deref() {
            Some(m) if m == model_id => {
                if let Some(rpd) = l.daily_limit {
                    d.ledger
                        .learn_model(account_id, model_id, quota::WindowKind::Rpd, rpd as f64, now);
                }
                if let Some(rpm) = l.minute_limit {
                    d.ledger
                        .learn_model(account_id, model_id, quota::WindowKind::Rpm, rpm as f64, now);
                }
                if let Some(tpm) = l.token_minute_limit {
                    d.ledger
                        .learn_model(account_id, model_id, quota::WindowKind::Tpm, tpm as f64, now);
                }
                if let Some(tpd) = l.token_day_limit {
                    d.ledger
                        .learn_model(account_id, model_id, quota::WindowKind::Tpd, tpd as f64, now);
                }
            }
            Some(_) => {}
            None => {
                if let Some(rpd) = l.daily_limit {
                    d.ledger.learn(account_id, quota::WindowKind::Rpd, rpd as f64, now);
                }
                if let Some(rpm) = l.minute_limit {
                    d.ledger.learn(account_id, quota::WindowKind::Rpm, rpm as f64, now);
                }
            }
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
    wire: upwire::Wire,
    slot: AcctSlot,
) -> Response {
    // header 需要 provider 名，spawn 内部结算也要用，留一份
    let provider_id_header = provider_id.clone();
    // 转换要用模型名，header 也要用——spawn 会把 model_id 移走，各留一份
    let model_for_wire = model_id.clone();
    let model_for_header = model_id.clone();
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(32);

    tokio::spawn(async move {
        // 名额跟着流走：流结束（或客户端断开）才释放，否则「限并发」只是限住了握手
        let _slot = slot;
        let mut tail = String::new();
        let mut broken = false;
        // head 是 Hold 阶段已经读掉的字节，必须先补发给客户端
        let mut stream = futures_util::stream::iter(
            head.into_iter().map(|b| Ok::<Bytes, reqwest::Error>(b)),
        )
        .chain(rest)
        .boxed();
        // 上游的原生 SSE 必须先转成 OpenAI 的 chunk 流：下面抓 usage、
        // 以及再往上一层对客户端协议的转换，全都按 OpenAI 的形状写的。
        // head 和 rest 一起进转换，否则断在帧中间的那一片会丢。
        if !wire.is_openai() {
            stream = upwire::rewrite(wire, model_for_wire, stream);
        }
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
            &model_id,
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
    h.insert("x-freeport-model", HeaderValue::from_str(&model_for_header).unwrap_or(HeaderValue::from_static("")));
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
    let hops = entry
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
        .join(" > ");
    // chat 是默认池，不加前缀：那条 trail 恰恰是「没有任何特别处理」的，
    // 给它挂一段 `[chat:default]` 只是给每次排障增加噪音。
    if entry.purpose == Purpose::Chat {
        return hops;
    }
    let src = entry.purpose_source.map(|s| s.as_str()).unwrap_or("?");
    format!("[{}:{}] {hops}", entry.purpose.as_str(), src)
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
    model: &str,
    limits: [f64; 4],
    reserved: f64,
    actual: f64,
) {
    let mut d = st.data.write().await;
    d.ledger.settle_now(account, limits, reserved, actual, false);
    // 模型级窗口同口径结算；没学过时是 no-op
    d.ledger.settle_model_now(account, model, reserved, actual, false);
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

/// 推进 `x-freeport-identifier` 的分钟窗口。返回 `Err(还需等待秒数)` 表示超限。
///
/// 没配上限、没带标识、标识里含可疑字符时一律放行：限流是附加能力，
/// 不该因为一个格式不对的头把正常请求拦下来。
async fn ident_touch(st: &Arc<State_>, headers: &HeaderMap) -> Result<(), i64> {
    let limit = { st.data.read().await.gateway.identifier_limit };
    if limit == 0 {
        return Ok(());
    }
    let Some(raw) = headers
        .get("x-freeport-identifier")
        .and_then(|v| v.to_str().ok())
    else {
        return Ok(());
    };
    let id = raw.trim();
    if id.is_empty()
        || id.len() > 128
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.:|@".contains(c))
    {
        return Ok(());
    }

    let now = chrono::Utc::now().timestamp();
    let minute = now / 60;
    let mut map = st.idents.lock().unwrap_or_else(|e| e.into_inner());
    // 别让这张表随客户端数量无限长大：一分钟以上的桶直接清掉
    if map.len() > 4096 {
        map.retain(|_, (m, _)| *m >= minute - 1);
    }
    let e = map.entry(id.to_string()).or_insert((minute, 0));
    if e.0 != minute {
        *e = (minute, 0);
    }
    e.1 += 1;
    if e.1 > limit {
        return Err(60 - now % 60);
    }
    Ok(())
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
    // key 的 token 账在这里记：只有这一处同时拿得到 key 身份与真实 usage。
    // 请求数在入口预扣，token 只能事后记——预扣得靠估算，估错了会挡住本来能跑的请求。
    if let (Some(kid), Some(u)) = (entry.key_id.as_deref(), entry.usage.as_ref()) {
        let tokens = u.tokens_in().saturating_add(u.tokens_out());
        if tokens > 0 {
            {
                let mut d = st.data.write().await;
                keys::spend_tokens(&mut d, kid, tokens);
            }
            persist_throttled(st).await;
        }
    }
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
