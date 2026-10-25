//! 自动备份（Pro）：定时把「配置 + 明文密钥」打成 transfer 信封写进用户
//! 指定的本地目录。
//!
//! 与手动迁移码（`.fpk`，口令加密）的关系：**备份不是迁移**。快照的口令
//! 由本机主密钥（`vault.key`）派生，所以：
//! - 本机恢复：随时可解，用于「改坏了回滚到昨天」；
//! - 跨机器：解不开（没有 vault.key）——跨机器请走手动迁移码，那才是
//!   带着口令走人的场景。这条边界写进 UI 文案，不藏在文档里。
//!
//! 借用 transfer 的信封与参数校验：同一套 Argon2id + AES-256-GCM，
//! 不发明第二种格式。Argon2 一次 0.3–0.8s，对「每天一次」的节奏无感。

use std::path::PathBuf;
use std::sync::Arc;

use crate::server::{save_here, State_};

const PREFIX: &str = "freeport-backup-";
const EXT: &str = crate::transfer::FILE_EXT;

/// 备份口令 = vault 主密钥的 hex。快照与密钥库同生死：密钥库没了
/// （vault.key 被删），快照自然也解不开——这正是「本机备份」的语义。
fn backup_passphrase() -> Result<String, String> {
    crate::secrets::master_hex().ok_or_else(|| "本机主密钥不可用，无法生成备份口令".to_string())
}

fn snapshot_name() -> String {
    format!("{}{}.{}", PREFIX, chrono::Local::now().format("%Y%m%d-%H%M%S"), EXT)
}

/// 打一份快照。读锁不跨 IO：export 是纯 CPU + 内存操作，拿到 blob 后
/// 再写文件、再更新 last_at。
async fn run_once(st: &Arc<State_>) -> Result<PathBuf, String> {
    let passphrase = backup_passphrase()?;
    let blob = {
        let d = st.data.read().await;
        crate::transfer::export(&d, &passphrase, "freeport auto-backup")?
    };

    let dir = {
        let d = st.data.read().await;
        match d.pro.backup.dir.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(p) => PathBuf::from(p),
            None => return Err("还没有配置备份目录".into()),
        }
    };
    std::fs::create_dir_all(&dir).map_err(|e| format!("备份目录创建失败：{e}"))?;
    let path = dir.join(snapshot_name());
    // tmp + rename：半截快照比没有快照更危险（恢复时才发现是坏的）
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, blob.as_bytes()).map_err(|e| format!("快照写入失败：{e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("快照落盘失败：{e}"))?;

    // 清理超额 + 记账
    let (keep, removed) = {
        let keep = {
            let d = st.data.read().await;
            d.pro.backup.keep.max(1) as usize
        };
        (keep, prune(&dir, keep))
    };
    if removed > 0 {
        tracing::info!(removed, keep, "自动备份清理了超额快照");
    }

    let mut d = st.data.write().await;
    d.pro.backup.last_at = Some(chrono::Utc::now().timestamp());
    save_here(st, &d);
    Ok(path)
}

/// 删掉最旧的快照，只留 `keep` 份。返回删掉的数量。
fn prune(dir: &PathBuf, keep: usize) -> usize {
    let Ok(rd) = std::fs::read_dir(dir) else { return 0 };
    let mut snaps: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with(PREFIX) && n.ends_with(EXT))
                .unwrap_or(false)
        })
        .collect();
    snaps.sort();
    if snaps.len() <= keep {
        return 0;
    }
    let excess = snaps.len() - keep;
    for p in snaps.into_iter().take(excess) {
        let _ = std::fs::remove_file(p);
    }
    excess
}

