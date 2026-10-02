//! pi-js: JS runtime semantics shared by every pi-rs crate (PORTING.md Appendix A).

// @generated-mods begin (scaffold-owned, do not edit)
pub mod abort;
pub mod b64;
pub mod callback;
pub mod console;
pub mod crypto;
pub mod env;
pub mod error;
pub mod fetch;
pub mod fs;
pub mod intl;
pub mod json;
pub mod num;
pub mod path;
pub mod regex;
pub mod seam;
pub mod str16;
pub mod testing;
pub mod text;
pub mod time;
pub mod uri;
pub mod vendor;
// @generated-mods end

pub use callback::Unsubscribe;
pub use error::{Error, JsError, NodeError, Result};

/// A boxed, `Send`, `'static` future: the return type of async callbacks
/// (`(a) => Promise<R>` becomes `Arc<dyn Fn(A) -> BoxFuture<R> + Send + Sync>`).
pub type BoxFuture<T> = futures::future::BoxFuture<'static, T>;

/// Installs the rustls `aws-lc-rs` crypto provider as the process default, once. Entry points
/// call this before any TLS client (reqwest, tokio-tungstenite) is built. Later calls, and a
/// provider installed by someone else first, are left untouched.
pub fn ensure_crypto_provider() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        if rustls::crypto::CryptoProvider::get_default().is_none() {
            // An `Err` means another thread installed a provider in between; keep theirs.
            let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        }
    });
}
