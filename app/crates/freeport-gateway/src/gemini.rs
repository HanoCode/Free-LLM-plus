//! Gemini 原生协议适配（`/v1beta`）。
//!
//! Gemini CLI 与一批 SDK 只说 `generateContent`，不会说 OpenAI 协议。
//! 这里做双向翻译：请求侧把 `contents` 摊平成 messages，响应侧再包回
//! `candidates`。和 Anthropic 适配层一样，中间仍然只走一条
//! `/v1/chat/completions`，路由、账本、Hold 全部复用。
//!
//! 三个必须处理的差异：
//! 1. 角色叫 `model` 不叫 `assistant`，且同一角色**不能连续出现**，要合并；
//! 2. 工具调用没有 id，只能按函数名回填，否则 `functionResponse` 对不上；
//! 3. 流式块没有事件名，只有 `data:`，结束也没有 `[DONE]`。

use std::collections::VecDeque;

use axum::body::Body;
use bytes::Bytes;
use futures_util::stream::Stream;
use serde_json::{json, Map, Value};

/// `generateContent` 请求 → OpenAI chat 请求。
pub fn to_openai(v: &Value, model: &str) -> Value {
    let mut out = Map::new();
    out.insert("model".into(), json!(model));
    out.insert("stream".into(), json!(false));

    let mut messages: Vec<Value> = Vec::new();

    // systemInstruction 等价于 system 消息；Gemini 允许多段，拼成一条
    if let Some(sys) = v.get("systemInstruction").or_else(|| v.get("system_instruction")) {
        let text = parts_text(sys.get("parts"));
        if !text.is_empty() {
            messages.push(json!({ "role": "system", "content": text }));
        }
    }

    if let Some(contents) = v.get("contents").and_then(|x| x.as_array()) {
        for c in contents {
            let role = c.get("role").and_then(|x| x.as_str()).unwrap_or("user");
            let parts = c.get("parts").and_then(|x| x.as_array());

            // 工具回执：Gemini 把它塞在 user 轮里，OpenAI 要独立的 tool 消息
            if let Some(ps) = parts {
                let mut texts: Vec<String> = Vec::new();
                let mut images: Vec<Value> = Vec::new();
                for p in ps {
                    if let Some(fr) = p.get("functionResponse") {
                        let name = fr.get("name").and_then(|x| x.as_str()).unwrap_or("");
                        let body = fr
                            .get("response")
                            .map(|r| r.get("result").cloned().unwrap_or_else(|| r.clone()))
                            .unwrap_or(Value::Null);
                        let content = match body {
                            Value::String(s) => s,
                            Value::Null => String::new(),
                            other => other.to_string(),
                        };
                        messages.push(json!({
                            "role": "tool",
                            "tool_call_id": tool_id(name),
                            "content": content
                        }));
                    } else if let Some(fc) = p.get("functionCall") {
                        let name = fc.get("name").and_then(|x| x.as_str()).unwrap_or("");
                        let args = fc.get("args").cloned().unwrap_or_else(|| json!({}));
                        // 客户端带回来的 thoughtSignature 必须原样还回上游，
                        // 少一个 Gemini 3 就 400，而且这 400 照样烧额度
                        push_assistant_call(&mut messages, name, &args, p.get("thoughtSignature"));
                    } else if let Some(t) = p.get("text").and_then(|x| x.as_str()) {
                        if !t.is_empty() {
                            texts.push(t.to_string());
                        }
                    } else if let Some(img) = gemini_image(p) {
                        // 以前这里直接把 inlineData 丢掉：图片静默消失，上游只收到
                        // 一句「这张图里是什么」。能力门禁也看不到图，会把它当文本请求。
                        images.push(img);
                    }
                }
                if !texts.is_empty() || !images.is_empty() {
                    let role = if role == "model" { "assistant" } else { "user" };
                    let content = if images.is_empty() {
                        // 纯文本保持字符串形态：多数上游对字符串的处理最稳
                        json!(texts.join(""))
                    } else {
                        // 有图就得走分块。文字在前、图片在后，和 Anthropic 侧一致。
                        let mut parts: Vec<Value> = texts
                            .iter()
                            .map(|t| json!({ "type": "text", "text": t }))
                            .collect();
                        parts.extend(images);
                        json!(parts)
                    };
                    messages.push(json!({ "role": role, "content": content }));
                }
            }
        }
    }

    // Gemini 要求 user/model 交替；合并相邻同角色，避免上游 400
    out.insert("messages".into(), Value::Array(coalesce(messages)));

    if let Some(tools) = v.get("tools").and_then(|x| x.as_array()) {
        let mut decls: Vec<Value> = Vec::new();
        for t in tools {
            if let Some(fs) = t.get("functionDeclarations").and_then(|x| x.as_array()) {
                for f in fs {
                    let mut params = f.get("parameters").cloned().unwrap_or_else(|| json!({}));
                    // Gemini 不接受 JSON Schema 的这两个键
                    if let Value::Object(m) = &mut params {
                        m.remove("$schema");
                        m.remove("additionalProperties");
                    }
                    decls.push(json!({
                        "type": "function",
                        "function": {
                            "name": f.get("name").cloned().unwrap_or(json!("")),
                            "description": f.get("description").cloned().unwrap_or(json!("")),
                            "parameters": params
                        }
                    }));
                }
            }
        }
        if !decls.is_empty() {
            out.insert("tools".into(), Value::Array(decls));
        }
    }

    if let Some(g) = v.get("generationConfig").or_else(|| v.get("generation_config")) {
        for (from, to) in [
            ("temperature", "temperature"),
            ("topP", "top_p"),
            ("topK", "top_k"),
        ] {
            if let Some(x) = g.get(from) {
                out.insert(to.into(), x.clone());
            }
        }
        if let Some(n) = g.get("maxOutputTokens") {
            out.insert("max_tokens".into(), n.clone());
        }
        if let Some(stop) = g.get("stopSequences") {
            out.insert("stop".into(), stop.clone());
        }
        if let Some(mime) = g.get("responseMimeType").and_then(|x| x.as_str()) {
            if mime.contains("json") {
                out.insert("response_format".into(), json!({ "type": "json_object" }));
            }
        }
    }

    // 客户端要求 JSON 输出时，Gemini 会附带 schema，OpenAI 侧只认 json_object
    Value::Object(out)
}

