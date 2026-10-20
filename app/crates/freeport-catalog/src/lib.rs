//! Freeport 的目录层。
//!
//! 契约：
//! - 永远有数据可用（内置基线打包在二进制里，断网也能启动）
//! - 刷新失败不清空旧目录，只把错误写进 `meta.error`
//! - 免费判定只看价格字段与目录显式标注，不看名字

pub mod baseline;
pub mod feed;
pub mod jcs;
pub mod modelsdev;
pub mod store;
pub mod types;

use std::time::Duration;

pub use feed::{Applied, Manifest, PatchFile, PatchRef, Poll, PollCfg, Reject, Snapshot};
pub use types::{
    model_is_free, Catalog, CatalogModel, CatalogProvider, Quota, MODELS_DEV_URL,
};

/// 本地缓存优先，缓存缺失回落基线。
pub fn load() -> Catalog {
    store::load().unwrap_or_else(baseline::catalog)
}

/// 目录是否该自动刷新：没有缓存、缓存来自基线、或已过期。
pub fn needs_refresh(cat: &Catalog, max_age_hours: i64) -> bool {
    if cat.meta.source != "models.dev" {
        return true;
    }
    let Some(fetched) = cat.meta.fetched_at.as_deref() else {
        return true;
    };
    let Some(then) = freeport_core::model::parse_rfc3339_ms(fetched) else {
        return true;
    };
    let age_ms = chrono::Utc::now().timestamp_millis() - then;
    age_ms > max_age_hours * 3_600_000
}

/// 拉 models.dev 并合并。成功才落盘；失败保留旧目录。
pub async fn refresh(client: &reqwest::Client) -> Result<Catalog, String> {
    let res = client
        .get(MODELS_DEV_URL)
        .timeout(Duration::from_secs(45))
        .header("accept", "application/json")
        .send()
        .await
        .map_err(|e| format!("拉取目录失败：{e}"))?;

    let status = res.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(format!("models.dev 返回 {status}"));
    }

    let bytes = res
        .bytes()
        .await
        .map_err(|e| format!("读取目录响应失败：{e}"))?;
    if bytes.len() < 1024 {
        return Err("models.dev 返回内容过短，可能是错误页".into());
    }

    let remote = modelsdev::parse(&bytes)?;
    if remote.len() < 20 {
        return Err(format!("目录解析结果异常：只有 {} 个厂商", remote.len()));
    }

    let fetched_at = chrono::Utc::now().to_rfc3339();
    let cat = modelsdev::merge(baseline::load(), remote, fetched_at);
    store::save(&cat);
    tracing::info!(
        providers = cat.meta.providers,
        models = cat.meta.models,
        free = cat.meta.free_models,
        "catalog refreshed"
    );
    Ok(cat)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baseline_fallback_is_always_available() {
        let cat = baseline::catalog();
        assert!(!cat.providers.is_empty());
        assert!(cat.providers.iter().any(|p| p.free_tier));
        assert!(needs_refresh(&cat, 24));
    }

    #[test]
    fn enrich_matches_openrouter_free_suffix() {
        let cat = Catalog {
            meta: Default::default(),
            providers: vec![CatalogProvider {
                id: "openrouter".into(),
                name: "OpenRouter".into(),
                api: None,
                docs: None,
                tier: "A".into(),
                free_tier: true,
                free_note: None,
                quota: None,
                source: "test".into(),
                models: vec![CatalogModel {
                    id: "deepseek/deepseek-r1:free".into(),
                    free: true,
                    context: Some(163_840),
                    ..Default::default()
                }],
            }],
        };
        // 上游 /models 返回的是不带 `:free` 的 id，也要能对上
        assert!(cat.find_model("openrouter", "deepseek/deepseek-r1").is_some());
        assert!(cat.find_model("openrouter", "deepseek/deepseek-r1:free").is_some());
    }

    /// 真拉一次 models.dev。默认跳过：离线环境不该挂。
    /// 手动跑：`cargo test -p freeport-catalog -- --ignored live_refresh`
    #[tokio::test]
    #[ignore = "需要网络"]
    async fn live_refresh_from_models_dev() {
        let dir = std::env::temp_dir().join(format!("fp-catalog-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("FREEPORT_DATA_DIR", &dir);

        let client = reqwest::Client::new();
        let cat = refresh(&client).await.expect("models.dev 应当可拉取");
        assert!(cat.meta.providers > 10, "厂商数 {}", cat.meta.providers);
        assert!(cat.meta.models > 500, "模型数 {}", cat.meta.models);
        assert!(cat.meta.free_models > 100, "零价模型 {}", cat.meta.free_models);
        // 基线里的厂商必须保留自己的 tier 与免费标注
        let groq = cat.provider("groq").expect("groq 应当在目录里");
        assert_eq!(groq.tier, "A");
        assert!(groq.free_tier);
        assert!(!groq.models.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
