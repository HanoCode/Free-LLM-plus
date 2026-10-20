//! Cookie jar —— RFC 6265 的最小可用子集。
//!
//! 为什么不用现成的 jar：我们只需要「按 URL 选出该发的 cookie、把上游轮换的
//! 新值学回来」这一件事，而这件事的坑全在**边界**上：
//!
//! * **域**：`notevil.com` 不能命中 `evil.com` 的 cookie。少了这道判断，
//!   就是把自己会话往别人域名里送。
//! * **路径**：`/foo` 不能命中 `/foobar`（RFC 要求前缀处必须是路径分隔）。
//! * **顺序**：RFC 6265 5.4 规定 path 长的先发；不少服务端「同名取第一个」，
//!   顺序错了等于发了个旧值。
//! * **注入**：粘贴进来的值里若带 CR/LF，就能往请求里塞任意头。这里**直接拒绝**
//!   而不是「清洗后照发」——清洗过的值只会 401，还会把真问题藏起来。
//!
//! 不做：Public Suffix List（我们用不到跨站 cookie）、SameSite（我们不模拟浏览器
//! 的站点上下文，只按域名发）。

use serde::{Deserialize, Serialize};

/// 一条 cookie。字段对齐浏览器 DevTools 导出格式，粘贴即可用。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CookieEntry {
    pub name: String,
    pub value: String,
    /// 小写、无前导点。`host_only` 为真时它就是精确 host。
    #[serde(default)]
    pub domain: String,
    #[serde(default = "root_path")]
    pub path: String,
    #[serde(default)]
    pub secure: bool,
    #[serde(default)]
    pub http_only: bool,
    /// 只发给 `domain` 本身，不含子域。用户手贴的一律 true——
    /// 我们没资格替用户决定「这条会话也发给所有子域」。
    #[serde(default = "yes")]
    pub host_only: bool,
    /// unix 秒。`None` = 会话 cookie（关掉浏览器就没了），我们照发。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    /// 写入顺序，用于同 path 长度下的稳定排序
    #[serde(default)]
    pub seq: u64,
}

fn root_path() -> String {
    "/".into()
}

fn yes() -> bool {
    true
}

