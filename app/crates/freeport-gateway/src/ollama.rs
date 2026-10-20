//! Ollama 兼容入口（`/api/tags`、`/api/chat`、`/api/generate`）。
//!
//! 目的只有一个：让只会说 Ollama 的客户端（Zed、部分 JetBrains 插件）
//! 也能用上免费额度。它们不认 OpenAI 协议，也不发鉴权头。
//!
//! 和另外两个适配层的差别在于**流式不是 SSE 而是 NDJSON**：一行一个
//! 完整 JSON 对象，没有 `data:` 前缀，结束靠 `done: true` 而不是 `[DONE]`。

use std::collections::VecDeque;

use axum::body::Body;
use bytes::Bytes;
use futures_util::stream::Stream;
use serde_json::{json, Map, Value};

/// `/api/chat` 请求 → OpenAI chat 请求。
pub fn to_openai_chat(v: &Value, model: &str) -> Value {
    let mut out = Map::new();
    out.insert("model".into(), json!(model));
    out.insert("stream".into(), json!(false));

    if let Some(msgs) = v.get("messages").and_then(|x| x.as_array()) {
        out.insert("messages".into(), Value::Array(msgs.iter().map(convert_message).collect()));
    }
    if let Some(tools) = v.get("tools") {
        if tools.as_array().map(|a| !a.is_empty()).unwrap_or(false) {
            out.insert("tools".into(), tools.clone());
        }
    }

    // options 是 Ollama 的参数口袋，只挑 OpenAI 认的那几个
    if let Some(o) = v.get("options") {
        if let Some(t) = o.get("temperature") {
            out.insert("temperature".into(), t.clone());
        }
        if let Some(p) = o.get("num_predict") {
            out.insert("max_tokens".into(), p.clone());
        }
        if let Some(p) = o.get("top_p") {
            out.insert("top_p".into(), p.clone());
        }
        if let Some(s) = o.get("stop") {
            out.insert("stop".into(), s.clone());
        }
    }

    if let Some(f) = v.get("format") {
        // format: "json" 等价于 JSON 模式；给了 schema 也只能退化成 json_object
        if f.as_str() == Some("json") || f.is_object() {
            out.insert("response_format".into(), json!({ "type": "json_object" }));
        }
    }

    Value::Object(out)
}

/// `/api/generate` 请求 → OpenAI chat 请求（prompt 包成单轮对话）。
pub fn to_openai_generate(v: &Value, model: &str) -> Value {
    let mut messages: Vec<Value> = Vec::new();
    if let Some(sys) = v.get("system").and_then(|x| x.as_str()) {
        if !sys.is_empty() {
            messages.push(json!({ "role": "system", "content": sys }));
        }
    }
    let prompt = v.get("prompt").and_then(|x| x.as_str()).unwrap_or("");
    messages.push(json!({ "role": "user", "content": prompt }));

    let mut out = Map::new();
    out.insert("model".into(), json!(model));
    out.insert("stream".into(), json!(false));
    out.insert("messages".into(), Value::Array(messages));
    if let Some(o) = v.get("options") {
        if let Some(t) = o.get("temperature") {
            out.insert("temperature".into(), t.clone());
        }
        if let Some(p) = o.get("num_predict") {
            out.insert("max_tokens".into(), p.clone());
        }
    }
    Value::Object(out)
}

fn convert_message(m: &Value) -> Value {
    // Ollama 的图片是消息上的 `images: [base64]`，OpenAI 要 content 数组
    let images = m.get("images").and_then(|x| x.as_array());
    let content = m.get("content").cloned().unwrap_or_else(|| json!(""));
    let Some(imgs) = images.filter(|a| !a.is_empty()) else {
        return json!({
            "role": m.get("role").cloned().unwrap_or(json!("user")),
            "content": content
        });
    };

    let mut parts: Vec<Value> = Vec::new();
    if let Some(t) = content.as_str() {
        if !t.is_empty() {
            parts.push(json!({ "type": "text", "text": t }));
        }
    }
    for img in imgs {
        if let Some(b64) = img.as_str() {
            parts.push(json!({
                "type": "image_url",
                "image_url": { "url": format!("data:image/png;base64,{b64}") }
            }));
        }
    }
    json!({
        "role": m.get("role").cloned().unwrap_or(json!("user")),
        "content": parts
    })
}

