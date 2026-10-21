//! 目录服务端的启动壳。路由在 `freeport_catalogd::http`，纯逻辑在 `lib.rs`。
//!
//! 四个环境变量：
//! - `FREEPORT_CATALOG_DIR`：含 `meta.json` / `live.json` / `monthly.json`（可选 `patches/`、`feats/`）
//! - `FREEPORT_CATALOG_SEED`：Ed25519 私钥种子（hex 32 字节）。**必填**——
//!   没有私钥就不该起服务，否则客户端会收到一份验不过的目录，症状是「目录永远不更新」。
//! - `FREEPORT_LICENSE_PUBKEY`：许可证公钥（hex），调试用；生产用编译期内置那把。
//! - `FREEPORT_CATALOG_ADDR`：监听地址，默认 `127.0.0.1:8787`
//!
//! 监听默认只绑回环：它是服务端，不是桌面组件，但一个没配 TLS 的实例
//! 被公网直接摸到是最糟的默认值。

use std::sync::Arc;

use ed25519_dalek::SigningKey;
use freeport_catalogd::http::{router, App};
use freeport_catalogd::Floor;

fn seed_from_env() -> Result<SigningKey, String> {
    let raw = std::env::var("FREEPORT_CATALOG_SEED")
        .map_err(|_| "缺少 FREEPORT_CATALOG_SEED（hex 32 字节）".to_string())?;
    let bytes = hex::decode(raw.trim()).map_err(|_| "FREEPORT_CATALOG_SEED 不是合法 hex".to_string())?;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| "FREEPORT_CATALOG_SEED 必须是 32 字节".to_string())?;
    Ok(SigningKey::from_bytes(&arr))
}

fn license_pubkey() -> Result<[u8; 32], String> {
    if let Ok(raw) = std::env::var("FREEPORT_LICENSE_PUBKEY") {
        let bytes = hex::decode(raw.trim()).map_err(|_| "FREEPORT_LICENSE_PUBKEY 不是合法 hex".to_string())?;
        return bytes
            .try_into()
            .map_err(|_| "FREEPORT_LICENSE_PUBKEY 必须是 32 字节".to_string());
    }
    freeport_license::pubkey().ok_or_else(|| "内置许可证公钥不可用".to_string())
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let dir = match std::env::var("FREEPORT_CATALOG_DIR") {
        Ok(d) => d,
        Err(_) => {
            eprintln!("缺 FREEPORT_CATALOG_DIR");
            std::process::exit(2);
        }
    };
    let floor = match Floor::load(&dir) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("目录加载失败：{e}");
            std::process::exit(2);
        }
    };
    let key = match seed_from_env() {
        Ok(k) => k,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let license_pubkey = match license_pubkey() {
        Ok(k) => k,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };

    let addr = std::env::var("FREEPORT_CATALOG_ADDR").unwrap_or_else(|_| "127.0.0.1:8787".into());
    let feats = floor
        .feats
        .iter()
        .map(|(n, _)| n.as_str())
        .collect::<Vec<_>>()
        .join(",");
    let seq = floor.meta.seq;
    let app = Arc::new(App {
        floor,
        key,
        license_pubkey,
    });
    let router = router(app);

    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("监听 {addr} 失败：{e}");
            std::process::exit(2);
        }
    };
    // 启动行写清 seq 与字段包清单：「有没有把包发布出去」是上线后第一个要确认的事
    tracing::info!(addr = %addr, seq, feats = %feats, "catalogd listening");
    if let Err(e) = axum::serve(listener, router).await {
        eprintln!("服务退出：{e}");
        std::process::exit(1);
    }
}