impl CookieEntry {
    pub fn new(name: impl Into<String>, value: impl Into<String>, host: &str) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
            domain: host.to_ascii_lowercase(),
            path: root_path(),
            secure: false,
            http_only: false,
            host_only: true,
            expires_at: None,
            seq: 0,
        }
    }

    fn expired(&self, now: i64) -> bool {
        matches!(self.expires_at, Some(at) if at <= now)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CookieJar {
    #[serde(default)]
    pub entries: Vec<CookieEntry>,
}

/// 导入结果：成功的条目 + 被拒的说明。界面要把后者如实显示出来，
/// 「静默丢两条」比「报错」危险得多——用户会以为已经登进去了。
#[derive(Debug, Clone, Default)]
pub struct Imported {
    pub jar: CookieJar,
    pub skipped: Vec<String>,
}

impl CookieJar {
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 全部过期了吗。会话 cookie（无 expiresAt）不算过期。
    pub fn all_expired(&self, now: i64) -> bool {
        !self.entries.is_empty() && self.entries.iter().all(|c| c.expired(now))
    }

    /// 还剩多久到期（秒）。取最早到期的那条；全是会话 cookie 则 `None`。
    pub fn expires_in(&self, now: i64) -> Option<i64> {
        self.entries
            .iter()
            .filter_map(|c| c.expires_at)
            .map(|at| (at - now).max(0))
            .min()
    }

    /// 先试着认三种粘贴格式：DevTools 的 JSON 数组、一整行 `Cookie:`、
    /// 多行 `Set-Cookie`。认不出来就退回「当成一整行 Cookie」。
    pub fn parse_input(raw: &str, host: &str) -> Imported {
        let text = raw.trim();
        if text.is_empty() {
            return Imported::default();
        }

        if text.starts_with('[') {
            return Self::parse_json(text, host);
        }

        // 多行且每行都带属性（`Domain=` / `Path=` / `Expires=`）→ 当 Set-Cookie 逐行吸收
        let lines: Vec<&str> = text
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .collect();
        let looks_like_set_cookie = lines.len() > 1
            || lines.first().is_some_and(|l| {
                l.to_ascii_lowercase().starts_with("set-cookie:")
                    || l.contains("; domain=")
                    || l.contains("; path=")
                    || l.contains("; expires=")
            });

        let mut jar = CookieJar::default();
        let mut skipped = Vec::new();
        if looks_like_set_cookie {
            let mut now = 0i64;
            for line in &lines {
                let line = strip_prefix(line, "set-cookie:");
                jar.absorb_one(host, line, &mut now, &mut skipped);
            }
        } else {
            let line = strip_prefix(text, "cookie:");
            // 单行 Cookie 串里可能有换行被折叠，统一按 `;` 切
            for part in line.split(';') {
                let part = part.trim();
                if part.is_empty() {
                    continue;
                }
                match split_pair(part) {
                    Some((name, value)) if valid_name(name) && valid_value(value) => {
                        jar.push(CookieEntry::new(name, value, host));
                    }
                    _ => skipped.push(format!("无法识别的片段：{part}")),
                }
            }
        }

        Imported { jar, skipped }
    }

    fn parse_json(text: &str, host: &str) -> Imported {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Raw {
            name: String,
            #[serde(default)]
            value: String,
            #[serde(default)]
            domain: String,
            #[serde(default)]
            path: String,
            #[serde(default)]
            secure: bool,
            #[serde(default)]
            http_only: bool,
            /// Chrome DevTools 用 f64 秒
            #[serde(default)]
            expiration_date: Option<f64>,
        }

        let mut out = Imported::default();
        let Ok(items) = serde_json::from_str::<Vec<Raw>>(text) else {
            out.skipped.push("JSON 格式无法解析".into());
            return out;
        };
        for it in items {
            if !valid_name(&it.name) || !valid_value(&it.value) {
                out.skipped.push(format!("跳过非法条目：{}", it.name));
                continue;
            }
            let domain = it.domain.trim().trim_start_matches('.').to_ascii_lowercase();
            // DevTools 导出的 domain 可能带前导点，表示「含子域」；这里据实记录
            let host_only = domain.is_empty() || it.domain.trim() == it.domain.trim_matches('.');
            out.jar.push(CookieEntry {
                name: it.name,
                value: it.value,
                domain: if domain.is_empty() { host.to_ascii_lowercase() } else { domain },
                path: if it.path.is_empty() { root_path() } else { it.path },
                secure: it.secure,
                http_only: it.http_only,
                host_only,
                expires_at: it.expiration_date.map(|f| f as i64),
                seq: 0,
            });
        }
        out
    }

    /// 从上游响应里学新 cookie。会话轮换（Set-Cookie 换 session）全靠这个，
    /// 否则用户过一会儿就被踢出去，而且看不出来为什么。
    pub fn absorb(&mut self, host: &str, set_cookie: &[String], now: i64) -> Vec<String> {
        let mut skipped = Vec::new();
        let mut clock = now;
        for raw in set_cookie {
            self.absorb_one(host, raw, &mut clock, &mut skipped);
        }
        skipped
    }

    fn absorb_one(&mut self, host: &str, raw: &str, now: &mut i64, skipped: &mut Vec<String>) {
        let mut parts = raw.split(';');
        let Some(first) = parts.next() else { return };
        let Some((name, value)) = split_pair(first.trim()) else {
            skipped.push(format!("无法识别的 Set-Cookie：{raw}"));
            return;
        };
        if !valid_name(name) || !valid_value(value) {
            skipped.push(format!("Set-Cookie 含非法字符：{name}"));
            return;
        }

        let mut entry = CookieEntry::new(name, value, host);
        let mut max_age: Option<i64> = None;
        let mut expires: Option<i64> = None;
        let mut domain_given = false;

        for attr in parts {
            let attr = attr.trim();
            let Some((k, v)) = split_pair(attr) else { continue };
            match k.to_ascii_lowercase().as_str() {
                "domain" => {
                    let d = v.trim().trim_start_matches('.').to_ascii_lowercase();
                    if !d.is_empty() {
                        entry.domain = d;
                        // RFC 6265：只要响应显式给了 Domain，这条就是「域 cookie」，
                        // 会发给该域及其子域。前导点只是写法差异，不代表语义。
                        entry.host_only = false;
                        domain_given = true;
                    }
                }
                "path" => {
                    if v.starts_with('/') {
                        entry.path = v.to_string();
                    }
                }
                "secure" => entry.secure = true,
                "httponly" => entry.http_only = true,
                "max-age" => max_age = v.trim().parse::<i64>().ok(),
                "expires" => expires = parse_http_date(v.trim()),
                _ => {}
            }
        }

        // Max-Age 优先于 Expires（RFC 6265 5.2.2）
        entry.expires_at = match max_age {
            Some(0) | Some(-1) => {
                // 删除指令
                self.remove(name, &entry.domain, &entry.path);
                return;
            }
            Some(secs) if secs > 0 => Some(*now + secs),
            _ => expires,
        };

        // 域既不是当前 host 的父域也不是它自己：多半是别的站点的 cookie，丢弃
        if domain_given && !domain_match(host, &entry.domain, false) {
            skipped.push(format!("域的 cookie 不属于当前站点，已丢弃：{}", entry.domain));
            return;
        }

        self.upsert(entry);
    }

    fn upsert(&mut self, mut entry: CookieEntry) {
        let seq = self.entries.iter().map(|c| c.seq).max().unwrap_or(0) + 1;
        entry.seq = seq;
        self.remove(&entry.name.clone(), &entry.domain.clone(), &entry.path.clone());
        self.entries.push(entry);
    }

    fn push(&mut self, mut entry: CookieEntry) {
        if entry.seq == 0 {
            entry.seq = self.entries.iter().map(|c| c.seq).max().unwrap_or(0) + 1;
        }
        self.entries.push(entry);
    }

    fn remove(&mut self, name: &str, domain: &str, path: &str) {
        self.entries
            .retain(|c| !(c.name == name && c.domain == domain && c.path == path));
    }

    /// 这次请求该带什么 cookie。没有可发的返回 `None`（不是空串——
    /// 空的 `Cookie:` 头有些网关会当非法请求）。
    pub fn header_for(&self, url: &str, now: i64) -> Option<String> {
        let (secure, host, path) = parse_url(url)?;
        let mut hits: Vec<&CookieEntry> = self
            .entries
            .iter()
            .filter(|c| !c.expired(now))
            .filter(|c| !c.secure || secure)
            .filter(|c| domain_match(&host, &c.domain, c.host_only))
            .filter(|c| path_match(&path, &c.path))
            .collect();

        if hits.is_empty() {
            return None;
        }

        // RFC 6265 5.4：path 长的先发，同长按写入顺序
        hits.sort_by(|a, b| {
            b.path
                .len()
                .cmp(&a.path.len())
                .then(a.seq.cmp(&b.seq))
        });

        Some(
            hits.iter()
                .map(|c| format!("{}={}", c.name, c.value))
                .collect::<Vec<_>>()
                .join("; "),
        )
    }

    /// 界面上显示「3 条 cookie」，不暴露值的内容。
    pub fn summary(&self) -> String {
        let names: Vec<&str> = self.entries.iter().take(4).map(|c| c.name.as_str()).collect();
        let more = self.entries.len().saturating_sub(names.len());
        if more > 0 {
            format!("{} 等 {} 条", names.join(", "), self.entries.len())
        } else {
            names.join(", ")
        }
    }
}

fn strip_prefix<'a>(s: &'a str, prefix: &str) -> &'a str {
    let lower = s.to_ascii_lowercase();
    if lower.starts_with(prefix) {
        s[prefix.len()..].trim()
    } else {
        s
    }
}

