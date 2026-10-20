//! 错误 → 作用域 → 动作。
//!
//! 这张表是整个网关的根：判错作用域比退避策略重要一个数量级。
//! 一个平台 5xx 如果被当成「模型失败」，会在它的每个模型上各烧一跳。
//!
//! v1.2 起，冷却到期不再直接恢复，而是进入半开试探（见 `Cooldowns::probe_ready`）。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    /// 单个 key / 账号
    Key,
    /// 某个 provider 下的某个模型
    Model,
    /// 整个 provider（本项目一个 provider 目前只有一条凭据）
    Provider,
}

impl Scope {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Key => "key",
            Self::Model => "model",
            Self::Provider => "provider",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorClass {
    pub scope: Scope,
    pub cooldown_ms: u64,
    pub kind: String,
    /// 能力缺口，例如 "tools"；由 M2 的探针与门禁消费
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capability: Option<String>,
    /// 从响应体里读到的额度事实（只有 429 的配额拒绝会带）。
    /// 不写进配置、不持久化，交给账本在本次运行期内用。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub learned: Option<QuotaSignal>,
}

/// 429 到底在说什么。三者混为一谈，代价是两个方向都贵：
///
/// - **没有免费档**（`limit: 0`）：重试永远无用，该把模型摘掉；
/// - **今日额度用完**：明天会好，该歇到重置时刻，而不是 90 秒后；
/// - **分钟级限流**：几秒到几分钟就好，歇久纯属浪费。
///
/// 厂商把这三个挤在同一个状态码里，只能靠 body 区分。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct QuotaSignal {
    /// 该模型根本不提供免费档
    pub no_free_tier: bool,
    /// 耗尽发生在哪个窗口：`day` / `minute`
    pub window: Option<String>,
    /// 厂商自己说的日请求上限（能回填账本，不用手工猜）
    pub daily_limit: Option<u32>,
    pub minute_limit: Option<u32>,
    /// 上游给的恢复等待（相对毫秒）
    pub reset_in_ms: Option<u64>,
}

impl QuotaSignal {
    /// 有可用的额度事实才值得往下传
    fn is_worth_learning(&self) -> bool {
        self.no_free_tier || self.daily_limit.is_some() || self.minute_limit.is_some()
    }
}

impl ErrorClass {
    fn new(scope: Scope, cooldown_ms: u64, kind: &str) -> Self {
        Self {
            scope,
            cooldown_ms,
            kind: kind.to_string(),
            capability: None,
            learned: None,
        }
    }

    fn with_learned(mut self, sig: QuotaSignal) -> Self {
        if sig.is_worth_learning() {
            self.learned = Some(sig);
        }
        self
    }

    fn with_capability(mut self, cap: &str) -> Self {
        self.capability = Some(cap.to_string());
        self
    }

    /// 只换下一跳、不写冷却的失败：模型慢、空响应、半截流。
    /// 这类失败不足以判死任何目标，但也不该被记成成功。
    pub fn soft(kind: &str) -> Self {
        Self::new(Scope::Model, 0, kind)
    }
}

const MIN: u64 = 60_000;
const HOUR: u64 = 3_600_000;
const DAY: u64 = 24 * HOUR;

/// 响应体文本规则。顺序即优先级，先匹配先返回。
///
/// 免费厂商最常见的失败形态是「状态码正常 + 文案说没钱了」，
/// 所以文本必须参与判定，不能只看状态码。
const BALANCE_HINTS: &[&str] = &[
    "insufficient balance",
    "insufficient_quota",
    "insufficient credit",
    "no credit",
    "credit balance",
    "payment required",
    "余额不足",
    "账户余额",
    "欠费",
    "请充值",
];

const PLAN_HINTS: &[&str] = &[
    "not included in your plan",
    "not available in your plan",
    "not available on your",
    "plan does not include",
    "套餐不包含",
    "不在套餐",
    "需要升级",
];

const FILTER_HINTS: &[&str] = &[
    "content filter",
    "content_filter",
    "content policy",
    "safety system",
    "was flagged",
    "敏感",
    "风控",
    "违规内容",
];

