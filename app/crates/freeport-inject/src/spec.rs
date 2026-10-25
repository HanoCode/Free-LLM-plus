//! Agent 注册表：检测路径、写入策略、协议。
//!
//! 一条纪律：**没把握就不写**。注释多、结构随版本变的配置文件（Continue 的
//! config.yaml、Roo 的私有存储）一律降级成「给步骤」，不硬改。
//! 宁可少一个自动化，也不要写坏别人的配置。

use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    OpenAi,
    Anthropic,
    Gemini,
}

impl Protocol {
    pub fn as_str(self) -> &'static str {
        match self {
            Protocol::OpenAi => "openai",
            Protocol::Anthropic => "anthropic",
            Protocol::Gemini => "gemini",
        }
    }
}

/// 谁决定客户端能选到哪些模型。与 [`Strategy`] 正交：
/// `Strategy` 回答「怎么写进去」，`Discovery` 回答「模型表从哪来」。
///
/// 这个维度比「支持哪些 IDE」有用得多——同一个模式下的十个客户端，
/// 边际成本只是一条数据；跨模式才是真的换实现。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Discovery {
    /// 客户端自己拉 OpenAI 形状的 `/v1/models`，或者让用户手填模型名
    SelfServe,
    /// 客户端启动后自己拉，**但只认自己的形状与命名**（Claude Code 只收 `claude*`）
    Pull,
    /// 客户端不做发现：模型表必须在注入时序列化进它的配置（VS Code Chat、Codex catalog）
    Push,
    /// 客户端只说自有协议，所需的 adapter 进程不是我们能提供的（JetBrains ACP）
    Host,
}

impl Discovery {
    pub fn as_str(self) -> &'static str {
        match self {
            Discovery::SelfServe => "self",
            Discovery::Pull => "pull",
            Discovery::Push => "push",
            Discovery::Host => "host",
        }
    }
}

/// 一个待写入的 JSON 节点。
pub struct KeySpec {
    /// JSON Pointer，形如 `/env/ANTHROPIC_BASE_URL`
    pub ptr: &'static str,
    pub value: Src,
}

