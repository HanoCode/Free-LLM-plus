//! 订阅登录（授权码 + PKCE）的真机链路。
//!
//! 对着一个假 IdP 跑，锁住四件最容易搞错的事：
//!
//! 1. **编码不能弄反**：Claude 只吃 JSON，Codex 只吃表单。弄反了上游只回一个
//!    含糊的 400，线上排查会花掉一整天。
//! 2. **工作区头要带上**：Codex 的每个请求都得有 `chatgpt-account-id`，
//!    它是从 id_token 里解出来的，不是配置项。
//! 3. **过期自动续期**：订阅 token 只有几小时，续期走同一条编码分派。
//! 4. **凭据要能被认出来是登录来的**：`auth` 标了厂商，界面才不会显示一把假 key。

use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
use base64::Engine as _;
use serde_json::{json, Value};

use freeport_gateway::tokens::Endpoints;
use freeport_gateway::Gateway;

#[derive(Default)]
struct Idp {
    /// 每次换 token 的 `(路径, content-type)`，用来验证编码分派
    calls: Mutex<Vec<(String, String)>>,
    /// 每次 `/models` 收到的头，用来验证工作区头有没有带上
    model_headers: Mutex<Vec<HeaderMap>>,
    /// 每次查额度收到的头，用来验证 beta 头与工作区头
    usage_headers: Mutex<Vec<(&'static str, HeaderMap)>>,
}

fn content_type(h: &HeaderMap) -> String {
    h.get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string()
}

/// 造一个只有 payload 有意义的 JWT——我们只读不验签。
fn fake_jwt(payload: Value) -> String {
    let body = B64URL.encode(serde_json::to_vec(&payload).unwrap());
    format!("header.{body}.sig")
}

/// Claude：回 JSON，身份放在 `account.email_address`。
async fn claude_token(State(idp): State<Arc<Idp>>, h: HeaderMap) -> Json<Value> {
    idp.calls
        .lock()
        .unwrap()
        .push(("/claude/token".into(), content_type(&h)));
    // expires_in=0 逼出「拿到手就过期」→ 下一次取凭据必然走续期
    Json(json!({
        "access_token": "cc-token",
        "refresh_token": "cc-refresh",
        "expires_in": 0,
        "token_type": "Bearer",
        "scope": "user:inference",
        "account": { "uuid": "u-claude", "email_address": "claude@example.test" }
    }))
}

/// Codex：回表单换来的 JSON，身份和**工作区**都只在 id_token 里。
async fn codex_token(State(idp): State<Arc<Idp>>, h: HeaderMap) -> Json<Value> {
    idp.calls
        .lock()
        .unwrap()
        .push(("/codex/token".into(), content_type(&h)));
    let id_token = fake_jwt(json!({
        "email": "codex@example.test",
        "https://api.openai.com/auth": { "chatgpt_account_id": "ws-42" }
    }));
    Json(json!({
        "access_token": "cx-token",
        "refresh_token": "cx-refresh",
        "id_token": id_token,
        "expires_in": 0,
        "token_type": "Bearer"
    }))
}

/// Claude 的用量：顶层窗口 + limits[] 覆盖。
async fn claude_usage(State(idp): State<Arc<Idp>>, h: HeaderMap) -> Json<Value> {
    idp.usage_headers
        .lock()
        .unwrap()
        .push(("/claude/usage", h));
    Json(json!({
        "five_hour": { "utilization": 12.5, "resets_at": "2026-10-20T18:00:00Z" },
        "seven_day": { "utilization": 40.0 },
        "limits": [
            { "percent": 66.0, "scope": { "model": { "display_name": "Opus" } } }
        ]
    }))
}

/// Codex 的用量：窗口秒数 + 字符串余额。
async fn codex_usage(State(idp): State<Arc<Idp>>, h: HeaderMap) -> Json<Value> {
    idp.usage_headers
        .lock()
        .unwrap()
        .push(("/codex/usage", h));
    Json(json!({
        "rate_limit": {
            "primary_window": { "used_percent": 30.0, "limit_window_seconds": 18000 },
            "secondary_window": { "used_percent": 5.0, "limit_window_seconds": 604800 }
        },
        "credits": { "has_credits": true, "unlimited": false, "balance": "1250" }
    }))
}

async fn models(State(idp): State<Arc<Idp>>, h: HeaderMap) -> Json<Value> {
    idp.model_headers.lock().unwrap().push(h);
    Json(json!({ "data": [{ "id": "some-model" }] }))
}

async fn spawn_idp() -> (String, Arc<Idp>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let idp = Arc::new(Idp::default());
    let app = Router::new()
        .route("/claude/token", post(claude_token))
        .route("/codex/token", post(codex_token))
        .route("/claude/usage", get(claude_usage))
        .route("/codex/usage", get(codex_usage))
        .route("/models", get(models))
        .with_state(idp.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), idp)
}

