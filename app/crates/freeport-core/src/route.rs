//! 候选链构建：Gate → Order → Pick。
//!
//! - **Gate**（`plan`）：模型健康、熔断冷却、额度余量，硬过滤。被拒的带原因，UI 要能解释。
//! - **Order**（[`Swrr::order`]）：平滑加权轮询，按「档位 × 余量 × 健康」的权重轮流。
//! - **Pick**（[`stick_to_front`]）：会话粘性，把上次答它的那家提到最前，保住 prompt cache。
//!
//! 顺序是刻意的：先保证「能发」，再决定「谁先发」，最后让「上次答过的那家」插队。
//! 粘性不能突破 Gate —— 冷却中的、额度见底的，粘也粘不住。

use crate::cooldown::{now_ms, BreakerState, Cooldowns};
use crate::model::{
    Account, AppData, HealthStatus, ModelCaps, ModelInfo, Provider, Purpose, RouteWeights,
};
use crate::quota::Ledger;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// 指向 `AppData.providers` 的下标
    pub provider: usize,
    /// 用哪条凭据发。额度、冷却、账本、粘性都按它算。
    pub account: String,
    pub model_id: String,
    /// 由半开试探放行；一轮里最多一个
    pub half_open: bool,
    /// 请求要求的能力里，这个模型有的没测过。
    /// 不是拒绝理由（不知道 ≠ 不支持），只是排序时要让位给确认支持的。
    pub cap_unknown: bool,
}

/// 一次请求对模型能力的硬要求。
///
/// 只收「模型不支持就一定跑不通」的三项。`reasoning` 不在里面 ——
/// 不支持的模型照样能答，只是答得不深，不该挡掉。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Needs {
    pub vision: bool,
    pub tools: bool,
    pub json: bool,
}

impl Needs {
    pub fn any(&self) -> bool {
        self.vision || self.tools || self.json
    }

    /// 从已经归一成 OpenAI 形状的请求体里推断。
    ///
    /// 所有协议入口（`/v1/messages`、`/v1beta`、`/api/chat`、`/v1/responses`）
    /// 都会先转成 chat 形态再进来，所以只需要看一处。
    pub fn of_chat(payload: &serde_json::Value) -> Self {
        let non_empty = |key: &str| {
            payload
                .get(key)
                .and_then(|v| v.as_array())
                .map(|a| !a.is_empty())
                .unwrap_or(false)
        };

        let tool_choice_active = payload
            .get("tool_choice")
            .map(|v| !matches!(v.as_str(), Some("none")))
            .unwrap_or(false);

        let json = payload
            .get("response_format")
            .and_then(|v| v.get("type"))
            .and_then(|v| v.as_str())
            .map(|t| t != "text")
            .unwrap_or(false);

        Needs {
            vision: has_image(payload.get("messages")),
            tools: non_empty("tools") || non_empty("functions") || tool_choice_active,
            json,
        }
    }
}

/// 消息里有没有图片块。Anthropic 的 `image`、Responses 的 `input_image`、
/// Ollama 的 `images` 在各自的入口都已归一成 `image_url`，这里再兜一层旧写法。
fn has_image(messages: Option<&serde_json::Value>) -> bool {
    let Some(list) = messages.and_then(|m| m.as_array()) else {
        return false;
    };
    list.iter().any(|m| match m.get("content") {
        Some(serde_json::Value::Array(parts)) => parts.iter().any(|p| {
            matches!(
                p.get("type").and_then(|t| t.as_str()),
                Some("image_url") | Some("image") | Some("input_image")
            )
        }),
        _ => false,
    })
}

/// 模型的有效能力：实测覆盖声明。
pub fn caps_of(data: &AppData, provider: &Provider, model_id: &str) -> ModelCaps {
    let declared = provider
        .models
        .iter()
        .find(|m| m.id == model_id)
        .map(|m| m.caps)
        .unwrap_or_default();
    let measured = data
        .model_health
        .get(&format!("{}:{}", provider.id, model_id))
        .map(|h| h.caps)
        .unwrap_or_default();
    declared.merge_measured(measured)
}

/// 能力未知的候选在排序里的折价。
///
/// 只是「让位」，不是「排除」：大部分免费厂商的目录根本没写能力字段，
/// 一律排除会连正常文本请求都发不出去。
pub const CAP_UNKNOWN_FACTOR: f64 = 0.6;

/// 目录是否**明确**把这个模型标成付费。
///
/// `None` 不算付费：大部分免费源（llm7 这类）的目录根本没给价格字段，
/// 把未知当付费会把整家可用的免费厂商踢出链。判免费只看价格 0 与目录
/// 显式标注，两条都不满足就是「不知道」，不是「收费」。
pub fn is_paid_model(p: &Provider, model_id: &str) -> bool {
    p.models
        .iter()
        .find(|m| m.id == model_id)
        .and_then(|m| m.free)
        == Some(false)
}

/// 这单是不是「让网关自己挑」（`auto` / `freeport/auto` / 空 / `auto:<profile>`）。
pub fn is_auto(model: &str) -> bool {
    model.is_empty() || model == "auto" || model == "freeport/auto" || model.starts_with("auto:")
}

/// 这次请求该用哪条链。
///
/// 优先级：`auto:<profile>` 点名 → 用途映射 → 全局链。
/// 点名的 profile 不存在时返回**空链**，让它继续走 ③ 的兜底扫描——
/// 直接回 503 等于「改错一个字就没有模型可用」，而兜底至少还能跑。
fn chain_of<'a>(data: &'a AppData, model: &str, purpose: Purpose) -> &'a [String] {
    if let Some(name) = model.strip_prefix("auto:").map(str::trim) {
        if !name.is_empty() {
            return data
                .routing
                .chains
                .get(name)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
        }
    }
    if let Some(name) = data.routing.purpose_chain.get(purpose.as_str()) {
        if let Some(c) = data.routing.chains.get(name) {
            return c.as_slice();
        }
    }
    data.routing.chain.as_slice()
}

#[derive(Debug, Clone)]
pub struct Rejected {
    pub provider: String,
    /// 厂商级原因（没开 / 没 key）为空，模型级原因带上具体凭据
    pub account: Option<String>,
    pub model: String,
    pub reason: String,
    /// 归类用，让界面能按「额度 / 判死 / 冷却」分区，不必去解析 reason 文本。
    /// 取值：`quota` / `health` / `cooldown` / `disabled` / `nokey` /
    /// `tombstone` / `capability` / `purpose`
    pub kind: &'static str,
}

#[derive(Debug, Clone, Default)]
pub struct Plan {
    pub candidates: Vec<Candidate>,
    pub rejected: Vec<Rejected>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.candidates.is_empty()
    }
}

/// 网关对外挂的别名前缀。
///
/// Claude Code 的模型发现**只收 `claude` / `anthropic` 前缀的 id**，上游真实名字
/// （`llama-3.3-70b`）会被它静默丢掉。所以对外露出的 id 统一加这层壳，进来再脱掉。
pub const ALIAS_PREFIX: &str = "claude-freeport-";

/// 别名 → 内部模型名。脱壳后仍走 `auto` / `provider:model` / 裸 id 三种写法。
pub fn resolve_alias(model: &str) -> &str {
    model.strip_prefix(ALIAS_PREFIX).unwrap_or(model)
}

/// 一条对外别名。
///
/// `provider` 为空有两种情形，靠 `members` 区分：
/// - `members` 也为空 → 虚拟项（`auto` / `auto:<chain>`），没有具体厂商
/// - `members` 非空 → **模型组**，同名模型在多家都有
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasEntry {
    pub alias: String,
    pub provider: String,
    pub model: String,
    /// 组的成员厂商；单厂商别名是空数组
    pub members: Vec<String>,
}

impl AliasEntry {
    pub fn is_virtual(&self) -> bool {
        self.provider.is_empty() && self.members.is_empty()
    }

    /// 组：一个名字，多家通道。
    pub fn is_group(&self) -> bool {
        self.provider.is_empty() && !self.members.is_empty()
    }
}

/// `provider:model` 的别名形式。
///
/// 别名里保留 `:`，因为脱壳后正好是 `plan` 能精确解析的显式点名语法；
/// 换成别的分隔符就得再维护一层反查表。
pub fn alias_of(provider: &str, model: &str) -> String {
    format!("{ALIAS_PREFIX}{provider}:{model}")
}

/// 对外可枚举的别名表：`auto` 打头，之后每个模型一条。
///
/// 三条过滤规则，都是为了「列出来的必须能发出去」：
///
/// 1. 厂商要启用且至少有一条可用凭据 —— 没 key 的列出来，用户选了必然 `nokey`；
/// 2. 被 feed 下架的模型不列 —— Gate 会拒；
/// 3. **同名模型收敛成一条**（见下）。
///
/// 收敛分两种，取决于有几家能跑这个模型：
///
/// - 只有一家 → `claude-freeport-provider:model`：那时「哪一家」是用户需要知道的信息，
///   脱壳后是精确点名，`plan` 直接认。
/// - 两家以上 → `claude-freeport-model`（不带厂商）：它是一个**组**，
///   路由到所有成员。留成 `provider:model` 等于把五条通道说成一条——
///   那家的 key 一冷却，这个名字就无路可走，而界面上明明写着 5 家都能跑。
pub fn aliases(data: &AppData, has_key: &dyn Fn(&Account) -> bool) -> Vec<AliasEntry> {
    let mut out = vec![AliasEntry {
        alias: format!("{ALIAS_PREFIX}auto"),
        provider: String::new(),
        model: "auto".into(),
        members: Vec::new(),
    }];
    // 命名链也露成别名：客户端的选择器里能直接看到 `coding` / `cheap` 这类档位，
    // 而不用记 `auto:coding` 这种内部语法。
    for name in data.routing.chains.keys() {
        out.push(AliasEntry {
            alias: format!("{ALIAS_PREFIX}auto:{name}"),
            provider: String::new(),
            model: format!("auto:{name}"),
            members: Vec::new(),
        });
    }

    // 先数清楚「谁家有这个模型」：组的成员要一次算全，不能边遍历边发现
    let mut offers: std::collections::HashMap<&str, Vec<String>> = std::collections::HashMap::new();
    for p in data.providers.iter().filter(|p| p.enabled) {
        if !p.live_accounts().any(has_key) {
            continue;
        }
        for m in &p.models {
            if data.feed.tombstoned(&p.id, &m.id) {
                continue;
            }
            let e = offers.entry(m.id.as_str()).or_default();
            if !e.iter().any(|x| x == &p.id) {
                e.push(p.id.clone());
            }
        }
    }

    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for p in data.providers.iter().filter(|p| p.enabled) {
        if !p.live_accounts().any(has_key) {
            continue;
        }
        for m in &p.models {
            if data.feed.tombstoned(&p.id, &m.id) || !seen.insert(m.id.as_str()) {
                continue;
            }
            let members = offers.get(m.id.as_str()).cloned().unwrap_or_default();
            if members.len() > 1 {
                out.push(AliasEntry {
                    alias: format!("{ALIAS_PREFIX}{}", m.id),
                    provider: String::new(),
                    model: m.id.clone(),
                    members,
                });
            } else {
                out.push(AliasEntry {
                    alias: alias_of(&p.id, &m.id),
                    provider: p.id.clone(),
                    model: m.id.clone(),
                    members: Vec::new(),
                });
            }
        }
    }
    out
}

