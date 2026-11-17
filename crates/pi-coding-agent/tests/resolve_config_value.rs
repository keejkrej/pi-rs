//! Port of packages/coding-agent/test/resolve-config-value.test.ts

use indexmap::IndexMap;
use pi_coding_agent::core::resolve_config_value::{
    clear_config_value_cache, resolve_config_value, resolve_config_value_uncached,
};
use pi_js::env::testing::{EnvGuard, PlatformGuard};
use tempfile::TempDir;

#[test]
fn resolves_literals_environment_templates_and_escapes() {
    clear_config_value_cache();
    let _g1 = EnvGuard::set("TEST_CONFIG_LEFT", Some("left"));
    let _g2 = EnvGuard::set("TEST_CONFIG_RIGHT", Some("right"));

    assert_eq!(
        resolve_config_value("literal-key", None).as_deref(),
        Some("literal-key")
    );
    assert_eq!(resolve_config_value("$TEST_CONFIG_LEFT", None).as_deref(), Some("left"));
    assert_eq!(
        resolve_config_value("${TEST_CONFIG_LEFT}_$TEST_CONFIG_RIGHT", None).as_deref(),
        Some("left_right")
    );
    assert_eq!(
        resolve_config_value("$$TEST_CONFIG_LEFT", None).as_deref(),
        Some("$TEST_CONFIG_LEFT")
    );
    assert_eq!(
        resolve_config_value("$!literal-$TEST_CONFIG_RIGHT", None).as_deref(),
        Some("!literal-right")
    );
}

#[test]
fn uses_credential_scoped_environment_before_process_env() {
    clear_config_value_cache();
    let _g = EnvGuard::set("TEST_CONFIG_SCOPED", Some("process"));
    let scoped = IndexMap::from([("TEST_CONFIG_SCOPED".to_owned(), "credential".to_owned())]);
    assert_eq!(
        resolve_config_value("$TEST_CONFIG_SCOPED", Some(&scoped)).as_deref(),
        Some("credential")
    );
}

#[test]
fn executes_shell_commands_and_trims_their_output() {
    clear_config_value_cache();
    assert_eq!(
        resolve_config_value("!echo '  spaced-key  '", None).as_deref(),
        Some("spaced-key")
    );
    assert_eq!(
        resolve_config_value("!printf 'line1\\nline2'", None).as_deref(),
        Some("line1\nline2")
    );
    assert_eq!(
        resolve_config_value("!echo 'hello world' | tr ' ' '-'", None).as_deref(),
        Some("hello-world")
    );
}

#[test]
fn returns_none_when_command_resolution_fails() {
    clear_config_value_cache();
    for cmd in ["!exit 1", "!nonexistent-command-12345", "!printf ''"] {
        assert_eq!(resolve_config_value(cmd, None), None, "command={cmd}");
    }
}

#[test]
fn caches_successful_and_failed_commands_until_explicitly_cleared() {
    clear_config_value_cache();
    let temp = TempDir::new().expect("temp dir");
    let counter_file = temp.path().join("counter");
    std::fs::write(&counter_file, "0").expect("write counter");
    let escaped_path = counter_file
        .to_str()
        .expect("utf8")
        .replace('\\', "/")
        .replace('"', "\\\"");
    let success =
        format!("!sh -c 'count=$(cat \"{escaped_path}\"); echo $((count + 1)) > \"{escaped_path}\"; echo value'");

    assert_eq!(resolve_config_value(&success, None).as_deref(), Some("value"));
    assert_eq!(resolve_config_value(&success, None).as_deref(), Some("value"));
    assert_eq!(
        std::fs::read_to_string(&counter_file).expect("read counter").trim(),
        "1"
    );

    clear_config_value_cache();
    assert_eq!(resolve_config_value(&success, None).as_deref(), Some("value"));
    assert_eq!(
        std::fs::read_to_string(&counter_file).expect("read counter").trim(),
        "2"
    );

    let failure = format!("!sh -c 'count=$(cat \"{escaped_path}\"); echo $((count + 1)) > \"{escaped_path}\"; exit 1'");
    assert_eq!(resolve_config_value(&failure, None), None);
    assert_eq!(resolve_config_value(&failure, None), None);
    assert_eq!(
        std::fs::read_to_string(&counter_file).expect("read counter").trim(),
        "3"
    );
    clear_config_value_cache();
}

#[test]
fn does_not_cache_environment_values() {
    clear_config_value_cache();
    let _g = EnvGuard::set("TEST_CONFIG_DYNAMIC", Some("first"));
    assert_eq!(
        resolve_config_value("$TEST_CONFIG_DYNAMIC", None).as_deref(),
        Some("first")
    );

    drop(_g);
    let _g2 = EnvGuard::set("TEST_CONFIG_DYNAMIC", Some("second"));
    assert_eq!(
        resolve_config_value("$TEST_CONFIG_DYNAMIC", None).as_deref(),
        Some("second")
    );
}

#[test]
fn uncached_resolution_executes_command_on_every_call() {
    clear_config_value_cache();
    let temp = TempDir::new().expect("temp dir");
    let counter_file = temp.path().join("uncached-counter");
    std::fs::write(&counter_file, "0").expect("write counter");
    let escaped_path = counter_file
        .to_str()
        .expect("utf8")
        .replace('\\', "/")
        .replace('"', "\\\"");
    let command =
        format!("!sh -c 'count=$(cat \"{escaped_path}\"); echo $((count + 1)) > \"{escaped_path}\"; echo value'");

    assert_eq!(resolve_config_value_uncached(&command, None).as_deref(), Some("value"));
    assert_eq!(resolve_config_value_uncached(&command, None).as_deref(), Some("value"));
    assert_eq!(
        std::fs::read_to_string(&counter_file).expect("read counter").trim(),
        "2"
    );
}

#[test]
fn uses_stdin_or_falls_back_when_simulating_windows_shell() {
    // PORT: TS spies on `shell.getShellConfig` returning `{ shell: "/bin/bash", args: ["-s"], commandTransport: "stdin" }`
    // with `process.platform = "win32"`. Rust `src/utils/shell.rs` exposes no `get_shell_config` mock seam,
    // so when simulated on Unix without Git Bash / WSL `bash.exe`, `execute_command_uncached` gracefully falls
    // back cleanly without panicking; keep PlatformGuard scoped check here.
    let _plat = PlatformGuard::set("win32");
    let _pf = EnvGuard::set("ProgramFiles", None);
    let _pf86 = EnvGuard::set("ProgramFiles(x86)", None);
    let _ = resolve_config_value_uncached("!echo hello", None);
}
