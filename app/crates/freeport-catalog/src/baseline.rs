//! 内置基线：随二进制打包，离线可用。

use serde::Deserialize;

use crate::types::{Catalog, CatalogProvider};

const RAW: &str = include_str!("baseline.json");

#[derive(Deserialize)]
struct Baseline {
    #[serde(default)]
    version: u32,
    #[serde(default, rename = "builtAt")]
    built_at: String,
    #[serde(default)]
    providers: Vec<CatalogProvider>,
}

#[derive(Debug, Clone)]
pub struct BaselineData {
    pub version: u32,
    pub built_at: String,
    pub providers: Vec<CatalogProvider>,
}

pub fn load() -> BaselineData {
    match serde_json::from_str::<Baseline>(RAW) {
        Ok(mut b) => {
            for p in b.providers.iter_mut() {
                p.source = "baseline".to_string();
            }
            BaselineData {
                version: b.version,
                built_at: b.built_at,
                providers: b.providers,
            }
        }
        Err(e) => {
            // 打包进去的 JSON 是编译期常量，正常不会失败；真失败也不能让应用起不来
            tracing::error!("内置目录基线解析失败：{e}");
            BaselineData {
                version: 0,
                built_at: String::new(),
                providers: Vec::new(),
            }
        }
    }
}

/// 只有基线、没有网络时的目录。
pub fn catalog() -> Catalog {
    let b = load();
    let mut c = Catalog {
        meta: freeport_core::model::CatalogMeta {
            source: "baseline".into(),
            built_at: Some(b.built_at),
            error: Some("尚未同步 models.dev，模型细节可能缺失".into()),
            ..Default::default()
        },
        providers: b.providers,
    };
    c.recount();
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baseline_is_valid_and_non_empty() {
        let b = load();
        assert!(b.version >= 1);
        assert!(b.providers.len() >= 10, "基线厂商数 {}", b.providers.len());
        assert!(b.providers.iter().all(|p| !p.id.is_empty()));
        assert!(b.providers.iter().any(|p| p.free_tier));
    }

    /// 断网冷启动的唯一模型来源就是这份基线：每家都得带上自己的免费模型，
    /// 总量太少用户第一屏就没东西可看。
    #[test]
    fn baseline_ships_enough_free_models() {
        let b = load();
        let total: usize = b.providers.iter().map(|p| p.models.len()).sum();
        assert!(total >= 70, "基线模型数 {total}");
        for p in &b.providers {
            assert!(
                !p.models.is_empty(),
                "{} 没有内置免费模型",
                p.id
            );
        }
    }

    /// 免费是硬规则：基线里每条模型都必须显式标 `free: true`。
    /// 漏标会让离线路由把用户打到付费档。
    #[test]
    fn every_baseline_model_is_marked_free() {
        let b = load();
        for p in &b.providers {
            for m in &p.models {
                assert!(m.free, "{}/{} 未标注 free", p.id, m.id);
                assert!(!m.id.is_empty());
            }
        }
    }

    /// (provider, model) 不能重复：重复条目会让 enrich 命中随机的一个。
    #[test]
    fn model_ids_are_unique_per_provider() {
        let b = load();
        for p in &b.providers {
            let mut ids: Vec<&str> = p.models.iter().map(|m| m.id.as_str()).collect();
            let n = ids.len();
            ids.sort_unstable();
            ids.dedup();
            assert_eq!(ids.len(), n, "{} 有重复模型 id", p.id);
        }
    }

    #[test]
    fn anonymous_providers_ship_their_models() {
        let b = load();
        for id in ["pollinations", "llm7"] {
            let p = b.providers.iter().find(|p| p.id == id).expect("存在");
            assert!(!p.models.is_empty(), "{id} 必须有内置模型列表");
            assert_eq!(p.tier, "B");
        }
    }
}
