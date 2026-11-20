//! Port of packages/coding-agent/test/exec.test.ts (+ exec-utf8-chunks)

use pi_coding_agent::core::exec::{ExecOptions, ExecResult, exec_command};

fn str_args(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

// #11085
#[tokio::test]
async fn reports_spawn_errors_in_stderr() {
    let result = exec_command("pi-command-that-does-not-exist-11085", &[], ".", None).await;
    assert_eq!(result.code, 1);
    assert!(!result.killed);
    assert!(!result.truncated);
    assert_eq!(result.stdout, "");
    assert!(!result.stderr.is_empty());
}

#[tokio::test]
async fn keeps_stderr_empty_when_command_exits_silently() {
    let result = exec_command("sh", &str_args(&["-c", "exit 1"]), ".", None).await;
    assert_eq!(
        result,
        ExecResult {
            stdout: String::new(),
            stderr: String::new(),
            code: 1,
            killed: false,
            truncated: false,
        }
    );
}

#[tokio::test]
async fn truncates_all_output_when_max_buffer_is_zero() {
    let result = exec_command(
        "sh",
        &str_args(&["-c", "printf x"]),
        ".",
        Some(ExecOptions {
            max_buffer: Some(0),
            ..Default::default()
        }),
    )
    .await;
    assert_eq!(result.stdout, "");
    assert_eq!(result.stderr, "");
    assert!(result.truncated);
    assert!(result.killed);
    assert_ne!(result.code, 0);
}

#[tokio::test]
async fn caps_combined_output_at_max_buffer_and_terminates() {
    let script = r#"
i=0
while [ "$i" -lt 600 ]; do printf "e" >&2; printf "o"; i=$((i+1)); done
while :; do printf "xxxxxxxxxx"; done
"#;
    let result = exec_command(
        "sh",
        &str_args(&["-c", script]),
        ".",
        Some(ExecOptions {
            max_buffer: Some(1000),
            ..Default::default()
        }),
    )
    .await;
    assert!(result.truncated);
    assert!(result.killed);
    assert_ne!(result.code, 0);
    assert!(result.stdout.len() + result.stderr.len() <= 1000);
    assert_eq!(result.stdout.len() + result.stderr.len(), 1000);
}

// #11360: a byte cap must not turn a partial UTF-8 character into replacement output.
#[tokio::test]
async fn keeps_complete_utf8_characters_capped_without_replacement_char() {
    let cases = [
        ("stdout", "\\342\\202\\254", 1usize, ""),
        ("stderr", "\\342\\202\\254", 1usize, ""),
        ("stdout", "a\\342\\202\\254\\360\\237\\231\\202", 5usize, "a€"),
        ("stderr", "a\\342\\202\\254\\360\\237\\231\\202", 5usize, "a€"),
    ];
    for (stream, octals, cap, expected) in cases {
        let cmd = if stream == "stderr" {
            format!("printf '{octals}' >&2")
        } else {
            format!("printf '{octals}'")
        };
        let result = exec_command(
            "sh",
            &str_args(&["-c", &cmd]),
            ".",
            Some(ExecOptions {
                max_buffer: Some(cap),
                ..Default::default()
            }),
        )
        .await;
        let actual = if stream == "stdout" {
            &result.stdout
        } else {
            &result.stderr
        };
        assert_eq!(actual, expected, "stream={stream} cap={cap}");
        assert!(result.truncated);
        assert!(result.killed);
        assert_ne!(result.code, 0);
        assert!(result.stdout.len() + result.stderr.len() <= cap);
    }
}

#[tokio::test]
async fn flushes_incomplete_utf8_at_normal_eof_when_not_truncated() {
    let result = exec_command(
        "sh",
        &str_args(&["-c", "printf '\\342'; printf '\\360' >&2"]),
        ".",
        None,
    )
    .await;
    assert_eq!(
        result,
        ExecResult {
            stdout: "\u{FFFD}".to_string(),
            stderr: "\u{FFFD}".to_string(),
            code: 0,
            killed: false,
            truncated: false,
        }
    );
}

#[tokio::test]
async fn discards_pending_utf8_bytes_when_split_across_chunks_hits_cap() {
    for stream in ["stdout", "stderr"] {
        let cmd = if stream == "stdout" {
            "printf 'a\\342'; sleep 0.05; printf '\\342' >&2; sleep 0.05; printf '\\202\\254'"
        } else {
            "printf 'a\\342' >&2; sleep 0.05; printf '\\342'; sleep 0.05; printf '\\202\\254' >&2"
        };
        let result = exec_command(
            "sh",
            &str_args(&["-c", cmd]),
            ".",
            Some(ExecOptions {
                max_buffer: Some(3),
                ..Default::default()
            }),
        )
        .await;
        assert_eq!(result.stdout, if stream == "stdout" { "a" } else { "" });
        assert_eq!(result.stderr, if stream == "stderr" { "a" } else { "" });
        assert!(result.truncated);
        assert!(result.killed);
        assert_ne!(result.code, 0);
        assert!(result.stdout.len() + result.stderr.len() <= 3);
    }
}

#[tokio::test]
async fn leaves_output_up_to_max_buffer_unchanged() {
    let expected = "a".repeat(1000);
    let result = exec_command(
        "sh",
        &str_args(&["-c", &format!("printf '%s' '{expected}'")]),
        ".",
        Some(ExecOptions {
            max_buffer: Some(1000),
            ..Default::default()
        }),
    )
    .await;
    assert_eq!(
        result,
        ExecResult {
            stdout: expected,
            stderr: String::new(),
            code: 0,
            killed: false,
            truncated: false,
        }
    );
}

#[cfg(unix)]
#[tokio::test]
async fn returns_when_shell_child_keeps_writing_after_shell_is_terminated() {
    let result = exec_command(
        "sh",
        &str_args(&["-c", "yes; echo done"]),
        ".",
        Some(ExecOptions {
            max_buffer: Some(1000),
            ..Default::default()
        }),
    )
    .await;
    assert!(result.killed);
    assert!(result.truncated);
    assert_eq!(result.stdout, "y\n".repeat(500));
}

#[cfg(unix)]
#[tokio::test]
async fn force_kills_command_that_ignores_sigterm() {
    let result = exec_command(
        "sh",
        &str_args(&["-c", "trap '' TERM; printf xx; while :; do sleep 1; done"]),
        ".",
        Some(ExecOptions {
            max_buffer: Some(1),
            ..Default::default()
        }),
    )
    .await;
    assert_eq!(result.stdout, "x");
    assert!(result.killed);
    assert!(result.truncated);
    assert_ne!(result.code, 0);
}
