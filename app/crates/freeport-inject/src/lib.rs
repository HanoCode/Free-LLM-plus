//! 把本机 Agent 指向 Freeport。
//!
//! 四条不可让渡的规则（每一条都有对应的失败案例）：
//! 1. **只写自己的节点**：JSON 按键路径写、TOML 写托管块，绝不做整对象合并
//! 2. **写前必备份**：原文件复制到 `backups/`，路径记进日志
//! 3. **幂等**：重复注入不会叠加，第二次写出的字节和第一次一样
//! 4. **可回滚**：JSON 恢复旧值（原来没有的就删掉），TOML 摘掉托管块

pub mod desktop;
pub mod diff;
pub mod duo;
pub mod envs;
pub mod journal;
pub mod json_edit;
pub mod push;
pub mod spec;
pub mod toml_block;

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{json, Value};

pub use diff::DiffLine;
pub use spec::{Discovery, Protocol, PushSpec, Strategy};

/// 注入时能告诉客户端的模型信息。
///
/// 刻意不复用目录的 `ModelInfo`：注入器只关心「客户端要展示、要判断的」，
/// 上游目录加字段不该逼着改这里。
#[derive(Debug, Clone)]
pub struct ModelCard {
    /// 客户端请求时填的 model 名（网关认的那个）
    pub id: String,
    /// 展示名。空则用 id
    pub label: String,
    /// 上下文窗口（token）
    pub context: u32,
    pub max_output: u32,
    pub tools: bool,
    pub vision: bool,
    pub reasoning: bool,
}

/// 渲染模板用的上下文。
pub struct Ctx<'a> {
    /// 网关根地址，不带 `/v1`
    pub base: &'a str,
    /// 给这个 Agent 单独签发的 key
    pub key: &'a str,
    pub model: &'a str,
    /// 模型目录快照。只有 Push 模式用得到，其余模式留空即可。
    pub models: Vec<ModelCard>,
}

impl<'a> Ctx<'a> {
    pub fn new(base: &'a str, key: &'a str, model: &'a str) -> Self {
        Self {
            base,
            key,
            model,
            models: Vec::new(),
        }
    }

    /// 带上目录。Push 模式的客户端不会自己去问网关要模型表，只能我们写进去。
    pub fn with_models(mut self, models: Vec<ModelCard>) -> Self {
        self.models = models;
        self
    }

