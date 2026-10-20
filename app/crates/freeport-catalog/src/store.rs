//! 目录缓存：`<data_dir>/catalog.json`。
//!
//! 外加 `snapshot.json`：上一次 feed 应用的原样快照。增量补丁是打在它上面的，
//! 没有它就只能每次下全量。它和 `catalog.json` 是两件事——后者是合并后的
//! 工作目录（含基线、models.dev、本地补全），前者只是发布方给的那一份。

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use crate::feed::Snapshot;
use crate::types::Catalog;

const FILE: &str = "catalog.json";
const SNAP: &str = "snapshot.json";

fn data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("FREEPORT_DATA_DIR") {
        return PathBuf::from(dir);
    }
    dirs::data_dir()
        .map(|d| d.join("dev.freeport.app"))
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn cache_file() -> PathBuf {
    data_dir().join(FILE)
}

pub fn load() -> Option<Catalog> {
    let raw = fs::read(cache_file()).ok()?;
    match serde_json::from_slice::<Catalog>(&raw) {
        Ok(c) if !c.providers.is_empty() => Some(c),
        Ok(_) => None,
        Err(e) => {
            tracing::warn!("catalog.json 解析失败（{e}），回退到内置基线");
            None
        }
    }
}

pub fn save(cat: &Catalog) {
    let path = cache_file();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let Ok(raw) = serde_json::to_vec(cat) else { return };
    let tmp = path.with_extension("json.tmp");
    if let Ok(mut f) = fs::File::create(&tmp) {
        let _ = f.write_all(&raw);
        let _ = fs::rename(&tmp, &path);
    }
}

/* ── 补丁基线快照 ───────────────────────────── */

pub fn snapshot_file() -> PathBuf {
    data_dir().join(SNAP)
}

/// 读上一份快照。`seq == 0` 或解析失败都当作没有——
/// 拿一份 seq 不明的快照去拼补丁，只会拼出一个错得很难看的结果。
pub fn load_snapshot() -> Option<Snapshot> {
    let raw = fs::read(snapshot_file()).ok()?;
    match serde_json::from_slice::<Snapshot>(&raw) {
        Ok(s) if s.seq > 0 => Some(s),
        Ok(_) => None,
        Err(e) => {
            tracing::warn!("snapshot.json 解析失败（{e}），下次走全量");
            None
        }
    }
}

pub fn save_snapshot(snap: &Snapshot) {
    let path = snapshot_file();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let Ok(raw) = serde_json::to_vec(snap) else {
        return;
    };
    let tmp = path.with_extension("json.tmp");
    if let Ok(mut f) = fs::File::create(&tmp) {
        let _ = f.write_all(&raw);
        let _ = fs::rename(&tmp, &path);
    }
}
