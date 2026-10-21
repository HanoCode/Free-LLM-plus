//! 本地控制凭据。
//!
//! `/v1/freeport/inspect` 把**内存里的**事实（实时日志、内存冷却、在途数）暴露给
//! 同机的另一个进程（MCP server）。它需要一把凭据，但这把凭据必须与数据面隔离：
//!
//! - **不能用 `authToken` / 虚拟 key**：那些能发 `/v1/chat/completions`，能烧额度。
//!   拿它当自省凭据，等于为了看日志而把「花钱」的能力交出去。
//! - **不能是"本机就放行"**：本机上任何一个网页都能打 `127.0.0.1`（CORS 只挡读
//!   响应，挡不住请求），没有凭据的话，DNSSEC 都不用就能被 DNS rebinding 读空。
//! - **只落本地文件，0600**：与 `secrets.json` 同一个信任域——能读数据目录的人本来
//!   就能读密钥。它不是一个"更强的秘密"，只是把「能发请求」与「能看状态」分开。
//!
//! 两道闸一起用：请求必须来自回环地址，且带上文件里的 token。少一道都够用，
//! 但两道都不贵。

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// 请求头名。刻意用独立的名字，不复用 `Authorization`——
/// 数据面永远不看这个头，控制面永远不看 `Authorization`，两边不可能互相冒充。
pub const HEADER: &str = "x-freeport-control";

const FILE_NAME: &str = "control.key";
const MIN_LEN: usize = 24;

pub fn token_file(dir: &Path) -> PathBuf {
    dir.join(FILE_NAME)
}

/// 读控制凭据。没有或明显不合法就返回 `None`。
pub fn read_token(dir: &Path) -> Option<String> {
    let raw = fs::read_to_string(token_file(dir)).ok()?;
    let t = raw.trim().to_string();
    (t.len() >= MIN_LEN).then_some(t)
}

/// 读或生成。只在**网关启动**时调用——MCP 进程只读不写，
/// 免得两个进程各生成一份、谁都不认对方。
pub fn ensure_token(dir: &Path) -> String {
    if let Some(t) = read_token(dir) {
        return t;
    }
    if let Err(e) = fs::create_dir_all(dir) {
        tracing::warn!("建数据目录失败 {}：{e}", dir.display());
        return String::new();
    }
    let token = new_token();
    let tmp = dir.join(format!("{FILE_NAME}.tmp"));
    // 先写 tmp 再 rename：读者永远看不到半截文件，也就不会读到空串后
    // 误判成「没有凭据」。
    let ok = (|| -> std::io::Result<()> {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(token.as_bytes())?;
        f.sync_all()?;
        harden(&tmp)?;
        fs::rename(&tmp, token_file(dir))
    })();
    match ok {
        Ok(()) => token,
        Err(e) => {
            tracing::warn!("写控制凭据失败 {}：{e}", token_file(dir).display());
            // 磁盘只读也不该让网关起不来：内存里有一把，本次运行可用，
            // 只是重启后 MCP 读不到（它会如实说「看不到实时状态」）。
            token
        }
    }
}

/// 常量时间比较。凭据短、攻击面是本机，时序攻击基本不成立，
/// 但比较函数本来就该这么写，没理由留一个例外。
pub fn verify(expected: &str, got: &str) -> bool {
    if expected.is_empty() || got.is_empty() {
        return false;
    }
    let (a, b) = (expected.as_bytes(), got.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn new_token() -> String {
    // 32 字节随机 → 64 hex。这把凭据唯一的强度来源就是随机性，
    // 所以别用时间戳凑——那等于把「凭据」写成「猜得到的数」。
    use rand::RngCore;
    let mut buf = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut buf);
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

fn harden(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir() -> PathBuf {
        // 计数器而不是时间戳：两个用例并行启动时，`SystemTime` 的实测精度
        // 未必真的到纳秒，撞到同一目录就会「A 删掉了 B 正在读的文件」。
        // 同一个进程内自增是唯一可靠的隔离。
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("fp-ctl-{}-{n}", std::process::id()));
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn generates_once_and_then_reads_the_same_token() {
        let d = tmpdir();
        let a = ensure_token(&d);
        let b = ensure_token(&d);
        assert_eq!(a.len(), 64);
        assert_eq!(a, b, "同一个目录反复启动必须是同一把凭据");
        assert_eq!(read_token(&d).as_deref(), Some(a.as_str()));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn missing_or_truncated_token_is_rejected() {
        let d = tmpdir();
        assert!(read_token(&d).is_none());
        fs::write(token_file(&d), "short").unwrap();
        assert!(read_token(&d).is_none(), "半截文件不能当成凭据");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn verify_requires_exact_match() {
        assert!(verify("abcdef", "abcdef"));
        assert!(!verify("abcdef", "abcdeg"));
        assert!(!verify("abcdef", "abcde"));
        assert!(!verify("", ""));
        assert!(!verify("abcdef", ""));
    }

    #[cfg(unix)]
    #[test]
    fn token_file_is_not_world_readable() {
        use std::os::unix::fs::PermissionsExt;
        let d = tmpdir();
        ensure_token(&d);
        let mode = fs::metadata(token_file(&d)).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let _ = fs::remove_dir_all(&d);
    }
}