fn push_assistant_call(messages: &mut Vec<Value>, name: &str, args: &Value, sig: Option<&Value>) {
    // Gemini 的 thoughtSignature 挂在 part 上；OpenAI 侧只能寄居在 extra_content
    let extra = sig.map(|s| json!({ "google": { "thought_signature": s } }));
    // 同一轮里连续多个 functionCall 要并进同一条 assistant 消息
    if let Some(last) = messages.last_mut() {
        if last.get("role").and_then(|x| x.as_str()) == Some("assistant") {
            if last.get("tool_calls").is_none() {
                last["tool_calls"] = json!([]);
            }
            if let Some(arr) = last.get_mut("tool_calls").and_then(|x| x.as_array_mut()) {
                let mut call = json!({
                    "id": tool_id(name),
                    "type": "function",
                    "function": { "name": name, "arguments": args.to_string() }
                });
                if let Some(e) = &extra {
                    call["extra_content"] = e.clone();
                }
                arr.push(call);
            }
            if last.get("content").map(|c| c.is_null()).unwrap_or(true) {
                last["content"] = json!("");
            }
            return;
        }
    }
    let mut call = json!({
        "id": tool_id(name),
        "type": "function",
        "function": { "name": name, "arguments": args.to_string() }
    });
    if let Some(e) = &extra {
        call["extra_content"] = e.clone();
    }
    messages.push(json!({
        "role": "assistant",
        "content": "",
        "tool_calls": [call]
    }));
}

/// Gemini 没有 tool_call id，用函数名派生一个稳定值。
/// 客户端回传 `functionResponse.name` 时能对上，这是唯一可用的关联键。
fn tool_id(name: &str) -> String {
    format!("call_{name}")
}

