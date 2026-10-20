//! Freeport 的桌面宿主。
//!
//! 同一个可执行文件有两种形态：
//! - 默认：Tauri 窗口 + 进程内网关（关窗收托盘，网关继续服务）
//! - `--headless`：无窗口，只跑网关，交给 launchd / systemd / 计划任务托管
//!
//! 网关本体在 `freeport-gateway`，这里只做宿主该做的事。

use std::sync::Arc;

use freeport_core::dto::{
    AccountInput, AccountPatch, AddProviderResult, ConfigPatch, FeedPatch, GwCatalog, GwFeed, GwKey,
    GwLicense, GwLoginStart, GwLoginState,
    GwModel, GwModelProbe, GwPatrol, GwProvider, GwQuota, GwRouting, GwStatus, KeyCreated, KeyInput,
    PatrolPatch, PatrolReport, ProbeResult, ProviderInput, RoutingPatch,
};
use freeport_core::model::LogEntry;
use freeport_core::presets::ProviderPreset;
use freeport_gateway::Gateway;
use tauri::{Emitter, Manager, WebviewWindow};

pub struct AppState {
    gw: Arc<Gateway>,
}

/* ── 应用信息与窗口 ─────────────────────────────── */

#[tauri::command]
fn app_info() -> serde_json::Value {
    serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "platform": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "runtime": "tauri",
    })
}

#[tauri::command]
fn win_minimize(win: WebviewWindow) -> Result<(), String> {
    win.minimize().map_err(|e| e.to_string())
}

#[tauri::command]
fn win_toggle_maximize(win: WebviewWindow) -> Result<bool, String> {
    let max = win.is_maximized().map_err(|e| e.to_string())?;
    if max {
        win.unmaximize().map_err(|e| e.to_string())?;
    } else {
        win.maximize().map_err(|e| e.to_string())?;
    }
    Ok(!max)
}

/// 关窗=收进托盘。真正退出走托盘菜单。
#[tauri::command]
fn win_close(win: WebviewWindow) -> Result<(), String> {
    win.hide().map_err(|e| e.to_string())
}

#[tauri::command]
fn win_toggle_devtools(win: WebviewWindow) {
    if win.is_devtools_open() {
        win.close_devtools();
    } else {
        win.open_devtools();
    }
}

#[tauri::command]
fn open_external(app: tauri::AppHandle, url: String) -> Result<bool, String> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("只允许打开 http(s) 链接".into());
    }
    app.opener()
        .open_url(url, None::<&str>)
        .map(|_| true)
        .map_err(|e| e.to_string())
}

use tauri_plugin_opener::OpenerExt;

/* ── 崩溃与诊断 ───────────────────────────────────
   崩溃必须留下证据。窗口进程一挂，用户只会说「打不开」；没有落盘记录，
   事后一句都查不了。这里只做两件事：
   1. panic hook 把现场写到 `<data>/crash/`（纯本地，不上传）；
   2. `gw_diagnostics` 汇总一份脱敏快照，由用户自己决定要不要贴出去。 */

fn crash_dir() -> std::path::PathBuf {
    freeport_gateway::store::data_dir().join("crash")
}

fn install_panic_hook() {
    let dir = crash_dir();
    std::panic::set_hook(Box::new(move |info| {
        let ts = chrono::Local::now();
        let body = format!(
            "Freeport {} — panic\n\
             time:   {}\n\
             os:     {} {}\n\
             thread: {}\n\
             panic:  {}\n\
             \n--- backtrace ---\n{}\n",
            env!("CARGO_PKG_VERSION"),
            ts.to_rfc3339(),
            std::env::consts::OS,
            std::env::consts::ARCH,
            std::thread::current().name().unwrap_or("<unnamed>"),
            info,
            std::backtrace::Backtrace::force_capture(),
        );
        // 磁盘满 / 权限不足时不能再崩一次：写不进去就退回 stderr
        if std::fs::create_dir_all(&dir).is_err()
            || std::fs::write(dir.join(format!("crash-{}.log", ts.format("%Y%m%d-%H%M%S"))), &body)
                .is_err()
        {
            eprintln!("{body}");
            return;
        }
        prune_crashes(&dir);
        eprintln!("{body}");
    }));
}

/// 只留最近 20 份。崩溃日志会带上路径与配置片段，不该无限堆在用户盘上。
fn prune_crashes(dir: &std::path::Path) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<std::path::PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "log"))
        .collect();
    if files.len() <= 20 {
        return;
    }
    files.sort();
    let extra = files.len() - 20;
    for p in files.into_iter().take(extra) {
        let _ = std::fs::remove_file(p);
    }
}

