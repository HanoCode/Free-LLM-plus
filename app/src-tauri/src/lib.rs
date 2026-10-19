//! Freeport 的桌面宿主。
//!
//! 同一个可执行文件有两种形态：
//! - 默认：Tauri 窗口 + 进程内网关（关窗收托盘，网关继续服务）
//! - `--headless`：无窗口，只跑网关，交给 launchd / systemd / 计划任务托管
//!
//! 网关本体在 `freeport-gateway`，这里只做宿主该做的事。

use std::sync::Arc;

use freeport_core::dto::{
    AddProviderResult, ConfigPatch, GwModel, GwProvider, GwRouting, GwStatus, ProbeResult,
    ProviderInput, RoutingPatch,
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

/* ── 网关控制面 ─────────────────────────────────── */

#[tauri::command]
async fn gw_status(state: tauri::State<'_, AppState>) -> Result<GwStatus, String> {
    Ok(state.gw.status().await)
}

#[tauri::command]
async fn gw_logs(state: tauri::State<'_, AppState>) -> Result<Vec<LogEntry>, String> {
    Ok(state.gw.logs().await)
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

#[tauri::command]
fn gw_reveal_key(state: tauri::State<'_, AppState>, id: String) -> String {
    state.gw.reveal_key(&id)
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

/* ── 入口 ───────────────────────────────────────── */

pub fn run() {
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
            gw_logs,
            gw_providers,
            gw_models,
            gw_routing,
            gw_presets,
            gw_add_provider,
            gw_remove_provider,
            gw_toggle_provider,
            gw_probe,
            gw_reveal_key,
            gw_set_routing,
            gw_clear_cooldowns,
            gw_set_config,
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

