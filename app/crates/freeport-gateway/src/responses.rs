//! OpenAI Responses 协议适配（`/v1/responses`）。
//!
//! Codex CLI 默认说 Responses，而免费厂商基本只会 Chat Completions。
//! 这里做的是「最小闭环」：
//! - 请求侧把 `input`（item 数组）摊平成 `messages`，`instructions` 搬到 system
//! - 响应侧把 chat 结果重新组装成 `output` 数组，流式逐块改成语义事件
//!
//! 刻意不做的事，每一条都有代价，写在这里免得以后被当成 bug：
//! - **不实现 `previous_response_id`**：网关无状态，Codex 每轮会把完整历史放进 `input`，
//!   真有工具依赖这个字段时应当报错而不是假装接住——目前静默忽略并在文档里标注。
//! - **不支持 hosted tools**（`web_search` / `file_search` / `local_shell`）：
//!   它们没有 `name`，塞给上游会变成无名函数，Gemini 这类严格校验的厂商直接 400。
//! - **不落 `store`**：网关不存会话，`store: true` 在这里等价于没有。
//! - **不透传 `reasoning.effort` / `parallel_tool_calls`**：严格校验的兼容端点会因
//!   多一个不认识的字段直接 400，而 400 会触发换家、白烧一次额度。

use std::collections::VecDeque;

use axum::body::Body;
use bytes::Bytes;
use futures_util::stream::Stream;
use serde_json::{json, Map, Value};

/* ── 请求：Responses → Chat Completions ─────────── */