/// 定时循环。10 分钟醒一次（间隔最小粒度是小时，醒来大多是看一眼就睡）。
pub fn spawn(gw: &crate::server::Gateway) {
    let st: Arc<State_> = gw.st.clone();
    tokio::spawn(async move {
        // 热身：等网关与密钥库初始化完
        tokio::time::sleep(std::time::Duration::from_secs(90)).await;
        loop {
            if !st.running.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            let due = {
                let d = st.data.read().await;
                crate::pro::allow(&d, crate::pro::Feature::AutoBackup)
                    && d.pro.backup.dir.as_deref().map(|s| !s.trim().is_empty()).unwrap_or(false)
                    && d.pro
                        .backup
                        .last_at
                        .map(|t| chrono::Utc::now().timestamp() - t >= (d.pro.backup.interval_hours as i64).max(1) * 3600)
                        .unwrap_or(true)
            };
            if due {
                match run_once(&st).await {
                    Ok(p) => tracing::info!(path = %p.display(), "自动备份完成"),
                    Err(e) => tracing::warn!(error = %e, "自动备份未成功"),
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(600)).await;
        }
    });
}

impl crate::server::Gateway {
    /// 手动备份一次（设置页按钮）。同样要求 Pro 与已配置目录。
    pub async fn backup_now(&self) -> Result<String, String> {
        {
            let d = self.st.data.read().await;
            if !crate::license::state(&d).pro {
                return Err("自动备份是 Pro 功能".into());
            }
            // 已是 Pro 但开关没开：报「未开启」而不是「Pro 功能」——
            // 用户明明是 Pro，说他是free等于把排查引向许可证
            if !d.pro.backup.enabled {
                return Err("自动备份未开启，请在设置里打开".into());
            }
        }
        run_once(&self.st)
            .await
            .map(|p| p.to_string_lossy().to_string())
    }

    /// 列出备份目录里的快照（文件名 + 大小 + 修改时间），按时间倒序。
    pub async fn backup_list(&self) -> Vec<BackupFile> {
        let dir = {
            let d = self.st.data.read().await;
            match &d.pro.backup.dir {
                Some(p) if !p.trim().is_empty() => PathBuf::from(p.trim()),
                _ => return Vec::new(),
            }
        };
        let mut out: Vec<BackupFile> = match std::fs::read_dir(&dir) {
            Ok(rd) => rd
                .flatten()
                .filter_map(|e| {
                    let p = e.path();
                    let name = p.file_name()?.to_str()?.to_string();
                    if !name.starts_with(PREFIX) || !name.ends_with(EXT) {
                        return None;
                    }
                    let meta = e.metadata().ok()?;
                    Some(BackupFile {
                        name,
                        bytes: meta.len(),
                        modified: meta
                            .modified()
                            .ok()
                            .map(|t| chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339()),
                    })
                })
                .collect(),
            Err(_) => Vec::new(),
        };
        out.sort_by(|a, b| b.name.cmp(&a.name));
        out
    }

    /// 从快照恢复。只支持 merge：Replace 会清掉本地多余凭据，
    /// 「回滚」场景用不上它，也别给用户一个能一键清库的按钮。
    pub async fn backup_restore(&self, name: &str) -> Result<crate::transfer::Summary, String> {
        let (dir, passphrase) = {
            let d = self.st.data.read().await;
            let dir = d
                .pro
                .backup
                .dir
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or("还没有配置备份目录")?;
            (PathBuf::from(dir), backup_passphrase()?)
        };
        // 文件名是拼出来的路径的一部分，绝不接受路径分隔符
        if name.contains('/') || name.contains('\\') || name.contains("..") {
            return Err("快照名不合法".into());
        }
        let blob = std::fs::read_to_string(dir.join(name)).map_err(|e| format!("快照读不到：{e}"))?;
        let body = crate::transfer::open(&blob, &passphrase)?;
        let mut d = self.st.data.write().await;
        let summary = crate::transfer::apply(body, &mut d, crate::transfer::Mode::Merge);
        save_here(&self.st, &d);
        Ok(summary)
    }
}

/// 快照列表条目。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupFile {
    pub name: String,
    pub bytes: u64,
    pub modified: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_name_shape() {
        let n = snapshot_name();
        assert!(n.starts_with(PREFIX));
        assert!(n.ends_with(&format!(".{EXT}")));
        // 文件名可排序 = 按时间排序
        let n2 = snapshot_name();
        assert!(n2 >= n);
    }
}
