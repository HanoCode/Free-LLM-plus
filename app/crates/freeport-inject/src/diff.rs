//! 行级 diff。写入前给用户看清楚「加哪几行」，比任何文案都可信。

use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct DiffLine {
    /// ctx | add | del
    pub kind: String,
    pub no: usize,
    pub text: String,
    /// 这一行是不是 Freeport 写的
    pub managed: bool,
}

/// 最长公共子序列做行对齐。
///
/// 配置文件通常几百行，O(n·m) 足够；换成 Myers 只会让代码难读，
/// 这里的瓶颈是终端渲染不是算法。
pub fn diff(old: &str, new: &str, needles: &[&str]) -> Vec<DiffLine> {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    let (n, m) = (a.len(), b.len());

    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if a[i] == b[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }

    let is_managed = |s: &str| needles.iter().any(|k| !k.is_empty() && s.contains(k));

    let mut out = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    let mut no = 1usize;
    while i < n && j < m {
        if a[i] == b[j] {
            out.push(DiffLine {
                kind: "ctx".into(),
                no,
                text: a[i].to_string(),
                managed: false,
            });
            i += 1;
            j += 1;
            no += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            out.push(DiffLine {
                kind: "del".into(),
                no,
                text: a[i].to_string(),
                managed: is_managed(a[i]),
            });
            i += 1;
            no += 1;
        } else {
            out.push(DiffLine {
                kind: "add".into(),
                no,
                text: b[j].to_string(),
                managed: is_managed(b[j]),
            });
            j += 1;
            no += 1;
        }
    }
    while i < n {
        out.push(DiffLine {
            kind: "del".into(),
            no,
            text: a[i].to_string(),
            managed: is_managed(a[i]),
        });
        i += 1;
        no += 1;
    }
    while j < m {
        out.push(DiffLine {
            kind: "add".into(),
            no,
            text: b[j].to_string(),
            managed: is_managed(b[j]),
        });
        j += 1;
        no += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_only_our_lines() {
        let old = "{\n  \"theme\": \"dark\"\n}\n";
        let new = "{\n  \"theme\": \"dark\",\n  \"baseUrl\": \"http://127.0.0.1:8787\"\n}\n";
        let d = diff(old, new, &["freeport", "127.0.0.1:8787"]);
        let added: Vec<_> = d.iter().filter(|l| l.kind == "add").collect();
        // 行级 diff 里「末尾补个逗号」也会显示成一删一加，和 git 一致
        assert!(
            added.iter().any(|l| l.managed),
            "带端点的那行应当被标成 freeport 写的"
        );
        assert_eq!(d.iter().filter(|l| l.kind == "del").count(), 1);
    }

    #[test]
    fn unchanged_text_has_no_noise() {
        let s = "a\nb\n";
        assert!(diff(s, s, &[]).iter().all(|l| l.kind == "ctx"));
    }
}