/// 合并相邻同角色消息。`tool` 角色必须单独存在，不参与合并。
fn coalesce(messages: Vec<Value>) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for m in messages {
        let role = m.get("role").and_then(|x| x.as_str()).unwrap_or("");
        let mergeable = role == "user" || role == "assistant";
        if mergeable {
            if let Some(last) = out.last_mut() {
                if last.get("role").and_then(|x| x.as_str()) == Some(role)
                    && last.get("tool_calls").is_none()
                    && m.get("tool_calls").is_none()
                {
                    let a = last.get("content").and_then(|x| x.as_str()).unwrap_or("");
                    let b = m.get("content").and_then(|x| x.as_str()).unwrap_or("");
                    let joined = if a.is_empty() {
                        b.to_string()
                    } else if b.is_empty() {
                        a.to_string()
                    } else {
                        format!("{a}\n\n{b}")
                    };
                    last["content"] = json!(joined);
                    continue;
                }
            }
        }
        out.push(m);
    }
    out
}

/// Gemini 的一个 part → OpenAI 的 `image_url` 分块。
///
/// `inlineData` 是 base64 内联（Gemini CLI 贴图走这条），`fileData` 是
/// Files API 的 URI。非图片的 `fileData`（PDF/音视频）不往里塞：
/// 当成 `image_url` 发给上游只会换来一个 400。
fn gemini_image(p: &Value) -> Option<Value> {
    if let Some(inline) = p.get("inlineData").or_else(|| p.get("inline_data")) {
        let mime = inline
            .get("mimeType")
            .or_else(|| inline.get("mime_type"))
            .and_then(|x| x.as_str())
            .unwrap_or("image/png");
        if !mime.starts_with("image/") {
            return None;
        }
        let data = inline.get("data").and_then(|x| x.as_str()).unwrap_or("");
        if data.is_empty() {
            return None;
        }
        return Some(json!({
            "type": "image_url",
            "image_url": { "url": format!("data:{mime};base64,{data}") }
        }));
    }

    let fd = p.get("fileData").or_else(|| p.get("file_data"))?;
    let mime = fd
        .get("mimeType")
        .or_else(|| fd.get("mime_type"))
        .and_then(|x| x.as_str())
        .unwrap_or("");
    if !mime.is_empty() && !mime.starts_with("image/") {
        return None;
    }
    let uri = fd
        .get("fileUri")
        .or_else(|| fd.get("file_uri"))
        .and_then(|x| x.as_str())?;
    if uri.is_empty() {
        return None;
    }
    Some(json!({ "type": "image_url", "image_url": { "url": uri } }))
}

fn parts_text(parts: Option<&Value>) -> String {
    parts
        .and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|p| p.get("text").and_then(|x| x.as_str()))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

/// OpenAI chat 响应 → `generateContent` 响应。
pub fn from_openai(v: &Value, model: &str) -> Value {
    let msg = v.pointer("/choices/0/message").cloned().unwrap_or_else(|| json!({}));

    let mut parts: Vec<Value> = Vec::new();
    let text = msg.get("content").and_then(|x| x.as_str()).unwrap_or("");
    if !text.is_empty() {
        parts.push(json!({ "text": text }));
    }
    let mut has_call = false;
    if let Some(calls) = msg.get("tool_calls").and_then(|x| x.as_array()) {
        for c in calls {
            let name = c
                .pointer("/function/name")
                .and_then(|x| x.as_str())
                .unwrap_or("");
            let args = c
                .pointer("/function/arguments")
                .and_then(|x| x.as_str())
                .and_then(|s| serde_json::from_str::<Value>(s).ok())
                .unwrap_or_else(|| json!({}));
            let mut part = json!({ "functionCall": { "name": name, "args": args } });
            // Gemini 的签名在 part 级，不在 functionCall 里面
            if let Some(sig) = c
                .pointer("/extra_content/google/thought_signature")
                .and_then(|x| x.as_str())
            {
                part["thoughtSignature"] = json!(sig);
            }
            parts.push(part);
            has_call = true;
        }
    }
    if parts.is_empty() {
        parts.push(json!({ "text": "" }));
    }

    let finish = v
        .pointer("/choices/0/finish_reason")
        .and_then(|x| x.as_str())
        .unwrap_or("stop");

    let mut out = json!({
        "candidates": [{
            "content": { "parts": parts, "role": "model" },
            "finishReason": if has_call { "STOP" } else { map_finish(finish) },
            "index": 0
        }],
        "modelVersion": model
    });

    if let Some(u) = v.get("usage") {
        out["usageMetadata"] = usage_of(u);
    }
    out
}

