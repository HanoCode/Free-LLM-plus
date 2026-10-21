//! Freeport 的扩展宿主：把「chat 之外的能力」关进独立进程。
//!
//! 四条安装门槛在这里全部是代码，不是文档：
//! 1. 看权限：[`manifest::ExtManifest::validate`]——权限清单必须显式声明；
//! 2. 验签名：[`sig::verify_manifest`]——Ed25519，作者公钥写死在发行包里；
//! 3. 进程隔离：[`host::Host`]——sidecar 是独立子进程，崩了只影响当次请求；
//! 4. 端口不暴露：[`runtime`]——127.0.0.1:0 动态端口，只进内部路由表。
//!
//! v1 带两个第一方扩展：ASR（音频转写）与图片生成。它们不进 chat 主链路，
//! 不占 chat 的账本与故障域。

pub mod host;
pub mod install;
pub mod manifest;
pub mod runtime;
pub mod sig;

pub use host::{shared, start_reaper, Host, SidecarCfg};
pub use manifest::{ExtKind, ExtManifest, ExtPermissions};

/// 给 UI 的扩展状态快照。端口在这里**故意不存在**——它只进内部路由表。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtStatus {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub version: String,
    pub author: String,
    pub domains: Vec<String>,
    pub paths: Vec<String>,
    /// manifest 验签是否通过（内置份永远 true；false 时整条不可用）
    pub trusted: bool,
    pub running: bool,
}
