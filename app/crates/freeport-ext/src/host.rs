//! sidecar 进程宿主：按需拉起、动态端口、崩溃隔离、空闲退出。
//!
//! 门槛三条的落点：
//! - 进程隔离：sidecar 是独立子进程，它崩了只是这一次请求失败，
//!   宿主与主网关照常活着；下次请求按需重新拉起。
//! - 端口不暴露：子进程绑 127.0.0.1:0（内核分配），端口号只进这里的
//!   内部路由表，不进配置、不进 UI、不落盘。
//! - 按需拉起：配置里写了「按需」，就真的只在第一个请求到来时才起进程。

use std::collections::HashMap;
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

/// 传给 sidecar 的启动配置。经 stdin 传入，不走 argv / 环境变量——
/// `ps` 和 `/proc/<pid>/environ` 都能被同机其他用户看到，stdin 看不到。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SidecarCfg {
    pub id: String,
    #[serde(rename = "baseUrl")]
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub model: String,
    /// 允许出网的域名（来自 manifest 的 permissions.domains）。
    #[serde(default)]
    pub domains: Vec<String>,
}

struct Running {
    port: u16,
    child: Child,
    last_used: Instant,
}

/// 空闲多久后杀掉 sidecar。图片/转写都是突发负载，常驻没有意义。
const IDLE_SHUTDOWN: Duration = Duration::from_secs(600);
/// 清扫节拍。
const REAP_TICK: Duration = Duration::from_secs(60);
/// 等 sidecar 报端口的上限。
const SPAWN_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Default)]
pub struct Host {
    running: Mutex<HashMap<String, Running>>,
}

pub struct HostStatus {
    pub id: String,
    pub running: bool,
}

impl Host {
    pub fn new() -> Self {
        Self::default()
    }

    /// 返回 sidecar 的内部端口；不在运行就拉起。
    ///
    /// 失败要给用户能看懂的中文：起不来 ≠ 网关坏了。
    pub async fn ensure_running(&self, cfg: &SidecarCfg) -> Result<u16, String> {
        {
            let mut map = self.running.lock().await;
            if let Some(r) = map.get_mut(&cfg.id) {
                // 上一次请求后进程可能已经自己退了；探一下再决定复用
                if !matches!(r.child.try_wait(), Ok(None)) {
                    tracing::warn!("sidecar {} 已退出，将重新拉起", cfg.id);
                    map.remove(&cfg.id);
                } else {
                    r.last_used = Instant::now();
                    return Ok(r.port);
                }
            }
        }
        self.spawn(cfg).await
    }

    async fn spawn(&self, cfg: &SidecarCfg) -> Result<u16, String> {
        let exe = std::env::current_exe().map_err(|e| format!("找不到宿主可执行文件：{e}"))?;
        let mut child = Command::new(exe)
            .args(["--freeport-sidecar", &cfg.id])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("拉起 sidecar 失败：{e}"))?;

        // 配置只走 stdin：凭据不能出现在命令行里
        {
            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| "sidecar stdin 不可用".to_string())?;
            let line = serde_json::to_string(cfg).map_err(|e| e.to_string())?;
            stdin
                .write_all(line.as_bytes())
                .await
                .map_err(|e| format!("向 sidecar 传配置失败：{e}"))?;
            stdin
                .shutdown()
                .await
                .map_err(|e| format!("关闭 sidecar stdin 失败：{e}"))?;
        }

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "sidecar stdout 不可用".to_string())?;
        let mut lines = BufReader::new(stdout).lines();

        // 端口握手：sidecar 就绪后向 stdout 打一行 `PORT <n>`。
        // 超时说明子进程没起来（二进制坏、磁盘满……），报错并回收。
        let wait = async {
            while let Some(line) = lines.next_line().await.map_err(|e| e.to_string())? {
                if let Some(port) = line.strip_prefix("PORT ") {
                    return port
                        .trim()
                        .parse::<u16>()
                        .map_err(|_| format!("sidecar 报告了坏端口：{line:?}"));
                }
            }
            Err("sidecar 在报端口前就退出了".to_string())
        };
        let port = match tokio::time::timeout(SPAWN_TIMEOUT, wait).await {
            Ok(r) => r,
            Err(_) => Err("sidecar 启动超时（15s）".into()),
        };
        if let Err(e) = &port {
            let mut c = child;
            let _ = c.start_kill();
            tracing::error!("sidecar {} 启动失败：{e}", cfg.id);
            return Err(e.clone());
        }
        let port = port.unwrap();

        let id = cfg.id.clone();
        let mut map = self.running.lock().await;
        map.insert(id, Running { port, child, last_used: Instant::now() });
        tracing::info!("sidecar {} 已就绪（内部端口不外露）", cfg.id);
        Ok(port)
    }

    pub async fn stop(&self, id: &str) -> bool {
        let mut map = self.running.lock().await;
        if let Some(mut r) = map.remove(id) {
            let _ = r.child.start_kill();
            tracing::info!("sidecar {id} 已停止");
            true
        } else {
            false
        }
    }

    pub async fn stop_all(&self) {
        let mut map = self.running.lock().await;
        for (id, mut r) in map.drain() {
            let _ = r.child.start_kill();
            tracing::info!("sidecar {id} 已停止（宿主关闭）");
        }
    }

    pub async fn status(&self, id: &str) -> bool {
        let mut map = self.running.lock().await;
        match map.get_mut(id) {
            Some(r) => {
                if matches!(r.child.try_wait(), Ok(None)) {
                    true
                } else {
                    map.remove(id);
                    false
                }
            }
            None => false,
        }
    }
}

/// 每个进程一份的宿主单例。网关只有一个，sidecar 宿主也只需要一个。
static HOST: std::sync::OnceLock<std::sync::Arc<Host>> = std::sync::OnceLock::new();

pub fn shared() -> std::sync::Arc<Host> {
    HOST.get_or_init(|| std::sync::Arc::new(Host::new())).clone()
}

/// 启动空闲清扫循环（幂等）。
pub fn start_reaper(host: std::sync::Arc<Host>) {
    static STARTED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    if STARTED.set(()).is_err() {
        return;
    }
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(REAP_TICK).await;
            let mut map = host.running.lock().await;
            let expired: Vec<String> = map
                .iter()
                .filter(|(_, r)| r.last_used.elapsed() > IDLE_SHUTDOWN)
                .map(|(id, _)| id.clone())
                .collect();
            for id in expired {
                if let Some(mut r) = map.remove(&id) {
                    let _ = r.child.start_kill();
                    tracing::info!("sidecar {id} 空闲超过 10 分钟，已回收");
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn status_of_unknown_is_false() {
        let h = Host::new();
        assert!(!h.status("nope").await);
        assert!(!h.stop("nope").await);
    }

    #[tokio::test]
    async fn stop_all_on_empty_is_noop() {
        let h = Host::new();
        h.stop_all().await;
    }
}
