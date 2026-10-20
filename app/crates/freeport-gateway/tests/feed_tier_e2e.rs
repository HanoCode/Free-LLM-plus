//! Pro 数据面：feed 请求带什么身份，以及许可证不会跟着请求乱跑。
//!
//! 验收标准三条：
//! ① 免费档：只报 `x-freeport-tier: free`，**不带** `Authorization`
//! ② 激活 Pro：两个请求都带 `tier: pro` 与 `Bearer <jwt>`，服务端标注的
//!    档位落到 `servedTier`（纯展示，不参与本地判断）
//! ③ 快照跨源：地址虽在签名里，但只要和 manifest 不同源就不带 Bearer——
//!    配置是用户填的，凭据不该被送到第三方域名
//!
//! 一个 `#[tokio::test]` 走完：`FREEPORT_DATA_DIR` 与
//! `FREEPORT_LICENSE_PUBKEY` 都是进程级环境变量。

use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::get;
use axum::Router;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use freeport_core::dto::FeedPatch;
use freeport_gateway::Gateway;
use freeport_license::issuer::Issuer;
use freeport_license::Claims;
use serde_json::json;
use sha2::{Digest, Sha256};

fn signing_key() -> SigningKey {
    SigningKey::from_bytes(&[11u8; 32])
}

fn pubkey_hex() -> String {
    hex::encode(signing_key().verifying_key().to_bytes())
}

fn manifest_bytes(seq: u64, snap: &[u8], tier: Option<&str>, snap_url: &str) -> Vec<u8> {
    let mut m = freeport_catalog::Manifest {
        version: format!("2026.10.19.{seq}"),
        channel: "stable".into(),
        seq,
        published_at: "2026-10-19T03:00:00Z".into(),
        sha256: hex::encode(Sha256::digest(snap)),
        sig: String::new(),
        url: snap_url.into(),
        tier: tier.map(str::to_string),
        notes: None,
        patches: Vec::new(),
    };
    let payload = freeport_catalog::jcs::canonical(&m.signing_payload()).unwrap();
    m.sig = base64::engine::general_purpose::STANDARD
        .encode(signing_key().sign(payload.as_bytes()).to_bytes());
    serde_json::to_vec(&m).unwrap()
}

/// 一次请求留下了什么。只记我们关心的两个头。
#[derive(Debug, Clone)]
struct Hit {
    tier: Option<String>,
    auth: Option<String>,
}

type Log = Arc<Mutex<Vec<Hit>>>;
type Feed = (Arc<Vec<u8>>, Arc<Vec<u8>>, Log);

async fn record(State((manifest, snap, log)): State<Feed>, uri: axum::http::Uri, headers: HeaderMap) -> Vec<u8> {
    let h = |k: &str| {
        headers
            .get(k)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    };
    log.lock().unwrap().push(Hit {
        tier: h("x-freeport-tier"),
        auth: h("authorization"),
    });
    if uri.path().ends_with("latest.json") {
        (*manifest).clone()
    } else {
        (*snap).clone()
    }
}

/// 起一个只吐固定字节的服务器，返回地址与请求记录。
/// manifest 与快照分两条路由，各自一份内容——共用一份会被摘要校验拦下。
async fn serve(manifest: Vec<u8>, snap: Vec<u8>) -> (String, Log) {
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new()
        .route("/latest.json", get(record))
        .route("/snap.json", get(record))
        .with_state((Arc::new(manifest), Arc::new(snap), log.clone()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), log)
}

fn hits(log: &Log) -> Vec<Hit> {
    log.lock().unwrap().clone()
}

fn seed(dir: &std::path::Path) {
    std::fs::write(dir.join("freeport.json"), json!({ "version": 1 }).to_string()).unwrap();
}

