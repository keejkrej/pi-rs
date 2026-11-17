//! Port of packages/coding-agent/test/clipboard.test.ts

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::{Arc, Mutex};

use pi_coding_agent::utils::clipboard::{copy_to_clipboard, read_clipboard_text};
use pi_js::Error;
use pi_js::console::testing::capture;
use pi_js::env::testing::{EnvGuard, PlatformGuard};
use pi_tui::native_platform::testing::{override_get_text, override_set_text};
use serial_test::serial;
use tempfile::TempDir;

fn write_exec(dir: &Path, name: &str, script: &str) {
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\n{script}\n")).expect("write shim");
    let mut perms = fs::metadata(&path).expect("metadata").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&path, perms).expect("chmod");
}

fn read_calls(log_path: &Path) -> Vec<String> {
    fs::read_to_string(log_path)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect()
}

fn base_clipboard_env(extra: &[(&str, Option<&str>)], path_dir: &Path) -> EnvGuard {
    let cur = pi_js::env::var("PATH").unwrap_or_default();
    let prefixed = format!("{}:{cur}", path_dir.display());
    let mut pairs: Vec<(&str, Option<&str>)> = vec![
        ("SSH_CONNECTION", None),
        ("SSH_CLIENT", None),
        ("MOSH_CONNECTION", None),
        ("WAYLAND_DISPLAY", None),
        ("DISPLAY", None),
        ("TERMUX_VERSION", None),
        ("WT_SESSION", None),
        ("WSL_DISTRO_NAME", None),
        ("WSL_INTEROP", None),
        ("WSLENV", None),
    ];
    for &(k, v) in extra {
        pairs.push((k, v));
    }
    let guard = EnvGuard::set_many(&pairs);
    pi_js::env::set_var("PATH", &prefixed);
    guard
}

