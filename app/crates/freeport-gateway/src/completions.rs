//! OpenAI 旧版补全协议（`/v1/completions`）适配。
//!
//! 免费厂商只会 Chat Completions，而一堆老工具与 SDK 仍在说 `/v1/completions`：
//! 请求体是 `prompt` + `max_tokens`，响应是 `choices[].text`，流式每块也是 `text`
//! 而不是 `delta`。这里做「一个 prompt → 一条 user 消息」的最小闭环。
//!
//! **刻意做的取舍**（写在这里，免得以后被当成 bug）：
//! - `prompt` 数组只接受单条。多条等于让上游一次跑多个独立补全，而 chat 接口
//!   没有这个语义；静默只跑第一条会给出一个看起来正常的错答案。
//! - **不接受 token id 数组**：上游要的是文本，我们无法把 token id 还原成它认识
//!   的东西。宁可 400，也不要编一段假 prompt。
//! - **不接受 `suffix`**（中间填充）：那是需要上游原生 FIM 的能力，转发实现不了。
//! - `echo` 在本地实现：把 prompt 拼回 `text` 前面。这件事语义明确、不需要上游配合。
//! - 不透传 `best_of` / `logprobs` / `top_logprobs`：多数免费厂商会因为不认识的字段
//!   直接 400，而 400 会触发换家、白烧一次额度。
//! - 工具调用没有旧协议对应物：`finish_reason: tool_calls` 一律降级成 `stop`
//!   （旧协议的读者不认识工具，把调用当普通文本收尾才是它期待的行为）。

use std::collections::VecDeque;

use axum::body::Body;
use bytes::Bytes;
use futures_util::stream::Stream;
use serde_json::{json, Map, Value};

/// 请求侧转换。返回 `(chat 请求体, 原始 prompt 文本)`——后者只给 `echo` 用。
pub fn to_chat(v: &Value) -> Result<(Value, String), String> {
    let prompt = match v.get("prompt") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(a)) => {
            if a.len() != 1 {
                return Err(
                    "prompt 只支持单条：多条 prompt 等于批量补全，chat 接口没有这个语义".into(),
                );
            }
            match a.first() {
                Some(Value::String(s)) => s.clone(),
                _ => {
                    return Err(
                        "prompt 数组里只接受字符串；token id 无法还原成上游认识的文本".into(),
                    )
                }
            }
        }
        _ => return Err("缺少 prompt（应为字符串）".into()),
    };

    if let Some(s) = v.get("suffix").and_then(|x| x.as_str()) {
        if !s.is_empty() {
            return Err("suffix（中间填充）需要上游原生 FIM 支持，转发实现不了".into());
        }
    }

    let mut out = Map::new();
    out.insert(
        "model".into(),
        v.get("model").cloned().unwrap_or(json!("auto")),
    );
    out.insert(
        "stream".into(),
        json!(v.get("stream").and_then(|x| x.as_bool()).unwrap_or(false)),
    );
    out.insert(
        "messages".into(),
        json!([{ "role": "user", "content": prompt }]),
    );

    // 只放「免费厂商普遍认」的字段。多一个不认识的字段就是一次 400 换家。
    for k in [
        "max_tokens",
        "temperature",
        "top_p",
        "stop",
        "presence_penalty",
        "frequency_penalty",
        "seed",
        "user",
        "n",
        "stream_options",
    ] {
        if let Some(x) = v.get(k) {
            out.insert(k.into(), x.clone());
        }
    }
    Ok((Value::Object(out), prompt))
}

