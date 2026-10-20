//! Claude Desktop 的 3p（第三方网关）配置。
//!
//! 与其它客户端最大的不同：它不是「一个文件里写几个键」，而是**三个文件互相引用**——
//!
//! ```text
//! Claude-3p/configLibrary/<id>.json     我们的网关档（地址、key、能力开关）
//! Claude-3p/configLibrary/_meta.json    { entries: [...], appliedId: <id> }
//! Claude-3p/claude_desktop_config.json  { deploymentMode: "3p" }
//! ```
//!
//! 少写任何一个，Desktop 都会退回登录态或选到别的档。所以写入三步一起做、
//! 回滚三份一起还——这也是 `journal::Entry::files` 存在的原因：
//! 单文件的 `prev` 表达不了「三个文件各自原来长什么样」。
//!
//! 两条纪律：
//!
//! 1. **只认领带标记的档**。`_meta.json` 里 id 对得上、内容却不是我们的（没有
//!    `X-Freeport-Model-View` 头），说明 id 撞了，停手而不是覆盖别人的档。
//! 2. **`_meta.json` 结构不对就不写**。那份索引里还记着用户其它网关档，
//!    猜着改会让他在 Desktop 里丢掉整套配置。

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::spec::{self, DesktopSpec};
use crate::{journal, Ctx, DiffLine};

const SUPPORTED: &[&str] = &["openai", "anthropic", "gemini"];

/// 一次计划好的写入。`after = None` 表示这个文件我们要删掉。
struct Planned {
    path: PathBuf,
    before: Option<Value>,
    after: Option<Value>,
}

fn read_json(path: &Path) -> Result<Option<Value>, String> {
    let raw = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("读取 {} 失败：{e}", path.display())),
    };
    let text = raw.trim_start_matches('\u{feff}');
    if text.trim().is_empty() {
        return Ok(None);
    }
    serde_json::from_str(text)
        .map(Some)
        .map_err(|_| format!("{} 不是合法 JSON，停止写入", name_of(path)))
}

fn name_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

fn pretty(v: &Value) -> String {
    let mut s = serde_json::to_string_pretty(v).unwrap_or_else(|_| "{}".into());
    s.push('\n');
    s
}

fn text_of(v: Option<&Value>) -> String {
    v.map(pretty).unwrap_or_default()
}

/// 36 位小写十六进制 + 短横线。Desktop 只认这个形状的 id。
fn valid_id(id: &str) -> bool {
    id.len() == 36
        && id
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase() || c == '-')
}

/// 这个档是我们的吗：provider 对不对 + 标记头在不在。
fn ours(profile: &Value, d: &DesktopSpec) -> bool {
    let gateway = profile.get("inferenceProvider").and_then(Value::as_str) == Some("gateway");
    let marked = profile
        .get("inferenceCustomHeaders")
        .and_then(Value::as_object)
        .is_some_and(|h| {
            h.iter().any(|(k, v)| {
                k.eq_ignore_ascii_case(d.marker_header) && v.as_str() == Some(d.marker_value)
            })
        });
    gateway && marked
}

fn validate_meta(meta: &Value, d: &DesktopSpec) -> Result<(), String> {
    let obj = meta.as_object().ok_or("_meta.json 不是对象，停止写入")?;
    let entries = match obj.get("entries") {
        None => Vec::new(),
        Some(Value::Array(a)) => a.clone(),
        Some(_) => return Err("_meta.json 的 entries 不是数组，停止写入".into()),
    };
    let mut ids = Vec::new();
    for e in &entries {
        let id = e
            .get("id")
            .and_then(Value::as_str)
            .ok_or("_meta.json 里有条目缺少 id，停止写入")?;
        if !valid_id(id) {
            return Err(format!("_meta.json 里的 id「{id}」不是合法 uuid，停止写入"));
        }
        if id == d.profile_id && e.get("name").and_then(Value::as_str) != Some(d.profile_name) {
            return Err("配置库里已有一套档占用了我们的 id，先手工改名或删除再注入".into());
        }
        ids.push(id.to_string());
    }
    if let Some(applied) = obj.get("appliedId").and_then(Value::as_str) {
        if !ids.iter().any(|i| i == applied) {
            return Err("_meta.json 的 appliedId 指向不存在的档，停止写入".into());
        }
    } else if !ids.is_empty() {
        return Err("_meta.json 缺少 appliedId，停止写入".into());
    }
    Ok(())
}

