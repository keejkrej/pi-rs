//! Port of packages/coding-agent/src/core/experimental.ts

pub fn are_experimental_features_enabled() -> bool {
    pi_js::env::var("PI_EXPERIMENTAL").as_deref() == Some("1")
}