/// 把 `/v1/responses` 的请求体转成 `/v1/chat/completions` 的请求体。
pub fn to_chat(v: &Value) -> Value {
    let mut out = Map::new();
    out.insert("model".into(), v.get("model").cloned().unwrap_or(json!("auto")));
    out.insert(
        "stream".into(),
        json!(v.get("stream").and_then(|x| x.as_bool()).unwrap_or(false)),
    );

    if let Some(mt) = v.get("max_output_tokens").and_then(|x| x.as_u64()) {
        out.insert("max_tokens".into(), json!(mt));
    }
    // 只透传「免费厂商普遍认」的两个采样参数。
    // `reasoning.effort` / `parallel_tool_calls` 看着无害，但严格校验的兼容端点
    // （Groq、Gemini 的 OpenAI 层）会因为多一个不认识的字段直接 400，
    // 而 400 会触发换家、白烧一次额度。少一个旋钮比多一次失败划算。
    for k in ["temperature", "top_p"] {
        if let Some(x) = v.get(k) {
            out.insert(k.into(), x.clone());
        }
    }
    // text.format 是 Responses 的结构化输出写法，chat 侧叫 response_format
    if let Some(fmt) = v.pointer("/text/format") {
        match fmt.get("type").and_then(|x| x.as_str()) {
            Some("json_schema") => {
                out.insert(
                    "response_format".into(),
                    json!({
                        "type": "json_schema",
                        "json_schema": {
                            "name": fmt.get("name").cloned().unwrap_or(json!("freeport")),
                            "schema": fmt.get("schema").cloned().unwrap_or(json!({})),
                            "strict": fmt.get("strict").cloned().unwrap_or(json!(false))
                        }
                    }),
                );
            }
            Some("json_object") => {
                out.insert("response_format".into(), json!({ "type": "json_object" }));
            }
            _ => {}
        }
    }

    let mut messages: Vec<Value> = Vec::new();
    if let Some(sys) = v.get("instructions").and_then(|x| x.as_str()) {
        if !sys.trim().is_empty() {
            messages.push(json!({ "role": "system", "content": sys }));
        }
    }

    // 思考内容要挂到下一个 assistant 上，chat 侧没有独立的 reasoning 消息
    let mut pending_reasoning = String::new();
    let mut extra_tools: Vec<Value> = Vec::new();

    match v.get("input") {
        Some(Value::String(s)) => messages.push(json!({ "role": "user", "content": s })),
        Some(Value::Array(items)) => {
            for item in items {
                // 部分 CLI 只发 role 不发 type，按 message 处理
                let kind = item
                    .get("type")
                    .and_then(|x| x.as_str())
                    .unwrap_or(if item.get("role").is_some() { "message" } else { "" });
                match kind {
                    "message" => {
                        let role = item.get("role").and_then(|x| x.as_str()).unwrap_or("user");
                        let mut m = Map::new();
                        m.insert("role".into(), json!(role));
                        m.insert("content".into(), map_content(item.get("content")));
                        if role == "assistant" && !pending_reasoning.is_empty() {
                            m.insert("reasoning_content".into(), json!(pending_reasoning.clone()));
                            pending_reasoning.clear();
                        } else if role != "assistant" {
                            pending_reasoning.clear();
                        }
                        messages.push(Value::Object(m));
                    }
                    "function_call" | "custom_tool_call" => {
                        let custom = kind == "custom_tool_call";
                        let name = item.get("name").and_then(|x| x.as_str()).unwrap_or("");
                        // 无名调用是 Codex 自己也会拒绝的形态，直接丢掉
                        if name.trim().is_empty() {
                            continue;
                        }
                        let call_id = item
                            .get("call_id")
                            .or_else(|| item.get("id"))
                            .and_then(|x| x.as_str())
                            .unwrap_or("call_0");
                        let args = if custom {
                            let input = item.get("input").cloned().unwrap_or(json!(""));
                            json!({ "input": input }).to_string()
                        } else {
                            arg_string(item.get("arguments"))
                        };
                        let call = json!({
                            "id": call_id,
                            "type": "function",
                            "function": { "name": name, "arguments": args }
                        });
                        // 连续多个 function_call 属于同一个 assistant 轮次
                        let appended = messages
                            .last_mut()
                            .filter(|m| {
                                m.get("role").and_then(|x| x.as_str()) == Some("assistant")
                                    && m.get("tool_calls").is_some()
                            })
                            .map(|m| {
                                if let Some(list) = m.get_mut("tool_calls").and_then(|x| x.as_array_mut()) {
                                    list.push(call.clone());
                                }
                            });
                        if appended.is_none() {
                            let mut m = Map::new();
                            m.insert("role".into(), json!("assistant"));
                            m.insert("content".into(), Value::Null);
                            m.insert("tool_calls".into(), json!([call]));
                            if !pending_reasoning.is_empty() {
                                m.insert("reasoning_content".into(), json!(pending_reasoning.clone()));
                                pending_reasoning.clear();
                            }
                            messages.push(Value::Object(m));
                        }
                    }
                    "function_call_output" | "custom_tool_call_output" => {
                        let out = match item.get("output") {
                            Some(Value::String(s)) => s.clone(),
                            Some(other) => other.to_string(),
                            None => String::new(),
                        };
                        let call_id = item
                            .get("call_id")
                            .or_else(|| item.get("id"))
                            .and_then(|x| x.as_str())
                            .unwrap_or("call_0");
                        messages.push(json!({
                            "role": "tool",
                            "tool_call_id": call_id,
                            "content": out
                        }));
                    }
                    "reasoning" => {
                        let txt = reasoning_text(item);
                        if !txt.is_empty() {
                            if pending_reasoning.is_empty() {
                                pending_reasoning = txt;
                            } else {
                                pending_reasoning.push('\n');
                                pending_reasoning.push_str(&txt);
                            }
                        }
                    }
                    "additional_tools" => {
                        if let Some(list) = item.get("tools").and_then(|x| x.as_array()) {
                            extra_tools.extend(list.iter().cloned());
                        }
                    }
                    // item_reference 依赖服务端存储，无状态网关只能忽略
                    _ => {}
                }
            }
        }
        _ => {}
    }

    if !messages.is_empty() {
        out.insert("messages".into(), Value::Array(messages));
    }

    let mut tools: Vec<Value> = Vec::new();
    for t in v
        .get("tools")
        .and_then(|x| x.as_array())
        .map(|a| a.as_slice())
        .unwrap_or(&[])
        .iter()
        .chain(extra_tools.iter())
    {
        // 已经是 chat 形态的直接透传
        if t.get("function").is_some() {
            tools.push(t.clone());
            continue;
        }
        let Some(name) = t.get("name").and_then(|x| x.as_str()) else { continue };
        if name.trim().is_empty() {
            continue;
        }
        let mut f = Map::new();
        f.insert("name".into(), json!(name));
        if let Some(d) = t.get("description") {
            f.insert("description".into(), d.clone());
        }
        f.insert(
            "parameters".into(),
            t.get("parameters").cloned().unwrap_or(json!({ "type": "object", "properties": {} })),
        );
        tools.push(json!({ "type": "function", "function": Value::Object(f) }));
    }
    if !tools.is_empty() {
        out.insert("tools".into(), Value::Array(tools));
    }

    match v.get("tool_choice") {
        Some(Value::String(s)) => {
            out.insert("tool_choice".into(), json!(s));
        }
        Some(obj @ Value::Object(_)) => {
            if let Some(name) = obj.get("name").and_then(|x| x.as_str()) {
                out.insert(
                    "tool_choice".into(),
                    json!({ "type": "function", "function": { "name": name } }),
                );
            }
        }
        _ => {}
    }

    Value::Object(out)
}