/// OpenAI chat 响应 → `/api/chat` 响应。
pub fn chat_response(v: &Value, model: &str) -> Value {
    let msg = v.pointer("/choices/0/message").cloned().unwrap_or_else(|| json!({}));
    let text = msg.get("content").and_then(|x| x.as_str()).unwrap_or("");

    let mut message = json!({ "role": "assistant", "content": text });
    if let Some(calls) = msg.get("tool_calls").and_then(|x| x.as_array()) {
        let mapped: Vec<Value> = calls
            .iter()
            .map(|c| {
                let name = c
                    .pointer("/function/name")
                    .and_then(|x| x.as_str())
                    .unwrap_or("");
                let args = c
                    .pointer("/function/arguments")
                    .and_then(|x| x.as_str())
                    .and_then(|s| serde_json::from_str::<Value>(s).ok())
                    .unwrap_or_else(|| json!({}));
                json!({ "function": { "name": name, "arguments": args } })
            })
            .collect();
        if !mapped.is_empty() {
            message["tool_calls"] = Value::Array(mapped);
        }
    }

    let finish = v
        .pointer("/choices/0/finish_reason")
        .and_then(|x| x.as_str())
        .unwrap_or("stop");
    let (prompt, eval) = usage(v);

    json!({
        "model": model,
        "created_at": chrono::Utc::now().to_rfc3339(),
        "message": message,
        "done": true,
        "done_reason": done_reason(finish),
        "prompt_eval_count": prompt,
        "eval_count": eval
    })
}

/// OpenAI chat 响应 → `/api/generate` 响应。
pub fn generate_response(v: &Value, model: &str) -> Value {
    let text = v
        .pointer("/choices/0/message/content")
        .and_then(|x| x.as_str())
        .unwrap_or("");
    let finish = v
        .pointer("/choices/0/finish_reason")
        .and_then(|x| x.as_str())
        .unwrap_or("stop");
    let (prompt, eval) = usage(v);
    json!({
        "model": model,
        "created_at": chrono::Utc::now().to_rfc3339(),
        "response": text,
        "done": true,
        "done_reason": done_reason(finish),
        "prompt_eval_count": prompt,
        "eval_count": eval
    })
}

fn usage(v: &Value) -> (u64, u64) {
    let u = v.get("usage");
    (
        u.and_then(|x| x.get("prompt_tokens")).and_then(|x| x.as_u64()).unwrap_or(0),
        u.and_then(|x| x.get("completion_tokens")).and_then(|x| x.as_u64()).unwrap_or(0),
    )
}

fn done_reason(finish: &str) -> &'static str {
    match finish {
        "length" => "length",
        _ => "stop",
    }
}

/// 错误体：Ollama 用 `{"error": "..."}`，不是嵌套对象。
pub fn error_body(message: &str) -> Value {
    json!({ "error": message })
}

/// `/api/tags` 的模型条目。
pub fn tag(id: &str, owned_by: &str, context: Option<u64>) -> Value {
    json!({
        "name": id,
        "model": id,
        "modified_at": chrono::Utc::now().to_rfc3339(),
        "size": 0u64,
        "digest": "",
        "details": {
            "parent_model": "",
            "format": "freeport",
            "family": owned_by,
            "families": [owned_by],
            "parameter_size": "",
            "quantization_level": "",
            "context_length": context.unwrap_or(0)
        }
    })
}

/// OpenAI SSE → Ollama NDJSON。
pub fn ndjson(body: Body, model: String, generate: bool) -> Body {
    Body::from_stream(Xf::new(Box::pin(body.into_data_stream()), model, generate))
}

struct Xf {
    inner: futures_util::stream::BoxStream<'static, Result<Bytes, axum::Error>>,
    buf: String,
    out: VecDeque<Bytes>,
    model: String,
    generate: bool,
    tools: Vec<(String, String)>,
    finish: Option<String>,
    prompt_tokens: u64,
    eval_tokens: u64,
    closed: bool,
}

impl Xf {
    fn new(
        inner: futures_util::stream::BoxStream<'static, Result<Bytes, axum::Error>>,
        model: String,
        generate: bool,
    ) -> Self {
        Self {
            inner,
            buf: String::new(),
            out: VecDeque::new(),
            model,
            generate,
            tools: Vec::new(),
            finish: None,
            prompt_tokens: 0,
            eval_tokens: 0,
            closed: false,
        }
    }

    fn line(&mut self, v: Value) {
        self.out.push_back(Bytes::from(format!("{v}\n")));
    }

    fn text_line(&mut self, text: &str) {
        if self.generate {
            self.line(json!({
                "model": self.model,
                "created_at": chrono::Utc::now().to_rfc3339(),
                "response": text,
                "done": false
            }));
        } else {
            self.line(json!({
                "model": self.model,
                "created_at": chrono::Utc::now().to_rfc3339(),
                "message": { "role": "assistant", "content": text },
                "done": false
            }));
        }
    }

