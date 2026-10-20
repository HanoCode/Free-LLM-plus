//! models.dev 增量：只取事实字段，解析全程容错。
//!
//! 上游 schema 会漂移（字段增删、类型变化）。这里一律用 `serde_json::Value`
//! 逐字段读，读不到就当没有 —— 宁可少一个上下文长度，也不能因为一个字段
//! 让整次同步失败。

use std::collections::{HashMap, HashSet};

use freeport_core::model::CatalogMeta;
use serde_json::Value;

use crate::baseline::BaselineData;
use crate::types::{model_is_free, Catalog, CatalogModel, CatalogProvider, Quota};

/// 从 models.dev 的原始 JSON 解析出厂商列表。不做过早过滤。
pub fn parse(raw: &[u8]) -> Result<Vec<CatalogProvider>, String> {
    let root: Value =
        serde_json::from_slice(raw).map_err(|e| format!("models.dev 返回的不是合法 JSON：{e}"))?;
    let obj = root
        .as_object()
        .ok_or_else(|| "models.dev 返回的顶层不是对象".to_string())?;

    let mut out = Vec::with_capacity(obj.len());
    for (key, pv) in obj {
        let id = str_of(pv, "id").unwrap_or_else(|| key.clone());
        let name = str_of(pv, "name").unwrap_or_else(|| id.clone());
        let api = str_of(pv, "api").filter(|s| !s.trim().is_empty());
        let docs = str_of(pv, "doc").filter(|s| !s.trim().is_empty());

        let mut models = Vec::new();
        if let Some(mobj) = pv.get("models").and_then(Value::as_object) {
            models.reserve(mobj.len());
            for (mid, mv) in mobj {
                if let Some(m) = parse_model(mid, mv) {
                    models.push(m);
                }
            }
        }
        models.sort_by(|a, b| a.id.cmp(&b.id));

        out.push(CatalogProvider {
            id,
            name,
            api,
            docs,
            tier: "C".to_string(),
            free_tier: false,
            free_note: None,
            quota: None,
            models,
            source: "models.dev".to_string(),
        });
    }
    Ok(out)
}

fn parse_model(mid: &str, mv: &Value) -> Option<CatalogModel> {
    if !mv.is_object() {
        return None;
    }
    let id = str_of(mv, "id").unwrap_or_else(|| mid.to_string());
    if id.is_empty() {
        return None;
    }
    let cost = mv.get("cost");
    let cost_in = cost.and_then(|c| num(c.get("input")));
    let cost_out = cost.and_then(|c| num(c.get("output")));

    let limit = mv.get("limit");
    let context = limit.and_then(|l| num(l.get("context"))).map(|n| n as u64);
    let output_limit = limit.and_then(|l| num(l.get("output"))).map(|n| n as u64);

    let modalities = mv.get("modalities");
    let vision = modalities
        .and_then(|m| m.get("input"))
        .and_then(Value::as_array)
        .map(|arr| arr.iter().any(|v| v.as_str() == Some("image")));

    Some(CatalogModel {
        id,
        name: str_of(mv, "name").unwrap_or_default(),
        context,
        output_limit,
        cost_in,
        cost_out,
        free: model_is_free(cost_in, cost_out, None),
        tools: mv.get("tool_call").and_then(Value::as_bool),
        vision,
        reasoning: mv.get("reasoning").and_then(Value::as_bool),
        released_at: str_of(mv, "release_date"),
        candidate: false,
    })
}

fn str_of(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(|s| s.to_string())
}

fn num(v: Option<&Value>) -> Option<f64> {
    match v {
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(s)) => s.parse::<f64>().ok(),
        _ => None,
    }
}

