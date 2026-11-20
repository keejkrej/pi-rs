//! Port of packages/coding-agent/test/shell-kill-process-tree.test.ts

use std::sync::{Arc, Mutex};

use pi_coding_agent::utils::shell::{kill_process_tree, testing};
use pi_js::env::testing::PlatformGuard;
use serial_test::serial;

// Regression coverage for #11252: stop-before-kill is macOS-only and best-effort.
#[test]
#[serial]
fn stops_macos_process_group_before_killing_it_without_resuming_it() {
    let _plat = PlatformGuard::set("darwin");
    let calls = Arc::new(Mutex::new(Vec::<(i32, String)>::new()));
    let calls_c = Arc::clone(&calls);
    let _hook = testing::override_unix_signal(Arc::new(move |pid, sig| {
        calls_c.lock().unwrap().push((pid, sig.to_string()));
        Ok(())
    }));

    kill_process_tree(1234);
    assert_eq!(
        *calls.lock().unwrap(),
        vec![(-1234, "SIGSTOP".to_string()), (-1234, "SIGKILL".to_string())]
    );
}

#[test]
#[serial]
fn still_kills_group_when_stopping_fails() {
    let _plat = PlatformGuard::set("darwin");
    let calls = Arc::new(Mutex::new(Vec::<(i32, String)>::new()));
    let calls_c = Arc::clone(&calls);
    let _hook = testing::override_unix_signal(Arc::new(move |pid, sig| {
        calls_c.lock().unwrap().push((pid, sig.to_string()));
        if sig == "SIGSTOP" { Err(()) } else { Ok(()) }
    }));

    kill_process_tree(1234);
    assert_eq!(
        *calls.lock().unwrap(),
        vec![(-1234, "SIGSTOP".to_string()), (-1234, "SIGKILL".to_string())]
    );
}

#[test]
#[serial]
fn falls_back_to_killing_shell_when_group_kill_fails() {
    let _plat = PlatformGuard::set("darwin");
    let calls = Arc::new(Mutex::new(Vec::<(i32, String)>::new()));
    let calls_c = Arc::clone(&calls);
    let _hook = testing::override_unix_signal(Arc::new(move |pid, sig| {
        calls_c.lock().unwrap().push((pid, sig.to_string()));
        if pid < 0 && sig == "SIGKILL" { Err(()) } else { Ok(()) }
    }));

    kill_process_tree(1234);
    assert_eq!(
        *calls.lock().unwrap(),
        vec![
            (-1234, "SIGSTOP".to_string()),
            (-1234, "SIGKILL".to_string()),
            (1234, "SIGKILL".to_string()),
        ]
    );
}

#[test]
#[serial]
fn tolerates_failure_of_both_group_signals_and_fallback() {
    let _plat = PlatformGuard::set("darwin");
    let calls = Arc::new(Mutex::new(Vec::<(i32, String)>::new()));
    let calls_c = Arc::clone(&calls);
    let _hook = testing::override_unix_signal(Arc::new(move |pid, sig| {
        calls_c.lock().unwrap().push((pid, sig.to_string()));
        Err(())
    }));

    kill_process_tree(1234);
    assert_eq!(
        *calls.lock().unwrap(),
        vec![
            (-1234, "SIGSTOP".to_string()),
            (-1234, "SIGKILL".to_string()),
            (1234, "SIGKILL".to_string()),
        ]
    );
}

#[test]
#[serial]
fn keeps_linux_group_termination_unchanged() {
    let _plat = PlatformGuard::set("linux");
    let calls = Arc::new(Mutex::new(Vec::<(i32, String)>::new()));
    let calls_c = Arc::clone(&calls);
    let _hook = testing::override_unix_signal(Arc::new(move |pid, sig| {
        calls_c.lock().unwrap().push((pid, sig.to_string()));
        Ok(())
    }));

    kill_process_tree(1234);
    assert_eq!(*calls.lock().unwrap(), vec![(-1234, "SIGKILL".to_string())]);
}
