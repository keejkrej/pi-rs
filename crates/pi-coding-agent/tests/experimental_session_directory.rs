//! Port of packages/coding-agent/test/experimental-session-directory.test.ts

use pi_coding_agent::experimental::server::resolve_session_directory;
use pi_js::path::{join, resolve};
use serial_test::serial;

struct AgentDirEnvGuard {
    original: Option<String>,
}

impl AgentDirEnvGuard {
    fn set(value: &str) -> Self {
        let original = pi_js::env::var("PI_CODING_AGENT_DIR");
        pi_js::env::set_var("PI_CODING_AGENT_DIR", value);
        Self { original }
    }
}

impl Drop for AgentDirEnvGuard {
    fn drop(&mut self) {
        match &self.original {
            Some(val) => pi_js::env::set_var("PI_CODING_AGENT_DIR", val),
            None => pi_js::env::remove_var("PI_CODING_AGENT_DIR"),
        }
    }
}

#[test]
#[serial]
fn uses_the_experimental_directory_under_the_configured_agent_directory_by_default() {
    let _guard = AgentDirEnvGuard::set("/tmp/pi-agent-config");
    assert_eq!(
        resolve_session_directory(None),
        "/tmp/pi-agent-config/experimental/sessions"
    );
}

#[test]
#[serial]
fn resolves_an_explicit_relative_directory_from_the_current_working_directory() {
    let _guard = AgentDirEnvGuard::set("/tmp/pi-agent-config");
    assert_eq!(
        resolve_session_directory(Some("relative/sessions")),
        resolve(&["relative/sessions"])
    );
}

#[test]
#[serial]
fn expands_a_tilde_in_an_explicit_directory() {
    assert_eq!(
        resolve_session_directory(Some("~/custom-sessions")),
        join(&[&pi_js::env::home_dir(), "custom-sessions"])
    );
}
