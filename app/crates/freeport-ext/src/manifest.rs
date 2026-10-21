//! 扩展 manifest：声明自己是谁、能访问什么。
//!
//! 四条安装门槛的第一条就落在这里：权限清单必须显式声明，
//! 「不勾不给装」——没有 permissions 字段的 manifest 直接解析失败。

use serde::{Deserialize, Serialize};

/// 扩展能力类型。新类型加进来要同时补：sidecar 路由、宿主的端点映射、
/// 以及内置 manifest（`builtin/` 下签名后的 JSON）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExtKind {
    Asr,
    Image,
    Speech,
    Video,
    Rerank,
}

impl ExtKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ExtKind::Asr => "asr",
            ExtKind::Image => "image",
            ExtKind::Speech => "speech",
            ExtKind::Video => "video",
            ExtKind::Rerank => "rerank",
        }
    }

    /// 宿主 → sidecar 的内部路径。sidecar 只暴露这几个固定名，
    /// 上游真实路径由 sidecar 自己拼。
    pub fn sidecar_path(&self) -> &'static str {
        match self {
            ExtKind::Asr => "/transcribe",
            ExtKind::Image => "/generate",
            ExtKind::Speech => "/speech",
            ExtKind::Video => "/video",
            ExtKind::Rerank => "/rerank",
        }
    }
}

/// 权限清单。域名按主机名声明（支持 `*.example.com`），目录按绝对路径声明。
///
/// v1 的 runtime 不做任何文件系统访问：声明了 `paths` 的扩展会被安装门槛
/// 拒收（fail-closed）——宁可少装，不给一个「声明了但没人拦」的假权限。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExtPermissions {
    #[serde(default)]
    pub domains: Vec<String>,
    #[serde(default)]
    pub paths: Vec<String>,
}

/// 扩展清单。`sig` 是对**去掉 sig 字段后的规范 JSON 字节**做的 Ed25519 签名。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub kind: ExtKind,
    /// `self` = 随宿主分发的内置 sidecar；文件安装的扩展填二进制路径。
    pub entry: String,
    /// entry 的 sha256（小写 hex）。`entry = "self"` 时可省——哈希自己没有意义。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sha256: String,
    pub author: String,
    pub permissions: ExtPermissions,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sig: String,
}

fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 32
        && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// 主机名：小写字母数字连字符与点。支持前缀通配 `*.example.com`。
pub fn is_valid_domain(d: &str) -> bool {
    let host = d.strip_prefix("*.").unwrap_or(d);
    !host.is_empty()
        && !host.starts_with('.')
        && !host.ends_with('.')
        && host
            .split('.')
            .all(|label| !label.is_empty() && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
}

fn is_semverish(v: &str) -> bool {
    !v.is_empty() && v.split('.').all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

impl ExtManifest {
    /// 签名覆盖的字节：去掉 sig 后重新序列化。
    ///
    /// 字段顺序由结构体定义，语义相同则字节相同——这是验签能跨机器成立的前提。
    pub fn signed_bytes(&self) -> Result<Vec<u8>, String> {
        let mut copy = self.clone();
        copy.sig = String::new();
        serde_json::to_vec(&copy).map_err(|e| format!("序列化 manifest 失败：{e}"))
    }

    /// 四条门槛里「看权限」这条的落点。任何一条不满足都不该进安装目录。
    pub fn validate(&self) -> Result<(), String> {
        if !is_valid_id(&self.id) {
            return Err(format!("manifest id 不合法：{:?}（只允许小写字母、数字、连字符）", self.id));
        }
        if self.name.trim().is_empty() {
            return Err("manifest name 不能为空".into());
        }
        if !is_semverish(&self.version) {
            return Err(format!("manifest version 不合法：{:?}", self.version));
        }
        if self.author.trim().is_empty() {
            return Err("manifest author 不能为空".into());
        }
        if self.entry.trim().is_empty() {
            return Err("manifest entry 不能为空".into());
        }
        if self.permissions.domains.is_empty() && self.permissions.paths.is_empty() {
            // 不勾不给装：连「我不访问任何东西」都要显式写不出来吗？——要写。
            // 两个都为空意味着作者没看清门槛就打包，拒收逼他把清单补全。
            return Err("权限清单为空：至少要声明 domains 或 paths（不勾不给装）".into());
        }
        for d in &self.permissions.domains {
            if !is_valid_domain(d) {
                return Err(format!("权限里的域名不合法：{d:?}"));
            }
        }
        for p in &self.permissions.paths {
            if !p.starts_with('/') {
                return Err(format!("权限里的目录必须是绝对路径：{p:?}"));
            }
            // v1 的 sidecar 不做文件系统访问，声明了也拦不住，所以直接拒收
            return Err(format!(
                "目录权限暂不支持（v1 的 sidecar 不访问文件系统）：{p:?}"
            ));
        }
        if self.entry != "self" && self.sha256.is_empty() {
            return Err("文件入口必须带 sha256（验哈希是安装门槛）".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ExtManifest {
        serde_json::from_value(serde_json::json!({
            "id": "asr",
            "name": "ASR",
            "version": "1.0.0",
            "kind": "asr",
            "entry": "self",
            "author": "freeport",
            "permissions": { "domains": ["api.groq.com"], "paths": [] },
        }))
        .unwrap()
    }

    #[test]
    fn valid_manifest_passes() {
        sample().validate().unwrap();
    }

    #[test]
    fn empty_permissions_rejected() {
        let mut m = sample();
        m.permissions.domains.clear();
        assert!(m.validate().is_err());
    }

    #[test]
    fn bad_id_rejected() {
        let mut m = sample();
        m.id = "ASR!".into();
        assert!(m.validate().is_err());
    }

    #[test]
    fn path_permissions_rejected_in_v1() {
        let mut m = sample();
        m.permissions.domains.clear();
        m.permissions.paths = vec!["/tmp".into()];
        // fail-closed：拦不住的权限不发
        assert!(m.validate().is_err());
    }

    #[test]
    fn domain_validation() {
        assert!(is_valid_domain("api.groq.com"));
        assert!(is_valid_domain("*.groq.com"));
        assert!(!is_valid_domain("https://api.groq.com"));
        assert!(!is_valid_domain("api.groq.com/v1"));
        assert!(!is_valid_domain(""));
        assert!(!is_valid_domain("a..b"));
    }

    #[test]
    fn file_entry_requires_hash() {
        let mut m = sample();
        m.entry = "/opt/ext/bin".into();
        assert!(m.validate().is_err());
        m.sha256 = "ab".repeat(32);
        m.validate().unwrap();
    }

    #[test]
    fn signed_bytes_exclude_sig() {
        let mut m = sample();
        let a = m.signed_bytes().unwrap();
        m.sig = "x".repeat(64);
        let b = m.signed_bytes().unwrap();
        assert_eq!(a, b);
    }
}