/// 值的来源。写成枚举而不是格式化字符串，方便审计「到底写了什么」。
#[derive(Clone, Copy)]
pub enum Src {
    /// 网关根地址，如 `http://127.0.0.1:8787`
    Base,
    /// 网关根地址 + `/v1`
    BaseV1,
    Key,
    Model,
    Literal(&'static str),
    /// 布尔开关。别用 `Literal("true")` 凑：写进去会变成字符串，
    /// JSON 里 `"true"` 是真的，但客户端按布尔读会当成 false。
    Bool(bool),
}

#[derive(Clone, Copy)]
pub enum Strategy {
    JsonKeys(&'static [KeySpec]),
    TomlBlock,
    /// Push：把整份模型组写进客户端的配置。需要目录，见 `Ctx::models`。
    Push(&'static PushSpec),
    /// 变量数组：VS Code 扩展的 `claudeCode.environmentVariables`，形如 `[{name, value}]`。
    /// 键表与 [`CLAUDE_KEYS`] 共用，`ptr` 只取最后一段当变量名。
    EnvList(&'static [KeySpec]),
    /// 寄宿：往**已存在**的 ACP agent 条目里注 env，不替客户端建条目。
    /// `command` / `args` 指向每台机器都不同的 node 与 adapter 脚本，猜不出来。
    AcpEnv(&'static [KeySpec]),
    /// Claude Desktop 的 3p 网关：三个文件互相引用，见 [`DesktopSpec`]。
    Desktop(&'static DesktopSpec),
    /// 双文件策略：两个文件各写一半，单独哪一个都生效不了，见 [`DuoKind`]。
    Duo(DuoKind),
    Guide,
}

/// 双文件策略的两类客户端。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DuoKind {
    /// Gemini CLI：`.env` 网关变量 + settings.json 的 API key 认证开关
    GeminiCli,
    /// Goose：custom_providers/freeport.json + config.yaml 的默认 provider
    Goose,
}

/// Claude Desktop 的独立网关配置目录（`Claude-3p/`）。
///
/// 这不是 `claude_desktop_config.json`：那个只管 MCP。3p 模式是一个**配置库**——
/// `configLibrary/<uuid>.json` 存各套网关，`_meta.json` 记哪套在用，
/// `claude_desktop_config.json` 的 `deploymentMode` 决定走不走第三方。
pub struct DesktopSpec {
    /// 我们的档位 id。Desktop 只要求 36 位小写十六进制 + 短横线，且**必须稳定**：
    /// 换了 id 就等于换了一套档，用户原来的那套会变成孤儿。
    pub profile_id: &'static str,
    /// 档位显示名
    pub profile_name: &'static str,
    /// 认领用的自定义请求头。Desktop 没有别的标记位可放，
    /// 只能借 `inferenceCustomHeaders` 里一条我们自己认识的头。
    pub marker_header: &'static str,
    pub marker_value: &'static str,
}

/// Push 模式要写的「模型组」形状。
///
/// 各家的字段名不同，但结构同构：顶层数组里放若干组，我们只碰带自己标记的那一组。
pub struct PushSpec {
    /// 组上的标记字段名。值固定写 `"freeport"`，用来认领「这组是我写的」。
    pub marker: &'static str,
    /// 组名，客户端里显示成一组
    pub group: &'static str,
    /// 客户端认的厂商标识（VS Code 要 `customendpoint`）
    pub vendor: &'static str,
    /// 请求地址后缀，拼在网关根地址后面
    pub path: &'static str,
    /// VS Code 的 `apiType`：`messages` / `chat` / `responses`。
    ///
    /// 选 `messages` 是有意的：网关的 Anthropic 入口是原生实现，走它比
    /// 先转成 chat 再转回来少两次改写，tool_use / thinking 都不会丢。
    pub api_type: &'static str,
}

pub struct AgentSpec {
    pub id: &'static str,
    pub name: &'static str,
    pub protocol: Protocol,
    /// 模型表从哪来
    pub discovery: Discovery,
    pub strategy: Strategy,
    /// 写入前先清掉的旧键。清掉的旧值会进回滚日志，revert 能原样还回去。
    pub prune: Option<&'static Prune>,
    /// 检测用的路径：任一存在就认为装了
    pub detect: fn() -> Vec<PathBuf>,
    /// 实际写入的配置文件
    pub file: fn() -> Option<PathBuf>,
    /// 给用户看的一句话
    pub note: &'static str,
}

/// 项目级落点：`(agent id, 相对仓库根的路径)`。
///
/// 单独一张表而不是 [`AgentSpec`] 的字段：19 个适配器里只有极少数存在
/// 「客户端自己认定的个人本地文件」，塞进每个 initializer 只会换来一屏
/// `project: None`，还把「能不能写进仓库」这件事埋在二十行样板里。
///
/// 入选门槛只有一条：**那份文件默认不进 git**。把 key 写进会被提交的文件，
/// 等于把本机网关的凭据推进团队仓库——一次 push 就撤不回来了。
///
/// 为什么只有 Claude Code：
/// - Claude Code：`.claude/settings.local.json` 是官方约定的「个人本地覆盖」，
///   与共享的 `.claude/settings.json` 分开；
/// - OpenCode / Crush：项目级 `opencode.json` / `.crush.json` 就是要提交的那份，
///   没有个人/共享的分层，key 无处安放；
/// - Cline / Roo / Kilo / VS Code Chat：配置存在扩展的 globalStorage 或用户目录，
///   没有项目级文件；
/// - Codex / Goose / Continue / Aider / Zed / Gemini CLI：要么只读用户级配置，
///   要么配置文件带注释与锚点，代写会破坏结构（见各自 `note`）。
static PROJECT_TARGETS: &[(&str, &str)] = &[("claude-code", ".claude/settings.local.json")];

/// 这个 Agent 的项目级落点（相对仓库根）。`None` = 不支持项目级注入。
pub fn project_rel(id: &str) -> Option<&'static str> {
    PROJECT_TARGETS
        .iter()
        .find(|(a, _)| *a == id)
        .map(|(_, rel)| *rel)
}

/// 写入前的一次清扫。
///
/// 为什么需要它：同一个 `env` 块里可能留着别的工具写的 `ANTHROPIC_API_KEY`、
/// 或者上一版注入残留的开关。两份配置同时生效时，客户端按自己的优先级取一个，
/// 结果就是「看起来注入了，实际还在打别处」。清理必须记旧值，否则回滚不回去。
pub struct Prune {
    /// 清扫的父节点 JSON Pointer，如 `/env`
    pub at: &'static str,
    /// 命中的键前缀，如 `ANTHROPIC_`
    pub prefixes: &'static [&'static str],
    /// 精确命中的键（没有统一前缀的开关）
    pub keys: &'static [&'static str],
}

fn home() -> PathBuf {
    if let Ok(h) = std::env::var("FREEPORT_HOME") {
        return PathBuf::from(h);
    }
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("~"))
}

/// VS Code 的用户目录（`Code/User`），三个平台各一份。
///
/// `settings.json`、`chatLanguageModels.json`、`globalStorage/` 都在这一层：
/// 前者是原生聊天模型选择器的配置，后两者是扩展自己的数据。
fn vscode_user() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        home().join("Library/Application Support/Code/User")
    }
    #[cfg(target_os = "windows")]
    {
        PathBuf::from(std::env::var("APPDATA").unwrap_or_default()).join("Code/User")
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        home().join(".config/Code/User")
    }
}

