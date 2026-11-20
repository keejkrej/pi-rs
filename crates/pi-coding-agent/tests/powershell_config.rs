//! Port of packages/coding-agent/test/powershell-config.test.ts

use std::sync::Arc;

use pi_coding_agent::utils::shell::{POWERSHELL_ARGS, get_power_shell_config, testing};
use pi_js::env::testing::{EnvGuard, PlatformGuard};
use serial_test::serial;

const PWSH7: &str = r"D:\Applications\PowerShell\7\pwsh.exe";
const WINDOWS_POWERSHELL: &str = r"E:\WinNT\System32\WindowsPowerShell\v1.0\powershell.exe";

fn expected_args() -> Vec<String> {
    POWERSHELL_ARGS.iter().map(|s| (*s).to_string()).collect()
}

// #9490: discover PowerShell outside PATH without assuming a drive or Windows directory name.
#[test]
#[serial]
fn prefers_powershell_7_on_path_over_known_installation_locations() {
    let _plat = PlatformGuard::set("win32");
    let _pf = EnvGuard::set("ProgramFiles", Some(r"D:\Applications"));
    let _sr = EnvGuard::set("SystemRoot", Some(r"E:\WinNT"));

    let shell = r"E:\Tools\pwsh.exe";
    let _find = testing::override_find_executable(Arc::new(move |exe| {
        if exe == "pwsh.exe" {
            Some(shell.to_string())
        } else {
            None
        }
    }));
    let _exists = testing::override_shell_path_exists(Arc::new(|_| true));

    let config = get_power_shell_config().expect("pwsh on PATH");
    assert_eq!(config.shell, shell);
    assert_eq!(config.args, expected_args());
}

#[test]
#[serial]
fn finds_powershell_7_under_program_files_before_windows_powershell_on_path() {
    let _plat = PlatformGuard::set("win32");
    let _pf = EnvGuard::set("ProgramFiles", Some(r"D:\Applications"));
    let _sr = EnvGuard::set("SystemRoot", Some(r"E:\WinNT"));

    let _find = testing::override_find_executable(Arc::new(|exe| {
        if exe == "powershell.exe" {
            Some(WINDOWS_POWERSHELL.to_string())
        } else {
            None
        }
    }));
    let _exists = testing::override_shell_path_exists(Arc::new(|p| p == PWSH7 || p == WINDOWS_POWERSHELL));

    let config = get_power_shell_config().expect("pwsh7 under ProgramFiles");
    assert_eq!(config.shell, PWSH7);
    assert_eq!(config.args, expected_args());
}

#[test]
#[serial]
fn prefers_windows_powershell_on_path_over_system_installation() {
    let _plat = PlatformGuard::set("win32");
    let _pf = EnvGuard::set("ProgramFiles", Some(r"D:\Applications"));
    let _sr = EnvGuard::set("SystemRoot", Some(r"E:\WinNT"));

    let shell = r"E:\Tools\powershell.exe";
    let _find = testing::override_find_executable(Arc::new(move |exe| {
        if exe == "powershell.exe" {
            Some(shell.to_string())
        } else {
            None
        }
    }));
    let _exists = testing::override_shell_path_exists(Arc::new(move |p| p == shell || p == WINDOWS_POWERSHELL));

    let config = get_power_shell_config().expect("windows powershell on PATH");
    assert_eq!(config.shell, shell);
    assert_eq!(config.args, expected_args());
}

#[test]
#[serial]
fn finds_windows_powershell_under_system_root_when_path_lookup_is_unavailable() {
    let _plat = PlatformGuard::set("win32");
    let _pf = EnvGuard::set("ProgramFiles", Some(r"D:\Applications"));
    let _sr = EnvGuard::set("SystemRoot", Some(r"E:\WinNT"));

    let _find = testing::override_find_executable(Arc::new(|_| None));
    let _exists = testing::override_shell_path_exists(Arc::new(|p| p == WINDOWS_POWERSHELL));

    let config = get_power_shell_config().expect("windows powershell under SystemRoot");
    assert_eq!(config.shell, WINDOWS_POWERSHELL);
    assert_eq!(config.args, expected_args());
}

#[test]
#[serial]
fn reports_error_when_no_powershell_installation_exists() {
    let _plat = PlatformGuard::set("win32");
    let _pf = EnvGuard::set("ProgramFiles", Some(r"D:\Applications"));
    let _sr = EnvGuard::set("SystemRoot", Some(r"E:\WinNT"));

    let _find = testing::override_find_executable(Arc::new(|_| None));
    let _exists = testing::override_shell_path_exists(Arc::new(|_| false));

    let err = get_power_shell_config().expect_err("no powershell");
    assert!(err.to_string().contains("No PowerShell executable found"));
}
