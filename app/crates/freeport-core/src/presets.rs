//! 内置厂商预设。M1 只有 OpenAI 兼容的这几家，
//! M2 会改为从目录 feed 加载，这里的列表仅作离线兜底。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderPreset {
    pub id: String,
    pub name: String,
    #[serde(rename = "baseUrl")]
    pub base_url: String,
    pub tier: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

const fn p(
    id: &'static str,
    name: &'static str,
    base: &'static str,
    tier: &'static str,
    homepage: &'static str,
    note: &'static str,
) -> (&'static str, &'static str, &'static str, &'static str, &'static str, &'static str) {
    (id, name, base, tier, homepage, note)
}

const PRESETS: &[(&str, &str, &str, &str, &str, &str)] = &[
    p("groq", "Groq", "https://api.groq.com/openai/v1", "A", "https://console.groq.com/keys", "免费额度高、速度快，coding agent 首选"),
    p("cerebras", "Cerebras", "https://api.cerebras.ai/v1", "A", "https://cloud.cerebras.ai", "推理极快，日额度有限"),
    p("openrouter", "OpenRouter", "https://openrouter.ai/api/v1", "A", "https://openrouter.ai/keys", "带 :free 后缀的模型价格全 0"),
    p("mistral", "Mistral", "https://api.mistral.ai/v1", "A", "https://console.mistral.ai/api-keys", "免费档含 codestral"),
    // 不写具体额度数字：额度随时会变，写死在预设里就成了「界面上的假数据」
    p("nvidia", "NVIDIA NIM", "https://integrate.api.nvidia.com/v1", "A", "https://build.nvidia.com", "有免费额度，具体以官网为准"),
    // Tier C：不用填 key，点「登录」走设备码授权，凭据就是你自己账号的订阅
    p("copilot", "GitHub Copilot", "https://api.githubcopilot.com", "C", "https://github.com/features/copilot", "用自己的 GitHub 账号登录，免填 key"),
    // Cookie 形态的唯一试点：只有它允许「粘贴 web 会话」而不填 key
    p("opencode", "OpenCode Zen", "https://opencode.ai/zen/v1", "C", "https://opencode.ai/auth", "官方免费模型池，可填 key 也可用 web 会话"),
    p("custom", "自定义", "", "C", "", "任意 OpenAI 兼容端点（含本地 chat2api / Ollama 代理）"),
];

pub fn all() -> Vec<ProviderPreset> {
    PRESETS
        .iter()
        .map(|(id, name, base, tier, homepage, note)| ProviderPreset {
            id: (*id).to_string(),
            name: (*name).to_string(),
            base_url: (*base).to_string(),
            tier: (*tier).to_string(),
            homepage: if homepage.is_empty() { None } else { Some((*homepage).to_string()) },
            note: if note.is_empty() { None } else { Some((*note).to_string()) },
        })
        .collect()
}

pub fn get(id: &str) -> Option<ProviderPreset> {
    all().into_iter().find(|p| p.id == id)
}
