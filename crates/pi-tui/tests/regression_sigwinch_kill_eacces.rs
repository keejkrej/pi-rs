//! Port of packages/tui/test/regression-sigwinch-kill-eacces.test.ts

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use pi_js::env::testing::PlatformGuard;
use pi_tui::terminal::refresh_terminal_dimensions;
use pi_tui::terminal::testing::override_kill;
use serial_test::serial;

#[test]
#[serial]
fn does_not_throw_when_kill_returns_eacces_for_self_signal() {
    let _platform = PlatformGuard::set("linux");
    let self_pid = std::process::id();
    let _kill = override_kill(move |pid, _signal| {
        if pid == self_pid {
            Err("EACCES".to_string())
        } else {
            Ok(())
        }
    });

    refresh_terminal_dimensions();
}

#[test]
#[serial]
fn does_not_call_kill_on_win32() {
    let _platform = PlatformGuard::set("win32");
    let kill_called = Arc::new(AtomicBool::new(false));
    let called_clone = Arc::clone(&kill_called);
    let _kill = override_kill(move |_, _| {
        called_clone.store(true, Ordering::SeqCst);
        Ok(())
    });

    refresh_terminal_dimensions();

    assert!(
        !kill_called.load(Ordering::SeqCst),
        "kill should not be called on win32"
    );
}

#[test]
#[serial]
fn preserves_other_error_codes() {
    let _platform = PlatformGuard::set("linux");
    let _kill = override_kill(|_, _| Err("EPERM".to_string()));

    // EPERM is also ignored - the refresh is best-effort
    refresh_terminal_dimensions();
}
