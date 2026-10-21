//! 工具定义与执行。
//!
//! 六个工具，全部只读，全部建立在 `Gateway` 已有的只读方法上（`status` / `models` /
//! `quota` / `digest` / `logs` / `explain_route`）。不新开数据通路，是因为这些方法
//! 已经被界面与 `/v1/route` 共用——MCP 看到的东西和用户看到的必须是同一份事实，
//! 否则「界面说能用、agent 说不能用」这种争执根本没法查。

use freeport_core::model::{ModelCaps, Purpose};
use freeport_gateway::Gateway;
use serde_json::{json, Value};

/// 调用失败的两类：名字错了（协议层）与跑挂了（工具层）。
pub enum CallError {
    Unknown(String),
    Failed(String),
}

/// 各工具内部只需要 `Result<_, String>`：它们是纯读，不会中途返回「未知工具」。
impl From<String> for CallError {
    fn from(msg: String) -> Self {
        CallError::Failed(msg)
    }
}

pub fn list() -> Value {
    json!({
        "tools": [
            {
                "name": "freeport_status",
                "title": "网关状态",
                "description": "Freeport 网关此刻的状态：是否在跑、是否暂停、监听端口、\
                                24 小时请求/错误计数、当前生效的冷却。回答「为什么请求被拒」之前先看它。\
                                不返回鉴权密钥。",
                "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
            },
            {
                "name": "freeport_models",
                "title": "可用模型",
                "description": "列出此刻真的可用的模型（已排除冷却中 / 不可用 / 已退役）。\
                                每条带厂商、档位、健康、是否免费、上下文长度、能力与实测延迟。\
                                需要 tool calling / 视觉 / JSON 模式时用 capability 过滤——\
                                能力未知的模型不会被当成「支持」。",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "provider": { "type": "string", "description": "只看这一家厂商（id）" },
                        "capability": {
                            "type": "string",
                            "enum": ["tools", "vision", "json", "reasoning"],
                            "description": "只要这项能力**实测或目录声明为支持**的模型"
                        },
                        "freeOnly": { "type": "boolean", "description": "只要零价模型" },
                        "includeUnavailable": {
                            "type": "boolean",
                            "description": "连冷却中/不可用的也列出来（排查用，默认 false）"
                        },
                        "limit": { "type": "integer", "minimum": 1, "maximum": 200, "default": 40 }
                    },
                    "additionalProperties": false
                }
            },
            {
                "name": "freeport_route",
                "title": "预演一条链路",
                "description": "给一个模型名，看这一单会走哪个厂商的哪条凭据，以及每个候选的权重\
                                与被排除的原因（冷却 / 额度耗尽 / 能力不符 / 已退役）。\
                                返回里的 `pool` 回答的是另一件事：这个用途池有几个号接着、\
                                最近战绩如何、排第一的号余量够不够。\
                                只预演，不推进轮转、不写粘性表，可以随便调。",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "model": { "type": "string", "description": "模型 id 或别名；auto 表示让网关自己挑" },
                        "session": { "type": "string", "description": "会话键，留空则不带粘性" },
                        "purpose": {
                            "type": "string",
                            "enum": ["chat", "coding", "vision", "probe"],
                            "description": "按哪个账号池预演。凭据可以打用途标签（coding 专用、vision 专用…），\
                                            同一份配置在不同池里的链可能完全不同。默认 chat"
                        },
                        "tokens": {
                            "type": "number",
                            "description": "这单预计吃掉多少 token（输入 + 输出）。给了才判「余量够不够」，\
                                            不给就不判断——猜一个数去报红比不报更糟"
                        }
                    },
                    "required": ["model"],
                    "additionalProperties": false
                }
            },
            {
                "name": "freeport_quota",
                "title": "额度余量",
                "description": "每个凭据的四个窗口（RPM/RPD/TPM/TPD）用量与剩余，以及正在生效的冷却。\
                                想知道「还能撑多久」「什么时候恢复」时用。",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "provider": { "type": "string", "description": "只看这一家厂商（id）" }
                    },
                    "additionalProperties": false
                }
            },
            {
                "name": "freeport_digest",
                "title": "今日摘要",
                "description": "过去 24 小时的目录变更、模型恢复、探针预算消耗，以及按用途池（chat / coding / vision / probe）\n                                分的近期请求成败，还有需要用户动手的事。回答「今天有什么变化」「我的 coding 号是不是挂了」时用。\n                                pools 的样本是网关内存里的最近若干条请求，条数见 logScanned。",
                "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
            },
            {
                "name": "freeport_recent_errors",
                "title": "最近的失败",
                "description": "最近的失败请求，含每一跳的厂商、凭据、状态码、错误类别与作用域。\
                                用户说「刚才那个请求挂了」时看它：能分清是凭据挂了、模型挂了，\
                                还是整个厂商挂了。网关在跑时读的是它内存里的实时日志\
                                （返回里 source=gateway）；连不上会退回磁盘快照并明说 source=disk。",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "limit": { "type": "integer", "minimum": 1, "maximum": 100, "default": 20 },
                        "model": { "type": "string", "description": "只看请求过这个模型的记录" },
                        "provider": { "type": "string", "description": "只看某一家厂商的尝试" }
                    },
                    "additionalProperties": false
                }
            }
        ]
    })
}