const RATE_HINTS: &[&str] = &[
    "rate limit",
    "rate_limit",
    "too many requests",
    "requests per minute",
    "requests per day",
    "频率限制",
    "请求过于频繁",
    "请稍后重试",
];

const QUOTA_HINTS: &[&str] = &[
    "exceeded your current quota",
    "quota exceeded",
    "exceeded your quota",
    "free tier",
    "monthly limit",
    "额度已用完",
    "额度不足",
    "配额已用完",
    "限额已用完",
    "今日额度",
];

fn hit_any(t: &str, hints: &[&str]) -> bool {
    hints.iter().any(|h| t.contains(h))
}

/// 收集 `needle` 之后所有出现的第一个数字。
///
/// Gemini 的 429 会在 message 里为每个超限指标写一遍 `limit: N`，
/// 只看第一处会漏掉「日额度有值、分钟额度为 0」这种混合情形。
fn numbers_after(s: &str, needle: &str) -> Vec<f64> {
    let mut out = Vec::new();
    let mut from = 0usize;
    while let Some(rel) = s[from..].find(needle) {
        let start = from + rel + needle.len();
        if start >= s.len() {
            break;
        }
        if let Some(n) = first_number(&s[start..]) {
            out.push(n);
        }
        from = start;
    }
    out
}

fn num_of(v: &serde_json::Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse::<f64>().ok()))
}

/// 在任意深度找同时带 `limit` 与 `remaining` 的对象。
fn limit_remaining(v: &serde_json::Value) -> Option<(f64, f64)> {
    match v {
        serde_json::Value::Object(m) => {
            if let (Some(l), Some(r)) = (m.get("limit").and_then(num_of), m.get("remaining").and_then(num_of)) {
                return Some((l, r));
            }
            m.values().find_map(limit_remaining)
        }
        serde_json::Value::Array(a) => a.iter().find_map(limit_remaining),
        _ => None,
    }
}

/// 429 的语义判定：把「没免费档 / 日额度耗尽 / 分钟限流」分开。
///
/// 返回 `None` 表示 body 里没有可用的额度事实，按普通限流处理。
pub fn parse_quota_signal(body: &str) -> Option<QuotaSignal> {
    if body.trim().is_empty() {
        return None;
    }
    let t = body.to_ascii_lowercase();
    let mut sig = QuotaSignal::default();
    let mut found = false;

    // ① Gemini / google.rpc.Status：数字在 message，窗口在 quotaId
    if t.contains("quota exceeded") || t.contains("quotafailure") || t.contains("resource_exhausted") {
        let limits = numbers_after(&t, "limit:");
        if !limits.is_empty() {
            found = true;
            if limits.iter().all(|n| *n <= 0.0) {
                // 每一条免费档都是 0：这个模型压根不提供免费额度，
                // 而不是「今天用完了」——重试永远无用。
                sig.no_free_tier = true;
            } else {
                let day = t.contains("perday") || t.contains("per_day") || t.contains("per day");
                let minute = t.contains("perminute") || t.contains("per minute");
                let max = limits.iter().copied().filter(|n| *n > 0.0).fold(0.0_f64, f64::max);
                let capped = (1.0..1e9).contains(&max);
                if day {
                    sig.window = Some("day".into());
                    if capped {
                        sig.daily_limit = Some(max.round() as u32);
                    }
                } else if minute {
                    sig.window = Some("minute".into());
                    if capped {
                        sig.minute_limit = Some(max.round() as u32);
                    }
                } else {
                    // 有额度却说不清窗口：按日算。歇久一点只是少发几次，
                    // 歇短了会在一个已知无望的目标上反复烧请求。
                    sig.window = Some("day".into());
                    if capped {
                        sig.daily_limit = Some(max.round() as u32);
                    }
                }
            }
        }
    }

    // ② 结构化 `limit` / `remaining`
    if !found {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
            if let Some((limit, remaining)) = limit_remaining(&v) {
                if limit > 0.0 && remaining <= 0.0 {
                    found = true;
                    let day = t.contains("\"day\"")
                        || t.contains("daily")
                        || t.contains("per day")
                        || t.contains("perday");
                    sig.window = Some(if day { "day".into() } else { "minute".into() });
                    if day && limit < 1e9 {
                        sig.daily_limit = Some(limit.round() as u32);
                    }
                }
            }
        }
    }

    // ③ 只有文案。注意 "free tier exhausted" 不算 —— 那是今天用完，不是没有。
    if !found
        && (t.contains("no free tier")
            || t.contains("not available on the free")
            || t.contains("free tier is not available")
            || t.contains("no free quota"))
    {
        sig.no_free_tier = true;
        found = true;
    }

    if !found {
        return None;
    }
    let ms = parse_retry_after(None, body);
    if ms > 0 {
        sig.reset_in_ms = Some(ms);
    }
    Some(sig)
}