    fn base_v1(&self) -> String {
        format!("{}/v1", self.base.trim_end_matches('/'))
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentInfo {
    pub id: String,
    pub name: String,
    pub protocol: String,
    /// self | pull | push | host —— 模型表从哪来
    pub discovery: String,
    /// json | toml | push | guide
    pub strategy: String,
    /// injected | detected | missing | guide | unsupported
    pub state: String,
    pub path: Option<String>,
    pub note: String,
    pub backup: Option<String>,
    pub applied_at: Option<String>,
    /// 项目级落点（相对仓库根）。`None` = 这个 Agent 不支持项目级注入
    pub project_path: Option<String>,
}

/// 注入作用域。
///
/// 用户级写的是「这台机器上所有项目都生效」的配置；项目级写的是**那一个仓库**
/// 里的一份文件，删库即消失、换机器即在。两者的回滚记录不能混在一起：
/// 同名 Agent 在两个仓库里各注一次，回滚一个不能把另一个也撤了。
#[derive(Debug, Clone, Copy)]
pub enum Scope<'a> {
    User,
    Project(&'a Path),
}

/// 列出全部 Agent 的当前状态。
///
/// `supported` 是网关此刻真能说出口的协议；不支持的标 `unsupported`，
/// 绝不让界面显示「已注入」却连不上。
pub fn list(supported: &[&str]) -> Vec<AgentInfo> {
    let log = journal::load();
    spec::AGENTS
        .iter()
        .map(|s| info_of(s, &log, supported))
        .collect()
}

pub fn current(id: &str, supported: &[&str]) -> Result<AgentInfo, String> {
    let s = spec::find(id).ok_or_else(|| format!("未知 Agent：{id}"))?;
    Ok(info_of(s, &journal::load(), supported))
}

fn strategy_str(s: &spec::AgentSpec) -> &'static str {
    match s.strategy {
        Strategy::JsonKeys(_) => "json",
        Strategy::TomlBlock => "toml",
        Strategy::Push(_) => "push",
        Strategy::EnvList(_) => "env",
        Strategy::AcpEnv(_) => "acp",
        Strategy::Desktop(_) => "desktop",
        Strategy::Duo(_) => "duo",
        Strategy::Guide => "guide",
    }
}

/// 只填与作用域无关的字段，状态与落点由调用方按作用域补。
fn info_base(s: &spec::AgentSpec) -> AgentInfo {
    AgentInfo {
        id: s.id.into(),
        name: s.name.into(),
        protocol: s.protocol.as_str().into(),
        discovery: s.discovery.as_str().into(),
        strategy: strategy_str(s).into(),
        state: String::new(),
        path: None,
        note: s.note.into(),
        backup: None,
        applied_at: None,
        project_path: spec::project_rel(s.id).map(str::to_string),
    }
}

fn info_of(
    s: &spec::AgentSpec,
    log: &std::collections::BTreeMap<String, journal::Entry>,
    supported: &[&str],
) -> AgentInfo {
    let path = spec::resolve_path(s);
    let entry = log.get(s.id);
    let state = if !supported.contains(&s.protocol.as_str()) {
        "unsupported"
    } else if matches!(s.strategy, Strategy::Guide) {
        "guide"
    } else if entry.is_some() {
        "injected"
    } else if path.is_some() {
        "detected"
    } else {
        "missing"
    };
    let mut i = info_base(s);
    i.state = state.into();
    i.path = entry
        .map(|e| e.path.clone())
        .or_else(|| path.map(|p| p.display().to_string()));
    i.backup = entry.map(|e| e.backup.clone());
    i.applied_at = entry.map(|e| e.applied_at.clone());
    i
}

/// 日志键。项目级必须带仓库根：同一个 Agent 在两个仓库各注一次，
/// 共用一个键就会让「回滚这个仓库」把那个仓库的也撤掉。
fn log_key(id: &str, scope: Scope<'_>) -> String {
    match scope {
        Scope::User => id.to_string(),
        Scope::Project(root) => format!("{id}@{}", root.display()),
    }
}

/// 备份文件名里能用的 agent 标识。
///
/// 不能直接拿日志键：项目级的键是 `id@<仓库绝对路径>`，斜杠会把备份
/// 写成 `backups/claude-code@/Users/x/repo-….bak` 这种子目录，`backup_named`
/// 直接失败——于是「项目级注入」永远卡在备份这一步。
fn backup_tag(id: &str, scope: Scope<'_>) -> String {
    match scope {
        Scope::User => id.to_string(),
        Scope::Project(root) => {
            let name = root
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            format!("{id}-{name}-{}", short_hash(&root.display().to_string()))
        }
    }
}

fn short_hash(s: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    format!("{:08x}", (h.finish() & 0xffff_ffff) as u32)
}

/// 解析项目级落点。仓库根必须真实存在——路径写错会造出一棵谁也不会读的目录树。
fn project_path(s: &spec::AgentSpec, root: &Path) -> Result<PathBuf, String> {
    let rel = spec::project_rel(s.id)
        .ok_or_else(|| format!("{} 没有可安全代写的项目级配置，只能写用户级", s.name))?;
    if !root.is_dir() {
        return Err(format!("项目目录不存在：{}", root.display()));
    }
    Ok(root.join(rel))
}

/// 这个仓库里注入了哪些 Agent（给界面列已注入项用）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectInjection {
    pub id: String,
    pub name: String,
    pub path: String,
    pub applied_at: String,
}

pub fn project_list(root: &Path) -> Vec<ProjectInjection> {
    let log = journal::load();
    spec::AGENTS
        .iter()
        .filter(|s| spec::project_rel(s.id).is_some())
        .filter_map(|s| {
            let e = log.get(&log_key(s.id, Scope::Project(root)))?;
            Some(ProjectInjection {
                id: s.id.into(),
                name: s.name.into(),
                path: e.path.clone(),
                applied_at: e.applied_at.clone(),
            })
        })
        .collect()
}

