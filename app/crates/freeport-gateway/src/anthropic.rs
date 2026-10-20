//! Anthropic Messages 协议适配。
//!
//! 为什么先做一对一适配而不是 Canonical IR：现在只有两种协议在飞，
//! 抽一层 IR 会多出两个空转换；等第三个协议（Gemini 原生）进来时，
//! 把 `to_openai` / `from_openai` 换成 IR 的两端实现即可，外部接口不变。
//!
//! 三条约定：
//! - 请求在进入路由前转成 OpenAI 形态，**复用同一条接力链**，不另写一套 failover
//! - 响应在离开网关前转回 Anthropic 形态，非流式整体转、流式逐块转
//! - 错误也转：Claude Code 只认 `{"type":"error","error":{...}}`

use std::collections::VecDeque;

use axum::body::Body;
use bytes::Bytes;
use futures_util::stream::Stream;
use serde_json::{json, Map, Value};

/* ── 请求：Anthropic → OpenAI ───────────────────── */

/// 把 Anthropic Messages 请求转成 `/v1/chat/completions` 的请求体。
pub fn to_openai(v: &Value) -> Value {
    let mut out = Map::new();
    out.insert("model".into(), v.get("model").cloned().unwrap_or(json!("auto")));
    out.insert("stream".into(), json!(v.get("stream").and_then(|x| x.as_bool()).unwrap_or(false)));

    if let Some(mt) = v.get("max_tokens") {
        out.insert("max_tokens".into(), mt.clone());
    }
    for k in ["temperature", "top_p"] {
        if let Some(x) = v.get(k) {
            out.insert(k.into(), x.clone());
        }
    }
    if let Some(stop) = v.get("stop_sequences").and_then(|x| x.as_array()) {
        if !stop.is_empty() {
            out.insert("stop".into(), Value::Array(stop.clone()));
        }
    }

    let mut messages: Vec<Value> = Vec::new();
    if let Some(sys) = v.get("system") {
        let text = blocks_text(sys);
        if !text.is_empty() {
            messages.push(json!({ "role": "system", "content": text }));
        }
    }

    if let Some(list) = v.get("messages").and_then(|x| x.as_array()) {
        for m in list {
            let role = m.get("role").and_then(|x| x.as_str()).unwrap_or("user");
            match m.get("content") {
                Some(Value::String(s)) => {
                    messages.push(json!({ "role": role, "content": s }));
                }
                Some(Value::Array(blocks)) => {
                    if role == "assistant" {
                        let (text, tool_calls) = assistant_blocks(blocks);
                        let mut msg = Map::new();
                        msg.insert("role".into(), json!("assistant"));
                        // 只带 tool_calls 时 content 必须省略，否则部分上游会拒
                        if !text.is_empty() {
                            msg.insert("content".into(), json!(text));
                        }
                        if !tool_calls.is_empty() {
                            msg.insert("tool_calls".into(), Value::Array(tool_calls));
                        }
                        if msg.len() > 1 {
                            messages.push(Value::Object(msg));
                        }
                    } else {
                        // tool_result 在 OpenAI 里是独立的一条 role=tool 消息，
                        // 必须排在文本之前，否则会丢上下文
                        for b in blocks {
                            if b.get("type").and_then(|x| x.as_str()) == Some("tool_result") {
                                let id = b
                                    .get("tool_use_id")
                                    .and_then(|x| x.as_str())
                                    .unwrap_or_default();
                                messages.push(json!({
                                    "role": "tool",
                                    "tool_call_id": id,
                                    "content": blocks_text(b.get("content").unwrap_or(&Value::Null)),
                                }));
                            }
                        }
                        let text = user_text(blocks);
                        if text != Value::Null {
                            messages.push(json!({ "role": "user", "content": text }));
                        }
                    }
                }
                _ => {}
            }
        }
    }
    out.insert("messages".into(), Value::Array(messages));

    if let Some(tools) = v.get("tools").and_then(|x| x.as_array()) {
        let converted: Vec<Value> = tools
            .iter()
            .filter_map(|t| {
                let name = t.get("name").and_then(|x| x.as_str())?;
                let mut f = Map::new();
                f.insert("name".into(), json!(name));
                if let Some(d) = t.get("description") {
                    f.insert("description".into(), d.clone());
                }
                f.insert(
                    "parameters".into(),
                    t.get("input_schema").cloned().unwrap_or_else(|| json!({ "type": "object" })),
                );
                Some(json!({ "type": "function", "function": Value::Object(f) }))
            })
            .collect();
        if !converted.is_empty() {
            out.insert("tools".into(), Value::Array(converted));
            if let Some(tc) = v.get("tool_choice") {
                let mapped = match tc.get("type").and_then(|x| x.as_str()) {
                    Some("any") => json!("required"),
                    Some("tool") => json!({
                        "type": "function",
                        "function": { "name": tc.get("name").and_then(|x| x.as_str()).unwrap_or_default() }
                    }),
                    Some("none") => json!("none"),
                    _ => json!("auto"),
                };
                out.insert("tool_choice".into(), mapped);
            }
        }
    }

    Value::Object(out)
}