fn crash_files() -> Vec<serde_json::Value> {
    let dir = crash_dir();
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<serde_json::Value> = rd
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "log"))
        .map(|e| {
            let p = e.path();
            let size = e.metadata().map(|m| m.len()).unwrap_or(0);
            serde_json::json!({
                "file": p.file_name().and_then(|n| n.to_str()).unwrap_or(""),
                "bytes": size,
            })
        })
        .collect();
    // 文件名带时间戳，倒序即最新在前
    out.sort_by(|a, b| b["file"].as_str().cmp(&a["file"].as_str()));
    out
}

/// 脱敏诊断快照。
///
/// 三条硬规矩：不带明文密钥、不带掩码（后四位也是信息）、不带网关 authToken。
/// 除此之外全给全——用户来报障时，最怕的是「贴上来还看不出问题」。
#[tauri::command]
async fn gw_diagnostics(state: tauri::State<'_, AppState>) -> Result<serde_json::Value, String> {
    let status = state.gw.status().await;
    let providers = state.gw.providers().await;
    let models = state.gw.models().await;
    let patrol = state.gw.patrol().await;
    let feed = state.gw.feed().await;
    let license = state.gw.license().await;
    let logs = state.gw.logs().await;
    let crashes = crash_files();

    let prov: Vec<serde_json::Value> = providers
        .iter()
        .map(|p| {
            serde_json::json!({
                "id": p.id,
                "name": p.name,
                "tier": p.tier,
                "baseUrl": p.base_url,
                "enabled": p.enabled,
                "accounts": p.accounts.len(),
                "accountsOk": p.accounts.iter().filter(|a| a.has_key).count(),
                "models": p.models,
                "health": p.health.status,
                "latencyMs": p.health.latency_ms,
                "error": p.health.error,
            })
        })
        .collect();

    let recent: Vec<serde_json::Value> = logs
        .iter()
        .take(20)
        .map(|l| {
            serde_json::json!({
                "at": l.at,
                "path": l.path,
                "requested": l.requested_model,
                "model": l.model,
                "provider": l.provider,
                "status": l.status,
                "ok": l.ok,
                "ms": l.ms,
                "usage": l.usage,
                "attempts": l.attempts.iter().map(|a| serde_json::json!({
                    "provider": a.provider,
                    "model": a.model,
                    "status": a.status,
                    "kind": a.kind,
                    "ms": a.ms,
                })).collect::<Vec<_>>(),
            })
        })
        .collect();

    Ok(serde_json::json!({
        "app": {
            "version": env!("CARGO_PKG_VERSION"),
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "dataDir": freeport_gateway::store::data_dir().display().to_string(),
        },
        "gateway": {
            "running": status.running,
            "bind": status.bind,
            "port": status.port,
            "requireAuth": status.require_auth,
            "requests24h": status.requests24h,
            "tokensIn": status.tokens_in,
            "tokensOut": status.tokens_out,
            "errors": status.errors,
            "cooldowns": status.cooldowns.len(),
            "error": status.error,
        },
        "providers": prov,
        "modelCount": models.len(),
        "patrol": {
            "enabled": patrol.enabled,
            "budgetToday": patrol.budget_today,
            "usedToday": patrol.used_today,
            "dueNow": patrol.due_now,
            "probesTotal": patrol.probes_total,
            "probesOk": patrol.probes_ok,
            "lastRunAt": patrol.last_run_at,
        },
        "feed": {
            "enabled": feed.enabled,
            "channel": feed.channel,
            "policy": feed.policy,
            "seq": feed.seq,
            "version": feed.version,
        },
        "license": {
            "status": license.status,
            "tier": license.tier,
        },
        "recentLogs": recent,
        "crashes": crashes,
        "generatedAt": chrono::Local::now().to_rfc3339(),
    }))
}

/// 把诊断快照存成文件。`None` = 用户取消（取消不是错误）。
#[tauri::command]
async fn gw_save_diagnostics(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;

    let diag = gw_diagnostics(state).await?;
    let name = format!(
        "freeport-diagnostics-{}.json",
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    );
    let picked = app
        .dialog()
        .file()
        .set_title("保存诊断信息")
        .set_file_name(&name)
        .add_filter("JSON", &["json"])
        .blocking_save_file();
    let Some(fp) = picked else { return Ok(None) };
    let path = fp
        .into_path()
        .map_err(|_| "选择的位置不是本地文件路径".to_string())?;
    let body = serde_json::to_string_pretty(&diag).map_err(|e| e.to_string())?;
    std::fs::write(&path, body).map_err(|e| e.to_string())?;
    Ok(Some(path.display().to_string()))
}

