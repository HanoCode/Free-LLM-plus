//! PKCE（RFC 7636，S256）与 `state` 随机串。
//!
//! 为什么不用 `plain`：公共客户端的 code 会在 redirect 上暴露给本机任何进程，
//! 只有 S256 才能保证「拿到 code 也换不了 token」。

use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
use base64::Engine as _;
use rand::distributions::Alphanumeric;
use rand::Rng;
use sha2::{Digest, Sha256};

/// verifier 长度取 RFC 允许的下限 43：熵够（256 bit），URL 也短。
const VERIFIER_LEN: usize = 43;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl Pkce {
    /// 生成一对新的 verifier / challenge。
    pub fn generate() -> Self {
        let verifier = random_string(VERIFIER_LEN);
        let challenge = challenge_for(&verifier);
        Self { verifier, challenge }
    }

    /// `code_challenge_method` 固定 S256。
    pub fn method(&self) -> &'static str {
        "S256"
    }
}

/// 由 verifier 算出 challenge。抽成独立函数，方便用 RFC 附录 B 的向量做校验。
pub fn challenge_for(verifier: &str) -> String {
    B64URL.encode(Sha256::digest(verifier.as_bytes()))
}

/// CSRF 用的 `state`：每次授权请求都要新生成，回调时必须逐字比对。
pub fn state() -> String {
    random_string(32)
}

fn random_string(len: usize) -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(len)
        .map(char::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_rfc7636_appendix_b() {
        // RFC 7636 附录 B 的示例向量
        assert_eq!(
            challenge_for("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn challenge_is_url_safe_and_unpadded() {
        let pkce = Pkce::generate();
        assert_eq!(pkce.verifier.len(), VERIFIER_LEN);
        assert_eq!(pkce.challenge.len(), 43); // sha256 → 32 字节 → 43 个 base64url 字符
        assert!(!pkce.challenge.contains('='));
        assert!(!pkce.challenge.contains('+'));
        assert!(!pkce.challenge.contains('/'));
        assert!(pkce.verifier.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }

    #[test]
    fn verifiers_are_not_repeated() {
        let a = Pkce::generate();
        let b = Pkce::generate();
        assert_ne!(a.verifier, b.verifier);
        assert_ne!(state(), state());
    }
}
