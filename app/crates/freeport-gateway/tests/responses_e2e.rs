//! `/v1/responses`（Codex 原生协议）的端到端验证。
//!
//! 盯四件事：
//! 1. `input` 数组被摊平成 messages，`instructions` 进 system，tools 换成 chat 形态
//! 2. 非流式响应组装成 `object: "response"` 且 output 项类型正确
//! 3. 流式响应是 Responses 语义事件（response.created / output_text.delta / response.completed）
//! 4. 请求日志按真实入口记账，不再一律写成 /v1/chat/completions

use std::sync::{Arc, Mutex};

use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};

fn write_config(dir: &std::path::Path, providers: Vec<Value>) {
    std::fs::write(
        dir.join("freeport.json"),
        json!({
            "version": 1,
            "gateway": { "port": 0, "host": "127.0.0.1" },
            "providers": providers,
            "routing": { "chain": ["a:m1"] }
        })
        .to_string(),
    )
    .unwrap();
}

fn provider(id: &str, base: String) -> Value {
    json!({
        "id": id,
        "name": id,
        "baseUrl": base,
        "tier": "A",
        "keyMask": "test••••key",
        "enabled": true,
        "addedAt": "2026-10-19T00:00:00Z",
        "models": [{ "id": "m1", "ownedBy": id }],
        "health": { "status": "unknown" }
    })
}

async fn spawn(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}/v1")
}

const SSE_BODY: &str = concat!(
    "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"think\"}}]}\n\n",
    "data: {\"choices\":[{\"delta\":{\"content\":\"po\"}}]}\n\n",
    "data: {\"choices\":[{\"delta\":{\"content\":\"ng\"}}]}\n\n",
    "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":2}}\n\n",
    "data: [DONE]\n\n",
);

#[tokio::test]
async fn responses_endpoint_speaks_responses_both_ways() {
    let dir = std::env::temp_dir().join(format!("fp-resp-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("FREEPORT_DATA_DIR", &dir);

    let seen: Arc<Mutex<Vec<Value>>> = Default::default();
    let sink = seen.clone();
    let upstream = Router::new().route(
        "/v1/chat/completions",
        post(move |Json(body): Json<Value>| {
            let sink = sink.clone();
            async move {
                let streaming = body["stream"].as_bool().unwrap_or(false);
                sink.lock().unwrap().push(body);

                if streaming {
                    return ([("content-type", "text/event-stream")], SSE_BODY.to_string())
                        .into_response();
                }

                Json(json!({
                    "id": "cmpl-1",
                    "model": "m1",
                    "choices": [{
                        "message": {
                            "role": "assistant",
                            "content": "done",
                            "tool_calls": [{
                                "id": "call_a",
                                "type": "function",
                                "function": { "name": "read_file", "arguments": "{\"path\":\"/tmp\"}" }
                            }]
                        },
                        "finish_reason": "tool_calls"
                    }],
                    "usage": { "prompt_tokens": 11, "completion_tokens": 5, "total_tokens": 16 }
                }))
                .into_response()
            }
        }),
    );
    let base = spawn(upstream).await;

    write_config(&dir, vec![provider("a", base)]);
    freeport_gateway::secrets::set_key("a", "k").unwrap();

    let gw = freeport_gateway::Gateway::new();
    gw.load().await;
    let port = gw.start().await.unwrap();
    let client = reqwest::Client::new();

    /* ── 非流式：多轮 history + tool 回填 + 工具声明 ── */
    let res = client
        .post(format!("http://127.0.0.1:{port}/v1/responses"))
        .header("authorization", "Bearer whatever")
        .json(&json!({
            "model": "auto",
            "instructions": "be brief",
            "max_output_tokens": 128,
            "input": [
                { "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "hi" }] },
                { "type": "function_call", "call_id": "call_prev", "name": "read_file", "arguments": "{}" },
                { "type": "function_call_output", "call_id": "call_prev", "output": "file body" }
            ],
            "tools": [
                { "type": "web_search" },
                { "type": "function", "name": "read_file", "description": "r", "parameters": { "type": "object" } }
            ]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["object"], "response");
    assert_eq!(body["status"], "completed");
    assert_eq!(body["output"][0]["type"], "message");
    assert_eq!(body["output"][0]["content"][0]["text"], "done");
    assert_eq!(body["output"][1]["type"], "function_call");
    assert_eq!(body["output"][1]["call_id"], "call_a");
    assert_eq!(body["output"][1]["name"], "read_file");
    assert_eq!(body["usage"]["input_tokens"], 11);
    assert_eq!(body["usage"]["total_tokens"], 16);

    /* ── 流式：必须是 Responses 事件，不是 OpenAI chunk ── */
    let res = client
        .post(format!("http://127.0.0.1:{port}/v1/responses"))
        .header("authorization", "Bearer whatever")
        .json(&json!({
            "model": "auto",
            "stream": true,
            "input": "hi"
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let sse = res.text().await.unwrap();
    assert!(sse.contains("event: response.created"), "缺 response.created：{sse}");
    assert!(
        sse.contains("event: response.reasoning_summary_text.delta"),
        "思考增量没转出来：{sse}"
    );
    assert!(sse.contains("\"delta\":\"think\""), "思考内容丢了：{sse}");
    assert!(sse.contains("event: response.output_text.delta"), "缺 output_text.delta：{sse}");
    assert!(sse.contains("\"delta\":\"po\""), "第一段文本丢了：{sse}");
    assert!(sse.contains("\"delta\":\"ng\""), "第二段文本丢了：{sse}");
    assert!(sse.contains("event: response.output_item.done"), "缺 output_item.done：{sse}");
    assert!(sse.contains("event: response.completed"), "缺 response.completed：{sse}");
    assert!(sse.contains("\"input_tokens\":7"), "usage 没带回去：{sse}");
    assert!(!sse.contains("chat.completion.chunk"), "不该把 OpenAI 原始块漏出去");

    /* ── 上游实际收到的形态 ── */
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2, "应当有两次上游调用");
    assert_eq!(seen[0]["messages"][0]["role"], "system");
    assert_eq!(seen[0]["messages"][0]["content"], "be brief");
    assert_eq!(seen[0]["messages"][1]["content"], "hi");
    assert_eq!(seen[0]["messages"][2]["tool_calls"][0]["id"], "call_prev");
    assert_eq!(seen[0]["messages"][3]["role"], "tool");
    assert_eq!(seen[0]["messages"][3]["content"], "file body");
    // hosted tool（web_search）没有 name，必须被丢掉，只留真函数
    assert_eq!(seen[0]["tools"].as_array().unwrap().len(), 1);
    assert_eq!(seen[0]["tools"][0]["function"]["name"], "read_file");
    assert_eq!(seen[0]["max_tokens"], 128);
    assert_eq!(seen[1]["stream"], true);
    assert_eq!(seen[1]["messages"][0]["content"], "hi", "字符串 input 也要变成 user 消息");
    drop(seen);

    /* ── 日志按真实入口记账 ── */
    let logs = gw.logs().await;
    assert_eq!(logs.len(), 2);
    assert_eq!(logs[0].path, "/v1/responses", "流式那次应记为 /v1/responses");
    assert_eq!(logs[1].path, "/v1/responses");
    // 测试里用的是老全局 token，归因应当落到 legacy 上而不是空
    assert_eq!(logs[0].key_id.as_deref(), Some("default"));
    assert_eq!(logs[0].key_name.as_deref(), Some("legacy token"));

    gw.stop().await;
    let _ = std::fs::remove_dir_all(&dir);
}