/// VS Code 的 globalStorage，三个平台各一份。
fn vscode_global() -> PathBuf {
    vscode_user().join("globalStorage")
}

/// 装过的扩展目录：`~/.vscode/extensions/<publisher.name>-<version>`。
///
/// 用前缀匹配而不是精确版本号：扩展升级后目录名里的版本会变，
/// 写死版本的检测在用户更新一次之后就会静默失效。
fn vscode_ext(prefix: &str) -> Vec<PathBuf> {
    let dir = home().join(".vscode/extensions");
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    rd.flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().starts_with(prefix))
                .unwrap_or(false)
        })
        .collect()
}

/// JetBrains 的 ACP 注册表。条目是我们注 env 的落点，不是我们建的。
fn acp_file() -> PathBuf {
    home().join(".jetbrains/acp.json")
}

/// Crush 的全局配置。mac / Linux / Windows 都是 `~/.config/crush/`。
fn crush_file() -> PathBuf {
    home().join(".config/crush/crush.json")
}

/// Claude Desktop 的 3p 根目录，三个平台各一份。
pub(crate) fn desktop_root() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        home().join("Library/Application Support/Claude-3p")
    }
    #[cfg(target_os = "windows")]
    {
        PathBuf::from(std::env::var("LOCALAPPDATA").unwrap_or_default()).join("Claude-3p")
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Ok(x) = std::env::var("XDG_CONFIG_HOME") {
            if !x.is_empty() {
                return PathBuf::from(x).join("Claude-3p");
            }
        }
        home().join(".config/Claude-3p")
    }
}

/// 我们那一套网关档的落点。
pub(crate) fn desktop_profile(d: &DesktopSpec) -> PathBuf {
    desktop_root()
        .join("configLibrary")
        .join(format!("{}.json", d.profile_id))
}

/// 配置库的索引：哪几套档、当前用哪套。
pub(crate) fn desktop_meta() -> PathBuf {
    desktop_root().join("configLibrary/_meta.json")
}

/// 只决定 `deploymentMode`，与 MCP 的配置同处一个文件。
pub(crate) fn desktop_mode() -> PathBuf {
    desktop_root().join("claude_desktop_config.json")
}

