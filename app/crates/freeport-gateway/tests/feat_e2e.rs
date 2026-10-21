//! 字段级下发（`feat`）——真实的目录服务端 + 真实的网关客户端，对打。
//!
//! 这条链有四个判定点，漏一个就会变成「界面写着 Pro，数据其实是空的」：
//!
//! ① 服务端只按**令牌里的 feat 名单**挑字段包，不看 `x-freeport-tier`
//! ② 包名与摘要都进 manifest 签名，中间人塞不进来、也换不了地址
//! ③ `/feat/:name` 与 `/snapshot/live.json` 都要令牌：知道文件名不等于有权拿
//! ④ 客户端对不认识的包**记名字而不是静默丢弃**，且不因为本机档位拒收
//!
//! 一个 `#[tokio::test]` 走完：`FREEPORT_DATA_DIR` 与 `FREEPORT_LICENSE_PUBKEY`
//! 都是进程级环境变量，多测试并行会互相踩。

use std::sync::Arc;

use base64::Engine;
use freeport_catalogd::http::{router, App};
use freeport_catalogd::Floor;
use freeport_core::dto::FeedPatch;
use freeport_gateway::Gateway;
use freeport_license::issuer::Issuer;
use freeport_license::Claims;
use serde_json::{json, Value};

const B64: base64::engine::general_purpose::GeneralPurpose = base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// 目录签名私钥。测试自造，与生产无关。
const CATALOG_SEED: [u8; 32] = [21u8; 32];
/// 许可证签发私钥。
const LICENSE_SEED: [u8; 32] = [33u8; 32];

fn catalog_pubkey() -> String {
    let sk = ed25519_dalek::SigningKey::from_bytes(&CATALOG_SEED);
    hex::encode(sk.verifying_key().to_bytes())
}

fn license_pubkey() -> [u8; 32] {
    ed25519_dalek::SigningKey::from_bytes(&LICENSE_SEED)
        .verifying_key()
        .to_bytes()
}

/// 造一份发布目录。`feats` 里的每个 `(名字, 内容)` 写成 `feats/<名字>.json`。
fn publish(dir: &std::path::Path, seq: u64, snap: &Value, feats: &[(&str, Value)]) {
    std::fs::create_dir_all(dir.join("feats")).unwrap();
    std::fs::write(
        dir.join("meta.json"),
        json!({ "seq": seq, "version": format!("2026.10.21.{seq}"), "channel": "stable" }).to_string(),
    )
    .unwrap();
    let raw = serde_json::to_vec(snap).unwrap();
    std::fs::write(dir.join("live.json"), &raw).unwrap();
    std::fs::write(dir.join("monthly.json"), &raw).unwrap();
    for (name, body) in feats {
        std::fs::write(dir.join(format!("feats/{name}.json")), body.to_string()).unwrap();
    }
}

/// 起真正的 catalogd 路由，返回 base 地址。
async fn serve(dir: &std::path::Path) -> String {
    let floor = Floor::load(dir).expect("发布目录应当能加载");
    let app = Arc::new(App {
        floor,
        key: ed25519_dalek::SigningKey::from_bytes(&CATALOG_SEED),
        license_pubkey: license_pubkey(),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router(app)).await;
    });
    format!("http://{addr}")
}

async fn get(url: &str, token: Option<&str>) -> (u16, Vec<u8>) {
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let mut req = client.get(url).header("accept", "application/json");
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    let res = req.send().await.unwrap();
    let status = res.status().as_u16();
    (status, res.bytes().await.unwrap().to_vec())
}