fn usage_of(u: &Value) -> Value {
    let prompt = u.get("prompt_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
    let completion = u.get("completion_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
    json!({
        "promptTokenCount": prompt,
        "candidatesTokenCount": completion,
        "totalTokenCount": u
            .get("total_tokens")
            .and_then(|x| x.as_u64())
            .unwrap_or(prompt + completion)
    })
}

pub fn map_finish(finish: &str) -> &'static str {
    match finish {
        "length" => "MAX_TOKENS",
        "content_filter" => "SAFETY",
        "tool_calls" => "STOP",
        _ => "STOP",
    }
}

/// Gemini 的错误体形状，客户端只认 `error.code/status/message`。
pub fn error_body(code: u16, message: &str) -> Value {
    json!({
        "error": {
            "code": code,
            "message": message,
            "status": status_of(code)
        }
    })
}

fn status_of(code: u16) -> &'static str {
    match code {
        400 => "INVALID_ARGUMENT",
        401 => "UNAUTHENTICATED",
        403 => "PERMISSION_DENIED",
        404 => "NOT_FOUND",
        429 => "RESOURCE_EXHAUSTED",
        500..=599 => "INTERNAL",
        _ => "UNKNOWN",
    }
}

/// `/v1/chat/completions` 的 SSE → Gemini `alt=sse` 流。
pub fn sse_transform(body: Body, model: String) -> Body {
    Body::from_stream(Xf::new(Box::pin(body.into_data_stream()), model))
}

struct Xf {
    inner: futures_util::stream::BoxStream<'static, Result<Bytes, axum::Error>>,
    buf: String,
    out: VecDeque<Bytes>,
    model: String,
    tools: Vec<(String, String, String)>, // (name, 已累积的 arguments, thoughtSignature)
    finish: Option<String>,
    usage: Option<Value>,
    closed: bool,
}

impl Xf {
    fn new(inner: futures_util::stream::BoxStream<'static, Result<Bytes, axum::Error>>, model: String) -> Self {
        Self {
            inner,
            buf: String::new(),
            out: VecDeque::new(),
            model,
            tools: Vec::new(),
            finish: None,
            usage: None,
            closed: false,
        }
    }