fn arg_string(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => "{}".into(),
        Some(other) => other.to_string(),
    }
}

fn reasoning_text(item: &Value) -> String {
    let pick = |k: &str| -> String {
        item.get(k)
            .and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default()
    };
    let s = pick("summary");
    if s.is_empty() {
        pick("content")
    } else {
        s
    }
}

fn map_content(c: Option<&Value>) -> Value {
    match c {
        Some(Value::String(s)) => json!(s),
        Some(Value::Array(parts)) => {
            let mut out: Vec<Value> = Vec::new();
            for p in parts {
                match p.get("type").and_then(|x| x.as_str()).unwrap_or("") {
                    "input_text" | "output_text" | "text" => out.push(json!({
                        "type": "text",
                        "text": p.get("text").cloned().unwrap_or(json!(""))
                    })),
                    "input_image" => {
                        let url = p
                            .get("image_url")
                            .and_then(|x| x.as_str())
                            .or_else(|| p.get("file_id").and_then(|x| x.as_str()))
                            .unwrap_or("");
                        out.push(json!({
                            "type": "image_url",
                            "image_url": {
                                "url": url,
                                "detail": p.get("detail").and_then(|x| x.as_str()).unwrap_or("auto")
                            }
                        }));
                    }
                    "refusal" => out.push(json!({
                        "type": "text",
                        "text": p.get("refusal").cloned().unwrap_or(json!(""))
                    })),
                    _ => out.push(p.clone()),
                }
            }
            // 纯文本塌成字符串：不少免费端点对 content 数组支持不全
            if out.len() == 1 && out[0].get("type").and_then(|x| x.as_str()) == Some("text") {
                out[0].get("text").cloned().unwrap_or(json!(""))
            } else {
                Value::Array(out)
            }
        }
        _ => Value::Null,
    }
}

/* ── 响应：Chat Completions → Responses ─────────── */

pub fn error_body(message: &str, code: &str) -> Value {
    json!({
        "error": {
            "message": message,
            "type": "freeport_error",
            "param": null,
            "code": code
        }
    })
}

fn usage_json(in_tokens: u64, out_tokens: u64, reasoning: u64) -> Value {
    json!({
        "input_tokens": in_tokens,
        "input_tokens_details": { "cached_tokens": 0 },
        "output_tokens": out_tokens,
        "output_tokens_details": { "reasoning_tokens": reasoning },
        "total_tokens": in_tokens + out_tokens
    })
}

fn message_item(id: String, text: String, status: &str) -> Value {
    json!({
        "type": "message",
        "id": id,
        "status": status,
        "role": "assistant",
        "content": [{
            "type": "output_text",
            "text": text,
            "annotations": []
        }]
    })
}

fn call_item(id: String, call_id: String, name: String, args: String, status: &str) -> Value {
    json!({
        "type": "function_call",
        "id": id,
        "call_id": call_id,
        "name": name,
        "arguments": args,
        "status": status
    })
}

