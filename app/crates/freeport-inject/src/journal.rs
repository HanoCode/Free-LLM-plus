//! 注入日志：记录「写了哪个文件、备份在哪、原来是什么值」。
//!
//! 没有这份日志，注入就是不可逆的。所以它和配置文件同等重要：
//! 备份缺失、日志损坏时，回滚会明确拒绝，而不是猜着删。

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Prev {
    pub ptr: String,
    /// `Null` 表示写入前这里本来没有东西
    pub value: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub path: String,
    pub backup: String,
    #[serde(rename = "appliedAt")]
    pub applied_at: String,
    /// json | toml | push | env | acp | desktop
    pub mode: String,
    #[serde(default)]
    pub prev: Vec<Prev>,
    #[serde(default, rename = "keyId")]
    pub key_id: Option<String>,
    /// 多文件策略（Desktop）用这一份；单文件策略留空。
    ///
    /// 为什么单文件那套 `prev` 不够：Desktop 的配置是三个文件互相引用
    /// （profile / `_meta.json` / `deploymentMode`），少还一个就停在半开状态。
    #[serde(default)]
    pub files: Vec<FileState>,
    /// user | project。老记录没有这个字段，一律当用户级
    #[serde(default)]
    pub scope: String,
    /// 项目级时的仓库根。用户级留空。
    #[serde(default)]
    pub root: String,
    /// 写入前这个文件存在吗。
    ///
    /// 需要单独记：回滚时「把文件删掉」和「写回一个空对象」是两回事。
    /// 我们给项目级注入新建的 `.claude/settings.local.json`，撤完就该消失，
    /// 留一个 `{}` 在仓库里比不写更脏。
    #[serde(default)]
    pub created: bool,
}

/// 一个文件的写入前状态。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileState {
    pub path: String,
    /// 备份文件名。空字符串表示写入前这个文件不存在。
    #[serde(default)]
    pub backup: String,
    /// 写入前的语义快照。`None` = 当时没有这个文件（回滚就删掉它）
    #[serde(default)]
    pub before: Option<Value>,
}

pub fn data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("FREEPORT_DATA_DIR") {
        return PathBuf::from(dir);
    }
    dirs::data_dir()
        .map(|d| d.join("dev.freeport.app"))
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn backups_dir() -> PathBuf {
    data_dir().join("backups")
}

fn file() -> PathBuf {
    data_dir().join("injections.json")
}

pub fn load() -> BTreeMap<String, Entry> {
    std::fs::read_to_string(file())
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

pub fn save(map: &BTreeMap<String, Entry>) {
    let path = file();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(raw) = serde_json::to_string_pretty(map) {
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, raw).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

/// 备份到 `backups/<agent>-<ts>.bak`，返回文件名。
pub fn backup(agent: &str, src: &std::path::Path) -> Result<String, String> {
    backup_named(agent, "", src)
}

/// 带标签的备份。多文件策略同一秒里要备份好几份，光靠时间戳会撞名。
pub fn backup_named(agent: &str, tag: &str, src: &std::path::Path) -> Result<String, String> {
    let dir = backups_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("无法创建备份目录：{e}"))?;
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let name = if tag.is_empty() {
        format!("{agent}-{stamp}.bak")
    } else {
        format!("{agent}-{tag}-{stamp}.bak")
    };
    let dst = dir.join(&name);
    std::fs::copy(src, &dst).map_err(|e| format!("备份失败：{e}"))?;
    Ok(name)
}