/// 一个裸模型名覆盖哪些厂商：声明了这个 id、且自己有凭据的那几家，按 provider 顺序。
///
/// 这就是「组」的实现：用户写 `llama-3.3-70b` 时，五家都在链上，
/// 而不是只取第一个声明它的那家（那等于把「5 家可用」偷偷缩成 1 条通道）。
pub fn group_of(data: &AppData, model: &str, usable: &[usize]) -> Vec<usize> {
    usable
        .iter()
        .copied()
        .filter(|&i| data.providers[i].models.iter().any(|m| m.id == model))
        .collect()
}

/// 组装本轮候选链。
///
/// `model` 支持三种写法：
///
/// - `auto` / `auto:<chain>`：链优先 + 全场兜底，只花免费额度；
/// - `provider:model`：只有这一家。别家同名模型不算，别的模型也不算；
/// - 裸 `model-id`：**模型组**——声明了这个 id 的每一家都入链，
///   顺序按 provider 顺序，最终由 SWRR 决定先打哪条凭据。
///
/// 组是「同名即同模型」这个前提的落地：同一个 `llama-3.3-70b` 在五家都有时，
/// 五家的凭据都在链上；那家限流了就换下一家，而不是从「有 5 条通道」
/// 退化成「只有 1 条」。
///
/// 候选的粒度是**凭据**而不是厂商：同一厂商的两个 key 是两条独立的通道，
/// 各自有额度、各自会 429，混成一条会让 Gate 用 A 的余量去放行 B 的请求。
pub fn plan(
    data: &AppData,
    model: &str,
    cooldowns: &Cooldowns,
    has_key: &dyn Fn(&Account) -> bool,
    ledger: &Ledger,
) -> Plan {
    plan_with(
        data,
        model,
        cooldowns,
        has_key,
        ledger,
        Needs::default(),
        Purpose::Chat,
    )
}

/// 带能力要求与用途的候选链。
///
/// 能力门禁放在 Gate 的第一位（tombstone 之后）：它和额度、冷却不同，
/// 是「这个请求和这个模型天生不匹配」，换时间、换 key 都不会变。
/// 放在后面会让一条带图请求先被冷却原因挡住，用户看不到真正的原因。
///
/// `purpose` 是账号池维度，见 [`Purpose`]：它只按凭据准入过滤，
/// 不改权重、不改顺序——分池是「谁能接」，不是「谁先接」。
pub fn plan_with(
    data: &AppData,
    model: &str,
    cooldowns: &Cooldowns,
    has_key: &dyn Fn(&Account) -> bool,
    ledger: &Ledger,
    needs: Needs,
    purpose: Purpose,
) -> Plan {
    let mut out: Vec<Candidate> = Vec::new();
    let mut rejected: Vec<Rejected> = Vec::new();
    // 让网关自己挑时，只允许花免费额度。显式点名不受这条约束 ——
    // 用户写了 `openrouter:xxx` 就是明确要花钱用它。
    let auto = is_auto(model);

    // 厂商级原因：接进来了却没进链，最常见的不是路由的错，而是没开或没 key。
    // 用 `model = "*"` 占位，让界面能直接回答「我明明接了它，为什么不在链里」。
    let keyed: Vec<bool> = data
        .providers
        .iter()
        .map(|p| p.live_accounts().any(has_key))
        .collect();
    for (i, p) in data.providers.iter().enumerate() {
        if !p.enabled {
            rejected.push(Rejected {
                provider: p.id.clone(),
                account: None,
                model: "*".into(),
                reason: "provider disabled".into(),
                kind: "disabled",
            });
        } else if !keyed[i] {
            rejected.push(Rejected {
                provider: p.id.clone(),
                account: None,
                model: "*".into(),
                reason: "no api key".into(),
                kind: "nokey",
            });
        }
    }

    let usable: Vec<usize> = data
        .providers
        .iter()
        .enumerate()
        .filter(|(i, p)| p.enabled && keyed[*i])
        .map(|(i, _)| i)
        .collect();

    // 每个厂商「有 key 且自己开着」的凭据，按下标索引好取
    let live: Vec<Vec<String>> = data
        .providers
        .iter()
        .map(|p| {
            p.live_accounts()
                .filter(|a| has_key(a))
                .map(|a| a.id.clone())
                .collect()
        })
        .collect();

    // 返回这次是否真的入链（重复项与空 model 都算没有）。
    let mut push = |idx: usize, account: &str, model_id: &str| -> bool {
        if model_id.is_empty() {
            return false;
        }
        if out
            .iter()
            .any(|c| c.provider == idx && c.account == account && c.model_id == model_id)
        {
            return false;
        }
        out.push(Candidate {
            provider: idx,
            account: account.to_string(),
            model_id: model_id.to_string(),
            half_open: false,
            cap_unknown: false,
        });
        true
    };

    // ① 显式点名
    // `auto:*` 也是 auto：不加这个 guard，`auto:coding` 会被当成厂商 `auto`，
    // 然后一条候选都拼不出来且不报错。
    if !auto {
        if let Some((pid, mid)) = model.split_once(':') {
            if let Some(idx) = usable.iter().copied().find(|&i| data.providers[i].id == pid) {
                for acc in &live[idx] {
                    push(idx, acc, mid);
                }
            }
        } else if !model.is_empty() {
            // 裸模型名 = 模型组：声明了它的每一家都入链，不是只取第一家。
            // 只取第一家的话，那家的 key 一冷却，「5 家都能跑」就变成 0 条通道。
            for idx in group_of(data, model, &usable) {
                for acc in &live[idx] {
                    push(idx, acc, model);
                }
            }
        }
    }

    // ② 用户配置的链 + ③ 兜底，**只对 auto 生效**。
    //
    // 显式点名是「我就要这个模型」，不是「优先这个模型」：把别的模型补在后面
    // 兜底，会让一次点名请求安静地落到另一个模型上（客户端只会看到
    // 响应里的 model 变了，代码照跑）。点名无路可走时正确的回应是
    // 明确说「这个模型没有可用通道」，不是换一个模型假装成功。
    if auto {
        // 链是**优先**不是独占：链上的条目先入列，③ 再把其余可用模型补在后面兜底。
        // 这条与既有测试 `chain_comes_before_fallback` 的契约一致——用户配链是为了
        // 决定「谁先用」，不是为了在链上全挂时拒绝服务。
        for entry in chain_of(data, model, purpose) {
            if let Some((pid, mid)) = entry.split_once(':') {
                if let Some(idx) = usable.iter().copied().find(|&i| data.providers[i].id == pid) {
                    for acc in &live[idx] {
                        push(idx, acc, mid);
                    }
                }
            }
        }

        // ③ 兜底：tier A 优先，每个 provider 取前两个模型。
        // auto 时先跳过明确付费的再取：一家「前两个是付费、免费的在后面」的厂商
        // （openrouter 这类）否则会一条候选都进不来，白丢一整个免费源。
        // 被跳过的付费模型只解释前两个位置上的那几条——整份目录记一遍，
        // 「被拦下的候选」会变成几百条噪音。
        let mut by_tier = usable.clone();
        by_tier.sort_by(|&a, &b| data.providers[a].tier.cmp(&data.providers[b].tier));
        for idx in by_tier {
            let p = &data.providers[idx];
            let mut picked: Vec<&ModelInfo> = Vec::new();
            for (i, m) in p.models.iter().enumerate() {
                if is_paid_model(p, &m.id) {
                    if i < 2 {
                        for acc in &live[idx] {
                            rejected.push(Rejected {
                                provider: p.id.clone(),
                                account: Some(acc.clone()),
                                model: m.id.clone(),
                                reason: "paid model, auto only spends free quota".into(),
                                kind: "paid",
                            });
                        }
                    }
                    continue;
                }
                if picked.len() < 2 {
                    picked.push(m);
                }
            }
            for m in picked {
                for acc in &live[idx] {
                    push(idx, acc, &m.id);
                }
            }
        }
    }

    // ── Gate：模型健康 + 熔断 + 额度过滤 ──
    let now = now_ms();
    let mut closed = Vec::new();
    let mut half = Vec::new();
    // 被冷却挡掉的候选连 `until` 一起留着：全挡光时要靠它决定「谁最可能已经好了」
    let mut cooling: Vec<(Candidate, i64)> = Vec::new();
    for mut c in out {
        let p = &data.providers[c.provider];

        // feed 已下架的模型：立刻不再选中，不等目录重新同步一次。
        // 「删掉它，路由立刻不再选」这条验收靠的就是这里，而不是靠合并时机。
        if data.feed.tombstoned(&p.id, &c.model_id) {
            rejected.push(Rejected {
                provider: p.id.clone(),
                account: Some(c.account.clone()),
                model: c.model_id.clone(),
                reason: "model withdrawn by feed".into(),
                kind: "tombstone",
            });
            continue;
        }

        // 用途分池：标签是**准入限制**，不是分配。空的 `purposes` = 不限，
        // 所以老配置一条候选都不会被这条挡住（兼容性是刻意保证的）。
        //
        // 放在能力门禁之前：这是配置层面的硬事实，跟这个模型会不会看图无关。
        // 先说「这条凭据不接这类活」，比说「模型不支持 vision」更接近真相。
        if let Some(a) = p.account(&c.account) {
            if !a.admits(purpose) {
                let pools = a
                    .purposes
                    .iter()
                    .map(|x| x.as_str())
                    .collect::<Vec<_>>()
                    .join("/");
                rejected.push(Rejected {
                    provider: p.id.clone(),
                    account: Some(c.account.clone()),
                    model: c.model_id.clone(),
                    reason: format!("credential not pooled for {} (only {pools})", purpose.as_str()),
                    kind: "purpose",
                });
                continue;
            }
        }

        // 免费优先：让网关自己挑时不花用户的钱。
        //
        // 这条只管「目录明确标了付费」的候选（`free == false`）。`None` 放行：
        // 没给价格 ≠ 收费，把未知一起挡掉会把 llm7 这类免费源整家踢出链。
        // 放在能力门禁之前——「这个模型要花钱」比「它不支持 vision」更根本。
        // 显式点名（含厂商前缀或裸模型名）不走这条：用户要的就是那个模型。
        if auto && is_paid_model(p, &c.model_id) {
            rejected.push(Rejected {
                provider: p.id.clone(),
                account: Some(c.account.clone()),
                model: c.model_id.clone(),
                reason: "paid model, auto only spends free quota".into(),
                kind: "paid",
            });
            continue;
        }

        // 能力门禁：声明「不支持」的直接排除；没测过的放行但降权。
        // 声明为 false 是硬事实，未知只是信息缺口，两者不能混。
        if needs.any() {
            let caps = caps_of(data, p, &c.model_id);
            let mut cap_unknown = false;
            let mut missing: Option<&'static str> = None;
            for (want, flag, name) in [
                (needs.vision, caps.vision, "vision"),
                (needs.tools, caps.tools, "tools"),
                (needs.json, caps.json_mode, "json"),
            ] {
                if !want {
                    continue;
                }
                match flag {
                    Some(false) => {
                        missing = Some(name);
                        break;
                    }
                    None => cap_unknown = true,
                    Some(true) => {}
                }
            }
            if let Some(cap) = missing {
                rejected.push(Rejected {
                    provider: p.id.clone(),
                    account: Some(c.account.clone()),
                    model: c.model_id.clone(),
                    reason: format!("model does not support {cap}"),
                    kind: "capability",
                });
                continue;
            }
            c.cap_unknown = cap_unknown;
        }

        // 额度耗尽：不是故障，只是这条凭据这一轮不该再打
        if !ledger.has_room(&c.account, now) {
            rejected.push(Rejected {
                provider: p.id.clone(),
                account: Some(c.account.clone()),
                model: c.model_id.clone(),
                reason: "quota exhausted".into(),
                kind: "quota",
            });
            continue;
        }

        // 模型级限额（从上游 429 学的，见 `Ledger::learn_model`）：只拦这一个
        // 模型。Groq / OpenAI 系的模型级 TPM/RPD 是跟着 key 走的——账号还有
        // 余量不等于这个模型还有，分开记才不会把请求推去撞一堵已知的墙。
        if !ledger.model_has_room(&c.account, &c.model_id, now) {
            rejected.push(Rejected {
                provider: p.id.clone(),
                account: Some(c.account.clone()),
                model: c.model_id.clone(),
                reason: "model quota exhausted (learned from upstream)".into(),
                kind: "quota",
            });
            continue;
        }

        // 探针判定不可用的模型不参与路由。恢复由巡检负责（unavailable 30 分钟后复检）。
        // 健康度按 `provider:model` 记 —— 模型坏没坏跟用哪条 key 无关。
        if let Some(h) = data.model_health.get(&format!("{}:{}", p.id, c.model_id)) {
            if !h.status.is_usable() {
                rejected.push(Rejected {
                    provider: p.id.clone(),
                    account: Some(c.account.clone()),
                    model: c.model_id.clone(),
                    reason: format!("model {}", h.status.as_str()),
                    kind: "health",
                });
                continue;
            }
        }

        match cooldowns.state_for(&p.id, &c.account, Some(&c.model_id)) {
            BreakerState::Closed => closed.push(c),
            BreakerState::HalfOpen => half.push(Candidate { half_open: true, ..c }),
            BreakerState::Open { until, reason } => {
                rejected.push(Rejected {
                    provider: p.id.clone(),
                    account: Some(c.account.clone()),
                    model: c.model_id.clone(),
                    reason: format!("{reason} until {until}"),
                    kind: "cooldown",
                });
                cooling.push((c, until));
            }
        }
    }

    // 半开只放一个试探，其余的等下一轮
    if let Some(probe) = half.into_iter().next() {
        closed.push(probe);
    }

    // ── 全冷却回退（清单 12）──
    //
    // 一条能打的路都没有时，只有两种诚实的回应：等，或者试。回 503「全在冷却」
    // 把「还剩十几秒的临时限流」和「日额度耗尽到明天」说成了同一件事，而前者
    // 只要等一会儿就好。所以：窗口内的（默认 30s）放**一个**出去当试探，
    // 窗口外的维持拒绝——几小时后的 reset 撞一次只是白烧一次失败计数。
    if closed.is_empty() && data.routing.fallback_when_all_cooling {
        let horizon = now.saturating_add(data.routing.fallback_horizon_ms.max(0));
        if let Some((c, _)) = cooling
            .into_iter()
            .filter(|(_, until)| *until <= horizon)
            .min_by_key(|(_, until)| *until)
        {
            // 标 half_open：成功即解锁，失败按一次正常熔断计，不算新账
            closed.push(Candidate {
                half_open: true,
                ..c
            });
        }
    }

    Plan {
        candidates: closed,
        rejected,
    }
}