/// 预览：算出「如果现在注入，会变成什么样」。不落盘。
pub fn preview(id: &str, ctx: &Ctx) -> Result<Vec<DiffLine>, String> {
    preview_scoped(id, Scope::User, ctx)
}

pub fn preview_scoped(id: &str, scope: Scope<'_>, ctx: &Ctx) -> Result<Vec<DiffLine>, String> {
    let spec = spec::find(id).ok_or_else(|| format!("未知 Agent：{id}"))?;
    if let Strategy::Desktop(d) = spec.strategy {
        return desktop::preview(d, ctx);
    }
    if let Strategy::Duo(k) = spec.strategy {
        return duo::preview(k, ctx);
    }
    let (s, path, old) = prepare(id, scope)?;
    let (new, _) = render(s, &path, &old, ctx)?;
    let n = needles(ctx);
    Ok(redact(diff::diff(&old.text, &new, &n), ctx.key))
}

/// 写入（用户级）。返回写完后的状态，UI 用它刷新那一行。
pub fn apply(id: &str, ctx: &Ctx, key_id: Option<String>) -> Result<AgentInfo, String> {
    apply_scoped(id, Scope::User, ctx, key_id)
}

/// 写入（按作用域）。项目级写的只是那个仓库里的一份文件。
pub fn apply_scoped(
    id: &str,
    scope: Scope<'_>,
    ctx: &Ctx,
    key_id: Option<String>,
) -> Result<AgentInfo, String> {
    let spec = spec::find(id).ok_or_else(|| format!("未知 Agent：{id}"))?;
    // Desktop 是三个文件互相引用，走不了「读一个文件、算一份新文本」这条路
    if let Strategy::Desktop(d) = spec.strategy {
        if matches!(scope, Scope::Project(_)) {
            return Err("Claude Desktop 只有用户级配置，没有项目级落点".into());
        }
        return desktop::apply(spec, d, ctx, key_id);
    }
    // Duo 同 Desktop：两个文件互相成就，走不了「读一个文件、算一份新文本」的单文件管线
    if let Strategy::Duo(k) = spec.strategy {
        if matches!(scope, Scope::Project(_)) {
            return Err("这个 Agent 只有用户级配置，没有项目级落点".into());
        }
        return duo::apply(spec, k, ctx, key_id);
    }
    let (s, path, mut old) = prepare(id, scope)?;
    let (new, prev) = render(s, &path, &old, ctx)?;
    old.prev = prev;

    // 写前校验：新内容必须能被目标格式解析
    match s.strategy {
        Strategy::JsonKeys(_) | Strategy::Push(_) | Strategy::EnvList(_) | Strategy::AcpEnv(_) => {
            serde_json::from_str::<Value>(&new).map_err(|e| format!("生成的 JSON 非法：{e}"))?;
        }
        Strategy::TomlBlock => toml_block::validate(&new)?,
        Strategy::Desktop(_) => return Err("Desktop 走多文件路径，不该到这里".into()),
        Strategy::Duo(_) => return Err("Duo 走多文件路径，不该到这里".into()),
        Strategy::Guide => return Err("这个 Agent 只提供手工步骤".into()),
    }

    let mut log = journal::load();
    let prior = log.get(&log_key(id, scope)).cloned();

    // 关键：重复注入时**不能**重新记一次「原值」。
    // 第二次读到的文件里已经有我们写的键，照抄下来回滚就还原不回去了。
    // 备份同理，永远保留第一次注入前的那份。
    let backup = match &prior {
        Some(p) if !p.backup.is_empty() => p.backup.clone(),
        _ if old.existed => journal::backup(&backup_tag(id, scope), &path)?,
        _ => String::new(),
    };
    let prev = merge_prev(
        prior.as_ref().map(|p| p.prev.clone()).unwrap_or_default(),
        old.prev,
    );

    atomic_write(&path, &new)?;

    log.insert(
        log_key(id, scope),
        journal::Entry {
            path: path.display().to_string(),
            backup,
            applied_at: chrono::Local::now().to_rfc3339(),
            mode: strategy_str(s).into(),
            prev,
            key_id,
            files: Vec::new(),
            scope: match scope {
                Scope::User => "user".into(),
                Scope::Project(_) => "project".into(),
            },
            root: match scope {
                Scope::User => String::new(),
                Scope::Project(root) => root.display().to_string(),
            },
            // 重复注入时沿用第一次的判断：文件是我们建的，就一直算我们建的
            created: prior.as_ref().map(|p| p.created).unwrap_or(!old.existed),
        },
    );
    journal::save(&log);
    Ok(settled(id, scope))
}