/// 把 chat 的非流式响应组装成 Responses 对象。
pub fn from_chat(v: &Value, model: &str) -> Value {
    let msg = v.pointer("/choices/0/message").cloned().unwrap_or(json!({}));
    let finish = v
        .pointer("/choices/0/finish_reason")
        .and_then(|x| x.as_str())
        .unwrap_or("");
    let trimmed = finish == "length";

    let mut output: Vec<Value> = Vec::new();
    let think = msg
        .get("reasoning_content")
        .or_else(|| msg.get("reasoning"))
        .and_then(|x| x.as_str())
        .unwrap_or("");
    if !think.is_empty() {
        output.push(json!({
            "type": "reasoning",
            "id": new_id("rs"),
            "summary": [{ "type": "summary_text", "text": think }]
        }));
    }

    let text = msg.get("content").and_then(|x| x.as_str()).unwrap_or("");
    let tool_calls = msg.get("tool_calls").and_then(|x| x.as_array());
    // content 为 null 且只有工具调用时，不要造一个空的 message 项
    if !text.is_empty() || tool_calls.map(|t| t.is_empty()).unwrap_or(true) {
        output.push(message_item(new_id("msg"), text.to_string(), "completed"));
    }
    if let Some(calls) = tool_calls {
        for (i, c) in calls.iter().enumerate() {
            let name = c.pointer("/function/name").and_then(|x| x.as_str()).unwrap_or("");
            if name.is_empty() {
                continue;
            }
            let call_id = c
                .get("id")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("call_{i}"));
            output.push(call_item(
                new_id("fc"),
                call_id,
                name.to_string(),
                arg_string(c.pointer("/function/arguments")),
                "completed",
            ));
        }
    }

    let usage = v.get("usage");
    let in_tokens = usage.and_then(|u| u.get("prompt_tokens")).and_then(|x| x.as_u64()).unwrap_or(0);
    let out_tokens = usage
        .and_then(|u| u.get("completion_tokens"))
        .and_then(|x| x.as_u64())
        .unwrap_or(0);
    let reasoning_tokens = usage
        .and_then(|u| u.pointer("/completion_tokens_details/reasoning_tokens"))
        .and_then(|x| x.as_u64())
        .unwrap_or(0);

    json!({
        "id": new_id("resp"),
        "object": "response",
        "created_at": chrono::Utc::now().timestamp(),
        "status": if trimmed { "incomplete" } else { "completed" },
        "model": model,
        "output": output,
        "incomplete_details": if trimmed { json!({ "reason": "max_output_tokens" }) } else { Value::Null },
        "error": Value::Null,
        "instructions": Value::Null,
        "metadata": {},
        "parallel_tool_calls": true,
        "tool_choice": "auto",
        "tools": [],
        "usage": usage_json(in_tokens, out_tokens, reasoning_tokens)
    })
}

fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", &uuid::Uuid::new_v4().simple().to_string()[..24])
}

/* ── 流式：chat SSE → Responses 语义事件 ────────── */

pub fn sse_transform(body: Body, model: String) -> Body {
    Body::from_stream(Xf::new(Box::pin(body.into_data_stream()), model))
}

struct Xf {
    inner: futures_util::stream::BoxStream<'static, Result<Bytes, axum::Error>>,
    buf: String,
    out: VecDeque<Bytes>,
    st: St,
    done: bool,
}

struct St {
    id: String,
    model: String,
    created_at: i64,
    seq: u64,
    started: bool,
    next_index: usize,
    text: Option<TextItem>,
    think: Option<TextItem>,
    tools: Vec<Tool>,
    /// 已收尾的 output item，按 output_index 排序后放进 response.completed
    items: Vec<(usize, Value)>,
    in_tokens: u64,
    out_tokens: u64,
    reasoning_tokens: u64,
    finish: Option<String>,
    completed: bool,
}

struct TextItem {
    index: usize,
    id: String,
    buf: String,
}

struct Tool {
    oi: u64,
    index: usize,
    id: String,
    call_id: String,
    name: String,
    args: String,
    /// 增量里 name 可能被拆成几段，只有拼完才能发 output_item.added
    started: bool,
}