// ── Order：平滑加权轮询 ─────────────────────────────────────────────────
//
// 朴素的「按权重随机」有两个毛病：小样本下分配不均（概率上对、体感上错），
// 而且同一权重下会连着几次选中同一家。SWRR 的做法是「累计权重取最大、选中后减总量」，
// 保证一个周期内严格按比例轮流，且任意两次命中之间的跨度不超过一家。

/// 排序策略。
///
/// 八种模式只改「谁先发」，不改准入 —— Gate 之后才有意义。
/// 默认 `balanced`：没有显式选择的老配置行为完全不变。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strategy {
    /// 平滑加权轮询（档位 × 余量 × 健康）。默认。
    Balanced,
    /// 严格按链的顺序，第一个不行才下一个。
    Order,
    /// 每次请求轮换起点，不看权重。
    Rotate,
    /// 今日用得最少的先上。
    Usage,
    /// 剩余额度按「到重置还有多久」摊平，越经得起持续的越先上。
    Pace,
    /// 探针实测延迟最短的先上。没测过的让位给测过的——猜的快不如测的快。
    Fastest,
    /// 健康状态最好的先上：Healthy > 未知 > Degraded（慢），
    /// 同档按连续失败次数细分，失败一样时成功次数多的靠前。
    Stablest,
    /// 按「智能」近似排序：实测支持推理的优先，其次工具调用、视觉，
    /// 最后按目录里的上下文长度。没有权威跑分，这三样是目录里最硬的信号。
    Smartest,
}

impl Strategy {
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "order" => Self::Order,
            "rotate" => Self::Rotate,
            "usage" => Self::Usage,
            "pace" => Self::Pace,
            "fastest" => Self::Fastest,
            "stablest" | "stable" => Self::Stablest,
            "smartest" => Self::Smartest,
            _ => Self::Balanced,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Balanced => "balanced",
            Self::Order => "order",
            Self::Rotate => "rotate",
            Self::Usage => "usage",
            Self::Pace => "pace",
            Self::Fastest => "fastest",
            Self::Stablest => "stablest",
            Self::Smartest => "smartest",
        }
    }
}

/// 平滑加权轮询状态。跨请求保留，才谈得上「平滑」。
#[derive(Debug, Default, Clone)]
pub struct Swrr {
    current: std::collections::BTreeMap<String, f64>,
    /// `rotate` 策略的轮转计数。与 `current` 分开：两者互不干扰。
    tick: u64,
}

impl Swrr {
    pub fn new() -> Self {
        Self::default()
    }

    /// 按给定策略重排候选。`balanced` 之外的模式都不写 `current`，
    /// 因此「切回 balanced」不会带着一份被别处搅乱的状态。
    pub fn order_with(
        &mut self,
        strategy: Strategy,
        cands: Vec<Candidate>,
        data: &AppData,
        ledger: &Ledger,
        now: i64,
    ) -> Vec<Candidate> {
        match strategy {
            Strategy::Balanced => self.order(cands, data, ledger, now),
            _ => self.order_with_weights(strategy, cands, data, ledger, now, None),
        }
    }