/// 检查更新：读 GitHub Releases 的 latest，与本地版本比。
///
/// 为什么不用 `tauri-plugin-updater`：自动更新要求发布端持有 minisign 私钥，
/// 客户端只验签。私钥没就位之前，装一个「点了没反应」的假按钮比没有更糟。
/// 所以 v1 先做真检查 + 跳转下载；密钥就位后把这里换成插件即可，前端不用改。
#[tauri::command]
async fn check_update() -> Result<serde_json::Value, String> {
    const API: &str = "https://api.github.com/repos/HanoCode/Free-LLM-plus/releases/latest";
    let current = env!("CARGO_PKG_VERSION");

    let client = reqwest::Client::builder()
        .user_agent(format!("Freeport/{current}"))
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| format!("初始化网络失败：{e}"))?;

    let resp = client
        .get(API)
        .header("accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| format!("连不上更新服务器：{e}"))?;

    // 404 = 还没发过 release，这不是错误，是「没有新版本」
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(serde_json::json!({
            "current": current,
            "latest": current,
            "updateAvailable": false,
            "url": null,
        }));
    }
    if !resp.status().is_success() {
        return Err(format!("更新服务器返回 {}", resp.status()));
    }

    let body: serde_json::Value = resp.json().await.map_err(|e| format!("解析失败：{e}"))?;
    let tag = body["tag_name"].as_str().unwrap_or_default().to_string();
    let latest = tag.trim_start_matches('v').to_string();

    Ok(serde_json::json!({
        "current": current,
        "latest": if latest.is_empty() { current } else { &latest },
        "updateAvailable": !latest.is_empty() && version_gt(&latest, current),
        "url": body["html_url"].as_str(),
        "notes": body["body"].as_str(),
        "publishedAt": body["published_at"].as_str(),
    }))
}

/// 只比较数字段。预发布后缀（-beta.1）一律按相等处理：宁可漏一次 beta 提醒，
/// 也不要因为字符串比较把 0.1.0 判成比 0.1.0-beta 旧而反复弹窗。
fn version_gt(a: &str, b: &str) -> bool {
    let pa: Vec<u32> = a.split('.').map(|s| s.parse().unwrap_or(0)).collect();
    let pb: Vec<u32> = b.split('.').map(|s| s.parse().unwrap_or(0)).collect();
    for i in 0..pa.len().max(pb.len()) {
        let x = pa.get(i).copied().unwrap_or(0);
        let y = pb.get(i).copied().unwrap_or(0);
        if x != y {
            return x > y;
        }
    }
    false
}

/* ── 网关控制面 ─────────────────────────────────── */

#[tauri::command]
async fn gw_status(state: tauri::State<'_, AppState>) -> Result<GwStatus, String> {
    Ok(state.gw.status().await)
}

#[tauri::command]
async fn gw_logs(state: tauri::State<'_, AppState>) -> Result<Vec<LogEntry>, String> {
    Ok(state.gw.logs().await)
}

/// 暂停业务转发。不发日志、不动配置：这是运行时开关。
#[tauri::command]
async fn gw_pause(state: tauri::State<'_, AppState>, paused: bool) -> Result<GwStatus, String> {
    state.gw.set_paused(paused);
    Ok(state.gw.status().await)
}

#[tauri::command]
async fn gw_providers(state: tauri::State<'_, AppState>) -> Result<Vec<GwProvider>, String> {
    Ok(state.gw.providers().await)
}

#[tauri::command]
async fn gw_models(state: tauri::State<'_, AppState>) -> Result<Vec<GwModel>, String> {
    Ok(state.gw.models().await)
}

#[tauri::command]
async fn gw_routing(state: tauri::State<'_, AppState>) -> Result<GwRouting, String> {
    Ok(state.gw.routing().await)
}

#[tauri::command]
fn gw_presets(state: tauri::State<'_, AppState>) -> Vec<ProviderPreset> {
    state.gw.presets()
}

#[tauri::command]
async fn gw_add_provider(
    state: tauri::State<'_, AppState>,
    input: ProviderInput,
) -> Result<AddProviderResult, String> {
    state.gw.add_provider(input).await
}

#[tauri::command]
async fn gw_remove_provider(
    state: tauri::State<'_, AppState>,
    id: String,
) -> Result<Vec<GwProvider>, String> {
    Ok(state.gw.remove_provider(&id).await)
}

#[tauri::command]
async fn gw_toggle_provider(
    state: tauri::State<'_, AppState>,
    id: String,
    enabled: bool,
) -> Result<Vec<GwProvider>, String> {
    state.gw.toggle_provider(&id, enabled).await
}

#[tauri::command]
async fn gw_probe(state: tauri::State<'_, AppState>, id: String) -> Result<ProbeResult, String> {
    Ok(state.gw.probe(&id).await)
}

/// 用指定的一条凭据校验。`id` 是厂商 id，`accountId` 是凭据 id。
#[tauri::command]
async fn gw_probe_account(
    state: tauri::State<'_, AppState>,
    id: String,
    account_id: String,
) -> Result<ProbeResult, String> {
    Ok(state.gw.probe_account(&id, &account_id).await)
}

