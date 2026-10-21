//! 第一方扩展：manifest 随宿主分发（include_str!），启动时落进扩展目录。
//!
//! 目录即真相：`<data>/extensions/<id>/manifest.json`。损坏或缺失时由内置
//! const 修复——用户手动改坏扩展目录不该把功能改没，改对也逃不过验签。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::manifest::ExtManifest;
use crate::sig;

pub const ASR_MANIFEST_JSON: &str = include_str!("../builtin/asr.manifest.json");
pub const IMAGE_MANIFEST_JSON: &str = include_str!("../builtin/image.manifest.json");
pub const SPEECH_MANIFEST_JSON: &str = include_str!("../builtin/speech.manifest.json");
pub const VIDEO_MANIFEST_JSON: &str = include_str!("../builtin/video.manifest.json");
pub const RERANK_MANIFEST_JSON: &str = include_str!("../builtin/rerank.manifest.json");

/// (id, 签名后的 manifest 原文)。写盘用原文，展示用解析结果。
const BUILTIN_RAW: &[(&str, &str)] = &[
    ("asr", ASR_MANIFEST_JSON),
    ("image", IMAGE_MANIFEST_JSON),
    ("speech", SPEECH_MANIFEST_JSON),
    ("video", VIDEO_MANIFEST_JSON),
    ("rerank", RERANK_MANIFEST_JSON),
];

/// 解析并验签过的内置 manifest。验签失败时宁可空手而归（fail-closed），
/// 让扩展页显示「不可用」，而不是装进来一个没人签过字的东西。
pub fn builtin() -> &'static [ExtManifest] {
    static CACHE: OnceLock<Vec<ExtManifest>> = OnceLock::new();
    CACHE.get_or_init(|| {
        BUILTIN_RAW
            .iter()
            .filter_map(|(_, raw)| match sig::parse_and_verify(raw.as_bytes()) {
                Ok(m) => Some(m),
                Err(e) => {
                    tracing::error!("内置扩展 manifest 验签失败，已跳过：{e}");
                    None
                }
            })
            .collect()
    })
}

pub fn builtin_by_id(id: &str) -> Option<&'static ExtManifest> {
    builtin().iter().find(|m| m.id == id)
}

pub fn extensions_dir() -> PathBuf {
    // 与 freeport-gateway::store::data_dir 同一套解析：测试与 headless
    // 都用 FREEPORT_DATA_DIR 把数据指到别处，扩展目录必须跟着走，
    // 否则会出现「配置在 A，扩展装进了 B」的分裂。
    if let Ok(dir) = std::env::var("FREEPORT_DATA_DIR") {
        return PathBuf::from(dir).join("extensions");
    }
    dirs::data_dir()
        .map(|d| d.join("dev.freeport.app").join("extensions"))
        .unwrap_or_else(|| PathBuf::from(".freeport-extensions"))
}

fn manifest_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(id).join("manifest.json")
}

/// 把内置 manifest 写进扩展目录。已存在且验签通过时不动（幂等）。
///
/// 返回 true 表示目录内容有变化（首次安装或修复）。
pub fn materialize_builtin(dir: &Path) -> bool {
    let mut changed = false;
    for (id, raw) in BUILTIN_RAW {
        let path = manifest_path(dir, id);
        if path.exists() {
            // 磁盘上的那份也要验签：手改过的 manifest 不能混进来
            match std::fs::read(&path)
                .map_err(|e| e.to_string())
                .and_then(|b| sig::parse_and_verify(&b))
            {
                Ok(_) => continue,
                Err(e) => tracing::warn!("扩展目录里的 {id} 不可信（{e}），用内置份覆盖"),
            }
        }
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // 先写 tmp 再 rename：`installed()` 是会被并发读的（启动检查 + UI 刷新 +
        // 并行测试），直接 write 会让读者读到「半截 manifest」——验签失败后
        // 这个扩展就从列表里静默消失，现象是「扩展页偶尔少一个」。
        // rename 是原子的：读者只会看到旧份或完整的新份，不会看到中间态。
        let tmp = path.with_extension("json.tmp");
        match std::fs::write(&tmp, raw).and_then(|_| std::fs::rename(&tmp, &path)) {
            Ok(()) => {
                changed = true;
                tracing::info!("已安装内置扩展：{id}");
            }
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                tracing::error!("写内置扩展 {id} 失败：{e}");
            }
        }
    }
    changed
}

/// 从扩展目录读全部可信 manifest。坏条目跳过并留痕，不拖垮整个目录。
pub fn installed(dir: &Path) -> Vec<ExtManifest> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in rd.flatten() {
        let path = e.path().join("manifest.json");
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        match sig::parse_and_verify(&bytes) {
            Ok(m) => out.push(m),
            Err(err) => tracing::warn!("扩展目录条目 {} 验签失败，已跳过：{err}", path.display()),
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_manifests_parse_and_validate() {
        // 公钥占位期间验签必然失败，keygen 替换后 builtin() 才会非空；
        // 但解析与门槛校验必须永远成功——打包时写错 JSON 比验签失败更糟。
        for (_, raw) in BUILTIN_RAW {
            let m: ExtManifest = serde_json::from_str(raw).unwrap();
            m.validate().unwrap();
        }
    }

    #[test]
    fn materialize_is_idempotent_and_repairs_tampering() {
        let dir = std::env::temp_dir().join(format!("fp-ext-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        // 首次：装进目录
        assert!(materialize_builtin(&dir));
        let listed = installed(&dir);
        // 内置份数量跟着 BUILTIN_RAW 走；写死数字是为了「加了新扩展却没打包」时立刻挂
        assert_eq!(listed.len(), 5);

        // 二次：幂等，不重写
        assert!(!materialize_builtin(&dir));

        // 手改 manifest：验签失败 → 用内置份修复
        let path = dir.join("asr").join("manifest.json");
        std::fs::write(&path, b"{\"id\":\"asr\",\"sig\":\"evil\"}").unwrap();
        assert!(materialize_builtin(&dir));
        assert_eq!(installed(&dir).len(), 5);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
