//! OAuth 凭据的真机链路：设备码登录 → 建凭据 → 过期自动续期 → 并发只换一次。
//!
//! 对着一个假 IdP 跑，覆盖的是最容易出事的四件事：
//! 轮询状态机、token 落盘、续期触发、并发去重。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};

use freeport_gateway::tokens::Endpoints;
use freeport_gateway::Gateway;

struct Idp {
    /// 已经发出去几枚 Copilot token
    copilot_calls: AtomicUsize,
    /// 第一次换 token 时故意给一个「已过期」的，逼出续期路径
    expire_first: bool,
    token_calls: AtomicUsize,
}

async fn device_code() -> Json<Value> {
    Json(json!({
        "device_code": "dc-test",
        "user_code": "TEST-1234",
        "verification_uri": "https://example.test/device",
        "verification_uri_complete": "https://example.test/device?code=TEST-1234",
        "interval": 1,
        "expires_in": 60
    }))
}

async fn github_token(State(idp): State<Arc<Idp>>) -> Json<Value> {
    let n = idp.token_calls.fetch_add(1, Ordering::SeqCst);
    if n == 0 {
        return Json(json!({ "error": "authorization_pending" }));
    }
    Json(json!({
        "access_token": "gh-token",
        "token_type": "bearer",
        "scope": "read:user"
    }))
}

async fn copilot_token(State(idp): State<Arc<Idp>>) -> Json<Value> {
    let n = idp.copilot_calls.fetch_add(1, Ordering::SeqCst);
    let now = chrono::Utc::now().timestamp();
    let expires_at = if idp.expire_first && n == 0 {
        now - 10 // 已经过期：走一次续期
    } else {
        now + 3600
    };
    Json(json!({ "token": format!("cp-{n}"), "expires_at": expires_at }))
}

async fn user() -> Json<Value> {
    Json(json!({ "login": "octocat" }))
}

async fn models() -> Json<Value> {
    Json(json!({ "data": [{ "id": "gpt-4o" }, { "id": "claude-sonnet-4" }] }))
}

async fn spawn_idp() -> (String, Arc<Idp>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let idp = Arc::new(Idp {
        copilot_calls: AtomicUsize::new(0),
        expire_first: true,
        token_calls: AtomicUsize::new(0),
    });
    let app = Router::new()
        .route("/login/device/code", post(device_code))
        .route("/login/oauth/access_token", post(github_token))
        .route("/copilot/token", get(copilot_token))
        .route("/user", get(user))
        .route("/models", get(models))
        .with_state(idp.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), idp)
}

#[tokio::test]
async fn device_login_refresh_and_single_flight() {
    let (base, idp) = spawn_idp().await;

    // 独立数据目录：别碰用户真实的 vault
    let dir = std::env::temp_dir().join(format!("freeport-oauth-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("FREEPORT_DATA_DIR", &dir);

    // 预置一个 Copilot 厂商（真实流程里由「添加厂商」建）
    let mut data = freeport_gateway::store::load();
    data.providers.push(freeport_core::model::Provider {
        id: "copilot".into(),
        name: "GitHub Copilot".into(),
        base_url: base.clone(),
        tier: "C".into(),
        homepage: Some("https://github.com/features/copilot".into()),
        note: None,
        key_mask: String::new(),
        enabled: true,
        added_at: chrono::Utc::now().to_rfc3339(),
        proxy: None,
        wire: None,
        models: Vec::new(),
        quota: None,
        accounts: Vec::new(),
        health: Default::default(),
    });
    freeport_gateway::store::save(&data);

    let gw = Gateway::with_endpoints(Endpoints::pointing_at(&base));
    gw.load().await;

    // ① 发起登录
    let start = gw.login_start("copilot").await.expect("应能发起登录");
    assert_eq!(start.user_code.as_deref(), Some("TEST-1234"));
    assert!(start.instruction.contains("TEST-1234"));

    // ② 轮询到成功（第一次必然是 pending）
    let mut state = gw.login_poll(&start.login_id).await;
    assert_eq!(state.status, "pending", "第一次轮询应是 pending");

    let mut tries = 0;
    while state.status == "pending" && tries < 15 {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        state = gw.login_poll(&start.login_id).await;
        tries += 1;
    }
    assert_eq!(state.status, "ready", "最终应登录成功：{state:?}");
    assert_eq!(state.login.as_deref(), Some("octocat"));
    let account_id = state.account_id.clone().expect("应建出凭据");

    // ③ 凭据列表要能看出这是登录来的，而不是一把假 key
    let providers = gw.providers().await;
    let copilot = providers.iter().find(|p| p.id == "copilot").unwrap();
    let acc = copilot
        .accounts
        .iter()
        .find(|a| a.id == account_id)
        .expect("凭据应出现在列表里");
    assert_eq!(acc.auth.as_deref(), Some("copilot"));
    assert!(acc.has_key, "只有 token 也要算「可用」");
    assert!(copilot.models > 0, "登录后应顺手拉到模型");

    // ④ 第一次 token 是「已过期」的，取凭据时必须自动续期
    let token = gw.credential(&account_id).await.expect("应拿到可用 token");
    assert_eq!(
        token.header("https://api.githubcopilot.com/chat/completions"),
        Some("Bearer cp-1".into()),
        "续期后应换成第二枚"
    );
    assert_eq!(
        idp.copilot_calls.load(Ordering::SeqCst),
        2,
        "登录换一次 + 过期换一次"
    );

    // ⑤ 并发取凭据：只允许一次刷新
    let before = idp.copilot_calls.load(Ordering::SeqCst);
    let (a, b) = tokio::join!(gw.credential(&account_id), gw.credential(&account_id));
    let head = |c: Option<freeport_gateway::cred::Credential>| {
        c.and_then(|c| c.header("https://api.githubcopilot.com/chat/completions"))
    };
    assert_eq!(head(a).as_deref(), Some("Bearer cp-1"));
    assert_eq!(head(b).as_deref(), Some("Bearer cp-1"));
    assert_eq!(
        idp.copilot_calls.load(Ordering::SeqCst),
        before,
        "没过期就不该再换"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
