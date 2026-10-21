//! HTTP 外壳。逻辑都在 `lib.rs`，这里只做**取身份 → 算该给什么 → 出货**。
//!
//! 三条路由各自守一件事：
//!
//! - `/v1/latest`：给 manifest。字段包清单按令牌的 `feat` 现算。
//! - `/snapshot/:name`：给快照。`live.json` 要 Pro 令牌——地址虽然只在 Pro 的
//!   manifest 里出现，但它是可猜的字符串，不守就等于所有人都能直接拿实时档。
//! - `/feat/:name`：给字段包。要令牌，且令牌的 `feat` 必须点名这一项。
//!
//! 三处都用同一个判定：[`tier_of`]。不认 `x-freeport-tier` 头——那是自述。

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use ed25519_dalek::SigningKey;
use crate::{build_manifest, sign_manifest, tier_of, Floor, Tier};
use serde::Deserialize;
use serde_json::json;

pub struct App {
    pub floor: Floor,
    pub key: SigningKey,
    pub license_pubkey: [u8; 32],
}

#[derive(Debug, Deserialize)]
struct LatestQ {
    #[serde(default)]
    since: u64,
    #[serde(default)]
    channel: Option<String>,
}

fn bearer(h: &HeaderMap) -> Option<&str> {
    h.get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::trim)
}

/// 这次的持有人能拿到哪些能力名。免费档给空表：**没有令牌就没有名单**，
/// 不是「令牌里没写就默认给」。
fn allowed_feats(claims: Option<&freeport_license::Claims>) -> Vec<String> {
    claims.map(|c| c.feat.clone()).unwrap_or_default()
}

fn denied(what: &str) -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({
            "error": "forbidden",
            "message": format!("{what}需要有效许可证，且许可证的 feat 里包含这一项"),
        })),
    )
        .into_response()
}

async fn latest(State(app): State<Arc<App>>, headers: HeaderMap, Query(q): Query<LatestQ>) -> Response {
    let now = chrono::Utc::now().timestamp();
    // 只认 token，不认 `x-freeport-tier`：那是客户端自述，可以随便写
    let (tier, claims) = tier_of(bearer(&headers), &app.license_pubkey, now);

    if q.since >= app.floor.meta.seq {
        return StatusCode::NOT_MODIFIED.into_response();
    }

    // 补丁链只对 Pro 有意义：免费包是另一份文档，拿实时链去拼会直接对不上摘要
    let patches = if tier == Tier::Pro {
        app.floor.patches_for(q.since)
    } else {
        Vec::new()
    };
    // 字段包同理——但判据不是档位，是令牌自己写着的那份 feat 名单
    let picked = app.floor.feats_for(&allowed_feats(claims.as_ref()));
    let refs: Vec<freeport_catalog::FeatRef> = picked.into_iter().map(|(r, _)| r).collect();

    let url = format!("snapshot/{}", tier.file());
    let mut m = build_manifest(
        &app.floor.meta,
        app.floor.snapshot(tier),
        tier,
        &url,
        patches,
        refs,
    );
    if let Err(e) = sign_manifest(&mut m, &app.key) {
        tracing::error!("签名失败：{e}");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    tracing::info!(
        tier = tier.as_str(),
        channel = q.channel.as_deref().unwrap_or("-"),
        seq = m.seq,
        feats = m.feat.len(),
        "manifest served"
    );
    (
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        serde_json::to_vec(&m).unwrap_or_default(),
    )
        .into_response()
}

async fn snapshot(State(app): State<Arc<App>>, Path(name): Path<String>, headers: HeaderMap) -> Response {
    let now = chrono::Utc::now().timestamp();
    let (tier, _) = tier_of(bearer(&headers), &app.license_pubkey, now);
    let bytes = match name.as_str() {
        // 实时档是卖点本身：没有有效令牌就当作不存在，而不是给一份旧的
        "live.json" if tier == Tier::Pro => &app.floor.live,
        "live.json" => return denied("实时目录"),
        "monthly.json" => &app.floor.monthly,
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    (
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        bytes.clone(),
    )
        .into_response()
}

/// 字段包。双重校验：令牌有效 **且** `feat` 点名了这一项。
///
/// 只靠 manifest 里的摘要不够——摘要只在那些拿到 manifest 的客户端手里有意义，
/// 这里要挡的是「知道文件名就直接来拿」的人。
async fn feat(State(app): State<Arc<App>>, Path(name): Path<String>, headers: HeaderMap) -> Response {
    let now = chrono::Utc::now().timestamp();
    let (_, claims) = tier_of(bearer(&headers), &app.license_pubkey, now);
    let allowed = allowed_feats(claims.as_ref());
    // 地址里带 `.json`（静态托管友好），能力名不带：比较前先脱掉后缀
    let bare = name.strip_suffix(".json").unwrap_or(&name);
    let ok = allowed.iter().any(|a| a.trim().eq_ignore_ascii_case(bare));
    if !ok {
        return denied("这个字段包");
    }
    match app.floor.feat_bytes(bare) {
        Some(b) => (
            [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
            b.to_vec(),
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn patch(State(app): State<Arc<App>>, Path(name): Path<String>) -> Response {
    match app.floor.patch_bytes(&name) {
        Some(b) => (
            [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
            b.to_vec(),
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// 排查用：这条 token 现在会被判成哪一档、属于谁、能拿哪些包。不返回 token 本身。
async fn whoami(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    let now = chrono::Utc::now().timestamp();
    let (tier, claims) = tier_of(bearer(&headers), &app.license_pubkey, now);
    Json(json!({
        "tier": tier.as_str(),
        "sub": claims.as_ref().map(|c| c.sub.clone()),
        "seats": claims.as_ref().and_then(|c| c.seats),
        "feat": allowed_feats(claims.as_ref()),
        "published": app.floor.feats.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>(),
        "seq": app.floor.meta.seq,
        "version": app.floor.meta.version,
    }))
    .into_response()
}

pub fn router(app: Arc<App>) -> Router {
    // 两组路径都注册：manifest 里的快照/补丁/字段包地址是相对 manifest 解析的，
    // 而用户填的 `feed.url` 是 `.../v1/latest`——相对解析出来一定带 `/v1/`。
    // 早先只注册了根路径，症状是「manifest 拿得到、快照 404」。
    Router::new()
        .route("/v1/latest", get(latest))
        .route("/v1/snapshot/:name", get(snapshot))
        .route("/v1/feat/:name", get(feat))
        .route("/v1/patch/:name", get(patch))
        .route("/v1/whoami", get(whoami))
        .route("/latest", get(latest))
        .route("/snapshot/:name", get(snapshot))
        .route("/feat/:name", get(feat))
        .route("/patch/:name", get(patch))
        .route("/whoami", get(whoami))
        .route("/healthz", get(|| async { "ok" }))
        .with_state(app)
}
