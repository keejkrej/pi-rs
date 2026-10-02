//! pi_js::crypto (Rust-only; API contract: PORTING.md Appendix A).
//!
//! `crypto.randomUUID`, `crypto.randomBytes`, `createHash("sha256")` and `Math.random`.

use std::sync::Arc;

use sha2::{Digest, Sha256};

use crate::seam::Slot;

type RandomFn = dyn Fn() -> f64 + Send + Sync;

thread_local! {
    static MATH_RANDOM: Slot<RandomFn> = Slot::new();
}

/// `crypto.randomUUID()`: a random (version 4) UUID, lowercase hyphenated.
pub fn random_uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// `crypto.randomBytes(n)`: `n` bytes from the OS CSPRNG.
///
/// Panics only if the OS random source fails, where Node throws an unrecoverable error.
pub fn random_bytes(n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    if let Err(e) = getrandom::fill(&mut buf) {
        panic!("crypto.randomBytes: OS random source failed: {e}");
    }
    buf
}

/// `createHash("sha256").update(b).digest()`.
pub fn sha256(b: &[u8]) -> [u8; 32] {
    Sha256::digest(b).into()
}

/// `createHash("sha256").update(b).digest("hex")`: lowercase hex.
pub fn sha256_hex(b: &[u8]) -> String {
    hex::encode(sha256(b))
}

/// `Math.random()`: uniform in `[0, 1)`. Tests replace it with
/// [`testing::override_math_random`] (`vi.spyOn(Math, "random")`).
pub fn math_random() -> f64 {
    match MATH_RANDOM.with(Slot::get) {
        Some(f) => f(),
        None => rand::random::<f64>(),
    }
}

pub mod testing {
    use super::*;

    /// `vi.spyOn(Math, "random").mockImplementation(f)` for the current thread; the previous
    /// behavior returns when the guard drops.
    pub fn override_math_random(f: impl Fn() -> f64 + Send + Sync + 'static) -> crate::seam::Guard {
        MATH_RANDOM.with(|s| s.set(Arc::new(f) as Arc<RandomFn>))
    }
}
