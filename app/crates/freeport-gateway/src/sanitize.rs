//! 出站清洗：把「不该出门的东西」留在家里。
//!
//! 两个方向，同一条底线——**上游和日志里都不该出现我们的私货**：
//!
//! 1. **剥字段**：客户端可能把 Freeport 自己的调试字段（`models` 兜底列表、
//!    `route` 调试回显、`x-freeport-*` 内部头对应的 body 键）一起 POST 上来。
//!    上游不认识这些键，宽容的厂商忽略，严格的（Anthropic 兼容层、Azure）
//!    直接 400。它们在 Freeport 内部有意义，出了门就是噪声。
//! 2. **去密钥**：上游把 key 回显在错误体里是常见行为（`Incorrect API key
//!    provided: sk-abc...`）。这条 body 以前会原样进 `LogEntry.error`、进
//!    界面、进返回给客户端的 400，等于把用户的密钥抄了一遍。
//!
//! 请求头不在清洗范围：`providers::headers` 每次从 `Provider` + `Credential`
//! 重建，客户端的 `Authorization`（虚拟 key）、`x-freeport-session`、
//! `x-freeport-path` 一个都不会被转发。

use serde_json::Value;

use crate::cred::Credential;
use crate::store::mask;

/// 短于这个长度的值不参与脱敏。
///
/// 一个 cookie 值可能是 `1`、`true`、`zh`，拿去全文替换会把正常内容打成
/// 马赛克。真实密钥（含 `sk-`、JWT、会话串）都远长于 8 位。
const MIN_NEEDLE: usize = 8;

/// 顶层要剥掉的键。`models` / `route` 是 OpenRouter 风格的兜底列表与调试
/// 回显——Freeport 自己就提供这两件事，不该再把它们丢给上游。
const INTERNAL_KEYS: &[&str] = &["models", "route", "_freeport", "freeport", "freeport_meta"];

/// 前缀式内部键：`x-freeport-*`（有的客户端会把内部头原样塞进 body）、
/// `x_freeport_*`（下划线变体）、`_fp_*`。
const INTERNAL_PREFIXES: &[&str] = &["x-freeport", "x_freeport", "_fp_"];

/// 剥掉 body 顶层的内部字段，返回剥掉的个数。
///
/// 只动顶层：`messages` 里出现的同名键是用户内容，改了就是篡改请求。
pub fn strip_internal(v: &mut Value) -> usize {
    let Some(obj) = v.as_object_mut() else {
        return 0;
    };
    let mut n = 0;
    for key in INTERNAL_KEYS {
        if obj.remove(*key).is_some() {
            n += 1;
        }
    }
    let prefixed: Vec<String> = obj
        .keys()
        .filter(|k| {
            let lower = k.to_ascii_lowercase();
            INTERNAL_PREFIXES.iter().any(|p| lower.starts_with(p))
        })
        .cloned()
        .collect();
    for key in prefixed {
        obj.remove(&key);
        n += 1;
    }
    n
}

/// 一组「不许出现在文本里」的字面值。
///
/// 按长度倒序替换：先换长的，短 needle 才不会把长 needle 切成半截——
/// `sk-abcdef123456` 被短的 `abcdef123456` 先换掉之后，剩下 `sk-` 前缀
/// 反而拼出一个看得见开头的残体。
#[derive(Debug, Default, Clone)]
pub struct Redactor {
    needles: Vec<String>,
}

impl Redactor {
    pub fn new<I, S>(parts: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut needles: Vec<String> = parts
            .into_iter()
            .map(Into::into)
            .map(|s| s.trim().to_string())
            .filter(|s| s.chars().count() >= MIN_NEEDLE)
            .collect();
        needles.sort_by(|a, b| b.chars().count().cmp(&a.chars().count()));
        needles.dedup();
        Self { needles }
    }

