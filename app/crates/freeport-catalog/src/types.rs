//! 目录数据结构与合并规则。
//!
//! 数据来源两条腿：
//! 1. **内置基线**（`baseline.json`，随版本发布）：厂商元数据、API 基址、免费额度标注。
//!    这些是 models.dev 里没有的，也是「免费」判断的唯一人工来源。
//! 2. **models.dev**（`https://models.dev/api.json`）：模型级事实——上下文长度、
//!    价格、工具/视觉/推理能力。它按天更新，比随版本发布的基线新得多。
//!
//! 合并策略：以基线定「厂商是谁、免不免费」，以 models.dev 定「有哪些模型、什么价位」。
//! 基线未收录但 models.dev 里价格全 0 且给了 api 基址的厂商也会进目录，标为 Tier C 观察位。

use freeport_core::model::{CatalogMeta, ModelCaps, ModelInfo};
use serde::{Deserialize, Serialize};

pub const MODELS_DEV_URL: &str = "https://models.dev/api.json";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Catalog {
    #[serde(default)]
    pub meta: CatalogMeta,
    #[serde(default)]
    pub providers: Vec<CatalogProvider>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CatalogProvider {
    pub id: String,
    #[serde(default)]
    pub name: String,
    /// OpenAI 兼容基址。含 `{account_id}` 这类占位符的需要用户替换。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api: Option<String>,
    /// 申请 Key / 查看文档的地址
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docs: Option<String>,
    /// A 官方免费档 / B 匿名公开端点 / C 订阅或账号额度
    #[serde(default = "default_tier")]
    pub tier: String,
    /// 厂商级「有免费额度」。与模型级零价是两件事。
    #[serde(default, rename = "freeTier")]
    pub free_tier: bool,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "freeNote")]
    pub free_note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quota: Option<Quota>,
    #[serde(default)]
    pub models: Vec<CatalogModel>,
    /// `baseline` / `models.dev` / `both`
    #[serde(default)]
    pub source: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub struct Quota {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rpm: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rpd: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CatalogModel {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "outputLimit")]
    pub output_limit: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "costIn")]
    pub cost_in: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "costOut")]
    pub cost_out: Option<f64>,
    /// 零价（价格为 0）或目录显式标注
    #[serde(default)]
    pub free: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vision: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "releasedAt")]
    pub released_at: Option<String>,
    /// 由签名 feed 新加入、尚未通过 canary 的模型。
    #[serde(default, skip_serializing_if = "is_false")]
    pub candidate: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

fn default_tier() -> String {
    "C".to_string()
}

impl CatalogModel {
    pub fn caps(&self) -> ModelCaps {
        ModelCaps {
            tools: self.tools,
            vision: self.vision,
            json_mode: self.tools, // 支持 tools 的 OpenAI 兼容端点基本都支持 json_object
            reasoning: self.reasoning,
        }
    }
}

/// 免费判定。**只**看两个信号：
/// - `cost_in` 与 `cost_out` 都为 0（models.dev 的价格字段）
/// - 目录里显式标注 `free: true`（人工确认，来自基线）
///
/// 绝不看模型名里有没有 "free"：那是错的，且会随上游改名而失效。
pub fn model_is_free(cost_in: Option<f64>, cost_out: Option<f64>, explicit: Option<bool>) -> bool {
    if explicit == Some(true) {
        return true;
    }
    match (cost_in, cost_out) {
        (Some(a), Some(b)) => a <= 0.0 && b <= 0.0,
        _ => false,
    }
}

