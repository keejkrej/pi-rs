//! Port of packages/coding-agent/test/experimental.test.ts

use pi_coding_agent::core::experimental::are_experimental_features_enabled;
use serial_test::serial;

struct EnvGuard {
    key: &'static str,
    original: Option<String>,
}

impl EnvGuard {
    fn save(key: &'static str) -> Self {
        Self {
            key,
            original: pi_js::env::var(key),
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.original {
            Some(val) => pi_js::env::set_var(self.key, val),
            None => pi_js::env::remove_var(self.key),
        }
    }
}

#[test]
#[serial]
fn returns_false_when_pi_experimental_is_unset() {
    let _guard = EnvGuard::save("PI_EXPERIMENTAL");
    pi_js::env::remove_var("PI_EXPERIMENTAL");
    assert!(!are_experimental_features_enabled());
}

#[test]
#[serial]
fn returns_false_when_pi_experimental_is_empty() {
    let _guard = EnvGuard::save("PI_EXPERIMENTAL");
    pi_js::env::set_var("PI_EXPERIMENTAL", "");
    assert!(!are_experimental_features_enabled());
}

#[test]
#[serial]
fn returns_true_when_pi_experimental_is_set_to_1() {
    let _guard = EnvGuard::save("PI_EXPERIMENTAL");
    pi_js::env::set_var("PI_EXPERIMENTAL", "1");
    assert!(are_experimental_features_enabled());
}

#[test]
#[serial]
fn returns_false_when_pi_experimental_is_set_to_0() {
    let _guard = EnvGuard::save("PI_EXPERIMENTAL");
    pi_js::env::set_var("PI_EXPERIMENTAL", "0");
    assert!(!are_experimental_features_enabled());
}

#[test]
#[serial]
fn returns_false_when_pi_experimental_is_set_to_a_non_1_value() {
    let _guard = EnvGuard::save("PI_EXPERIMENTAL");
    pi_js::env::set_var("PI_EXPERIMENTAL", "true");
    assert!(!are_experimental_features_enabled());
}
