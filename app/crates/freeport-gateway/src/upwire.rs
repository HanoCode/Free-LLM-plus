//! 上游「线格式」适配：把网关内部的 OpenAI 形态翻译成上游真正认的协议。
//!
//! ## 为什么需要这一层
//!
//! 网关内部一律用 OpenAI chat 的形态做规范化（路由、幂等、缓存、压缩全都基于它）。
//! 但有些上游根本不提供 OpenAI 兼容端点：
//!
//! | 上游 | 端点 | 说什么 |
//! |---|---|---|
//! | Anthropic（订阅号） | `/v1/messages` | Anthropic Messages |
//! | Codex（订阅号） | `/backend-api/codex/responses` | OpenAI Responses |
//! | Gemini 原生 | `/v1beta/models/{m}:generateContent` | Gemini generateContent |
//!
//! 订阅额度只能从这两个端点用掉，Gemini 原生端点则有兼容层覆盖不到的语义
//! （工具调用的严格 schema、thinkingConfig），所以必须有一层「进去转一次、
//! 出来再转回来」。这和 [`crate::anthropic`] / [`crate::responses`] 是两个方向：
//! 那两处服务**客户端**（Claude Code 说 Messages，我们把它转成 OpenAI 去路由），
//! 这里服务**上游**（上游说原生协议，我们把 OpenAI 转成它去发）。
//!
//! ## 三条约定
//!
//! 1. **尽量无损，认不出就丢**：宁可少一个边缘字段，也不要猜着填一个上游会
//!    400 的值。每处丢弃都在注释里写明原因。
//! 2. **不假装支持**：上游不支持的参数（Codex 拒收 `temperature`）要主动删，
//!    而不是留着让上游报一个含糊的错。
//! 3. **流式按帧转**：上游的 SSE 帧边界和 TCP 分片无关，必须自己攒缓冲。

use std::collections::VecDeque;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use futures_util::stream::{BoxStream, Stream};
use serde_json::{json, Map, Value};

/// 上游说的是哪种协议。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wire {
    /// OpenAI 兼容的 `/chat/completions`（绝大多数厂商）
    Openai,
    /// Anthropic Messages
    Anthropic,
    /// OpenAI Responses（Codex 订阅）
    Responses,
    /// Gemini generateContent（Google AI Studio 原生端点）
    Gemini,
}

impl Wire {
    /// 空串按 OpenAI 处理：存量配置里没有这个字段，一律是老行为。
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "anthropic" | "messages" | "claude" => Wire::Anthropic,
            "responses" | "response" | "codex" => Wire::Responses,
            "gemini" | "google" => Wire::Gemini,
            _ => Wire::Openai,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Wire::Openai => "openai",
            Wire::Anthropic => "anthropic",
            Wire::Responses => "responses",
            Wire::Gemini => "gemini",
        }
    }

    pub fn is_openai(&self) -> bool {
        matches!(self, Wire::Openai)
    }

    /// 上游路径。拼在 `base_url` 后面。
    ///
    /// Gemini 没有固定路径（模型名和动作编在路径里），一律走 [`url`]。
    pub fn path(&self) -> &'static str {
        match self {
            Wire::Openai => "/chat/completions",
            Wire::Anthropic => "/messages",
            Wire::Responses => "/responses",
            Wire::Gemini => "",
        }
    }
}

/// 拼上游请求 URL。
///
/// 只有 Gemini 需要模型名和 stream 标志；其余协议是固定路径，两个参数被忽略。
/// `?alt=sse` 不能省：不带它 Gemini 流式返回的是一整个 JSON 数组，没法按帧转。
pub fn url(wire: Wire, base_url: &str, model: &str, stream: bool) -> String {
    let base = base_url.trim_end_matches('/');
    match wire {
        Wire::Gemini if stream => format!("{base}/models/{model}:streamGenerateContent?alt=sse"),
        Wire::Gemini => format!("{base}/models/{model}:generateContent"),
        _ => format!("{base}{}", wire.path()),
    }
}

/// 从厂商配置取线格式。
///
/// 显式 `wire` 字段优先（新配置的唯一来源）；空值回落到按 id 推断——
/// 那是 `wire` 字段出现前的老行为，只为存量配置与订阅预设保留，
/// 没有字段也要保证 `claude-sub` / `codex-sub` 继续走原生端点。
pub fn wire_of(p: &freeport_core::model::Provider) -> Wire {
    match p.wire.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(w) => Wire::parse(w),
        None => wire_of_provider(&p.id),
    }
}

/// 按厂商 id 判上游线格式（老路径，仅兜底）。
///
/// 用 id 而不是配置字段是 v1 的权宜：当时只有内置的两个订阅预设需要
/// 非 OpenAI 线格式。现在有了 [`Wire::Gemini`]，显式配置走
/// [`wire_of`]（读 `Provider.wire`），这个函数只服务「没写字段的存量配置」。
pub fn wire_of_provider(id: &str) -> Wire {
    match id {
        "claude-sub" => Wire::Anthropic,
        "codex-sub" => Wire::Responses,
        _ => Wire::Openai,
    }
}

/* ── 请求：OpenAI → 上游 ─────────────────────────── */

pub fn request(wire: Wire, oa: &Value) -> Value {
    match wire {
        Wire::Openai => oa.clone(),
        Wire::Anthropic => anthropic_request(oa),
        Wire::Responses => responses_request(oa),
        Wire::Gemini => gemini_request(oa),
    }
}

/// OpenAI chat → Anthropic Messages。
fn anthropic_request(oa: &Value) -> Value {
    let mut out = Map::new();
    out.insert(
        "model".into(),
        oa.get("model").cloned().unwrap_or(json!("auto")),
    );
    // Anthropic 的 `max_tokens` 是**必填**。不给就塞一个宽松的默认值：
    // 截断比报错好，客户端没写通常是它不在乎长度。
    let mt = oa
        .get("max_tokens")
        .or_else(|| oa.get("max_completion_tokens"))
        .cloned()
        .unwrap_or(json!(4096));
    out.insert("max_tokens".into(), mt);

    for k in ["temperature", "top_p"] {
        if let Some(x) = oa.get(k) {
            out.insert(k.into(), x.clone());
        }
    }
    match oa.get("stream") {
        Some(x) => {
            out.insert("stream".into(), x.clone());
        }
        None => {}
    }

    // OpenAI 的 stop 既可能是字符串也可能是数组，Anthropic 只认数组
    match oa.get("stop") {
        Some(Value::String(s)) => {
            out.insert("stop_sequences".into(), json!([s.clone()]));
        }
        Some(Value::Array(a)) if !a.is_empty() => {
            out.insert("stop_sequences".into(), Value::Array(a.clone()));
        }
        _ => {}
    }

    let mut system: Vec<String> = Vec::new();
    let mut msgs: Vec<Value> = Vec::new();

    if let Some(list) = oa.get("messages").and_then(|m| m.as_array()) {
        for m in list {
            let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("user");
            match role {
                // OpenAI 的 developer 等价于 system
                "system" | "developer" => {
                    let t = text_of(m.get("content"));
                    if !t.is_empty() {
                        system.push(t);
                    }
                }
                "assistant" => {
                    let mut blocks: Vec<Value> = Vec::new();
                    let t = text_of(m.get("content"));
                    if !t.is_empty() {
                        blocks.push(json!({ "type": "text", "text": t }));
                    }
                    if let Some(calls) = m.get("tool_calls").and_then(|c| c.as_array()) {
                        for (i, c) in calls.iter().enumerate() {
                            let args = c
                                .pointer("/function/arguments")
                                .and_then(|x| x.as_str())
                                .unwrap_or("{}");
                            blocks.push(json!({
                                "type": "tool_use",
                                "id": c.get("id").and_then(|x| x.as_str())
                                    .map(|s| s.to_string())
                                    .unwrap_or_else(|| format!("toolu_{i}")),
                                "name": c.pointer("/function/name").and_then(|x| x.as_str()).unwrap_or(""),
                                // 上游回的是字符串，Anthropic 要对象
                                "input": serde_json::from_str::<Value>(args).unwrap_or_else(|_| json!({})),
                            }));
                        }
                    }
                    if !blocks.is_empty() {
                        push_msg(&mut msgs, "assistant", Value::Array(blocks));
                    }
                }
                // 工具结果在 Anthropic 里是 user 消息里的 tool_result 块，
                // 而且连续多个结果必须塞进同一条消息——分开发会被拒。
                "tool" => {
                    let id = m.get("tool_call_id").and_then(|x| x.as_str()).unwrap_or_default();
                    let block = json!({
                        "type": "tool_result",
                        "tool_use_id": id,
                        "content": text_of(m.get("content")),
                    });
                    append_block(&mut msgs, "user", block);
                }
                _ => {
                    let blocks = user_blocks(m.get("content"));
                    if !blocks.is_empty() {
                        push_msg(&mut msgs, "user", Value::Array(blocks));
                    }
                }
            }
        }
    }

    if !system.is_empty() {
        out.insert("system".into(), json!(system.join("\n\n")));
    }
    out.insert("messages".into(), Value::Array(msgs));

    if let Some(tools) = oa.get("tools").and_then(|t| t.as_array()) {
        let converted: Vec<Value> = tools
            .iter()
            .filter_map(|t| {
                let f = t.get("function")?;
                Some(json!({
                    "name": f.get("name").and_then(|x| x.as_str())?,
                    "description": f.get("description").cloned().unwrap_or(json!("")),
                    "input_schema": f.get("parameters").cloned().unwrap_or_else(|| json!({ "type": "object" })),
                }))
            })
            .collect();
        if !converted.is_empty() {
            out.insert("tools".into(), Value::Array(converted));
            if let Some(tc) = oa.get("tool_choice") {
                let mapped = match tc {
                    Value::String(s) if s == "required" => json!({ "type": "any" }),
                    Value::String(s) if s == "none" => json!({ "type": "none" }),
                    Value::Object(o) => {
                        let name = o
                            .get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|x| x.as_str())
                            .unwrap_or_default();
                        json!({ "type": "tool", "name": name })
                    }
                    _ => json!({ "type": "auto" }),
                };
                out.insert("tool_choice".into(), mapped);
            }
        }
    }

    Value::Object(out)
}