/// 文本规则。命中返回分类，未命中返回 `None` 交给状态码处理。
fn text_rule(t: &str, retry_after_ms: u64) -> Option<ErrorClass> {
    if hit_any(t, BALANCE_HINTS) {
        // 30 分钟：用户可能刚充值，但也不该一分钟试一次
        return Some(ErrorClass::new(Scope::Key, 30 * MIN, "no_balance"));
    }
    if hit_any(t, PLAN_HINTS) {
        return Some(ErrorClass::new(Scope::Model, HOUR, "not_in_plan"));
    }
    if hit_any(t, FILTER_HINTS) {
        // 安全拒答不是故障：谁都不停，只换下一跳
        return Some(ErrorClass::new(Scope::Model, 0, "content_filter"));
    }
    if hit_any(t, RATE_HINTS) {
        return Some(ErrorClass::new(Scope::Key, retry_after_ms.max(MIN), "rate_limit"));
    }
    if hit_any(t, QUOTA_HINTS) {
        // 不知道窗口何时重置，先歇 15 分钟；巡检成功可提前解除
        return Some(ErrorClass::new(Scope::Key, 15 * MIN, "quota_exhausted"));
    }
    None
}

/// 把上游的 HTTP 状态与响应体映射成作用域与冷却时长。
pub fn classify(status: u16, body: &str, retry_after_ms: u64) -> ErrorClass {
    let t = body.to_ascii_lowercase();

    // 连接层失败：没有状态码。判平台级，冷却短——它常常是本地网络抖动。
    if status == 0 {
        let kind = if t.contains("timeout") || t.contains("timed out") {
            "timeout"
        } else {
            "network"
        };
        return ErrorClass::new(Scope::Provider, 15_000, kind);
    }

    // 402/403/400 里藏着的余额与配额，必须靠文本区分
    if matches!(status, 400 | 402 | 403) {
        if let Some(c) = text_rule(&t, retry_after_ms) {
            return c;
        }
    }

    match status {
        401 => ErrorClass::new(Scope::Key, 5 * MIN, "invalid_key"),
        402 => ErrorClass::new(Scope::Key, 30 * MIN, "no_balance"),
        403 => ErrorClass::new(Scope::Model, HOUR, "forbidden"),
        429 => {
            // 429 是三种完全不同的失败被挤进同一个状态码，先拆开再决定歇多久。
            let sig = parse_quota_signal(body);
            match sig {
                Some(s) if s.no_free_tier => {
                    // 摘的是「这个模型在此账号上没有免费档」，不是整家厂商
                    ErrorClass::new(Scope::Model, DAY, "no_free_tier").with_learned(s)
                }
                Some(s) if s.window.as_deref() == Some("day") => {
                    // 日额度：歇到上游说的重置时刻；没说就 30 分钟。
                    // 下限 5 分钟，避免上游的 retryDelay 把日额度当分钟限流。
                    let cd = s.reset_in_ms.unwrap_or(30 * MIN).max(5 * MIN);
                    ErrorClass::new(Scope::Key, cd, "quota_exhausted").with_learned(s)
                }
                Some(s) => {
                    // 分钟级限流：按 Retry-After，下限 90 秒
                    let cd = s.reset_in_ms.unwrap_or(retry_after_ms).max(90_000);
                    ErrorClass::new(Scope::Key, cd, "rate_limit").with_learned(s)
                }
                None => ErrorClass::new(Scope::Key, retry_after_ms.max(90_000), "rate_limit"),
            }
        }
        404 | 410 => ErrorClass::new(Scope::Model, DAY, "not_found"),
        413 => ErrorClass::new(Scope::Model, 0, "context"),
        400 => {
            if t.contains("context")
                || t.contains("too long")
                || t.contains("maximum")
                || t.contains("reduce")
            {
                ErrorClass::new(Scope::Model, 0, "context")
            } else if t.contains("tool") || t.contains("function") {
                ErrorClass::new(Scope::Model, 0, "no_tools").with_capability("tools")
            } else if t.contains("image")
                || t.contains("vision")
                || t.contains("multimodal")
                || t.contains("image_url")
            {
                // 「不支持图片」不是故障，是这个模型干不了这活。记下能力缺口，
                // 下一单带图请求就不用再烧一次。
                ErrorClass::new(Scope::Model, 0, "no_vision").with_capability("vision")
            } else if t.contains("response_format")
                || t.contains("json_object")
                || t.contains("json mode")
                || t.contains("json schema")
            {
                ErrorClass::new(Scope::Model, 0, "no_json").with_capability("json")
            } else {
                ErrorClass::new(Scope::Model, 0, "bad_request")
            }
        }
        408 | 504 => ErrorClass::new(Scope::Provider, 60_000, "timeout"),
        500..=599 => ErrorClass::new(Scope::Provider, 90_000, "upstream"),
        _ => ErrorClass::new(Scope::Model, 0, "unknown"),
    }
}

