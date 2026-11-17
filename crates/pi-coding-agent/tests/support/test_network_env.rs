//! Port of packages/coding-agent/test/test-network-env.ts

use pi_js::env::testing::EnvGuard;

/// Enable network code paths for tests that replace external I/O with local fixtures or mocks.
pub fn allow_network() -> EnvGuard {
    EnvGuard::set("PI_OFFLINE", None)
}
