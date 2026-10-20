//! 候选链构建：Gate → Order → Pick。
//!
//! - **Gate**（`plan`）：模型健康、熔断冷却、额度余量，硬过滤。被拒的带原因，UI 要能解释。
//! - **Order**（[`Swrr::order`]）：平滑加权轮询，按「档位 × 余量 × 健康」的权重轮流。
//! - **Pick**（[`stick_to_front`]）：会话粘性，把上次答它的那家提到最前，保住 prompt cache。
//!
//! 顺序是刻意的：先保证「能发」，再决定「谁先发」，最后让「上次答过的那家」插队。
//! 粘性不能突破 Gate —— 冷却中的、额度见底的，粘也粘不住。

use crate::cooldown::{now_ms, BreakerState, Cooldowns};
use crate::model::{Account, AppData, HealthStatus, ModelCaps, Provider};
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

#[derive(Debug, Clone)]
pub struct Rejected {
    pub provider: String,
    /// 厂商级原因（没开 / 没 key）为空，模型级原因带上具体凭据
    pub account: Option<String>,
    pub model: String,
    pub reason: String,
    /// 归类用，让界面能按「额度 / 判死 / 冷却」分区，不必去解析 reason 文本。
    /// 取值：`quota` / `health` / `cooldown` / `disabled` / `nokey` /
    /// `tombstone` / `capability`
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
/// `provider` 为空表示虚拟项（目前只有 `auto`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasEntry {
    pub alias: String,
    pub provider: String,
    pub model: String,
}

impl AliasEntry {
    pub fn is_virtual(&self) -> bool {
        self.provider.is_empty()
    }
}

/// `provider:model` 的别名形式。
///
/// 别名里保留 `:`，因为脱壳后正好是 `plan` 能精确解析的显式点名语法；
/// 换成别的分隔符就得再维护一层反查表。
pub fn alias_of(provider: &str, model: &str) -> String {
    format!("{ALIAS_PREFIX}{provider}:{model}")
}

/// 对外可枚举的别名表：`auto` 打头，之后每个可用厂商的每个模型各一条。
///
/// 三条过滤规则，都是为了「列出来的必须能发出去」：
///
/// 1. 厂商要启用且至少有一条可用凭据 —— 没 key 的列出来，用户选了必然 `nokey`；
/// 2. 被 feed 下架的模型不列 —— Gate 会拒；
/// 3. **模型名去重** —— 同一个 `llama-3.3-70b` 在五家都有时只留第一条。
///    这是别名层真正的价值：把 `厂商 × 模型` 收敛成 `模型`，否则客户端选择器
///    会被同名条目淹掉。被收敛掉的厂商仍然可通过 `provider:model` 显式点到。
pub fn aliases(data: &AppData, has_key: &dyn Fn(&Account) -> bool) -> Vec<AliasEntry> {
    let mut out = vec![AliasEntry {
        alias: format!("{ALIAS_PREFIX}auto"),
        provider: String::new(),
        model: "auto".into(),
    }];
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for p in data.providers.iter().filter(|p| p.enabled) {
        if !p.live_accounts().any(has_key) {
            continue;
        }
        for m in &p.models {
            if data.feed.tombstoned(&p.id, &m.id) || !seen.insert(m.id.as_str()) {
                continue;
            }
            out.push(AliasEntry {
                alias: alias_of(&p.id, &m.id),
                provider: p.id.clone(),
                model: m.id.clone(),
            });
        }
    }
    out
}

/// 组装本轮候选链。
///
/// `model` 支持三种写法：`auto` / `provider:model` / 裸 `model-id`。
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
    plan_with(data, model, cooldowns, has_key, ledger, Needs::default())
}

