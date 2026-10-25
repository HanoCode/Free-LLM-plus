//! OTLP 导出（OTLP/HTTP + JSON 编码，traces 信号）。
//!
//! 把每条请求日志变成一个 OTLP span，按小批推给用户指定的 OpenTelemetry
//! Collector（或任何讲 OTLP/HTTP 的后端：Jaeger / Tempo / SigNoz …）。有了
//! 这条路，「为什么这次选了它、哪一跳挂的、烧了多少 token」就能进用户自己
//! 的观测栈，而不是只有本机界面看得到。
//!
//! ## 实现取舍
//!
//! - **手写 OTLP/JSON，不引 opentelemetry 系依赖**：OTLP/HTTP 的 JSON 编码
//!   是规范化的（64 位整数编码为十进制字符串、resource → scopeSpans → spans
//!   三层），collector 的 OTLP receiver 原生接受 `application/json`。为此多
//!   拉一整套 opentelemetry + tonic 的编译成本不值。
//! - **一请求一 span**：span 属性带 provider / account / model / purpose /
//!   attempts / token / 折算成本；每跳失败是 span 的一个 event。追踪整条接力
//!   链用不着多 span，属性已经说得清。
//! - **只做 trace，不做 metrics**：额度账本（4 窗口）就是本产品的 metrics，
//!   再往 collector 推一份重复的 counter 只会制造两个对不上的数。
//! - **绝不反压**：队列有上限，满了丢最旧的并计数。观测是增强功能，
//!   collector 挂了不该让用户的请求跟着挂。
//! - **默认关**：这会把请求元数据发往网络，必须用户显式开并填端点。
//!   span 里只有元数据，没有 prompt / 回复内容。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use freeport_core::model::LogEntry;
use serde_json::{json, Value};

use crate::server::State_;

/// 队列上限。超过就丢最旧的——collector 卡住时最多丢这么多，不会吃内存。
const MAX_QUEUE: usize = 2048;
/// 单次上报的 span 数上限（可配，硬顶 512）。
const MAX_BATCH: usize = 512;

/// 把一条请求日志挂进导出队列。`record()` 的尾巴调用；关着时零成本。
pub async fn on_record(st: &Arc<State_>, entry: &LogEntry) {
    let enabled = { st.data.read().await.gateway.otlp.enabled };
    if !enabled {
        return;
    }
    // 折算成本要在这一步算：目录价可能热更新，span 里存的是当时的价格
    let cost = {
        let cat = st.catalog.read().await;
        entry
            .provider
            .as_deref()
            .zip(entry.model.as_deref())
            .and_then(|(p, m)| cat.find_model(p, m))
            .and_then(|x| freeport_core::observe::cost_of(entry.usage.as_ref(), x.cost_in, x.cost_out))
    };
    let span = span_of(entry, cost);
    let mut q = st.otlp_queue.lock().unwrap_or_else(|e| e.into_inner());
    if q.len() >= MAX_QUEUE {
        q.pop_front();
        st.otlp_dropped.fetch_add(1, Ordering::Relaxed);
    }
    q.push_back(span);
}

/// 常驻导出循环。随 Gateway 生命周期启停；配置每轮重读，改端点即时生效。
pub fn spawn(gw: &crate::server::Gateway) {
    let st = gw.st.clone();
    tokio::spawn(async move {
        let mut shutdown = std::pin::pin!(st.shutdown.notified());
        loop {
            let (enabled, endpoint, headers, batch, interval) = {
                let d = st.data.read().await;
                let o = &d.gateway.otlp;
                (
                    o.enabled,
                    o.endpoint.clone(),
                    o.headers.clone(),
                    o.batch_size.clamp(1, MAX_BATCH),
                    o.interval_secs.clamp(1, 300),
                )
            };
            let tick = tokio::time::sleep(Duration::from_secs(interval));
            tokio::select! {
                _ = &mut shutdown => return,
                _ = tick => {}
            }
            if !enabled || endpoint.trim().is_empty() {
                // 关着的时候把残留队列清掉：重新开启不应该把积压一口气泼出去
                st.otlp_queue.lock().unwrap_or_else(|e| e.into_inner()).clear();
                continue;
            }
            let batch_spans: Vec<Value> = {
                let mut q = st.otlp_queue.lock().unwrap_or_else(|e| e.into_inner());
                let take = q.len().min(batch);
                q.drain(..take).collect()
            };
            if batch_spans.is_empty() {
                continue;
            }
            if let Err(e) = export(&st.client, &endpoint, &headers, batch_spans).await {
                let dropped = st.otlp_dropped.fetch_add(0, Ordering::Relaxed);
                tracing::warn!(error = %e, dropped_total = dropped, "OTLP 上报失败，本批已丢弃");
            }
        }
    });
}

