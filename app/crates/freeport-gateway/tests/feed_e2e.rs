//! 签名目录 feed 的端到端验证。
//!
//! 三个场景对应三条验收标准：
//! ① feed 里加一个模型 → 目录里出现且带 candidate 标记（0.3 倍权重）
//! ② feed 里下架一个模型 → 路由立刻不再选它
//! ③ 篡改签名 → 拒绝应用，目录一个字节都不动
//!
//! 放在同一个 `#[tokio::test]` 里：`FREEPORT_DATA_DIR` 是进程级环境变量，
//! 拆成多个测试并行跑会互相串目录。

use std::path::Path;
use std::sync::Arc;

use axum::routing::get;
use axum::Router;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use freeport_catalog::{Catalog, CatalogModel, CatalogProvider};
use freeport_core::dto::FeedPatch;
use freeport_gateway::{secrets, Gateway};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

fn signing_key() -> SigningKey {
    // 固定种子：测试要可重复，不需要真随机
    SigningKey::from_bytes(&[7u8; 32])
}

fn pubkey_hex() -> String {
    hex::encode(signing_key().verifying_key().to_bytes())
}

fn manifest_bytes(seq: u64, snap: &[u8]) -> Vec<u8> {
    manifest_bytes_with(seq, snap, "snap.json", Vec::new())
}

fn manifest_bytes_with(
    seq: u64,
    snap_or_patch: &[u8],
    snap_url: &str,
    patches: Vec<freeport_catalog::PatchRef>,
) -> Vec<u8> {
    let mut m = freeport_catalog::Manifest {
        version: format!("2026.10.19.{seq}"),
        channel: "stable".into(),
        seq,
        published_at: "2026-10-19T03:00:00Z".into(),
        sha256: hex::encode(Sha256::digest(snap_or_patch)),
        sig: String::new(),
        url: snap_url.into(),
        tier: None,
        notes: None,
        patches,
    };
    let payload = freeport_catalog::jcs::canonical(&m.signing_payload()).unwrap();
    m.sig = base64::engine::general_purpose::STANDARD
        .encode(signing_key().sign(payload.as_bytes()).to_bytes());
    serde_json::to_vec(&m).unwrap()
}