mod read_clipboard_text_tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    #[serial]
    async fn awaits_native_clipboard_text_and_catches_errors() {
        let dir = TempDir::new().expect("tempdir");
        let _env = base_clipboard_env(&[], dir.path());
        let _plat = PlatformGuard::set("darwin");

        {
            let _g = override_get_text(|_| Ok(Some(Some("clipboard text".to_owned()))));
            assert_eq!(read_clipboard_text().await.as_deref(), Some("clipboard text"));
        }
        {
            let _g = override_get_text(|_| Err(Error::msg("clipboard unavailable")));
            assert_eq!(read_clipboard_text().await, None);
        }
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial]
    async fn wayland_and_x11_results_stop_fallback() {
        for (env_name, command, expected_calls) in [
            ("WAYLAND_DISPLAY", "wl-paste", vec!["wl-paste"]),
            ("DISPLAY", "xclip", vec!["xclip"]),
            ("DISPLAY", "xsel", vec!["xclip", "xsel"]),
        ] {
            for text in ["clipboard text", ""] {
                // Regression test for #7248: empty Wayland content must not fall through to stale X11.
                let dir = TempDir::new().expect("tempdir");
                let log = dir.path().join("calls.log");
                for tool in ["wl-paste", "xclip", "xsel"] {
                    let body = if tool == command {
                        format!(
                            "printf '%s\\n' \"{tool}\" >> '{}'\nprintf '%s' \"{text}\"",
                            log.display()
                        )
                    } else {
                        format!("printf '%s\\n' \"{tool}\" >> '{}'\nexit 1", log.display())
                    };
                    write_exec(dir.path(), tool, &body);
                }

                let _env = base_clipboard_env(&[("DISPLAY", Some(":0")), (env_name, Some("1"))], dir.path());
                let _plat = PlatformGuard::set("linux");
                let native_calls = Arc::new(Mutex::new(0usize));
                let nc = Arc::clone(&native_calls);
                let _native = override_get_text(move |_| {
                    *nc.lock().unwrap() += 1;
                    Ok(Some(Some("native fallback".to_owned())))
                });

                let expected = if text.is_empty() { None } else { Some(text.to_owned()) };
                assert_eq!(read_clipboard_text().await, expected);
                assert_eq!(read_calls(&log), expected_calls);
                assert_eq!(*native_calls.lock().unwrap(), 0);
            }
        }
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial]
    async fn uses_native_x11_after_command_failures() {
        for text in [Some("native text"), Some(""), None] {
            let dir = TempDir::new().expect("tempdir");
            let log = dir.path().join("calls.log");
            for tool in ["wl-paste", "xclip", "xsel"] {
                write_exec(
                    dir.path(),
                    tool,
                    &format!("printf '%s\\n' \"{tool}\" >> '{}'\nexit 1", log.display()),
                );
            }

            let _env = base_clipboard_env(
                &[("DISPLAY", Some(":0")), ("WAYLAND_DISPLAY", Some("wayland-0"))],
                dir.path(),
            );
            let _plat = PlatformGuard::set("linux");
            let ret = text.map(str::to_owned);
            let native_calls = Arc::new(Mutex::new(0usize));
            let nc = Arc::clone(&native_calls);
            let _native = override_get_text(move |_| {
                *nc.lock().unwrap() += 1;
                Ok(Some(ret.clone()))
            });

            let expected = text.filter(|s| !s.is_empty()).map(str::to_owned);
            assert_eq!(read_clipboard_text().await, expected);
            assert_eq!(read_calls(&log), vec!["wl-paste", "xclip", "xsel"]);
            assert_eq!(*native_calls.lock().unwrap(), 1);
        }
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial]
    async fn termux_reads_termux_clipboard_get_on_android() {
        for text in ["clipboard text", ""] {
            let dir = TempDir::new().expect("tempdir");
            let log = dir.path().join("calls.log");
            write_exec(
                dir.path(),
                "termux-clipboard-get",
                &format!("printf 'termux\\n' >> '{}'\nprintf '%s' \"{text}\"", log.display()),
            );

            // Note: Rust utils/clipboard.rs gates command reads on platform == "linux"
            let _env = base_clipboard_env(&[("TERMUX_VERSION", Some("0.119"))], dir.path());
            let _plat = PlatformGuard::set("linux");

            let expected = if text.is_empty() { None } else { Some(text.to_owned()) };
            assert_eq!(read_clipboard_text().await, expected);
            assert_eq!(read_calls(&log), vec!["termux"]);
        }
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial]
    async fn falls_back_to_x11_tools_when_wl_paste_is_unavailable() {
        let dir = TempDir::new().expect("tempdir");
        write_exec(dir.path(), "wl-paste", "exit 1");
        write_exec(dir.path(), "xclip", "printf 'X11 text'");

        let _env = base_clipboard_env(
            &[("WAYLAND_DISPLAY", Some("wayland-0")), ("DISPLAY", Some(":0"))],
            dir.path(),
        );
        let _plat = PlatformGuard::set("linux");
        let native_calls = Arc::new(Mutex::new(0usize));
        let nc = Arc::clone(&native_calls);
        let _native = override_get_text(move |_| {
            *nc.lock().unwrap() += 1;
            Ok(Some(None))
        });

        assert_eq!(read_clipboard_text().await.as_deref(), Some("X11 text"));
        assert_eq!(*native_calls.lock().unwrap(), 0);
    }
}