/// 算出三个文件各自要变成什么。纯函数：不落盘，预览与写入共用。
fn plan(d: &DesktopSpec, ctx: &Ctx) -> Result<Vec<Planned>, String> {
    let profile_path = spec::desktop_profile(d);
    let meta_path = spec::desktop_meta();
    let mode_path = spec::desktop_mode();

    let meta_before = read_json(&meta_path)?.ok_or_else(|| {
        "还没找到 Claude-3p 配置库；先在 Claude Desktop 里启用一次第三方网关模式再注入".to_string()
    })?;
    validate_meta(&meta_before, d)?;

    let profile_before = read_json(&profile_path)?;
    if let Some(p) = &profile_before {
        if !ours(p, d) {
            return Err("配置库里我们的档位 id 已被别的网关占用，先手工删除那一档再注入".into());
        }
    }
    let mode_before = read_json(&mode_path)?;

    /* ── 我们的档 ── */
    let mut profile = profile_before.clone().unwrap_or_else(|| json!({}));
    if !profile.is_object() {
        return Err("我们的档不是对象，停止写入".into());
    }
    profile["inferenceProvider"] = json!("gateway");
    profile["inferenceGatewayBaseUrl"] = json!(ctx.base.trim_end_matches('/'));
    // static + bearer：Desktop 自己发 `Authorization: Bearer <key>`，
    // 与网关的虚拟 key 校验对得上，不需要它做任何握手
    profile["inferenceCredentialKind"] = json!("static");
    profile["inferenceGatewayApiKey"] = json!(ctx.key);
    profile["inferenceGatewayAuthScheme"] = json!("bearer");
    // 让 Desktop 自己拉 /v1/models，模型换了不用重新注入
    profile["modelDiscoveryEnabled"] = json!(true);
    let mut headers = profile
        .get("inferenceCustomHeaders")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    headers.retain(|k, _| !k.eq_ignore_ascii_case(d.marker_header));
    headers.insert(d.marker_header.to_string(), json!(d.marker_value));
    profile["inferenceCustomHeaders"] = Value::Object(headers);
    // Desktop 默认只允许推理流量，工具要连别的主机会被拦。
    // 只在用户没设过时补：他已经配过的白名单比我们猜的准。
    if profile.get("coworkEgressAllowedHosts").is_none() {
        profile["coworkEgressAllowedHosts"] = json!(["*"]);
    }

    /* ── 索引 ── */
    let mut meta = meta_before.clone();
    let entries: Vec<Value> = meta_before
        .get("entries")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|e| e.get("id").and_then(Value::as_str) != Some(d.profile_id))
        .collect();
    let mut entries = entries;
    entries.push(json!({ "id": d.profile_id, "name": d.profile_name }));
    meta["entries"] = Value::Array(entries);
    meta["appliedId"] = json!(d.profile_id);
    if let Some(o) = meta.as_object_mut() {
        // 混合指针是 Desktop 内部的一种过渡状态，切档时清掉，否则它会盖过 appliedId
        o.remove("hybridPointer");
    }

    /* ── 模式开关 ── */
    let mut mode = mode_before.clone().unwrap_or_else(|| json!({}));
    if !mode.is_object() {
        return Err("claude_desktop_config.json 不是对象，停止写入".into());
    }
    mode["deploymentMode"] = json!("3p");

    Ok(vec![
        // 顺序有意：先把档写好，再登记，最后才切模式。
        // 反过来的话，Desktop 会短暂指着一个还不存在的档。
        Planned {
            path: profile_path,
            before: profile_before,
            after: Some(profile),
        },
        Planned {
            path: meta_path,
            before: Some(meta_before),
            after: Some(meta),
        },
        Planned {
            path: mode_path,
            before: mode_before,
            after: Some(mode),
        },
    ])
}