/// 把一条消息塞进去；和上一条同角色就合并内容块。
fn push_msg(msgs: &mut Vec<Value>, role: &str, content: Value) {
    if let Some(last) = msgs.last_mut() {
        if last.get("role").and_then(|r| r.as_str()) == Some(role) {
            if let (Some(a), Value::Array(add)) = (last.get_mut("content"), &content) {
                if let Value::Array(arr) = a {
                    arr.extend(add.iter().cloned());
                    return;
                }
            }
        }
    }
    msgs.push(json!({ "role": role, "content": content }));
}

fn append_block(msgs: &mut Vec<Value>, role: &str, block: Value) {
    push_msg(msgs, role, Value::Array(vec![block]));
}

fn text_of(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| {
                p.get("text")
                    .or_else(|| p.get("content"))
                    .and_then(|t| t.as_str())
            })
            .collect::<Vec<_>>()
            .join(""),
        Some(Value::Object(o)) => o
            .get("text")
            .and_then(|t| t.as_str())
            .map(|s| s.to_string())
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// OpenAI 的 user content（字符串或分区数组）→ Anthropic 内容块。
fn user_blocks(v: Option<&Value>) -> Vec<Value> {
    match v {
        Some(Value::String(s)) => {
            if s.is_empty() {
                Vec::new()
            } else {
                vec![json!({ "type": "text", "text": s })]
            }
        }
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| {
                let ty = p.get("type").and_then(|x| x.as_str()).unwrap_or("");
                match ty {
                    "text" => p
                        .get("text")
                        .and_then(|t| t.as_str())
                        .map(|t| json!({ "type": "text", "text": t })),
                    "image_url" => {
                        let url = p
                            .pointer("/image_url/url")
                            .or_else(|| p.get("image_url"))
                            .and_then(|u| u.as_str())?;
                        Some(json!({ "type": "image", "source": image_source(url) }))
                    }
                    _ => None,
                }
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// `data:image/png;base64,...` → base64 源；普通 URL → url 源。
fn image_source(url: &str) -> Value {
    if let Some(rest) = url.strip_prefix("data:") {
        let mut it = rest.splitn(2, ',');
        let meta = it.next().unwrap_or("");
        let data = it.next().unwrap_or("");
        let media = meta.split(';').next().unwrap_or("image/png");
        return json!({ "type": "base64", "media_type": media, "data": data });
    }
    json!({ "type": "url", "url": url })
}

/// OpenAI chat → Responses。
///
/// 这里的取舍比 Anthropic 狠：Codex 后端会**拒收**它不认识的字段，
/// 而且被拒时只回一句含糊的 `routing_unsupported`，所以宁可少发。
fn responses_request(oa: &Value) -> Value {
    let mut out = Map::new();
    out.insert(
        "model".into(),
        oa.get("model").cloned().unwrap_or(json!("auto")),
    );
    // 无状态：不带 previous_response_id 就必须 store=false，
    // 否则上游会让下一次只带 id 的请求落到「另一个调用者的响应」上
    out.insert("store".into(), json!(false));
    out.insert(
        "stream".into(),
        json!(oa.get("stream").and_then(|x| x.as_bool()).unwrap_or(false)),
    );

    let mut instructions: Vec<String> = Vec::new();
    let mut input: Vec<Value> = Vec::new();

    if let Some(list) = oa.get("messages").and_then(|m| m.as_array()) {
        for m in list {
            let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("user");
            match role {
                "system" | "developer" => {
                    let t = text_of(m.get("content"));
                    if !t.is_empty() {
                        instructions.push(t);
                    }
                }
                "assistant" => {
                    // 先正文、后 function_call：和模型实际的产出顺序一致，
                    // 反过来的话上游会认为「先调工具再说话」，历史读起来就错位了
                    let t = text_of(m.get("content"));
                    if !t.is_empty() {
                        input.push(json!({
                            "type": "message",
                            "role": "assistant",
                            "content": [{ "type": "output_text", "text": t }],
                        }));
                    }
                    if let Some(calls) = m.get("tool_calls").and_then(|c| c.as_array()) {
                        for (i, c) in calls.iter().enumerate() {
                            input.push(json!({
                                "type": "function_call",
                                "call_id": c.get("id").and_then(|x| x.as_str())
                                    .map(|s| s.to_string())
                                    .unwrap_or_else(|| format!("call_{i}")),
                                "name": c.pointer("/function/name").and_then(|x| x.as_str()).unwrap_or(""),
                                "arguments": c.pointer("/function/arguments").and_then(|x| x.as_str()).unwrap_or("{}"),
                            }));
                        }
                    }
                }
                "tool" => {
                    input.push(json!({
                        "type": "function_call_output",
                        "call_id": m.get("tool_call_id").and_then(|x| x.as_str()).unwrap_or_default(),
                        "output": text_of(m.get("content")),
                    }));
                }
                _ => {
                    let parts = user_blocks(m.get("content"));
                    if !parts.is_empty() {
                        // Responses 的输入图片类型叫 input_image，文本叫 input_text
                        let content: Vec<Value> = parts
                            .iter()
                            .map(|b| match b.get("type").and_then(|x| x.as_str()) {
                                Some("image") => {
                                    let src = b.get("source").cloned().unwrap_or(Value::Null);
                                    let url = match src.get("type").and_then(|t| t.as_str()) {
                                        Some("base64") => format!(
                                            "data:{};base64,{}",
                                            src.get("media_type").and_then(|x| x.as_str()).unwrap_or("image/png"),
                                            src.get("data").and_then(|x| x.as_str()).unwrap_or("")
                                        ),
                                        _ => src.get("url").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                                    };
                                    json!({ "type": "input_image", "image_url": url })
                                }
                                _ => json!({
                                    "type": "input_text",
                                    "text": b.get("text").and_then(|x| x.as_str()).unwrap_or(""),
                                }),
                            })
                            .collect();
                        input.push(json!({ "type": "message", "role": "user", "content": content }));
                    }
                }
            }
        }
    }

    if !instructions.is_empty() {
        out.insert("instructions".into(), json!(instructions.join("\n\n")));
    }
    out.insert("input".into(), Value::Array(input));

    if let Some(tools) = oa.get("tools").and_then(|t| t.as_array()) {
        let converted: Vec<Value> = tools
            .iter()
            .filter_map(|t| {
                let f = t.get("function")?;
                Some(json!({
                    "type": "function",
                    "name": f.get("name").and_then(|x| x.as_str())?,
                    "description": f.get("description").cloned().unwrap_or(json!("")),
                    "parameters": f.get("parameters").cloned().unwrap_or_else(|| json!({ "type": "object" })),
                    "strict": false,
                }))
            })
            .collect();
        if !converted.is_empty() {
            out.insert("tools".into(), Value::Array(converted));
            if let Some(tc) = oa.get("tool_choice") {
                let mapped = match tc {
                    Value::String(s) if s == "required" => json!("required"),
                    Value::String(s) if s == "none" => json!("none"),
                    Value::Object(o) => json!({
                        "type": "function",
                        "name": o
                            .get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|x| x.as_str())
                            .unwrap_or_default(),
                    }),
                    _ => json!("auto"),
                };
                out.insert("tool_choice".into(), mapped);
            }
        }
    }

    // 思考档位：只搬客户端显式写的那个，绝不替它默认开。
    // 给一个非推理模型塞 reasoning 会被拒。
    if let Some(effort) = oa.get("reasoning_effort").and_then(|x| x.as_str()) {
        out.insert(
            "reasoning".into(),
            json!({ "effort": effort, "summary": "auto" }),
        );
        // 开了 reasoning 就必须要求回加密思考内容，否则下一轮带不回来
        out.insert("include".into(), json!(["reasoning.encrypted_content"]));
    }

    Value::Object(out)
}

/// OpenAI chat → Gemini generateContent。
///
/// Gemini 是 proto-JSON：**不认识的键会整单拒收**（不是忽略），所以请求体
/// 全部从零构造，OpenAI 的 `stream_options` 之类的键一个都不带。工具的
/// JSON Schema 也要洗一遍——`type` 必须大写枚举、`additionalProperties`
/// 这类 proto Schema 没有的键必须剥掉。
fn gemini_request(oa: &Value) -> Value {
    let mut out = Map::new();
    let mut system: Vec<String> = Vec::new();
    let mut contents: Vec<Value> = Vec::new();
    // call_id → 函数名。Gemini 的工具结果按**函数名**回填（functionResponse
    // 不带 id），名字只能从配对的 assistant 消息里查——OpenAI 历史里
    // tool 消息只引用 tool_call_id。
    let mut call_names: std::collections::HashMap<String, String> = Default::default();

    if let Some(list) = oa.get("messages").and_then(|m| m.as_array()) {
        for m in list {
            let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("user");
            match role {
                "system" | "developer" => {
                    let t = text_of(m.get("content"));
                    if !t.is_empty() {
                        system.push(t);
                    }
                }
                "assistant" => {
                    let mut parts: Vec<Value> = Vec::new();
                    let t = text_of(m.get("content"));
                    if !t.is_empty() {
                        parts.push(json!({ "text": t }));
                    }
                    if let Some(calls) = m.get("tool_calls").and_then(|c| c.as_array()) {
                        for (i, c) in calls.iter().enumerate() {
                            let id = c
                                .get("id")
                                .and_then(|x| x.as_str())
                                .map(|s| s.to_string())
                                .unwrap_or_else(|| format!("call_{i}"));
                            let name = c
                                .pointer("/function/name")
                                .and_then(|x| x.as_str())
                                .unwrap_or("")
                                .to_string();
                            call_names.insert(id, name.clone());
                            let args = c
                                .pointer("/function/arguments")
                                .and_then(|x| x.as_str())
                                .unwrap_or("{}");
                            parts.push(json!({
                                "functionCall": {
                                    "name": name,
                                    // 上游给的是字符串，Gemini 要对象
                                    "args": serde_json::from_str::<Value>(args)
                                        .unwrap_or_else(|_| json!({})),
                                }
                            }));
                        }
                    }
                    if !parts.is_empty() {
                        push_parts(&mut contents, "model", parts);
                    }
                }
                "tool" => {
                    let id = m.get("tool_call_id").and_then(|x| x.as_str()).unwrap_or_default();
                    let name = call_names.get(id).cloned().unwrap_or_default();
                    let content = text_of(m.get("content"));
                    // functionResponse.response 必须是对象：裸字符串会被拒。
                    // 包一层 {"result": …} 是各家 OpenAI→Gemini 代理的通用做法。
                    let part = json!({
                        "functionResponse": { "name": name, "response": { "result": content } }
                    });
                    push_parts(&mut contents, "user", vec![part]);
                }
                _ => {
                    let parts = gemini_user_parts(m.get("content"));
                    if !parts.is_empty() {
                        push_parts(&mut contents, "user", parts);
                    }
                }
            }
        }
    }

    if !system.is_empty() {
        out.insert(
            "systemInstruction".into(),
            json!({ "parts": [{ "text": system.join("\n\n") }] }),
        );
    }
    out.insert("contents".into(), Value::Array(contents));

    // 采样参数全进 generationConfig，这是 Gemini 和 OpenAI 最容易踩错的地方
    let mut gc = Map::new();
    if let Some(mt) = oa
        .get("max_tokens")
        .or_else(|| oa.get("max_completion_tokens"))
        .filter(|x| !x.is_null())
    {
        gc.insert("maxOutputTokens".into(), mt.clone());
    }
    for (from, to) in [("temperature", "temperature"), ("top_p", "topP")] {
        if let Some(x) = oa.get(from) {
            gc.insert(to.into(), x.clone());
        }
    }
    match oa.get("stop") {
        Some(Value::String(s)) => {
            gc.insert("stopSequences".into(), json!([s.clone()]));
        }
        Some(Value::Array(a)) if !a.is_empty() => {
            gc.insert("stopSequences".into(), Value::Array(a.clone()));
        }
        _ => {}
    }
    // 思考档位：只搬客户端显式写的那个。effort 没有对应的官方字段，
    // 映射到 thinkingBudget（都在 2.5 系模型的合法区间内）：
    // low=1024 / medium=8192 / high=24576（flash 上限即 24576）。
    if let Some(effort) = oa.get("reasoning_effort").and_then(|x| x.as_str()) {
        let budget = match effort {
            "low" => 1024,
            "high" => 24_576,
            _ => 8_192,
        };
        gc.insert("thinkingConfig".into(), json!({ "thinkingBudget": budget }));
    }
    if !gc.is_empty() {
        out.insert("generationConfig".into(), Value::Object(gc));
    }

    if let Some(tools) = oa.get("tools").and_then(|t| t.as_array()) {
        let decls: Vec<Value> = tools
            .iter()
            .filter_map(|t| {
                let f = t.get("function")?;
                Some(json!({
                    "name": f.get("name").and_then(|x| x.as_str())?,
                    "description": f.get("description").cloned().unwrap_or(json!("")),
                    "parameters": gemini_schema(
                        f.get("parameters").unwrap_or(&json!({ "type": "object" })),
                    ),
                }))
            })
            .collect();
        if !decls.is_empty() {
            out.insert("tools".into(), json!([{ "functionDeclarations": decls }]));
            if let Some(tc) = oa.get("tool_choice") {
                let cfg = match tc {
                    Value::String(s) if s == "required" => json!({ "mode": "ANY" }),
                    Value::String(s) if s == "none" => json!({ "mode": "NONE" }),
                    Value::Object(o) => {
                        let name = o
                            .get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|x| x.as_str())
                            .unwrap_or_default();
                        json!({ "mode": "ANY", "allowedFunctionNames": [name] })
                    }
                    _ => json!({ "mode": "AUTO" }),
                };
                out.insert("toolConfig".into(), json!({ "functionCallingConfig": cfg }));
            }
        }
    }

    Value::Object(out)
}

/// 把一条消息的内容块塞进去；和上一条同角色就合并——Gemini 对相邻同角色
/// 的 contents 虽然不总是拒绝，但合并后的历史读起来才和 OpenAI 语义一致。
fn push_parts(contents: &mut Vec<Value>, role: &str, parts: Vec<Value>) {
    if let Some(last) = contents.last_mut() {
        if last.get("role").and_then(|r| r.as_str()) == Some(role) {
            if let Some(arr) = last.get_mut("parts").and_then(|p| p.as_array_mut()) {
                arr.extend(parts);
                return;
            }
        }
    }
    contents.push(json!({ "role": role, "parts": parts }));
}

/// OpenAI 的 user content（字符串或分区数组）→ Gemini parts。
///
/// 远程 URL 图片被丢弃：Gemini 的 REST 端点只收 inlineData（base64）或
/// 先经 Files API 上传的 fileUri，我们没有代上传——猜着发一个 URL 上游
/// 会 400，还不如不发。
fn gemini_user_parts(v: Option<&Value>) -> Vec<Value> {
    match v {
        Some(Value::String(s)) => {
            if s.is_empty() {
                Vec::new()
            } else {
                vec![json!({ "text": s })]
            }
        }
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| {
                let ty = p.get("type").and_then(|x| x.as_str()).unwrap_or("");
                match ty {
                    "text" => p
                        .get("text")
                        .and_then(|t| t.as_str())
                        .filter(|t| !t.is_empty())
                        .map(|t| json!({ "text": t })),
                    "image_url" => {
                        let url = p
                            .pointer("/image_url/url")
                            .or_else(|| p.get("image_url"))
                            .and_then(|u| u.as_str())?;
                        let data = url.strip_prefix("data:")?;
                        let mut it = data.splitn(2, ',');
                        let meta = it.next().unwrap_or("");
                        let body = it.next()?;
                        let media = meta.split(';').next().unwrap_or("image/png");
                        Some(json!({ "inlineData": { "mimeType": media, "data": body } }))
                    }
                    _ => None,
                }
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// OpenAPI JSON Schema → Gemini proto Schema。
///
/// proto 的枚举按名字精确匹配，`"object"` 会被拒，必须是 `"OBJECT"`；
/// `additionalProperties` / `$schema` / `default` 这些 proto Schema 没有的键
/// 见到就整单 400，必须剥掉。`properties` 里的键是**用户定义的字段名**，
/// 不是 schema 关键字，必须原样保留、只递归洗值。
fn gemini_schema(v: &Value) -> Value {
    match v {
        Value::Object(o) => {
            let mut out = Map::new();
            for (k, val) in o {
                match k.as_str() {
                    "type" => {
                        if let Some(t) = val.as_str() {
                            out.insert("type".into(), json!(t.to_ascii_uppercase()));
                        } else {
                            out.insert("type".into(), val.clone());
                        }
                    }
                    "properties" => {
                        let mut props = Map::new();
                        if let Some(obj) = val.as_object() {
                            for (pk, pv) in obj {
                                props.insert(pk.clone(), gemini_schema(pv));
                            }
                        }
                        out.insert("properties".into(), Value::Object(props));
                    }
                    "description" | "required" | "items" | "enum" | "format" => {
                        out.insert(k.clone(), gemini_schema(val));
                    }
                    _ => {}
                }
            }
            Value::Object(out)
        }
        Value::Array(a) => Value::Array(a.iter().map(gemini_schema).collect()),
        other => other.clone(),
    }
}

/* ── 响应：上游 → OpenAI ─────────────────────────── */

pub fn response(wire: Wire, native: &Value, model: &str) -> Value {
    match wire {
        Wire::Openai => native.clone(),
        Wire::Anthropic => anthropic_response(native, model),
        Wire::Responses => responses_response(native, model),
        Wire::Gemini => gemini_response(native, model),
    }
}

fn anthropic_response(v: &Value, model: &str) -> Value {
    let mut text = String::new();
    let mut thinking = String::new();
    let mut calls: Vec<Value> = Vec::new();

    if let Some(blocks) = v.get("content").and_then(|c| c.as_array()) {
        for b in blocks {
            match b.get("type").and_then(|t| t.as_str()).unwrap_or("") {
                "text" => text.push_str(b.get("text").and_then(|t| t.as_str()).unwrap_or("")),
                "thinking" => {
                    thinking.push_str(b.get("thinking").and_then(|t| t.as_str()).unwrap_or(""))
                }
                "tool_use" => {
                    let args = b.get("input").cloned().unwrap_or_else(|| json!({}));
                    calls.push(json!({
                        "id": b.get("id").and_then(|x| x.as_str()).unwrap_or(""),
                        "type": "function",
                        "function": {
                            "name": b.get("name").and_then(|x| x.as_str()).unwrap_or(""),
                            "arguments": serde_json::to_string(&args).unwrap_or_else(|_| "{}".into()),
                        }
                    }));
                }
                _ => {}
            }
        }
    }

    let mut msg = Map::new();
    msg.insert("role".into(), json!("assistant"));
    msg.insert("content".into(), json!(text));
    if !thinking.is_empty() {
        msg.insert("reasoning_content".into(), json!(thinking));
    }
    if !calls.is_empty() {
        msg.insert("tool_calls".into(), Value::Array(calls));
    }

    let finish = match v.get("stop_reason").and_then(|s| s.as_str()).unwrap_or("end_turn") {
        "max_tokens" => "length",
        "tool_use" => "tool_calls",
        "refusal" => "content_filter",
        _ => "stop",
    };
    let input = v.pointer("/usage/input_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
    let output = v.pointer("/usage/output_tokens").and_then(|x| x.as_u64()).unwrap_or(0);

    chat_completion(v.get("id").and_then(|x| x.as_str()), model, Value::Object(msg), finish, input, output)
}

fn responses_response(v: &Value, model: &str) -> Value {
    let mut text = String::new();
    let mut thinking = String::new();
    let mut calls: Vec<Value> = Vec::new();

    if let Some(items) = v.get("output").and_then(|o| o.as_array()) {
        for item in items {
            match item.get("type").and_then(|t| t.as_str()).unwrap_or("") {
                "message" => {
                    if let Some(parts) = item.get("content").and_then(|c| c.as_array()) {
                        for p in parts {
                            match p.get("type").and_then(|t| t.as_str()).unwrap_or("") {
                                "output_text" => text
                                    .push_str(p.get("text").and_then(|t| t.as_str()).unwrap_or("")),
                                "refusal" => text.push_str(
                                    p.get("refusal").and_then(|t| t.as_str()).unwrap_or(""),
                                ),
                                _ => {}
                            }
                        }
                    }
                }
                "reasoning" => {
                    if let Some(s) = item.get("summary").and_then(|s| s.as_array()) {
                        for p in s {
                            thinking.push_str(
                                p.get("text").and_then(|t| t.as_str()).unwrap_or(""),
                            );
                        }
                    }
                }
                "function_call" => {
                    calls.push(json!({
                        "id": item.get("call_id").and_then(|x| x.as_str()).unwrap_or(""),
                        "type": "function",
                        "function": {
                            "name": item.get("name").and_then(|x| x.as_str()).unwrap_or(""),
                            "arguments": item.get("arguments").and_then(|x| x.as_str()).unwrap_or("{}"),
                        }
                    }));
                }
                _ => {}
            }
        }
    }

    // 上游判了 incomplete（比如 max_output_tokens 用尽）要如实转成 length，
    // 否则客户端会以为回答完整、把截断的半句话当结论
    let incomplete = v
        .get("incomplete_details")
        .and_then(|d| d.get("reason"))
        .and_then(|r| r.as_str());
    let finish = if !calls.is_empty() {
        "tool_calls"
    } else if v.get("status").and_then(|s| s.as_str()) == Some("incomplete") || incomplete.is_some() {
        "length"
    } else {
        "stop"
    };

    let mut msg = Map::new();
    msg.insert("role".into(), json!("assistant"));
    msg.insert("content".into(), json!(text));
    if !thinking.is_empty() {
        msg.insert("reasoning_content".into(), json!(thinking));
    }
    if !calls.is_empty() {
        msg.insert("tool_calls".into(), Value::Array(calls));
    }

    let input = v.pointer("/usage/input_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
    let output = v.pointer("/usage/output_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
    chat_completion(v.get("id").and_then(|x| x.as_str()), model, Value::Object(msg), finish, input, output)
}

/// Gemini generateContent → OpenAI chat.completion。
fn gemini_response(v: &Value, model: &str) -> Value {
    let mut text = String::new();
    let mut thinking = String::new();
    let mut calls: Vec<Value> = Vec::new();

    if let Some(parts) = v.pointer("/candidates/0/content/parts").and_then(|p| p.as_array()) {
        for p in parts {
            if p.get("thought").and_then(|x| x.as_bool()).unwrap_or(false) {
                thinking.push_str(p.get("text").and_then(|t| t.as_str()).unwrap_or(""));
            } else if let Some(fc) = p.get("functionCall") {
                // Gemini 不给 tool_call id：按出现顺序生成，客户端下轮回传的
                // tool_call_id 就是它——下一轮 gemini_request 靠它查回函数名
                let idx = calls.len();
                let args = fc.get("args").cloned().unwrap_or_else(|| json!({}));
                calls.push(json!({
                    "id": format!("call_{idx}"),
                    "type": "function",
                    "function": {
                        "name": fc.get("name").and_then(|x| x.as_str()).unwrap_or(""),
                        "arguments": serde_json::to_string(&args).unwrap_or_else(|_| "{}".into()),
                    }
                }));
            } else {
                text.push_str(p.get("text").and_then(|t| t.as_str()).unwrap_or(""));
            }
        }
    }

    let mut finish = gemini_finish(
        v.pointer("/candidates/0/finishReason")
            .and_then(|x| x.as_str())
            .unwrap_or(""),
    );
    // 整单被安全策略拦下时 promptFeedback 里有 blockReason、candidates 可能为空
    if let Some(block) = v.pointer("/promptFeedback/blockReason").and_then(|x| x.as_str()) {
        if finish == "stop" || text.is_empty() {
            finish = "content_filter";
            text.clear();
            tracing::debug!(block = %block, "gemini 响应被安全策略拦截");
        }
    }
    if !calls.is_empty() {
        finish = "tool_calls";
    }

    let mut msg = Map::new();
    msg.insert("role".into(), json!("assistant"));
    msg.insert("content".into(), json!(text));
    if !thinking.is_empty() {
        msg.insert("reasoning_content".into(), json!(thinking));
    }
    if !calls.is_empty() {
        msg.insert("tool_calls".into(), Value::Array(calls));
    }

    let input = v
        .pointer("/usageMetadata/promptTokenCount")
        .and_then(|x| x.as_u64())
        .unwrap_or(0);
    // 思考 token 也是真实输出，账本必须认账
    let output = v
        .pointer("/usageMetadata/candidatesTokenCount")
        .and_then(|x| x.as_u64())
        .unwrap_or(0)
        + v.pointer("/usageMetadata/thoughtsTokenCount")
            .and_then(|x| x.as_u64())
            .unwrap_or(0);
    chat_completion(v.get("responseId").and_then(|x| x.as_str()), model, Value::Object(msg), finish, input, output)
}

/// Gemini finishReason → OpenAI finish_reason。
fn gemini_finish(fr: &str) -> &'static str {
    match fr {
        "MAX_TOKENS" => "length",
        "SAFETY" | "RECITATION" | "BLOCKLIST" | "PROHIBITED_CONTENT" | "SPII" => "content_filter",
        "MALFORMED_FUNCTION_CALL" => "stop",
        _ => "stop",
    }
}

fn chat_completion(
    id: Option<&str>,
    model: &str,
    message: Value,
    finish: &str,
    input: u64,
    output: u64,
) -> Value {
    json!({
        "id": id.map(|s| s.to_string()).unwrap_or_else(|| format!("chatcmpl-{}", &uuid::Uuid::new_v4().simple().to_string()[..16])),
        "object": "chat.completion",
        "created": chrono::Utc::now().timestamp(),
        "model": model,
        "choices": [{
            "index": 0,
            "message": message,
            "finish_reason": finish,
        }],
        "usage": {
            // 上游有缓存命中时 `input_tokens` 是「未命中部分」，这里不拆：
            // 拆错会让我们自己的账本比上游宽松，宁可报大
            "prompt_tokens": input,
            "completion_tokens": output,
            "total_tokens": input + output,
        }
    })
}

/* ── 流式：上游 SSE → OpenAI SSE ─────────────────── */

/// 把上游的原生 SSE 流改写成 OpenAI `chat.completion.chunk` 流。
///
/// 只在 `wire != Openai` 时用。返回的 Item 类型与输入一致，
/// 可以直接替掉原来那条流，不影响上游的 hedge / 结算逻辑。
pub fn rewrite(
    wire: Wire,
    model: String,
    input: BoxStream<'static, Result<Bytes, reqwest::Error>>,
) -> BoxStream<'static, Result<Bytes, reqwest::Error>> {
    Box::pin(Down {
        wire,
        model,
        inner: input,
        buf: String::new(),
        out: VecDeque::new(),
        started: false,
        finished: false,
        done: false,
        tool_slots: Vec::new(),
        input_tokens: 0,
        output_tokens: 0,
        finish: None,
    })
}

struct Down {
    wire: Wire,
    model: String,
    inner: BoxStream<'static, Result<Bytes, reqwest::Error>>,
    buf: String,
    out: VecDeque<Bytes>,
    /// 已经发过「角色声明」那一块
    started: bool,
    /// 已经发过收尾块
    finished: bool,
    done: bool,
    /// Responses 的 function_call 是「先建项后补参数」，要按 output_index 占位
    tool_slots: Vec<String>,
    input_tokens: u64,
    output_tokens: u64,
    finish: Option<String>,
}

impl Down {
    fn chunk(&mut self, delta: Value, finish: Option<&str>) -> Bytes {
        let choices = json!([{
            "index": 0,
            "delta": delta,
            "finish_reason": finish,
        }]);
        let frame = json!({
            "id": "chatcmpl-freeport",
            "object": "chat.completion.chunk",
            "created": chrono::Utc::now().timestamp(),
            "model": self.model,
            "choices": choices,
        });
        Bytes::from(format!("data: {frame}\n\n"))
    }

    fn ensure_start(&mut self) {
        if self.started {
            return;
        }
        self.started = true;
        let b = self.chunk(json!({ "role": "assistant", "content": "" }), None);
        self.out.push_back(b);
    }

    fn text(&mut self, t: &str) {
        if t.is_empty() {
            return;
        }
        self.ensure_start();
        let b = self.chunk(json!({ "content": t }), None);
        self.out.push_back(b);
    }

    fn think(&mut self, t: &str) {
        if t.is_empty() {
            return;
        }
        self.ensure_start();
        let b = self.chunk(json!({ "reasoning_content": t }), None);
        self.out.push_back(b);
    }

    fn tool_delta(&mut self, index: usize, id: Option<&str>, name: Option<&str>, args: &str) {
        self.ensure_start();
        let mut call = Map::new();
        call.insert("index".into(), json!(index));
        if let Some(id) = id {
            call.insert("id".into(), json!(id));
            call.insert("type".into(), json!("function"));
        }
        let mut f = Map::new();
        if let Some(n) = name {
            f.insert("name".into(), json!(n));
        }
        f.insert("arguments".into(), json!(args));
        call.insert("function".into(), Value::Object(f));
        let b = self.chunk(json!({ "tool_calls": [Value::Object(call)] }), None);
        self.out.push_back(b);
    }

    fn finish(&mut self, reason: &str) {
        if self.finished {
            return;
        }
        self.ensure_start();
        self.finished = true;
        let b = self.chunk(json!({}), Some(reason));
        self.out.push_back(b);
        // usage 单独一帧、choices 为空——和 OpenAI 自己的 `include_usage` 一致
        let usage = json!({
            "id": "chatcmpl-freeport",
            "object": "chat.completion.chunk",
            "created": chrono::Utc::now().timestamp(),
            "model": self.model,
            "choices": [],
            "usage": {
                "prompt_tokens": self.input_tokens,
                "completion_tokens": self.output_tokens,
                "total_tokens": self.input_tokens + self.output_tokens,
            }
        });
        self.out.push_back(Bytes::from(format!("data: {usage}\n\n")));
        self.out.push_back(Bytes::from_static(b"data: [DONE]\n\n"));
        self.done = true;
    }

    /// 按空行切 SSE 帧。不完整的尾巴留在 buf 里等下一片。
    fn drain(&mut self) {
        loop {
            let Some(idx) = self.buf.find("\n\n") else {
                return;
            };
            let frame: String = self.buf.drain(..idx + 2).collect();
            let data = frame
                .lines()
                .filter_map(|l| l.strip_prefix("data:"))
                .map(str::trim)
                .collect::<Vec<_>>()
                .join("");
            if data.is_empty() || data == "[DONE]" {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(&data) else {
                continue; // 帧没收全或不是 JSON：丢掉，别猜
            };
            match self.wire {
                Wire::Anthropic => self.on_anthropic(&v),
                Wire::Responses => self.on_responses(&v),
                Wire::Gemini => self.on_gemini(&v),
                Wire::Openai => {}
            }
        }
    }

    fn on_anthropic(&mut self, v: &Value) {
        let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match ty {
            "message_start" => {
                // 这里的 usage 是「输入 token」，必须先记下来：
                // 收尾的 message_delta 只带输出
                if let Some(n) = v.pointer("/message/usage/input_tokens").and_then(|x| x.as_u64()) {
                    self.input_tokens = n;
                }
                self.ensure_start();
            }
            "content_block_start" => {
                let block = v.get("content_block").cloned().unwrap_or(Value::Null);
                if block.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                    let idx = v.get("index").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
                    self.tool_delta(
                        idx,
                        block.get("id").and_then(|x| x.as_str()),
                        block.get("name").and_then(|x| x.as_str()),
                        "",
                    );
                }
            }
            "content_block_delta" => {
                let d = v.get("delta").cloned().unwrap_or(Value::Null);
                match d.get("type").and_then(|t| t.as_str()).unwrap_or("") {
                    "text_delta" => {
                        let t = d.get("text").and_then(|x| x.as_str()).unwrap_or("");
                        self.text(t);
                    }
                    "thinking_delta" => {
                        let t = d.get("thinking").and_then(|x| x.as_str()).unwrap_or("");
                        self.think(t);
                    }
                    "input_json_delta" => {
                        let idx = v.get("index").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
                        let partial = d
                            .get("partial_json")
                            .and_then(|x| x.as_str())
                            .unwrap_or("");
                        self.tool_delta(idx, None, None, partial);
                    }
                    _ => {}
                }
            }
            "message_delta" => {
                if let Some(n) = v.pointer("/usage/output_tokens").and_then(|x| x.as_u64()) {
                    self.output_tokens = n;
                }
                if let Some(r) = v.pointer("/delta/stop_reason").and_then(|x| x.as_str()) {
                    self.finish = Some(
                        match r {
                            "max_tokens" => "length",
                            "tool_use" => "tool_calls",
                            "refusal" => "content_filter",
                            _ => "stop",
                        }
                        .to_string(),
                    );
                }
            }
            "message_stop" => {
                let f = self.finish.clone().unwrap_or_else(|| "stop".into());
                self.finish(&f);
            }
            "error" => {
                let msg = v
                    .pointer("/error/message")
                    .and_then(|x| x.as_str())
                    .unwrap_or("上游返回错误")
                    .to_string();
                self.emit_error(&msg);
            }
            _ => {}
        }
    }

    fn on_responses(&mut self, v: &Value) {
        let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match ty {
            "response.created" | "response.in_progress" => self.ensure_start(),
            "response.output_text.delta" => {
                let t = v.get("delta").and_then(|x| x.as_str()).unwrap_or("");
                self.text(t);
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                let t = v.get("delta").and_then(|x| x.as_str()).unwrap_or("");
                self.think(t);
            }
            "response.output_item.added" => {
                let item = v.get("item").cloned().unwrap_or(Value::Null);
                if item.get("type").and_then(|t| t.as_str()) == Some("function_call") {
                    let idx = v.get("output_index").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
                    let id = item
                        .get("call_id")
                        .or_else(|| item.get("id"))
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                    // 记住 index→call_id：后面的参数增量只带 item_id，不再带 call_id
                    if self.tool_slots.len() <= idx {
                        self.tool_slots.resize(idx + 1, String::new());
                    }
                    self.tool_slots[idx] = id.clone();
                    self.tool_delta(
                        idx,
                        Some(&id),
                        item.get("name").and_then(|x| x.as_str()),
                        "",
                    );
                }
            }
            "response.function_call_arguments.delta" => {
                let idx = v.get("output_index").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
                let delta = v.get("delta").and_then(|x| x.as_str()).unwrap_or("");
                self.tool_delta(idx, None, None, delta);
            }
            "response.completed" => {
                if let Some(u) = v.pointer("/response/usage") {
                    self.input_tokens = u.get("input_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
                    self.output_tokens =
                        u.get("output_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
                }
                let incomplete = v
                    .pointer("/response/incomplete_details/reason")
                    .and_then(|x| x.as_str())
                    .is_some();
                let reason = if !self.tool_slots.is_empty() {
                    "tool_calls"
                } else if incomplete {
                    "length"
                } else {
                    "stop"
                };
                self.finish(reason);
            }
            "response.incomplete" => {
                let reason = if self.tool_slots.is_empty() { "length" } else { "tool_calls" };
                self.finish(reason);
            }
            "error" | "response.failed" => {
                let msg = v
                    .pointer("/error/message")
                    .or_else(|| v.pointer("/response/error/message"))
                    .and_then(|x| x.as_str())
                    .unwrap_or("上游返回错误")
                    .to_string();
                self.emit_error(&msg);
            }
            _ => {}
        }
    }

    /// Gemini 的流式帧就是 `GenerateContentResponse`，没有独立的事件类型：
    /// 文本增量在 `parts[].text`，工具调用整体出现在一个 part 里（没有参数
    /// 分片），`usageMetadata` 逐帧累计，`finishReason` 只在最后一帧出现。
    /// 帧结束没有 `[DONE]`——断流时由 `poll_next` 的兜底补收尾。
    fn on_gemini(&mut self, v: &Value) {
        if let Some(e) = v.get("error") {
            let msg = e
                .get("message")
                .and_then(|x| x.as_str())
                .unwrap_or("上游返回错误")
                .to_string();
            self.emit_error(&msg);
            return;
        }
        if let Some(parts) = v.pointer("/candidates/0/content/parts").and_then(|p| p.as_array()) {
            for p in parts {
                if p.get("thought").and_then(|x| x.as_bool()).unwrap_or(false) {
                    self.think(p.get("text").and_then(|t| t.as_str()).unwrap_or(""));
                } else if let Some(fc) = p.get("functionCall") {
                    let idx = self.tool_slots.len();
                    let name = fc.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string();
                    let id = format!("call_{idx}");
                    // Gemini 的参数是完整对象，一次性发给客户端
                    let args = serde_json::to_string(&fc.get("args").cloned().unwrap_or_else(|| json!({})))
                        .unwrap_or_else(|_| "{}".into());
                    self.tool_slots.push(name.clone());
                    self.tool_delta(idx, Some(&id), Some(&name), &args);
                } else {
                    self.text(p.get("text").and_then(|t| t.as_str()).unwrap_or(""));
                }
            }
        }
        // 逐帧累计：中间帧的 candidatesTokenCount 是增量到当前的值，取 max 即总量
        if let Some(u) = v.get("usageMetadata") {
            self.input_tokens = self
                .input_tokens
                .max(u.get("promptTokenCount").and_then(|x| x.as_u64()).unwrap_or(0));
            let out = u.get("candidatesTokenCount").and_then(|x| x.as_u64()).unwrap_or(0)
                + u.get("thoughtsTokenCount").and_then(|x| x.as_u64()).unwrap_or(0);
            self.output_tokens = self.output_tokens.max(out);
        }
        if let Some(fr) = v.pointer("/candidates/0/finishReason").and_then(|x| x.as_str()) {
            let f = if !self.tool_slots.is_empty() && fr == "STOP" {
                "tool_calls"
            } else {
                gemini_finish(fr)
            };
            self.finish(f);
        } else if v.pointer("/promptFeedback/blockReason").is_some() {
            // 没有内容也没有 finishReason、只有拦截标记：如实告诉客户端被拦了
            self.finish("content_filter");
        }
    }

    fn emit_error(&mut self, msg: &str) {
        let frame = json!({ "error": { "message": msg, "type": "upstream_error" } });
        self.out.push_back(Bytes::from(format!("data: {frame}\n\n")));
        self.out
            .push_back(Bytes::from_static(b"data: [DONE]\n\n"));
        self.done = true;
    }
}

impl Stream for Down {
    type Item = Result<Bytes, reqwest::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let me = self.get_mut();
        loop {
            if let Some(b) = me.out.pop_front() {
                return Poll::Ready(Some(Ok(b)));
            }
            if me.done {
                return Poll::Ready(None);
            }
            match me.inner.as_mut().poll_next(cx) {
                Poll::Ready(Some(Ok(chunk))) => {
                    me.buf.push_str(&String::from_utf8_lossy(&chunk));
                    me.drain();
                }
                Poll::Ready(Some(Err(e))) => {
                    me.emit_error(&e.to_string());
                }
                Poll::Ready(None) => {
                    // 上游没发收尾事件就断了：补一个 stop，
                    // 否则客户端会一直等 finish_reason，表现出来就是「卡住了」
                    if !me.finished {
                        let f = me.finish.clone().unwrap_or_else(|| "stop".into());
                        me.finish(&f);
                    }
                    me.done = true;
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oa_req() -> Value {
        json!({
            "model": "claude-sonnet-4",
            "max_tokens": 512,
            "stream": false,
            "messages": [
                // OpenAI 形态里 system 是一条消息，不是顶层字段
                { "role": "system", "content": "you are terse" },
                { "role": "user", "content": "hi" },
                { "role": "assistant", "content": "hello", "tool_calls": [
                    { "id": "c1", "type": "function", "function": { "name": "ls", "arguments": "{\"p\":\"/\"}" } }
                ]},
                { "role": "tool", "tool_call_id": "c1", "content": "/bin" },
                { "role": "tool", "tool_call_id": "c2", "content": "/etc" }
            ],
            "tools": [{ "type": "function", "function": { "name": "ls", "parameters": { "type": "object" } } }],
            "tool_choice": "required"
        })
    }

    #[test]
    fn wire_parsing_defaults_to_openai() {
        assert_eq!(Wire::parse(""), Wire::Openai);
        assert_eq!(Wire::parse("gpt-whatever"), Wire::Openai);
        assert_eq!(Wire::parse("Anthropic"), Wire::Anthropic);
        assert_eq!(Wire::parse("responses"), Wire::Responses);
        assert_eq!(Wire::Anthropic.path(), "/messages");
        assert_eq!(Wire::Responses.path(), "/responses");
    }

    #[test]
    fn anthropic_request_lifts_system_and_keeps_tool_pairs_together() {
        let out = anthropic_request(&oa_req());
        assert!(out.get("messages").is_some());
        assert_eq!(out["system"], "you are terse");
        assert_eq!(out["max_tokens"], 512);
        assert_eq!(out["tools"][0]["name"], "ls");
        assert_eq!(out["tool_choice"]["type"], "any");

        let msgs = out["messages"].as_array().unwrap();
        // user / assistant(带 tool_use) / user(两个 tool_result 合在一条)
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[1]["content"][1]["type"], "tool_use");
        assert_eq!(msgs[1]["content"][1]["input"]["p"], "/");
        let results = msgs[2]["content"].as_array().unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["type"], "tool_result");
        assert_eq!(results[1]["tool_use_id"], "c2");
    }

    #[test]
    fn anthropic_request_fills_missing_max_tokens() {
        // Anthropic 的 max_tokens 必填，缺了会 400
        let out = anthropic_request(&json!({ "model": "x", "messages": [] }));
        assert_eq!(out["max_tokens"], 4096);
    }

    #[test]
    fn data_url_images_become_base64_sources() {
        let out = anthropic_request(&json!({
            "model": "x",
            "messages": [{ "role": "user", "content": [
                { "type": "text", "text": "看图" },
                { "type": "image_url", "image_url": { "url": "data:image/png;base64,AAAA" } }
            ]}]
        }));
        let blocks = out["messages"][0]["content"].as_array().unwrap();
        assert_eq!(blocks[1]["type"], "image");
        assert_eq!(blocks[1]["source"]["type"], "base64");
        assert_eq!(blocks[1]["source"]["media_type"], "image/png");
        assert_eq!(blocks[1]["source"]["data"], "AAAA");
    }

    #[test]
    fn responses_request_is_stateless_and_drops_unsupported_fields() {
        let out = responses_request(&oa_req());
        assert_eq!(out["store"], false);
        assert!(out.get("temperature").is_none());
        assert!(out.get("max_output_tokens").is_none(), "Codex 拒收 max_output_tokens");
        assert_eq!(out["instructions"], "you are terse");
        assert_eq!(out["tools"][0]["type"], "function");
        assert_eq!(out["tool_choice"], "required");

        let input = out["input"].as_array().unwrap();
        let kinds: Vec<&str> = input
            .iter()
            .map(|i| i["type"].as_str().unwrap_or(""))
            .collect();
        assert_eq!(
            kinds,
            vec![
                "message",             // user
                "message",             // assistant 的正文
                "function_call",       // assistant 的 tool_call
                "function_call_output",// 第一个工具结果
                "function_call_output",// 第二个
            ]
        );
        assert_eq!(input[2]["call_id"], "c1");
        assert_eq!(input[4]["call_id"], "c2");
    }

    #[test]
    fn reasoning_is_only_sent_when_the_caller_asks_for_it() {
        // 默认不开：给非推理模型塞 reasoning 会被拒
        assert!(responses_request(&oa_req()).get("reasoning").is_none());

        let mut with = oa_req();
        with["reasoning_effort"] = json!("high");
        let out = responses_request(&with);
        assert_eq!(out["reasoning"]["effort"], "high");
        assert_eq!(out["include"][0], "reasoning.encrypted_content");
    }

    #[test]
    fn anthropic_response_carries_text_thinking_and_tools() {
        let v = json!({
            "id": "msg_1",
            "content": [
                { "type": "thinking", "thinking": "想一想" },
                { "type": "text", "text": "答案" },
                { "type": "tool_use", "id": "t1", "name": "ls", "input": { "p": "/" } }
            ],
            "stop_reason": "tool_use",
            "usage": { "input_tokens": 11, "output_tokens": 7 }
        });
        let out = anthropic_response(&v, "claude-sonnet-4");
        assert_eq!(out["choices"][0]["message"]["content"], "答案");
        assert_eq!(out["choices"][0]["message"]["reasoning_content"], "想一想");
        assert_eq!(out["choices"][0]["message"]["tool_calls"][0]["id"], "t1");
        assert_eq!(out["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(out["usage"]["prompt_tokens"], 11);
        assert_eq!(out["usage"]["total_tokens"], 18);
    }

    #[test]
    fn responses_truncation_is_reported_as_length_not_success() {
        let v = json!({
            "id": "resp_1",
            "status": "incomplete",
            "incomplete_details": { "reason": "max_output_tokens" },
            "output": [{ "type": "message", "content": [{ "type": "output_text", "text": "半句" }] }],
            "usage": { "input_tokens": 5, "output_tokens": 2 }
        });
        let out = responses_response(&v, "gpt-5");
        assert_eq!(out["choices"][0]["finish_reason"], "length");
        assert_eq!(out["choices"][0]["message"]["content"], "半句");
    }

    #[test]
    fn responses_function_call_becomes_tool_calls() {
        let v = json!({
            "id": "resp_2",
            "status": "completed",
            "output": [
                { "type": "function_call", "call_id": "c9", "name": "ls", "arguments": "{\"p\":\"/\"}" }
            ]
        });
        let out = responses_response(&v, "gpt-5");
        assert_eq!(out["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(out["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"], "{\"p\":\"/\"}");
    }

    /* ── 流式 ── */

    fn drive(wire: Wire, frames: &[&str]) -> Vec<Value> {
        use futures_util::StreamExt;
        let chunks: Vec<Result<Bytes, reqwest::Error>> = frames
            .iter()
            .map(|f| Ok(Bytes::from(f.to_string())))
            .collect();
        let input = futures_util::stream::iter(chunks).boxed();
        let out = rewrite(wire, "m".into(), input);

        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let bytes = runtime.block_on(async {
            use futures_util::StreamExt;
            let mut all = Vec::new();
            let mut s = out;
            while let Some(Ok(b)) = s.next().await {
                all.extend_from_slice(&b);
            }
            all
        });
        String::from_utf8_lossy(&bytes)
            .lines()
            .filter_map(|l| l.strip_prefix("data: "))
            .filter(|d| *d != "[DONE]")
            .filter_map(|d| serde_json::from_str::<Value>(d).ok())
            .collect()
    }

    #[test]
    fn anthropic_sse_becomes_openai_chunks() {
        let out = drive(Wire::Anthropic, &[
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":9}}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"你\"}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"好\"}}\n\n",
            "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":2}}\n\n",
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
        ]);

        // 第一块必须是角色声明，客户端据此建立消息
        assert_eq!(out[0]["choices"][0]["delta"]["role"], "assistant");
        let text: String = out
            .iter()
            .filter_map(|c| c.pointer("/choices/0/delta/content").and_then(|x| x.as_str()))
            .collect();
        assert_eq!(text, "你好");
        let stop = out
            .iter()
            .find(|c| c.pointer("/choices/0/finish_reason").map(|f| !f.is_null()).unwrap_or(false))
            .expect("必须有收尾块");
        assert_eq!(stop["choices"][0]["finish_reason"], "stop");
        // usage 那一帧在收尾块之后、choices 为空
        let usage = out
            .iter()
            .find(|c| c.get("usage").is_some())
            .expect("必须带 usage，账本靠它");
        assert_eq!(usage["usage"]["prompt_tokens"], 9);
        assert_eq!(usage["usage"]["completion_tokens"], 2);
    }

    #[test]
    fn a_frame_split_across_tcp_chunks_is_still_parsed() {
        let out = drive(Wire::Anthropic, &[
            "event: content_block_delta\ndata: {\"type\":\"content_bl",
            "ock_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"x\"}}\n\n",
        ]);
        let text: String = out
            .iter()
            .filter_map(|c| c.pointer("/choices/0/delta/content").and_then(|x| x.as_str()))
            .collect();
        assert_eq!(text, "x");
    }

    #[test]
    fn a_stream_that_dies_without_message_stop_still_finishes() {
        // 上游断流时客户端不能永远等 finish_reason，否则看起来就是卡死
        use futures_util::StreamExt;
        let chunks: Vec<Result<Bytes, reqwest::Error>> = vec![Ok(Bytes::from_static(
            b"data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"a\"}}\n\n",
        ))];
        let input = futures_util::stream::iter(chunks).boxed();
        let out = rewrite(Wire::Anthropic, "m".into(), input);
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let text = runtime.block_on(async {
            use futures_util::StreamExt;
            let mut all = Vec::new();
            let mut s = out;
            while let Some(Ok(b)) = s.next().await {
                all.extend_from_slice(&b);
            }
            String::from_utf8_lossy(&all).to_string()
        });
        assert!(text.contains("\"finish_reason\":\"stop\""));
        assert!(text.contains("[DONE]"));
    }

    /* ── Gemini 原生上游 ── */

    #[test]
    fn gemini_url_encodes_model_and_stream() {
        let base = "https://generativelanguage.googleapis.com/v1beta";
        assert_eq!(
            url(Wire::Gemini, base, "gemini-2.5-flash", false),
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:generateContent"
        );
        assert_eq!(
            url(Wire::Gemini, base, "gemini-2.5-flash", true),
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:streamGenerateContent?alt=sse"
        );
        // 其余协议仍是固定路径
        assert_eq!(url(Wire::Anthropic, "https://x/v1/", "m", true), "https://x/v1/messages");
    }

    #[test]
    fn gemini_request_lifts_system_and_tools() {
        let out = gemini_request(&oa_req());
        assert_eq!(
            out["systemInstruction"]["parts"][0]["text"],
            "you are terse",
            "system 消息进 systemInstruction"
        );
        assert_eq!(out["generationConfig"]["maxOutputTokens"], 512);
        assert!(out.get("stream").is_none(), "stream 编在 URL 里，不在 body");
        assert!(out.get("model").is_none(), "模型名编在 URL 里，不在 body");

        let decls = out["tools"][0]["functionDeclarations"].as_array().unwrap();
        assert_eq!(decls[0]["name"], "ls");
        assert_eq!(
            decls[0]["parameters"]["type"], "OBJECT",
            "proto 枚举必须大写，小写会被整单拒收"
        );
        assert_eq!(
            out["toolConfig"]["functionCallingConfig"]["mode"], "ANY",
            "required → ANY"
        );
        // role: tool 的两条结果按函数名回填，且合并进同一条 user contents
        let contents = out["contents"].as_array().unwrap();
        let kinds: Vec<String> = contents
            .iter()
            .map(|c| c["role"].as_str().unwrap_or("").to_string())
            .collect();
        assert_eq!(kinds, vec!["user", "model", "user"]);
        let fr = contents[2]["parts"][0]["functionResponse"].clone();
        assert_eq!(fr["name"], "ls", "函数名来自配对的 assistant tool_calls");
        assert_eq!(fr["response"]["result"], "/bin");
    }

    #[test]
    fn gemini_request_keeps_data_url_images_and_drops_remote() {
        let out = gemini_request(&json!({
            "model": "x",
            "messages": [{ "role": "user", "content": [
                { "type": "text", "text": "看图" },
                { "type": "image_url", "image_url": { "url": "data:image/png;base64,AAAA" } },
                { "type": "image_url", "image_url": { "url": "https://example.com/a.png" } }
            ]}]
        }));
        let parts = out["contents"][0]["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 2, "远程 URL 图片没有 fileUri，只能丢弃");
        assert_eq!(parts[1]["inlineData"]["mimeType"], "image/png");
        assert_eq!(parts[1]["inlineData"]["data"], "AAAA");
    }

    #[test]
    fn gemini_schema_sanitizes_unknown_keys() {
        let out = gemini_request(&json!({
            "model": "x",
            "messages": [{ "role": "user", "content": "hi" }],
            "tools": [{ "type": "function", "function": {
                "name": "ls",
                "parameters": {
                    "type": "object",
                    "additionalProperties": false,
                    "$schema": "https://json-schema.org/draft/2020-12/schema",
                    "properties": { "p": { "type": "string", "default": "/" } },
                    "required": ["p"]
                }
            }}]
        }));
        let params = &out["tools"][0]["functionDeclarations"][0]["parameters"];
        assert_eq!(params["type"], "OBJECT");
        assert!(params.get("additionalProperties").is_none());
        assert!(params.get("$schema").is_none());
        assert_eq!(params["properties"]["p"]["type"], "STRING");
        assert!(params["properties"]["p"].get("default").is_none());
    }

    #[test]
    fn gemini_response_carries_text_thoughts_tools_and_usage() {
        let v = json!({
            "responseId": "resp_g",
            "candidates": [{
                "content": { "role": "model", "parts": [
                    { "text": "想一想" , "thought": true },
                    { "text": "答案" },
                    { "functionCall": { "name": "ls", "args": { "p": "/" } } }
                ]},
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 11,
                "candidatesTokenCount": 7,
                "thoughtsTokenCount": 3
            }
        });
        let out = gemini_response(&v, "gemini-2.5-flash");
        let msg = &out["choices"][0]["message"];
        assert_eq!(msg["content"], "答案");
        assert_eq!(msg["reasoning_content"], "想一想");
        assert_eq!(msg["tool_calls"][0]["id"], "call_0", "Gemini 不给 id，按序生成");
        assert_eq!(msg["tool_calls"][0]["function"]["arguments"], "{\"p\":\"/\"}");
        assert_eq!(out["choices"][0]["finish_reason"], "tool_calls");
        // 思考 token 也是真实输出，账本必须认账
        assert_eq!(out["usage"]["prompt_tokens"], 11);
        assert_eq!(out["usage"]["completion_tokens"], 10);
    }

    #[test]
    fn gemini_truncation_and_blocks_are_reported_honestly() {
        let truncated = json!({
            "candidates": [{ "content": { "parts": [{ "text": "半句" }] }, "finishReason": "MAX_TOKENS" }],
            "usageMetadata": { "promptTokenCount": 5, "candidatesTokenCount": 2 }
        });
        let out = gemini_response(&truncated, "m");
        assert_eq!(out["choices"][0]["finish_reason"], "length");

        let blocked = json!({
            "promptFeedback": { "blockReason": "SAFETY" },
            "usageMetadata": { "promptTokenCount": 5, "candidatesTokenCount": 0 }
        });
        let out = gemini_response(&blocked, "m");
        assert_eq!(out["choices"][0]["finish_reason"], "content_filter");
    }

    #[test]
    fn gemini_sse_becomes_openai_chunks() {
        let out = drive(Wire::Gemini, &[
            "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"你\"}]}}],\"usageMetadata\":{\"promptTokenCount\":9,\"candidatesTokenCount\":1}}\n\n",
            "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"好\"}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":9,\"candidatesTokenCount\":2}}\n\n",
        ]);

        assert_eq!(out[0]["choices"][0]["delta"]["role"], "assistant");
        let text: String = out
            .iter()
            .filter_map(|c| c.pointer("/choices/0/delta/content").and_then(|x| x.as_str()))
            .collect();
        assert_eq!(text, "你好");
        let stop = out
            .iter()
            .find(|c| c.pointer("/choices/0/finish_reason").map(|f| !f.is_null()).unwrap_or(false))
            .expect("必须有收尾块");
        assert_eq!(stop["choices"][0]["finish_reason"], "stop");
        let usage = out
            .iter()
            .find(|c| c.get("usage").is_some())
            .expect("必须带 usage，账本靠它");
        assert_eq!(usage["usage"]["prompt_tokens"], 9);
        assert_eq!(usage["usage"]["completion_tokens"], 2);
    }

    #[test]
    fn gemini_sse_tool_call_arrives_complete_in_one_part() {
        let out = drive(Wire::Gemini, &[
            "data: {\"candidates\":[{\"content\":{\"parts\":[{\"functionCall\":{\"name\":\"ls\",\"args\":{\"p\":\"/\"}}}]},\"finishReason\":\"STOP\"}]}\n\n",
        ]);
        let delta = &out[1]["choices"][0]["delta"];
        let call = &delta["tool_calls"][0];
        assert_eq!(call["id"], "call_0");
        assert_eq!(call["function"]["name"], "ls");
        assert_eq!(call["function"]["arguments"], "{\"p\":\"/\"}");
        let stop = out
            .iter()
            .find(|c| c.pointer("/choices/0/finish_reason").map(|f| !f.is_null()).unwrap_or(false))
            .expect("必须有收尾块");
        assert_eq!(stop["choices"][0]["finish_reason"], "tool_calls");
    }

    #[test]
    fn gemini_wire_parses_from_config() {
        assert_eq!(Wire::parse("gemini"), Wire::Gemini);
        assert_eq!(Wire::parse("google"), Wire::Gemini);
        assert_eq!(Wire::Gemini.as_str(), "gemini");
    }
}