impl Xf {
    fn new(inner: futures_util::stream::BoxStream<'static, Result<Bytes, axum::Error>>, model: String) -> Self {
        Self {
            inner,
            buf: String::new(),
            out: VecDeque::new(),
            st: St {
                id: new_id("resp"),
                model,
                created_at: chrono::Utc::now().timestamp(),
                seq: 0,
                started: false,
                next_index: 0,
                text: None,
                think: None,
                tools: Vec::new(),
                items: Vec::new(),
                in_tokens: 0,
                out_tokens: 0,
                reasoning_tokens: 0,
                finish: None,
                completed: false,
            },
            done: false,
        }
    }

    fn emit(&mut self, kind: &str, mut data: Value) {
        data["type"] = json!(kind);
        data["sequence_number"] = json!(self.st.seq);
        self.st.seq += 1;
        self.out
            .push_back(Bytes::from(format!("event: {kind}\ndata: {data}\n\n")));
    }

    /// `response.created` / `response.in_progress` 只发一次，且必须早于任何内容事件。
    fn ensure_start(&mut self) {
        if self.st.started {
            return;
        }
        self.st.started = true;
        let resp = self.envelope("in_progress");
        self.emit("response.created", json!({ "response": resp.clone() }));
        self.emit("response.in_progress", json!({ "response": resp }));
    }

    fn envelope(&self, status: &str) -> Value {
        // 收尾时已按 output_index 排过序，这里直接按序取出
        let output: Vec<Value> = self.st.items.iter().map(|(_, v)| v.clone()).collect();
        json!({
            "id": self.st.id,
            "object": "response",
            "created_at": self.st.created_at,
            "status": status,
            "model": self.st.model,
            "output": if status == "completed" { Value::Array(output) } else { json!([]) },
            "incomplete_details": Value::Null,
            "error": Value::Null,
            "instructions": Value::Null,
            "metadata": {},
            "parallel_tool_calls": true,
            "tool_choice": "auto",
            "tools": [],
            "usage": Value::Null
        })
    }