    fn drain(&mut self) {
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
            if let Some(n) = u.get("prompt_tokens").and_then(|x| x.as_u64()) {
                self.prompt_tokens = n;
            }
            if let Some(n) = u.get("completion_tokens").and_then(|x| x.as_u64()) {
                self.eval_tokens = n;
            }
        }

        let Some(choice) = v.pointer("/choices/0") else { return };
        let delta = choice.get("delta").cloned().unwrap_or_else(|| json!({}));

        // 思考内容没有对应字段，并进正文，至少不丢信息
        if let Some(t) = delta.get("content").and_then(|x| x.as_str()) {
            if !t.is_empty() {
                self.text_line(t);
            }
        }

        if let Some(calls) = delta.get("tool_calls").and_then(|x| x.as_array()) {
            for c in calls {
                let idx = c.get("index").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
                while self.tools.len() <= idx {
                    self.tools.push((String::new(), String::new()));
                }
                if let Some(n) = c.pointer("/function/name").and_then(|x| x.as_str()) {
                    if !n.is_empty() {
                        self.tools[idx].0 = n.to_string();
                    }
                }
                if let Some(a) = c.pointer("/function/arguments").and_then(|x| x.as_str()) {
                    self.tools[idx].1.push_str(a);
                }
            }
        }

        if let Some(f) = choice.get("finish_reason").and_then(|x| x.as_str()) {
            self.finish = Some(f.to_string());
        }
    }

    fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;

        let reason = done_reason(self.finish.as_deref().unwrap_or("stop"));
        if self.generate {
            self.line(json!({
                "model": self.model,
                "created_at": chrono::Utc::now().to_rfc3339(),
                "response": "",
                "done": true,
                "done_reason": reason,
                "prompt_eval_count": self.prompt_tokens,
                "eval_count": self.eval_tokens
            }));
            return;
        }

        let mut message = json!({ "role": "assistant", "content": "" });
        if !self.tools.is_empty() {
            let calls: Vec<Value> = self
                .tools
                .iter()
                .filter(|(n, _)| !n.is_empty())
                .map(|(n, a)| {
                    let args = serde_json::from_str::<Value>(a).unwrap_or_else(|_| json!({}));
                    json!({ "function": { "name": n, "arguments": args } })
                })
                .collect();
            if !calls.is_empty() {
                message["tool_calls"] = Value::Array(calls);
            }
        }

        self.line(json!({
            "model": self.model,
            "created_at": chrono::Utc::now().to_rfc3339(),
            "message": message,
            "done": true,
            "done_reason": reason,
            "prompt_eval_count": self.prompt_tokens,
            "eval_count": self.eval_tokens
        }));
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
                    this.drain();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_request_maps_options_and_images() {
        let v = json!({
            "model": "auto",
            "messages": [{
                "role": "user",
                "content": "what is this",
                "images": ["AAA"]
            }],
            "options": { "temperature": 0.1, "num_predict": 32, "stop": ["END"] },
            "format": "json",
            "tools": [{ "type": "function", "function": { "name": "f" } }]
        });
        let oa = to_openai_chat(&v, "m");
        assert_eq!(oa["max_tokens"], 32);
        assert_eq!(oa["temperature"], 0.1);
        assert_eq!(oa["stop"][0], "END");
        assert_eq!(oa["response_format"]["type"], "json_object");
        assert_eq!(oa["messages"][0]["content"][1]["type"], "image_url");
        assert_eq!(oa["tools"][0]["function"]["name"], "f");
    }

    #[test]
    fn generate_request_wraps_prompt_and_system() {
        let v = json!({ "prompt": "hi", "system": "be brief", "options": { "num_predict": 8 } });
        let oa = to_openai_generate(&v, "m");
        assert_eq!(oa["messages"][0]["role"], "system");
        assert_eq!(oa["messages"][1]["content"], "hi");
        assert_eq!(oa["max_tokens"], 8);
    }

    #[test]
    fn chat_response_maps_tool_calls() {
        let v = json!({
            "choices": [{
                "message": {
                    "content": "",
                    "tool_calls": [{ "function": { "name": "ls", "arguments": "{\"p\":\"/\"}" } }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": { "prompt_tokens": 5, "completion_tokens": 2 }
        });
        let r = chat_response(&v, "m");
        assert_eq!(r["done"], true);
        assert_eq!(r["message"]["tool_calls"][0]["function"]["name"], "ls");
        assert_eq!(r["prompt_eval_count"], 5);
        assert_eq!(r["eval_count"], 2);
    }

    #[test]
    fn maps_length_finish() {
        assert_eq!(done_reason("length"), "length");
        assert_eq!(done_reason("stop"), "stop");
    }
}