/// 写完之后那一行的状态：用户级看全局列表，项目级看这个仓库。
fn settled(id: &str, scope: Scope<'_>) -> AgentInfo {
    match scope {
        Scope::User => current(id, &["openai", "anthropic", "gemini"]),
        Scope::Project(root) => project_info(id, root),
    }
    .unwrap_or_else(|e| {
        let mut i = info_base(spec::find(id).expect("调用方已经查过存在"));
        i.state = format!("error: {e}");
        i
    })
}

/// 项目级当前状态：只反映这个仓库里的注入情况，不掺用户级。
pub fn project_info(id: &str, root: &Path) -> Result<AgentInfo, String> {
    let s = spec::find(id).ok_or_else(|| format!("未知 Agent：{id}"))?;
    let path = project_path(s, root)?;
    let entry = journal::load().get(&log_key(id, Scope::Project(root))).cloned();
    let mut i = info_base(s);
    i.state = if entry.is_some() {
        "injected"
    } else if path.exists() {
        "detected"
    } else {
        "missing"
    }
    .into();
    i.path = entry
        .as_ref()
        .map(|e| e.path.clone())
        .or_else(|| Some(path.display().to_string()));
    i.backup = entry.as_ref().map(|e| e.backup.clone());
    i.applied_at = entry.as_ref().map(|e| e.applied_at.clone());
    Ok(i)
}

/// 回滚（用户级）。JSON 逐节点恢复，TOML 摘托管块。
pub fn revert(id: &str) -> Result<AgentInfo, String> {
    revert_scoped(id, Scope::User)
}

pub fn revert_scoped(id: &str, scope: Scope<'_>) -> Result<AgentInfo, String> {
    let s = spec::find(id).ok_or_else(|| format!("未知 Agent：{id}"))?;
    if let Strategy::Desktop(_) = s.strategy {
        return desktop::revert(s);
    }
    if let Strategy::Duo(_) = s.strategy {
        return duo::revert(s);
    }
    let key = log_key(id, scope);
    let mut log = journal::load();
    let Some(entry) = log.get(&key).cloned() else {
        return Err("没有注入记录，无需回滚".into());
    };
    let path = PathBuf::from(&entry.path);
    if !path.exists() {
        log.remove(&key);
        journal::save(&log);
        return Ok(settled(id, scope));
    }

    let text = std::fs::read_to_string(&path).map_err(|e| format!("读取失败：{e}"))?;

    match s.strategy {
        Strategy::TomlBlock => {
            let back = toml_block::strip(&text);
            if entry.created && back.trim().is_empty() {
                let _ = std::fs::remove_file(&path);
            } else {
                atomic_write(&path, &back)?;
            }
        }
        Strategy::JsonKeys(_) | Strategy::Push(_) | Strategy::EnvList(_) | Strategy::AcpEnv(_) => {
            let mut doc: Value = serde_json::from_str(&text)
                .map_err(|_| "配置文件已不是合法 JSON，停止自动回滚".to_string())?;
            // 倒序恢复：先写的后撤，父子关系不会互相踩
            for p in entry.prev.iter().rev() {
                if p.ptr.is_empty() {
                    // 空指针 = 整份快照。Push 写的是数组，逐节点撤不掉被删掉的模型。
                    doc = p.value.clone();
                } else if p.value.is_null() {
                    json_edit::remove(&mut doc, &p.ptr);
                } else {
                    json_edit::set(&mut doc, &p.ptr, p.value.clone());
                }
            }
            // 这个文件本来没有、撤完也没剩下别人的东西 → 直接删掉。
            // 留一个 `{}` 在仓库里，用户会以为是我们塞的垃圾。
            if entry.created && is_empty_doc(&doc) {
                let _ = std::fs::remove_file(&path);
                log.remove(&key);
                journal::save(&log);
                return Ok(settled(id, scope));
            }
            let mut out = serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?;
            out.push('\n');
            // 如果能确认「除了我们写的节点，其余和备份一致」，就直接还原备份的字节，
            // 连缩进和空行都还回去；用户中途改过（语义不等）才退回格式化写回。
            if let Some(exact) = restore_backup_bytes(&entry.backup, &doc) {
                atomic_write(&path, &exact)?;
            } else {
                atomic_write(&path, &out)?;
            }
        }
        Strategy::Desktop(_) => return Err("Desktop 走多文件路径，不该到这里".into()),
        Strategy::Duo(_) => return Err("Duo 走多文件路径，不该到这里".into()),
        Strategy::Guide => return Err("这个 Agent 没有被写入过".into()),
    }

    log.remove(&key);
    journal::save(&log);
    Ok(settled(id, scope))
}