/// 响应体里藏的错误。
///
/// 覆盖三种形态：结构化的 `{"error":{...}}`、SSE 里夹的 error 事件、
/// 以及短响应里直白的余额/额度文案。正常长回答里出现 "error" 这个词不会被误判。
pub fn body_error(body: &str) -> Option<ErrorClass> {
    if body.trim().is_empty() {
        return Some(ErrorClass::new(Scope::Model, 0, "empty_body"));
    }
    let t = body.to_ascii_lowercase();

    let structured = t.contains("\"error\":{")
        || t.contains("\"error\": {")
        || t.contains("\"error\":\"")
        || t.contains("\"error\": \"")
        || t.contains("\"type\":\"error\"")
        || t.contains("event: error");
    if structured {
        if let Some(c) = text_rule(&t, 0) {
            return Some(c);
        }
        // 有 error 结构但没命中文案：这是上游的临时错误，本请求内换下一跳
        return Some(ErrorClass::new(Scope::Model, 0, "upstream_body"));
    }

    // 短响应才做纯文本判定，避免把正常回答里的词当真
    if body.len() < 2000 {
        if let Some(c) = text_rule(&t, 0) {
            if matches!(c.kind.as_str(), "no_balance" | "quota_exhausted") {
                return Some(c);
            }
        }
    }
    None
}

/// 是否是一次「真空响应」：200 但没有任何可用内容。
/// 免费厂商的典型形态，必须当成失败处理，否则账本会记成健康。
pub fn is_empty_completion(body: &str) -> bool {
    let t = body.trim();
    if t.is_empty() {
        return true;
    }
    if t == "[DONE]" || t == "data: [DONE]" {
        return true;
    }
    false
}