/* ── Qwen Code ───────────────────────────────── */
///
/// Qwen Code 是 gemini-cli 的分支，但走 OpenAI 协议：`settings.json` 的 `env`
/// 块里放 `OPENAI_*` 三项，再把认证切到 openai。不写 `modelProviders`——
/// 那套多 provider 表结构随版本变，env 覆盖是官方文档给的无头配置方式。
static QWEN_KEYS: &[KeySpec] = &[
    KeySpec {
        ptr: "/env/OPENAI_BASE_URL",
        value: Src::BaseV1,
    },
    KeySpec {
        ptr: "/env/OPENAI_API_KEY",
        value: Src::Key,
    },
    KeySpec {
        ptr: "/env/OPENAI_MODEL",
        value: Src::Model,
    },
    KeySpec {
        ptr: "/security/auth/selectedType",
        value: Src::Literal("openai"),
    },
    KeySpec {
        ptr: "/model/name",
        value: Src::Model,
    },
];

/// env 块只服务于 Qwen Code：别的工具留下的 `OPENAI_API_KEY` 会让请求打去别处，
/// 与 Claude 的 `ANTHROPIC_` 清扫同一条理由。
static QWEN_PRUNE: Prune = Prune {
    at: "/env",
    prefixes: &["OPENAI_"],
    keys: &[],
};

/* ── iFlow CLI ───────────────────────────────── */
///
/// 只写顶层三键（`apiKey` / `baseUrl` / `modelName`），**不碰 `selectedAuthType`**：
/// 官方 README 的示例就是 `selectedAuthType: "iflow"` 与三键共存的形状，
/// 动它反而可能把用户的心流登录态弄丢。
static IFLOW_KEYS: &[KeySpec] = &[
    KeySpec {
        ptr: "/baseUrl",
        value: Src::BaseV1,
    },
    KeySpec {
        ptr: "/apiKey",
        value: Src::Key,
    },
    KeySpec {
        ptr: "/modelName",
        value: Src::Model,
    },
];

/* ── Goose 的落点 ────────────────────────────── */
///
/// mac / Linux 在 `~/.config/goose`，Windows 在 `%APPDATA%\Block\goose\config`。
pub(crate) fn goose_dir() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        PathBuf::from(std::env::var("APPDATA").unwrap_or_default()).join("Block/goose/config")
    }
    #[cfg(not(target_os = "windows"))]
    {
        home().join(".config/goose")
    }
}

/* ── Claude Code ─────────────────────────────── */
///
/// 只写三项是不够的：Claude Code 内部按 opus / sonnet / haiku 三档发请求
/// （生成标题、快速编辑这类小任务走 haiku），不给它三档的映射，它就会去要
/// `claude-haiku-*` 这样的原厂名，网关没有这个模型 → 400。
///
/// 另外两个开关不写就会漏流量：
/// - `CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY`：不打开，客户端只认自己内置的模型表
/// - `CLAUDE_CODE_AUTO_MODE_SERVER=0`：不关，一部分请求仍会打到 Anthropic 的分类器
pub(crate) static CLAUDE_KEYS: &[KeySpec] = &[
    KeySpec {
        ptr: "/env/ANTHROPIC_BASE_URL",
        value: Src::Base,
    },
    KeySpec {
        ptr: "/env/ANTHROPIC_AUTH_TOKEN",
        value: Src::Key,
    },
    KeySpec {
        ptr: "/env/ANTHROPIC_MODEL",
        value: Src::Model,
    },
    // 现阶段三档都指向同一个选择值（默认 auto）。等模型别名层落地后，
    // 这里会换成 claude-sonnet-* / claude-haiku-* 这类网关侧的名字。
    KeySpec {
        ptr: "/env/ANTHROPIC_DEFAULT_SONNET_MODEL",
        value: Src::Model,
    },
    KeySpec {
        ptr: "/env/ANTHROPIC_DEFAULT_OPUS_MODEL",
        value: Src::Model,
    },
    KeySpec {
        ptr: "/env/ANTHROPIC_DEFAULT_HAIKU_MODEL",
        value: Src::Model,
    },
    KeySpec {
        ptr: "/env/CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY",
        value: Src::Literal("1"),
    },
    KeySpec {
        ptr: "/env/CLAUDE_CODE_AUTO_MODE_SERVER",
        value: Src::Literal("0"),
    },
    KeySpec {
        ptr: "/env/CLAUDE_CODE_DISABLE_ADVISOR_TOOL",
        value: Src::Literal("1"),
    },
    KeySpec {
        ptr: "/env/CLAUDE_CODE_AUTO_COMPACT_WINDOW",
        value: Src::Literal("190000"),
    },
    // 自动更新会把用户拉回原厂版本，顺手把配置改回去
    KeySpec {
        ptr: "/env/DISABLE_AUTOUPDATER",
        value: Src::Literal("1"),
    },
    KeySpec {
        ptr: "/env/DISABLE_FEEDBACK_COMMAND",
        value: Src::Literal("1"),
    },
    KeySpec {
        ptr: "/env/DISABLE_ERROR_REPORTING",
        value: Src::Literal("1"),
    },
];