/// 带能力要求的候选链。
///
/// 能力门禁放在 Gate 的第一位（tombstone 之后）：它和额度、冷却不同，
/// 是「这个请求和这个模型天生不匹配」，换时间、换 key 都不会变。
/// 放在后面会让一条带图请求先被冷却原因挡住，用户看不到真正的原因。
pub fn plan_with(
    data: &AppData,
    model: &str,
    cooldowns: &Cooldowns,
    has_key: &dyn Fn(&Account) -> bool,
    ledger: &Ledger,
    needs: Needs,
) -> Plan {
    let mut out: Vec<Candidate> = Vec::new();
    let mut rejected: Vec<Rejected> = Vec::new();

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

    let mut push = |idx: usize, account: &str, model_id: &str| {
        if model_id.is_empty() {
            return;
        }
        if out
            .iter()
            .any(|c| c.provider == idx && c.account == account && c.model_id == model_id)
        {
            return;
        }
        out.push(Candidate {
            provider: idx,
            account: account.to_string(),
            model_id: model_id.to_string(),
            half_open: false,
            cap_unknown: false,
        });
    };

    // ① 显式点名
    if let Some((pid, mid)) = model.split_once(':') {
        if let Some(idx) = usable.iter().copied().find(|&i| data.providers[i].id == pid) {
            for acc in &live[idx] {
                push(idx, acc, mid);
            }
        }
    } else if !model.is_empty() && model != "auto" && model != "freeport/auto" {
        // 裸模型名：按 provider 顺序取第一个声明了它的
        if let Some(idx) = usable
            .iter()
            .copied()
            .find(|&i| data.providers[i].models.iter().any(|m| m.id == model))
        {
            for acc in &live[idx] {
                push(idx, acc, model);
            }
        }
    }

    // ② 用户配置的链
    for entry in &data.routing.chain {
        if let Some((pid, mid)) = entry.split_once(':') {
            if let Some(idx) = usable.iter().copied().find(|&i| data.providers[i].id == pid) {
                for acc in &live[idx] {
                    push(idx, acc, mid);
                }
            }
        }
    }

    // ③ 兜底：tier A 优先，每个 provider 取前两个模型
    let mut by_tier = usable.clone();
    by_tier.sort_by(|&a, &b| data.providers[a].tier.cmp(&data.providers[b].tier));
    for idx in by_tier {
        for m in data.providers[idx].models.iter().take(2) {
            for acc in &live[idx] {
                push(idx, acc, &m.id);
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

/// 平滑加权轮询状态。跨请求保留，才谈得上「平滑」。
#[derive(Debug, Default, Clone)]
pub struct Swrr {
    current: std::collections::BTreeMap<String, f64>,
}

impl Swrr {
    pub fn new() -> Self {
        Self::default()
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
        if cands.len() < 2 {
            return cands;
        }

        let mut live: Vec<(Candidate, f64, String)> = Vec::with_capacity(cands.len());
        for c in cands {
            let p = &data.providers[c.provider];
            let health = data.model_health.get(&format!("{}:{}", p.id, c.model_id));
            let w = weight_of(p, &c.account, ledger, health, now)
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

/// 一条候选有多值得先用。三个因子相乘，每个都能在 UI 上解释清楚。
///
/// - **档位**：Tier A 官方免费 > B 匿名端点 > C 订阅/待验证。免费额度是我们唯一确定的事实，优先花。
/// - **余量**：这条**凭据**还剩多少比例。剩得多的先花，避免把一家的分钟额度打到底再换家。
/// - **健康**：实测过的优先。`Degraded`（慢）压到 0.55，但不为 0——慢总比没有强。
pub fn weight_of(
    p: &Provider,
    account: &str,
    ledger: &Ledger,
    health: Option<&crate::model::ModelHealth>,
    now: i64,
) -> f64 {
    let (tier, remaining, health_f) = weight_factors(p, account, ledger, health, now);
    (tier * remaining * health_f).max(0.01)
}

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
    let health_f = match health.map(|h| h.status) {
        Some(HealthStatus::Healthy) | None => 1.0,
        Some(HealthStatus::Unknown) | Some(HealthStatus::Probing) => 0.95,
        Some(HealthStatus::Degraded) => 0.55,
        Some(HealthStatus::Cooling) => 0.3,
        _ => 0.1,
    };
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
            quota: None,
            accounts: vec![Account {
                id: format!("{id}_a"),
                name: "default".into(),
                key_mask: "sk-••••1234".into(),
                enabled: true,
                added_at: String::new(),
                quota: None,
                health: None,
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
        // 别的模型照常接力
        assert!(!p.candidates.is_empty());
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
        // 第二家也有同名模型：只留先出现的那条，否则选择器会被同名条目淹掉
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
        assert_eq!(list[1].alias, "claude-freeport-groq:llama-3");
        assert_eq!(list[1].provider, "groq");
        // 别名脱壳后必须刚好是 plan 能解析的显式点名语法
        assert_eq!(resolve_alias(&list[1].alias), "groq:llama-3");
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
        );
        assert!(
            p.candidates.iter().all(|c| c.model_id != "llama-3"),
            "声明不支持图像的模型不能进链"
        );
        let r = p.rejected.iter().find(|r| r.kind == "capability").expect("要给出能力原因");
        assert!(r.reason.contains("vision"));
        assert!(!p.candidates.is_empty(), "同厂商其他模型仍可用");
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
        let p = plan_with(&d, "auto", &Cooldowns::default(), &|_| true, &Ledger::default(), needs);
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
}