fn split_pair(s: &str) -> Option<(&str, &str)> {
    let (k, v) = s.split_once('=')?;
    Some((k.trim(), v.trim()))
}

/// 名字里出现分隔符或空白就没法拼回头部了；控制字符更是注入面。
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && !name
            .chars()
            .any(|c| c == '=' || c == ';' || c == ',' || c.is_whitespace() || c.is_control())
}

/// 值允许多数可见字符，但不许 `;`（会改变语义）与 CR/LF（头注入）。
fn valid_value(value: &str) -> bool {
    !value
        .chars()
        .any(|c| c == ';' || c == '\r' || c == '\n' || c == '\0')
}

/// 极简 URL 拆分：只服务我们自己配置的 base_url，不追求覆盖所有合法形态。
fn parse_url(url: &str) -> Option<(bool, String, String)> {
    let (scheme, rest) = url.split_once("://")?;
    let secure = matches!(scheme.to_ascii_lowercase().as_str(), "https" | "wss");
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };

    // 去掉 userinfo 与端口；IPv6 字面量用方括号包着
    let host_port = authority.rsplit('@').next().unwrap_or(authority);
    let host = if let Some(stripped) = host_port.strip_prefix('[') {
        match stripped.find(']') {
            Some(i) => stripped[..i].to_string(),
            None => host_port.to_string(),
        }
    } else {
        host_port.split(':').next().unwrap_or("").to_string()
    };
    if host.is_empty() {
        return None;
    }

    let path = path.split(['?', '#']).next().unwrap_or("/");
    let path = if path.is_empty() { "/" } else { path };
    Some((secure, host.to_ascii_lowercase(), path.to_string()))
}