/* ── 内部 ─────────────────────────────────────── */

struct Old {
    text: String,
    existed: bool,
    prev: Vec<journal::Prev>,
}

fn prepare(id: &str, scope: Scope<'_>) -> Result<(&'static spec::AgentSpec, PathBuf, Old), String> {
    let s = spec::find(id).ok_or_else(|| format!("未知 Agent：{id}"))?;
    if matches!(s.strategy, Strategy::Guide) {
        return Err(format!("{} 只提供手工步骤", s.name));
    }
    let path = match scope {
        // 用户级：装在哪写在哪，装不装得上由探测决定
        Scope::User => spec::resolve_path(s).ok_or_else(|| format!("没有检测到 {}", s.name))?,
        // 项目级：落点由仓库根 + 相对路径算出，跟本机装没装无关
        Scope::Project(root) => project_path(s, root)?,
    };
    let existed = path.exists();
    let text = if existed {
        std::fs::read_to_string(&path).map_err(|e| format!("读取 {} 失败：{e}", path.display()))?
    } else {
        String::new()
    };
    Ok((
        s,
        path,
        Old {
            text,
            existed,
            prev: Vec::new(),
        },
    ))
}

fn render(
    s: &spec::AgentSpec,
    _path: &Path,
    old: &Old,
    ctx: &Ctx,
) -> Result<(String, Vec<journal::Prev>), String> {
    match s.strategy {
        Strategy::JsonKeys(keys) => {
            let mut doc: Value = if old.text.trim().is_empty() {
                json!({})
            } else {
                serde_json::from_str(&old.text).map_err(|_| {
                    "目标文件不是合法 JSON，为免写坏已停止（可先手工整理格式）".to_string()
                })?
            };
            let mut prev = Vec::new();
            // 先清扫再写：同一前缀下的旧键留着，客户端可能按自己的优先级取到它，
            // 结果就是「看起来注入了，实际还在打别处」
            if let Some(rule) = s.prune {
                sweep(&mut doc, rule, keys, &mut prev);
            }
            for k in keys {
                let before =
                    json_edit::set(&mut doc, k.ptr, value_of(k.value, ctx)).unwrap_or(Value::Null);
                prev.push(journal::Prev {
                    ptr: k.ptr.to_string(),
                    value: before,
                });
            }
            let mut out = serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?;
            out.push('\n');
            Ok((out, prev))
        }
        Strategy::TomlBlock => {
            let block = toml_block::block(&ctx.base_v1(), ctx.key, ctx.model);
            Ok((toml_block::upsert(&old.text, &block), Vec::new()))
        }
        // Push 自己带一套「按目录展开」的逻辑，回滚记录也不一样（整份快照）
        Strategy::Push(p) => push::upsert(&old.text, p, ctx),
        Strategy::EnvList(keys) => envs::vscode_extension(&old.text, keys, ctx),
        Strategy::AcpEnv(keys) => envs::acp_servers(&old.text, keys, ctx, spec::ACP_MARK),
        Strategy::Desktop(_) => Err("Desktop 走多文件路径，不该到这里".into()),
        Strategy::Duo(_) => Err("Duo 走多文件路径，不该到这里".into()),
        Strategy::Guide => Err("这个 Agent 只提供手工步骤".into()),
    }
}