/// 响应侧转换：`chat.completion` → `text_completion`。
///
/// `echo` 为 `Some(prompt)` 时把 prompt 拼回每条 choice 的 `text` 前面。
pub fn from_chat(v: &Value, fallback_model: &str, echo: Option<&str>) -> Value {
    let id = legacy_id(v.get("id").and_then(|x| x.as_str()).unwrap_or(""));
    let created = v
        .get("created")
        .cloned()
        .unwrap_or_else(|| json!(chrono::Utc::now().timestamp()));
    let model = v
        .get("model")
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(fallback_model);

    let choices: Vec<Value> = v
        .get("choices")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .map(|c| {
                    let text = c
                        .pointer("/message/content")
                        .and_then(|x| x.as_str())
                        .unwrap_or("");
                    let text = match echo {
                        Some(p) if !p.is_empty() => format!("{p}{text}"),
                        _ => text.to_string(),
                    };
                    json!({
                        "text": text,
                        "index": c.get("index").cloned().unwrap_or(json!(0)),
                        // 旧协议的 logprobs 是扁平数组，chat 给的是嵌套对象。
                        // 没有上游原生支持时给 null 比给一个形状不对的对象诚实。
                        "logprobs": Value::Null,
                        "finish_reason": legacy_finish(c.get("finish_reason").and_then(|x| x.as_str())),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let mut out = json!({
        "id": id,
        "object": "text_completion",
        "created": created,
        "model": model,
        "choices": choices,
    });
    // usage 的字段名两个协议一样（prompt_tokens / completion_tokens / total_tokens）
    if let Some(u) = v.get("usage") {
        out["usage"] = u.clone();
    }
    out
}

/// `chatcmpl-xxx` → `cmpl-xxx`；上游没给 id 时自己造一个。
fn legacy_id(id: &str) -> String {
    if id.is_empty() {
        return format!(
            "cmpl-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..24]
        );
    }
    match id.strip_prefix("chatcmpl-") {
        Some(rest) => format!("cmpl-{rest}"),
        None => id.to_string(),
    }
}

/// 旧协议只认 stop / length / content_filter。工具调用降级成 stop。
fn legacy_finish(f: Option<&str>) -> Value {
    match f {
        None => Value::Null,
        Some("length") => json!("length"),
        Some("content_filter") => json!("content_filter"),
        Some(_) => json!("stop"),
    }
}

/* ── 流式：chat chunk → text_completion chunk ───── */

pub fn sse_transform(body: Body, model: String, echo: Option<String>) -> Body {
    Body::from_stream(Xf::new(
        Box::pin(body.into_data_stream()),
        model,
        echo,
    ))
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
    created: i64,
    model: String,
    /// prompt 回显只拼一次；空串等于没有
    echo: Option<String>,
}

impl Xf {
    fn new(
        inner: futures_util::stream::BoxStream<'static, Result<Bytes, axum::Error>>,
        model: String,
        echo: Option<String>,
    ) -> Self {
        Self {
            inner,
            buf: String::new(),
            out: VecDeque::new(),
            st: St {
                id: format!("cmpl-{}", &uuid::Uuid::new_v4().simple().to_string()[..24]),
                created: chrono::Utc::now().timestamp(),
                model,
                echo: echo.filter(|s| !s.is_empty()),
            },
            done: false,
        }
    }

    fn drain_frames(&mut self) {
        loop {
            let Some(pos) = self.buf.find("\n\n") else { break };
            let frame: String = self.buf.drain(..pos + 2).collect();
            for line in frame.lines() {
                let Some(rest) = line.strip_prefix("data:") else {
                    continue;
                };
                let rest = rest.trim();
                if rest.is_empty() {
                    continue;
                }
                if rest == "[DONE]" {
                    self.out.push_back(Bytes::from("data: [DONE]\n\n"));
                    self.done = true;
                    continue;
                }
                let Ok(v) = serde_json::from_str::<Value>(rest) else {
                    continue;
                };
                // 上游在流里塞 error：原样转过去。旧协议的客户端认这个形状。
                if let Some(err) = v.get("error") {
                    self.out.push_back(Bytes::from(format!(
                        "data: {}\n\n",
                        json!({ "error": err })
                    )));
                    self.done = true;
                    continue;
                }
                if let Some(chunk) = self.chunk_of(&v) {
                    self.out
                        .push_back(Bytes::from(format!("data: {chunk}\n\n")));
                }
            }
        }
    }

    /// 一个 chat chunk → 一个 legacy chunk。返回 `None` 表示这块没必要转发
    /// （例如既没有文本也没有收尾原因的中间块）。
    fn chunk_of(&mut self, v: &Value) -> Option<Value> {
        let choices: Vec<Value> = v
            .get("choices")
            .and_then(|x| x.as_array())
            .map(|arr| {
                arr.iter()
                    .map(|c| {
                        let mut text = c
                            .pointer("/delta/content")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string();
                        let finish = c.get("finish_reason").and_then(|x| x.as_str());
                        // echo 挂在第一块有内容的增量上：role-only 的首块拼上去
                        // 会让客户端以为 prompt 单独成一段
                        if !text.is_empty() {
                            if let Some(p) = self.st.echo.take() {
                                text = format!("{p}{text}");
                            }
                        }
                        json!({
                            "text": text,
                            "index": c.get("index").cloned().unwrap_or(json!(0)),
                            "logprobs": Value::Null,
                            "finish_reason": legacy_finish(finish),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        // 既无内容又无收尾的块（纯 role 声明）→ 旧协议里没有对应物，丢掉
        let meaningful = choices.iter().any(|c| {
            c["text"].as_str().map(|s| !s.is_empty()).unwrap_or(false)
                || !c["finish_reason"].is_null()
        });
        let has_usage = v.get("usage").map(|u| !u.is_null()).unwrap_or(false);
        if !meaningful && !has_usage {
            return None;
        }

        // 上游没给 id 时**整条流共用一个**：每块各造一个新 id 会让「同一段补全」
        // 在客户端看来是好几段，前缀匹配之类的逻辑直接失效。
        let id = match v
            .get("id")
            .and_then(|x| x.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(x) => legacy_id(x),
            None => self.st.id.clone(),
        };

        let mut out = json!({
            "id": id,
            "object": "text_completion",
            "created": v.get("created").cloned().unwrap_or(json!(self.st.created)),
            "model": v
                .get("model")
                .and_then(|x| x.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or(&self.st.model),
            "choices": choices,
        });
        if has_usage {
            out["usage"] = v["usage"].clone();
        }
        Some(out)
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
                // 上游流断了：旧协议没有「事件」可以承载错误，直接收尾。
                // 客户端看到的是「补全提前结束」，比卡住直到超时好。
                std::task::Poll::Ready(Some(Err(_))) | std::task::Poll::Ready(None) => {
                    me.out.push_back(Bytes::from("data: [DONE]\n\n"));
                    me.done = true;
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
    fn prompt_becomes_a_single_user_message() {
        let (body, prompt) = to_chat(&json!({
            "model": "groq:llama",
            "prompt": "say hi",
            "max_tokens": 32,
            "temperature": 0.2,
            "echo": true
        }))
        .unwrap();
        assert_eq!(prompt, "say hi");
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"], "say hi");
        assert_eq!(body["max_tokens"], 32);
        // echo 是本地实现的，不该漏给上游（多一个不认识的字段就是一次 400）
        assert!(body.get("echo").is_none());
        assert!(body.get("prompt").is_none());
    }

    #[test]
    fn unsupported_prompt_shapes_are_refused_not_guessed() {
        // 批量
        assert!(to_chat(&json!({ "prompt": ["a", "b"] })).is_err());
        // token id
        assert!(to_chat(&json!({ "prompt": [1, 2, 3] })).is_err());
        // FIM
        assert!(to_chat(&json!({ "prompt": "a", "suffix": "b" })).is_err());
        // 单条数组是合法的
        let (body, _) = to_chat(&json!({ "prompt": ["solo"] })).unwrap();
        assert_eq!(body["messages"][0]["content"], "solo");
    }

    #[test]
    fn response_is_rewritten_into_the_legacy_shape() {
        let chat = json!({
            "id": "chatcmpl-abc",
            "object": "chat.completion",
            "created": 123,
            "model": "m1",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": "done" },
                "finish_reason": "tool_calls"
            }],
            "usage": { "prompt_tokens": 3, "completion_tokens": 2, "total_tokens": 5 }
        });
        let v = from_chat(&chat, "m1", None);
        assert_eq!(v["id"], "cmpl-abc");
        assert_eq!(v["object"], "text_completion");
        assert_eq!(v["choices"][0]["text"], "done");
        // 旧协议没有工具概念，收尾降级成 stop
        assert_eq!(v["choices"][0]["finish_reason"], "stop");
        assert!(v["choices"][0]["logprobs"].is_null());
        assert_eq!(v["usage"]["total_tokens"], 5);
    }

    #[test]
    fn echo_prepends_the_prompt_to_every_choice() {
        let chat = json!({
            "choices": [
                { "index": 0, "message": { "content": " world" }, "finish_reason": "stop" },
                { "index": 1, "message": { "content": "!" }, "finish_reason": "length" }
            ]
        });
        let v = from_chat(&chat, "m1", Some("hello"));
        assert_eq!(v["choices"][0]["text"], "hello world");
        assert_eq!(v["choices"][1]["text"], "hello!");
        assert_eq!(v["choices"][1]["finish_reason"], "length");
    }

    #[test]
    fn stream_chunks_only_carry_what_the_legacy_client_can_read() {
        let mut xf = Xf::new(
            Box::pin(futures_util::stream::empty::<Result<Bytes, axum::Error>>()),
            "m1".into(),
            Some("hi ".into()),
        );

        // role-only 首块：旧协议没有 delta.role，丢掉
        assert!(xf
            .chunk_of(&json!({ "choices": [{ "index": 0, "delta": { "role": "assistant" } }] }))
            .is_none());

        // 第一段内容带上 echo，之后不再重复
        let c = xf
            .chunk_of(&json!({ "choices": [{ "index": 0, "delta": { "content": "there" } }] }))
            .unwrap();
        assert_eq!(c["choices"][0]["text"], "hi there");
        let c = xf
            .chunk_of(&json!({ "choices": [{ "index": 0, "delta": { "content": "!" } }] }))
            .unwrap();
        assert_eq!(c["choices"][0]["text"], "!");

        // 收尾块：没有文本但有 finish_reason，必须转发
        let c = xf
            .chunk_of(&json!({ "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }] }))
            .unwrap();
        assert_eq!(c["choices"][0]["finish_reason"], "stop");

        // usage-only 块（choices 为空）要留住，不然客户端算不出花费
        let c = xf
            .chunk_of(&json!({
                "choices": [],
                "usage": { "prompt_tokens": 1, "completion_tokens": 2, "total_tokens": 3 }
            }))
            .unwrap();
        assert_eq!(c["usage"]["total_tokens"], 3);
    }

    /// 上游不给 id 时，同一条流的所有块必须共用一个——否则客户端会以为是
    /// 好几段补全（前缀去重、增量拼接都会跟着错）。
    #[test]
    fn chunks_without_upstream_id_share_one_generated_id() {
        let mut xf = Xf::new(
            Box::pin(futures_util::stream::empty::<Result<Bytes, axum::Error>>()),
            "m1".into(),
            None,
        );
        let a = xf
            .chunk_of(&json!({ "choices": [{ "index": 0, "delta": { "content": "a" } }] }))
            .unwrap();
        let b = xf
            .chunk_of(&json!({ "choices": [{ "index": 0, "delta": { "content": "b" } }] }))
            .unwrap();
        assert_eq!(a["id"], b["id"]);
        assert!(a["id"].as_str().unwrap().starts_with("cmpl-"));
    }
}