mod copy_to_clipboard_tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    #[serial]
    async fn local_native_success_skips_osc52_and_commands() {
        let dir = TempDir::new().expect("tempdir");
        let log = dir.path().join("calls.log");
        write_exec(
            dir.path(),
            "pbcopy",
            &format!("printf 'pbcopy\\n' >> '{}'", log.display()),
        );

        let _env = base_clipboard_env(&[], dir.path());
        let _plat = PlatformGuard::set("darwin");
        let cap = capture();
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let seen_c = Arc::clone(&seen);
        let _native = override_set_text(move |_, text| {
            seen_c.lock().unwrap().push(text.to_owned());
            Ok(())
        });

        copy_to_clipboard("hello").await.expect("copy ok");
        assert_eq!(*seen.lock().unwrap(), vec!["hello".to_owned()]);
        assert!(cap.stdout().is_empty());
        assert!(read_calls(&log).is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial]
    async fn linux_skips_native_writer() {
        let dir = TempDir::new().expect("tempdir");
        let log = dir.path().join("calls.log");
        let stdin_path = dir.path().join("stdin.txt");
        write_exec(
            dir.path(),
            "xclip",
            &format!(
                "printf 'xclip %s\\n' \"$*\" >> '{}'\ncat > '{}'",
                log.display(),
                stdin_path.display()
            ),
        );

        let _env = base_clipboard_env(&[("DISPLAY", Some(":0"))], dir.path());
        let _plat = PlatformGuard::set("linux");
        let native_calls = Arc::new(Mutex::new(0usize));
        let nc = Arc::clone(&native_calls);
        let _native = override_set_text(move |_, _| {
            *nc.lock().unwrap() += 1;
            Ok(())
        });

        copy_to_clipboard("hello").await.expect("copy ok");
        assert_eq!(*native_calls.lock().unwrap(), 0);
        assert_eq!(read_calls(&log), vec!["xclip -selection clipboard"]);
        assert_eq!(fs::read_to_string(&stdin_path).expect("stdin"), "hello");
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial]
    async fn ssh_session_writes_native_and_emits_osc52() {
        let dir = TempDir::new().expect("tempdir");
        let _env = base_clipboard_env(&[("SSH_CONNECTION", Some("client server"))], dir.path());
        let _plat = PlatformGuard::set("darwin");
        let cap = capture();
        let _native = override_set_text(|_, _| Ok(()));

        copy_to_clipboard("hello").await.expect("copy ok");
        assert_eq!(cap.stdout(), "\x1b]52;c;aGVsbG8=\x07");
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial]
    async fn rejected_native_write_falls_back_to_pbcopy() {
        let dir = TempDir::new().expect("tempdir");
        let log = dir.path().join("calls.log");
        let stdin_path = dir.path().join("pbcopy.txt");
        write_exec(
            dir.path(),
            "pbcopy",
            &format!(
                "printf 'pbcopy\\n' >> '{}'\ncat > '{}'",
                log.display(),
                stdin_path.display()
            ),
        );

        let _env = base_clipboard_env(&[], dir.path());
        let _plat = PlatformGuard::set("darwin");
        let cap = capture();
        let _native = override_set_text(|_, _| Err(Error::msg("native failed")));

        copy_to_clipboard("hello").await.expect("copy ok");
        assert_eq!(read_calls(&log), vec!["pbcopy"]);
        assert_eq!(fs::read_to_string(&stdin_path).expect("stdin"), "hello");
        assert!(cap.stdout().is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial]
    async fn tries_xclip_and_xsel_after_wl_copy_fails() {
        let dir = TempDir::new().expect("tempdir");
        let log = dir.path().join("calls.log");
        write_exec(
            dir.path(),
            "wl-copy",
            &format!("printf 'wl-copy\\n' >> '{}'\nexit 1", log.display()),
        );
        write_exec(
            dir.path(),
            "xclip",
            &format!("printf 'xclip\\n' >> '{}'\nexit 1", log.display()),
        );
        write_exec(
            dir.path(),
            "xsel",
            &format!("printf 'xsel\\n' >> '{}'\ncat >/dev/null", log.display()),
        );

        let _env = base_clipboard_env(
            &[("WAYLAND_DISPLAY", Some("wayland-0")), ("DISPLAY", Some(":0"))],
            dir.path(),
        );
        let _plat = PlatformGuard::set("linux");
        let cap = capture();

        copy_to_clipboard("hello").await.expect("copy ok");
        assert_eq!(read_calls(&log), vec!["wl-copy", "xclip", "xsel"]);
        assert!(cap.stdout().is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial]
    async fn local_linux_failure_does_not_report_unverified_osc52_as_success() {
        // Regression test for #9618.
        let dir = TempDir::new().expect("tempdir");
        let log = dir.path().join("calls.log");
        write_exec(
            dir.path(),
            "xclip",
            &format!("printf 'xclip\\n' >> '{}'\nexit 1", log.display()),
        );
        write_exec(
            dir.path(),
            "xsel",
            &format!("printf 'xsel\\n' >> '{}'\nexit 1", log.display()),
        );

        let _env = base_clipboard_env(&[("DISPLAY", Some(":0"))], dir.path());
        let _plat = PlatformGuard::set("linux");
        let cap = capture();

        let err = copy_to_clipboard("hello").await.expect_err("expected failure");
        assert!(
            err.to_string()
                .contains("Clipboard unavailable: install `xclip` or `xsel`, or check X11 access"),
            "{err}"
        );
        assert_eq!(read_calls(&log), vec!["xclip", "xsel"]);
        assert!(cap.stdout().is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial]
    async fn display_less_linux_falls_back_to_osc52() {
        let dir = TempDir::new().expect("tempdir");
        let _env = base_clipboard_env(&[], dir.path());
        let _plat = PlatformGuard::set("linux");
        let cap = capture();

        copy_to_clipboard("hello").await.expect("copy ok");
        assert_eq!(cap.stdout(), "\x1b]52;c;aGVsbG8=\x07");
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial]
    async fn wsl_without_display_writes_windows_clipboard_via_powershell() {
        let dir = TempDir::new().expect("tempdir");
        let log = dir.path().join("calls.log");
        let captured = dir.path().join("captured.txt");
        let wsl_tmp_record = dir.path().join("wsl_tmp.txt");
        let ps_script_log = dir.path().join("ps.script");

        write_exec(
            dir.path(),
            "wslpath",
            &format!(
                "printf 'wslpath\\n' >> '{}'\nprintf '%s' \"$2\" > '{}'\ncat \"$2\" > '{}'\nprintf '\\\\\\\\wsl.localhost\\\\Ubuntu\\\\tmp\\\\clip.txt\\n'",
                log.display(),
                wsl_tmp_record.display(),
                captured.display()
            ),
        );
        write_exec(
            dir.path(),
            "powershell.exe",
            &format!(
                "printf 'powershell.exe\\n' >> '{}'\nprintf '%s' \"$3\" > '{}'",
                log.display(),
                ps_script_log.display()
            ),
        );

        let _env = base_clipboard_env(&[("WSL_DISTRO_NAME", Some("Ubuntu"))], dir.path());
        let _plat = PlatformGuard::set("linux");
        let cap = capture();

        copy_to_clipboard("héllo").await.expect("copy ok");
        assert_eq!(read_calls(&log), vec!["wslpath", "powershell.exe"]);
        assert_eq!(fs::read_to_string(&captured).expect("written"), "héllo");
        let tmp_path = fs::read_to_string(&wsl_tmp_record).expect("tmp path");
        assert!(!Path::new(&tmp_path).exists());
        let script = fs::read_to_string(&ps_script_log).expect("ps script");
        assert!(script.contains("Set-Clipboard"), "{script}");
        assert!(
            script.contains("'\\\\wsl.localhost\\Ubuntu\\tmp\\clip.txt'"),
            "{script}"
        );
        assert!(cap.stdout().is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial]
    async fn wsl_falls_back_to_osc52_when_windows_interop_is_unavailable() {
        let dir = TempDir::new().expect("tempdir");
        let log = dir.path().join("calls.log");
        write_exec(
            dir.path(),
            "wslpath",
            &format!("printf 'wslpath\\n' >> '{}'\nexit 1", log.display()),
        );

        let _env = base_clipboard_env(&[("WSL_DISTRO_NAME", Some("Ubuntu"))], dir.path());
        let _plat = PlatformGuard::set("linux");
        let cap = capture();

        copy_to_clipboard("hello").await.expect("copy ok");
        assert_eq!(read_calls(&log), vec!["wslpath"]);
        assert_eq!(cap.stdout(), "\x1b]52;c;aGVsbG8=\x07");
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial]
    async fn wsl_in_windows_terminal_prefers_osc52_once_even_in_remote_session() {
        let dir = TempDir::new().expect("tempdir");
        let log = dir.path().join("calls.log");
        write_exec(
            dir.path(),
            "wslpath",
            &format!("printf 'wslpath\\n' >> '{}'", log.display()),
        );

        let _env = base_clipboard_env(
            &[
                ("WSL_DISTRO_NAME", Some("Ubuntu")),
                ("WT_SESSION", Some("session")),
                ("SSH_CONNECTION", Some("client server")),
            ],
            dir.path(),
        );
        let _plat = PlatformGuard::set("linux");
        let cap = capture();

        copy_to_clipboard("hello").await.expect("copy ok");
        assert!(read_calls(&log).is_empty());
        assert_eq!(cap.stdout(), "\x1b]52;c;aGVsbG8=\x07");
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial]
    async fn wsl_in_windows_terminal_uses_powershell_for_oversized_osc52() {
        let dir = TempDir::new().expect("tempdir");
        let log = dir.path().join("calls.log");
        write_exec(
            dir.path(),
            "wslpath",
            &format!("printf 'wslpath\\n' >> '{}'\nprintf 'C:\\\\clip.txt\\n'", log.display()),
        );
        write_exec(
            dir.path(),
            "powershell.exe",
            &format!("printf 'powershell.exe\\n' >> '{}'", log.display()),
        );

        let _env = base_clipboard_env(
            &[("WSL_DISTRO_NAME", Some("Ubuntu")), ("WT_SESSION", Some("session"))],
            dir.path(),
        );
        let _plat = PlatformGuard::set("linux");
        let cap = capture();

        copy_to_clipboard(&"x".repeat(80_000)).await.expect("copy ok");
        assert_eq!(read_calls(&log), vec!["wslpath", "powershell.exe"]);
        assert!(cap.stdout().is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial]
    async fn wsl_with_display_prefers_linux_clipboard_tools() {
        let dir = TempDir::new().expect("tempdir");
        let log = dir.path().join("calls.log");
        write_exec(
            dir.path(),
            "wl-copy",
            &format!("printf 'wl-copy\\n' >> '{}'\ncat >/dev/null", log.display()),
        );

        let _env = base_clipboard_env(
            &[
                ("WSL_DISTRO_NAME", Some("Ubuntu")),
                ("WAYLAND_DISPLAY", Some("wayland-0")),
            ],
            dir.path(),
        );
        let _plat = PlatformGuard::set("linux");
        let cap = capture();

        copy_to_clipboard("hello").await.expect("copy ok");
        assert_eq!(read_calls(&log), vec!["wl-copy"]);
        assert!(cap.stdout().is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial]
    async fn reports_wayland_clipboard_tool_instead_of_x11_fallback() {
        let dir = TempDir::new().expect("tempdir");
        let log = dir.path().join("calls.log");
        for tool in ["wl-copy", "xclip", "xsel"] {
            write_exec(
                dir.path(),
                tool,
                &format!("printf '{tool}\\n' >> '{}'\nexit 1", log.display()),
            );
        }

        let _env = base_clipboard_env(
            &[("WAYLAND_DISPLAY", Some("wayland-0")), ("DISPLAY", Some(":0"))],
            dir.path(),
        );
        let _plat = PlatformGuard::set("linux");

        let err = copy_to_clipboard("hello").await.expect_err("expected failure");
        assert!(
            err.to_string()
                .contains("Clipboard unavailable: install `wl-clipboard` (`wl-copy`) or check Wayland access"),
            "{err}"
        );
        assert_eq!(read_calls(&log), vec!["wl-copy", "xclip", "xsel"]);
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial]
    async fn termux_writes_through_termux_clipboard_set_and_reports_missing_package() {
        let dir = TempDir::new().expect("tempdir");
        let log = dir.path().join("calls.log");
        let stdin_path = dir.path().join("termux.txt");
        write_exec(
            dir.path(),
            "termux-clipboard-set",
            &format!(
                "printf 'termux\\n' >> '{}'\ncat > '{}'",
                log.display(),
                stdin_path.display()
            ),
        );

        let _env = base_clipboard_env(&[("TERMUX_VERSION", Some("0.119"))], dir.path());
        let _plat = PlatformGuard::set("linux");
        let cap = capture();

        copy_to_clipboard("hello").await.expect("termux copy ok");
        assert_eq!(read_calls(&log), vec!["termux"]);
        assert_eq!(fs::read_to_string(&stdin_path).expect("stdin"), "hello");
        assert!(cap.stdout().is_empty());

        write_exec(dir.path(), "termux-clipboard-set", "exit 1");
        let err = copy_to_clipboard("hello").await.expect_err("termux error");
        assert!(
            err.to_string()
                .contains("Clipboard unavailable: install the Termux:API app and `termux-api` package"),
            "{err}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial]
    async fn uses_osc52_when_native_and_command_fail_in_remote_and_rejects_oversized() {
        let dir = TempDir::new().expect("tempdir");
        write_exec(dir.path(), "pbcopy", "exit 1");

        let _env = base_clipboard_env(&[("SSH_CONNECTION", Some("client server"))], dir.path());
        let _plat = PlatformGuard::set("darwin");
        let _native = override_set_text(|_, _| Err(Error::msg("native failed")));

        {
            let cap = capture();
            copy_to_clipboard("hello").await.expect("osc52 ok");
            assert_eq!(cap.stdout(), "\x1b]52;c;aGVsbG8=\x07");
        }
        {
            let cap = capture();
            let err = copy_to_clipboard(&"x".repeat(80_000))
                .await
                .expect_err("oversized should error");
            assert!(
                err.to_string()
                    .contains("Clipboard unavailable: text exceeds the OSC 52 size limit"),
                "{err}"
            );
            assert!(cap.stdout().is_empty());
        }
    }
}