#[tokio::test]
async fn feed_carries_identity_but_never_leaks_the_license() {
    let dir = std::env::temp_dir().join(format!("fp-tier-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("FREEPORT_DATA_DIR", &dir);
    // 签发用的测试公钥要在第一次验签前就位（debug 构建才会读）
    let iss = Issuer::generate();
    std::env::set_var("FREEPORT_LICENSE_PUBKEY", iss.public_b64());
    seed(&dir);

    let gw = Gateway::new();
    gw.load().await;

    let snap1 = serde_json::to_vec(&json!({
        "seq": 1,
        "version": "2026.10.19.1",
        "providers": [{ "id": "p1", "name": "P1", "freeTier": true, "models": [{ "id": "m1", "free": true }] }]
    }))
    .unwrap();

    /* ── ① 没激活：只报档位，不带凭据 ── */

    let (base1, log1) = serve(manifest_bytes(1, &snap1, None, "snap.json"), snap1.clone()).await;
    gw.set_feed(FeedPatch {
        enabled: Some(true),
        url: Some(format!("{base1}/latest.json")),
        channel: None,
        policy: Some("auto".into()),
        pubkey: Some(pubkey_hex()),
        check_minutes: Some(1),
    })
    .await;

    let f = gw.feed_check(true).await;
    assert_eq!(f.seq, 1, "{:?}", f.last_error);
    assert!(f.served_tier.is_none(), "manifest 没标档位，就不该凭空猜一个");

    let seen = hits(&log1);
    assert_eq!(seen.len(), 2, "manifest 与快照各一次：{seen:?}");
    for h in &seen {
        assert_eq!(h.tier.as_deref(), Some("free"), "免费档要明说自己是 free");
        assert!(
            h.auth.is_none(),
            "没有许可证时绝不能带 Authorization：{:?}",
            h.auth
        );
    }

    /* ── ② 激活 Pro：两个请求都带 Bearer，档位落库 ── */

    let now = chrono::Utc::now().timestamp();
    let token = iss.sign(&Claims::pro("pro@example.com", 30, now));
    gw.activate_license(&token).await.unwrap();

    let snap2 = serde_json::to_vec(&json!({
        "seq": 2,
        "version": "2026.10.19.2",
        "providers": [{ "id": "p1", "quota": { "rpd": 5000 } }],
        "events": [{ "kind": "quota.changed", "title": "p1 额度上调" }]
    }))
    .unwrap();
    let (base2, log2) = serve(manifest_bytes(2, &snap2, Some("pro"), "snap.json"), snap2.clone()).await;
    gw.set_feed(FeedPatch {
        enabled: Some(true),
        url: Some(format!("{base2}/latest.json")),
        channel: None,
        policy: Some("auto".into()),
        pubkey: Some(pubkey_hex()),
        check_minutes: Some(1),
    })
    .await;

    let f = gw.feed_check(true).await;
    assert_eq!(f.seq, 2, "{:?}", f.last_error);
    assert_eq!(f.served_tier.as_deref(), Some("pro"));

    let want = format!("Bearer {token}");
    let seen = hits(&log2);
    assert_eq!(seen.len(), 2, "manifest 与快照各一次：{seen:?}");
    for h in &seen {
        assert_eq!(h.tier.as_deref(), Some("pro"));
        assert_eq!(h.auth.as_deref(), Some(want.as_str()), "Pro 的请求要带签名");
    }

    /* ── ③ 快照跨源：仍然报档位，但不带 Bearer ── */

    let snap3 = serde_json::to_vec(&json!({
        "seq": 3,
        "version": "2026.10.19.3",
        "providers": [{ "id": "p1", "freeNote": "CDN 上的快照" }]
    }))
    .unwrap();
    let (cdn, log3) = serve(snap3.clone(), snap3.clone()).await;
    let abs = format!("{cdn}/snap.json");
    let (base3, log4) = serve(manifest_bytes(3, &snap3, Some("pro"), &abs), snap3.clone()).await;
    gw.set_feed(FeedPatch {
        enabled: Some(true),
        url: Some(format!("{base3}/latest.json")),
        channel: None,
        policy: Some("auto".into()),
        pubkey: Some(pubkey_hex()),
        check_minutes: Some(1),
    })
    .await;

    let f = gw.feed_check(true).await;
    assert_eq!(f.seq, 3, "{:?}", f.last_error);

    let m = hits(&log4);
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].tier.as_deref(), Some("pro"));
    assert_eq!(
        m[0].auth.as_deref(),
        Some(want.as_str()),
        "manifest 和它同源，凭据该带"
    );

    let s = hits(&log3);
    assert_eq!(s.len(), 1);
    assert_eq!(s[0].tier.as_deref(), Some("pro"), "档位不是秘密，照报");
    assert!(
        s[0].auth.is_none(),
        "跨源快照不该收到本机许可证：{:?}",
        s[0].auth
    );

    /* ── ④ 解除许可证：下一个请求立刻退回 free，不需要重启 ── */

    gw.clear_license().await;
    let snap4 = serde_json::to_vec(&json!({ "seq": 4, "version": "2026.10.19.4" })).unwrap();
    let (base4, log5) = serve(manifest_bytes(4, &snap4, None, "snap.json"), snap4.clone()).await;
    gw.set_feed(FeedPatch {
        enabled: Some(true),
        url: Some(format!("{base4}/latest.json")),
        channel: None,
        policy: Some("auto".into()),
        pubkey: Some(pubkey_hex()),
        check_minutes: Some(1),
    })
    .await;
    let f = gw.feed_check(true).await;
    assert_eq!(f.seq, 4, "{:?}", f.last_error);
    for h in hits(&log5) {
        assert_eq!(h.tier.as_deref(), Some("free"));
        assert!(h.auth.is_none(), "解除后不能再带旧 token");
    }

    let _ = std::fs::remove_dir_all(&dir);
}
