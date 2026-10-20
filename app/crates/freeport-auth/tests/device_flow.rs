//! 设备码流程对着一个假 IdP 跑完整链路。
//!
//! 真实 GitHub 需要人工点授权，没法进 CI；这里用 axum 复刻它的三个响应
//! （pending / slow_down / ready），把「等多久、怎么退避、什么时候放弃」
//! 这些容易写错的地方固定下来。

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};

use freeport_auth::device::{self, DeviceCfg, DeviceStep};
use freeport_auth::AuthError;

struct Mock {
    port: u16,
    calls: AtomicUsize,
    /// true 时 /token 一律回 access_denied
    deny: bool,
}

async fn device_code(State(m): State<Arc<Mock>>) -> Json<Value> {
    let base = format!("http://127.0.0.1:{}", m.port);
    Json(json!({
        "device_code": "dc-1",
        "user_code": "ABCD-EFGH",
        "verification_uri": format!("{base}/device"),
        "verification_uri_complete": format!("{base}/device?code=ABCD-EFGH"),
        "interval": 1,
        "expires_in": 60
    }))
}

async fn token(State(m): State<Arc<Mock>>) -> Json<Value> {
    if m.deny {
        return Json(json!({ "error": "access_denied" }));
    }
    let n = m.calls.fetch_add(1, Ordering::SeqCst);
    match n {
        0 => Json(json!({ "error": "authorization_pending" })),
        1 => Json(json!({ "error": "slow_down" })),
        _ => Json(json!({
            "access_token": "at-1",
            "token_type": "bearer",
            "scope": "read:user",
            "refresh_token": "rt-1",
            "expires_in": 3600
        })),
    }
}

async fn refresh(State(_m): State<Arc<Mock>>) -> Json<Value> {
    Json(json!({ "access_token": "at-2", "token_type": "bearer", "expires_in": 3600 }))
}

async fn spawn(deny: bool) -> (SocketAddr, Arc<Mock>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let mock = Arc::new(Mock {
        port: addr.port(),
        calls: AtomicUsize::new(0),
        deny,
    });

    let app = Router::new()
        .route("/device/code", post(device_code))
        .route("/token", post(token))
        .route("/refresh", post(refresh))
        .with_state(mock.clone());

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, mock)
}

fn cfg_for(addr: SocketAddr) -> DeviceCfg {
    DeviceCfg {
        device_code_url: format!("http://{addr}/device/code"),
        token_url: format!("http://{addr}/token"),
        client_id: "test-client".into(),
        scope: "read:user".into(),
    }
}

#[tokio::test]
async fn device_flow_waits_then_succeeds() {
    let (addr, mock) = spawn(false).await;
    let cfg = cfg_for(addr);
    let client = reqwest::Client::new();

    let start = device::start(&client, &cfg).await.expect("应拿到设备码");
    assert_eq!(start.user_code, "ABCD-EFGH");
    assert_eq!(start.interval, 1);
    assert!(start.instruction().contains("?code=ABCD-EFGH"));

    let began = std::time::Instant::now();
    let token = device::wait(&client, &cfg, &start, 30).await.expect("最终应拿到 token");
    let elapsed = began.elapsed();

    assert_eq!(token.access_token, "at-1");
    assert_eq!(token.refresh_token.as_deref(), Some("rt-1"));
    assert!(token.expires_at.is_some(), "expires_in 应换算成绝对时间");
    assert_eq!(mock.calls.load(Ordering::SeqCst), 3, "应恰好轮询三次");

    // 1s(pending) + 2s? 不对：第二次 slow_down 后间隔 +5s，所以总耗时必须 > 6s
    assert!(
        elapsed.as_secs() >= 7,
        "slow_down 必须把间隔拉长，实际只等了 {elapsed:?}"
    );
}

#[tokio::test]
async fn denial_stops_polling() {
    let (addr, mock) = spawn(true).await;
    let cfg = cfg_for(addr);
    let client = reqwest::Client::new();

    let start = device::start(&client, &cfg).await.unwrap();
    let err = device::wait(&client, &cfg, &start, 30).await.unwrap_err();
    assert!(matches!(err, AuthError::Denied), "实际 {err:?}");
    assert_eq!(mock.calls.load(Ordering::SeqCst), 0, "拒绝时不该计入正常轮询");
}

#[tokio::test]
async fn refresh_swaps_the_access_token() {
    let (addr, _mock) = spawn(false).await;
    let client = reqwest::Client::new();
    let now = chrono::Utc::now().timestamp();

    let mut cfg = cfg_for(addr);
    cfg.token_url = format!("http://{addr}/refresh");
    let ts = device::refresh(&client, &cfg, "rt-1", now)
        .await
        .map_err(|e| e.to_string())
        .expect("刷新应成功");
    assert_eq!(ts.access_token, "at-2");
    assert_eq!(ts.expires_at, Some(now + 3600));
}

#[tokio::test]
async fn pending_alone_does_not_become_a_token() {
    let (addr, _mock) = spawn(false).await;
    let client = reqwest::Client::new();
    let now = chrono::Utc::now().timestamp();

    // 第一次轮询必然是 pending，不能被当成成功
    let step = device::poll_once(&client, &cfg_for(addr), "dc-1", now)
        .await
        .unwrap();
    assert_eq!(step, DeviceStep::Pending);
}
