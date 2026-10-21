//! 官方额度查询。
//!
//! 账本里的「剩余多少」是从 429 响应体反推的**推测值**，而订阅号的额度在厂商
//! 那侧才说了算。两家都有公开的 usage 端点，问一次就能拿到权威数字：
//!
//! | | Claude | Codex |
//! |---|---|---|
//! | 端点 | `api.anthropic.com/api/oauth/usage` | `chatgpt.com/backend-api/wham/usage` |
//! | 要的认证 | `Bearer` + `anthropic-beta: oauth-2025-04-20` | `Bearer` + `ChatGPT-Account-Id` |
//! | 窗口来源 | 顶层 `five_hour` / `seven_day*` + `limits[]` | `rate_limit.*_window` 的 `limit_window_seconds` |
//!
//! 三条实现约束：
//!
//! 1. **只读**。这两个端点不消耗额度，也改不了任何东西。
//! 2. **失败要说清是哪一种**：凭据过期（401/403）和「上游抽风」对用户是两件事，
//!    前者要去重新登录，后者等一会儿再来。
//! 3. **解析失败不算查询失败**：形状变了就是拿不到数字，但 HTTP 是通的，
//!    报「解析不了」比报「网络错误」诚实。

use serde_json::Value;

use crate::tokens::Endpoints;
use freeport_core::dto::{GwUsage, GwUsageWindow};

/// Claude 的顶层窗口名。`limits[]` 里模型专属的周限额会覆盖同名的这一项。
const CLAUDE_TIERS: [&str; 5] = [
    "five_hour",
    "seven_day",
    "seven_day_opus",
    "seven_day_sonnet",
    "seven_day_fable",
];

/// `used_pct` 统一成 0..100 的百分比。两个端点给的都是百分数，
/// 但历史上出现过 0..1 的形态，所以小于 1.5 的一律当比例放大——
/// 一个已用 0.8% 的窗口被画成 80% 比画成 0.8% 糟得多。
fn pct(v: f64) -> f64 {
    if !v.is_finite() || v < 0.0 {
        return 0.0;
    }
    let p = if v > 0.0 && v < 1.5 { v * 100.0 } else { v };
    p.min(100.0)
}

fn iso(v: Option<&Value>) -> Option<String> {
    let v = v?;
    if let Some(s) = v.as_str() {
        return (!s.trim().is_empty()).then(|| s.to_string());
    }
    // 有的窗口给的是 unix 秒
    let n = v.as_i64()?;
    chrono::DateTime::from_timestamp(n, 0).map(|t| t.to_rfc3339())
}

/// 窗口秒数 → 人类认得的名字。和 Claude 那侧用同一套命名，
/// 界面上「5 小时 / 7 天」才不会一家一个说法。
fn window_name(secs: i64) -> String {
    match secs {
        18_000 => "five_hour".into(),
        604_800 => "seven_day".into(),
        2_592_000 => "thirty_day".into(),
        s if s >= 86_400 => format!("{}_day", s / 86_400),
        s => format!("{}_hour", s / 3_600),
    }
}

/// Claude 的 usage 响应 → 窗口列表。
///
/// 顶层窗口是旧形状，`limits[]` 是新形状（模型专属周限额）。两者可能同时存在，
/// 新形状优先——它更精确，而旧的 `seven_day` 是聚合值。
pub fn parse_claude(v: &Value) -> Vec<GwUsageWindow> {
    let mut out: Vec<GwUsageWindow> = Vec::new();

    for name in CLAUDE_TIERS {
        let Some(w) = v.get(name) else { continue };
        let Some(used) = w.get("utilization").and_then(|x| x.as_f64()) else {
            continue;
        };
        out.push(GwUsageWindow {
            name: name.to_string(),
            used_pct: pct(used),
            resets_at: iso(w.get("resets_at")),
        });
    }

    if let Some(limits) = v.get("limits").and_then(|l| l.as_array()) {
        for item in limits {
            // 带 surface 的是「某类客户端」的限额，不是账号额度，混进来会看不懂
            if item
                .pointer("/scope/surface")
                .is_some_and(|x| !x.is_null())
            {
                continue;
            }
            let Some(model) = item
                .pointer("/scope/model/display_name")
                .and_then(|x| x.as_str())
            else {
                continue;
            };
            let name = match model.trim().to_ascii_lowercase().as_str() {
                "opus" => "seven_day_opus",
                "sonnet" => "seven_day_sonnet",
                "fable" => "seven_day_fable",
                _ => continue,
            };
            let Some(used) = item.get("percent").and_then(|x| x.as_f64()) else {
                continue;
            };
            let w = GwUsageWindow {
                name: name.to_string(),
                used_pct: pct(used),
                resets_at: iso(item.get("resets_at")),
            };
            match out.iter_mut().find(|x| x.name == name) {
                Some(slot) => *slot = w,
                None => out.push(w),
            }
        }
    }

    out
}

