//! 配置持久化。
//!
//! 配置落 `freeport.json`，权限 0600；密钥由 `secrets` 模块单独保管，
//! 这里只存掩码（`keyMask`）用于展示。

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use freeport_core::model::AppData;

const FILE_NAME: &str = "freeport.json";

pub fn data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("FREEPORT_DATA_DIR") {
        return PathBuf::from(dir);
    }
    dirs::data_dir()
        .map(|d| d.join("dev.freeport.app"))
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn data_file() -> PathBuf {
    data_dir().join(FILE_NAME)
}

pub fn load() -> AppData {
    let path = data_file();
    match fs::read_to_string(&path) {
        Ok(raw) => serde_json::from_str::<AppData>(&raw).unwrap_or_else(|e| {
            tracing::warn!("freeport.json 解析失败（{}），使用默认配置", e);
            AppData::default()
        }),
        Err(_) => {
            let mut data = AppData::default();
            data.gateway.auth_token = new_token();
            data.stats.since = chrono::Utc::now().to_rfc3339();
            save(&data);
            data
        }
    }
}

pub fn save(data: &AppData) {
    let path = data_file();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(raw) = serde_json::to_string_pretty(data) {
        // 原子写：tmp + rename，权限 0600
        let tmp = path.with_extension("json.tmp");
        if let Ok(mut f) = fs::File::create(&tmp) {
            let _ = f.write_all(raw.as_bytes());
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600));
            }
            let _ = fs::rename(&tmp, &path);
        }
    }
}

pub fn new_token() -> String {
    format!("fp-{}", uuid::Uuid::new_v4().simple())
}

pub fn mask(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    if chars.len() <= 10 {
        return format!("{}••••", chars.iter().take(2).collect::<String>());
    }
    let head: String = chars.iter().take(6).collect();
    let tail: String = chars.iter().skip(chars.len() - 4).collect();
    format!("{head}••••{tail}")
}

pub fn ensure_dir(path: &Path) {
    let _ = fs::create_dir_all(path);
}
