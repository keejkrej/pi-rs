//! Port of packages/ai/src/auth/oauth/pkce.ts

use pi_js::Result;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PkceChallenge {
    pub verifier: String,
    pub challenge: String,
}

/// Generate PKCE code verifier and S256 challenge.
pub async fn generate_pkce() -> Result<PkceChallenge> {
    let verifier_bytes = pi_js::crypto::random_bytes(32);
    let verifier = pi_js::b64::encode_url(&verifier_bytes);
    let digest = pi_js::crypto::sha256(verifier.as_bytes());
    let challenge = pi_js::b64::encode_url(&digest);
    Ok(PkceChallenge { verifier, challenge })
}