    /// 带权重覆盖的版本（策略档用）：`balanced` 的第三因子改用给定权重，
    /// 其余策略与 [`order_with`] 完全一致——极值策略本来就只看一个维度。
    pub fn order_with_weights(
        &mut self,
        strategy: Strategy,
        cands: Vec<Candidate>,
        data: &AppData,
        ledger: &Ledger,
        now: i64,
        weights_override: Option<crate::model::RouteWeights>,
    ) -> Vec<Candidate> {
        match strategy {
            Strategy::Balanced => self.order_weighted(cands, data, ledger, now, weights_override),
            Strategy::Order => cands,
            Strategy::Rotate => {
                if cands.len() < 2 {
                    return cands;
                }
                let n = cands.len();
                let start = (self.tick % n as u64) as usize;
                self.tick = self.tick.wrapping_add(1);
                let mut v = cands;
                v.rotate_left(start);
                v
            }
            Strategy::Usage => {
                let used = |c: &Candidate| -> f64 {
                    ledger
                        .window(&c.account, crate::quota::WindowKind::Rpd)
                        .map(|w| w.used_total())
                        .unwrap_or(0.0)
                };
                let mut v = cands;
                v.sort_by(|a, b| {
                    used(a)
                        .partial_cmp(&used(b))
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
                v
            }
            Strategy::Pace => {
                let pace = |c: &Candidate| -> f64 {
                    match ledger.tightest(&c.account, now) {
                        None => 1.0,
                        Some(t) => {
                            let hours = ((t.reset_at - now) as f64 / 3_600_000.0).max(0.1);
                            t.ratio / hours
                        }
                    }
                };
                let mut v = cands;
                // 越大越先：剩得多、且离重置还久的排前面
                v.sort_by(|a, b| {
                    pace(b)
                        .partial_cmp(&pace(a))
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
                v
            }
            // 三个极值策略都是纯排序（稳定排序保住链内原有次序做平手裁决），
            // 不写 SWRR 状态，与 balanced 的 `current` 互不污染。
            Strategy::Fastest => {
                let mut v = cands;
                v.sort_by_key(|c| match health_of(data, c).and_then(|h| h.latency_ms) {
                    Some(ms) => (0, ms),
                    // 没测过的排在测过的后面，彼此之间维持链上原序
                    None => (1, 0),
                });
                v
            }
            Strategy::Stablest => {
                let rank = |c: &Candidate| -> u8 {
                    match health_of(data, c).map(|h| h.status) {
                        Some(HealthStatus::Healthy) => 0,
                        None | Some(HealthStatus::Unknown) | Some(HealthStatus::Probing) => 1,
                        Some(HealthStatus::Degraded) => 2,
                        Some(HealthStatus::Cooling) => 3,
                        _ => 4,
                    }
                };
                let mut v = cands;
                v.sort_by(|a, b| {
                    rank(a)
                        .cmp(&rank(b))
                        .then_with(|| {
                            let fa = health_of(data, a).map(|h| h.fails).unwrap_or(0);
                            let fb = health_of(data, b).map(|h| h.fails).unwrap_or(0);
                            fa.cmp(&fb)
                        })
                        .then_with(|| {
                            let oa = health_of(data, a).map(|h| h.oks).unwrap_or(0);
                            let ob = health_of(data, b).map(|h| h.oks).unwrap_or(0);
                            ob.cmp(&oa)
                        })
                });
                v
            }
            Strategy::Smartest => {
                let mut v = cands;
                v.sort_by_key(|c| std::cmp::Reverse(smart_key(data, c)));
                v
            }
        }
    }

    /// 按权重重排候选。权重见 [`weight_of`]。
    ///
    /// 权重为 0 的候选不会被丢掉——它们是失败接力的后备，只是排在最后。
    pub fn order(
        &mut self,
        cands: Vec<Candidate>,
        data: &AppData,
        ledger: &Ledger,
        now: i64,
    ) -> Vec<Candidate> {
        self.order_weighted(cands, data, ledger, now, None)
    }

    /// `order` 的权重可覆盖版本。`weights_override` 优先于全局配置
    /// （策略档命中时用）；`None` 时行为与 [`order`] 完全一致。
    pub fn order_weighted(
        &mut self,
        cands: Vec<Candidate>,
        data: &AppData,
        ledger: &Ledger,
        now: i64,
        weights_override: Option<crate::model::RouteWeights>,
    ) -> Vec<Candidate> {
        if cands.len() < 2 {
            return cands;
        }

        let weights = weights_override.or_else(|| data.routing.weights.active());
        let mut live: Vec<(Candidate, f64, String)> = Vec::with_capacity(cands.len());
        for c in cands {
            let p = &data.providers[c.provider];
            let health = data.model_health.get(&format!("{}:{}", p.id, c.model_id));
            let w = weight_of_model(p, &c.account, &c.model_id, ledger, health, now, weights)
                * canary_factor(data, &p.id, &c.model_id, health)
                * cap_factor(&c);
            // 轮转的粒度是凭据：两家厂商各两个 key，应该是四条通道轮流
            let key = format!("{}:{}", c.account, c.model_id);
            live.push((c, w, key));
        }

        let (mut hot, cold): (Vec<_>, Vec<_>) = live.into_iter().partition(|(_, w, _)| *w > 0.0);
        if hot.len() < 2 {
            let mut out: Vec<Candidate> = hot.into_iter().map(|(c, _, _)| c).collect();
            out.extend(cold.into_iter().map(|(c, _, _)| c));
            return out;
        }

        let total: f64 = hot.iter().map(|(_, w, _)| *w).sum();
        let mut out = Vec::with_capacity(hot.len() + cold.len());

        while !hot.is_empty() {
            for (_, w, key) in hot.iter() {
                *self.current.entry(key.clone()).or_insert(0.0) += *w;
            }
            // 平手时按 key 定序，避免 HashMap 迭代顺序带来的不确定
            let mut best = 0usize;
            for i in 1..hot.len() {
                let ci = *self.current.get(&hot[i].2).unwrap_or(&0.0);
                let cb = *self.current.get(&hot[best].2).unwrap_or(&0.0);
                if ci > cb || (ci == cb && hot[i].2 < hot[best].2) {
                    best = i;
                }
            }
            if let Some(cur) = self.current.get_mut(&hot[best].2) {
                *cur -= total;
            }
            let (cand, _, _) = hot.remove(best);
            out.push(cand);
        }

        out.extend(cold.into_iter().map(|(c, _, _)| c));
        out
    }
}

/// 一条候选的模型健康记录。键与 `order()` 里的权重查找同构：`provider:model`。
fn health_of<'a>(data: &'a AppData, c: &Candidate) -> Option<&'a crate::model::ModelHealth> {
    let p = &data.providers[c.provider];
    data.model_health.get(&format!("{}:{}", p.id, c.model_id))
}

/// 「最智能」的近似排序键。目录里没有权威跑分，能硬比的三样按可信度排：
/// 实测推理 > 实测工具 > 实测视觉，最后拿声明的上下文长度兜底。
/// 实测值优先于目录声明（与能力门禁同一套合并规则）。
fn smart_key(data: &AppData, c: &Candidate) -> (u8, u8, u8, u64) {
    let p = &data.providers[c.provider];
    let declared = p.models.iter().find(|m| m.id == c.model_id);
    let caps = declared
        .map(|m| m.caps)
        .unwrap_or_default()
        .merge_measured(health_of(data, c).map(|h| h.caps).unwrap_or_default());
    let b = |x: Option<bool>| u8::from(x == Some(true));
    (
        b(caps.reasoning),
        b(caps.tools),
        b(caps.vision),
        declared.and_then(|m| m.context).unwrap_or(0),
    )
}

/// 一条候选有多值得先用。三个因子相乘，每个都能在 UI 上解释清楚。
///
/// - **档位**：Tier A 官方免费 > B 匿名端点 > C 订阅/待验证。免费额度是我们唯一确定的事实，优先花。
/// - **余量**：这条**凭据**还剩多少比例。剩得多的先花，避免把一家的分钟额度打到底再换家。
/// - **健康**：实测过的优先。`Degraded`（慢）压到 0.55，但不为 0——慢总比没有强。
///
/// 用户启用三维权重后，第三个因子换成 [`blend_score`]；本函数是「没启用」的那条路。
pub fn weight_of(
    p: &Provider,
    account: &str,
    ledger: &Ledger,
    health: Option<&crate::model::ModelHealth>,
    now: i64,
) -> f64 {
    weight_of_model(p, account, "", ledger, health, now, None)
}

/// 带三维权重的版本。`weights = None` 时与 [`weight_of`] 完全等价。
///
/// 需要 `model_id` 是因为「智能」维度只能从模型元数据（能力、上下文）里读，
/// 凭据与档位都不携带这个信息。
pub fn weight_of_model(
    p: &Provider,
    account: &str,
    model_id: &str,
    ledger: &Ledger,
    health: Option<&crate::model::ModelHealth>,
    now: i64,
    weights: Option<RouteWeights>,
) -> f64 {
    let (tier, remaining, health_f) = weight_factors(p, account, ledger, health, now);
    let last = match weights {
        None => health_f,
        Some(w) => blend_score(
            w,
            health,
            p.models.iter().find(|m| m.id == model_id),
            now,
        ),
    };
    (tier * remaining * last).max(0.01)
}

/// 三维合成评分，落在 0..=1。权重按总和归一，所以「70/15/15」与「7/1.5/1.5」等价。
///
/// 设计取舍：合成结果是**乘在档位与余量之后**的第三个因子，不是替代它们。
/// 档位（免费与否）与余量（还能撑多久）属于「该不该花」，是产品判断；
/// 三个维度属于「两件都能花时先花哪件」，是用户的偏好。两层不该混在一起。
pub fn blend_score(
    w: RouteWeights,
    health: Option<&crate::model::ModelHealth>,
    model: Option<&ModelInfo>,
    now_ms: i64,
) -> f64 {
    let rel = reliability_factor(health, now_ms);
    let spd = speed_factor(health.and_then(|h| h.latency_ms));
    let intel = intelligence_factor(model, health);
    let total =
        (w.reliability as f64 + w.speed as f64 + w.intelligence as f64).max(1.0);
    (w.reliability as f64 * rel + w.speed as f64 * spd + w.intelligence as f64 * intel)
        / total
}

/// 可靠性因子：有足够统计样本就用 Beta 后验，没有才退回状态机。
///
/// 为什么两者不叠加：状态机是「现在挂了没有」（Gate 已经在用），
/// 后验是「长期多靠谱」。乘在一起等于对一次事故罚两遍，
/// 而且会让「刚恢复的模型」永远抬不起头。
///
/// 下限 0.05 而不是 0：SWRR 里权重为 0 等于永不选中，
/// 那样一个被判死的模型连翻身的机会都没有，冷却到期也白搭。
pub fn reliability_factor(health: Option<&crate::model::ModelHealth>, now_ms: i64) -> f64 {
    if let Some(h) = health {
        if let Some(p) = h.reliability(now_ms) {
            return p.clamp(0.05, 1.0);
        }
    }
    health_factor(health.map(|h| h.status))
}

/// 健康 → 可靠性因子。与 [`weight_factors`] 的第三项同源，只写一次常量。
pub fn health_factor(status: Option<HealthStatus>) -> f64 {
    match status {
        Some(HealthStatus::Healthy) | None => 1.0,
        Some(HealthStatus::Unknown) | Some(HealthStatus::Probing) => 0.95,
        Some(HealthStatus::Degraded) => 0.55,
        Some(HealthStatus::Cooling) => 0.3,
        _ => 0.1,
    }
}

/// 速度因子的半衰点：400ms 得 0.5，100ms 得 0.8，2s 得 0.17。
///
/// 没测过给 0.6 —— 中性偏低。给 1.0 等于让「没测过」在速度维度白拿满分，
/// 那样用户开了「速度」权重反而会先打没测过的通道。
pub fn speed_factor(latency_ms: Option<u64>) -> f64 {
    const PIVOT: f64 = 400.0;
    match latency_ms {
        Some(ms) => PIVOT / (PIVOT + ms as f64),
        None => 0.6,
    }
}

/// 智能因子。没有权威跑分，只用目录里最硬的四样近似：
/// 实测推理（0.30）> 实测工具（0.12）> 实测视觉（0.05）> 上下文长度（最多 0.18）。
///
/// 基准 0.35 的用意：一个什么都没标的模型不该被打到 0 —— 那会让「智能」权重
/// 变成「有没有元数据」权重，而不是「谁更聪明」。
pub fn intelligence_factor(
    model: Option<&ModelInfo>,
    health: Option<&crate::model::ModelHealth>,
) -> f64 {
    let caps = model
        .map(|m| m.caps)
        .unwrap_or_default()
        .merge_measured(health.map(|h| h.caps).unwrap_or_default());
    let mut s = 0.35;
    if caps.reasoning == Some(true) {
        s += 0.30;
    }
    if caps.tools == Some(true) {
        s += 0.12;
    }
    if caps.vision == Some(true) {
        s += 0.05;
    }
    let ctx = model.and_then(|m| m.context).unwrap_or(0) as f64;
    // 32K → 0.09，128K → 0.144，1M → 0.174，永远不会到满
    s += 0.18 * (ctx / (ctx + 32_768.0));
    s.clamp(0.0, 1.0)
}

/// 把 `weight_of` 的三个因子单独返回，供调试接口与界面逐个摊开。

/// canary 系数：feed 新加进来、还没跑满 [`crate::model::CANARY_OKS`] 次的模型只给 0.3 倍权重。
///
/// 为什么不在 Gate 层直接拒：新模型能不能用，只有真流量知道。直接拒会把
/// 「发布即到」变成「发布即等待」，而且社区也永远验证不了它到底能不能跑。
pub fn canary_factor(
    data: &AppData,
    provider: &str,
    model: &str,
    health: Option<&crate::model::ModelHealth>,
) -> f64 {
    let candidate = data
        .providers
        .iter()
        .find(|p| p.id == provider)
        .and_then(|p| p.models.iter().find(|m| m.id == model))
        .map(|m| m.candidate)
        .unwrap_or(false);
    if !candidate {
        return 1.0;
    }
    let oks = health.map(|h| h.oks).unwrap_or(0);
    if oks >= crate::model::CANARY_OKS {
        1.0
    } else {
        0.3
    }
}

/// 能力未知的折价因子。见 [`CAP_UNKNOWN_FACTOR`]。
pub fn cap_factor(c: &Candidate) -> f64 {
    if c.cap_unknown {
        CAP_UNKNOWN_FACTOR
    } else {
        1.0
    }
}

/// 把 `weight_of` 的三个因子单独返回，供调试接口与界面逐个摊开。
/// 因子公式只在这里写一次——UI 不复制常量，也就不会和实现漂移。
pub fn weight_factors(
    p: &Provider,
    account: &str,
    ledger: &Ledger,
    health: Option<&crate::model::ModelHealth>,
    now: i64,
) -> (f64, f64, f64) {
    let tier = match p.tier.as_str() {
        "A" | "a" => 1.0,
        "B" | "b" => 0.85,
        _ => 0.6,
    };
    let remaining = 0.2 + 0.8 * ledger.remaining_ratio(account, now);
    let health_f = reliability_factor(health, now);
    (tier, remaining, health_f)
}

/// 把这段会话上次的答主提到最前。
///
/// 优先精确到**同一凭据**（prompt cache 挂在那条 key 所属的账号上），
/// 退到同一家厂商的任意凭据，再退到同一家的任意模型。
///
/// 返回「粘性命中」——即那家确实还在候选里。
/// **注意它可能本来就在最前**：命中与否不能看「顺序有没有变」，
/// 否则「刚好排第一」会被当成没命中，反手被 SWRR 轮走。
pub fn stick_to_front(
    plan: &mut Plan,
    provider: &str,
    account: &str,
    model: &str,
    data: &AppData,
) -> bool {
    stick_to_front_if(plan, provider, account, model, data, &|_| true)
}

/// 带准入条件的粘性。
///
/// 唯一的用途：请求带了能力要求，而上次那家没被测过这个能力时，
/// 不该为了 prompt cache 冒一次 400 的风险 —— 让确认支持的那家先上。
pub fn stick_to_front_if(
    plan: &mut Plan,
    provider: &str,
    account: &str,
    model: &str,
    data: &AppData,
    allow: &dyn Fn(&Candidate) -> bool,
) -> bool {
    if plan.candidates.is_empty() {
        return false;
    }
    let exact = plan.candidates.iter().position(|c| {
        allow(c) && data.providers[c.provider].id == provider && c.account == account && c.model_id == model
    });
    let idx = exact
        .or_else(|| plan.candidates.iter().position(|c| allow(c) && c.account == account))
        .or_else(|| {
            plan.candidates
                .iter()
                .position(|c| allow(c) && data.providers[c.provider].id == provider)
        });
    match idx {
        None => false,
        Some(0) => true,
        Some(i) => {
            let c = plan.candidates.remove(i);
            plan.candidates.insert(0, c);
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Account, Health, ModelInfo};

    fn provider(id: &str, tier: &str, models: &[&str]) -> Provider {
        Provider {
            id: id.into(),
            name: id.into(),
            base_url: "https://example.com/v1".into(),
            tier: tier.into(),
            homepage: None,
            note: None,
            key_mask: "sk-••••1234".into(),
            enabled: true,
            added_at: String::new(),
            proxy: None,
            wire: None,
            quota: None,
            accounts: vec![Account {
                id: format!("{id}_a"),
                name: "default".into(),
                key_mask: "sk-••••1234".into(),
                enabled: true,
                added_at: String::new(),
                quota: None,
                health: None,
                purposes: Vec::new(),
            }],
            models: models
                .iter()
                .map(|m| ModelInfo {
                    id: (*m).into(),
                    owned_by: id.into(),
                    context: None,
                    created: None,
                    free: None,
                    caps: Default::default(),
                    candidate: false,
                })
                .collect(),
            health: Health::default(),
        }
    }

    fn data() -> AppData {
        AppData {
            providers: vec![provider("groq", "A", &["llama-3", "mixtral"]), provider("cerebras", "A", &["qwen-3"])],
            routing: crate::model::RoutingConfig {
                strategy: "balanced".into(),
                chain: vec!["cerebras:qwen-3".into()],
                ..Default::default()
            },
            ..Default::default()
        }
    }

    /// 接了但没开 / 没 key 的厂商，必须出现在 rejected 里并带上原因，
    /// 否则界面上「我接过它，为什么不在链里」永远无解。
    #[test]
    fn provider_level_reasons_are_reported() {
        let mut d = data();
        d.providers[1].enabled = false;
        let cd = Cooldowns::new();
        let has_key = |a: &Account| !a.id.starts_with("groq");
        let p = plan(&d, "auto", &cd, &has_key, &Ledger::default());
        assert!(p.candidates.iter().all(|c| d.providers[c.provider].id != "groq"));
        assert!(p
            .rejected
            .iter()
            .any(|r| r.provider == "groq" && r.kind == "nokey"));
        assert!(p
            .rejected
            .iter()
            .any(|r| r.provider == "cerebras" && r.kind == "disabled"));
    }

    #[test]
    fn explicit_model_wins() {
        let d = data();
        let cd = Cooldowns::new();
        let p = plan(&d, "cerebras:qwen-3", &cd, &|_| true, &Ledger::default());
        assert_eq!(p.candidates[0].model_id, "qwen-3");
        assert_eq!(d.providers[p.candidates[0].provider].id, "cerebras");
    }

    /// auto 只花免费额度：目录明确标了付费的候选被拦下，理由是 paid。
    /// `free: None` 必须放行——目录没给价格不等于收费，否则 llm7 这类
    /// 免费源会被整家踢出链，最后只剩几家失败。
    #[test]
    fn auto_skips_paid_models_but_keeps_unknown() {
        let mut d = data();
        d.routing.chain.clear();
        d.providers[0].models[0].free = Some(false);
        d.providers[0].models[1].free = Some(true);
        let p = plan(&d, "auto", &Cooldowns::new(), &|_| true, &Ledger::default());
        assert!(
            p.candidates.iter().all(|c| c.model_id != "llama-3"),
            "付费模型不能进 auto 的候选链"
        );
        assert!(p.candidates.iter().any(|c| c.model_id == "mixtral"), "免费模型照常");
        assert!(
            p.candidates.iter().any(|c| c.model_id == "qwen-3"),
            "价格未知不是付费，必须放行"
        );
        assert!(p.rejected.iter().any(|r| r.model == "llama-3" && r.kind == "paid"));
    }

    /// 显式点名不受免费约束：用户写了 `groq:llama-3` 就是要花钱用它。
    #[test]
    fn explicit_paid_model_still_routes() {
        let mut d = data();
        d.providers[0].models[0].free = Some(false);
        let p = plan(&d, "groq:llama-3", &Cooldowns::new(), &|_| true, &Ledger::default());
        assert!(p.candidates.iter().any(|c| c.model_id == "llama-3"));
    }

    /// 兜底候选要越过前面的付费模型去拿免费的：一家「前两个是付费、
    /// 免费的在后面」的厂商不能因此一条候选都进不来。
    #[test]
    fn auto_fallback_looks_past_paid_models() {
        let mut d = AppData {
            providers: vec![provider("openrouter", "A", &["paid-1", "paid-2", "free-1"])],
            ..Default::default()
        };
        d.providers[0].models[0].free = Some(false);
        d.providers[0].models[1].free = Some(false);
        d.providers[0].models[2].free = Some(true);
        let p = plan(&d, "auto", &Cooldowns::new(), &|_| true, &Ledger::default());
        assert!(p.candidates.iter().any(|c| c.model_id == "free-1"));
        assert!(p.candidates.iter().all(|c| c.model_id != "paid-1"));
    }

    /// 下架的模型必须在 Gate 层被拦下，且原因是 tombstone 而不是 quota/health。
    /// 这是「删掉它，路由立刻不再选」那条验收的内核部分。
    #[test]
    fn tombstoned_model_is_gated_out() {
        let mut d = data();
        d.feed.tombstones.insert("cerebras:qwen-3".into());
        let cd = Cooldowns::new();
        let p = plan(&d, "cerebras:qwen-3", &cd, &|_| true, &Ledger::default());
        assert!(
            p.candidates
                .iter()
                .all(|c| !(d.providers[c.provider].id == "cerebras" && c.model_id == "qwen-3")),
            "下架的模型不能出现在候选链里"
        );
        assert!(p.rejected.iter().any(|r| r.kind == "tombstone"));
        // 点名到被下架的模型时不换别的模型顶上：用户要的是那个模型，
        // 悄悄换一个只会让问题更晚被发现。auto 才继续接力。
        assert!(p.is_empty(), "点名的模型被下架时不能拿别的模型顶上");
        let a = plan(&d, "auto", &cd, &|_| true, &Ledger::default());
        assert!(!a.candidates.is_empty(), "auto 照常接力");
    }

    /// feed 新模型 0.3 倍权重，跑满 20 次成功才转正。
    #[test]
    fn candidate_weight_ramps_up_after_canary() {
        let mut d = data();
        d.providers[0].models[0].candidate = true;
        let cold = crate::model::ModelHealth::default();
        assert!((canary_factor(&d, "groq", "llama-3", Some(&cold)) - 0.3).abs() < 1e-9);
        assert_eq!(
            canary_factor(&d, "groq", "mixtral", Some(&cold)),
            1.0,
            "老模型不受 canary 影响"
        );
        let warm = crate::model::ModelHealth {
            oks: crate::model::CANARY_OKS,
            ..Default::default()
        };
        assert_eq!(canary_factor(&d, "groq", "llama-3", Some(&warm)), 1.0);
    }

    #[test]
    fn chain_comes_before_fallback() {
        let d = data();
        let cd = Cooldowns::new();
        let p = plan(&d, "auto", &cd, &|_| true, &Ledger::default());
        assert_eq!(p.candidates[0].model_id, "qwen-3");
        assert!(p.candidates.len() >= 3);
    }

    #[test]
    fn open_breaker_removes_candidate() {
        let d = data();
        let cd = Cooldowns::new();
        cd.hit(crate::error::Scope::Key, "groq", "groq_a", None, 60_000, "rate_limit");
        let p = plan(&d, "auto", &cd, &|_| true, &Ledger::default());
        assert!(p.candidates.iter().all(|c| d.providers[c.provider].id != "groq"));
        assert!(p.rejected.iter().any(|r| r.provider == "groq"));
    }

    // ── Order / Pick ──

    fn two_even() -> (AppData, Plan) {
        let d = AppData {
            providers: vec![provider("alpha", "A", &["m1"]), provider("beta", "A", &["m1"])],
            ..Default::default()
        };
        let cd = Cooldowns::new();
        let p = plan(&d, "auto", &cd, &|_| true, &Ledger::default());
        (d, p)
    }

    #[test]
    fn swrr_rotates_instead_of_repeating() {
        let (d, p) = two_even();
        let mut swrr = Swrr::new();
        let ledger = Ledger::default();
        let firsts: Vec<String> = (0..4)
            .map(|_| {
                let ordered = swrr.order(p.candidates.clone(), &d, &ledger, 0);
                d.providers[ordered[0].provider].id.clone()
            })
            .collect();
        // 等权两家的理想轮换是交替，而不是连中同一家
        assert_eq!(firsts, vec!["alpha", "beta", "alpha", "beta"]);
    }

    #[test]
    fn weight_shrinks_as_quota_drains() {
        let (d, _) = two_even();
        let p = &d.providers[0];
        let mut ledger = Ledger::default();
        let fresh = weight_of(p, "alpha_a", &ledger, None, 1_000);
        // 把日额度用掉八成
        ledger.reserve("alpha_a", [0.0, 10.0, 0.0, 0.0], 0.0, false, 1_000);
        for _ in 0..8 {
            ledger.reserve("alpha_a", [0.0, 10.0, 0.0, 0.0], 0.0, false, 1_000);
        }
        let low = weight_of(p, "alpha_a", &ledger, None, 1_000);
        assert!(low < fresh, "余量越少权重越低：{low:.3} vs {fresh:.3}");
        assert!(low > 0.0, "见底也要保留出手机会，交给 Gate 判断能不能发");
    }

    #[test]
    fn degraded_model_weighs_less_than_healthy() {
        let (d, _) = two_even();
        let p = &d.providers[0];
        let ledger = Ledger::default();
        let mut h = crate::model::ModelHealth {
            status: HealthStatus::Healthy,
            ..Default::default()
        };
        let healthy = weight_of(p, "alpha_a", &ledger, Some(&h), 0);
        h.status = HealthStatus::Degraded;
        let slow = weight_of(p, "alpha_a", &ledger, Some(&h), 0);
        assert!(slow < healthy);
    }

    #[test]
    fn stick_moves_the_previous_answerer_to_front() {
        let (d, mut p) = two_even();
        assert_eq!(d.providers[p.candidates[0].provider].id, "alpha");
        assert!(stick_to_front(&mut p, "beta", "beta_a", "m1", &d));
        assert_eq!(d.providers[p.candidates[0].provider].id, "beta");
        // 本来就在最前，也算是命中——否则会被 SWRR 反手轮走
        assert!(stick_to_front(&mut p, "beta", "beta_a", "m1", &d));
        assert_eq!(d.providers[p.candidates[0].provider].id, "beta");
    }

    #[test]
    fn stick_falls_back_to_the_same_provider_when_model_is_gone() {
        let (d, mut p) = two_even();
        // 模型改了名，但「那一家」的缓存还在
        assert!(stick_to_front(&mut p, "beta", "beta_a", "old-model-name", &d));
        assert_eq!(d.providers[p.candidates[0].provider].id, "beta");
    }

    #[test]
    fn stick_ignores_provider_that_is_no_longer_a_candidate() {
        let (d, mut p) = two_even();
        let before: Vec<String> = p
            .candidates
            .iter()
            .map(|c| d.providers[c.provider].id.clone())
            .collect();
        assert!(!stick_to_front(&mut p, "gone-provider", "gone_a", "m1", &d));
        let after: Vec<String> = p
            .candidates
            .iter()
            .map(|c| d.providers[c.provider].id.clone())
            .collect();
        assert_eq!(before, after, "找不到就保持原顺序");
    }

    #[test]
    fn alias_shell_is_stripped_and_inner_syntax_still_works() {
        assert_eq!(resolve_alias("claude-freeport-auto"), "auto");
        assert_eq!(resolve_alias("claude-freeport-groq:llama-3.3-70b"), "groq:llama-3.3-70b");
        assert_eq!(resolve_alias("auto"), "auto", "没有壳的名字原样返回");
        assert_eq!(resolve_alias("claude-sonnet-4-5"), "claude-sonnet-4-5", "别人的 claude 名不是我们的壳");
    }

    #[test]
    fn aliases_start_with_auto_and_drop_duplicate_model_names() {
        let mut d = data();
        // 第二家也有同名模型：只留一条，并且是**组**（不带厂商前缀），
        // 因为它现在覆盖两条通道
        d.providers[1].models.push(ModelInfo {
            id: "llama-3".into(),
            owned_by: "cerebras".into(),
            context: None,
            created: None,
            free: None,
            caps: Default::default(),
            candidate: false,
        });

        let list = aliases(&d, &|_| true);
        let names: Vec<&str> = list.iter().map(|a| a.model.as_str()).collect();
        assert_eq!(names, vec!["auto", "llama-3", "mixtral", "qwen-3"]);
        assert!(list[0].is_virtual());
        assert_eq!(list[1].alias, "claude-freeport-llama-3");
        assert!(list[1].is_group(), "两家都有 llama-3，别名必须是组");
        assert_eq!(list[1].members, vec!["groq", "cerebras"]);
        // 别名脱壳后必须刚好是 plan 能解析的语法：组名就是裸模型名
        assert_eq!(resolve_alias(&list[1].alias), "llama-3");
        // 只在一家出现的模型保留厂商前缀：这时「哪一家」是用户要知道的信息
        assert_eq!(list[2].alias, "claude-freeport-groq:mixtral");
        assert!(!list[2].is_group());
    }

    /// 组的本质：裸模型名覆盖所有声明它的厂商，而不是「第一家」。
    /// 只取第一家的话，「5 家都能跑」在界面上是一句话，在路由里是一条通道。
    #[test]
    fn bare_model_name_covers_every_provider_that_offers_it() {
        let d = AppData {
            providers: vec![
                provider("groq", "A", &["llama-3"]),
                provider("cerebras", "A", &["llama-3"]),
                provider("other", "B", &["llama-3"]),
            ],
            ..Default::default()
        };
        let p = plan(&d, "llama-3", &Cooldowns::new(), &|_| true, &Ledger::default());
        let on: Vec<&str> = p
            .candidates
            .iter()
            .map(|c| d.providers[c.provider].id.as_str())
            .collect();
        assert_eq!(on, vec!["groq", "cerebras", "other"], "三家的凭据都要在链上");
        assert!(p.candidates.iter().all(|c| c.model_id == "llama-3"));
    }

    /// 组里一家冷却，另一家照用：这是「组」比「第一家」值钱的地方。
    #[test]
    fn group_survives_one_member_cooling() {
        let d = AppData {
            providers: vec![provider("groq", "A", &["llama-3"]), provider("cerebras", "A", &["llama-3"])],
            ..Default::default()
        };
        let cd = Cooldowns::new();
        // 把 groq 整家按掉：组里还剩 cerebras
        cd.hit_with(
            crate::cooldown::Source::Heuristic,
            crate::Scope::Provider,
            "groq",
            "groq_a",
            None,
            60_000,
            "upstream 500",
        );
        let p = plan(&d, "llama-3", &cd, &|_| true, &Ledger::default());
        assert!(p.candidates.iter().all(|c| d.providers[c.provider].id != "groq"));
        assert!(p.candidates.iter().any(|c| d.providers[c.provider].id == "cerebras"));
    }

    /// 显式点名不兜底到别的模型：把别的模型补在后面，会让一次点名请求
    /// 安静地落到另一个模型上——客户端只看得到响应里的 model 变了。
    /// 点名无路可走时的正确回应是 503 说清原因（server 侧），不是换模型。
    #[test]
    fn explicit_model_does_not_fall_back_to_another_model() {
        let d = data();
        // 裸名不在任何目录里 → 组为空
        let p = plan(&d, "does-not-exist", &Cooldowns::new(), &|_| true, &Ledger::default());
        assert!(p.is_empty(), "点名的模型不存在时不能拿别的模型顶上");
        // 带厂商前缀的点名叫了个目录里没有的模型：照样只发那一家
        // （上游认不认是上游的事，本地目录没记录不该拦住显式点名）
        let q = plan(&d, "groq:brand-new-model", &Cooldowns::new(), &|_| true, &Ledger::default());
        assert_eq!(q.candidates.len(), 1);
        assert_eq!(d.providers[q.candidates[0].provider].id, "groq");
        assert_eq!(q.candidates[0].model_id, "brand-new-model");
    }

    /// auto 仍然兜底：那条契约没变。
    #[test]
    fn auto_still_falls_back_to_other_models() {
        let d = data();
        let p = plan(&d, "auto", &Cooldowns::new(), &|_| true, &Ledger::default());
        assert!(p.candidates.len() > 1);
        assert!(p.candidates.iter().any(|c| c.model_id != "qwen-3"));
    }

    /// 从上游 429 学到的模型级限额只拦那一个模型：账号还有余量不等于
    /// 这个模型还有——模型级 TPM/RPD 是跟着 key 走的，得分开记。
    #[test]
    fn learned_model_quota_excludes_only_that_model() {
        let d = AppData {
            providers: vec![provider("groq", "A", &["llama-3", "mixtral"])],
            ..Default::default()
        };
        let mut ledger = Ledger::default();
        let now = now_ms();
        ledger.learn_model("groq_a", "llama-3", crate::quota::WindowKind::Rpd, 1.0, now);
        ledger.reserve_model("groq_a", "llama-3", 0.0, false, now);
        assert!(!ledger.model_has_room("groq_a", "llama-3", now));

        let p = plan_with(
            &d,
            "auto",
            &Cooldowns::new(),
            &|_| true,
            &ledger,
            Needs::default(),
            Purpose::Chat,
        );
        assert!(p.candidates.iter().all(|c| c.model_id != "llama-3"), "学过耗尽的模型不进链");
        assert!(p.candidates.iter().any(|c| c.model_id == "mixtral"), "同 key 的其它模型照常");
        assert!(p
            .rejected
            .iter()
            .any(|r| r.kind == "quota" && r.model == "llama-3"));
        // 显式点名也一样拦：模型级限额是硬事实，不是排序偏好
        let q = plan(&d, "groq:llama-3", &Cooldowns::new(), &|_| true, &ledger);
        assert!(q.is_empty());
    }

    #[test]
    fn aliases_skip_provider_without_credential() {
        let d = data();
        let list = aliases(&d, &|a: &Account| a.id != "cerebras_a");
        assert!(!list.iter().any(|a| a.provider == "cerebras"), "没 key 的厂商不该被列出来");
        assert!(list.iter().any(|a| a.provider == "groq"));
    }

    #[test]
    fn aliases_skip_tombstoned_model() {
        let mut d = data();
        d.feed.tombstones.insert("groq:mixtral".into());
        let list = aliases(&d, &|_| true);
        assert!(!list.iter().any(|a| a.model == "mixtral"));
    }

    /* ── 能力门禁 ── */

    fn with_caps(mut d: AppData, provider: &str, model: &str, caps: ModelCaps) -> AppData {
        let p = d.providers.iter_mut().find(|p| p.id == provider).unwrap();
        p.models.iter_mut().find(|m| m.id == model).unwrap().caps = caps;
        d
    }

    fn vision_yes() -> ModelCaps {
        ModelCaps {
            vision: Some(true),
            ..Default::default()
        }
    }

    #[test]
    fn vision_request_skips_model_declared_text_only() {
        let d = with_caps(
            data(),
            "groq",
            "llama-3",
            ModelCaps {
                vision: Some(false),
                ..Default::default()
            },
        );
        let p = plan_with(
            &d,
            "groq:llama-3",
            &Cooldowns::default(),
            &|_| true,
            &Ledger::default(),
            Needs {
                vision: true,
                ..Default::default()
            },
            Purpose::Chat,
        );
        assert!(
            p.candidates.iter().all(|c| c.model_id != "llama-3"),
            "声明不支持图像的模型不能进链"
        );
        let r = p.rejected.iter().find(|r| r.kind == "capability").expect("要给出能力原因");
        assert!(r.reason.contains("vision"));
        // 点名严格：点了不满足能力要求的模型就明确拒绝，不拿同厂商的
        // 其他模型顶上（那会让请求安静地换模型跑）。
        // auto 仍然可用：能力门禁只是把这条候选从链上剔掉。
        assert!(p.is_empty(), "点名到不满足能力要求的模型要明确拒绝");
        let a = plan_with(
            &d,
            "auto",
            &Cooldowns::default(),
            &|_| true,
            &Ledger::default(),
            Needs {
                vision: true,
                ..Default::default()
            },
            Purpose::Chat,
        );
        assert!(!a.candidates.is_empty(), "auto 绕开它继续跑");
    }

    #[test]
    fn text_request_still_uses_text_only_model() {
        let d = with_caps(
            data(),
            "groq",
            "llama-3",
            ModelCaps {
                vision: Some(false),
                ..Default::default()
            },
        );
        let p = plan(&d, "groq:llama-3", &Cooldowns::default(), &|_| true, &Ledger::default());
        assert_eq!(p.candidates[0].model_id, "llama-3", "纯文本请求不该被能力门禁挡下");
        assert!(p.rejected.iter().all(|r| r.kind != "capability"));
    }

    #[test]
    fn unknown_capability_is_kept_but_deprioritised() {
        // groq:llama-3 没测过视觉，cerebras:qwen-3 明确支持
        let d = with_caps(data(), "cerebras", "qwen-3", vision_yes());
        let needs = Needs {
            vision: true,
            ..Default::default()
        };
        let p = plan_with(
            &d,
            "auto",
            &Cooldowns::default(),
            &|_| true,
            &Ledger::default(),
            needs,
            Purpose::Chat,
        );
        assert_eq!(p.candidates.len(), 3, "未知不等于不支持，仍要留在链里");

        let known = p.candidates.iter().find(|c| c.model_id == "qwen-3").unwrap();
        let unknown = p.candidates.iter().find(|c| c.model_id == "llama-3").unwrap();
        assert!(!known.cap_unknown);
        assert!(unknown.cap_unknown);
        assert!(cap_factor(unknown) < cap_factor(known));

        // 权重差要真的把确认支持的排到前面 —— 否则折价只是纸面数字
        let mut swrr = Swrr::new();
        let ordered = swrr.order(p.candidates, &d, &Ledger::default(), now_ms());
        assert_eq!(ordered[0].model_id, "qwen-3");
    }

    #[test]
    fn tools_need_filters_models_without_tools() {
        let d = with_caps(
            data(),
            "cerebras",
            "qwen-3",
            ModelCaps {
                tools: Some(false),
                ..Default::default()
            },
        );
        let p = plan_with(
            &d,
            "auto",
            &Cooldowns::default(),
            &|_| true,
            &Ledger::default(),
            Needs {
                tools: true,
                ..Default::default()
            },
            Purpose::Chat,
        );
        assert!(p.candidates.iter().all(|c| c.model_id != "qwen-3"));
    }

    #[test]
    fn measured_capability_beats_declared_one() {
        let mut d = with_caps(data(), "groq", "llama-3", vision_yes());
        // 目录说支持，实测打脸：门禁要以实测为准
        d.model_health.insert(
            "groq:llama-3".into(),
            crate::model::ModelHealth {
                caps: ModelCaps {
                    vision: Some(false),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        let p = plan_with(
            &d,
            "groq:llama-3",
            &Cooldowns::default(),
            &|_| true,
            &Ledger::default(),
            Needs {
                vision: true,
                ..Default::default()
            },
            Purpose::Chat,
        );
        assert!(
            p.candidates.iter().all(|c| c.model_id != "llama-3"),
            "实测说不支持，目录说支持也不算"
        );
    }

    #[test]
    fn needs_are_read_from_the_openai_shape() {
        let vision = serde_json::json!({
            "messages": [{ "role": "user", "content": [
                { "type": "text", "text": "看图" },
                { "type": "image_url", "image_url": { "url": "data:image/png;base64,xx" } }
            ]}]
        });
        assert!(Needs::of_chat(&vision).vision);

        let tools = serde_json::json!({
            "messages": [{ "role": "user", "content": "hi" }],
            "tools": [{ "type": "function", "function": { "name": "f" } }]
        });
        let n = Needs::of_chat(&tools);
        assert!(n.tools && !n.vision && !n.json);

        let plain = serde_json::json!({
            "messages": [{ "role": "user", "content": "hi" }],
            "tool_choice": "none"
        });
        assert!(!Needs::of_chat(&plain).any(), "tool_choice=none 不算需要工具");

        let json = serde_json::json!({
            "messages": [{ "role": "user", "content": "json" }],
            "response_format": { "type": "json_object" }
        });
        assert!(Needs::of_chat(&json).json);
    }

    // ── 全冷却回退 ────────────────────────────────────────────────
    //
    // 判据只有一条：**这个冷却是不是马上就结束了**。还剩 10 秒的限流值得
    // 试一次；要到明天的日额度耗尽不值得——撞了也只是多一条失败日志，
    // 而用户真正需要知道的是「几点恢复」。

    fn all_cooling(ms: u64) -> (AppData, Cooldowns) {
        use crate::error::Scope;
        let d = data();
        let cd = Cooldowns::new();
        cd.hit(Scope::Provider, "groq", "groq_a", None, ms, "rate_limit");
        cd.hit(Scope::Provider, "cerebras", "cerebras_a", None, ms, "rate_limit");
        (d, cd)
    }

    #[test]
    fn all_cooling_with_short_rest_releases_exactly_one_probe() {
        let (d, cd) = all_cooling(10_000);
        let has_key = |_: &Account| true;
        let p = plan(&d, "auto", &cd, &has_key, &Ledger::default());
        assert_eq!(p.candidates.len(), 1, "只放一个试探，不把整轮冷却硬闯一遍");
        assert!(p.candidates[0].half_open, "回退要按半开算：成功即解锁");
        assert!(
            p.rejected.iter().all(|r| r.kind == "cooldown"),
            "拒绝原因不能丢，界面还要靠它解释为什么刚才没走这条"
        );
    }

    #[test]
    fn all_cooling_with_long_rest_stays_rejected() {
        // 一小时后的额度恢复，撞一次没有任何收益
        let (d, cd) = all_cooling(3_600_000);
        let has_key = |_: &Account| true;
        let p = plan(&d, "auto", &cd, &has_key, &Ledger::default());
        assert!(p.candidates.is_empty());
    }

    #[test]
    fn fallback_can_be_switched_off() {
        let (mut d, cd) = all_cooling(10_000);
        d.routing.fallback_when_all_cooling = false;
        let has_key = |_: &Account| true;
        assert!(plan(&d, "auto", &cd, &has_key, &Ledger::default()).candidates.is_empty());
    }

    #[test]
    fn fallback_never_overrides_quota_or_capability() {
        // 额度耗尽是硬事实，不是「等一会儿就好了」；回退不能把它撬开
        let d = data();
        let cd = Cooldowns::new();
        let mut ledger = Ledger::default();
        for p in d.providers.iter() {
            for a in p.accounts.iter() {
                ledger.reserve_now(&a.id, [100.0; 4], 999.0, false);
            }
        }
        let has_key = |_: &Account| true;
        let p = plan(&d, "auto", &cd, &has_key, &ledger);
        assert!(p.candidates.is_empty(), "回退只针对 cooldown，不针对账本");
    }

    // ── 账号按用途分池 ────────────────────────────────────────────

    /// 没打标签的号必须什么活都接。
    ///
    /// 这条是兼容性红线：存量配置全都没有 `purposes` 字段，如果空列表被读成
    /// 「什么都不能干」，升级那一刻所有人都会断网。
    #[test]
    fn empty_purposes_admits_every_purpose() {
        let d = data();
        let cd = Cooldowns::new();
        let led = Ledger::default();
        for purpose in Purpose::ALL {
            let p = plan_with(&d, "auto", &cd, &|_| true, &led, Needs::default(), purpose);
            assert_eq!(p.candidates.len(), 3, "不限用途的号在 {purpose:?} 下也要进链");
            assert!(p.rejected.iter().all(|r| r.kind != "purpose"));
        }
    }

    #[test]
    fn purposes_narrow_the_pool() {
        let mut d = data();
        d.providers[0].accounts[0].purposes = vec![Purpose::Coding];
        let cd = Cooldowns::new();
        let p = plan_with(
            &d,
            "auto",
            &cd,
            &|_| true,
            &Ledger::default(),
            Needs::default(),
            Purpose::Chat,
        );
        assert!(
            p.candidates.iter().all(|c| c.account != "groq_a"),
            "标成 coding 的号不该接 chat 请求"
        );
        assert!(!p.candidates.is_empty(), "另一家没打标签，仍然可用");

        let r = p
            .rejected
            .iter()
            .find(|r| r.kind == "purpose")
            .expect("被分池挡掉要说原因，不能只让候选凭空变少");
        assert_eq!(r.account.as_deref(), Some("groq_a"));
        assert!(r.reason.contains("chat") && r.reason.contains("coding"), "{}", r.reason);
    }

    /// 图片请求只看「谁接了 vision」，不因为发送方是 coding agent 就走 coding 池。
    #[test]
    fn vision_request_only_uses_accounts_that_admit_vision() {
        let mut d = data();
        d.providers[0].accounts[0].purposes = vec![Purpose::Coding];
        d.providers[1].accounts[0].purposes = vec![Purpose::Vision];
        let cd = Cooldowns::new();
        let p = plan_with(
            &d,
            "auto",
            &cd,
            &|_| true,
            &Ledger::default(),
            Needs {
                vision: true,
                ..Default::default()
            },
            Purpose::Vision,
        );
        assert!(p.candidates.iter().all(|c| c.account == "cerebras_a"));
        assert!(!p.candidates.is_empty());
    }

    /// 全池被挡光时，链是空的，但原因必须是 `purpose`——
    /// 上层据此说「没有账号接这个用途」，而不是「都在冷却」。
    #[test]
    fn all_accounts_in_the_wrong_pool_yields_empty_chain_with_purpose_reason() {
        let mut d = data();
        for p in d.providers.iter_mut() {
            for a in p.accounts.iter_mut() {
                a.purposes = vec![Purpose::Chat];
            }
        }
        let p = plan_with(
            &d,
            "auto",
            &Cooldowns::new(),
            &|_| true,
            &Ledger::default(),
            Needs::default(),
            Purpose::Coding,
        );
        assert!(p.candidates.is_empty());
        assert!(!p.rejected.is_empty());
        assert!(p.rejected.iter().all(|r| r.kind == "purpose"));
    }

    /// 命名链：`auto:<profile>` 的条目排在全局链与兜底之前。
    #[test]
    fn named_chain_goes_first() {
        let mut d = data();
        d.routing.chain = vec!["groq:llama-3".into()];
        d.routing
            .chains
            .insert("coding".into(), vec!["cerebras:qwen-3".into()]);

        let p = plan(
            &d,
            "auto:coding",
            &Cooldowns::new(),
            &|_| true,
            &Ledger::default(),
        );
        assert_eq!(
            d.providers[p.candidates[0].provider].id,
            "cerebras",
            "命名链的第一条必须排在最前"
        );
        assert_eq!(p.candidates[0].model_id, "qwen-3");
    }

    /// `auto:<profile>` 必须被当成 auto 处理：不加这个 guard，
    /// 它会被拆成厂商 `auto` 再悄悄拼出空计划。
    #[test]
    fn auto_profile_is_not_mistaken_for_a_provider() {
        let d = data();
        let p = plan(
            &d,
            "auto:nope",
            &Cooldowns::new(),
            &|_| true,
            &Ledger::default(),
        );
        assert!(!p.candidates.is_empty(), "profile 不存在时应回落兜底扫描");
        assert!(
            p.rejected.iter().all(|r| r.provider != "auto"),
            "不该把 auto 当厂商去解释"
        );
    }

    /// 用途映射：coding 池默认走自己的链。
    #[test]
    fn purpose_mapping_selects_its_chain() {
        let mut d = data();
        d.routing.chain = vec!["groq:llama-3".into()];
        d.routing
            .chains
            .insert("strong".into(), vec!["cerebras:qwen-3".into()]);
        d.routing
            .purpose_chain
            .insert(Purpose::Coding.as_str().into(), "strong".into());

        let p = plan_with(
            &d,
            "auto",
            &Cooldowns::new(),
            &|_| true,
            &Ledger::default(),
            Needs::default(),
            Purpose::Coding,
        );
        assert!(p
            .candidates
            .iter()
            .any(|c| d.providers[c.provider].id == "cerebras"));
    }

    /// order 策略保持链顺序；rotate 每次换一个起点且严格轮转。
    #[test]
    fn order_and_rotate_behave_predictably() {
        let d = data();
        let ledger = Ledger::default();
        let mk = |ids: &[&str]| -> Vec<Candidate> {
            ids.iter()
                .map(|id| Candidate {
                    provider: 0,
                    account: format!("{id}_a"),
                    model_id: (*id).into(),
                    half_open: false,
                    cap_unknown: false,
                })
                .collect()
        };

        let mut s = Swrr::new();
        let v = s.order_with(
            Strategy::Order,
            mk(&["a", "b", "c"]),
            &d,
            &ledger,
            now_ms(),
        );
        assert_eq!(
            v.iter().map(|c| c.model_id.as_str()).collect::<Vec<_>>(),
            ["a", "b", "c"]
        );

        let mut s = Swrr::new();
        let first = s.order_with(
            Strategy::Rotate,
            mk(&["a", "b", "c"]),
            &d,
            &ledger,
            now_ms(),
        );
        let second = s.order_with(
            Strategy::Rotate,
            mk(&["a", "b", "c"]),
            &d,
            &ledger,
            now_ms(),
        );
        assert_eq!(first[0].model_id, "a");
        assert_eq!(second[0].model_id, "b", "第二次轮转应从下一条起");
    }

    /// fastest：实测延迟最短的先上，没测过的让位给测过的。
    #[test]
    fn fastest_sorts_by_probe_latency() {
        let mut d = data();
        d.model_health.insert(
            "groq:llama-3".into(),
            crate::model::ModelHealth {
                latency_ms: Some(400),
                ..Default::default()
            },
        );
        d.model_health.insert(
            "groq:mixtral".into(),
            crate::model::ModelHealth {
                latency_ms: Some(90),
                ..Default::default()
            },
        );

        let mut s = Swrr::new();
        let v = s.order_with(
            Strategy::Fastest,
            vec![
                Candidate {
                    provider: 0,
                    account: "groq_a".into(),
                    model_id: "llama-3".into(),
                    half_open: false,
                    cap_unknown: false,
                },
                Candidate {
                    provider: 0,
                    account: "groq_a".into(),
                    model_id: "mixtral".into(),
                    half_open: false,
                    cap_unknown: false,
                },
                Candidate {
                    provider: 1,
                    account: "cerebras_a".into(),
                    model_id: "qwen-3".into(),
                    half_open: false,
                    cap_unknown: false,
                },
            ],
            &d,
            &Ledger::default(),
            now_ms(),
        );
        assert_eq!(
            v.iter().map(|c| c.model_id.as_str()).collect::<Vec<_>>(),
            ["mixtral", "llama-3", "qwen-3"],
            "延迟最短的先上；没测过的排最后"
        );
    }

    /// stablest：Healthy > 未知 > Degraded，同档按失败次数细分。
    #[test]
    fn stablest_prefers_health_over_history() {
        let mut d = data();
        d.model_health.insert(
            "groq:llama-3".into(),
            crate::model::ModelHealth {
                status: crate::model::HealthStatus::Degraded,
                fails: 1,
                ..Default::default()
            },
        );
        d.model_health.insert(
            "groq:mixtral".into(),
            crate::model::ModelHealth {
                status: crate::model::HealthStatus::Healthy,
                fails: 2,
                oks: 9,
                ..Default::default()
            },
        );

        let mut s = Swrr::new();
        let v = s.order_with(
            Strategy::Stablest,
            vec![
                Candidate {
                    provider: 0,
                    account: "groq_a".into(),
                    model_id: "llama-3".into(),
                    half_open: false,
                    cap_unknown: false,
                },
                Candidate {
                    provider: 0,
                    account: "groq_a".into(),
                    model_id: "mixtral".into(),
                    half_open: false,
                    cap_unknown: false,
                },
                Candidate {
                    provider: 1,
                    account: "cerebras_a".into(),
                    model_id: "qwen-3".into(),
                    half_open: false,
                    cap_unknown: false,
                },
            ],
            &d,
            &Ledger::default(),
            now_ms(),
        );
        assert_eq!(
            v.iter().map(|c| c.model_id.as_str()).collect::<Vec<_>>(),
            ["mixtral", "qwen-3", "llama-3"],
            "健康的 > 没测过的 > 降级的"
        );
    }

    /// 三维权重生效：可靠性压满时慢而稳的先上，速度/智能压满时快而聪明的先上。
    ///
    /// 这条测试是整个「可调权重」的端到端闸门——权重没接进 order()、
    /// 或者被极值策略抢走，都会让它挂。
    #[test]
    fn custom_weights_flip_the_winner() {
        let mut d = data();
        // groq 健康但慢；cerebras 降级（慢机）但快、会推理、上下文大
        d.model_health.insert(
            "groq:llama-3".into(),
            crate::model::ModelHealth {
                status: HealthStatus::Healthy,
                latency_ms: Some(2_000),
                ..Default::default()
            },
        );
        d.model_health.insert(
            "cerebras:qwen-3".into(),
            crate::model::ModelHealth {
                status: HealthStatus::Degraded,
                latency_ms: Some(80),
                caps: crate::model::ModelCaps {
                    reasoning: Some(true),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        d.providers[1].models[0].context = Some(128_000);

        let mk = || -> Vec<Candidate> {
            vec![
                Candidate {
                    provider: 0,
                    account: "groq_a".into(),
                    model_id: "llama-3".into(),
                    half_open: false,
                    cap_unknown: false,
                },
                Candidate {
                    provider: 1,
                    account: "cerebras_a".into(),
                    model_id: "qwen-3".into(),
                    half_open: false,
                    cap_unknown: false,
                },
            ]
        };

        // 可靠性 70：稳的先上
        d.routing.weights = crate::model::RouteWeights {
            enabled: true,
            reliability: 70,
            speed: 15,
            intelligence: 15,
        };
        let mut s = Swrr::new();
        let v = s.order(mk(), &d, &Ledger::default(), now_ms());
        assert_eq!(v[0].model_id, "llama-3", "可靠性压满时该选健康的那个");

        // 速度 + 智能 90：快而聪明的先上，慢机标签不再是死刑
        d.routing.weights = crate::model::RouteWeights {
            enabled: true,
            reliability: 10,
            speed: 45,
            intelligence: 45,
        };
        let mut s = Swrr::new();
        let v = s.order(mk(), &d, &Ledger::default(), now_ms());
        assert_eq!(v[0].model_id, "qwen-3", "速度/智能压满时该选快的那个");
    }

    /// 未启用权重时，`weight_of_model` 必须与旧公式逐位相同——
    /// 老配置升级后排序不能有任何变化。
    #[test]
    fn disabled_weights_keep_the_legacy_formula() {
        let d = data();
        let p = &d.providers[0];
        let legacy = weight_of(p, "groq_a", &Ledger::default(), None, 0);
        let withnone = weight_of_model(p, "groq_a", "llama-3", &Ledger::default(), None, 0, None);
        assert_eq!(legacy, withnone);
        assert_eq!(d.routing.weights.active(), None, "默认必须是关的");
    }

    /// smartest：实测推理能力优先于目录声明；同档按上下文长度。
    #[test]
    fn smartest_measured_beats_declared() {
        let mut d = data();
        // 声明支持推理，但实测不支持 —— 实测赢，掉到后面
        d.providers[0].models[1].caps.reasoning = Some(true);
        d.providers[0].models[1].context = Some(32_768);
        d.model_health.insert(
            "groq:mixtral".into(),
            crate::model::ModelHealth {
                caps: crate::model::ModelCaps {
                    reasoning: Some(false),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        d.providers[0].models[0].context = Some(131_072);

        let mut s = Swrr::new();
        let v = s.order_with(
            Strategy::Smartest,
            vec![
                Candidate {
                    provider: 0,
                    account: "groq_a".into(),
                    model_id: "llama-3".into(),
                    half_open: false,
                    cap_unknown: false,
                },
                Candidate {
                    provider: 0,
                    account: "groq_a".into(),
                    model_id: "mixtral".into(),
                    half_open: false,
                    cap_unknown: false,
                },
                Candidate {
                    provider: 1,
                    account: "cerebras_a".into(),
                    model_id: "qwen-3".into(),
                    half_open: false,
                    cap_unknown: false,
                },
            ],
            &d,
            &Ledger::default(),
            now_ms(),
        );
        assert_eq!(
            v.iter().map(|c| c.model_id.as_str()).collect::<Vec<_>>(),
            ["llama-3", "mixtral", "qwen-3"],
            "实测推翻声明后按上下文排；全空的垫底"
        );
    }
}