    fn data(&mut self, v: Value) {
        self.out.push_back(Bytes::from(format!("data: {v}\n\n")));
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
                    self.close();
                    continue;
                }
                if let Ok(v) = serde_json::from_str::<Value>(rest) {
                    self.on_chunk(&v);
                }
            }
        }
    }

    fn on_chunk(&mut self, v: &Value) {
        if let Some(u) = v.get("usage") {
            self.usage = Some(usage_of(u));
        }

        let Some(choice) = v.pointer("/choices/0") else { return };
        let delta = choice.get("delta").cloned().unwrap_or_else(|| json!({}));

        if let Some(t) = delta.get("content").and_then(|x| x.as_str()) {
            if !t.is_empty() {
                self.data(json!({
                    "candidates": [{
                        "content": { "parts": [{ "text": t }], "role": "model" },
                        "index": 0
                    }],
                    "modelVersion": self.model
                }));
            }
        }

        if let Some(calls) = delta.get("tool_calls").and_then(|x| x.as_array()) {
            for c in calls {
                let idx = c.get("index").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
                while self.tools.len() <= idx {
                    self.tools.push((String::new(), String::new(), String::new()));
                }
                if let Some(name) = c.pointer("/function/name").and_then(|x| x.as_str()) {
                    if !name.is_empty() {
                        self.tools[idx].0 = name.to_string();
                    }
                }
                if let Some(args) = c.pointer("/function/arguments").and_then(|x| x.as_str()) {
                    self.tools[idx].1.push_str(args);
                }
                // 签名不参与分片累积：上游只在首个 chunk 给一次
                if let Some(sig) = c
                    .pointer("/extra_content/google/thought_signature")
                    .and_then(|x| x.as_str())
                {
                    self.tools[idx].2 = sig.to_string();
                }
            }
        }

        if let Some(f) = choice.get("finish_reason").and_then(|x| x.as_str()) {
            self.finish = Some(f.to_string());
        }
    }

    /// 收尾只发一次：工具调用攒完整再一次性给出，Gemini 不接受分片参数。
    fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;

        let mut parts: Vec<Value> = Vec::new();
        for (name, args, sig) in &self.tools {
            if name.is_empty() {
                continue;
            }
            let parsed = serde_json::from_str::<Value>(args).unwrap_or_else(|_| json!({}));
            let mut part = json!({ "functionCall": { "name": name, "args": parsed } });
            if !sig.is_empty() {
                part["thoughtSignature"] = json!(sig);
            }
            parts.push(part);
        }
        if parts.is_empty() {
            parts.push(json!({ "text": "" }));
        }

        let finish = if self.tools.is_empty() {
            map_finish(self.finish.as_deref().unwrap_or("stop"))
        } else {
            "STOP"
        };

        let mut chunk = json!({
            "candidates": [{
                "content": { "parts": parts, "role": "model" },
                "finishReason": finish,
                "index": 0
            }],
            "modelVersion": self.model
        });
        if let Some(u) = self.usage.take() {
            chunk["usageMetadata"] = u;
        }
        self.data(chunk);
    }
}

impl Stream for Xf {
    type Item = Result<Bytes, axum::Error>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        use std::task::Poll;
        let this = &mut *self;