pub async fn call(gw: &Gateway, params: &Value) -> Result<Value, CallError> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| CallError::Failed("tools/call 缺少 name".into()))?;
    let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
    if !args.is_object() {
        return Err(CallError::Failed("arguments 必须是对象".into()));
    }

    let out = match name {
        "freeport_status" => status(gw).await,
        "freeport_models" => models(gw, &args).await,
        "freeport_route" => route(gw, &args).await,
        "freeport_quota" => quota(gw, &args).await,
        "freeport_digest" => digest(gw).await,
        "freeport_recent_errors" => recent_errors(gw, &args).await,
        other => return Err(CallError::Unknown(other.to_string())),
    }?;

    Ok(json!({
        "content": [{ "type": "text", "text": text_of(out) }],
        "isError": false,
    }))
}

/// 工具结果统一给一份缩进 JSON：模型读结构化文本比读散文靠谱，
/// 而且 `structuredContent` 还是 2025-06-18 才有的字段，不必依赖。
fn text_of(v: Value) -> String {
    serde_json::to_string_pretty(&v).unwrap_or_else(|_| v.to_string())
}

/* ── 各工具 ─────────────────────────────────────── */

async fn status(gw: &Gateway) -> Result<Value, String> {
    let s = gw.status().await;
    // 实时视图：问一次正在跑的那个网关。连不上就退回磁盘快照，并且**明说是快照**。
    let live = gw.inspect_live().await;
    let pick = |k: &str| live.as_ref().and_then(|v| v.get(k)).cloned();

    // `s.port` 是本进程绑定的端口；MCP 不起网关，所以通常是 None。
    // 但配置里那个端口才是客户端该连的地址——两个都给出，谁在跑都不影响读法。
    let (host, configured_port) = gw.configured_addr().await;
    let port = pick("port")
        .and_then(|v| v.as_u64())
        .map(|p| p as u16)
        .or(s.port)
        .or((configured_port > 0).then_some(configured_port));
    let base = port.map(|p| format!("http://{host}:{p}"));

    // `running` 只说明**本进程**有没有起网关。MCP 是独立进程，用户开着窗口时
    // 这里永远是 false——拿它说「没在跑」就是撒谎。
    let online = live.is_some();
    let hint = if online {
        "网关在另一个进程里跑着。下面的数字是它**此刻的内存状态**：实时日志、\
         内存里的冷却、在途请求数都在（走 /v1/freeport/inspect）。"
    } else if s.running {
        "网关就在本进程里跑着（MCP 与网关同进程：测试或内嵌场景）。"
    } else {
        "连不上网关：下面的数字来自磁盘快照（最后一次保存时的值）。\
         实时日志与内存冷却在网关进程里，这里看不到——别把「看不到」读成「没有」。"
    };

    Ok(json!({
        "running": pick("running").unwrap_or(json!(s.running)),
        "gatewayOnline": online,
        "paused": pick("paused").unwrap_or(json!(s.paused)),
        "source": if online { "gateway" } else { "disk" },
        "inflight": pick("inflight"),
        "uptimeSec": pick("uptimeSec"),
        "bind": s.bind,
        "host": host,
        "port": port,
        "configuredPort": configured_port,
        "baseUrl": base,
        "requireAuth": s.require_auth,
        "logTotal": pick("logTotal"),
        "requests24h": pick("stats").and_then(|v| v.get("requests24h").cloned()).unwrap_or(json!(s.requests24h)),
        "tokensIn": pick("stats").and_then(|v| v.get("tokensIn").cloned()).unwrap_or(json!(s.tokens_in)),
        "tokensOut": pick("stats").and_then(|v| v.get("tokensOut").cloned()).unwrap_or(json!(s.tokens_out)),
        "errors": pick("stats").and_then(|v| v.get("errors").cloned()).unwrap_or(json!(s.errors)),
        "requestBudgetMs": s.request_budget_ms,
        // 冷却：在线时用内存里的那份，离线时只能给磁盘快照（已经过时）
        "cooldowns": pick("cooldowns").unwrap_or(json!(s.cooldowns)),
        "since": s.since,
        "lastError": s.error,
        "hint": hint,
    }))
}