fn provider(id: &str, name: &str, base: &str) -> freeport_core::model::Provider {
    freeport_core::model::Provider {
        id: id.into(),
        name: name.into(),
        base_url: base.into(),
        tier: "C".into(),
        homepage: None,
        note: None,
        key_mask: String::new(),
        enabled: true,
        added_at: chrono::Utc::now().to_rfc3339(),
        proxy: None,
        models: Vec::new(),
        quota: None,
        accounts: Vec::new(),
        health: Default::default(),
    }
}

#[tokio::test]
async fn subscription_login_creates_usable_credentials() {
    let (base, idp) = spawn_idp().await;

    // 独立数据目录：别碰用户真实的 vault。
    // 两个厂商放在同一个用例里跑，因为 FREEPORT_DATA_DIR 是进程级变量，
    // 拆成两个 #[tokio::test] 会互相踩。
    let dir = std::env::temp_dir().join(format!("freeport-sub-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("FREEPORT_DATA_DIR", &dir);

    // 预置两个订阅厂商（真实流程里由「接入」建，且因为没 key 而不建空账号）
    let mut data = freeport_gateway::store::load();
    data.providers
        .push(provider("claude-sub", "Claude", &base));
    data.providers
        .push(provider("codex-sub", "Codex", &base));
    freeport_gateway::store::save(&data);

    let gw = Gateway::with_endpoints(Endpoints::pointing_at(&base));
    gw.load().await;

    /* ── Claude ─────────────────────────────────── */

    let start = gw.login_start("claude-sub").await.expect("应能发起授权");
    assert_eq!(start.kind, "pkce", "订阅号走授权码，不是设备码");
    let url = start.url.clone().expect("要给出授权地址");
    assert!(url.starts_with(&format!("{base}/claude/authorize")), "{url}");
    assert!(url.contains("code_challenge_method=S256"));
    // 手工粘贴的路必须一直留着：端口被占时它就是唯一出路
    let redirect = start.redirect_uri.clone().expect("要给出回调地址");
    assert!(redirect.starts_with("http://127.0.0.1:"), "{redirect}");
    assert!(redirect.ends_with("/callback"), "{redirect}");

    // 用户复制了一个裸 code（Claude 网页上给的就是这个）
    let st = gw.login_complete(&start.login_id, "cc-code-1").await;
    assert_eq!(st.status, "ready", "登录应成功：{st:?}");
    assert_eq!(st.login.as_deref(), Some("claude@example.test"));
    let claude_acc = st.account_id.clone().expect("应建出凭据");

    // 登录后立刻拉过一次模型，顺带确认这家的请求不该带工作区头
    {
        let heads = idp.model_headers.lock().unwrap();
        assert!(
            heads.iter().all(|h| h.get("chatgpt-account-id").is_none()),
            "Claude 不该带 Codex 的工作区头"
        );
    }

    let cred = gw.credential(&claude_acc).await.expect("应拿到可用凭据");
    assert!(
        cred.extras().is_empty(),
        "Claude 的 account id 只给界面看，不进请求"
    );

    /* ── Codex ──────────────────────────────────── */

    let start = gw.login_start("codex-sub").await.expect("应能发起授权");
    assert_eq!(start.kind, "pkce");
    let url = start.url.clone().expect("要给出授权地址");
    assert!(url.starts_with(&format!("{base}/codex/authorize")), "{url}");
    assert!(url.contains("codex_cli_simplified_flow=true"));
    let redirect = start.redirect_uri.clone().unwrap();
    assert!(redirect.ends_with("/auth/callback"), "{redirect}");

    let st = gw
        .login_complete(&start.login_id, "http://127.0.0.1:1455/auth/callback?code=cx-code-1&state=whatever")
        .await;
    // state 不对就该拒绝：别人给的链接不能被当成自己的授权
    assert_eq!(st.status, "error", "state 不匹配必须拒绝");
    assert!(
        st.message.unwrap_or_default().contains("state"),
        "错误要说清是 state 的问题"
    );

    // 换成裸 code（不校验 state）走通
    let st = gw.login_complete(&start.login_id, "cx-code-1").await;
    assert_eq!(st.status, "ready", "登录应成功：{st:?}");
    assert_eq!(st.login.as_deref(), Some("codex@example.test"));
    let codex_acc = st.account_id.clone().expect("应建出凭据");

    // 工作区 id 必须出现在之后的每个请求上
    {
        let heads = idp.model_headers.lock().unwrap();
        let with_ws = heads
            .iter()
            .filter(|h| h.get("chatgpt-account-id").and_then(|v| v.to_str().ok()) == Some("ws-42"))
            .count();
        assert!(with_ws >= 1, "登录后拉模型必须带上 chatgpt-account-id");
    }

    let cred = gw.credential(&codex_acc).await.expect("应拿到可用凭据");
    assert_eq!(
        cred.extras(),
        [("chatgpt-account-id".to_string(), "ws-42".to_string())],
        "工作区 id 要作为附加认证头发出去"
    );

    /* ── 编码分派与续期 ─────────────────────────── */

    // expires_in=0，上面的 credential() 已经各触发过一次续期。
    // 续期走的是同一条编码分派，这里把「谁用什么编码」钉死。
    let calls = idp.calls.lock().unwrap().clone();
    let claude_ct: Vec<&str> = calls
        .iter()
        .filter(|(p, _)| p == "/claude/token")
        .map(|(_, ct)| ct.as_str())
        .collect();
    let codex_ct: Vec<&str> = calls
        .iter()
        .filter(|(p, _)| p == "/codex/token")
        .map(|(_, ct)| ct.as_str())
        .collect();

    assert!(claude_ct.len() >= 2, "换 token + 续期各一次：{claude_ct:?}");
    assert!(
        claude_ct.iter().all(|ct| ct.starts_with("application/json")),
        "Claude 只吃 JSON：{claude_ct:?}"
    );
    assert!(codex_ct.len() >= 2, "换 token + 续期各一次：{codex_ct:?}");
    assert!(
        codex_ct
            .iter()
            .all(|ct| ct.starts_with("application/x-www-form-urlencoded")),
        "Codex 只吃表单：{codex_ct:?}"
    );

    /* ── 官方额度查询 ───────────────────────────── */

    let u = gw.usage(&claude_acc).await;
    assert!(u.ok, "Claude 额度查询应成功：{u:?}");
    assert_eq!(u.kind, "claude");
    let names: Vec<&str> = u.windows.iter().map(|w| w.name.as_str()).collect();
    assert_eq!(names, vec!["five_hour", "seven_day", "seven_day_opus"]);
    // limits[] 里的 66% 覆盖顶层没有的 opus 窗口
    let opus = u.windows.iter().find(|w| w.name == "seven_day_opus").unwrap();
    assert!((opus.used_pct - 66.0).abs() < 1e-9);
    assert_eq!(
        u.windows[0].resets_at.as_deref(),
        Some("2026-10-20T18:00:00Z")
    );

    let u = gw.usage(&codex_acc).await;
    assert!(u.ok, "Codex 额度查询应成功：{u:?}");
    let names: Vec<&str> = u.windows.iter().map(|w| w.name.as_str()).collect();
    assert_eq!(names, vec!["five_hour", "seven_day"], "秒数要翻成窗口名");
    assert_eq!(u.credits, Some(1250.0), "字符串余额也要认");

    {
        let heads = idp.usage_headers.lock().unwrap();
        let claude = heads
            .iter()
            .find(|(p, _)| *p == "/claude/usage")
            .map(|(_, h)| h)
            .expect("应该问过 Claude 的用量端点");
        assert_eq!(
            claude
                .get("anthropic-beta")
                .and_then(|v| v.to_str().ok()),
            Some("oauth-2025-04-20"),
            "少了这个 beta 头，OAuth token 会被当普通 key 拒掉"
        );
        assert!(
            claude.get("chatgpt-account-id").is_none(),
            "Claude 不该带 Codex 的工作区头"
        );

        let codex = heads
            .iter()
            .find(|(p, _)| *p == "/codex/usage")
            .map(|(_, h)| h)
            .expect("应该问过 Codex 的用量端点");
        assert_eq!(
            codex
                .get("chatgpt-account-id")
                .and_then(|v| v.to_str().ok()),
            Some("ws-42")
        );
    }

    // 普通 key 没有订阅额度：说清楚，而不是当成查询失败
    let u = gw.usage("acct_no_such").await;
    assert!(!u.ok);
    assert!(u.note.is_some(), "查不到必须给一句原因");

    // 凭据列表要能看出这是登录来的
    let providers = gw.providers().await;
    for (pid, kind) in [("claude-sub", "claude"), ("codex-sub", "codex")] {
        let p = providers.iter().find(|p| p.id == pid).expect("厂商应在列表里");
        let acc = p.accounts.last().expect("应有凭据");
        assert_eq!(acc.auth.as_deref(), Some(kind), "{pid} 的凭据形态");
        assert!(acc.has_key, "{pid}：只有 token 也要算可用");
        assert!(p.models > 0, "{pid}：登录后应拉到模型");
    }

    let _ = std::fs::remove_dir_all(&dir);
}