/// 一个按路径查表的静态服务器。缺的路径回 404——
/// 增量测试靠「全量不存在」这一点来证明它真的没下全量。
async fn serve_files(files: Vec<(String, Vec<u8>)>) -> String {
    use axum::response::IntoResponse;
    use std::collections::HashMap;

    let map: Arc<HashMap<String, Vec<u8>>> =
        Arc::new(files.into_iter().collect::<HashMap<_, _>>());
    let app = Router::new().route(
        "/*path",
        get(move |axum::extract::Path(p): axum::extract::Path<String>| {
            let map = map.clone();
            async move {
                match map.get(&p) {
                    Some(b) => Ok(([("content-type", "application/json")], b.clone()).into_response()),
                    None => Err(axum::http::StatusCode::NOT_FOUND),
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}/latest.json")
}

async fn serve(manifest: Vec<u8>, snap: Vec<u8>) -> String {
    let m = Arc::new(manifest);
    let s = Arc::new(snap);
    let app = Router::new()
        .route(
            "/latest.json",
            get(move || {
                let m = m.clone();
                async move { ([("content-type", "application/json")], (*m).clone()) }
            }),
        )
        .route(
            "/snap.json",
            get(move || {
                let s = s.clone();
                async move { ([("content-type", "application/json")], (*s).clone()) }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}/latest.json")
}

fn seed(dir: &Path) {
    std::fs::write(
        dir.join("freeport.json"),
        json!({
            "version": 1,
            "providers": [{
                "id": "p1",
                "name": "P1",
                "baseUrl": "http://127.0.0.1:9/v1",
                "tier": "A",
                "keyMask": "k••••k",
                "enabled": true,
                "addedAt": "2026-10-19T00:00:00Z",
                "models": [{ "id": "m1" }, { "id": "m2" }],
                "health": { "status": "unknown" }
            }],
            "routing": { "chain": [] }
        })
        .to_string(),
    )
    .unwrap();

    let cat = Catalog {
        providers: vec![CatalogProvider {
            id: "p1".into(),
            name: "P1".into(),
            tier: "A".into(),
            free_tier: true,
            source: "baseline".into(),
            models: vec![
                CatalogModel {
                    id: "m1".into(),
                    name: "M1".into(),
                    free: true,
                    ..Default::default()
                },
                CatalogModel {
                    id: "m2".into(),
                    name: "M2".into(),
                    free: true,
                    ..Default::default()
                },
            ],
            ..Default::default()
        }],
        ..Default::default()
    };
    freeport_catalog::store::save(&cat);
}

fn candidate_of(models: &[freeport_core::dto::GwModel], id: &str) -> Option<bool> {
    models
        .iter()
        .find(|m| m.id == id)
        .and_then(|m| m.freeport.clone())
        .and_then(|f| f.candidate)
}

#[tokio::test]
async fn feed_adds_candidates_withdraws_models_and_rejects_tampering() {
    let dir = std::env::temp_dir().join(format!("fp-feed-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("FREEPORT_DATA_DIR", &dir);

    seed(&dir);
    secrets::set_key("p1", "test-key").unwrap();

    let gw = Gateway::new();
    gw.load().await;

    /* ── 场景 1：正常应用（新增 m3、下架 m2）── */

    let snap = json!({
        "seq": 1,
        "version": "2026.10.19.1",
        "providers": [{
            "id": "p1",
            "freeTier": true,
            "freeNote": "1000 RPD",
            "models": [{ "id": "m3", "free": true, "context": 32768, "tools": true }]
        }],
        "tombstones": ["p1:m2"]
    });
    let snap_bytes = serde_json::to_vec(&snap).unwrap();
    let url = serve(manifest_bytes(1, &snap_bytes), snap_bytes).await;

    let f = gw
        .set_feed(FeedPatch {
            enabled: Some(true),
            url: Some(url),
            channel: None,
            policy: Some("auto".into()),
            pubkey: Some(pubkey_hex()),
            check_minutes: Some(1),
        })
        .await;
    assert!(f.configured, "配好地址后应当算已配置");

    let f = gw.feed_check(true).await;
    assert_eq!(f.seq, 1, "应当应用 seq 1：{:?}", f.last_error);
    assert!(f.last_error.is_none(), "{:?}", f.last_error);
    assert!(f.halted.is_none(), "{:?}", f.halted);
    assert_eq!(f.version, "2026.10.19.1");
    assert!(f.tombstones.contains(&"p1:m2".to_string()));
    assert!(
        f.events.iter().any(|e| e.kind == "model.added"),
        "新模型必须产生一条变更事件：{:?}",
        f.events
    );

    // ① 新模型真的进了厂商模型表，并且带 candidate
    let models = gw.models().await;
    assert_eq!(
        candidate_of(&models, "p1:m3"),
        Some(true),
        "feed 新加的模型必须标 candidate"
    );

    // 目录里也带上了 feed 的免费情报
    let cat = gw.catalog().await;
    let p1 = cat.providers.iter().find(|p| p.id == "p1").unwrap();
    assert_eq!(p1.free_note.as_deref(), Some("1000 RPD"));

    // ② 下架的模型立刻不路由：直接点名它，也必须被 Gate 拦下
    let route: Value = gw.explain_route("p1:m2", "").await;
    let rejected = route["rejected"].as_array().unwrap();
    assert!(
        rejected
            .iter()
            .any(|r| r["model"] == "m2" && r["kind"] == "tombstone"),
        "m2 应当以 tombstone 被拦下：{rejected:?}"
    );
    assert!(
        route["chain"]
            .as_array()
            .unwrap()
            .iter()
            .all(|h| h["model"] != "m2"),
        "下架模型不能出现在候选链里：{}",
        route["chain"]
    );
    // auto 链同样绕开它
    let auto: Value = gw.explain_route("auto", "").await;
    assert!(
        auto["chain"]
            .as_array()
            .unwrap()
            .iter()
            .all(|h| h["model"] != "m2"),
        "auto 链不能选中下架模型：{}",
        auto["chain"]
    );

    /* ── 场景 2：篡改签名 ── */

    let snap2 = json!({
        "seq": 2,
        "version": "2026.10.19.2",
        "providers": [{
            "id": "p1",
            "models": [{ "id": "evil", "free": true }]
        }]
    });
    let snap2_bytes = serde_json::to_vec(&snap2).unwrap();
    // 签好之后改 seq：签名覆盖 seq，必须失效
    let mut m2: Value = serde_json::from_slice(&manifest_bytes(2, &snap2_bytes)).unwrap();
    m2["seq"] = json!(9);
    let url2 = serve(serde_json::to_vec(&m2).unwrap(), snap2_bytes).await;
    gw.set_feed(FeedPatch {
        url: Some(url2),
        ..Default::default()
    })
    .await;

    let f = gw.feed_check(true).await;
    assert_eq!(f.seq, 1, "验签失败不能推进版本");
    let err = f.last_error.clone().unwrap_or_default();
    assert!(err.contains("签名"), "错误要说清是签名问题：{err}");
    assert_eq!(f.rejected, 1);
    let models = gw.models().await;
    assert!(
        models.iter().all(|m| m.id != "p1:evil"),
        "篡改的 feed 绝不能改变目录"
    );

    /* ── 场景 3：notify 策略只报信，不动目录 ── */

    let snap3 = json!({
        "seq": 3,
        "version": "2026.10.19.3",
        "providers": [{ "id": "p1", "models": [{ "id": "m4", "free": true }] }]
    });
    let snap3_bytes = serde_json::to_vec(&snap3).unwrap();
    let url3 = serve(manifest_bytes(3, &snap3_bytes), snap3_bytes).await;
    gw.set_feed(FeedPatch {
        url: Some(url3),
        policy: Some("notify".into()),
        ..Default::default()
    })
    .await;

    let f = gw.feed_check(true).await;
    assert_eq!(f.seq, 1, "notify 不自动应用");
    let pending = f.pending.expect("应当有待确认版本");
    assert_eq!(pending.seq, 3);
    assert!(
        gw.models().await.iter().all(|m| m.id != "p1:m4"),
        "没点应用之前不能进目录"
    );

    // 用户确认后应用
    let f = gw.feed_apply().await;
    assert_eq!(f.seq, 3, "确认后应当应用 seq 3：{:?}", f.last_error);
    assert!(f.pending.is_none());
    assert!(
        gw.models().await.iter().any(|m| m.id == "p1:m4"),
        "确认后模型必须出现"
    );

    /* ── 场景 4：只有补丁、没有全量也必须能更新 ──
     * 服务端故意不提供 snap4.json：谁去下全量谁 404。
     * 所以「seq 推进到 4」就等于证明这次真走了增量。 */

    let s3: freeport_catalog::Snapshot = {
        let raw = std::fs::read(freeport_catalog::store::snapshot_file())
            .expect("应用成功后要把快照存下来，否则下一轮永远只能下全量");
        serde_json::from_slice(&raw).unwrap()
    };
    assert_eq!(s3.seq, 3, "补丁基线应当是上次应用的那一份");

    let s4v = json!({
        "seq": 4,
        "version": "2026.10.19.4",
        "providers": [{
            "id": "p1",
            "models": [
                { "id": "m4", "free": true, "context": 8192 },
                { "id": "m5", "free": true }
            ]
        }]
    });
    let s4: freeport_catalog::Snapshot = serde_json::from_value(s4v.clone()).unwrap();
    let ops = freeport_catalog::feed::diff(&freeport_catalog::feed::snapshot_value(&s3), &s4v);
    let pf4 = serde_json::to_vec(&json!({
        "fromSeq": 3,
        "toSeq": 4,
        "ops": ops,
        "sha256After": freeport_catalog::feed::snapshot_digest(&s4),
    }))
    .unwrap();

    let m4 = manifest_bytes_with(
        4,
        b"placeholder",
        "missing-snap.json",
        vec![freeport_catalog::PatchRef {
            from_seq: 3,
            to_seq: 4,
            url: "p3-4.json".into(),
            sha256: hex::encode(Sha256::digest(&pf4)),
        }],
    );
    let url4 = serve_files(vec![
        ("latest.json".into(), m4),
        ("p3-4.json".into(), pf4),
    ])
    .await;
    gw.set_feed(FeedPatch {
        url: Some(url4),
        policy: Some("auto".into()),
        ..Default::default()
    })
    .await;

    let f = gw.feed_check(true).await;
    assert_eq!(f.seq, 4, "只有补丁也该更新成功：{:?}", f.last_error);
    assert!(f.last_error.is_none(), "{:?}", f.last_error);
    assert!(
        gw.models().await.iter().any(|m| m.id == "p1:m5"),
        "补丁新增的模型必须进目录"
    );

    /* ── 场景 5：补丁被改一个字节 → 摘要不符 → 回退全量 → 全量也没有 → 失败 ── */

    let s5v = json!({
        "seq": 5,
        "version": "2026.10.19.5",
        "providers": [{
            "id": "p1",
            "models": [{ "id": "m4", "free": true, "context": 8192 }]
        }]
    });
    let s5: freeport_catalog::Snapshot = serde_json::from_value(s5v.clone()).unwrap();
    // 删除第 0 个（m4），但摘要按 s5 填 —— 应用后必然对不上
    let bad_pf = serde_json::to_vec(&json!({
        "fromSeq": 4,
        "toSeq": 5,
        "ops": [{ "op": "remove", "path": "/providers/0/models/0" }],
        "sha256After": freeport_catalog::feed::snapshot_digest(&s5),
    }))
    .unwrap();
    let m5b = manifest_bytes_with(
        5,
        b"placeholder",
        "missing-snap.json",
        vec![freeport_catalog::PatchRef {
            from_seq: 4,
            to_seq: 5,
            url: "p4-5.json".into(),
            sha256: hex::encode(Sha256::digest(&bad_pf)),
        }],
    );
    let url5 = serve_files(vec![
        ("latest.json".into(), m5b),
        ("p4-5.json".into(), bad_pf),
    ])
    .await;
    gw.set_feed(FeedPatch {
        url: Some(url5),
        policy: Some("auto".into()),
        ..Default::default()
    })
    .await;

    let f = gw.feed_check(true).await;
    assert_eq!(f.seq, 4, "坏补丁绝不能推进版本");
    assert!(f.last_error.is_some(), "失败要说清原因");
    assert!(
        gw.models().await.iter().any(|m| m.id == "p1:m5"),
        "失败时目录一个字节都不该动"
    );

    /* ── 场景 6：本地没有基线快照 → 不硬拼，直接下全量 ── */

    std::fs::remove_file(freeport_catalog::store::snapshot_file()).unwrap();
    let s5_bytes = serde_json::to_vec(&s5).unwrap();
    let m5 = manifest_bytes_with(5, &s5_bytes, "snap5.json", Vec::new());
    let url6 = serve_files(vec![
        ("latest.json".into(), m5),
        ("snap5.json".into(), s5_bytes),
    ])
    .await;
    gw.set_feed(FeedPatch {
        url: Some(url6),
        policy: Some("auto".into()),
        ..Default::default()
    })
    .await;

    let f = gw.feed_check(true).await;
    assert_eq!(f.seq, 5, "没有基线时应当回退全量：{:?}", f.last_error);
    assert!(f.last_error.is_none(), "{:?}", f.last_error);

    let _ = std::fs::remove_dir_all(&dir);
}