/// 明文 key 只给一条凭据看一次。多账号之后 `id` 是凭据 id，不是厂商 id。
#[tauri::command]
fn gw_reveal_key(state: tauri::State<'_, AppState>, id: String) -> String {
    state.gw.reveal_key(&id)
}

/* ── 凭据（同一厂商多账号） ───────────────────────
   额度、冷却、账本、粘性全部按凭据算，所以增删改都挂在（厂商, 凭据）这一对上。 */

#[tauri::command]
async fn gw_add_account(
    state: tauri::State<'_, AppState>,
    input: AccountInput,
) -> Result<Vec<GwProvider>, String> {
    state.gw.add_account(input).await
}

/* ── 设备码登录（订阅类厂商） ─────────────────────
   用户在自己的浏览器里完成授权，宿主只负责发起与轮询。 */

#[tauri::command]
async fn gw_login_start(
    state: tauri::State<'_, AppState>,
    provider_id: String,
) -> Result<GwLoginStart, String> {
    state.gw.login_start(&provider_id).await
}

/// 用粘贴进来的 web 会话新建一条凭据（只给没有公开 API 的站点）。
#[tauri::command]
async fn gw_add_cookie_account(
    state: tauri::State<'_, AppState>,
    provider_id: String,
    name: Option<String>,
    cookie: String,
) -> Result<Vec<GwProvider>, String> {
    state
        .gw
        .add_cookie_account(&provider_id, name, &cookie)
        .await
}

/// 会话过期后重新粘贴。
#[tauri::command]
async fn gw_set_account_cookie(
    state: tauri::State<'_, AppState>,
    provider_id: String,
    account_id: String,
    cookie: String,
) -> Result<Vec<GwProvider>, String> {
    state
        .gw
        .set_account_cookie(&provider_id, &account_id, &cookie)
        .await
}

#[tauri::command]
async fn gw_login_poll(
    state: tauri::State<'_, AppState>,
    login_id: String,
) -> Result<GwLoginState, String> {
    Ok(state.gw.login_poll(&login_id).await)
}

#[tauri::command]
async fn gw_remove_account(
    state: tauri::State<'_, AppState>,
    provider_id: String,
    account_id: String,
) -> Result<Vec<GwProvider>, String> {
    Ok(state.gw.remove_account(&provider_id, &account_id).await)
}

#[tauri::command]
async fn gw_patch_account(
    state: tauri::State<'_, AppState>,
    provider_id: String,
    account_id: String,
    patch: AccountPatch,
) -> Result<Vec<GwProvider>, String> {
    state
        .gw
        .patch_account(&provider_id, &account_id, patch)
        .await
}

/* ── 迁移（多机同步 v1） ─────────────────────────
   迁移码是口令加密的信封，里面含明文密钥。口令只在内存里过一遍，不落盘。 */

#[tauri::command]
async fn gw_export_bundle(
    state: tauri::State<'_, AppState>,
    passphrase: String,
) -> Result<String, String> {
    state.gw.export_bundle(&passphrase).await
}

#[tauri::command]
async fn gw_import_bundle(
    state: tauri::State<'_, AppState>,
    blob: String,
    passphrase: String,
    mode: Option<String>,
) -> Result<freeport_gateway::transfer::Summary, String> {
    state
        .gw
        .import_bundle(&blob, &passphrase, mode.as_deref().unwrap_or("merge"))
        .await
}

/// 导出成 `.fpk` 文件。
///
/// 对话框走 Rust 侧：这样不需要给前端开 `dialog:` 之外的文件系统权限，
/// 也不需要把路径在 IPC 上来回搬。返回 `None` 表示用户取消——取消不是错误，
/// 不该弹红字。
#[tauri::command]
async fn gw_export_bundle_file(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    passphrase: String,
) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;

    // 口令先校验，再弹框：让用户挑完位置才被告知「口令太短」是很差的体验
    if passphrase.chars().count() < freeport_gateway::transfer::MIN_PASSPHRASE {
        return Err(format!(
            "口令至少 {} 位——迁移包里是你全部的 API key",
            freeport_gateway::transfer::MIN_PASSPHRASE
        ));
    }

    let name = format!(
        "freeport-{}.{}",
        chrono::Local::now().format("%Y%m%d"),
        freeport_gateway::transfer::FILE_EXT
    );
    // 这里在 async 命令线程上（不是主线程），所以可以用阻塞版；
    // 主线程调用会与事件循环死锁。
    let picked = app
        .dialog()
        .file()
        .set_title("保存 Freeport 迁移包")
        .set_file_name(&name)
        .add_filter("Freeport 迁移包", &[freeport_gateway::transfer::FILE_EXT])
        .blocking_save_file();
    let Some(fp) = picked else { return Ok(None) };
    let path = fp
        .into_path()
        .map_err(|_| "选择的位置不是本地文件路径".to_string())?;

    state
        .gw
        .export_bundle_to(path, &passphrase)
        .await
        .map(Some)
}