fn blocks_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .filter_map(|b| b.get("text").and_then(|x| x.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(o) => o.get("text").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        _ => String::new(),
    }
}

/// 用户消息里的文本块，图片转成 data URL。
fn user_text(blocks: &[Value]) -> Value {
    let mut parts: Vec<Value> = Vec::new();
    let mut only_text = true;
    for b in blocks {
        match b.get("type").and_then(|x| x.as_str()) {
            Some("text") => parts.push(json!({
                "type": "text",
                "text": b.get("text").and_then(|x| x.as_str()).unwrap_or("")
            })),
            Some("image") => {
                only_text = false;
                if let Some(src) = b.get("source") {
                    let url = match src.get("type").and_then(|x| x.as_str()) {
                        Some("base64") => format!(
                            "data:{};base64,{}",
                            src.get("media_type").and_then(|x| x.as_str()).unwrap_or("image/png"),
                            src.get("data").and_then(|x| x.as_str()).unwrap_or("")
                        ),
                        _ => src.get("url").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                    };
                    parts.push(json!({ "type": "image_url", "image_url": { "url": url } }));
                }
            }
            _ => {}
        }
    }
    if parts.is_empty() {
        return json!(null);
    }
    if only_text {
        let s = parts
            .iter()
            .filter_map(|p| p.get("text").and_then(|x| x.as_str()))
            .collect::<Vec<_>>()
            .join("\n");
        json!(s)
    } else {
        Value::Array(parts)
    }
}

fn assistant_blocks(blocks: &[Value]) -> (String, Vec<Value>) {
    let mut text: Vec<String> = Vec::new();
    let mut calls: Vec<Value> = Vec::new();
    // thinking 块的 signature 里封着上游要求原样回传的 extra_content
    let mut sealed: Vec<Value> = Vec::new();
    for b in blocks {
        match b.get("type").and_then(|x| x.as_str()) {
            Some("text") => text.push(b.get("text").and_then(|x| x.as_str()).unwrap_or("").to_string()),
            Some("thinking") => {
                // 思考**文本**不回传：多数 OpenAI 兼容端点不接受它。
                // 但签名要带回去——里面是上游的 thought_signature，
                // 丢了 Gemini 3 会直接 400，而且这 400 照样烧额度。
                if let Some(s) = b.get("signature").and_then(|x| x.as_str()) {
                    if let Some(sealed_sig) = crate::sig::open(s) {
                        sealed = sealed_sig.extras;
                    }
                }
            }
            Some("tool_use") => {
                let id = b.get("id").and_then(|x| x.as_str()).unwrap_or("call_0");
                let mut call = json!({
                    "id": id,
                    "type": "function",
                    "function": {
                        "name": b.get("name").and_then(|x| x.as_str()).unwrap_or(""),
                        "arguments": b.get("input").cloned().unwrap_or_else(|| json!({})).to_string(),
                    }
                });
                // 流式路径下签名挂在 tool_use 上（块顺序不允许它回到 thinking 里）
                if let Some(x) = b.get("extra_content") {
                    if !x.is_null() {
                        call["extra_content"] = x.clone();
                    }
                }
                calls.push(call);
            }
            _ => {}
        }
    }
    // 签名里的 extras 按 index 补到还没带 extra_content 的 call 上
    for (i, x) in sealed.iter().enumerate() {
        if x.is_null() {
            continue;
        }
        if let Some(c) = calls.get_mut(i) {
            if c.get("extra_content").is_none() {
                c["extra_content"] = x.clone();
            }
        }
    }
    (text.join("\n"), calls)
}