/// 由上游 `Retry-After` 头或 429 文案推出等待时间。
pub fn parse_retry_after(headers: Option<&str>, body: &str) -> u64 {
    if let Some(raw) = headers {
        let raw = raw.trim();
        if let Ok(sec) = raw.parse::<f64>() {
            return (sec.max(0.0) * 1000.0).round() as u64;
        }
    }
    // Gemini 的 retryDelay: "7s"；部分厂商写在 body 里
    let lower = body.to_ascii_lowercase();
    for needle in ["retry_after", "retrydelay", "retry in", "try again in"] {
        if let Some(pos) = lower.find(needle) {
            // 只看 needle 之后的一小段，避免抓到无关数字（如状态码 429）
            let tail: String = lower[pos + needle.len()..].chars().take(48).collect();
            if let Some(num) = first_number(&tail) {
                // 带 ms 的写法按毫秒算
                if tail.contains("ms") && num > 100.0 {
                    return num.round() as u64;
                }
                return (num * 1000.0).round() as u64;
            }
        }
    }
    0
}

fn first_number(s: &str) -> Option<f64> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                i += 1;
            }
            if let Ok(v) = s[start..i].parse::<f64>() {
                return Some(v);
            }
        } else {
            i += 1;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_error_cools_only_the_key() {
        let c = classify(401, "", 0);
        assert_eq!(c.scope, Scope::Key);
        assert_eq!(c.cooldown_ms, 300_000);
    }

    #[test]
    fn server_error_cools_the_whole_provider() {
        let c = classify(503, "upstream down", 0);
        assert_eq!(c.scope, Scope::Provider);
    }

    #[test]
    fn tool_rejection_is_scoped_to_the_model() {
        let c = classify(400, "this model does not support tools", 0);
        assert_eq!(c.scope, Scope::Model);
        assert_eq!(c.kind, "no_tools");
        assert_eq!(c.capability.as_deref(), Some("tools"));
    }

    #[test]
    fn image_rejection_is_a_capability_gap_not_a_fault() {
        let c = classify(400, r#"{"error":{"message":"Invalid content type image_url for this model"}}"#, 0);
        assert_eq!(c.kind, "no_vision");
        assert_eq!(c.capability.as_deref(), Some("vision"));
        assert_eq!(c.cooldown_ms, 0, "模型没坏，只是不会看图，不该冷却");
    }

    #[test]
    fn json_mode_rejection_is_caught() {
        let c = classify(400, "response_format json_object is not supported", 0);
        assert_eq!(c.kind, "no_json");
        assert_eq!(c.capability.as_deref(), Some("json"));
    }

    #[test]
    fn rate_limit_honours_retry_after() {
        let c = classify(429, "", 12_000);
        assert_eq!(c.cooldown_ms, 90_000); // 低于下限时用 90s
        let c = classify(429, "", 300_000);
        assert_eq!(c.cooldown_ms, 300_000);
    }

    #[test]
    fn parses_gemini_retry_delay() {
        assert_eq!(parse_retry_after(None, r#"{"retryDelay":"7s"}"#), 7000);
    }

    #[test]
    fn chinese_quota_text_wins_over_status_code() {
        let c = classify(403, "该账号本月额度已用完，请下月再试", 0);
        assert_eq!(c.scope, Scope::Key);
        assert_eq!(c.kind, "quota_exhausted");
        assert_eq!(c.cooldown_ms, 15 * MIN);
    }

    #[test]
    fn balance_short_cools_key_for_30_minutes() {
        let c = classify(402, r#"{"error":{"message":"Insufficient balance"}}"#, 0);
        assert_eq!(c.kind, "no_balance");
        assert_eq!(c.cooldown_ms, 30 * MIN);
    }

    #[test]
    fn safety_refusal_never_rests() {
        let c = classify(400, "blocked by content filter", 0);
        assert_eq!(c.kind, "content_filter");
        assert_eq!(c.cooldown_ms, 0, "拒答不是故障，谁都不该被冷却");
    }

    #[test]
    fn connection_failure_cools_provider_not_model() {
        let c = classify(0, "operation timed out", 0);
        assert_eq!(c.scope, Scope::Provider);
        assert_eq!(c.kind, "timeout");
    }

    #[test]
    fn body_error_detects_structured_error() {
        let c = body_error(r#"data: {"error":{"message":"free tier exhausted","code":"quota"}}"#)
            .expect("should be an error");
        assert_eq!(c.kind, "quota_exhausted");
    }

    #[test]
    fn body_error_ignores_error_word_in_normal_answer() {
        let answer = format!(
            r#"{{"choices":[{{"message":{{"content":"{}"}}}}]}}"#,
            "The compiler reports an error here. ".repeat(100)
        );
        assert!(body_error(&answer).is_none());
    }

    #[test]
    fn body_error_flags_empty_body() {
        assert_eq!(body_error("   ").unwrap().kind, "empty_body");
        assert!(body_error("[DONE]").is_none());
    }

    const GEMINI_ZERO: &str = r#"{"error":{"code":429,"message":"Quota exceeded for metric: generativelanguage.googleapis.com/generate_content_free_tier_requests, limit: 0, model: gemini-2.0-flash","status":"RESOURCE_EXHAUSTED","details":[{"@type":"type.googleapis.com/google.rpc.QuotaFailure","violations":[{"quotaMetric":"generativelanguage.googleapis.com/generate_content_free_tier_requests","quotaId":"GenerateRequestsPerDayPerProjectPerModel-FreeTier"}]},{"@type":"type.googleapis.com/google.rpc.RetryInfo","retryDelay":"23s"}]}}"#;

    const GEMINI_DAILY: &str = r#"{"error":{"code":429,"message":"Quota exceeded for metric: generativelanguage.googleapis.com/generate_content_free_tier_requests, limit: 250, model: gemini-2.0-flash","status":"RESOURCE_EXHAUSTED","details":[{"@type":"type.googleapis.com/google.rpc.QuotaFailure","violations":[{"quotaMetric":"generativelanguage.googleapis.com/generate_content_free_tier_requests","quotaId":"GenerateRequestsPerDayPerProjectPerModel-FreeTier"}]}]}}"#;

    #[test]
    fn zero_limit_means_no_free_tier_and_is_retired_for_a_day() {
        let c = classify(429, GEMINI_ZERO, 0);
        assert_eq!(c.kind, "no_free_tier");
        assert_eq!(c.scope, Scope::Model, "摘的是模型，不是整家");
        assert_eq!(c.cooldown_ms, DAY);
        assert!(c.learned.as_ref().unwrap().no_free_tier);
    }

    #[test]
    fn daily_quota_is_learned_and_waits_for_reset() {
        let c = classify(429, GEMINI_DAILY, 0);
        assert_eq!(c.kind, "quota_exhausted");
        assert_eq!(c.scope, Scope::Key);
        let l = c.learned.as_ref().unwrap();
        assert_eq!(l.window.as_deref(), Some("day"));
        assert_eq!(l.daily_limit, Some(250), "厂商自己说的上限，直接回填账本");
        assert_eq!(c.cooldown_ms, 30 * MIN, "没给重置时刻就歇 30 分钟，而不是 90 秒");
    }

    #[test]
    fn structured_limit_reports_a_learned_cap() {
        let body = r#"{"error":{"message":"Rate limit reached for requests","metadata":{"limit":20,"remaining":0,"window":"day"}}}"#;
        let sig = parse_quota_signal(body).expect("should parse");
        assert_eq!(sig.window.as_deref(), Some("day"));
        assert_eq!(sig.daily_limit, Some(20));
        assert!(!sig.no_free_tier);
    }

    #[test]
    fn plain_429_stays_a_short_rate_limit() {
        let c = classify(429, r#"{"error":{"message":"Too many requests"}}"#, 5_000);
        assert_eq!(c.kind, "rate_limit");
        assert_eq!(c.cooldown_ms, 90_000);
        assert!(c.learned.is_none(), "没有额度事实就不该往下传");
    }

    #[test]
    fn exhausted_free_tier_is_not_mistaken_for_absent_one() {
        // 「今天用完了」和「没有免费档」是两件事，混了会白白摘掉一个月才好一次的模型
        let sig = parse_quota_signal(r#"{"error":{"message":"free tier exhausted for today"}}"#);
        assert!(sig.is_none());
        assert_ne!(classify(429, "free tier exhausted", 0).kind, "no_free_tier");
    }
}