    fn on_chunk(&mut self, v: &Value) {
        if let Some(u) = v.get("usage") {
            if let Some(n) = u.get("prompt_tokens").and_then(|x| x.as_u64()) {
                self.st.in_tokens = n;
            }
            if let Some(n) = u.get("completion_tokens").and_then(|x| x.as_u64()) {
                self.st.out_tokens = n;
            }
            if let Some(n) = u
                .pointer("/completion_tokens_details/reasoning_tokens")
                .and_then(|x| x.as_u64())
            {
                self.st.reasoning_tokens = n;
            }
        }

        let Some(choice) = v.pointer("/choices/0") else { return };
        let delta = choice.get("delta").cloned().unwrap_or_else(|| json!({}));

        let think = delta
            .get("reasoning_content")
            .or_else(|| delta.get("reasoning"))
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        if !think.is_empty() {
            self.ensure_start();
            let (index, id) = match &self.st.think {
                Some(t) => (t.index, t.id.clone()),
                None => {
                    let index = self.st.next_index;
                    self.st.next_index += 1;
                    let id = new_id("rs");
                    self.emit(
                        "response.output_item.added",
                        json!({
                            "output_index": index,
                            "item": { "type": "reasoning", "id": id, "summary": [] }
                        }),
                    );
                    self.emit(
                        "response.reasoning_summary_part.added",
                        json!({
                            "item_id": id,
                            "output_index": index,
                            "summary_index": 0,
                            "part": { "type": "summary_text", "text": "" }
                        }),
                    );
                    self.st.think = Some(TextItem { index, id: id.clone(), buf: String::new() });
                    (index, id)
                }
            };
            if let Some(t) = self.st.think.as_mut() {
                t.buf.push_str(&think);
            }
            self.emit(
                "response.reasoning_summary_text.delta",
                json!({
                    "item_id": id,
                    "output_index": index,
                    "summary_index": 0,
                    "delta": think
                }),
            );
        }

        if let Some(text) = delta.get("content").and_then(|x| x.as_str()) {
            if !text.is_empty() {
                self.ensure_start();
                let (index, id) = match &self.st.text {
                    Some(t) => (t.index, t.id.clone()),
                    None => {
                        let index = self.st.next_index;
                        self.st.next_index += 1;
                        let id = new_id("msg");
                        self.emit(
                            "response.output_item.added",
                            json!({
                                "output_index": index,
                                "item": {
                                    "type": "message", "id": id, "status": "in_progress",
                                    "role": "assistant", "content": []
                                }
                            }),
                        );
                        self.emit(
                            "response.content_part.added",
                            json!({
                                "item_id": id,
                                "output_index": index,
                                "content_index": 0,
                                "part": { "type": "output_text", "text": "", "annotations": [] }
                            }),
                        );
                        self.st.text = Some(TextItem { index, id: id.clone(), buf: String::new() });
                        (index, id)
                    }
                };
                if let Some(t) = self.st.text.as_mut() {
                    t.buf.push_str(text);
                }
                self.emit(
                    "response.output_text.delta",
                    json!({
                        "item_id": id,
                        "output_index": index,
                        "content_index": 0,
                        "delta": text
                    }),
                );
            }
        }

        if let Some(calls) = delta.get("tool_calls").and_then(|x| x.as_array()) {
            for c in calls {
                let oi = c.get("index").and_then(|x| x.as_u64()).unwrap_or(0);
                let name_piece = c
                    .pointer("/function/name")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();
                let args = c
                    .pointer("/function/arguments")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();
                let call_id = c
                    .get("id")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();

                // 先更新「上游那个 tool_call 槽位」的累计内容，再把要发的事件参数取出来
                let (index, id, call_id_now, name, started, is_new) = {
                    let pos = match self.st.tools.iter().position(|t| t.oi == oi) {
                        Some(p) => p,
                        None => {
                            let index = self.st.next_index;
                            self.st.next_index += 1;
                            self.st.tools.push(Tool {
                                oi,
                                index,
                                id: new_id("fc"),
                                call_id: if call_id.is_empty() {
                                    format!("call_{oi}")
                                } else {
                                    call_id.clone()
                                },
                                name: String::new(),
                                args: String::new(),
                                started: false,
                            });
                            self.st.tools.len() - 1
                        }
                    };
                    let t = &mut self.st.tools[pos];
                    t.name.push_str(&name_piece);
                    t.args.push_str(&args);
                    let started = t.started;
                    if !started && !t.name.is_empty() {
                        t.started = true;
                    }
                    (t.index, t.id.clone(), t.call_id.clone(), t.name.clone(), started, !started)
                };

                self.ensure_start();
                if is_new && !name.is_empty() {
                    self.emit(
                        "response.output_item.added",
                        json!({
                            "output_index": index,
                            "item": {
                                "type": "function_call", "id": id, "call_id": call_id_now,
                                "name": name, "arguments": "", "status": "in_progress"
                            }
                        }),
                    );
                }
                if !args.is_empty() && (started || is_new) {
                    self.emit(
                        "response.function_call_arguments.delta",
                        json!({ "item_id": id, "output_index": index, "delta": args }),
                    );
                }
            }
        }

        if let Some(f) = choice.get("finish_reason").and_then(|x| x.as_str()) {
            self.st.finish = Some(f.to_string());
        }
    }

    fn drain_frames(&mut self) {
        loop {
            let Some(pos) = self.buf.find("\n\n") else { break };
            let frame: String = self.buf.drain(..pos + 2).collect();
            for line in frame.lines() {
                let Some(rest) = line.strip_prefix("data:") else { continue };
                let rest = rest.trim();
                if rest.is_empty() {
                    continue;
                }
                if rest == "[DONE]" {
                    self.finish_stream();
                    continue;
                }
                if let Ok(v) = serde_json::from_str::<Value>(rest) {
                    // 上游在流里塞 error（免费厂商的老把戏）：转成 response.failed
                    if let Some(err) = v.get("error") {
                        if !self.st.completed {
                            self.ensure_start();
                            let msg = err
                                .get("message")
                                .and_then(|x| x.as_str())
                                .unwrap_or("上游返回错误");
                            let mut resp = self.envelope("failed");
                            resp["error"] = json!({
                                "code": err.get("code").cloned().unwrap_or(json!("upstream_error")),
                                "message": msg
                            });
                            self.emit("response.failed", json!({ "response": resp }));
                            self.st.completed = true;
                            self.done = true;
                        }
                        continue;
                    }
                    self.on_chunk(&v);
                }
            }
        }
    }