/* ── 响应：OpenAI → Anthropic ──────────────────── */

pub fn error_body(kind: &str, message: &str) -> Value {
    json!({ "type": "error", "error": { "type": kind, "message": message } })
}

/// 把 OpenAI 的非流式响应体转成 Anthropic Message。
///
/// `provider` 只用于签名里的来源标记，方便排查「这个 thinking 是谁产的」。
pub fn from_openai(v: &Value, provider: &str) -> Value {
    let msg = v
        .pointer("/choices/0/message")
        .cloned()
        .unwrap_or_else(|| json!({}));

    let mut content: Vec<Value> = Vec::new();

    // 思考过程必须显式交回客户端，否则下一轮它带不回来，
    // 上游（Gemini 3 这类）发现少了 thought_signature 就 400。
    let think = msg
        .get("reasoning_content")
        .or_else(|| msg.get("reasoning"))
        .and_then(|x| x.as_str())
        .unwrap_or("");
    if !think.is_empty() {
        let model = v.get("model").and_then(|x| x.as_str()).unwrap_or("freeport");
        content.push(json!({
            "type": "thinking",
            "thinking": think,
            "signature": crate::sig::seal(provider, model, &crate::sig::extras_of(&msg)),
        }));
    }

    if let Some(t) = msg.get("content").and_then(|x| x.as_str()) {
        if !t.is_empty() {
            content.push(json!({ "type": "text", "text": t }));
        }
    } else if let Some(parts) = msg.get("content").and_then(|x| x.as_array()) {
        // 少数上游回的是分段数组
        let text = parts
            .iter()
            .filter_map(|p| p.get("text").and_then(|x| x.as_str()))
            .collect::<Vec<_>>()
            .join("");
        if !text.is_empty() {
            content.push(json!({ "type": "text", "text": text }));
        }
    }

    if let Some(calls) = msg.get("tool_calls").and_then(|x| x.as_array()) {
        for (i, c) in calls.iter().enumerate() {
            let args = c
                .pointer("/function/arguments")
                .and_then(|x| x.as_str())
                .unwrap_or("{}");
            content.push(json!({
                "type": "tool_use",
                "id": c.get("id").and_then(|x| x.as_str()).unwrap_or(&format!("toolu_{i}")),
                "name": c.pointer("/function/name").and_then(|x| x.as_str()).unwrap_or(""),
                "input": serde_json::from_str::<Value>(args).unwrap_or_else(|_| json!({})),
            }));
        }
    }
    if content.is_empty() {
        content.push(json!({ "type": "text", "text": "" }));
    }

    let finish = v
        .pointer("/choices/0/finish_reason")
        .and_then(|x| x.as_str())
        .unwrap_or("stop");
    json!({
        "id": v.get("id").and_then(|x| x.as_str()).map(|s| format!("msg_{s}")).unwrap_or_else(|| "msg_freeport".into()),
        "type": "message",
        "role": "assistant",
        "model": v.get("model").and_then(|x| x.as_str()).unwrap_or("freeport"),
        "content": content,
        "stop_reason": stop_reason(finish),
        "stop_sequence": null,
        "usage": {
            "input_tokens": v.pointer("/usage/prompt_tokens").and_then(|x| x.as_u64()).unwrap_or(0),
            "output_tokens": v.pointer("/usage/completion_tokens").and_then(|x| x.as_u64()).unwrap_or(0),
        }
    })
}

