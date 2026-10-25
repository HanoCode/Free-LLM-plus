//! 双文件策略（Duo）：两个文件各写一半，单独哪一个都生效不了。
//!
//! - **Gemini CLI**：`~/.gemini/.env` 里的网关变量只在 API key 认证下才被读，
//!   `settings.json` 的 `security.auth.selectedType` 必须一起切——只写 .env，
//!   OAuth 用户会「看着注入了，实际还在打 Google」；
//! - **Goose**：`custom_providers/freeport.json` 定义了 provider，
//!   但默认 provider / model 在 `config.yaml` 的 `GOOSE_PROVIDER` / `GOOSE_MODEL`，
//!   少切这一半 Goose 根本不会选我们。
//!
//! 文本类配置（.env、config.yaml）走**带标记的托管块**，与 toml_block 同一套手法：
//! 只占自己那几行；与用户键同名时先注释掉（`#freeport-off:` 前缀），回滚原样还原。
//! 为什么必须注释而不是靠覆盖顺序：dotenv 与 YAML 对同名键谁赢没有统一约定，
//! 留着旧值就是「看着注入了，实际还在打别处」。
//!
//! 多文件的写入/回滚状态记在 `journal::Entry::files`（与 Desktop 共用一套）。

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::spec::{self, DuoKind};
use crate::toml_block::{BEGIN, END};
use crate::{journal, Ctx, DiffLine};

const SUPPORTED: &[&str] = &["openai", "anthropic", "gemini"];

/// 用户同名键的注释前缀。与 toml_block 的 OFF 同义，但那边是私有常量。
const OFF: &str = "#freeport-off: ";

/// 一次计划好的写入。
struct Planned {
    path: PathBuf,
    /// 写入前的原文。`None` = 文件当时不存在（回滚就删掉）。
    before_raw: Option<String>,
    after: String,
}

fn read_text(path: &Path) -> Result<Option<String>, String> {
    match std::fs::read_to_string(path) {
        Ok(t) => Ok(Some(t)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("读取 {} 失败：{e}", path.display())),
    }
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

/* ── 托管块（dotenv / YAML 通用）────────────────── */

/// 这个文件里我们独占的键。upsert 前先注释掉同名行，strip 时还原。
fn managed_keys(kind: DuoKind) -> &'static [&'static str] {
    match kind {
        DuoKind::GeminiCli => &["GOOGLE_GEMINI_BASE_URL", "GEMINI_API_KEY", "GEMINI_MODEL"],
        DuoKind::Goose => &["GOOSE_PROVIDER", "GOOSE_MODEL"],
    }
}

fn line_defines(line: &str, key: &str) -> bool {
    let Some(rest) = line.trim_start().strip_prefix(key) else {
        return false;
    };
    let rest = rest.trim_start();
    // dotenv 用 `=`，YAML 用 `:`
    rest.starts_with('=') || rest.starts_with(':')
}

fn block_body(kind: DuoKind, ctx: &Ctx) -> String {
    match kind {
        // Gemini CLI 对 `{base}/v1beta/...` 发请求，所以这里是不带 /v1 的根地址
        DuoKind::GeminiCli => format!(
            "{BEGIN}\nGOOGLE_GEMINI_BASE_URL={}\nGEMINI_API_KEY={}\nGEMINI_MODEL={}\n{END}",
            ctx.base.trim_end_matches('/'),
            ctx.key,
            ctx.model
        ),
        DuoKind::Goose => format!(
            "{BEGIN}\nGOOSE_PROVIDER: freeport\nGOOSE_MODEL: {}\n{END}",
            ctx.model
        ),
    }
}

