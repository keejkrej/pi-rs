//! Port of packages/durable/test/env-node-spill.test.ts

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use pi_chord::context::BACKGROUND_CONTEXT;
use pi_durable::env::node::{NodeExecutionEnv, SpillTestConfig};
use pi_durable::env::{FileSystem, Shell, ShellExecOptions, ShellSpillOptions, get_or_throw};
use pretty_assertions::assert_eq;

#[cfg(unix)]
#[tokio::test]
async fn keeps_inherited_stdio_open_past_exit_grace_period_while_spill_write_pending() {
    let dir = tempfile::Builder::new()
        .prefix("pi-durable-env-spill-")
        .tempdir()
        .expect("tempdir");
    let root = dir.path().to_string_lossy().into_owned();
    let rejected_writes = Arc::new(AtomicUsize::new(0));
    let env = NodeExecutionEnv::new(&root).with_spill_test_config(SpillTestConfig {
        high_water_mark: Some(1),
        write_delay_ms: Some(600),
        rejected_writes: Some(Arc::clone(&rejected_writes)),
    });

    let command = "printf '%020d' 0 | tr 0 a; (sleep 0.2; printf '%01000d' 0 | tr 0 b) &";
    let output = Arc::new(std::sync::Mutex::new(String::new()));
    let output_clone = Arc::clone(&output);

    let res = env
        .exec(
            command,
            Some(ShellExecOptions {
                spill: Some(ShellSpillOptions {
                    after_bytes: 10,
                    after_lines: 10,
                }),
                on_output: Some(Arc::new(move |text, _| {
                    output_clone
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push_str(text);
                    Ok(())
                })),
                ..Default::default()
            }),
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("exec succeeds");

    assert!(rejected_writes.load(Ordering::SeqCst) > 0);
    let collected = output.lock().unwrap_or_else(|e| e.into_inner()).clone();
    assert_eq!(collected.len(), 1020);

    let spill_path = res.spill_path.expect("spill path present");
    let expected = format!("{}{}", "a".repeat(20), "b".repeat(1000));
    let actual = get_or_throw(env.read_text_file(&spill_path, &BACKGROUND_CONTEXT).await).unwrap();
    let _ = pi_js::fs::rm(&pi_js::path::dirname(&spill_path), true, true);
    assert_eq!(actual, expected);
}
