//! web 会话凭据（Cookie 形态）的端到端验证。
//!
//! 只做 Cookie 试点那一家（OpenCode Zen），但覆盖的是这类凭据最容易出错的四件事：
//!
//! 1. 导入：粘贴格式能被认出来，导入即落库、即校验
//! 2. 发送：cookie 头按 URL 算，**绝不额外带 Authorization**
//! 3. 轮换：上游 Set-Cookie 换 session，要当场学回来
//! 4. 失效：全部过期就不再算「可用凭据」，也就不该再占候选位

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};

use freeport_core::model::Provider;
use freeport_gateway::cred::Credential;
use freeport_gateway::{providers, secrets, Gateway};

/// 假上游：只有带上正确的 cookie 才放行；一旦看到 Authorization 就记一笔
/// （Cookie 形态下带了 Bearer，真实站点会直接 401，而且我们看不出原因）。
struct Upstream {
    expect: String,
    saw_bearer: AtomicUsize,
    /// 每次 chat 都发一条新的 Set-Cookie，模拟会话轮换
    rotate: bool,
    calls: AtomicUsize,
}

async fn models(State(up): State<Arc<Upstream>>, headers: HeaderMap) -> Json<Value> {
    up.calls.fetch_add(1, Ordering::SeqCst);
    if headers.contains_key("authorization") {
        up.saw_bearer.fetch_add(1, Ordering::SeqCst);
    }
    let ok = headers
        .get("cookie")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains(&up.expect));
    if !ok {
        return Json(json!({ "error": "unauthorized" }));
    }
    Json(json!({ "data": [{ "id": "grok-code" }, { "id": "kimi-k2" }] }))
}

async fn chat(
    State(up): State<Arc<Upstream>>,
    headers: HeaderMap,
    _body: Json<Value>,
) -> (axum::http::StatusCode, HeaderMap, Json<Value>) {
    up.calls.fetch_add(1, Ordering::SeqCst);
    let mut out = HeaderMap::new();
    if headers.contains_key("authorization") {
        up.saw_bearer.fetch_add(1, Ordering::SeqCst);
    }
    let ok = headers
        .get("cookie")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains(&up.expect));
    if !ok {
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            out,
            Json(json!({ "error": "session expired" })),
        );
    }
    if up.rotate {
        if let Ok(v) = "session=rotated; Path=/; HttpOnly".parse() {
            out.insert("set-cookie", v);
        }
    }
    (
        axum::http::StatusCode::OK,
        out,
        Json(json!({
            "id": "cmpl-1",
            "choices": [{ "message": { "role": "assistant", "content": "pong" } }],
            "usage": { "prompt_tokens": 3, "completion_tokens": 1 }
        })),
    )
}

async fn spawn(expect: &str, rotate: bool) -> (String, Arc<Upstream>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let up = Arc::new(Upstream {
        expect: expect.to_string(),
        saw_bearer: AtomicUsize::new(0),
        rotate,
        calls: AtomicUsize::new(0),
    });
    let app = Router::new()
        .route("/v1/models", axum::routing::get(models))
        .route("/v1/chat/completions", post(chat))
        .with_state(up.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}/v1"), up)
}

fn provider_at(base: &str) -> Provider {
    Provider {
        id: "opencode".into(),
        name: "OpenCode Zen".into(),
        base_url: base.to_string(),
        tier: "C".into(),
        homepage: None,
        note: None,
        key_mask: String::new(),
        enabled: true,
        added_at: String::new(),
        proxy: None,
        wire: None,
        models: Vec::new(),
        quota: None,
        accounts: Vec::new(),
        health: Default::default(),
    }
}

fn setup_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("fp-cookie-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("FREEPORT_DATA_DIR", &dir);
    dir
}

fn seed_provider(dir: &std::path::Path, base: &str) {
    std::fs::write(
        dir.join("freeport.json"),
        json!({
            "version": 1,
            "providers": [{
                "id": "opencode",
                "name": "OpenCode Zen",
                "baseUrl": base,
                "tier": "C",
                "keyMask": "",
                "enabled": true,
                "addedAt": "2026-10-19T00:00:00Z",
                "models": [{ "id": "grok-code" }],
                "health": { "status": "unknown" }
            }],
            "routing": { "chain": [] }
        })
        .to_string(),
    )
    .unwrap();
}

#[tokio::test]
async fn cookie_credential_flows() {
    // `FREEPORT_DATA_DIR` 是进程级环境变量，拆成多个 #[tokio::test]
    // 并行跑会互相串目录，所以三个场景顺序执行。
    import_and_send().await;
    rotation_is_learned().await;
    expired_session_drops_out().await;
}