pub(crate) fn value_of(src: spec::Src, ctx: &Ctx) -> Value {
    match src {
        spec::Src::Base => json!(ctx.base.trim_end_matches('/')),
        spec::Src::BaseV1 => json!(ctx.base_v1()),
        spec::Src::Key => json!(ctx.key),
        spec::Src::Model => json!(ctx.model),
        spec::Src::Literal(s) => json!(s),
        spec::Src::Bool(b) => json!(b),
    }
}

/// 按 [`spec::Prune`] 规则删掉旧键，旧值进回滚日志。
///
/// `keep` 是本次要写的指针集合：它们不参与清扫，否则预览里会先删一行再加一行，
/// 看起来像我们动了两下。
fn sweep(
    doc: &mut Value,
    rule: &spec::Prune,
    keep: &[spec::KeySpec],
    prev: &mut Vec<journal::Prev>,
) {
    let at = rule.at.trim_end_matches('/');
    let Some(parent) = parent_mut(doc, rule.at) else {
        return;
    };
    let doomed: Vec<String> = parent
        .keys()
        .filter(|k| {
            let ptr = format!("{at}/{k}");
            !keep.iter().any(|x| x.ptr == ptr.as_str())
                && (rule.prefixes.iter().any(|p| k.starts_with(p))
                    || rule.keys.contains(&k.as_str()))
        })
        .cloned()
        .collect();
    for k in doomed {
        if let Some(v) = parent.remove(&k) {
            prev.push(journal::Prev {
                ptr: format!("{at}/{k}"),
                value: v,
            });
        }
    }
}

/// 走到 `at` 指向的对象；中途不存在就什么都不做。
fn parent_mut<'v>(doc: &'v mut Value, at: &str) -> Option<&'v mut serde_json::Map<String, Value>> {
    let mut cur = doc;
    for seg in at.split('/').filter(|s| !s.is_empty()) {
        cur = cur.get_mut(seg)?;
    }
    cur.as_object_mut()
}

/// diff 的「这是 Freeport 写的」判据。
pub(crate) fn needles<'a>(ctx: &'a Ctx) -> Vec<&'a str> {
    vec!["freeport", ctx.base, "Bearer "]
}

/// diff 里不能出现明文 key：UI 要展示，但一眼扫屏幕的人不该看到它。
pub(crate) fn redact(mut lines: Vec<DiffLine>, key: &str) -> Vec<DiffLine> {
    if key.len() < 8 {
        return lines;
    }
    let masked = format!("{}…", &key[..8]);
    for l in lines.iter_mut() {
        if l.text.contains(key) {
            l.text = l.text.replace(key, &masked);
        }
    }
    lines
}

/// 恢复后什么都不剩：空对象或空数组。
fn is_empty_doc(v: &Value) -> bool {
    match v {
        Value::Object(m) => m.is_empty(),
        Value::Array(a) => a.is_empty(),
        _ => false,
    }
}

/// 合并历史原值：已经记过的指针保留最早的，新增指针追加。
fn merge_prev(mut prior: Vec<journal::Prev>, fresh: Vec<journal::Prev>) -> Vec<journal::Prev> {
    for f in fresh {
        if !prior.iter().any(|p| p.ptr == f.ptr) {
            prior.push(f);
        }
    }
    prior
}

/// 备份还在、且备份内容与「恢复后的节点树」语义一致时，返回备份原文。
///
/// 这样回滚是**字节级**的：用户原来的缩进、键顺序、空行都不会被我们的
/// 序列化器改掉。语义不一致说明用户后来又动过，那就只能按当前结构写回。
fn restore_backup_bytes(backup_name: &str, restored: &Value) -> Option<String> {
    if backup_name.is_empty() {
        return None;
    }
    let path = journal::backups_dir().join(backup_name);
    let text = std::fs::read_to_string(path).ok()?;
    let parsed: Value = serde_json::from_str(&text).ok()?;
    (parsed == *restored).then_some(text)
}