pub fn stop_reason(finish: &str) -> &'static str {
    match finish {
        "length" => "max_tokens",
        "tool_calls" | "function_call" => "tool_use",
        "stop" | "" => "end_turn",
        _ => "end_turn",
    }
}

/* ── 流式：OpenAI SSE → Anthropic SSE ──────────── */

/// 把 `/v1/chat/completions` 的 SSE 逐块改写成 Anthropic 事件流。
///
/// 有一个必须处理的细节：Anthropic 是**先声明块再增量**，OpenAI 只有 delta。
/// 所以这里要记住「当前打开的块」，遇到类型切换先 `content_block_stop`。
pub fn sse_transform(body: Body, model: String) -> Body {
    Body::from_stream(Xf::new(Box::pin(body.into_data_stream()), model))
}

struct Xf {
    inner: futures_util::stream::BoxStream<'static, Result<Bytes, axum::Error>>,
    buf: String,
    out: VecDeque<Bytes>,
    st: SseState,
    done: bool,
}

struct SseState {
    model: String,
    started: bool,
    next_index: usize,
    open: Option<Open>,
    tool_index: std::collections::HashMap<u64, usize>,
    input_tokens: u64,
    output_tokens: u64,
    finish: Option<String>,
    stopped: bool,
}

#[derive(PartialEq, Clone, Copy)]
enum Open {
    Text,
    Thinking,
    Tool(u64),
}

impl SseState {
    fn new(model: String) -> Self {
        Self {
            model,
            started: false,
            next_index: 0,
            open: None,
            tool_index: Default::default(),
            input_tokens: 0,
            output_tokens: 0,
            finish: None,
            stopped: false,
        }
    }

    fn frame(&mut self, event: &str, data: Value) -> Bytes {
        Bytes::from(format!("event: {event}\ndata: {data}\n\n"))
    }

    fn ensure_start(&mut self) -> Option<Bytes> {
        if self.started {
            return None;
        }
        self.started = true;
        Some(self.frame(
            "message_start",
            json!({
                "type": "message_start",
                "message": {
                    "id": format!("msg_{}", &uuid::Uuid::new_v4().simple().to_string()[..16]),
                    "type": "message",
                    "role": "assistant",
                    "model": self.model,
                    "content": [],
                    "stop_reason": null,
                    "stop_sequence": null,
                    "usage": { "input_tokens": 0, "output_tokens": 0 }
                }
            }),
        ))
    }

    /// 关掉当前块；类型相同则什么都不做。
    fn switch(&mut self, want: Open) -> Vec<Bytes> {
        let mut out = Vec::new();
        if self.open == Some(want) {
            return out;
        }
        if self.open.is_some() {
            out.push(self.frame(
                "content_block_stop",
                json!({ "type": "content_block_stop", "index": self.next_index - 1 }),
            ));
        }
        match want {
            Open::Text => {
                out.push(self.frame(
                    "content_block_start",
                    json!({
                        "type": "content_block_start",
                        "index": self.next_index,
                        "content_block": { "type": "text", "text": "" }
                    }),
                ));
                self.next_index += 1;
            }
            Open::Thinking => {
                out.push(self.frame(
                    "content_block_start",
                    json!({
                        "type": "content_block_start",
                        "index": self.next_index,
                        "content_block": { "type": "thinking", "thinking": "", "signature": "" }
                    }),
                ));
                self.next_index += 1;
            }
            Open::Tool(_) => {}
        }
        self.open = Some(want);
        out
    }
}

impl Xf {
    fn new(inner: futures_util::stream::BoxStream<'static, Result<Bytes, axum::Error>>, model: String) -> Self {
        Self {
            inner,
            buf: String::new(),
            out: VecDeque::new(),
            st: SseState::new(model),
            done: false,
        }
    }

