//! Push 模式：客户端不做模型发现，模型表得由我们序列化进它的配置。
//!
//! 现状只实现 VS Code Chat（`chatLanguageModels.json`）那一种形状：顶层是数组，
//! 数组里是若干模型组，我们只碰带自己标记的那一组，其余原样留着。
//!
//! 两条纪律：
//!
//! 1. **认领靠标记，不靠位置**。用户可能把我们那组手动挪到前面或后面，
//!    按下标写回就会覆盖别人的组。
//! 2. **整组替换，不逐字段合并**。模型表是我们全权产出的，半途合并只会留下
//!    上一次注入的残骸——比如已经下架的模型还挂在选择器里。

use serde_json::{json, Map, Value};

use crate::journal;
use crate::spec::PushSpec;
use crate::{Ctx, ModelCard};

/// 我们自己那一组的标记值。
pub const MARK: &str = "freeport";

/// 写入（或替换）本组的模型列表，其余组一字不动。
///
/// 返回新文本与回滚记录。回滚记的是**整份数组快照**：模型组是数组，
/// 逐节点撤不掉「上次有、这次没有了」的那些模型。
pub fn upsert(old: &str, p: &PushSpec, ctx: &Ctx) -> Result<(String, Vec<journal::Prev>), String> {
    // 空表写进去比不写更糟：选择器里会出现一组「Freeport（0 个模型）」，
    // 用户以为注入成功了，实际一个都选不到。
    if ctx.models.is_empty() {
        return Err("模型目录还是空的，先到「模型」页刷新一次再注入".into());
    }
    let mut doc: Value = if old.trim().is_empty() {
        Value::Array(Vec::new())
    } else {
        serde_json::from_str(old)
            .map_err(|_| "目标文件不是合法 JSON，为免写坏已停止（可先手工整理格式）".to_string())?
    };
    if !doc.is_array() {
        return Err("这个客户端的配置应是「模型组数组」，当前结构不符，停止写入".into());
    }
    let before = doc.clone();

    let mut hit: Option<usize> = None;
    for (i, g) in doc.as_array().expect("刚判过是数组").iter().enumerate() {
        if ours(g, p) {
            if hit.is_some() {
                return Err(format!(
                    "配置里有多组 {} = \"{MARK}\"，分不清哪组是我们的，先手工删掉多余的",
                    p.marker
                ));
            }
            hit = Some(i);
        }
    }

    let group = group(p, ctx);
    let arr = doc.as_array_mut().expect("刚判过是数组");
    match hit {
        Some(i) => arr[i] = group,
        None => arr.push(group),
    }

    let mut out = serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?;
    out.push('\n');
    Ok((
        out,
        vec![journal::Prev {
            ptr: String::new(),
            value: before,
        }],
    ))
}

fn ours(g: &Value, p: &PushSpec) -> bool {
    g.get(p.marker).and_then(Value::as_str) == Some(MARK)
}

fn group(p: &PushSpec, ctx: &Ctx) -> Value {
    let mut g = Map::new();
    g.insert("name".into(), json!(p.group));
    g.insert(p.marker.into(), json!(MARK));
    g.insert("vendor".into(), json!(p.vendor));
    // VS Code 靠它决定请求怎么发：`messages` = Anthropic 形状，
    // 而 `url` 我们写在每个模型条目上，所以这里**不写**组级 url——
    // 组级 url 会把 VS Code 推去「自己发现模型」的分支，我们这组就白写了。
    g.insert("apiType".into(), json!(p.api_type));
    g.insert(
        "models".into(),
        Value::Array(ctx.models.iter().map(|m| entry(p, ctx, m)).collect()),
    );
    Value::Object(g)
}

/// 单个模型条目。字段名照 VS Code Chat 的约定，缺数据时给保守值：
/// 能力拿不准就往「不支持」写，工具调用拿不准就往「支持」写——后者错了只是
/// 报错一次，前者错了会让本来能用的模型在选择器里消失。
fn entry(p: &PushSpec, ctx: &Ctx, m: &ModelCard) -> Value {
    let context = m.context.max(1024);
    let max_out = m.max_output.min(context / 2).max(256);
    let mut o = Map::new();
    o.insert("id".into(), json!(m.id));
    o.insert(
        "name".into(),
        json!(if m.label.is_empty() {
            m.id.as_str()
        } else {
            m.label.as_str()
        }),
    );
    o.insert(
        "url".into(),
        json!(format!("{}{}", ctx.base.trim_end_matches('/'), p.path)),
    );
    o.insert("toolCalling".into(), json!(m.tools));
    o.insert("vision".into(), json!(m.vision));
    o.insert("maxInputTokens".into(), json!(context - max_out));
    o.insert("maxOutputTokens".into(), json!(max_out));
    if m.reasoning {
        o.insert(
            "supportsReasoningEffort".into(),
            json!(["low", "medium", "high"]),
        );
        o.insert("defaultReasoningEffort".into(), json!("medium"));
    }
    // 认证按条目走：VS Code 把组级 `apiKey` 当成 secret-store 的引用，
    // 不是原始 token；写原始值会被忽略（表现为「配好了但一直 401」）。
    // 每个模型一条 `requestHeaders`，这也是我们给这个 Agent 单独签发 key 的原因。
    o.insert("requestHeaders".into(), json!({ "x-api-key": ctx.key }));
    Value::Object(o)
}