/// 合并：基线定厂商身份，models.dev 定模型事实。
pub fn merge(baseline: BaselineData, remote: Vec<CatalogProvider>, fetched_at: String) -> Catalog {
    let mut remote_by_id: HashMap<String, CatalogProvider> =
        remote.into_iter().map(|p| (p.id.clone(), p)).collect();
    let mut out: Vec<CatalogProvider> = Vec::with_capacity(baseline.providers.len() + 8);
    let mut used: HashSet<String> = HashSet::new();

    for base in baseline.providers {
        let mut p = match remote_by_id.remove(&base.id) {
            Some(mut r) => {
                // 基线的模型条目是「人工补充/覆盖」，优先保留人工判断
                for bm in &base.models {
                    match r.models.iter_mut().find(|m| m.id == bm.id) {
                        Some(rm) => {
                            if rm.context.is_none() {
                                rm.context = bm.context;
                            }
                            if rm.name.is_empty() {
                                rm.name = bm.name.clone();
                            }
                            rm.free = true; // 基线显式标注过，就按免费算
                        }
                        None => r.models.push(bm.clone()),
                    }
                }
                r.name = base.name;
                r.api = base.api.or(r.api);
                r.docs = base.docs.or(r.docs);
                r.tier = base.tier;
                r.free_tier = base.free_tier;
                r.free_note = base.free_note;
                r.quota = base.quota;
                r.source = "both".into();
                r
            }
            None => base,
        };

        // 匿名端点本来就没有计费，所有模型按免费算
        if p.tier == "B" && p.free_tier {
            for m in p.models.iter_mut() {
                m.free = true;
            }
        }
        used.insert(p.id.clone());
        out.push(p);
    }

    // 基线没收录、但 models.dev 里确有零价模型且给了 api 基址的厂商：
    // 进「观察位」，Tier C，UI 上必须标「未实测」。
    let mut extras: Vec<CatalogProvider> = remote_by_id
        .into_values()
        .filter(|p| {
            p.api.is_some() && p.models.iter().any(|m| m.free) && !used.contains(&p.id)
        })
        .collect();
    extras.sort_by(|a, b| {
        let fa = a.models.iter().filter(|m| m.free).count();
        let fb = b.models.iter().filter(|m| m.free).count();
        fb.cmp(&fa).then_with(|| a.id.cmp(&b.id))
    });
    extras.truncate(40);
    for p in extras.iter_mut() {
        p.tier = "C".into();
        p.free_note = Some(format!(
            "models.dev 显示 {} 个零价模型，未进入内置基线，需实测后启用",
            p.models.iter().filter(|m| m.free).count()
        ));
    }
    out.extend(extras);

    let mut cat = Catalog {
        meta: CatalogMeta {
            source: "models.dev".into(),
            fetched_at: Some(fetched_at),
            built_at: Some(baseline.built_at),
            ..Default::default()
        },
        providers: out,
    };
    cat.recount();
    cat
}

/// 目录里某厂商的额度，供探针预算计算使用。
pub fn quota_of(cat: &Catalog, provider_id: &str) -> Quota {
    cat.provider(provider_id)
        .and_then(|p| p.quota)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "groq": {
        "id": "groq", "name": "Groq", "doc": "https://console.groq.com",
        "models": {
          "llama-3.3-70b": {
            "id": "llama-3.3-70b", "name": "Llama 3.3 70B",
            "limit": {"context": 131072, "output": 8192},
            "cost": {"input": 0.59, "output": 0.79},
            "tool_call": true, "reasoning": false,
            "modalities": {"input": ["text"], "output": ["text"]}
          }
        }
      },
      "weird": {
        "id": "weird", "name": "Weird", "api": "https://weird.example/v1",
        "models": {
          "free-1": {"id": "free-1", "cost": {"input": 0, "output": 0}, "tool_call": false},
          "paid-1": {"id": "paid-1", "cost": {"input": 1, "output": 1}}
        }
      },
      "broken": { "id": "broken", "models": "not-an-object" }
    }"#;

    #[test]
    fn parse_tolerates_broken_entries() {
        let list = parse(SAMPLE.as_bytes()).unwrap();
        assert_eq!(list.len(), 3);
        let weird = list.iter().find(|p| p.id == "weird").unwrap();
        assert_eq!(weird.models.len(), 2);
        let free = weird.models.iter().find(|m| m.id == "free-1").unwrap();
        assert!(free.free);
    }

    #[test]
    fn merge_keeps_baseline_identity_and_adds_zero_price_providers() {
        let base = crate::baseline::load();
        let remote = parse(SAMPLE.as_bytes()).unwrap();
        let cat = merge(base, remote, "2026-10-19T00:00:00Z".into());

        let groq = cat.provider("groq").unwrap();
        assert_eq!(groq.tier, "A");
        assert!(groq.free_tier);
        assert_eq!(groq.source, "both");
        // 价格不为 0，模型级不算免费
        assert!(!groq.models[0].free);

        // 基线外的零价厂商进观察位
        assert!(cat.provider("weird").is_some());
        assert_eq!(cat.provider("weird").unwrap().tier, "C");
        // 无 api 的厂商不进目录
        assert!(cat.provider("broken").is_none());
        assert!(cat.meta.free_models >= 1);
    }
}