/// `ANTHROPIC_` 前缀全清的理由：这个 env 块只服务于 Claude Code，留着别的工具写的
/// `ANTHROPIC_API_KEY` 会和 `ANTHROPIC_AUTH_TOKEN` 打架；`DISABLE_NONESSENTIAL_TRAFFIC`
/// 必须精确点名，因为它会把网关模型发现一起压掉。
static CLAUDE_PRUNE: Prune = Prune {
    at: "/env",
    prefixes: &["ANTHROPIC_"],
    keys: &["CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC"],
};

/* ── Cline ───────────────────────────────────── */
static CLINE_KEYS: &[KeySpec] = &[
    KeySpec {
        ptr: "/apiProvider",
        value: Src::Literal("openai"),
    },
    KeySpec {
        ptr: "/openAiBaseUrl",
        value: Src::BaseV1,
    },
    KeySpec {
        ptr: "/openAiApiKey",
        value: Src::Key,
    },
    KeySpec {
        ptr: "/openAiModelId",
        value: Src::Model,
    },
];

/* ── OpenCode ────────────────────────────────── */
static OPENCODE_KEYS: &[KeySpec] = &[
    KeySpec {
        ptr: "/provider/freeport/npm",
        value: Src::Literal("@ai-sdk/openai-compatible"),
    },
    KeySpec {
        ptr: "/provider/freeport/name",
        value: Src::Literal("Freeport"),
    },
    KeySpec {
        ptr: "/provider/freeport/options/baseURL",
        value: Src::BaseV1,
    },
    KeySpec {
        ptr: "/provider/freeport/options/apiKey",
        value: Src::Key,
    },
    KeySpec {
        ptr: "/provider/freeport/models/auto/name",
        value: Src::Literal("Freeport auto"),
    },
];

/* ── VS Code 原生聊天（Push 模式）──
 *
 * VS Code 的模型选择器**不做发现**：它只认 `Code/User/chatLanguageModels.json`
 * 里序列化好的模型组，能力字段（`toolCalling` / `vision` / `maxInputTokens`）
 * 也只能由我们给。这正是 Push 模式存在的理由——客户端不问，我们只能写。
 */
static VSCODE_CHAT: PushSpec = PushSpec {
    marker: "freeportVendor",
    group: "Freeport",
    vendor: "customendpoint",
    path: "/v1/messages",
    api_type: "messages",
};

/* ── Crush ───────────────────────────────────── */
///
/// 只写 provider 与 `discover_models`，不写模型数组：目录里的模型每天都在变，
/// 写死的 `models` 一周后就过期，而 Crush 自己拉 `/v1/models` 永远是最新的。
/// 代价是没有上下文窗口数字（`/v1/models` 不带这个字段），Crush 里看不到占用比例。
static CRUSH_KEYS: &[KeySpec] = &[
    KeySpec {
        ptr: "/providers/freeport/type",
        value: Src::Literal("openai-compat"),
    },
    KeySpec {
        ptr: "/providers/freeport/name",
        value: Src::Literal("Freeport"),
    },
    KeySpec {
        ptr: "/providers/freeport/base_url",
        value: Src::BaseV1,
    },
    KeySpec {
        ptr: "/providers/freeport/api_key",
        value: Src::Key,
    },
    KeySpec {
        ptr: "/providers/freeport/discover_models",
        value: Src::Bool(true),
    },
];

