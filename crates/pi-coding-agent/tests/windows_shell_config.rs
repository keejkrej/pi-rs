//! Port of packages/coding-agent/test/windows-shell-config.test.ts

use std::sync::Arc;

use pi_coding_agent::utils::shell::{CommandTransport, ShellConfig, get_shell_config, testing};
use pi_js::env::testing::{EnvGuard, PlatformGuard};
use serial_test::serial;

// #9490: identify legacy WSL launchers under the actual Windows directory.
#[test]
#[serial]
fn identifies_legacy_wsl_bash_launchers_across_system_roots() {
    let _plat = PlatformGuard::set("win32");
    let _exists = testing::override_shell_path_exists(Arc::new(|_| true));

    let cases: [(Option<&str>, &str, bool); 6] = [
        (Some(r"D:\WinNT\"), "d:/WINNT/System32/BASH.EXE", true),
        (Some(r"D:\WinNT"), r"D:\WinNT\Sysnative\bash.exe", true),
        (None, r"E:\Windows\System32\bash.exe", true),
        (Some(r"D:\WinNT"), r"D:\Git\bin\bash.exe", false),
        (Some(r"D:\WinNT"), r"E:\Windows\System32\bash.exe", false),
        (Some("WinNT"), r"C:\Windows\System32\bash.exe", false),
    ];

    for (system_root, shell, stdin) in cases {
        let _sr = EnvGuard::set("SystemRoot", system_root);
        let expected = if stdin {
            ShellConfig {
                shell: shell.to_string(),
                args: vec!["-s".to_string()],
                command_transport: Some(CommandTransport::Stdin),
            }
        } else {
            ShellConfig {
                shell: shell.to_string(),
                args: vec!["-c".to_string()],
                command_transport: None,
            }
        };
        assert_eq!(
            get_shell_config(Some(shell)).expect("get_shell_config"),
            expected,
            "system_root={system_root:?}, shell={shell}"
        );
    }
}