    fn finish_stream(&mut self) {
        if self.st.completed {
            return;
        }
        self.ensure_start();
        self.st.completed = true;

        // 收尾按 output_index 升序，客户端才能把 item 顺序拼回去
        let mut closing: Vec<(usize, u8, usize)> = Vec::new();
        if let Some(t) = &self.st.think {
            closing.push((t.index, 0, 0));
        }
        if let Some(t) = &self.st.text {
            closing.push((t.index, 1, 0));
        }
        for (i, t) in self.st.tools.iter().enumerate() {
            closing.push((t.index, 2, i));
        }
        closing.sort_by_key(|(idx, _, _)| *idx);

        for (index, kind, i) in closing {
            if kind == 0 {
                let Some(t) = self.st.think.take() else { continue };
                self.emit(
                    "response.reasoning_summary_text.done",
                    json!({
                        "item_id": t.id, "output_index": index, "summary_index": 0, "text": t.buf
                    }),
                );
                self.emit(
                    "response.reasoning_summary_part.done",
                    json!({
                        "item_id": t.id, "output_index": index, "summary_index": 0,
                        "part": { "type": "summary_text", "text": t.buf }
                    }),
                );
                let item = json!({
                    "type": "reasoning", "id": t.id,
                    "summary": [{ "type": "summary_text", "text": t.buf }]
                });
                self.emit("response.output_item.done", json!({ "output_index": index, "item": item.clone() }));
                self.st.items.push((index, item));
            } else if kind == 1 {
                let Some(t) = self.st.text.take() else { continue };
                self.emit(
                    "response.output_text.done",
                    json!({
                        "item_id": t.id, "output_index": index, "content_index": 0, "text": t.buf
                    }),
                );
                self.emit(
                    "response.content_part.done",
                    json!({
                        "item_id": t.id, "output_index": index, "content_index": 0,
                        "part": { "type": "output_text", "text": t.buf, "annotations": [] }
                    }),
                );
                let item = message_item(t.id, t.buf, "completed");
                self.emit("response.output_item.done", json!({ "output_index": index, "item": item.clone() }));
                self.st.items.push((index, item));
            } else {
                let t = &self.st.tools[i];
                let (index, id, call_id, name, args) =
                    (t.index, t.id.clone(), t.call_id.clone(), t.name.clone(), t.args.clone());
                // 增量里一个 name 都没拼出来时按匿名调用处理，比丢一个已发生的调用好
                let name = if name.is_empty() { "unknown".to_string() } else { name };
                let item = call_item(id.clone(), call_id, name.clone(), args.clone(), "completed");
                self.emit(
                    "response.function_call_arguments.done",
                    json!({ "item_id": id, "output_index": index, "arguments": args }),
                );
                self.emit("response.output_item.done", json!({ "output_index": index, "item": item.clone() }));
                self.st.items.push((index, item));
            }
        }

        self.st.items.sort_by_key(|(i, _)| *i);
        let mut resp = self.envelope("completed");
        resp["usage"] = usage_json(self.st.in_tokens, self.st.out_tokens, self.st.reasoning_tokens);
        if self.st.finish.as_deref() == Some("length") {
            resp["status"] = json!("incomplete");
            resp["incomplete_details"] = json!({ "reason": "max_output_tokens" });
        }
        self.emit("response.completed", json!({ "response": resp }));
        self.done = true;
    }
}