/// 从 `.fpk` 文件导入。`None` = 用户取消。
#[tauri::command]
async fn gw_import_bundle_file(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    passphrase: String,
    mode: Option<String>,
) -> Result<Option<freeport_gateway::transfer::Summary>, String> {
    use tauri_plugin_dialog::DialogExt;

    let picked = app
        .dialog()
        .file()
        .set_title("选择 Freeport 迁移包")
        .add_filter("Freeport 迁移包", &[freeport_gateway::transfer::FILE_EXT])
        .blocking_pick_file();
    let Some(fp) = picked else { return Ok(None) };
    let path = fp
        .into_path()
        .map_err(|_| "选择的位置不是本地文件路径".to_string())?;

    state
        .gw
        .import_bundle_from(path, &passphrase, mode.as_deref().unwrap_or("merge"))
        .await
        .map(Some)
}

/* ── 许可证（Pro） ───────────────────────────────
   离线验签，不联网。失效只降级，不影响转发。 */

#[tauri::command]
async fn gw_license(state: tauri::State<'_, AppState>) -> Result<GwLicense, String> {
    Ok(state.gw.license().await)
}

#[tauri::command]
async fn gw_activate_license(
    state: tauri::State<'_, AppState>,
    token: String,
) -> Result<GwLicense, String> {
    state.gw.activate_license(&token).await
}

#[tauri::command]
async fn gw_clear_license(state: tauri::State<'_, AppState>) -> Result<GwLicense, String> {
    Ok(state.gw.clear_license().await)
}

#[tauri::command]
async fn gw_set_routing(
    state: tauri::State<'_, AppState>,
    patch: RoutingPatch,
) -> Result<GwRouting, String> {
    Ok(state.gw.set_routing(patch).await)
}

#[tauri::command]
fn gw_clear_cooldowns(state: tauri::State<'_, AppState>) -> bool {
    state.gw.clear_cooldowns();
    true
}

#[tauri::command]
async fn gw_set_config(
    state: tauri::State<'_, AppState>,
    patch: ConfigPatch,
) -> Result<GwStatus, String> {
    Ok(state.gw.set_config(patch).await)
}

/* ── 目录 ───────────────────────────────────────── */

#[tauri::command]
async fn gw_catalog(state: tauri::State<'_, AppState>) -> Result<GwCatalog, String> {
    Ok(state.gw.catalog().await)
}

/// 路由预演。页面刷新链路不该推动真实的加权轮转。
#[tauri::command]
async fn gw_route(
    state: tauri::State<'_, AppState>,
    model: Option<String>,
    session: Option<String>,
) -> Result<serde_json::Value, String> {
    Ok(state
        .gw
        .explain_route(
            &model.unwrap_or_else(|| "auto".into()),
            &session.unwrap_or_default(),
        )
        .await)
}

#[tauri::command]
async fn gw_catalog_refresh(state: tauri::State<'_, AppState>) -> Result<GwCatalog, String> {
    state.gw.refresh_catalog().await
}

/* ── 签名目录 feed ─────────────────────────────── */

#[tauri::command]
async fn gw_feed(state: tauri::State<'_, AppState>) -> Result<GwFeed, String> {
    Ok(state.gw.feed().await)
}

#[tauri::command]
async fn gw_set_feed(state: tauri::State<'_, AppState>, patch: FeedPatch) -> Result<GwFeed, String> {
    Ok(state.gw.set_feed(patch).await)
}

#[tauri::command]
async fn gw_feed_check(
    state: tauri::State<'_, AppState>,
    manual: Option<bool>,
) -> Result<GwFeed, String> {
    Ok(state.gw.feed_check(manual.unwrap_or(true)).await)
}

#[tauri::command]
async fn gw_feed_apply(state: tauri::State<'_, AppState>) -> Result<GwFeed, String> {
    Ok(state.gw.feed_apply().await)
}

/* ── 探针与巡检 ─────────────────────────────────── */

#[tauri::command]
async fn gw_probe_model(
    state: tauri::State<'_, AppState>,
    provider: String,
    model: String,
    deep: Option<bool>,
) -> Result<GwModelProbe, String> {
    Ok(state.gw.probe_model(&provider, &model, deep.unwrap_or(false)).await)
}

#[tauri::command]
async fn gw_probe_failed(state: tauri::State<'_, AppState>) -> Result<PatrolReport, String> {
    Ok(state.gw.probe_failed().await)
}

#[tauri::command]
async fn gw_patrol(state: tauri::State<'_, AppState>) -> Result<GwPatrol, String> {
    Ok(state.gw.patrol().await)
}