    /// 这次请求实际会用的那条凭据。只收当前凭据，不扫全库——
    /// 拿别的厂商的 key 去替换本请求的文本，等于给无关内容打码。
    pub fn for_credential(cred: &Credential) -> Self {
        match cred {
            Credential::Bearer { token, .. } => Self::new([token.clone()]),
            Credential::Cookie { jar, .. } => {
                Self::new(jar.entries.iter().map(|e| e.value.clone()))
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.needles.is_empty()
    }

    /// 替换并计数。返回 `(清洗后文本, 命中次数)`。
    pub fn redact_counted(&self, s: &str) -> (String, usize) {
        if self.needles.is_empty() {
            return (s.to_string(), 0);
        }
        let mut out = s.to_string();
        let mut hits = 0;
        for needle in &self.needles {
            let n = out.matches(needle.as_str()).count();
            if n > 0 {
                out = out.replace(needle.as_str(), &mask(needle));
                hits += n;
            }
        }
        (out, hits)
    }

    pub fn redact(&self, s: &str) -> String {
        self.redact_counted(s).0
    }

    /// 深度遍历，把所有字符串值里的字面量换掉。返回命中次数。
    pub fn redact_value(&self, v: &mut Value) -> usize {
        if self.needles.is_empty() {
            return 0;
        }
        match v {
            Value::String(s) => {
                let (clean, hits) = self.redact_counted(s);
                if hits > 0 {
                    *s = clean;
                }
                hits
            }
            Value::Array(items) => items.iter_mut().map(|i| self.redact_value(i)).sum(),
            Value::Object(map) => map.values_mut().map(|i| self.redact_value(i)).sum(),
            _ => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use freeport_auth::cookie::CookieJar;
    use serde_json::json;

    #[test]
    fn strips_only_top_level_internal_keys() {
        let mut body = json!({
            "model": "gpt-4o-mini",
            "messages": [{ "role": "user", "content": "hi", "models": ["keep-me"] }],
            "models": ["a", "b"],
            "route": { "debug": true },
            "x-freeport-path": "/v1/messages",
            "_fp_trace": "1",
            "temperature": 0.2
        });
        assert_eq!(strip_internal(&mut body), 4);
        assert!(body.get("models").is_none());
        assert!(body.get("route").is_none());
        assert!(body.get("x-freeport-path").is_none());
        // 用户内容里的同名键不能动
        assert_eq!(body["messages"][0]["models"][0], "keep-me");
        assert_eq!(body["temperature"], 0.2);
    }

    #[test]
    fn strips_nothing_from_non_object() {
        let mut body = json!([1, 2, 3]);
        assert_eq!(strip_internal(&mut body), 0);
        assert_eq!(body, json!([1, 2, 3]));
    }

    #[test]
    fn redacts_echoed_key_and_keeps_a_readable_stub() {
        let red = Redactor::new(["sk-abcdef1234567890"]);
        let (out, hits) = red.redact_counted("Incorrect API key provided: sk-abcdef1234567890.");
        assert_eq!(hits, 1);
        assert!(!out.contains("abcdef1234567890"));
        assert!(out.contains("sk-abc••••7890"), "{out}");
    }

    #[test]
    fn shortest_needle_never_wins() {
        // 长 needle 先换，短 needle 就不会把长的那半截留在明面上
        let red = Redactor::new(["abcdef1234567890", "sk-abcdef1234567890"]);
        let (out, hits) = red.redact_counted("key=sk-abcdef1234567890");
        assert_eq!(hits, 1);
        assert!(!out.contains("abcdef1234567890"));
    }

    #[test]
    fn short_values_are_ignored() {
        let red = Redactor::new(["1", "true", "zh"]);
        assert!(red.is_empty());
        assert_eq!(red.redact("1 true zh"), "1 true zh");
    }

    #[test]
    fn redacts_deeply_nested_strings() {
        let red = Redactor::new(["sk-abcdef1234567890"]);
        let mut v = json!({
            "error": { "message": ["bad key sk-abcdef1234567890"] },
            "choices": [{ "index": 0, "ok": true }]
        });
        assert_eq!(red.redact_value(&mut v), 1);
        assert!(!v.to_string().contains("abcdef1234567890"));
        // 非字符串原样保留
        assert_eq!(v["choices"][0]["index"], 0);
    }

    #[test]
    fn cookie_jar_values_are_all_collected() {
        let jar = CookieJar::parse_input(
            "sid=abcdefgh12345678; theme=dark; token=zzzzzzzzzzzzzzzz",
            "opencode.ai",
        )
        .jar;
        let cred = Credential::Cookie {
            account_id: "a".into(),
            jar,
        };
        let red = Redactor::for_credential(&cred);
        let (out, hits) = red.redact_counted("sid=abcdefgh12345678 token=zzzzzzzzzzzzzzzz theme=dark");
        assert_eq!(hits, 2, "{out}");
        assert!(!out.contains("abcdefgh12345678"));
        assert!(!out.contains("zzzzzzzzzzzzzzzz"));
        // 短值不动
        assert!(out.contains("theme=dark"));
    }

    #[test]
    fn empty_redactor_is_a_passthrough() {
        let red = Redactor::default();
        assert!(red.is_empty());
        let mut v = json!({ "a": "sk-abcdef1234567890" });
        assert_eq!(red.redact_value(&mut v), 0);
        assert_eq!(v["a"], "sk-abcdef1234567890");
    }
}
