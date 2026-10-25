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
    /// 上游线格式。空 = OpenAI 兼容（绝大多数预设都是）；
    /// 只有「额度只能从原生端点用掉」的预设才声明，接入时会写进 Provider。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wire: Option<String>,
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
    // 订阅号：不填 key，点「登录」走授权码 + PKCE，凭据是你自己的订阅。
    // 这两家的额度**只能**从原生端点用掉（Messages / Responses），
    // 所以网关会为它们启用上游协议适配（见 freeport-gateway::upwire）。
    // 条款风险必须写在界面上：用订阅额度跑第三方客户端可能违反厂商条款，
    // 让用户自己决定，比偷偷用强。
    p("claude-sub", "Claude 订阅（Claude Code）", "https://api.anthropic.com/v1", "C", "https://claude.ai", "用自己的 Claude 订阅登录，不填 key；额度走订阅，可能不符合厂商条款"),
    p("codex-sub", "ChatGPT 订阅（Codex）", "https://chatgpt.com/backend-api/codex", "C", "https://chatgpt.com", "用自己的 ChatGPT 订阅登录，不填 key；额度走订阅，可能不符合厂商条款"),
    // Cookie 形态的唯一试点：只有它允许「粘贴 web 会话」而不填 key
    p("opencode", "OpenCode Zen", "https://opencode.ai/zen/v1", "C", "https://opencode.ai/auth", "官方免费模型池，可填 key 也可用 web 会话"),

    // ── 以下为「常见 OpenAI 兼容端点」预设 ────────────────────────────
    // 只写端点与申请页，不写额度数字：额度随时会变，写死就成了界面上的假数据。
    p("openai", "OpenAI", "https://api.openai.com/v1", "C", "https://platform.openai.com/api-keys", "官方付费端点，也用于自建中转"),
    p("google", "Google Gemini（OpenAI 兼容）", "https://generativelanguage.googleapis.com/v1beta/openai", "A", "https://aistudio.google.com/apikey", "官方提供 OpenAI 兼容层，免费档按模型给"),
    // Gemini 原生协议：functionCall / systemInstruction 按官方语义走，
    // 由网关做双向转换（见 freeport-gateway::upwire）。兼容层覆盖不到的
    // 场景（工具调用的严格模式、thinkingConfig）只有原生端点认。
    p("google-gemini", "Google Gemini（原生）", "https://generativelanguage.googleapis.com/v1beta", "A", "https://aistudio.google.com/apikey", "Gemini 原生协议端点，免费档按模型给；与兼容层二选一即可"),
    p("deepseek", "DeepSeek", "https://api.deepseek.com/v1", "C", "https://platform.deepseek.com/api_keys", "余额制，价格低"),
    p("moonshot", "Moonshot Kimi", "https://api.moonshot.cn/v1", "C", "https://platform.moonshot.cn/console/api-keys", "国内端点，长上下文"),
    p("zhipuai", "智谱 GLM", "https://open.bigmodel.cn/api/paas/v4", "A", "https://open.bigmodel.cn/usercenter/apikeys", "有免费模型档，具体以官网为准"),
    p("zai", "Z.ai GLM (国际)", "https://api.z.ai/api/paas/v4", "C", "https://z.ai/manage-apikey/apikey-list", "GLM 国际站，编码套餐走这里"),
    p("minimax", "MiniMax", "https://api.minimax.chat/v1", "C", "https://platform.minimaxi.com/user-center/basic-information/interface-key", "含语音与视频模型"),
    p("siliconflow", "SiliconFlow 硅基流动", "https://api.siliconflow.cn/v1", "A", "https://cloud.siliconflow.cn/account/ak", "聚合站，部分小模型免费"),
    p("dashscope", "阿里云百炼", "https://dashscope.aliyuncs.com/compatible-mode/v1", "A", "https://bailian.console.aliyun.com/", "Qwen 系列，新号有免费额度"),
    p("dashscope-intl", "阿里云百炼（国际）", "https://dashscope-intl.aliyuncs.com/compatible-mode/v1", "A", "https://bailian.console.alibabacloud.com/", "国际站端点，账户体系不同"),
    p("ark", "火山方舟 Volcengine", "https://ark.cn-beijing.volces.com/api/v3", "C", "https://console.volcengine.com/ark", "豆包系列，需先建推理接入点"),
    p("hunyuan", "腾讯混元", "https://api.hunyuan.cloud.tencent.com/v1", "C", "https://console.cloud.tencent.com/hunyuan", "混元 OpenAI 兼容层"),
    p("qianfan", "百度千帆", "https://qianfan.baidubce.com/v2", "C", "https://console.bce.baidu.com/qianfan/ais/console/applicationConsole/application", "文心系列 v2 接口"),
    p("baichuan", "百川智能", "https://api.baichuan-ai.com/v1", "C", "https://platform.baichuan-ai.com/console/apikey", "Baichuan 系列"),
    p("stepfun", "阶跃星辰 StepFun", "https://api.stepfun.com/v1", "C", "https://platform.stepfun.com/interface-key", "Step 系列，含多模态"),
    p("lingyi", "零一万物 01.AI", "https://api.lingyiwanwu.com/v1", "C", "https://platform.lingyiwanwu.com/apikeys", "Yi 系列"),
    p("modelscope", "ModelScope 魔搭", "https://api-inference.modelscope.cn/v1", "A", "https://modelscope.cn/my/myaccesstoken", "社区推理 API，每日有免费调用"),
    p("iflow", "iFlow", "https://apis.iflow.cn/v1", "A", "https://iflow.cn/", "心流平台，免费模型池"),
    p("xai", "xAI Grok", "https://api.x.ai/v1", "C", "https://console.x.ai/", "Grok 系列，订阅与 API 分开计费"),
    p("together", "Together AI", "https://api.together.xyz/v1", "A", "https://api.together.ai/settings/api-keys", "开源模型托管，部分小模型免费"),
    p("fireworks", "Fireworks AI", "https://api.fireworks.ai/inference/v1", "A", "https://fireworks.ai/account/api-keys", "开源模型托管，有试用额度"),
    p("deepinfra", "DeepInfra", "https://api.deepinfra.com/v1/openai", "C", "https://deepinfra.com/dash/api_keys", "按 token 计费，价格低"),
    p("hyperbolic", "Hyperbolic", "https://api.hyperbolic.xyz/v1", "A", "https://app.hyperbolic.xyz/settings", "开源模型，有免费额度"),
    p("novita", "Novita AI", "https://api.novita.ai/v3/openai", "A", "https://novita.ai/settings/key-management", "开源模型与图像，有试用额度"),
    p("lambda", "Lambda Labs", "https://api.lambdalabs.com/v1", "C", "https://cloud.lambdalabs.com/api-keys", "开源模型推理"),
    p("sambanova", "SambaNova", "https://api.sambanova.ai/v1", "A", "https://cloud.sambanova.ai/apis", "有免费档，具体以官网为准"),
    p("github-models", "GitHub Models", "https://models.inference.ai.azure.com", "A", "https://github.com/settings/tokens", "用 GitHub token 调，免费档按请求数"),
    p("vercel", "Vercel AI Gateway", "https://ai-gateway.vercel.sh/v1", "C", "https://vercel.com/dashboard/ai-gateway", "聚合网关，按额度计费"),
    p("requesty", "Requesty", "https://router.requesty.ai/v1", "C", "https://app.requesty.ai/api-keys", "聚合路由，自带用量面板"),
    p("cohere", "Cohere", "https://api.cohere.ai/compatibility/v1", "A", "https://dashboard.cohere.com/api-keys", "有试用额度，兼容层只覆盖 chat"),
    p("perplexity", "Perplexity", "https://api.perplexity.ai", "C", "https://www.perplexity.ai/settings/api", "联网问答，按请求计费"),
    p("voyage", "Voyage AI", "https://api.voyageai.com/v1", "A", "https://dash.voyageai.com/api-keys", "嵌入与重排，免费档按 token"),
    p("jina", "Jina AI", "https://api.jina.ai/v1", "A", "https://jina.ai/embeddings/", "嵌入与重排，有免费额度"),
    p("upstage", "Upstage Solar", "https://api.upstage.ai/v1/solar", "A", "https://console.upstage.ai/api-keys", "Solar 系列，有免费档"),
    p("pollinations", "Pollinations", "https://text.pollinations.ai/openai", "B", "https://pollinations.ai/", "匿名公开端点，无需 key"),
    p("llm7", "LLM7", "https://api.llm7.io/v1", "B", "https://llm7.io/", "匿名公开端点，无需 key"),
    p("kluster", "Kluster AI", "https://api.kluster.ai/v1", "A", "https://platform.kluster.ai/apikeys", "开源模型推理，有试用额度"),
    p("custom", "自定义", "", "C", "", "任意 OpenAI 兼容端点（含本地 chat2api / Ollama 代理）"),
];