async fn models(gw: &Gateway, args: &Value) -> Result<Value, String> {
    let want_provider = arg_str(args, "provider");
    let want_cap = arg_str(args, "capability");
    let free_only = arg_bool(args, "freeOnly", false);
    let include_unavailable = arg_bool(args, "includeUnavailable", false);
    let limit = arg_usize(args, "limit", 40, 200);

    let all = gw.models().await;
    let total = all.len();

    let mut rows: Vec<Value> = Vec::new();
    let mut skipped_unhealthy = 0usize;
    for m in &all {
        let meta = m.freeport.as_ref();
        // `models()` 会额外给两行噪音：`freeport/auto` 这个虚拟模型，以及每个模型
        // 不带厂商前缀的裸 id 副本。工具输出要能直接喂给决策，重复项只会让模型
        // 在两行「同一个模型」之间反复犹豫。
        if meta.and_then(|x| x.virtual_model) == Some(true) {
            continue;
        }
        if meta.map(|x| x.upstream_model.is_none()).unwrap_or(false) {
            continue;
        }
        let provider = meta.and_then(|x| x.provider.clone()).unwrap_or_default();
        let health = meta
            .and_then(|x| x.health.clone())
            .unwrap_or_else(|| "unknown".into());

        if let Some(p) = want_provider {
            if !provider.eq_ignore_ascii_case(p) {
                continue;
            }
        }
        if !include_unavailable && !usable(&health) {
            skipped_unhealthy += 1;
            continue;
        }
        if free_only && meta.and_then(|x| x.free) != Some(true) {
            continue;
        }
        if let Some(cap) = want_cap {
            if !supports(meta.and_then(|x| x.caps.as_ref()), cap) {
                continue;
            }
        }

        rows.push(json!({
            "id": m.id,
            "provider": provider,
            "upstreamModel": meta.and_then(|x| x.upstream_model.clone()),
            "tier": meta.and_then(|x| x.tier.clone()),
            "health": health,
            "free": meta.and_then(|x| x.free),
            "contextLength": m.context_length,
            "caps": meta.and_then(|x| x.caps.clone()),
            "capsProbed": meta.and_then(|x| x.caps_probed),
            "latencyMs": meta.and_then(|x| x.latency_ms),
            "canary": meta.and_then(|x| x.candidate),
            "note": meta.and_then(|x| x.note.clone()),
        }));
    }

    // 稳定顺序：agent 多次调用拿到的顺序一致，diff 才有意义
    rows.sort_by(|a, b| {
        let ka = (
            a["provider"].as_str().unwrap_or(""),
            a["id"].as_str().unwrap_or(""),
        );
        let kb = (
            b["provider"].as_str().unwrap_or(""),
            b["id"].as_str().unwrap_or(""),
        );
        ka.cmp(&kb)
    });
    let matched = rows.len();
    rows.truncate(limit);

    let hint = if matched == 0 {
        "一个可用模型都没有。先让用户在 Freeport 的「厂商」页添加凭据；\
         刚加完需要一次探测才会变成可用。"
    } else if matched > rows.len() {
        "结果被 limit 截断了，用 provider / capability 收窄。"
    } else {
        ""
    };

    Ok(json!({
        "total": total,
        "matched": matched,
        "returned": rows.len(),
        "skippedUnhealthy": skipped_unhealthy,
        "models": rows,
        "hint": hint,
    }))
}