#[tauri::command]
async fn gw_patrol_pass(
    state: tauri::State<'_, AppState>,
    all: Option<bool>,
) -> Result<PatrolReport, String> {
    Ok(state.gw.patrol_pass(all.unwrap_or(false)).await)
}

#[tauri::command]
async fn gw_set_patrol(
    state: tauri::State<'_, AppState>,
    patch: PatrolPatch,
) -> Result<GwPatrol, String> {
    Ok(state.gw.set_patrol(patch).await)
}

#[tauri::command]
async fn gw_quota(state: tauri::State<'_, AppState>) -> Result<Vec<GwQuota>, String> {
    Ok(state.gw.quota().await)
}

#[tauri::command]
async fn gw_keys(state: tauri::State<'_, AppState>) -> Result<Vec<GwKey>, String> {
    Ok(state.gw.keys().await)
}

#[tauri::command]
async fn gw_create_key(
    state: tauri::State<'_, AppState>,
    input: KeyInput,
) -> Result<KeyCreated, String> {
    state.gw.create_key(input).await
}

#[tauri::command]
async fn gw_delete_key(
    state: tauri::State<'_, AppState>,
    id: String,
) -> Result<Vec<GwKey>, String> {
    Ok(state.gw.delete_key(&id).await)
}

#[tauri::command]
async fn gw_toggle_key(
    state: tauri::State<'_, AppState>,
    id: String,
    enabled: bool,
) -> Result<Vec<GwKey>, String> {
    state.gw.toggle_key(&id, enabled).await
}

/* ── Agent 注入 ─────────────────────────────────── */

/// 网关此刻真能说出口的协议。没有的协议绝不让界面显示「已注入」。
const SUPPORTED: &[&str] = &["openai", "anthropic", "gemini"];

fn agent_key_name(id: &str) -> String {
    format!("agent:{id}")
}

/// 网关根地址；没起网关就没法给定一个「能用」的地址。
async fn endpoint(state: &tauri::State<'_, AppState>) -> Result<String, String> {
    let st = state.gw.status().await;
    let port = st
        .port
        .ok_or_else(|| "网关还没启动，先启动再注入（否则写进去的地址连不上）".to_string())?;
    Ok(format!("http://127.0.0.1:{port}"))
}

/// 预览用：网关没起也给一个默认地址。
///
/// 预览是只读的，没必要因为它拿不到真实端口就整页报错——用户常常
/// 先看 diff 再决定要不要开网关。
async fn endpoint_for_preview(state: &tauri::State<'_, AppState>) -> String {
    let st = state.gw.status().await;
    format!("http://127.0.0.1:{}", st.port.unwrap_or(8787))
}

#[tauri::command]
async fn gw_agents() -> Result<Vec<freeport_inject::AgentInfo>, String> {
    Ok(freeport_inject::list(SUPPORTED))
}

/// 把网关此刻能说出口的模型摊成注入器认识的卡片。
///
/// 不直接复用 `GwModel`：注入器只关心客户端要展示、要判断的那几项，
/// `/v1/models` 加字段不该逼着改注入逻辑。
async fn model_cards(state: &tauri::State<'_, AppState>) -> Vec<freeport_inject::ModelCard> {
    state
        .gw
        .models()
        .await
        .into_iter()
        .map(|m| {
            // 实测 > 目录声明，网关已经合并好放在 freeport.caps 里
            let caps = m.freeport.as_ref().and_then(|f| f.caps).unwrap_or_default();
            freeport_inject::ModelCard {
                id: m.id,
                label: String::new(),
                context: m.context_length.unwrap_or(128_000).min(u32::MAX as u64) as u32,
                max_output: 8192,
                // 「目录没说」不等于「不支持」：工具调用放行，视觉只在明确支持时开
                tools: caps.tools != Some(false),
                vision: caps.vision == Some(true),
                reasoning: caps.reasoning == Some(true),
            }
        })
        .collect()
}

/// 预览只读不写。key 用现有 key 的前缀占位，界面上看到的就是真实前缀。
#[tauri::command]
async fn gw_agent_preview(
    state: tauri::State<'_, AppState>,
    id: String,
) -> Result<Vec<freeport_inject::DiffLine>, String> {
    let base = endpoint_for_preview(&state).await;
    let prefix = state
        .gw
        .keys()
        .await
        .into_iter()
        .find(|k| k.name == agent_key_name(&id))
        .map(|k| k.prefix)
        .unwrap_or_else(|| "fp-preview".into());
    let placeholder = format!("{prefix}{}", "0".repeat(32usize.saturating_sub(prefix.len())));
    let ctx = freeport_inject::Ctx::new(&base, &placeholder, "auto")
        .with_models(model_cards(&state).await);
    freeport_inject::preview(&id, &ctx)
}