async fn export(
    client: &reqwest::Client,
    endpoint: &str,
    headers: &[String],
    spans: Vec<Value>,
) -> Result<(), String> {
    let url = traces_url(endpoint);
    let mut req = client
        .post(&url)
        .timeout(Duration::from_secs(5))
        .header("content-type", "application/json")
        .json(&export_payload(spans));
    for h in headers {
        // 自定义头一行一个 `Key: Value`（比如 collector 要 Bearer 时）
        let Some((k, v)) = h.split_once(':') else {
            continue;
        };
        let k = k.trim();
        if k.is_empty() || k.eq_ignore_ascii_case("content-type") {
            continue;
        }
        req = req.header(k, v.trim());
    }
    let res = req.send().await.map_err(|e| e.to_string())?;
    let status = res.status();
    if !(200..300).contains(&status.as_u16()) {
        let body = res.text().await.unwrap_or_default();
        // 只截一小段：collector 的错误体不该刷爆日志
        return Err(format!("{status}: {}", body.chars().take(200).collect::<String>()));
    }
    Ok(())
}

/// OTLP traces 端点。填基址（如 `http://127.0.0.1:4318`）自动补
/// `/v1/traces`；已经写到 `/v1/traces` 的按原样用，两种习惯都收。
fn traces_url(endpoint: &str) -> String {
    let e = endpoint.trim().trim_end_matches('/');
    if e.ends_with("/v1/traces") {
        e.to_string()
    } else {
        format!("{e}/v1/traces")
    }
}

fn export_payload(spans: Vec<Value>) -> Value {
    json!({
        "resourceSpans": [{
            "resource": { "attributes": [
                str_attr("service.name", "freeport-gateway"),
                str_attr("service.version", env!("CARGO_PKG_VERSION")),
                str_attr("telemetry.sdk.language", "rust"),
            ]},
            "scopeSpans": [{
                "scope": { "name": "freeport-gateway", "version": env!("CARGO_PKG_VERSION") },
                "spans": spans,
            }]
        }]
    })
}

fn str_attr(k: &str, v: &str) -> Value {
    json!({ "key": k, "value": { "stringValue": v } })
}

fn int_attr(k: &str, v: i64) -> Value {
    json!({ "key": k, "value": { "intValue": v.to_string() } })
}

fn bool_attr(k: &str, v: bool) -> Value {
    json!({ "key": k, "value": { "boolValue": v } })
}

fn double_attr(k: &str, v: f64) -> Value {
    json!({ "key": k, "value": { "doubleValue": v } })
}

/// LogEntry → OTLP span。失败时 status.code = ERROR，每跳失败是一个 event。
fn span_of(entry: &LogEntry, cost: Option<f64>) -> Value {
    // OTLP 的 64 位时间戳在 JSON 里必须是十进制字符串（超出 JS 安全整数）
    let start_nano: u128 = chrono::DateTime::parse_from_rfc3339(&entry.at)
        .ok()
        .and_then(|t| t.timestamp_nanos_opt())
        .map(|n| n.max(0) as u128)
        .unwrap_or(0);
    let end_nano = start_nano + entry.ms as u128 * 1_000_000;

    let mut attrs = vec![
        str_attr("http.request.method", "POST"),
        str_attr("url.path", &entry.path),
        str_attr("freeport.requested_model", &entry.requested_model),
        bool_attr("freeport.stream", entry.stream),
        bool_attr("freeport.ok", entry.ok),
        int_attr("http.response.status_code", entry.status as i64),
        int_attr("freeport.duration_ms", entry.ms as i64),
        str_attr("freeport.purpose", entry.purpose.as_str()),
        int_attr("freeport.attempts", entry.attempts.len() as i64),
    ];
    if let Some(m) = &entry.model {
        attrs.push(str_attr("freeport.model", m));
    }
    if let Some(p) = &entry.provider {
        attrs.push(str_attr("freeport.provider", p));
    }
    if let Some(a) = &entry.account_id {
        attrs.push(str_attr("freeport.account", a));
    }
    if let Some(k) = &entry.key_id {
        attrs.push(str_attr("freeport.key", k));
    }
    if let Some(u) = &entry.usage {
        attrs.push(int_attr("freeport.tokens.input", u.tokens_in() as i64));
        attrs.push(int_attr("freeport.tokens.output", u.tokens_out() as i64));
    }
    if let Some(c) = cost {
        attrs.push(double_attr("freeport.cost.usd", (c * 1e6).round() / 1e6));
    }
    if let Some(e) = &entry.error {
        attrs.push(str_attr("error.message", e));
    }

    let events: Vec<Value> = entry
        .attempts
        .iter()
        .map(|a| {
            // kind 为空串表示这一跳成功了，不作为错误事件标记
            json!({
                // event 时间只能取请求开始 + 已耗时：Attempt.ms 是这一跳的时长
                "timeUnixNano": (start_nano + a.ms as u128 * 1_000_000).to_string(),
                "name": "freeport.attempt",
                "attributes": [
                    str_attr("freeport.provider", &a.provider),
                    int_attr("http.response.status_code", a.status as i64),
                    int_attr("freeport.duration_ms", a.ms as i64),
                    str_attr("freeport.error_kind", &a.kind),
                ],
            })
        })
        .collect();

    json!({
        "traceId": hex_id(16),
        "spanId": hex_id(8),
        "name": "freeport.request",
        "kind": 2,
        "startTimeUnixNano": start_nano.to_string(),
        "endTimeUnixNano": end_nano.to_string(),
        "attributes": attrs,
        "events": events,
        "status": { "code": if entry.ok { 1 } else { 2 } },
    })
}

