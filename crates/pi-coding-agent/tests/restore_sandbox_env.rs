//! Port of packages/coding-agent/test/restore-sandbox-env.test.ts
//!
//! PORT: `packages/coding-agent/src/bun/restore-sandbox-env.ts` is explicitly marked not ported
//! under PORTING.md §2.6 (`coding-agent/src/bun/**` is Bun-only: restoring `/proc/self/environ` when
//! a compiled Bun binary starts with an empty `process.env`). The three TS unit tests
//! (`does nothing when not running under bun`, `does nothing when process.env already has entries`,
//! `restores environment from /proc/self/environ when bun env is empty`) are N/A in Rust.

use pi_coding_agent::config::{IS_BUN_BINARY, IS_BUN_RUNTIME};

#[test]
fn bun_sandbox_env_restoration_is_not_applicable_to_rust_binary() {
    assert!(!IS_BUN_BINARY);
    assert!(!IS_BUN_RUNTIME);
}