/// Codex 的 usage 响应 → (窗口, 余额)。
///
/// `balance` 实测是**字符串**（`"62500"`），也收数字；`unlimited` 或
/// `has_credits=false` 时余额无意义，返回 `None` 而不是 0——0 会被读成「没钱了」。
pub fn parse_codex(v: &Value) -> (Vec<GwUsageWindow>, Option<f64>) {
    let mut out = Vec::new();
    if let Some(rl) = v.get("rate_limit") {
        for key in ["primary_window", "secondary_window"] {
            let Some(w) = rl.get(key) else { continue };
            let Some(used) = w.get("used_percent").and_then(|x| x.as_f64()) else {
                continue;
            };
            let name = w
                .get("limit_window_seconds")
                .and_then(|x| x.as_i64())
                .map(window_name)
                .unwrap_or_else(|| key.trim_end_matches("_window").to_string());
            out.push(GwUsageWindow {
                name,
                used_pct: pct(used),
                resets_at: iso(w.get("reset_at")),
            });
        }
    }

    let credits = v.get("credits").and_then(|c| {
        let has = c.get("has_credits").and_then(|x| x.as_bool()) == Some(true);
        let unlimited = c.get("unlimited").and_then(|x| x.as_bool()) == Some(true);
        if !has || unlimited {
            return None;
        }
        let bal = match c.get("balance")? {
            Value::Number(n) => n.as_f64()?,
            Value::String(s) => s.trim().parse::<f64>().ok()?,
            _ => return None,
        };
        (bal.is_finite() && bal > 0.0).then_some(bal)
    });

    (out, credits)
}

