//! Port of packages/tui/test/exit-restore.test.ts and test/fixtures/exit-without-stop.ts

#[cfg(unix)]
mod unix_exit_restore {
    use std::process::Command;
    use std::sync::Arc;

    use pi_tui::terminal::ProcessTerminal;
    use pi_tui::tui::TuiStopOptions;
    use pi_tui::tui_alt_screen::{TuiAltScreen, TuiAltScreenOptions};

    fn run_fixture(mode: &str) -> String {
        let exe = std::env::current_exe().expect("current_exe");
        let output = Command::new(exe)
            .args(["--exact", "unix_exit_restore::child_exit_fixture", "--nocapture"])
            .env("PI_TUI_EXIT_RESTORE_CHILD", mode)
            .env("TERM", "xterm-256color")
            .env_remove("TMUX")
            .env_remove("STY")
            .env_remove("ZELLIJ")
            .output()
            .expect("spawn exit_restore child");
        assert!(
            output.status.success(),
            "child failed: status={:?}, stderr={}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn count_occurrences(haystack: &str, needle: &str) -> usize {
        haystack.matches(needle).count()
    }

    // Regression for #11170: exit paths that skip stop() left mouse tracking, focus reporting,
    // the Kitty keyboard protocol, bracketed paste, and the alternate screen enabled.
    #[test]
    fn restores_terminal_modes_when_the_process_exits_without_stop() {
        let output = run_fixture("exit");
        for sequence in [
            "\x1b[?1006l",
            "\x1b[?1004l",
            "\x1b[?1003l",
            "\x1b[?1000l",
            "\x1b[<u",
            "\x1b[?2004l",
        ] {
            assert_eq!(
                count_occurrences(&output, sequence),
                1,
                "sequence={sequence:?}, output={output:?}"
            );
        }
        assert!(
            output.find("\x1b[<u") < output.find("\x1b[?1049l"),
            "Kitty pop must precede leaving alt screen"
        );
        assert!(
            output.ends_with("\x1b[?1049l\x1b[?25h"),
            "unexpected ending in output={output:?}"
        );
    }

    #[test]
    fn does_not_restore_twice_after_a_normal_stop() {
        let output = run_fixture("stop");
        for sequence in ["\x1b[?1006l", "\x1b[?1004l", "\x1b[<u", "\x1b[?2004l", "\x1b[?1049l"] {
            assert_eq!(
                count_occurrences(&output, sequence),
                1,
                "sequence={sequence:?}, output={output:?}"
            );
        }
    }

    #[test]
    fn child_exit_fixture() {
        let Ok(mode) = std::env::var("PI_TUI_EXIT_RESTORE_CHILD") else {
            return;
        };
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        rt.block_on(async move {
            let terminal = Arc::new(ProcessTerminal::new());
            let tui = TuiAltScreen::new(
                terminal,
                Some(false),
                None,
                Some(TuiAltScreenOptions {
                    mouse: Some(true),
                    ..Default::default()
                }),
            );
            tui.start();
            if mode == "stop" {
                tui.stop(Some(TuiStopOptions {
                    preserve_screen: Some(true),
                }));
            }
            std::process::exit(0);
        });
    }
}