/// JetBrains 侧的认领标记：写在 agent 条目的 env 里，第二次注入靠它认人。
pub(crate) const ACP_MARK: &str = "FREEPORT_ACP";

/// Claude Desktop 的网关档。id 是常量而不是随机生成：
/// 它同时写在 `_meta.json` 的 entries 与 appliedId 里，每次注入换一个就等于
/// 在用户的配置库里堆一堆孤儿档。
pub(crate) const DESKTOP: DesktopSpec = DesktopSpec {
    profile_id: "5f2c8a41-7d63-5b90-9e14-3c6a8b02d7f5",
    profile_name: "Freeport",
    marker_header: "X-Freeport-Model-View",
    marker_value: "claude-desktop",
};

pub const AGENTS: &[AgentSpec] = &[
    AgentSpec {
        id: "claude-code",
        name: "Claude Code",
        protocol: Protocol::Anthropic,
        discovery: Discovery::Pull,
        strategy: Strategy::JsonKeys(CLAUDE_KEYS),
        prune: Some(&CLAUDE_PRUNE),
        detect: || vec![home().join(".claude")],
        file: || Some(home().join(".claude/settings.json")),
        note: "写入 ~/.claude/settings.json 的 env 三项，不动其余设置",
    },
    AgentSpec {
        id: "claude-code-vscode",
        name: "Claude Code（VS Code 扩展）",
        protocol: Protocol::Anthropic,
        discovery: Discovery::Pull,
        strategy: Strategy::EnvList(CLAUDE_KEYS),
        prune: None,
        detect: || {
            let mut v = vscode_ext("anthropic.claude-code");
            v.push(vscode_global().join("anthropic.claude-code"));
            v
        },
        file: || Some(vscode_user().join("settings.json")),
        note: "写 VS Code 的 claudeCode.environmentVariables（同一份变量表）；装了扩展时这条才是真正生效的那条",
    },
    AgentSpec {
        id: "codex",
        name: "Codex CLI",
        protocol: Protocol::OpenAi,
        discovery: Discovery::Push,
        strategy: Strategy::TomlBlock,
        prune: None,
        detect: || vec![home().join(".codex")],
        file: || Some(home().join(".codex/config.toml")),
        note: "在 ~/.codex/config.toml 插入一个带标记的托管块，密钥随请求头走",
    },
    AgentSpec {
        id: "cline",
        name: "Cline",
        protocol: Protocol::OpenAi,
        discovery: Discovery::SelfServe,
        strategy: Strategy::JsonKeys(CLINE_KEYS),
        prune: None,
        detect: || vec![vscode_global().join("saoudrizwan.claude-dev")],
        file: || {
            Some(
                vscode_global()
                    .join("saoudrizwan.claude-dev/settings/cline_settings.json"),
            )
        },
        note: "写 Cline 的 apiProvider / baseUrl / apiKey 四项，注入前会备份原值",
    },
    AgentSpec {
        id: "opencode",
        name: "OpenCode",
        protocol: Protocol::OpenAi,
        discovery: Discovery::SelfServe,
        strategy: Strategy::JsonKeys(OPENCODE_KEYS),
        prune: None,
        detect: || vec![home().join(".config/opencode")],
        file: || {
            let dir = home().join(".config/opencode");
            let json = dir.join("opencode.json");
            if json.exists() {
                Some(json)
            } else {
                let alt = dir.join("config.json");
                if alt.exists() {
                    Some(alt)
                } else {
                    Some(json)
                }
            }
        },
        note: "在 provider 下加一个 freeport 条目，原有 provider 不动",
    },
    AgentSpec {
        id: "vscode-chat",
        name: "VS Code Chat",
        // 走 `messages` 而不是 chat：网关的 Anthropic 入口是原生实现，
        // 少一次来回转换，tool_use 与 thinking 都能原样过
        protocol: Protocol::Anthropic,
        discovery: Discovery::Push,
        strategy: Strategy::Push(&VSCODE_CHAT),
        prune: None,
        detect: || vec![vscode_user()],
        file: || Some(vscode_user().join("chatLanguageModels.json")),
        note: "在原生模型选择器里加一组 Freeport（写 chatLanguageModels.json，其余组原样保留）",
    },
    AgentSpec {
        id: "crush",
        name: "Crush",
        protocol: Protocol::OpenAi,
        discovery: Discovery::SelfServe,
        strategy: Strategy::JsonKeys(CRUSH_KEYS),
        prune: None,
        detect: || vec![crush_file()],
        file: || Some(crush_file()),
        note: "在 providers 下加一个 freeport（openai-compat），模型表由 Crush 自己拉 /v1/models",
    },
    AgentSpec {
        id: "jetbrains",
        name: "JetBrains（Claude Agent / ACP）",
        protocol: Protocol::Anthropic,
        discovery: Discovery::Host,
        strategy: Strategy::AcpEnv(CLAUDE_KEYS),
        prune: None,
        // 只认 acp.json：条目得由 JetBrains 自己建，我们才有 command / args 可寄宿
        detect: || vec![acp_file()],
        file: || Some(acp_file()),
        note: "往已有的 Claude Agent 条目里注环境变量，不新建条目；acp.json 不存在时先在 IDE 里装好并启动一次",
    },
    AgentSpec {
        id: "claude-desktop",
        name: "Claude Desktop",
        // Desktop 走的是 Anthropic 形状的推理接口，模型表则自己拉（Pull）
        protocol: Protocol::Anthropic,
        discovery: Discovery::Pull,
        strategy: Strategy::Desktop(&DESKTOP),
        prune: None,
        // 认 _meta.json：没有它说明用户还没进过 3p 模式，配置库的 schema 我们无从校验
        detect: || vec![desktop_meta(), desktop_root()],
        file: || Some(desktop_profile(&DESKTOP)),
        note: "在 Claude-3p 配置库里加一套 Freeport 网关档并切过去；先启动一次 Desktop 进入第三方网关模式",
    },
    AgentSpec {
        id: "continue",
        name: "Continue",
        protocol: Protocol::OpenAi,
        discovery: Discovery::SelfServe,
        strategy: Strategy::Guide,
        prune: None,
        detect: || vec![home().join(".continue")],
        file: || None,
        note: "config.yaml 带注释与锚点，外部改写容易破坏结构；给步骤，不代写",
    },
    AgentSpec {
        id: "gemini-cli",
        name: "Gemini CLI",
        protocol: Protocol::Gemini,
        discovery: Discovery::SelfServe,
        strategy: Strategy::Duo(DuoKind::GeminiCli),
        prune: None,
        detect: || vec![home().join(".gemini")],
        file: || Some(home().join(".gemini/settings.json")),
        note: "写 ~/.gemini/.env（网关地址 / key / 模型）并把 settings.json 切到 API key 认证——.env 的变量只在 API key 模式下被读，只写一半对 OAuth 用户无效；回滚会还原认证开关",
    },
    AgentSpec {
        id: "roo",
        name: "Roo Code",
        protocol: Protocol::OpenAi,
        discovery: Discovery::SelfServe,
        strategy: Strategy::Guide,
        prune: None,
        detect: || vec![vscode_global().join("rooveterinaryinc.roo-cline")],
        file: || None,
        note: "配置存在扩展私有存储里，写入会被覆盖；给步骤",
    },
    AgentSpec {
        id: "cursor",
        name: "Cursor",
        protocol: Protocol::OpenAi,
        discovery: Discovery::SelfServe,
        strategy: Strategy::Guide,
        prune: None,
        detect: || {
            vec![
                home().join("Library/Application Support/Cursor"),
                home().join(".cursor"),
            ]
        },
        file: || None,
        note: "只在设置界面里配（Settings → Models），没有可安全改写的文件",
    },
    AgentSpec {
        id: "zed",
        name: "Zed",
        protocol: Protocol::OpenAi,
        discovery: Discovery::SelfServe,
        strategy: Strategy::Guide,
        prune: None,
        detect: || vec![home().join(".config/zed")],
        file: || None,
        note: "settings.json 的 language_models.openai_compatible 加一个 freeport 块（api_url + available_models）；Zed 明确要求 key 不进 settings.json，首次聊天时在设置面板粘贴一次",
    },
    AgentSpec {
        id: "aider",
        name: "Aider",
        protocol: Protocol::OpenAi,
        discovery: Discovery::SelfServe,
        strategy: Strategy::Guide,
        prune: None,
        detect: || vec![home().join(".aider.conf.yml")],
        file: || None,
        note: "启动前 export OPENAI_API_BASE / OPENAI_API_KEY；aider.conf.yml 是带锚点的 YAML，外部改写容易坏，给步骤不代写",
    },
    AgentSpec {
        id: "kilo",
        name: "Kilo Code",
        protocol: Protocol::OpenAi,
        discovery: Discovery::SelfServe,
        strategy: Strategy::Guide,
        prune: None,
        detect: || vec![home().join(".config/kilo"), vscode_global().join("kilocode.kilo-code")],
        file: || None,
        note: "配置是 kilo.jsonc（JSONC，带注释），serde 解析会丢注释；给步骤：Providers → Custom provider（OpenAI Compatible），填网关地址（保留 /v1 后缀），Kilo 会自己拉 /v1/models",
    },
    AgentSpec {
        id: "goose",
        name: "Goose",
        protocol: Protocol::OpenAi,
        discovery: Discovery::SelfServe,
        strategy: Strategy::Duo(DuoKind::Goose),
        prune: None,
        detect: || vec![goose_dir()],
        file: || Some(goose_dir().join("custom_providers/freeport.json")),
        note: "写 custom_providers/freeport.json（engine=openai，密钥随 Authorization 头走，不动你的 shell 配置）并把 config.yaml 的 GOOSE_PROVIDER/GOOSE_MODEL 指到 freeport；两处都可一键回滚",
    },
    AgentSpec {
        id: "workbuddy",
        name: "WorkBuddy",
        protocol: Protocol::OpenAi,
        discovery: Discovery::SelfServe,
        strategy: Strategy::Guide,
        prune: None,
        detect: || vec![home().join(".workbuddy")],
        file: || None,
        note: "模型接入走应用内的连接器/专家配置，没有可安全改写的模型配置文件；在设置里选自定义 OpenAI 兼容端点即可",
    },
    AgentSpec {
        id: "qwen-code",
        name: "Qwen Code",
        protocol: Protocol::OpenAi,
        discovery: Discovery::SelfServe,
        strategy: Strategy::JsonKeys(QWEN_KEYS),
        prune: Some(&QWEN_PRUNE),
        detect: || vec![home().join(".qwen")],
        file: || Some(home().join(".qwen/settings.json")),
        note: "写 ~/.qwen/settings.json 的 OPENAI_ 三项并切到 openai 认证；env 块里的旧 OPENAI_ 键先清后写，原值进回滚日志",
    },
    AgentSpec {
        id: "iflow",
        name: "iFlow CLI",
        protocol: Protocol::OpenAi,
        discovery: Discovery::SelfServe,
        strategy: Strategy::JsonKeys(IFLOW_KEYS),
        prune: None,
        detect: || vec![home().join(".iflow")],
        file: || Some(home().join(".iflow/settings.json")),
        note: "写 ~/.iflow/settings.json 的 apiKey / baseUrl / modelName 三项；认证方式不动，心流登录态不受影响",
    },
];

pub fn find(id: &str) -> Option<&'static AgentSpec> {
    AGENTS.iter().find(|a| a.id == id)
}

/// 检测到的路径：优先已存在的，其次第一个候选（用于新建）。
pub fn resolve_path(spec: &AgentSpec) -> Option<PathBuf> {
    let file = (spec.file)();
    let detected = (spec.detect)();
    let installed =
        detected.iter().any(|p| p.exists()) || file.as_ref().is_some_and(|p| p.exists());
    if !installed {
        return None;
    }
    file
}