/// 查一条 OAuth 凭据的官方额度。
///
/// 返回的 `GwUsage` 永远不 panic：出错时 `ok=false` + `note` 说原因，
/// 界面照常渲染一行字，不用为「查额度失败」单独做一套错误界面。
pub async fn fetch(
    client: &reqwest::Client,
    ep: &Endpoints,
    kind: &str,
    access_token: &str,
    account_id: Option<&str>,
) -> GwUsage {
    let at = chrono::Utc::now().to_rfc3339();
    let mut out = GwUsage {
        kind: kind.to_string(),
        ok: false,
        windows: Vec::new(),
        credits: None,
        note: None,
        checked_at: at,
    };

    let req = match kind {
        "claude" => client
            .get(&ep.claude_usage_url)
            .header("authorization", format!("Bearer {access_token}"))
            // 少了这个 beta 头，OAuth token 会被当成普通 key 拒掉
            .header("anthropic-beta", "oauth-2025-04-20"),
        "codex" => {
            let mut r = client
                .get(&ep.codex_usage_url)
                .header("authorization", format!("Bearer {access_token}"))
                .header("user-agent", "codex-cli");
            if let Some(id) = account_id.filter(|s| !s.is_empty()) {
                r = r.header("chatgpt-account-id", id);
            }
            r
        }
        // 普通 API key 没有「订阅额度」这个概念，别让界面以为查错了
        other => {
            out.note = Some(format!("{other} 没有可查询的订阅额度，只有 OAuth 登录的账号才有"));
            return out;
        }
    };

    let res = match req
        .header("accept", "application/json")
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            out.note = Some(format!("连不上上游：{e}"));
            return out;
        }
    };

    let status = res.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        // 这一种要用户动手：token 被吊销或过期，重新登录才行
        out.note = Some(format!("授权已失效（HTTP {status}），请重新登录这个账号"));
        return out;
    }
    if !status.is_success() {
        let body = res.text().await.unwrap_or_default();
        out.note = Some(format!(
            "上游返回 HTTP {status}：{}",
            body.chars().take(200).collect::<String>()
        ));
        return out;
    }

    // 先拿完整 body 再解析：读体失败是瞬时问题，解析失败是形状变了，两件事
    let raw = match res.bytes().await {
        Ok(b) => b,
        Err(e) => {
            out.note = Some(format!("读取响应失败：{e}"));
            return out;
        }
    };
    let v: Value = match serde_json::from_slice(&raw) {
        Ok(v) => v,
        Err(e) => {
            out.note = Some(format!("上游返回的不是 JSON（{e}），接口可能改版了"));
            return out;
        }
    };

    match kind {
        "claude" => out.windows = parse_claude(&v),
        _ => {
            let (w, c) = parse_codex(&v);
            out.windows = w;
            out.credits = c;
        }
    }
    out.ok = true;
    if out.windows.is_empty() {
        out.note = Some("上游没有返回任何额度窗口，可能这个方案没有订阅额度".into());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn claude_reads_top_level_windows() {
        let v = json!({
            "five_hour": { "utilization": 12.0, "resets_at": "2026-09-09T15:00:00Z" },
            "seven_day": { "utilization": 25.0, "resets_at": null },
            "seven_day_opus": { "utilization": 8.0 },
            // 不认识的名字要忽略，不能当窗口塞进去
            "other_window": { "utilization": 4.0 }
        });
        let w = parse_claude(&v);
        let names: Vec<&str> = w.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(names, vec!["five_hour", "seven_day", "seven_day_opus"]);
        assert!((w[0].used_pct - 12.0).abs() < 1e-9);
        assert_eq!(w[0].resets_at.as_deref(), Some("2026-09-09T15:00:00Z"));
        assert!(w[1].resets_at.is_none());
    }

    #[test]
    fn claude_limits_override_the_aggregate_window() {
        // 新形状：顶层给聚合值，limits[] 给模型专属值，后者更准
        let v = json!({
            "seven_day_opus": { "utilization": 30.0 },
            "limits": [
                { "percent": 55.0, "resets_at": "2026-10-21T00:00:00Z",
                  "scope": { "model": { "display_name": "Opus" } } },
                // 带 surface 的是客户端限额，不该混进账号额度
                { "percent": 90.0, "scope": { "surface": "cli",
                  "model": { "display_name": "Sonnet" } } },
                // 不认识的模型名忽略
                { "percent": 10.0, "scope": { "model": { "display_name": "Haiku" } } }
            ]
        });
        let w = parse_claude(&v);
        assert_eq!(w.len(), 1, "只应留下 opus 这一条：{w:?}");
        assert_eq!(w[0].name, "seven_day_opus");
        assert!((w[0].used_pct - 55.0).abs() < 1e-9);
        assert_eq!(w[0].resets_at.as_deref(), Some("2026-10-21T00:00:00Z"));
    }

    #[test]
    fn codex_maps_window_seconds_to_names() {
        let v = json!({
            "rate_limit": {
                "primary_window": {
                    "used_percent": 42.0,
                    "limit_window_seconds": 18000,
                    "reset_at": 1_800_000_000i64
                },
                "secondary_window": {
                    "used_percent": 7.0,
                    "limit_window_seconds": 604800
                }
            },
            "credits": { "has_credits": true, "unlimited": false, "balance": "62500" }
        });
        let (w, credits) = parse_codex(&v);
        assert_eq!(w[0].name, "five_hour");
        assert_eq!(w[1].name, "seven_day");
        assert!((w[0].used_pct - 42.0).abs() < 1e-9);
        assert!(w[0].resets_at.is_some(), "unix 秒要转成 RFC3339");
        // 字符串余额也要认
        assert_eq!(credits, Some(62500.0));
    }

    #[test]
    fn codex_unlimited_or_missing_credits_is_none_not_zero() {
        // 0 会被界面读成「余额用光」，None 才是「没有这个概念」
        let (_, c) = parse_codex(&json!({
            "credits": { "has_credits": true, "unlimited": true, "balance": "0" }
        }));
        assert_eq!(c, None);
        let (_, c) = parse_codex(&json!({ "credits": { "has_credits": false } }));
        assert_eq!(c, None);
        let (_, c) = parse_codex(&json!({}));
        assert_eq!(c, None);
    }

    #[test]
    fn percentages_are_normalised_and_clamped() {
        assert!((pct(12.0) - 12.0).abs() < 1e-9);
        // 0..1 的比例形态：0.8 意思是 80%，不是 0.8%
        assert!((pct(0.8) - 80.0).abs() < 1e-9);
        assert!((pct(0.0) - 0.0).abs() < 1e-9);
        assert!((pct(-5.0) - 0.0).abs() < 1e-9);
        assert!((pct(140.0) - 100.0).abs() < 1e-9);
        assert!((pct(f64::NAN) - 0.0).abs() < 1e-9);
    }

    #[test]
    fn unknown_window_seconds_still_get_a_readable_name() {
        assert_eq!(window_name(3 * 86_400), "3_day");
        assert_eq!(window_name(8 * 3_600), "8_hour");
        assert_eq!(window_name(2_592_000), "thirty_day");
    }
}