async fn route(gw: &Gateway, args: &Value) -> Result<Value, String> {
    let model = arg_str(args, "model")
        .ok_or_else(|| "缺少 model；不知道用哪个就传 auto".to_string())?;
    let session = arg_str(args, "session").unwrap_or("");
    let purpose = match arg_str(args, "purpose") {
        Some(s) => Purpose::parse(s)
            .ok_or_else(|| format!("purpose 只能是 chat / coding / vision / probe，收到 {s}"))?,
        None => Purpose::Chat,
    };
    let cost = args
        .get("tokens")
        .and_then(|v| v.as_f64())
        .filter(|v| v.is_finite() && *v > 0.0);
    let mut out = gw.explain_route_cost(model, session, purpose, cost).await;
    let hint = route_hint(&out["pool"]);
    out["hint"] = json!(hint);
    Ok(out)
}

/// 把 `pool.verdict` 翻成一句中文。
///
/// 措辞留在这一层而不是网关里：GUI 要中英双语，agent 只要一句能直接回给
/// 用户的话。网关给码、各端给话，才不会出现「同一件事两种说法」。
fn route_hint(pool: &Value) -> String {
    let purpose = pool["purpose"].as_str().unwrap_or("chat");
    let zh = purpose_zh(purpose);
    let admits = pool["accounts"]["admits"].as_u64().unwrap_or(0);
    let blocked = pool["accounts"]["blocked"].as_u64().unwrap_or(0);
    let acct = pool["landsOn"]["account"].as_str().unwrap_or("");
    let pct = (pool["landsOn"]["headroom"].as_f64().unwrap_or(0.0) * 100.0).round();

    match pool["verdict"].as_str().unwrap_or("") {
        "empty-pool" => format!(
            "没有任何凭据接「{zh}」用途（共 {} 条凭据，全在别的池里）。\
             让用户在「账号」页给某条凭据勾上 {purpose}，否则这类请求会全部 503。",
            pool["accounts"]["total"].as_u64().unwrap_or(0)
        ),
        "no-candidate" => format!(
            "池里 {admits} 个号接「{zh}」，但这一轮全被拦下（额度 / 冷却 / 能力 / 没 key）。\
             逐个看 rejected 里的 reason。"
        ),
        "too-big" => {
            let left = pool["shortfall"]["left"].as_f64().unwrap_or(0.0);
            let w = pool["shortfall"]["window"].as_str().unwrap_or("tpd");
            let cost = pool["cost"].as_f64().unwrap_or(0.0);
            format!(
                "会落在 {acct}，但它的 {w} 窗口只剩 {left:.0}，这单要 {cost:.0} —— 上游会 429。\
                 换一个跳或等窗口重置（tightest.resetAt）。"
            )
        }
        "pool-failing" => {
            let r = pool["recent"]["requests"].as_u64().unwrap_or(0);
            let e = pool["recent"]["errors"].as_u64().unwrap_or(0);
            format!("「{zh}」池最近 {r} 单挂了 {e} 单。先看是哪家在挂，别只盯这一跳。")
        }
        "tight" => format!(
            "会落在 {acct}，余量只剩 {pct}%。这一单还能跑，但很快就要换家。"
        ),
        _ => format!(
            "「{zh}」池 {admits} 个号接着{}，这一单会走 {acct}（余量 {pct}%）。",
            if blocked > 0 {
                format!("（另有 {blocked} 条在别的池里）")
            } else {
                String::new()
            }
        ),
    }
}

