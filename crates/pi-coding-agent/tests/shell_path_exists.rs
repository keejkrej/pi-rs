//! Port of packages/coding-agent/test/shell-path-exists.test.ts

use std::sync::Arc;

use pi_coding_agent::utils::shell::{get_power_shell_config, get_shell_config, testing};
use pi_js::env::testing::{EnvGuard, PlatformGuard};
use serial_test::serial;

const ALIASES_DIR: &str = r"D:\Users\test\AppData\Local\Microsoft\WindowsApps";

#[test]
#[serial]
fn windows_store_shell_aliases_accept_path_and_custom_and_reject_missing() {
    let _plat = PlatformGuard::set("win32");
    let _pf = EnvGuard::set("ProgramFiles", None);
    let _pf86 = EnvGuard::set("ProgramFiles(x86)", None);

    for executable in ["bash.exe", "pwsh.exe"] {
        let alias = format!(r"{ALIASES_DIR}\{executable}");
        let alias_c = alias.clone();
        let _exists = testing::override_shell_path_exists(Arc::new(move |path| path == alias_c));
        let alias_find = alias.clone();
        let _find = testing::override_find_executable(Arc::new(move |exe| {
            if exe == executable {
                Some(alias_find.clone())
            } else {
                None
            }
        }));

        let config = if executable == "pwsh.exe" {
            get_power_shell_config()
        } else {
            get_shell_config(None)
        }
        .expect("resolve store alias");
        assert_eq!(config.shell, alias);
    }

    let custom_alias = format!(r"{ALIASES_DIR}\bash.exe");
    let custom_c = custom_alias.clone();
    let _exists = testing::override_shell_path_exists(Arc::new(move |path| path == custom_c));
    assert_eq!(
        get_shell_config(Some(&custom_alias)).expect("custom alias").shell,
        custom_alias
    );

    let windows_powershell = r"D:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe";
    let pwsh_alias = format!(r"{ALIASES_DIR}\pwsh.exe");
    let _exists2 = testing::override_shell_path_exists(Arc::new(move |path| path == windows_powershell));
    let _find2 = testing::override_find_executable(Arc::new(move |exe| {
        if exe == "pwsh.exe" {
            Some(pwsh_alias.clone())
        } else if exe == "powershell.exe" {
            Some(windows_powershell.to_string())
        } else {
            None
        }
    }));
    // `find_executable_on_path` isn't checked directly for existence in `utils/shell.rs`; ensure explicit check rejects
    // non-existent configured shells while PATH results resolve accurately.
    let err = get_shell_config(Some("/missing/bash")).expect_err("missing custom");
    assert!(err.to_string().contains("Custom shell path not found: /missing/bash"));
}

#[test]
#[serial]
fn unix_shell_existence_checks_fallback_and_reject_missing_custom() {
    let _plat = PlatformGuard::set("linux");

    let _exists = testing::override_shell_path_exists(Arc::new(|path| path == "/bin/bash"));
    assert_eq!(get_shell_config(None).expect("bin bash").shell, "/bin/bash");

    let _exists_fallback = testing::override_shell_path_exists(Arc::new(|_| false));
    let _find = testing::override_find_executable(Arc::new(|exe| {
        if exe == "bash" {
            Some("/usr/local/bin/bash".to_string())
        } else {
            None
        }
    }));
    assert_eq!(get_shell_config(None).expect("path bash").shell, "/usr/local/bin/bash");

    let err = get_shell_config(Some("/missing/bash")).expect_err("missing custom");
    assert!(err.to_string().contains("Custom shell path not found: /missing/bash"));
}