fn without_block(text: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut skipping = false;
    for line in text.lines() {
        if line.trim() == BEGIN {
            skipping = true;
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
    out.join("\n")
}

/// 有块就替换，没有就先把同名键注释掉再追加到文件末尾。
fn upsert_block(text: &str, block: &str, keys: &[&str]) -> String {
    let body = without_block(text);
    let mut lines: Vec<String> = body
        .lines()
        .map(|l| {
            if keys.iter().any(|k| line_defines(l, k)) {
                format!("{OFF}{l}")
            } else {
                l.to_string()
            }
        })
        .collect();
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    let mut s = lines.join("\n");
    if !s.is_empty() {
        s.push_str("\n\n");
    }
    s.push_str(block);
    s.push('\n');
    s
}

/// 摘掉托管块，还原被注释的同名键。
///
/// 正式回滚走 `before` 文本快照（字节级），这个函数供测试验证托管块可干净摘除。
#[cfg(test)]
fn strip_block(text: &str) -> String {
    let body = without_block(text);
    let out: Vec<String> = body
        .lines()
        .map(|l| match l.strip_prefix(OFF) {
            Some(rest) => rest.to_string(),
            None => l.to_string(),
        })
        .collect();
    let mut s = out.join("\n");
    while s.ends_with("\n\n") {
        s.truncate(s.len() - 1);
    }
    s
}

/* ── 各自的 plan ─────────────────────────────────── */

fn gemini_plan(ctx: &Ctx) -> Result<Vec<Planned>, String> {
    let dir = home().join(".gemini");
    let env_path = dir.join(".env");
    let settings_path = dir.join("settings.json");

    let env_raw = read_text(&env_path)?;
    let env_after = upsert_block(
        env_raw.as_deref().unwrap_or(""),
        &block_body(DuoKind::GeminiCli, ctx),
        managed_keys(DuoKind::GeminiCli),
    );

    // settings.json：认证开关 + 默认模型。非 JSON 就停手——那是用户手工维护的文件。
    let settings_raw = read_text(&settings_path)?;
    let mut doc: Value = match settings_raw.as_deref() {
        None | Some("") => json!({}),
        Some(t) if t.trim().is_empty() => json!({}),
        Some(t) => serde_json::from_str(t)
            .map_err(|_| format!("{} 不是合法 JSON，停止写入", name_of(&settings_path)))?,
    };
    doc["security"]["auth"]["selectedType"] = json!("gemini-api-key");
    doc["model"]["name"] = json!(ctx.model);

    Ok(vec![
        // 顺序有意：先写 .env 再切认证。切过去的那一刻变量必须已经在。
        Planned {
            path: env_path,
            before_raw: env_raw,
            after: env_after,
        },
        Planned {
            path: settings_path,
            before_raw: settings_raw,
            after: pretty(&doc),
        },
    ])
}

/// Goose 的配置根目录（mac / Linux `~/.config/goose`，Windows 在 APPDATA 下）
/// 定义在 spec.rs，检测与写入必须用同一个。
use crate::spec::goose_dir;

fn goose_plan(ctx: &Ctx) -> Result<Vec<Planned>, String> {
    let dir = goose_dir();
    let provider_path = dir.join("custom_providers/freeport.json");
    let config_path = dir.join("config.yaml");

    // 文件名就是认领：freeport.json 里装的不是名叫 freeport 的 provider，
    // 说明用户自己建过同名文件，覆盖等于毁掉别人的配置。
    let provider_raw = read_text(&provider_path)?;
    if let Some(t) = provider_raw.as_deref() {
        if !t.trim().is_empty() {
            let v: Value = serde_json::from_str(t)
                .map_err(|_| format!("{} 不是合法 JSON，停止写入", name_of(&provider_path)))?;
            if v.get("name").and_then(Value::as_str) != Some("freeport") {
                return Err(
                    "custom_providers/freeport.json 已被别的 provider 占用，先改名或删除再注入"
                        .into(),
                );
            }
        }
    }

    // models 只供上下文窗口数字，Goose 仍会自己拉 /v1/models 补全目录；
    // 目录快照是空的（调用方没带）就退化为当前选中的那一个。
    let models: Vec<Value> = if ctx.models.is_empty() {
        vec![json!({ "name": ctx.model })]
    } else {
        ctx.models
            .iter()
            .map(|m| json!({ "name": m.id, "context_limit": m.context }))
            .collect()
    };
    let provider_after = pretty(&json!({
        "name": "freeport",
        "engine": "openai",
        "display_name": "Freeport",
        // Goose 的 openai 引擎把 base_url 当完整端点用，不带 /chat/completions 会 404
        "base_url": format!("{}/v1/chat/completions", ctx.base.trim_end_matches('/')),
        // Goose 要求密钥放 shell 环境变量（api_key_env），我们不动用户的 shell 配置，
        // 改走自定义请求头——与 Codex 托管块同一条路
        "headers": { "Authorization": format!("Bearer {}", ctx.key) },
        "models": models,
    }));

    let config_raw = read_text(&config_path)?;
    let config_after = upsert_block(
        config_raw.as_deref().unwrap_or(""),
        &block_body(DuoKind::Goose, ctx),
        managed_keys(DuoKind::Goose),
    );
    validate_config(&config_after)?;

    Ok(vec![
        // 先落 provider 文件再切默认值：切过去的那一刻 provider 必须已经在。
        Planned {
            path: provider_path,
            before_raw: provider_raw,
            after: provider_after,
        },
        Planned {
            path: config_path,
            before_raw: config_raw,
            after: config_after,
        },
    ])
}

/// 写完的 config.yaml 必须真的指到 freeport，且没有未注释的同名竞争键。
fn validate_config(text: &str) -> Result<(), String> {
    if !(text.contains(BEGIN) && text.contains(END)) {
        return Err("config.yaml 缺少托管块".into());
    }
    for l in text.lines() {
        if l.starts_with(OFF) || l.trim_start().starts_with('#') {
            continue;
        }
        if line_defines(l, "GOOSE_PROVIDER") && !l.contains("freeport") {
            return Err("GOOSE_PROVIDER 仍有指向别处的未注释行".into());
        }
    }
    Ok(())
}

fn plan(kind: DuoKind, ctx: &Ctx) -> Result<Vec<Planned>, String> {
    match kind {
        DuoKind::GeminiCli => gemini_plan(ctx),
        DuoKind::Goose => goose_plan(ctx),
    }
}

/* ── 预览 / 写入 / 回滚 ──────────────────────────── */

pub fn preview(kind: DuoKind, ctx: &Ctx) -> Result<Vec<DiffLine>, String> {
    let needles = crate::needles(ctx);
    let mut out = Vec::new();
    for p in plan(kind, ctx)? {
        out.push(DiffLine {
            kind: "ctx".into(),
            no: 0,
            text: format!("── {}", name_of(&p.path)),
            managed: true,
        });
        out.extend(crate::diff::diff(
            p.before_raw.as_deref().unwrap_or(""),
            &p.after,
            &needles,
        ));
    }
    Ok(crate::redact(out, ctx.key))
}

pub fn apply(
    s: &spec::AgentSpec,
    kind: DuoKind,
    ctx: &Ctx,
    key_id: Option<String>,
) -> Result<crate::AgentInfo, String> {
    // 探测是「装没装」的唯一判据：没装就去猜路径，猜错只会静默写进没人读的文件
    if spec::resolve_path(s).is_none() {
        return Err(format!("没有检测到 {}", s.name));
    }
    let plans = plan(kind, ctx)?;

    let mut log = journal::load();
    let prior = log.get(s.id).cloned();
    // 重复注入沿用第一次的原值与备份：第二次读到的文件里已经有我们的块
    let reuse = |path: &Path| -> Option<journal::FileState> {
        let key = path.display().to_string();
        prior
            .as_ref()
            .and_then(|p| p.files.iter().find(|f| f.path == key).cloned())
    };

    // 先记日志再落盘：两个文件不可能原子地一起成功，写一半失败时至少还有原样可回滚
    let mut files = Vec::new();
    for (i, p) in plans.iter().enumerate() {
        if let Some(f) = reuse(&p.path) {
            files.push(f);
            continue;
        }
        let backup = if p.before_raw.is_some() {
            journal::backup_named(s.id, &i.to_string(), &p.path).unwrap_or_default()
        } else {
            String::new()
        };
        files.push(journal::FileState {
            path: p.path.display().to_string(),
            backup,
            // 文本快照存成 JSON 字符串；回滚按字节还原
            before: p.before_raw.clone().map(Value::String),
        });
    }
    log.insert(
        s.id.to_string(),
        journal::Entry {
            path: plans[0].path.display().to_string(),
            backup: files
                .iter()
                .map(|f| f.backup.clone())
                .find(|b| !b.is_empty())
                .unwrap_or_default(),
            applied_at: chrono::Local::now().to_rfc3339(),
            mode: "duo".into(),
            prev: Vec::new(),
            key_id,
            files,
            // 两个落点都在用户主目录下，没有项目级形态
            scope: "user".into(),
            root: String::new(),
            created: false,
        },
    );
    journal::save(&log);

    for p in &plans {
        crate::atomic_write(&p.path, &p.after)?;
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

    // 倒序：先撤认证开关/默认 provider，再撤变量与 provider 文件。
    // 中途失败时不会停在「开关切了、地址没切」的半开状态。
    for f in entry.files.iter().rev() {
        let path = PathBuf::from(&f.path);
        match &f.before {
            None => {
                let _ = std::fs::remove_file(&path);
            }
            Some(Value::String(t)) => crate::atomic_write(&path, t)?,
            Some(_) => {
                return Err(format!(
                    "{} 的快照不是文本，停止自动回滚（可从备份手工恢复）",
                    path.display()
                ))
            }
        }
    }

    log.remove(s.id);
    journal::save(&log);
    crate::current(s.id, SUPPORTED)
}

fn home() -> PathBuf {
    if let Ok(h) = std::env::var("FREEPORT_HOME") {
        return PathBuf::from(h);
    }
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("~"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> Ctx<'static> {
        Ctx::new("http://127.0.0.1:8787", "fp-test-key", "auto")
    }

    #[test]
    fn env_block_upsert_is_idempotent_and_sweeps_conflicts() {
        let src = "GOOGLE_GEMINI_BASE_URL=https://api.example.com\nCUSTOM=1\n";
        let block = block_body(DuoKind::GeminiCli, &ctx());
        let once = upsert_block(src, &block, managed_keys(DuoKind::GeminiCli));
        assert!(once.contains("GOOGLE_GEMINI_BASE_URL=http://127.0.0.1:8787"));
        assert!(once.contains("CUSTOM=1"), "无关变量不许动");
        assert!(
            once.contains(&format!("{OFF}GOOGLE_GEMINI_BASE_URL=https://api.example.com")),
            "同名旧键要注释掉"
        );
        let twice = upsert_block(&once, &block, managed_keys(DuoKind::GeminiCli));
        assert_eq!(once, twice, "重复注入不应叠加");
    }

    #[test]
    fn strip_restores_commented_keys() {
        let src = "GOOSE_PROVIDER: openai\nGOOSE_MODEL: kimi\nother: 1\n";
        let block = block_body(DuoKind::Goose, &ctx());
        let once = upsert_block(src, &block, managed_keys(DuoKind::Goose));
        let back = strip_block(&once);
        assert!(back.contains("GOOSE_PROVIDER: openai"));
        assert!(back.contains("other: 1"));
        assert!(!back.contains(BEGIN));
        assert!(!back.contains(OFF));
    }

    #[test]
    fn yaml_validation_rejects_foreign_provider_line() {
        let block = block_body(DuoKind::Goose, &ctx());
        let bad = upsert_block(
            "GOOSE_PROVIDER: openai\n",
            &block,
            managed_keys(DuoKind::Goose),
        )
        .replace(&format!("{OFF}GOOSE_PROVIDER: openai\n"), "GOOSE_PROVIDER: openai\n");
        assert!(validate_config(&bad).is_err());
        let good = upsert_block("x: 1\n", &block, managed_keys(DuoKind::Goose));
        validate_config(&good).unwrap();
    }
}