impl Stream for Xf {
    type Item = Result<Bytes, axum::Error>;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let me = self.get_mut();
        loop {
            if let Some(b) = me.out.pop_front() {
                return std::task::Poll::Ready(Some(Ok(b)));
            }
            if me.done {
                return std::task::Poll::Ready(None);
            }
            match me.inner.as_mut().poll_next(cx) {
                std::task::Poll::Ready(Some(Ok(chunk))) => {
                    me.buf.push_str(&String::from_utf8_lossy(&chunk));
                    me.drain_frames();
                }
                std::task::Poll::Ready(Some(Err(e))) => {
                    me.ensure_start();
                    let mut resp = me.envelope("failed");
                    resp["error"] = json!({ "code": "stream_broken", "message": e.to_string() });
                    me.emit("response.failed", json!({ "response": resp }));
                    me.st.completed = true;
                    me.done = true;
                }
                std::task::Poll::Ready(None) => me.finish_stream(),
                std::task::Poll::Pending => return std::task::Poll::Pending,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instructions_and_items_flatten_into_messages() {
        let v = json!({
            "model": "auto",
            "instructions": "be brief",
            "max_output_tokens": 64,
            "input": [
                { "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "hi" }] },
                { "type": "function_call", "call_id": "c1", "name": "ls", "arguments": "{\"p\":\"/\"}" },
                { "type": "function_call_output", "call_id": "c1", "output": "a.txt" }
            ],
            "tools": [{ "type": "function", "name": "ls", "description": "list", "parameters": { "type": "object" } }],
            "tool_choice": { "type": "function", "name": "ls" }
        });
        let o = to_chat(&v);
        assert_eq!(o["messages"][0]["role"], "system");
        assert_eq!(o["messages"][1]["content"], "hi");
        assert_eq!(o["messages"][2]["tool_calls"][0]["id"], "c1");
        assert_eq!(o["messages"][3]["role"], "tool");
        assert_eq!(o["messages"][3]["content"], "a.txt");
        assert_eq!(o["tools"][0]["function"]["name"], "ls");
        assert_eq!(o["tool_choice"]["function"]["name"], "ls");
        assert_eq!(o["max_tokens"], 64);
    }

    #[test]
    fn consecutive_calls_share_one_assistant_turn() {
        let v = json!({
            "model": "auto",
            "input": [
                { "type": "message", "role": "user", "content": "go" },
                { "type": "function_call", "call_id": "c1", "name": "a", "arguments": "{}" },
                { "type": "function_call", "call_id": "c2", "name": "b", "arguments": "{}" }
            ]
        });
        let o = to_chat(&v);
        let calls = o["messages"][1]["tool_calls"].as_array().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(o["messages"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn hosted_tools_are_dropped_not_sent_nameless() {
        let v = json!({
            "model": "auto",
            "input": "hi",
            "tools": [
                { "type": "web_search" },
                { "type": "function", "name": "ls", "parameters": { "type": "object" } }
            ]
        });
        let o = to_chat(&v);
        assert_eq!(o["tools"].as_array().unwrap().len(), 1);
        assert_eq!(o["tools"][0]["function"]["name"], "ls");
    }

    #[test]
    fn chat_result_becomes_response_object() {
        let v = json!({
            "id": "cmpl-1",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "hello",
                    "tool_calls": [{ "id": "c1", "function": { "name": "ls", "arguments": "{\"p\":\"/\"}" } }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": { "prompt_tokens": 9, "completion_tokens": 4, "total_tokens": 13 }
        });
        let r = from_chat(&v, "m1");
        assert_eq!(r["object"], "response");
        assert_eq!(r["status"], "completed");
        assert_eq!(r["output"][0]["type"], "message");
        assert_eq!(r["output"][0]["content"][0]["text"], "hello");
        assert_eq!(r["output"][1]["type"], "function_call");
        assert_eq!(r["output"][1]["call_id"], "c1");
        assert_eq!(r["usage"]["input_tokens"], 9);
        assert_eq!(r["usage"]["total_tokens"], 13);
    }

    #[test]
    fn length_finish_marks_incomplete() {
        let v = json!({
            "choices": [{ "message": { "content": "cut" }, "finish_reason": "length" }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 2 }
        });
        let r = from_chat(&v, "m1");
        assert_eq!(r["status"], "incomplete");
        assert_eq!(r["incomplete_details"]["reason"], "max_output_tokens");
    }
}