/// 临时文件 + rename：中途失败也不会留下半个配置文件。
pub(crate) fn atomic_write(path: &Path, text: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建目录失败：{e}"))?;
    }
    let tmp = path.with_extension(format!(
        "{}.tmp",
        path.extension().and_then(|x| x.to_str()).unwrap_or("")
    ));
    std::fs::write(&tmp, text).map_err(|e| format!("写入临时文件失败：{e}"))?;

    // 保留原文件权限（settings.json 常见 0600）
    #[cfg(unix)]
    if let Ok(meta) = std::fs::metadata(path) {
        let _ = std::fs::set_permissions(&tmp, meta.permissions());
    }

    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("替换文件失败：{e}")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claude_ctx() -> Ctx<'static> {
        Ctx::new("http://127.0.0.1:8787", "fp-test-key", "auto")
    }

    fn render_claude(text: &str) -> (String, Vec<journal::Prev>) {
        let s = spec::find("claude-code").expect("claude-code 必须在册");
        let old = Old {
            text: text.into(),
            existed: !text.is_empty(),
            prev: Vec::new(),
        };
        render(s, Path::new("/tmp/settings.json"), &old, &claude_ctx()).expect("渲染不该失败")
    }

    #[test]
    fn claude_injects_every_switch() {
        let (out, _) = render_claude("");
        let doc: Value = serde_json::from_str(&out).unwrap();
        let env = &doc["env"];
        assert_eq!(env["ANTHROPIC_BASE_URL"], "http://127.0.0.1:8787");
        assert_eq!(env["ANTHROPIC_AUTH_TOKEN"], "fp-test-key");
        // 三档都要有值：缺哪一档，那一档的内部任务就会去要原厂模型名
        for k in [
            "ANTHROPIC_MODEL",
            "ANTHROPIC_DEFAULT_SONNET_MODEL",
            "ANTHROPIC_DEFAULT_OPUS_MODEL",
            "ANTHROPIC_DEFAULT_HAIKU_MODEL",
        ] {
            assert_eq!(env[k], "auto", "{k} 缺失会让那一档打空");
        }
        assert_eq!(env["CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY"], "1");
        assert_eq!(env["CLAUDE_CODE_AUTO_MODE_SERVER"], "0");
    }

    #[test]
    fn old_anthropic_keys_are_swept_and_recorded() {
        let old = r#"{"env":{"ANTHROPIC_API_KEY":"sk-old","ANTHROPIC_BASE_URL":"https://api.anthropic.com","CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC":"1","KEEP_ME":"1"},"permissions":{"allow":[]}}"#;
        let (out, prev) = render_claude(old);
        let doc: Value = serde_json::from_str(&out).unwrap();
        assert!(
            doc["env"].get("ANTHROPIC_API_KEY").is_none(),
            "旧的 API_KEY 会和 AUTH_TOKEN 打架"
        );
        assert_eq!(doc["env"]["ANTHROPIC_BASE_URL"], "http://127.0.0.1:8787");
        assert!(
            doc["env"]
                .get("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC")
                .is_none(),
            "它会连网关模型发现一起压掉"
        );
        assert_eq!(doc["env"]["KEEP_ME"], "1", "无关变量不许动");
        assert_eq!(doc["permissions"]["allow"], json!([]));

        let rec = prev
            .iter()
            .find(|p| p.ptr == "/env/ANTHROPIC_API_KEY")
            .expect("清掉的旧值必须进回滚日志");
        assert_eq!(rec.value, "sk-old");
        let base = prev
            .iter()
            .find(|p| p.ptr == "/env/ANTHROPIC_BASE_URL")
            .expect("被顶掉的原地址要能还回去");
        assert_eq!(base.value, "https://api.anthropic.com");
    }

    #[test]
    fn sweeping_leaves_our_own_keys_as_a_single_edit() {
        let (out, prev) = render_claude(r#"{"env":{"ANTHROPIC_MODEL":"claude-3-opus"}}"#);
        let doc: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(doc["env"]["ANTHROPIC_MODEL"], "auto");
        // 只该记一条「旧值 → 新值」，不该先删后加两条
        let hits: Vec<_> = prev
            .iter()
            .filter(|p| p.ptr == "/env/ANTHROPIC_MODEL")
            .collect();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].value, "claude-3-opus");
    }

    #[test]
    fn sweeping_tolerates_missing_parent() {
        let (out, _) = render_claude(r#"{"permissions":{}}"#);
        let doc: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(doc["env"]["ANTHROPIC_MODEL"], "auto");
    }
}