pub fn preview(d: &DesktopSpec, ctx: &Ctx) -> Result<Vec<DiffLine>, String> {
    let needles = crate::needles(ctx);
    let mut out = Vec::new();
    for p in plan(d, ctx)? {
        out.push(DiffLine {
            kind: "ctx".into(),
            no: 0,
            text: format!("── {}", name_of(&p.path)),
            managed: true,
        });
        let lines = crate::diff::diff(
            &text_of(p.before.as_ref()),
            &text_of(p.after.as_ref()),
            &needles,
        );
        out.extend(lines);
    }
    Ok(crate::redact(out, ctx.key))
}

pub fn apply(
    s: &spec::AgentSpec,
    d: &DesktopSpec,
    ctx: &Ctx,
    key_id: Option<String>,
) -> Result<crate::AgentInfo, String> {
    let plans = plan(d, ctx)?;

    let mut log = journal::load();
    let prior = log.get(s.id).cloned();

    // 重复注入时**不能**重新记一次「原值」：第二次读到的三个文件里已经有我们
    // 写的内容，照抄下来回滚就回到「已注入」而不是「注入前」。
    // 备份同理，永远保留第一次注入前的那份。
    let reuse = |path: &Path| -> Option<journal::FileState> {
        let key = path.display().to_string();
        prior
            .as_ref()
            .and_then(|p| p.files.iter().find(|f| f.path == key).cloned())
    };

    // 先写日志再落盘：三个文件不可能原子地一起成功，写一半失败时
    // 至少还有「原来长什么样」可回滚。
    let mut files = Vec::new();
    for (i, p) in plans.iter().enumerate() {
        if let Some(f) = reuse(&p.path) {
            files.push(f);
            continue;
        }
        let backup = if p.before.is_some() && p.path.exists() {
            journal::backup_named(s.id, &i.to_string(), &p.path).unwrap_or_default()
        } else {
            String::new()
        };
        files.push(journal::FileState {
            path: p.path.display().to_string(),
            backup,
            before: p.before.clone(),
        });
    }
    log.insert(
        s.id.to_string(),
        journal::Entry {
            // 展示用：这一项改的是整个配置库，不是某一个档位文件。
            // 拿 uuid 当文件名显示对用户没有意义。
            path: spec::desktop_root().display().to_string(),
            backup: files
                .iter()
                .map(|f| f.backup.clone())
                .find(|b| !b.is_empty())
                .unwrap_or_default(),
            applied_at: chrono::Local::now().to_rfc3339(),
            mode: "desktop".into(),
            prev: Vec::new(),
            key_id,
            files,
            // Desktop 只有用户级：3p 配置库是整个应用共享的一份
            scope: "user".into(),
            root: String::new(),
            // 多文件用 files[] 里的 before 判断「原来有没有」，这个标志位用不上
            created: false,
        },
    );
    journal::save(&log);

    for p in &plans {
        match &p.after {
            Some(v) => crate::atomic_write(&p.path, &pretty(v))?,
            None => {
                let _ = std::fs::remove_file(&p.path);
            }
        }
    }
    crate::current(s.id, SUPPORTED)
}