impl Catalog {
    /// 目录里这个厂商的某条模型。
    pub fn find_model(&self, provider_id: &str, model_id: &str) -> Option<&CatalogModel> {
        let p = self.providers.iter().find(|p| p.id == provider_id)?;
        let lower = model_id.to_ascii_lowercase();
        p.models
            .iter()
            .find(|m| m.id == model_id)
            .or_else(|| p.models.iter().find(|m| m.id.eq_ignore_ascii_case(&lower)))
            .or_else(|| {
                // OpenRouter 的免费模型带 `:free` 后缀，上游 /models 返回的是不带后缀的 id
                let base = lower.split(':').next().unwrap_or(&lower).to_string();
                p.models.iter().find(|m| {
                    m.id.to_ascii_lowercase().split(':').next().unwrap_or("") == base
                })
            })
    }

    pub fn provider(&self, id: &str) -> Option<&CatalogProvider> {
        self.providers.iter().find(|p| p.id == id)
    }

    /// 目录里某厂商的模型，转成可以直接入库的形状。
    /// 匿名端点不实现 `/models` 时用它兜底。
    pub fn models_of(&self, provider_id: &str) -> Vec<ModelInfo> {
        let Some(p) = self.provider(provider_id) else {
            return Vec::new();
        };
        p.models
            .iter()
            .map(|m| ModelInfo {
                id: m.id.clone(),
                owned_by: provider_id.to_string(),
                context: m.context,
                created: None,
                free: Some(m.free),
                caps: m.caps(),
                candidate: m.candidate,
            })
            .collect()
    }

    /// 用目录数据补齐上游 `/models` 返回的条目。
    /// 只填「上游没给」的字段，不覆盖事实。
    pub fn enrich(&self, provider_id: &str, models: &mut [ModelInfo]) {
        for m in models.iter_mut() {
            if let Some(c) = self.find_model(provider_id, &m.id) {
                if m.context.is_none() {
                    m.context = c.context;
                }
                if m.free.is_none() {
                    m.free = Some(c.free);
                }
                if m.caps.is_empty() {
                    m.caps = c.caps();
                }
                if c.candidate {
                    m.candidate = true;
                }
                if m.owned_by.is_empty() {
                    m.owned_by = provider_id.to_string();
                }
            } else if m.free.is_none() {
                // 目录里没有这条模型：不猜，保持「未知」
                m.free = None;
            }
        }
    }

    /// 汇总计数，写回 `meta`。
    pub fn recount(&mut self) {
        self.meta.providers = self.providers.len();
        self.meta.models = self.providers.iter().map(|p| p.models.len()).sum();
        self.meta.free_models = self
            .providers
            .iter()
            .map(|p| p.models.iter().filter(|m| m.free).count())
            .sum();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_is_price_based_not_name_based() {
        // 名字里有 free，但价格不为 0 → 不是免费
        assert!(!model_is_free(Some(0.5), Some(1.5), None));
        // 价格为 0 → 免费
        assert!(model_is_free(Some(0.0), Some(0.0), None));
        // 价格缺失但目录显式标注 → 免费
        assert!(model_is_free(None, None, Some(true)));
        // 价格缺失且无标注 → 未知，按不免费处理
        assert!(!model_is_free(None, None, None));
    }

    #[test]
    fn enrich_does_not_overwrite_known_facts() {
        let mut cat = Catalog::default();
        cat.providers.push(CatalogProvider {
            id: "p".into(),
            name: "P".into(),
            api: None,
            docs: None,
            tier: "A".into(),
            free_tier: true,
            free_note: None,
            quota: None,
            source: "baseline".into(),
            models: vec![CatalogModel {
                id: "m".into(),
                name: "M".into(),
                context: Some(128_000),
                free: true,
                tools: Some(true),
                ..Default::default()
            }],
        });

        let mut models = vec![ModelInfo {
            id: "m".into(),
            owned_by: "p".into(),
            context: Some(4096), // 上游给了，就不该被目录覆盖
            created: None,
            free: None,
            caps: ModelCaps::default(),
            candidate: false,
        }];

        cat.enrich("p", &mut models);
        assert_eq!(models[0].context, Some(4096));
        assert_eq!(models[0].free, Some(true));
        assert_eq!(models[0].caps.tools, Some(true));
    }
}