    fn push(&mut self, b: Option<Bytes>) {
        if let Some(b) = b {
            self.out.push_back(b);
        }
    }

    fn emit(&mut self, event: &str, data: Value) {
        let b = self.st.frame(event, data);
        self.out.push_back(b);
    }

    /// 抽完 buffer 里所有完整的 SSE 帧。
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
                    self.finish_stream("stop");
                    continue;
                }
                if let Ok(v) = serde_json::from_str::<Value>(rest) {
                    self.on_chunk(&v);
                }
            }
        }
    }

    fn on_chunk(&mut self, v: &Value) {
        let start = self.st.ensure_start();
        self.push(start);

        if let Some(u) = v.get("usage") {
            if let Some(n) = u.get("prompt_tokens").and_then(|x| x.as_u64()) {
                self.st.input_tokens = n;
            }
            if let Some(n) = u.get("completion_tokens").and_then(|x| x.as_u64()) {
                self.st.output_tokens = n;
            }
        }

        let Some(choice) = v.pointer("/choices/0") else { return };
        let delta = choice.get("delta").cloned().unwrap_or_else(|| json!({}));

        // 思考内容：主流 OpenAI 兼容端点用 reasoning_content，少数用 reasoning
        let think = delta
            .get("reasoning_content")
            .or_else(|| delta.get("reasoning"))
            .and_then(|x| x.as_str())
            .unwrap_or("");
        if !think.is_empty() {
            for b in self.st.switch(Open::Thinking) {
                self.push(Some(b));
            }
            let idx = self.st.next_index - 1;
            let ev = self.st.frame(
                "content_block_delta",
                json!({
                    "type": "content_block_delta",
                    "index": idx,
                    "delta": { "type": "thinking_delta", "thinking": think }
                })
            );
            self.push(Some(ev));
        }

        if let Some(text) = delta.get("content").and_then(|x| x.as_str()) {
            if !text.is_empty() {
                for b in self.st.switch(Open::Text) {
                    self.push(Some(b));
                }
                let idx = self.st.next_index - 1;
                let ev = self.st.frame(
                    "content_block_delta",
                    json!({
                        "type": "content_block_delta",
                        "index": idx,
                        "delta": { "type": "text_delta", "text": text }
                    }),
                );
                self.push(Some(ev));
            }
        }

        if let Some(calls) = delta.get("tool_calls").and_then(|x| x.as_array()) {
            for c in calls {
                let oi = c.get("index").and_then(|x| x.as_u64()).unwrap_or(0);
                let known = self.st.tool_index.get(&oi).copied();
                let idx = match known {
                    Some(i) => i,
                    None => {
                        if self.st.open.is_some() {
                            let i = self.st.next_index - 1;
                            self.emit(
                                "content_block_stop",
                                json!({ "type": "content_block_stop", "index": i }),
                            );
                        }
                        let idx = self.st.next_index;
                        self.st.next_index += 1;
                        self.st.tool_index.insert(oi, idx);
                        self.st.open = Some(Open::Tool(oi));
                        let id = c
                            .get("id")
                            .and_then(|x| x.as_str())
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| format!("toolu_{idx}"));
                        let name = c
                            .pointer("/function/name")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string();
                        let mut block = json!({
                            "type": "tool_use",
                            "id": id,
                            "name": name,
                            "input": {}
                        });
                        // 流式下 thinking 块早于 tool_calls 关闭，签名只能挂在这里。
                        // Anthropic 没有这个字段，但客户端会原样回传，我们照收。
                        if let Some(x) = c.get("extra_content") {
                            if !x.is_null() {
                                block["extra_content"] = x.clone();
                            }
                        }
                        self.emit(
                            "content_block_start",
                            json!({
                                "type": "content_block_start",
                                "index": idx,
                                "content_block": block
                            }),
                        );
                        idx
                    }
                };
                if let Some(args) = c.pointer("/function/arguments").and_then(|x| x.as_str()) {
                    if !args.is_empty() {
                        let ev = self.st.frame(
                            "content_block_delta",
                            json!({
                                "type": "content_block_delta",
                                "index": idx,
                                "delta": { "type": "input_json_delta", "partial_json": args }
                            }),
                        );
                        self.push(Some(ev));
                    }
                }
            }
        }

        if let Some(f) = choice.get("finish_reason").and_then(|x| x.as_str()) {
            self.st.finish = Some(f.to_string());
        }
    }

    fn finish_stream(&mut self, fallback: &str) {
        if self.st.stopped {
            return;
        }
        let start = self.st.ensure_start();
        self.push(start);
        if self.st.open.is_some() {
            let i = self.st.next_index.saturating_sub(1);
            self.emit(
                "content_block_stop",
                json!({ "type": "content_block_stop", "index": i }),
            );
            self.st.open = None;
        }
        let reason = self
            .st
            .finish
            .clone()
            .map(|f| stop_reason(&f).to_string())
            .unwrap_or_else(|| fallback.to_string());
        let delta = self.st.frame(
            "message_delta",
            json!({
                "type": "message_delta",
                "delta": { "stop_reason": reason, "stop_sequence": null },
                "usage": {
                    "input_tokens": self.st.input_tokens,
                    "output_tokens": self.st.output_tokens
                }
            })
        );
        self.push(Some(delta));
        let stop = self.st.frame("message_stop", json!({ "type": "message_stop" }));
        self.push(Some(stop));
        self.st.stopped = true;
        self.done = true;
    }
}