/// 注入：每个 Agent 单独签发一把 key，方便按 Agent 归因和吊销。
#[tauri::command]
async fn gw_agent_apply(
    state: tauri::State<'_, AppState>,
    id: String,
) -> Result<freeport_inject::AgentInfo, String> {
    let base = endpoint(&state).await?;

    // 轮换：老 key 先吊销，避免同一 Agent 攒一把历史 key
    let name = agent_key_name(&id);
    if let Some(old) = state.gw.keys().await.into_iter().find(|k| k.name == name) {
        state.gw.delete_key(&old.id).await;
    }
    let created = state
        .gw
        .create_key(KeyInput {
            name: name.clone(),
            rpm_limit: None,
            daily_limit: None,
        })
        .await?;

    let ctx = freeport_inject::Ctx::new(&base, &created.secret, "auto")
        .with_models(model_cards(&state).await);
    freeport_inject::apply(&id, &ctx, Some(created.key.id.clone()))
}

#[tauri::command]
async fn gw_agent_revert(
    state: tauri::State<'_, AppState>,
    id: String,
) -> Result<freeport_inject::AgentInfo, String> {
    let out = freeport_inject::revert(&id)?;
    // 回滚后那把 key 就没有使用者了，一并吊销
    if let Some(k) = state
        .gw
        .keys()
        .await
        .into_iter()
        .find(|k| k.name == agent_key_name(&id))
    {
        state.gw.delete_key(&k.id).await;
    }
    Ok(out)
}

/* ── 项目级注入 ─────────────────────────────────── */

/// 项目级用**同一把** `agent:<id>` key：同一个 Agent 在用户级与某个仓库里
/// 指向的是同一个网关，按仓库再签一把只会攒出一堆重复凭据。
/// 所以这里不复用 `gw_agent_apply` 的「先吊销旧的再新建」——那会把用户级
/// 正在用的那把一起撤掉。
async fn ensure_agent_key(
    state: &tauri::State<'_, AppState>,
    id: &str,
) -> Result<(String, Option<String>), String> {
    let name = agent_key_name(id);
    if let Some(k) = state.gw.keys().await.into_iter().find(|k| k.name == name) {
        let secret = state.gw.reveal_key(&k.id);
        if !secret.is_empty() {
            return Ok((secret, None));
        }
        // 只剩掩码（密钥库被清过）：删掉重建，否则注入的是一个不存在的 key
        state.gw.delete_key(&k.id).await;
    }
    let created = state
        .gw
        .create_key(KeyInput {
            name,
            rpm_limit: None,
            daily_limit: None,
        })
        .await?;
    Ok((created.secret, Some(created.key.id)))
}

fn project_root(root: &str) -> Result<std::path::PathBuf, String> {
    let p = std::path::PathBuf::from(root.trim());
    if p.as_os_str().is_empty() {
        return Err("先填项目目录".into());
    }
    if !p.is_dir() {
        return Err(format!("项目目录不存在：{}", p.display()));
    }
    Ok(p)
}

#[tauri::command]
async fn gw_agent_project_list(root: String) -> Result<Vec<freeport_inject::ProjectInjection>, String> {
    Ok(freeport_inject::project_list(&project_root(&root)?))
}

#[tauri::command]
async fn gw_agent_project_preview(
    state: tauri::State<'_, AppState>,
    id: String,
    root: String,
) -> Result<Vec<freeport_inject::DiffLine>, String> {
    let base = endpoint_for_preview(&state).await;
    let prefix = state
        .gw
        .keys()
        .await
        .into_iter()
        .find(|k| k.name == agent_key_name(&id))
        .map(|k| k.prefix)
        .unwrap_or_else(|| "fp-preview".into());
    let placeholder = format!("{prefix}{}", "0".repeat(32usize.saturating_sub(prefix.len())));
    let ctx = freeport_inject::Ctx::new(&base, &placeholder, "auto")
        .with_models(model_cards(&state).await);
    freeport_inject::preview_scoped(
        &id,
        freeport_inject::Scope::Project(&project_root(&root)?),
        &ctx,
    )
}

#[tauri::command]
async fn gw_agent_project_apply(
    state: tauri::State<'_, AppState>,
    id: String,
    root: String,
) -> Result<freeport_inject::AgentInfo, String> {
    let dir = project_root(&root)?;
    let base = endpoint(&state).await?;
    let (secret, _created) = ensure_agent_key(&state, &id).await?;
    let ctx = freeport_inject::Ctx::new(&base, &secret, "auto").with_models(model_cards(&state).await);
    let info = freeport_inject::apply_scoped(
        &id,
        freeport_inject::Scope::Project(&dir),
        &ctx,
        Some(agent_key_name(&id)),
    )?;
    Ok(info)
}

