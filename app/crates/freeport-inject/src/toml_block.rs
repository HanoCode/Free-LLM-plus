//! Codex 的 config.toml 用「带标记的托管块」。
//!
//! 为什么不整文件重排：TOML 里注释、空行、键顺序都是用户写下的意图，
//! 重排一次就丢一次信息。托管块只占自己那几行，删掉就和没写过一样。
//!
//! 一个必须处理的细节：`model_provider` 是根键，必须插在**第一个表头之前**，
//! 否则会被解析成属于上一张表，Codex 就读不到了。

/// 用户自己的根键会被我们暂时注释掉，加这个前缀，回滚时原样还回去。
const OFF: &str = "#freeport-off: ";

pub const BEGIN: &str = "# >>> freeport managed begin";
pub const END: &str = "# <<< freeport managed end";

/// 生成托管块正文（不含结尾换行）。
pub fn block(base_v1: &str, key: &str, model: &str) -> String {
    format!(
        "{BEGIN}
model_provider = \"freeport\"
model = \"{model}\"

[model_providers.freeport]
name = \"Freeport\"
base_url = \"{base_v1}\"
# Codex 自定义 provider 不读 auth.json，密钥随请求头走
wire_api = \"chat\"
http_headers = {{ Authorization = \"Bearer {key}\" }}
{END}"
    )
}

pub fn has_block(text: &str) -> bool {
    text.contains(BEGIN) && text.contains(END)
}

/// 有就替换，没有就插到第一个表头之前。
pub fn upsert(text: &str, block: &str) -> String {
    if has_block(text) {
        let mut out = String::new();
        let mut skipping = false;
        for line in text.lines() {
            if line.trim() == BEGIN {
                out.push_str(block);
                out.push('\n');
                skipping = true;
                continue;
            }
            if skipping {
                if line.trim() == END {
                    skipping = false;
                }
                continue;
            }
            out.push_str(line);
            out.push('\n');
        }
        return out;
    }

    // 根键 `model` / `model_provider` 会和我们块里的键重名（TOML 直接判非法），
    // 先注释掉用户那几行，标记前缀保证回滚能原样还原。
    let text = comment_root_keys(text);
    let mut lines: Vec<&str> = text.lines().collect();
    let at = lines
        .iter()
        .position(|l| l.trim_start().starts_with('['))
        .unwrap_or(lines.len());

    let mut out = String::new();
    for (i, l) in lines.drain(..).enumerate() {
        if i == at {
            out.push_str(block);
            out.push_str("\n\n");
        }
        out.push_str(l);
        out.push('\n');
    }
    if at >= out.lines().count() {
        out.push_str(block);
        out.push('\n');
    }
    out
}

/// 删掉托管块，连同它留下的空行。
pub fn strip(text: &str) -> String {
    if !has_block(text) {
        return text.to_string();
    }
    let mut out: Vec<String> = Vec::new();
    let mut skipping = false;
    for line in text.lines() {
        if line.trim() == BEGIN {
            skipping = true;
            // 顺手吃掉块前面刚加的那个空行
            if out.last().is_some_and(|l: &String| l.trim().is_empty()) {
                out.pop();
            }
            continue;
        }
        if skipping {
            if line.trim() == END {
                skipping = false;
            }
            continue;
        }
        out.push(line.to_string());
    }
    let mut s = out.join("\n");
    if text.ends_with('\n') && !s.is_empty() {
        s.push('\n');
    }
    uncomment_root_keys(&s)
}

/// 注释掉根段的 `model` / `model_provider`（遇到第一个表头就停）。
fn comment_root_keys(text: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut in_root = true;
    for line in text.lines() {
        let t = line.trim_start();
        if t.starts_with('[') {
            in_root = false;
        }
        if in_root && is_override(line) {
            out.push(format!("{OFF}{line}"));
        } else {
            out.push(line.to_string());
        }
    }
    let mut s = out.join("\n");
    if text.ends_with('\n') {
        s.push('\n');
    }
    s
}

fn uncomment_root_keys(text: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        match line.strip_prefix(OFF) {
            Some(rest) => out.push(rest.to_string()),
            None => out.push(line.to_string()),
        }
    }
    let mut s = out.join("\n");
    if text.ends_with('\n') {
        s.push('\n');
    }
    s
}

fn is_override(line: &str) -> bool {
    let t = line.trim_start();
    for k in ["model_provider", "model"] {
        if let Some(rest) = t.strip_prefix(k) {
            if rest.trim_start().starts_with('=') {
                return true;
            }
        }
    }
    false
}

/// 写完必须能解析，且真的指到 freeport。宁可在写入前失败，也不要留一个坏文件。
pub fn validate(text: &str) -> Result<(), String> {
    let v: toml::Value = toml::from_str(text).map_err(|e| format!("TOML 解析失败：{e}"))?;
    let provider = v.get("model_provider").and_then(|x| x.as_str());
    if provider != Some("freeport") {
        return Err("model_provider 没指到 freeport".into());
    }
    let table = v
        .get("model_providers")
        .and_then(|x| x.get("freeport"))
        .ok_or("缺少 [model_providers.freeport] 表")?;
    if table.get("base_url").and_then(|x| x.as_str()).is_none() {
        return Err("托管块里没有 base_url".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> String {
        block("http://127.0.0.1:8787/v1", "fp-abc", "auto")
    }

    #[test]
    fn inserts_before_first_table_so_root_keys_stay_root() {
        let src = "model = \"gpt-5\"\n\n[model_providers.other]\nname = \"x\"\n";
        let out = upsert(src, &sample());
        assert!(out.find(BEGIN).unwrap() < out.find("[model_providers.other]").unwrap());
        validate(&out).unwrap();
        assert_eq!(
            toml::from_str::<toml::Value>(&out).unwrap()["model_providers"]["other"]["name"],
            toml::Value::String("x".into())
        );
    }

    #[test]
    fn upsert_is_idempotent() {
        let once = upsert("model = \"x\"\n", &sample());
        let twice = upsert(&once, &sample());
        assert_eq!(once, twice, "重复注入不应叠加");
        assert_eq!(twice.matches(BEGIN).count(), 1);
    }

    #[test]
    fn strip_restores_original_shape() {
        let src = "model = \"gpt-5\"\n\n[model_providers.other]\nname = \"x\"\n";
        let out = upsert(src, &sample());
        let back = strip(&out);
        assert_eq!(back, src, "回滚后应与原文一致，实际：{back:?}");
    }

    #[test]
    fn broken_block_is_rejected() {
        let bad = format!("{BEGIN}\nmodel_provider = \"x\"\n{END}\n");
        assert!(validate(&bad).is_err());
    }
}