impl Stream for Xf {
    type Item = Result<Bytes, axum::Error>;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        // 所有字段都是 Unpin，可以安全拿 &mut
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
                    // 没有完整帧就继续拉
                }
                std::task::Poll::Ready(Some(Err(e))) => {
                    let msg = e.to_string();
                    me.emit(
                        "error",
                        json!({ "type": "error", "error": { "type": "api_error", "message": msg } }),
                    );
                    me.done = true;
                }
                std::task::Poll::Ready(None) => {
                    me.finish_stream("end_turn");
                }
                std::task::Poll::Pending => return std::task::Poll::Pending,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_and_tools_map_to_openai() {
        let v = json!({
            "model": "auto",
            "max_tokens": 256,
            "system": [{ "type": "text", "text": "be brief" }],
            "messages": [{ "role": "user", "content": "hi" }],
            "tools": [{ "name": "read_file", "description": "read", "input_schema": { "type": "object" } }],
            "tool_choice": { "type": "any" }
        });
        let o = to_openai(&v);
        assert_eq!(o["messages"][0]["role"], "system");
        assert_eq!(o["tools"][0]["function"]["name"], "read_file");
        assert_eq!(o["tool_choice"], "required");
    }

    #[test]
    fn tool_result_becomes_tool_message_before_user_text() {
        let v = json!({
            "model": "auto",
            "messages": [{
                "role": "user",
                "content": [
                    { "type": "tool_result", "tool_use_id": "t1", "content": [{ "type": "text", "text": "42" }] },
                    { "type": "text", "text": "继续" }
                ]
            }]
        });
        let o = to_openai(&v);
        assert_eq!(o["messages"][0]["role"], "tool");
        assert_eq!(o["messages"][0]["content"], "42");
        assert_eq!(o["messages"][1]["role"], "user");
    }

    #[test]
    fn assistant_tool_use_becomes_tool_calls() {
        let v = json!({
            "model": "auto",
            "messages": [{
                "role": "assistant",
                "content": [{ "type": "tool_use", "id": "t1", "name": "ls", "input": { "path": "/" } }]
            }]
        });
        let o = to_openai(&v);
        assert_eq!(o["messages"][0]["tool_calls"][0]["id"], "t1");
        assert_eq!(
            o["messages"][0]["tool_calls"][0]["function"]["arguments"],
            json!({ "path": "/" }).to_string()
        );
    }

    #[test]
    fn openai_message_becomes_anthropic_content() {
        let v = json!({
            "id": "chatcmpl-1",
            "model": "qwen3",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "hello",
                    "tool_calls": [{ "id": "c1", "function": { "name": "ls", "arguments": "{\"path\":\"/\"}" } }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": { "prompt_tokens": 9, "completion_tokens": 4 }
        });
        let a = from_openai(&v, "groq");
        assert_eq!(a["content"][0]["text"], "hello");
        assert_eq!(a["content"][1]["type"], "tool_use");
        assert_eq!(a["content"][1]["input"]["path"], "/");
        assert_eq!(a["stop_reason"], "tool_use");
        assert_eq!(a["usage"]["input_tokens"], 9);
    }

    #[test]
    fn thinking_is_returned_with_a_sealed_signature() {
        let v = json!({
            "model": "gemini-3-pro",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "done",
                    "reasoning_content": "先看目录再决定",
                    "tool_calls": [{
                        "id": "c1",
                        "type": "function",
                        "function": { "name": "ls", "arguments": "{}" },
                        "extra_content": { "google": { "thought_signature": "SIG" } }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        });
        let a = from_openai(&v, "gemini");
        // thinking 必须在 text 之前，否则客户端渲染顺序会反
        assert_eq!(a["content"][0]["type"], "thinking");
        assert_eq!(a["content"][0]["thinking"], "先看目录再决定");

        let sig = a["content"][0]["signature"].as_str().expect("signature");
        let sealed = crate::sig::open(sig).expect("our own signature");
        assert_eq!(sealed.src, "gemini");
        assert_eq!(
            sealed.extras[0]["google"]["thought_signature"], "SIG",
            "上游要求回传的字节必须原样封进去"
        );
    }

    #[test]
    fn sealed_signature_is_replayed_onto_tool_calls() {
        // 客户端把上一轮的 thinking 块原样带回来
        let sig = crate::sig::seal(
            "gemini",
            "gemini-3-pro",
            &[json!({ "google": { "thought_signature": "SIG" } })],
        );
        let v = json!({
            "model": "auto",
            "messages": [{
                "role": "assistant",
                "content": [
                    { "type": "thinking", "thinking": "想一想", "signature": sig },
                    { "type": "tool_use", "id": "c1", "name": "ls", "input": {} }
                ]
            }]
        });
        let o = to_openai(&v);
        assert_eq!(
            o["messages"][0]["tool_calls"][0]["extra_content"]["google"]["thought_signature"],
            "SIG",
            "少了它，Gemini 3 的二次请求会 400"
        );
        // 思考文本仍然不回传，避免污染上游上下文
        assert!(o["messages"][0].get("content").is_none());
    }

    #[test]
    fn foreign_signature_is_ignored_not_fatal() {
        let v = json!({
            "model": "auto",
            "messages": [{
                "role": "assistant",
                "content": [
                    { "type": "thinking", "thinking": "x", "signature": "EqQBCkgI-real-anthropic" },
                    { "type": "tool_use", "id": "c1", "name": "ls", "input": {} }
                ]
            }]
        });
        let o = to_openai(&v);
        assert!(
            o["messages"][0]["tool_calls"][0].get("extra_content").is_none(),
            "别人家的签名不猜，也不该把请求弄坏"
        );
    }

    #[test]
    fn streaming_tool_use_carries_extra_content() {
        let sig_val = json!({ "google": { "thought_signature": "SIG" } });
        let chunk = json!({
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "c1",
                        "function": { "name": "ls", "arguments": "{}" },
                        "extra_content": sig_val
                    }]
                }
            }]
        });
        let empty: futures_util::stream::BoxStream<'static, Result<Bytes, axum::Error>> =
            Box::pin(futures_util::stream::empty());
        let mut xf = Xf::new(empty, "m".into());
        xf.on_chunk(&chunk);
        let out: Vec<String> = xf
            .out
            .iter()
            .map(|b| String::from_utf8_lossy(b).to_string())
            .collect();
        let joined = out.join("");
        assert!(joined.contains("thought_signature"), "流式只能把签名挂在 tool_use 上");
    }
}
