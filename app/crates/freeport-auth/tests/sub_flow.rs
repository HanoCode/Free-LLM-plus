//! 订阅登录（授权码 + PKCE）对着假 IdP 跑完整链路。
//!
//! 真跑一次 Claude / Codex 需要真人登录，进不了 CI。这里用 axum 复刻两个厂商
//! **最容易搞反的那一点**——Claude 只吃 JSON、Codex 只吃表单——外加回环回调。
//! 编码弄反时上游只回一个含糊的 400，所以这条契约必须锁住。

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};

use freeport_auth::sub::{self, SubCfg, Vendor};
use freeport_auth::AuthError;

#[derive(Clone, Default)]
struct Seen {
    /// 每次换 token 请求的 content-type，按到达顺序
    content_types: Arc<Mutex<Vec<String>>>,
    bodies: Arc<Mutex<Vec<String>>>,
}

async fn token(
    State(seen): State<Seen>,
    headers: HeaderMap,
    body: String,
) -> Json<Value> {
    let ct = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    seen.content_types.lock().unwrap().push(ct.clone());
    seen.bodies.lock().unwrap().push(body.clone());

    // 表单编码里没有引号，JSON 里有——用它区分「谁把编码写反了」
    let json_body = ct.contains("json");
    if body.contains("code_verifier") && json_body {
        return Json(json!({
            "access_token": "at-claude",
            "refresh_token": "rt-claude",
            "expires_in": 3600,
            "token_type": "Bearer",
            "account": { "uuid": "u-1", "email_address": "me@example.com" }
        }));
    }
    if body.contains("code_verifier") && !json_body {
        return Json(json!({
            "access_token": "at-codex",
            "refresh_token": "rt-codex",
            "id_token": fake_jwt("a@example.com", "ws-1"),
            "expires_in": 600
        }));
    }
    // 刷新：故意只回 access_token，考验「旧 refresh_token 要沿用」
    Json(json!({ "access_token": "at-refreshed", "expires_in": 300 }))
}

fn fake_jwt(email: &str, ws: &str) -> String {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
    use base64::Engine as _;
    let payload = json!({
        "email": email,
        "https://api.openai.com/auth": { "chatgpt_account_id": ws }
    });
    format!(
        "h.{}.s",
        B64URL.encode(serde_json::to_vec(&payload).unwrap())
    )
}

async fn spawn_idp() -> (SocketAddr, Seen) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Seen::default();
    let app = Router::new().route("/token", post(token)).with_state(seen.clone());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, seen)
}

fn cfg_for(vendor: Vendor, addr: SocketAddr) -> SubCfg {
    SubCfg {
        vendor,
        client_id: "test-client".into(),
        authorize_url: format!("http://{addr}/authorize"),
        token_url: format!("http://{addr}/token"),
        scopes: "openid profile".into(),
    }
}

/// 拿一个「刚空出来」的回环端口给回调监听用。
async fn free_port() -> u16 {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let p = l.local_addr().unwrap().port();
    drop(l);
    p
}

/// 等监听真的起来再打过去。绑端口发生在 `wait_callback` 里，
/// 抢先发 GET 会得到 connection refused（不是流程的问题，是测试的时序问题）。
async fn settle() {
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
}

/// 一律用不带代理的客户端：`reqwest::get` 会读 `HTTP_PROXY`，
/// 一旦机器上配了代理，对 127.0.0.1 的请求就永远不返回。
fn client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

#[tokio::test]
async fn loopback_callback_catches_the_code_and_then_exchanges_it() {
    let (addr, seen) = spawn_idp().await;
    let port = free_port().await;
    let cfg = cfg_for(Vendor::Claude, addr);
    let flow = sub::begin(&cfg, port, true);

    // 浏览器那一步：拿到回调地址、带着 state 打过来
    let target = format!(
        "{}?code=the-code&state={}",
        flow.redirect_uri, flow.state
    );
    let hit = tokio::spawn(async move {
        settle().await;
        client().get(target).send().await
    });

    let cb = sub::wait_callback(port, "/callback", 10).await.expect("应收到回调");
    assert_eq!(cb.code, "the-code");
    assert_eq!(cb.state.as_deref(), Some(flow.state.as_str()));
    assert!(hit.await.unwrap().expect("回调页应能打开").status().is_success());

    let ts = sub::exchange_code(&client(), &cfg, &flow, "the-code", 1_000)
        .await
        .expect("换 token 应成功");
    assert_eq!(ts.access_token, "at-claude");
    assert_eq!(ts.refresh_token.as_deref(), Some("rt-claude"));
    assert_eq!(ts.account.as_deref(), Some("me@example.com"));

    // Claude 必须是 JSON
    let ct = seen.content_types.lock().unwrap().first().cloned().unwrap_or_default();
    assert!(ct.contains("json"), "Claude 的换 token 请求必须是 JSON，实际 {ct}");
}