async fn import_and_send() {
    let (base, up) = spawn("session=abc", false).await;
    let dir = setup_dir();
    seed_provider(&dir, &base);
    let gw = Gateway::new();
    gw.load().await;

    /* ── ① 导入：粘贴即校验，成功才入库 ── */
    let providers = gw
        .add_cookie_account("opencode", Some("我的号".into()), "session=abc; other=1")
        .await
        .expect("会话应能导入");
    let p = providers.iter().find(|p| p.id == "opencode").unwrap();
    assert_eq!(p.accounts.len(), 1);
    let acc = &p.accounts[0];
    assert_eq!(acc.name, "我的号");
    assert_eq!(acc.auth.as_deref(), Some("cookie"));
    assert!(acc.has_key, "会话可用就要算「可用凭据」");
    assert!(p.models > 0, "导入时应顺手拉到模型");
    let account_id = acc.id.clone();
    let provider = provider_at(&base);

    /* ── ② 发送：带 Cookie 头，绝不带 Authorization ── */
    let cred = gw.credential(&account_id).await.expect("应拿到 web 会话");
    assert!(cred.is_cookie(), "应当是 Cookie 形态");
    let (hn, hv) = cred.auth_pair(&format!("{base}/chat/completions")).unwrap();
    assert_eq!(hn, "cookie");
    assert!(hv.contains("session=abc"));

    let res = providers::create_chat(
        &providers::client(),
        &provider,
        &cred,
        &json!({ "model": "grok-code", "messages": [] }),
        std::time::Duration::from_secs(5),
    )
    .await
    .map_err(|e| format!("create_chat 失败：{}", e))
    .expect("带会话的 chat 应成功");
    assert_eq!(res.status(), 200);
    assert_eq!(
        up.saw_bearer.load(Ordering::SeqCst),
        0,
        "Cookie 形态下带 Bearer 会被真实站点判 401"
    );

    /* ── ③ 换会话：旧值立刻失效，新值生效 ── */
    gw.set_account_cookie("opencode", &account_id, "session=xyz")
        .await
        .expect("应能换会话");
    let cred = gw.credential(&account_id).await.unwrap();
    let (_, hv) = cred.auth_pair(&format!("{base}/chat/completions")).unwrap();
    assert!(hv.contains("session=xyz"), "换过之后应发新值：{hv}");
    assert!(!hv.contains("session=abc"), "旧值必须被覆盖，不能两条并存");

    let _ = std::fs::remove_dir_all(&dir);
}

async fn rotation_is_learned() {
    let (base, _up) = spawn("session=abc", true).await;
    let dir = setup_dir();
    seed_provider(&dir, &base);

    let gw = Gateway::new();
    gw.load().await;
    gw.add_cookie_account("opencode", None, "session=abc")
        .await
        .unwrap();
    let account_id = gw.providers().await[0].accounts[0].id.clone();

    let provider = provider_at(&base);
    let mut cred = gw.credential(&account_id).await.unwrap();
    providers::create_chat(
        &providers::client(),
        &provider,
        &cred,
        &json!({ "model": "grok-code", "messages": [] }),
        std::time::Duration::from_secs(5),
    )
    .await
    .unwrap();

    // 上游这次回了 Set-Cookie: session=rotated
    let learned = cred.absorb(
        Some("127.0.0.1"),
        &["session=rotated; Path=/; HttpOnly".to_string()],
    );
    assert!(learned, "轮换应当被学回来并落盘");

    let next = gw.credential(&account_id).await.unwrap();
    let (_, hv) = next.auth_pair(&format!("{base}/chat/completions")).unwrap();
    assert!(hv.contains("session=rotated"), "下次要用新 session：{hv}");

    let _ = std::fs::remove_dir_all(&dir);
}

async fn expired_session_drops_out() {
    let (base, _up) = spawn("session=abc", false).await;
    let dir = setup_dir();
    seed_provider(&dir, &base);

    let gw = Gateway::new();
    gw.load().await;
    gw.add_cookie_account("opencode", None, "session=abc")
        .await
        .unwrap();
    let account_id = gw.providers().await[0].accounts[0].id.clone();
    assert!(secrets::has_credential(&account_id));

    // 手工塞一个已经过期的 jar：模拟「用户昨天的会话」
    let stale = json!({
        "entries": [{
            "name": "session",
            "value": "abc",
            "domain": "127.0.0.1",
            "path": "/",
            "hostOnly": true,
            "expiresAt": 1
        }]
    })
    .to_string();
    secrets::set_cookie(&account_id, &stale).unwrap();

    assert!(
        !secrets::has_credential(&account_id),
        "全过期的会话不该继续占候选位"
    );

    // 重新粘贴一份有效的，立刻回到池子里
    gw.set_account_cookie("opencode", &account_id, "session=abc")
        .await
        .unwrap();
    assert!(secrets::has_credential(&account_id));
    assert!(matches!(
        gw.credential(&account_id).await,
        Some(Credential::Cookie { .. })
    ));

    let _ = std::fs::remove_dir_all(&dir);
}