/// 声明非 OpenAI 线格式的预设。只有「额度只能从原生端点用掉」的厂商才上这张表；
/// 不在表里的一律 OpenAI 兼容，不需要给 47 家各写一个 `""`。
const WIRED: &[(&str, &str)] = &[
    ("claude-sub", "anthropic"),
    ("codex-sub", "responses"),
    ("google-gemini", "gemini"),
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
            wire: WIRED
                .iter()
                .find(|(w, _)| *w == *id)
                .map(|(_, v)| v.to_string()),
        })
        .collect()
}

pub fn get(id: &str) -> Option<ProviderPreset> {
    all().into_iter().find(|p| p.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 预设是冷启动的第一屏。少于 40 家说明这一层没维护，
    /// 用户就得自己去查 baseUrl——那正是我们要省掉的事。
    #[test]
    fn preset_list_is_wide_enough() {
        let list = all();
        assert!(list.len() >= 40, "预设只有 {} 家", list.len());
    }

    #[test]
    fn ids_are_unique_and_custom_is_last() {
        let list = all();
        let mut ids: Vec<&str> = list.iter().map(|p| p.id.as_str()).collect();
        let n = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), n, "预设 id 有重复");
        assert_eq!(list.last().unwrap().id, "custom", "自定义必须排在最后");
    }

    /// 除「自定义」外，每家都得有可用的 https 基址。
    /// 带 `{占位符}` 的端点会被添加表单拒绝（表单不允许改非自定义项的 baseUrl），
    /// 所以这类条目一进来就是死链，必须在测试里挡住。
    #[test]
    fn every_other_preset_has_a_usable_base_url() {
        for p in all().iter().filter(|p| p.id != "custom") {
            assert!(
                p.base_url.starts_with("https://"),
                "{} 的基址不是 https：{}",
                p.id,
                p.base_url
            );
            assert!(
                !p.base_url.contains('{'),
                "{} 的基址含占位符，添加表单改不了它",
                p.id
            );
        }
    }
}