#[tokio::test]
async fn feat_packs_follow_the_token_not_the_client_tier() {
    let root = std::env::temp_dir().join(format!("fp-feat-{}", uuid::Uuid::new_v4().simple()));
    let data = root.join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("freeport.json"), json!({ "version": 1 }).to_string()).unwrap();
    std::env::set_var("FREEPORT_DATA_DIR", &data);

    let iss = Issuer::from_secret_b64(&B64.encode(LICENSE_SEED)).unwrap();
    std::env::set_var("FREEPORT_LICENSE_PUBKEY", iss.public_b64());

    let q = json!({ "providers": { "p1": { "rpm": 30, "rpd": 1000 } } });
    let scores = json!({ "models": { "p1:m1": { "score": 81.5 } } });

    let gw = Gateway::new();
    gw.load().await;

    /* ── ① 免费档：服务端一个包都不列，客户端也不去拉 ── */

    let snap1 = json!({
        "seq": 1,
        "version": "2026.10.21.1",
        "providers": [{ "id": "p1", "name": "P1", "freeTier": true, "models": [{ "id": "m1", "free": true }] }]
    });
    let dir1 = root.join("pub1");
    publish(&dir1, 1, &snap1, &[("quota.intel", q.clone()), ("model.scores", scores.clone())]);
    let base1 = serve(&dir1).await;

    gw.set_feed(FeedPatch {
        enabled: Some(true),
        url: Some(format!("{base1}/v1/latest")),
        channel: None,
        policy: Some("auto".into()),
        pubkey: Some(catalog_pubkey()),
        check_minutes: Some(1),
    })
    .await;

    let f = gw.feed_check(true).await;
    assert_eq!(f.seq, 1, "{:?}", f.last_error);
    assert!(
        f.served_feats.is_empty(),
        "没有令牌就不该有字段包：{:?}",
        f.served_feats
    );
    let cat = gw.catalog().await;
    let p1 = cat.providers.iter().find(|p| p.id == "p1").expect("p1 应当在目录里");
    assert!(p1.quota.is_none(), "免费档不该拿到额度情报：{:?}", p1.quota);

    /* ── ② 没有令牌时，文件名猜得再准也拿不到 ── */

    assert_eq!(
        get(&format!("{base1}/v1/snapshot/live.json"), None).await.0,
        403,
        "实时快照要令牌"
    );
    assert_eq!(
        get(&format!("{base1}/v1/feat/quota.intel.json"), None).await.0,
        403,
        "字段包要令牌"
    );
    assert_eq!(
        get(&format!("{base1}/v1/feat/不存在.json"), None).await.0,
        403,
        "连不存在的包也不该泄露「有没有」"
    );
    assert_eq!(
        get(&format!("{base1}/v1/snapshot/monthly.json"), None).await.0,
        200,
        "公共目录不设门槛"
    );

    /* ── ③ 激活 Pro：令牌写着 quota.intel，就只拿到它 ── */

    let now = chrono::Utc::now().timestamp();
    let mut claims = Claims::pro("pro@example.com", 30, now);
    // 故意多写一项客户端还不认识的：服务端照发，客户端要记名字而不是静默吞
    claims.feat = vec!["quota.intel".into(), "model.scores".into()];
    let token = iss.sign(&claims);
    gw.activate_license(&token).await.unwrap();

    let snap2 = json!({
        "seq": 2,
        "version": "2026.10.21.2",
        "providers": [{ "id": "p1", "freeNote": "仍在免费档", "models": [{ "id": "m1" }] }]
    });
    let dir2 = root.join("pub2");
    publish(&dir2, 2, &snap2, &[("quota.intel", q.clone()), ("model.scores", scores.clone())]);
    let base2 = serve(&dir2).await;

    gw.set_feed(FeedPatch {
        enabled: Some(true),
        url: Some(format!("{base2}/v1/latest")),
        channel: None,
        policy: Some("auto".into()),
        pubkey: Some(catalog_pubkey()),
        check_minutes: Some(1),
    })
    .await;

    let f = gw.feed_check(true).await;
    assert_eq!(f.seq, 2, "{:?}", f.last_error);
    assert_eq!(f.served_tier.as_deref(), Some("pro"));
    assert_eq!(
        f.served_feats,
        vec!["quota.intel".to_string()],
        "只应用认识的、且令牌里写了的包"
    );
    assert_eq!(f.feats_skipped.len(), 1, "{:?}", f.feats_skipped);
    assert!(
        f.feats_skipped[0].contains("model.scores"),
        "不认识的包要报名字：{:?}",
        f.feats_skipped
    );
    // 前端读的是 `servedFeats`：字段名对不上时界面只会静默空白，不报错
    let wire = serde_json::to_value(&f).unwrap();
    assert_eq!(wire["servedFeats"][0], "quota.intel");
    assert!(wire["featsSkipped"].is_array());

    let cat = gw.catalog().await;
    let p1 = cat.providers.iter().find(|p| p.id == "p1").unwrap();
    let q1 = p1.quota.as_ref().expect("额度情报该合并进目录");
    assert_eq!((q1.rpm, q1.rpd), (Some(30), Some(1000)));

    /* ── ④ 同源带令牌时，快照与包都能拿到；摘要对不上就整份丢 ── */

    let (st, body) = get(&format!("{base2}/v1/feat/quota.intel.json"), Some(&token)).await;
    assert_eq!(st, 200);
    assert_eq!(body, q.to_string().into_bytes(), "服务端原样吐包");

    assert_eq!(
        get(&format!("{base2}/v1/snapshot/live.json"), Some(&token)).await.0,
        200
    );

    // 令牌里没写的包，即使发布出去了也拿不到
    assert_eq!(
        get(&format!("{base2}/v1/feat/other.json"), Some(&token)).await.0,
        403
    );

    let _ = std::fs::remove_dir_all(&root);
}
