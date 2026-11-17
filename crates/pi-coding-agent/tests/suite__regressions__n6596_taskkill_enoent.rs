//! Port of packages/coding-agent/test/suite/regressions/6596-taskkill-enoent.test.ts

use std::sync::{Arc, Mutex};

use pi_coding_agent::utils::shell::{kill_process_tree, testing::override_taskkill_spawn};
use pi_js::env::testing::{EnvGuard, PlatformGuard};
use serial_test::serial;

#[test]
#[serial]
fn uses_system32_taskkill_and_consumes_spawn_enoent_errors() {
    let cases = [
        (Some("D:\\CustomWindows"), "D:\\CustomWindows"),
        (None, "C:\\Windows"),
        (Some(""), "C:\\Windows"),
    ];

    for (system_root, expected_root) in cases {
        let _plat = PlatformGuard::set("win32");
        let _env = EnvGuard::set("SystemRoot", system_root);

        let calls: Arc<Mutex<Vec<(String, Vec<String>)>>> = Arc::new(Mutex::new(Vec::new()));
        let calls_cb = Arc::clone(&calls);
        let _spawn_guard = override_taskkill_spawn(Arc::new(move |cmd, args| {
            calls_cb
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push((cmd.to_owned(), args.to_vec()));
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "spawn taskkill ENOENT",
            ))
        }));

        kill_process_tree(1234);

        let recorded = calls.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].0, format!("{expected_root}\\System32\\taskkill.exe"));
        assert_eq!(recorded[0].1, vec!["/F", "/T", "/PID", "1234"]);
    }
}