#[tokio::test]
async fn codex_uses_form_encoding_and_pulls_identity_from_the_id_token() {
    let (addr, seen) = spawn_idp().await;
    let cfg = cfg_for(Vendor::Codex, addr);
    let flow = sub::begin(&cfg, 1455, true);

    let ts = sub::exchange_code(&client(), &cfg, &flow, "c-2", 0)
        .await
        .expect("换 token 应成功");
    assert_eq!(ts.access_token, "at-codex");
    // 身份藏在 id_token 里，邮箱和工作区都要解出来
    assert_eq!(ts.account.as_deref(), Some("a@example.com"));
    assert_eq!(ts.account_id.as_deref(), Some("ws-1"), "缺了它就发不出请求");

    let ct = seen.content_types.lock().unwrap().first().cloned().unwrap_or_default();
    assert!(
        ct.contains("x-www-form-urlencoded"),
        "Codex 的换 token 请求必须是表单，实际 {ct}"
    );
}

#[tokio::test]
async fn a_foreign_state_is_refused_before_any_token_request() {
    let (addr, seen) = spawn_idp().await;
    let cfg = cfg_for(Vendor::Claude, addr);
    let flow = sub::begin(&cfg, 54545, true);

    let err = sub::exchange_code(
        &client(),
        &cfg,
        &flow,
        "some-code#someone-elses-state",
        0,
    )
    .await
    .unwrap_err();

    match err {
        AuthError::Rejected { code, .. } => assert_eq!(code, "state_mismatch"),
        other => panic!("期望 state_mismatch，实际 {other:?}"),
    }
    assert!(
        seen.content_types.lock().unwrap().is_empty(),
        "state 不对时不该把 code 发给上游"
    );
}

#[tokio::test]
async fn refresh_keeps_the_old_refresh_token_when_upstream_omits_it() {
    let (addr, _seen) = spawn_idp().await;
    let cfg = cfg_for(Vendor::Codex, addr);
    let now = chrono::Utc::now().timestamp();

    let current = freeport_auth::TokenSet {
        access_token: "at-old".into(),
        refresh_token: Some("rt-codex".into()),
        account: Some("a@example.com".into()),
        account_id: Some("ws-1".into()),
        ..Default::default()
    };
    let next = sub::refresh(&client(), &cfg, &current, now)
        .await
        .expect("刷新应成功");

    assert_eq!(next.access_token, "at-refreshed");
    assert_eq!(next.expires_at, Some(now + 300));
    // 上游只回了 access_token：丢了 rt 等于下次必须重新登录
    assert_eq!(next.refresh_token.as_deref(), Some("rt-codex"));
    // 身份信息也要继承
    assert_eq!(next.account.as_deref(), Some("a@example.com"));
    assert_eq!(next.account_id.as_deref(), Some("ws-1"));
}

#[tokio::test]
async fn waiting_for_a_callback_times_out_instead_of_hanging_forever() {
    let port = free_port().await;
    let err = sub::wait_callback(port, "/callback", 1).await.unwrap_err();
    assert!(matches!(err, AuthError::Expired), "实际 {err:?}");
}

#[tokio::test]
async fn a_stray_favicon_does_not_steal_the_callback() {
    let port = free_port().await;
    let cfg = cfg_for(Vendor::Codex, SocketAddr::from(([127, 0, 0, 1], 1)));
    let flow = sub::begin(&cfg, port, true);
    let base = format!("http://127.0.0.1:{port}");

    let noise = tokio::spawn({
        let base = base.clone();
        async move {
            settle().await;
            let _ = client().get(format!("{base}/favicon.ico")).send().await;
        }
    });
    let real = tokio::spawn({
        let url = format!("{base}/auth/callback?code=good&state={}", flow.state);
        async move {
            settle().await;
            client().get(url).send().await
        }
    });

    let cb = sub::wait_callback(port, "/auth/callback", 10)
        .await
        .expect("应跳过 favicon、收到真正的回调");
    assert_eq!(cb.code, "good");
    let _ = noise.await;
    let _ = real.await;
}