pub fn revert(s: &spec::AgentSpec) -> Result<crate::AgentInfo, String> {
    let mut log = journal::load();
    let Some(entry) = log.get(s.id).cloned() else {
        return Err("没有注入记录，无需回滚".into());
    };
    if entry.files.is_empty() {
        return Err("注入记录里没有文件快照，停止自动回滚（可先备份后手工恢复）".into());
    }

    // 倒序：先撤最后写的（模式开关），再撤索引，最后删档。
    // 这样中途失败时 Desktop 至少不会指着一个只剩半边的档。
    for f in entry.files.iter().rev() {
        let path = PathBuf::from(&f.path);
        match &f.before {
            None => {
                let _ = std::fs::remove_file(&path);
            }
            Some(v) => crate::atomic_write(&path, &restore_text(&f.backup, v))?,
        }
    }

    log.remove(s.id);
    journal::save(&log);
    crate::current(s.id, SUPPORTED)
}

/// 备份文本与「恢复后的内容」语义一致时，直接用备份原文——
/// 连用户的缩进、键顺序、空行都还回去。用户中途改过就只能按结构写回。
fn restore_text(backup: &str, want: &Value) -> String {
    if !backup.is_empty() {
        let path = journal::backups_dir().join(backup);
        if let Ok(text) = std::fs::read_to_string(path) {
            let clean = text.trim_start_matches('\u{feff}');
            if serde_json::from_str::<Value>(clean).ok().as_ref() == Some(want) {
                return text;
            }
        }
    }
    pretty(want)
}

#[cfg(test)]
mod tests {
    use super::*;

    const D: DesktopSpec = DesktopSpec {
        profile_id: "5f2c8a41-7d63-5b90-9e14-3c6a8b02d7f5",
        profile_name: "Freeport",
        marker_header: "X-Freeport-Model-View",
        marker_value: "claude-desktop",
    };

    fn ctx() -> Ctx<'static> {
        Ctx::new("http://127.0.0.1:8787", "fp-test-key", "auto")
    }

    #[test]
    fn valid_ids_follow_the_desktop_shape() {
        assert!(valid_id(&D.profile_id));
        assert!(!valid_id("freeport")); // 名字不是 id
        assert!(!valid_id("5F2C8A41-7D63-5B90-9E14-3C6A8B02D7F5")); // 大写不认
        assert!(!valid_id("5f2c8a41-7d63-5b90-9e14-3c6a8b02d7f")); // 少一位
    }

    #[test]
    fn our_profile_is_recognised_by_provider_plus_marker() {
        assert!(ours(
            &json!({
                "inferenceProvider": "gateway",
                "inferenceCustomHeaders": {"x-freeport-model-view": "claude-desktop"}
            }),
            &D
        ));
        // 少了标记头 → 不是我们的，不能覆盖
        assert!(!ours(&json!({ "inferenceProvider": "gateway" }), &D));
        assert!(!ours(
            &json!({
                "inferenceProvider": "bedrock",
                "inferenceCustomHeaders": {"X-Freeport-Model-View": "claude-desktop"}
            }),
            &D
        ));
    }

    #[test]
    fn meta_rejects_foreign_owner_of_our_id() {
        let meta = json!({
            "entries": [{ "id": D.profile_id, "name": "别家的" }],
            "appliedId": D.profile_id
        });
        let err = validate_meta(&meta, &D).unwrap_err();
        assert!(err.contains("占用"), "{err}");
    }

    #[test]
    fn meta_rejects_dangling_applied_id() {
        let meta = json!({
            "entries": [{ "id": "11111111-1111-5111-8111-111111111111", "name": "Default" }],
            "appliedId": "22222222-2222-5222-8222-222222222222"
        });
        let err = validate_meta(&meta, &D).unwrap_err();
        assert!(err.contains("appliedId"), "{err}");
    }

    #[test]
    fn empty_library_is_acceptable() {
        assert!(validate_meta(&json!({}), &D).is_ok());
    }

    #[test]
    fn restore_prefers_backup_bytes_when_semantics_match() {
        let want = json!({"a": 1});
        assert_eq!(restore_text("", &want), "{\n  \"a\": 1\n}\n");
    }
}