#[cfg(test)]
mod tests {
    use super::*;

    const VSCODE: PushSpec = PushSpec {
        marker: "freeportVendor",
        group: "Freeport",
        vendor: "customendpoint",
        path: "/v1/messages",
        api_type: "messages",
    };

    fn card(id: &str, context: u32) -> ModelCard {
        ModelCard {
            id: id.into(),
            label: String::new(),
            context,
            max_output: 8192,
            tools: true,
            vision: false,
            reasoning: false,
        }
    }

    fn ctx(ids: &[&str]) -> Ctx<'static> {
        Ctx::new("http://127.0.0.1:8787", "fp-test", "auto")
            .with_models(ids.iter().map(|i| card(i, 200_000)).collect())
    }

    #[test]
    fn writes_one_group_into_empty_file() {
        let (out, prev) = upsert("", &VSCODE, &ctx(&["m1", "m2"])).unwrap();
        let doc: Value = serde_json::from_str(&out).unwrap();
        let arr = doc.as_array().unwrap();
        assert_eq!(arr.len(), 1, "空文件只该出现我们这一组");
        assert_eq!(arr[0]["freeportVendor"], MARK);
        assert_eq!(arr[0]["vendor"], "customendpoint");
        assert_eq!(arr[0]["models"].as_array().unwrap().len(), 2);
        assert_eq!(
            arr[0]["models"][0]["url"],
            "http://127.0.0.1:8787/v1/messages"
        );
        assert_eq!(arr[0]["apiType"], "messages");
        // 组级 url 会把 VS Code 推去自发现分支，必须没有
        assert!(arr[0].get("url").is_none());
        assert_eq!(
            arr[0]["models"][0]["requestHeaders"]["x-api-key"],
            "fp-test"
        );
        assert_eq!(prev.len(), 1);
        assert!(prev[0].ptr.is_empty(), "整份快照用空指针表示");
    }

    #[test]
    fn empty_catalog_is_refused_instead_of_writing_an_empty_group() {
        let c = Ctx::new("http://127.0.0.1:8787", "fp-test", "auto");
        let err = upsert("", &VSCODE, &c).unwrap_err();
        assert!(err.contains("目录"), "{err}");
    }

    #[test]
    fn keeps_foreign_groups_untouched() {
        let old = r#"[{"name":"别人的组","models":[{"id":"x"}]}]"#;
        let (out, _) = upsert(old, &VSCODE, &ctx(&["m1"])).unwrap();
        let doc: Value = serde_json::from_str(&out).unwrap();
        let arr = doc.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["name"], "别人的组", "别人的组必须原样留在原位");
        assert_eq!(arr[1]["freeportVendor"], MARK);
    }

    #[test]
    fn replacing_ours_does_not_duplicate() {
        let (once, _) = upsert("", &VSCODE, &ctx(&["m1", "m2"])).unwrap();
        let (twice, _) = upsert(&once, &VSCODE, &ctx(&["m1", "m2"])).unwrap();
        assert_eq!(once, twice, "重复注入必须字节一致");
    }

    #[test]
    fn shrunk_catalog_drops_stale_models() {
        let (wide, _) = upsert("", &VSCODE, &ctx(&["m1", "m2", "m3"])).unwrap();
        let (narrow, _) = upsert(&wide, &VSCODE, &ctx(&["m1"])).unwrap();
        let doc: Value = serde_json::from_str(&narrow).unwrap();
        let models = doc[0]["models"].as_array().unwrap();
        assert_eq!(models.len(), 1, "整组替换：下架的模型不能留在选择器里");
    }

    #[test]
    fn duplicate_marker_is_refused() {
        let old = r#"[{"freeportVendor":"freeport"},{"freeportVendor":"freeport"}]"#;
        let err = upsert(old, &VSCODE, &ctx(&["m1"])).unwrap_err();
        assert!(err.contains("多组"), "分不清就不写，而不是猜一组覆盖");
    }

    #[test]
    fn reasoning_fields_only_when_supported() {
        let mut c = ctx(&["m1"]);
        c.models[0].reasoning = true;
        let (out, _) = upsert("", &VSCODE, &c).unwrap();
        let doc: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(doc[0]["models"][0]["defaultReasoningEffort"], "medium");

        let (plain, _) = upsert("", &VSCODE, &ctx(&["m1"])).unwrap();
        let doc: Value = serde_json::from_str(&plain).unwrap();
        assert!(doc[0]["models"][0].get("supportsReasoningEffort").is_none());
    }

    #[test]
    fn output_budget_never_exceeds_half_the_window() {
        let mut c = ctx(&["tiny"]);
        c.models[0].context = 4000;
        c.models[0].max_output = 8192;
        let (out, _) = upsert("", &VSCODE, &c).unwrap();
        let doc: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(doc[0]["models"][0]["maxOutputTokens"], 2000);
        assert_eq!(doc[0]["models"][0]["maxInputTokens"], 2000);
    }

    #[test]
    fn non_array_config_is_an_error() {
        let err = upsert(r#"{"models":[]}"#, &VSCODE, &ctx(&["m1"])).unwrap_err();
        assert!(err.contains("数组"));
    }
}