fn is_ip(host: &str) -> bool {
    !host.is_empty() && host.chars().all(|c| c.is_ascii_digit() || c == '.')
}

fn domain_match(host: &str, domain: &str, host_only: bool) -> bool {
    if domain.is_empty() {
        return false;
    }
    if host == domain {
        return true;
    }
    if host_only || is_ip(host) || is_ip(domain) {
        return false;
    }
    // 必须是「点 + 域」结尾：notevil.com 不能命中 evil.com
    host.len() > domain.len()
        && host.ends_with(domain)
        && host.as_bytes()[host.len() - domain.len() - 1] == b'.'
}

fn path_match(request: &str, cookie_path: &str) -> bool {
    if request == cookie_path {
        return true;
    }
    if !request.starts_with(cookie_path) {
        return false;
    }
    // /foo 不能命中 /foobar
    cookie_path.ends_with('/')
        || request.as_bytes().get(cookie_path.len()).copied() == Some(b'/')
}

/// 只认 RFC 1123 那一种（cookie 的实际形态）。解析失败就当没有过期时间，
/// 比「猜一个过期时间」安全——猜早了会把有效会话丢掉。
fn parse_http_date(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc2822(s)
        .ok()
        .map(|d| d.timestamp())
        .or_else(|| {
            chrono::NaiveDateTime::parse_from_str(s, "%a, %d %b %Y %H:%M:%S GMT")
                .ok()
                .map(|d| d.and_utc().timestamp())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_700_000_000;

    fn jar(pairs: &[(&str, &str)], host: &str) -> CookieJar {
        let mut j = CookieJar::default();
        for (k, v) in pairs {
            j.push(CookieEntry::new(*k, *v, host));
        }
        j
    }

    #[test]
    fn parses_plain_cookie_header() {
        let out = CookieJar::parse_input("Cookie: session=abc; theme=dark", "opencode.ai");
        assert_eq!(out.jar.len(), 2);
        assert!(out.skipped.is_empty());
        assert_eq!(
            out.jar.header_for("https://opencode.ai/zen/v1/chat", NOW).as_deref(),
            Some("session=abc; theme=dark")
        );
    }

    #[test]
    fn parses_devtools_json() {
        let raw = r#"[
          {"name":"sid","value":"s1","domain":".opencode.ai","path":"/","secure":true,"httpOnly":true,"expirationDate":1800000000},
          {"name":"uid","value":"7","domain":"opencode.ai","path":"/zen"}
        ]"#;
        let out = CookieJar::parse_input(raw, "opencode.ai");
        assert_eq!(out.jar.len(), 2);
        // 带前导点的是共享 cookie，可以发给子域
        assert_eq!(
            out.jar.header_for("https://api.opencode.ai/v1", NOW).as_deref(),
            Some("sid=s1")
        );
        // /zen 那条在根路径上不该发出去
        assert_eq!(
            out.jar.header_for("https://opencode.ai/other", NOW).as_deref(),
            Some("sid=s1")
        );
        assert_eq!(
            out.jar.header_for("https://opencode.ai/zen/v1", NOW).as_deref(),
            // /zen 的 path 更长，按 RFC 6265 5.4 要排在前面
            Some("uid=7; sid=s1")
        );
    }

    #[test]
    fn rejects_crlf_injection() {
        // 单行里的裸 \r：能拼进头就是注入面
        let out = CookieJar::parse_input("sid=abc\rX-Evil: 1", "opencode.ai");
        assert!(out.jar.is_empty(), "带控制字符的值必须整条拒绝");
        assert_eq!(out.skipped.len(), 1);
    }

    #[test]
    fn rejects_crlf_hidden_in_json_values() {
        let raw = r#"[{"name":"sid","value":"abc\r\nX-Evil: 1"}]"#;
        let out = CookieJar::parse_input(raw, "opencode.ai");
        assert!(out.jar.is_empty());
        assert_eq!(out.skipped.len(), 1);
    }

    #[test]
    fn domain_suffix_must_align_on_dot() {
        let j = jar(&[("sid", "1")], "evil.com");
        assert!(j.header_for("https://evil.com/x", NOW).is_some());
        // 关键：notevil.com 不是 evil.com 的子域
        assert!(j.header_for("https://notevil.com/x", NOW).is_none());
    }

    #[test]
    fn path_prefix_must_align_on_slash() {
        let mut j = CookieJar::default();
        j.push(CookieEntry {
            path: "/foo".into(),
            ..CookieEntry::new("a", "1", "x.com")
        });
        assert!(j.header_for("https://x.com/foo/bar", NOW).is_some());
        assert!(j.header_for("https://x.com/foobar", NOW).is_none());
    }

    #[test]
    fn secure_cookie_never_goes_over_plain_http() {
        let mut j = CookieJar::default();
        j.push(CookieEntry {
            secure: true,
            ..CookieEntry::new("sid", "1", "x.com")
        });
        assert!(j.header_for("https://x.com/", NOW).is_some());
        assert!(j.header_for("http://x.com/", NOW).is_none());
    }

    #[test]
    fn expired_cookies_are_dropped() {
        let mut j = CookieJar::default();
        j.push(CookieEntry {
            expires_at: Some(NOW - 1),
            ..CookieEntry::new("old", "1", "x.com")
        });
        assert!(j.header_for("https://x.com/", NOW).is_none());
        assert!(j.all_expired(NOW));
    }

    #[test]
    fn longer_path_comes_first() {
        let mut j = CookieJar::default();
        j.push(CookieEntry::new("root", "1", "x.com"));
        j.push(CookieEntry {
            path: "/zen/v1".into(),
            ..CookieEntry::new("deep", "2", "x.com")
        });
        assert_eq!(
            j.header_for("https://x.com/zen/v1/chat", NOW).as_deref(),
            Some("deep=2; root=1")
        );
    }

    #[test]
    fn absorbs_rotation_from_set_cookie() {
        let mut j = jar(&[("sid", "old")], "x.com");
        j.absorb(
            "x.com",
            &["sid=new; Path=/; HttpOnly".to_string()],
            NOW,
        );
        assert_eq!(j.len(), 1, "同名同域同路径要覆盖，不能两条并存");
        assert_eq!(j.header_for("https://x.com/", NOW).as_deref(), Some("sid=new"));
    }

    #[test]
    fn max_age_zero_deletes() {
        let mut j = jar(&[("sid", "1")], "x.com");
        j.absorb("x.com", &["sid=; Max-Age=0".to_string()], NOW);
        assert!(j.is_empty());
    }

    #[test]
    fn ignores_cookies_for_other_domains() {
        let mut j = CookieJar::default();
        let skipped = j.absorb("x.com", &["sid=1; Domain=other.com".to_string()], NOW);
        assert!(j.is_empty(), "别的域的 cookie 不能进这条凭据");
        assert_eq!(skipped.len(), 1);
    }

    #[test]
    fn set_cookie_lines_are_recognised() {
        let raw = "sid=1; Path=/; Domain=.x.com\nuid=2; Path=/zen";
        let out = CookieJar::parse_input(raw, "x.com");
        assert_eq!(out.jar.len(), 2);
        assert_eq!(
            out.jar.header_for("https://x.com/zen/v1", NOW).as_deref(),
            Some("uid=2; sid=1")
        );
    }

    #[test]
    fn summary_does_not_leak_values() {
        let j = jar(&[("sid", "super-secret")], "x.com");
        let s = j.summary();
        assert_eq!(s, "sid");
        assert!(!s.contains("super-secret"));
    }
}
