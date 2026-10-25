//! 桌面宿主自己的偏好：开机自启 / 启动最小化 / 退出清理注入。
//!
//! 刻意**不塞进网关配置**：这三项是「窗口与进程怎么活」的事，
//! headless / MCP 形态根本不关心，混进 `ConfigPatch` 等于给每条入口
//! 加一堆永远用不到的字段，还会让「配置文件」和「这台机器的用法」纠缠不清。
//!
//! 落盘位置与网关配置同一个数据目录（`dirs::data_dir()/dev.freeport.app`），
//! 换机迁移不带走它——迁移码搬的是凭据，不是「这台机器要不要开机自启」。

use std::io::Write;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DesktopPrefs {
    /// 启动后只驻留托盘，不显示主窗口
    pub start_minimized: bool,
    /// 退出前回滚本机用户级注入。默认关：清理会改用户自己的客户端配置，
    /// 这种事必须是用户点名要的，不能替他决定。
    pub cleanup_on_quit: bool,
    /// 界面语言（`zh` / `en`）。前端是权威来源，这里只给托盘菜单用——
    /// 托盘在 Rust 侧，读不到 localStorage。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ui_lang: Option<String>,
}

impl Default for DesktopPrefs {
    fn default() -> Self {
        Self {
            start_minimized: false,
            cleanup_on_quit: false,
            ui_lang: None,
        }
    }
}

/// 局部更新：只改传进来的字段，其余保持原值
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrefsPatch {
    pub start_minimized: Option<bool>,
    pub cleanup_on_quit: Option<bool>,
    pub ui_lang: Option<String>,
}

fn path() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("dev.freeport.app").join("desktop.json"))
}

/// 读不到 / 解析失败一律回落默认值。
/// 这里不报错：偏好的读失败不该挡住应用启动，最坏结果就是「按默认行为跑」。
pub fn load() -> DesktopPrefs {
    let Some(p) = path() else {
        return DesktopPrefs::default();
    };
    match std::fs::read_to_string(&p) {
        Ok(s) => serde_json::from_str::<DesktopPrefs>(&s).unwrap_or_default(),
        Err(_) => DesktopPrefs::default(),
    }
}

/// 原子写：tmp + sync_all + rename。
/// 直接 `fs::write` 会被并发读看到半截文件（扩展 manifest 踩过同样的坑）。
pub fn save(p: &DesktopPrefs) -> Result<(), String> {
    let file = path().ok_or_else(|| "找不到数据目录".to_string())?;
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let tmp = file.with_extension("json.tmp");
    let body = serde_json::to_vec_pretty(p).map_err(|e| e.to_string())?;
    {
        let mut f = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
        f.write_all(&body).map_err(|e| e.to_string())?;
        f.sync_all().map_err(|e| e.to_string())?;
    }
    std::fs::rename(&tmp, &file).map_err(|e| e.to_string())
}

pub fn apply(patch: PrefsPatch) -> Result<DesktopPrefs, String> {
    let mut cur = load();
    if let Some(v) = patch.start_minimized {
        cur.start_minimized = v;
    }
    if let Some(v) = patch.cleanup_on_quit {
        cur.cleanup_on_quit = v;
    }
    if let Some(v) = patch.ui_lang {
        cur.ui_lang = Some(v);
    }
    save(&cur)?;
    Ok(cur)
}
