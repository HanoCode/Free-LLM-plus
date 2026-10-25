//! 粘性会话持久化（Pro）：重启后把上次的会话→通道映射灌回来。
//!
//! 动机：粘性表只存内存（不落盘是刻意的），重启后第一轮全部未命中——
//! 每个会话都要换一家重烧一遍 prompt cache。对开了十几个并行 agent 的
//! 用户，这是一笔真实的额度浪费，值得 Pro 把它保住。
//!
//! 边界（红线友好）：`Affinity` 本体零改动、零 license 判断——落盘与
//! 回放都收在这个模块，`is_pro` 不进路由核心。免费版既不落盘也不恢复，
//! 行为与 v1.11 完全一致。
//!
//! 文件：`<实例目录>/sessions.json`。tmp + rename 原子写；只存未过期项
//! （恢复时再按 TTL 筛一遍，双保险）。每 60 秒节流快照一次，丢一次
//! 崩溃最多损失 60 秒内的粘性——代价只是几单未命中，可接受。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use freeport_core::affinity::Stick;

use crate::server::State_;

const FILE_NAME: &str = "sessions.json";

#[derive(Serialize, Deserialize)]
struct Snapshot {
    saved_at: i64,
    entries: Vec<Entry>,
}

#[derive(Serialize, Deserialize)]
struct Entry {
    session: String,
    #[serde(flatten)]
    stick: Stick,
}

fn file_path(st: &State_) -> PathBuf {
    st.instance_dir().join(FILE_NAME)
}

/// 启动时回放。只在该 Pro 时读；过期项由 `Affinity::restore` 淘汰。
pub fn restore(gw: &crate::server::Gateway) {
    let st = gw.st.clone();
    tokio::spawn(async move {
        let d = st.data.read().await;
        if !crate::license::state(&d).pro {
            return;
        }
        drop(d);
        let path = file_path(&st);
        let Ok(body) = std::fs::read_to_string(&path) else {
            return;
        };
        let Ok(snap) = serde_json::from_str::<Snapshot>(&body) else {
            tracing::warn!("粘性快照解析失败，跳过恢复");
            return;
        };
        let now = freeport_core::cooldown::now_ms();
        let entries = snap
            .entries
            .into_iter()
            .map(|e| (e.session, e.stick))
            .collect::<Vec<_>>();
        let restored = {
            let mut af = st.affinity.lock().unwrap_or_else(|e| e.into_inner());
            let before = af.len();
            af.restore(entries, now);
            af.len() - before
        };
        if restored > 0 {
            tracing::info!(restored, "已从快照恢复粘性会话");
        }
    });
}

/// 定时节流快照。60 秒一拍；网关停了就退出。
pub fn spawn(gw: &crate::server::Gateway) {
    let st = gw.st.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            if !st.running.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            let pro = {
                let d = st.data.read().await;
                crate::license::state(&d).pro
            };
            if !pro {
                continue;
            }
            let now = freeport_core::cooldown::now_ms();
            let active = {
                let af = st.affinity.lock().unwrap_or_else(|e| e.into_inner());
                af.active(now)
            };
            if active.is_empty() {
                continue;
            }
            let snap = Snapshot {
                saved_at: now,
                entries: active
                    .into_iter()
                    .map(|(session, stick)| Entry { session, stick })
                    .collect(),
            };
            let Ok(body) = serde_json::to_string(&snap) else { continue };
            let path = file_path(&st);
            let tmp = path.with_extension("tmp");
            if std::fs::write(&tmp, body.as_bytes()).is_ok() {
                let _ = std::fs::rename(&tmp, &path);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_roundtrip_shape() {
        let snap = Snapshot {
            saved_at: 1,
            entries: vec![Entry {
                session: "s".into(),
                stick: Stick {
                    provider: "groq".into(),
                    account: "a".into(),
                    model: "m".into(),
                    at: 2,
                },
            }],
        };
        let body = serde_json::to_string(&snap).unwrap();
        let back: Snapshot = serde_json::from_str(&body).unwrap();
        assert_eq!(back.entries[0].stick.provider, "groq");
    }
}