fn hex_id(bytes: usize) -> String {
    let id = uuid::Uuid::new_v4();
    let b = id.as_bytes();
    b[..bytes].iter().map(|x| format!("{x:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use freeport_core::model::{Attempt, LogEntry, Purpose, Usage};

    fn entry() -> LogEntry {
        LogEntry {
            id: "l1".into(),
            at: "2026-10-25T08:00:00Z".into(),
            path: "/v1/chat/completions".into(),
            requested_model: "auto".into(),
            model: Some("groq:m1".into()),
            provider: Some("groq".into()),
            account_id: Some("acct_1".into()),
            account_name: None,
            stream: true,
            ok: true,
            status: 200,
            ms: 1234,
            usage: Some(Usage {
                prompt_tokens: Some(100),
                completion_tokens: Some(50),
                total_tokens: Some(150),
            }),
            attempts: vec![Attempt {
                provider: "groq".into(),
                account: Some("acct_1".into()),
                model: "m1".into(),
                ms: 1200,
                status: 200,
                kind: String::new(),
                scope: None,
                cooled_ms: None,
                detail: None,
                skip_provider: None,
            }],
            key_id: Some("agent:claude-code".into()),
            key_name: None,
            sticky: false,
            purpose: Purpose::Coding,
            purpose_source: None,
            error: None,
        }
    }

    #[test]
    fn span_carries_routing_metadata_and_ids() {
        let s = span_of(&entry(), Some(0.000012));
        assert_eq!(s["name"], "freeport.request");
        assert_eq!(s["kind"], 2);
        assert_eq!(s["status"]["code"], 1);
        let trace = s["traceId"].as_str().unwrap();
        let span = s["spanId"].as_str().unwrap();
        assert_eq!(trace.len(), 32, "traceId 必须是 32 位 hex");
        assert_eq!(span.len(), 16, "spanId 必须是 16 位 hex");
        assert!(trace.chars().all(|c| c.is_ascii_hexdigit()));
        // 64 位时间戳在 JSON 里必须是字符串，数字会被 JS 端截断
        assert!(s["startTimeUnixNano"].is_string());
        assert!(s["endTimeUnixNano"].as_str().unwrap().parse::<u128>().unwrap()
            > s["startTimeUnixNano"].as_str().unwrap().parse::<u128>().unwrap());

        let get = |k: &str| {
            s["attributes"]
                .as_array()
                .unwrap()
                .iter()
                .find(|a| a["key"] == k)
                .map(|a| a["value"].clone())
        };
        assert_eq!(get("freeport.model").unwrap()["stringValue"], "groq:m1");
        assert_eq!(get("freeport.provider").unwrap()["stringValue"], "groq");
        assert_eq!(get("freeport.purpose").unwrap()["stringValue"], "coding");
        assert_eq!(get("freeport.tokens.input").unwrap()["intValue"], "100");
        assert_eq!(get("freeport.cost.usd").unwrap()["doubleValue"], 0.000012);
        assert_eq!(get("freeport.key").unwrap()["stringValue"], "agent:claude-code");
    }

    #[test]
    fn failures_become_error_status_and_attempt_events() {
        let mut e = entry();
        e.ok = false;
        e.error = Some("全部候选失败".into());
        e.attempts.push(Attempt {
            provider: "cerebras".into(),
            account: None,
            model: "m1".into(),
            ms: 300,
            status: 429,
            kind: "rate_limited".into(),
            scope: None,
            cooled_ms: None,
            detail: None,
            skip_provider: None,
        });
        let s = span_of(&e, None);
        assert_eq!(s["status"]["code"], 2);
        let ev = s["events"].as_array().unwrap();
        assert_eq!(ev.len(), 2);
        assert_eq!(ev[1]["name"], "freeport.attempt");
        let kind = ev[1]["attributes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["key"] == "freeport.error_kind")
            .unwrap();
        assert_eq!(kind["value"]["stringValue"], "rate_limited");
    }

    #[test]
    fn traces_url_accepts_both_endpoint_styles() {
        assert_eq!(
            traces_url("http://127.0.0.1:4318"),
            "http://127.0.0.1:4318/v1/traces"
        );
        assert_eq!(
            traces_url("http://127.0.0.1:4318/"),
            "http://127.0.0.1:4318/v1/traces"
        );
        assert_eq!(
            traces_url("http://127.0.0.1:4318/v1/traces"),
            "http://127.0.0.1:4318/v1/traces"
        );
    }

    #[test]
    fn payload_wraps_resource_and_scope() {
        let p = export_payload(vec![span_of(&entry(), None)]);
        let rs = p["resourceSpans"].as_array().unwrap();
        assert_eq!(rs.len(), 1);
        let svc = rs[0]["resource"]["attributes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["key"] == "service.name")
            .unwrap();
        assert_eq!(svc["value"]["stringValue"], "freeport-gateway");
        let spans = rs[0]["scopeSpans"][0]["spans"].as_array().unwrap();
        assert_eq!(spans.len(), 1);
    }
}