fn purpose_zh(p: &str) -> &'static str {
    match p {
        "coding" => "编码",
        "vision" => "图像",
        "probe" => "巡检",
        _ => "对话",
    }
}

async fn quota(gw: &Gateway, args: &Value) -> Result<Value, String> {
    let want_provider = arg_str(args, "provider");
    let rows = gw.quota().await;
    // 用途标签不在 GwQuota 里（它是账本视图），从厂商视图取一次建表。
    // 带上的理由：agent 看到「还有余量」也要知道这条额度是不是只给某类请求用。
    let pools: std::collections::HashMap<String, Vec<&'static str>> = gw
        .providers()
        .await
        .into_iter()
        .flat_map(|p| p.accounts)
        .map(|a| (a.id, a.purposes.iter().map(|x| x.as_str()).collect()))
        .collect();
    let accounts: Vec<Value> = rows
        .into_iter()
        .filter(|q| want_provider.map(|p| q.provider.eq_ignore_ascii_case(p)).unwrap_or(true))
        .map(|q| {
            json!({
                "provider": q.provider,
                "name": q.name,
                "accountId": q.account_id,
                "accountName": q.account_name,
                // 空数组 = 不限用途
                "purposes": pools.get(&q.account_id).cloned().unwrap_or_default(),
                "headroom": round3(q.headroom),
                "resetAt": q.reset_at,
                "resetAtIso": q.reset_at.map(iso),
                "windows": q.windows.iter().map(|w| json!({
                    "kind": w.kind,
                    "usedUser": w.used_user,
                    "usedProbe": w.used_probe,
                    "limit": w.limit,
                    "remaining": round3(w.remaining),
                    "exhausted": w.exhausted,
                    "resetAtIso": iso(w.reset_at),
                })).collect::<Vec<_>>(),
            })
        })
        .collect();

    let s = gw.status().await;
    Ok(json!({
        "accounts": accounts,
        "cooldowns": s.cooldowns,
        "hint": if accounts.is_empty() {
            "还没有任何凭据，或者所有厂商都被停用了。"
        } else {
            "headroom 是最紧的那个窗口的剩余比例；耗尽后看 resetAtIso。\
             purposes 为空表示这条凭据不限用途，非空表示它只接列出来的那几类请求。"
        },
    }))
}

async fn digest(gw: &Gateway) -> Result<Value, String> {
    let d = gw.digest().await;
    let mut v = serde_json::to_value(&d).map_err(|e| e.to_string())?;
    // 池统计的样本是网关内存日志。一个实例都没跑时它必然是空的，
    // 而空数组最容易被读成「每个池都正常」——把这句话明确写出来。
    if d.log_scanned == 0 {
        v["poolHint"] = json!(
            "没有读到任何请求日志：网关可能没在运行，或今天还没有流量。pools 为空不代表各池正常。"
        );
    }
    Ok(v)
}