#[tauri::command]
async fn gw_agent_project_revert(
    state: tauri::State<'_, AppState>,
    id: String,
    root: String,
) -> Result<freeport_inject::AgentInfo, String> {
    let dir = project_root(&root)?;
    let info = freeport_inject::revert_scoped(&id, freeport_inject::Scope::Project(&dir))?;
    // 用户级还在用这把 key 时不能吊销：撤仓库里的配置不该把全局那条路也断了
    let still_in_use = freeport_inject::current(&id, SUPPORTED)
        .map(|i| i.state == "injected")
        .unwrap_or(false);
    if !still_in_use {
        if let Some(k) = state
            .gw
            .keys()
            .await
            .into_iter()
            .find(|k| k.name == agent_key_name(&id))
        {
            state.gw.delete_key(&k.id).await;
        }
    }
    Ok(info)
}

/* ── 入口 ───────────────────────────────────────── */

pub fn run() {
    install_panic_hook();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--headless" || a == "serve") {
        run_headless();
        return;
    }

    let gw = Arc::new(Gateway::new());

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState { gw: gw.clone() })
        .setup(move |app| {
            // 网关与窗口同进程：先起网关，窗口才能读到真实状态
            let gw_setup = gw.clone();
            let handle = app.handle().clone();
            tauri::async_runtime::block_on(async {
                gw_setup.load().await;
                gw_setup
                    .set_log_sink(Arc::new(move |entry: LogEntry| {
                        let _ = handle.emit("gateway:log", &entry);
                    }))
                    .await;
                match gw_setup.start().await {
                    Ok(port) => tracing::info!("gateway ready on 127.0.0.1:{port}"),
                    Err(e) => tracing::error!("gateway failed to start: {e}"),
                }
            });

            build_tray(app.handle())?;

            if let Some(win) = app.get_webview_window("main") {
                let _ = win.show();
                let _ = win.set_focus();
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                // 关窗不退出：网关要继续服务
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            app_info,
            win_minimize,
            win_toggle_maximize,
            win_close,
            win_toggle_devtools,
            open_external,
            gw_status,
            gw_pause,
            gw_diagnostics,
            gw_save_diagnostics,
            check_update,
            gw_logs,
            gw_providers,
            gw_models,
            gw_routing,
            gw_presets,
            gw_add_provider,
            gw_remove_provider,
            gw_toggle_provider,
            gw_probe,
            gw_probe_account,
            gw_reveal_key,
            gw_add_account,
            gw_remove_account,
            gw_patch_account,
            gw_export_bundle,
            gw_import_bundle,
            gw_export_bundle_file,
            gw_import_bundle_file,
            gw_license,
            gw_activate_license,
            gw_clear_license,
            gw_login_start,
            gw_login_poll,
            gw_add_cookie_account,
            gw_set_account_cookie,
            gw_set_routing,
            gw_clear_cooldowns,
            gw_set_config,
            gw_catalog,
            gw_catalog_refresh,
            gw_feed,
            gw_set_feed,
            gw_feed_check,
            gw_feed_apply,
            gw_route,
            gw_probe_model,
            gw_probe_failed,
            gw_patrol,
            gw_patrol_pass,
            gw_set_patrol,
            gw_quota,
            gw_keys,
            gw_create_key,
            gw_delete_key,
            gw_toggle_key,
            gw_agents,
            gw_agent_preview,
            gw_agent_apply,
            gw_agent_revert,
            gw_agent_project_list,
            gw_agent_project_preview,
            gw_agent_project_apply,
            gw_agent_project_revert,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Freeport");
}

fn build_tray(app: &tauri::AppHandle) -> tauri::Result<()> {
    use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
    use tauri::tray::TrayIconBuilder;

    let show = MenuItem::with_id(app, "show", "打开 Freeport", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &sep, &quit])?;

    let mut builder = TrayIconBuilder::with_id("freeport-tray")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => {
                if let Some(win) = app.get_webview_window("main") {
                    let _ = win.show();
                    let _ = win.set_focus();
                }
            }
            "quit" => {
                if let Some(state) = app.try_state::<AppState>() {
                    tauri::async_runtime::block_on(state.gw.stop());
                }
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let tauri::tray::TrayIconEvent::Click {
                button: tauri::tray::MouseButton::Left,
                button_state: tauri::tray::MouseButtonState::Up,
                ..
            } = event
            {
                let app = tray.app_handle();
                if let Some(win) = app.get_webview_window("main") {
                    if win.is_visible().unwrap_or(false) {
                        let _ = win.hide();
                    } else {
                        let _ = win.show();
                        let _ = win.set_focus();
                    }
                }
            }
        });

    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

fn run_headless() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    rt.block_on(async {
        let gw = Gateway::new();
        gw.load().await;
        match gw.start().await {
            Ok(port) => println!("FREEPORT_READY port={port}"),
            Err(e) => {
                eprintln!("freeport: {e}");
                std::process::exit(1);
            }
        }
        let _ = tokio::signal::ctrl_c().await;
        gw.stop().await;
    });
}