        loop {
            if let Some(b) = this.out.pop_front() {
                return Poll::Ready(Some(Ok(b)));
            }
            if this.closed {
                return Poll::Ready(None);
            }

            match std::pin::Pin::new(&mut this.inner).poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => {
                    this.close();
                    continue;
                }
                Poll::Ready(Some(Err(e))) => return Poll::Ready(Some(Err(e))),
                Poll::Ready(Some(Ok(chunk))) => {
                    this.buf.push_str(&String::from_utf8_lossy(&chunk));
                    this.drain_frames();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_request_with_system_and_tools() {
        let v = json!({
            "systemInstruction": { "parts": [{ "text": "be brief" }] },
            "contents": [
                { "role": "user", "parts": [{ "text": "hi" }] },
                { "role": "model", "parts": [{ "functionCall": { "name": "ls", "args": { "p": "/" } } }] },
                { "role": "user", "parts": [{ "functionResponse": { "name": "ls", "response": { "result": "a\nb" } } }] }
            ],
            "tools": [{ "functionDeclarations": [{
                "name": "ls",
                "description": "list",
                "parameters": { "$schema": "x", "type": "object", "properties": {} }
            }]}],
            "generationConfig": { "maxOutputTokens": 64, "temperature": 0.2, "stopSequences": ["END"] }
        });
        let oa = to_openai(&v, "gemini-2.0-flash");
        assert_eq!(oa["messages"][0]["role"], "system");
        assert_eq!(oa["messages"][1]["content"], "hi");
        assert_eq!(oa["messages"][2]["tool_calls"][0]["function"]["name"], "ls");
        assert_eq!(oa["messages"][3]["role"], "tool");
        assert_eq!(oa["messages"][3]["content"], "a\nb");
        assert_eq!(oa["max_tokens"], 64);
        assert_eq!(oa["stop"][0], "END");
        assert!(oa["tools"][0]["function"]["parameters"].get("$schema").is_none());
    }

    #[test]
    fn thought_signature_survives_both_directions() {
        // 入站：客户端把上一轮拿到的签名带回来，必须原样还给上游
        let req = json!({
            "contents": [
                { "role": "user", "parts": [{ "text": "hi" }] },
                { "role": "model", "parts": [
                    { "functionCall": { "name": "ls", "args": { "p": "/" } }, "thoughtSignature": "SIG" }
                ]}
            ]
        });
        let oa = to_openai(&req, "x");
        assert_eq!(
            oa["messages"][1]["tool_calls"][0]["extra_content"]["google"]["thought_signature"],
            "SIG",
            "少了它，Gemini 3 的二次请求会 400"
        );

        // 出站：上游的 extra_content 要变回 part 级的 thoughtSignature
        let res = json!({
            "choices": [{
                "message": {
                    "tool_calls": [{
                        "id": "c1",
                        "type": "function",
                        "function": { "name": "ls", "arguments": "{\"p\":\"/\"}" },
                        "extra_content": { "google": { "thought_signature": "SIG2" } }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        });
        let g = from_openai(&res, "gemini-3-pro");
        let part = &g["candidates"][0]["content"]["parts"][0];
        assert_eq!(part["thoughtSignature"], "SIG2");
        assert_eq!(part["functionCall"]["name"], "ls");
    }

    #[test]
    fn coalesces_consecutive_user_turns() {
        let v = json!({
            "contents": [
                { "role": "user", "parts": [{ "text": "a" }] },
                { "role": "user", "parts": [{ "text": "b" }] }
            ]
        });
        let oa = to_openai(&v, "x");
        assert_eq!(oa["messages"].as_array().unwrap().len(), 1);
        assert_eq!(oa["messages"][0]["content"], "a\n\nb");
    }

    #[test]
    fn inline_image_survives_the_conversion() {
        let v = json!({
            "contents": [{ "role": "user", "parts": [
                { "text": "这是什么" },
                { "inlineData": { "mimeType": "image/png", "data": "AAAA" } }
            ]}]
        });
        let oa = to_openai(&v, "m");
        let content = &oa["messages"][0]["content"];
        assert!(content.is_array(), "有图就必须走分块，否则图会被上游忽略");
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "image_url");
        assert_eq!(content[1]["image_url"]["url"], "data:image/png;base64,AAAA");
        // 能力门禁读的就是这一份：图没转出来，带图请求会被当成纯文本发出去
        assert!(freeport_core::route::Needs::of_chat(&oa).vision);
    }

    #[test]
    fn non_image_file_data_is_dropped() {
        let v = json!({
            "contents": [{ "role": "user", "parts": [
                { "text": "总结这个 PDF" },
                { "fileData": { "mimeType": "application/pdf", "fileUri": "https://x/a.pdf" } }
            ]}]
        });
        let oa = to_openai(&v, "m");
        assert_eq!(oa["messages"][0]["content"], "总结这个 PDF", "别把 PDF 当图片发");
        assert!(!freeport_core::route::Needs::of_chat(&oa).vision);
    }

    #[test]
    fn maps_response_with_tool_call() {
        let v = json!({
            "choices": [{
                "message": {
                    "content": "let me look",
                    "tool_calls": [{ "id": "c1", "type": "function", "function": { "name": "ls", "arguments": "{\"p\":\"/\"}" } }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": { "prompt_tokens": 7, "completion_tokens": 3, "total_tokens": 10 }
        });
        let g = from_openai(&v, "gemini-2.0-flash");
        assert_eq!(g["candidates"][0]["content"]["parts"][0]["text"], "let me look");
        assert_eq!(g["candidates"][0]["content"]["parts"][1]["functionCall"]["name"], "ls");
        assert_eq!(g["candidates"][0]["finishReason"], "STOP");
        assert_eq!(g["usageMetadata"]["totalTokenCount"], 10);
    }

    #[test]
    fn maps_length_to_max_tokens() {
        assert_eq!(map_finish("length"), "MAX_TOKENS");
        assert_eq!(map_finish("content_filter"), "SAFETY");
    }
}