async fn recent_errors(gw: &Gateway, args: &Value) -> Result<Value, String> {
    let limit = arg_usize(args, "limit", 20, 100);
    let want_model = arg_str(args, "model");
    let want_provider = arg_str(args, "provider");

    // 优先问正在跑的那个网关要实时日志。连不上才退回本进程的日志——
    // 那个通常是空的，而空结果最容易被读成「一条错都没有」。
    let live = gw.inspect_live().await;
    let (logs, log_total, source) = match &live {
        Some(v) => {
            let total = v.get("logTotal").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
            let arr = v.get("logs").cloned().unwrap_or_else(|| json!([]));
            let parsed: Vec<freeport_core::model::LogEntry> =
                serde_json::from_value(arr).unwrap_or_default();
            (parsed, total, "gateway")
        }
        None => {
            let own = gw.logs().await;
            let n = own.len();
            (own, n, "process")
        }
    };
    let mut rows: Vec<Value> = logs
        .iter()
        .filter(|l| !l.ok || l.status >= 400)
        .filter(|l| {
            want_model
                .map(|m| {
                    l.requested_model.eq_ignore_ascii_case(m)
                        || l.model.as_deref() == Some(m)
                })
                .unwrap_or(true)
        })
        .filter(|l| {
            want_provider
                .map(|p| {
                    l.provider.as_deref().map(|x| x.eq_ignore_ascii_case(p)).unwrap_or(false)
                        || l.attempts
                            .iter()
                            .any(|a| a.provider.eq_ignore_ascii_case(p))
                })
                .unwrap_or(true)
        })
        .map(|l| {
            json!({
                "at": l.at,
                "path": l.path,
                "requestedModel": l.requested_model,
                "model": l.model,
                "provider": l.provider,
                "accountName": l.account_name,
                "status": l.status,
                "ms": l.ms,
                "error": l.error,
                "attempts": l.attempts.iter().map(|a| json!({
                    "provider": a.provider,
                    "account": a.account,
                    "model": a.model,
                    "status": a.status,
                    "kind": a.kind,
                    "scope": a.scope,
                    "detail": a.detail,
                })).collect::<Vec<_>>(),
            })
        })
        .collect();

    // 日志本身是时间正序，取最近的；输出按最新在前，省得模型自己倒着看
    if rows.len() > limit {
        rows = rows.split_off(rows.len() - limit);
    }
    rows.reverse();

    let online = live.is_some();
    Ok(json!({
        "count": rows.len(),
        "logTotal": log_total,
        "source": source,
        "gatewayOnline": online,
        "errors": rows,
        "hint": if !rows.is_empty() {
            "scope 说明失败的作用域：key（换凭据可能就行）/ model（换模型）/ \
             platform（这一家整个挂了，换厂商才管用）。"
        } else if online {
            // 这次是**真的**看过了网关的实时日志，可以下这个结论
            "网关在线，它的实时日志里确实没有失败记录（这条结论可以信）。\
             想看更早的，去「日志」页——那里和这里读的是同一份环形缓冲。"
        } else {
            "连不上网关：这份日志来自 MCP 自己的进程内缓冲，基本是空的——\
             别把它读成「一条错都没有」。让用户打开 Freeport（或 `freeport --headless`）再问一次。"
        },
    }))
}

/* ── 小工具 ─────────────────────────────────────── */

/// 能路由的健康态。`cooling` 不放行（正在冷却），`retired` 已移出路由。
fn usable(health: &str) -> bool {
    matches!(health, "healthy" | "degraded" | "unknown" | "probing")
}

/// 能力判定沿用核心的三态语义：`Some(false)` 明确不支持，`None` 是未知——
/// 未知不能算支持，否则 agent 会拿一个不支持 tools 的模型去跑工具调用，
/// 然后在「模型不听话」上浪费半天。
fn supports(caps: Option<&ModelCaps>, want: &str) -> bool {
    let Some(c) = caps else { return false };
    match want {
        "tools" => c.tools == Some(true),
        "vision" => c.vision == Some(true),
        "json" => c.json_mode == Some(true),
        "reasoning" => c.reasoning == Some(true),
        _ => false,
    }
}

fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

fn iso(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_default()
}

fn arg_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn arg_bool(args: &Value, key: &str, default: bool) -> bool {
    args.get(key).and_then(Value::as_bool).unwrap_or(default)
}

fn arg_usize(args: &Value, key: &str, default: usize, max: usize) -> usize {
    args.get(key)
        .and_then(Value::as_u64)
        .map(|v| (v as usize).clamp(1, max))
        .unwrap_or(default)
}
