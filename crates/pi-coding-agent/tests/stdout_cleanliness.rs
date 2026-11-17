//! Port of packages/coding-agent/test/stdout-cleanliness.test.ts

use std::process::Command;

use pi_coding_agent::config::ENV_AGENT_DIR;

fn run_cli(args: &[&str]) -> (String, String, Option<i32>) {
    let dir = tempfile::tempdir().unwrap();
    let agent_dir = dir.path().join("agent");
    let project_dir = dir.path().join("project");
    let project_cfg = project_dir.join(".pi");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::create_dir_all(&project_cfg).unwrap();

    let fake_npm = dir.path().join("fake-npm.sh");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(
            &fake_npm,
            "#!/bin/sh\necho \"changed 1 package in 471ms\"\necho \"found 0 vulnerabilities\"\nexit 0\n",
        )
        .unwrap();
        let mut perms = std::fs::metadata(&fake_npm).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&fake_npm, perms).unwrap();
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&fake_npm, "@echo off\r\necho changed 1 package in 471ms\r\n").unwrap();
    }

    let settings = serde_json::json!({
        "packages": ["npm:fake-package"],
        "npmCommand": [fake_npm.to_string_lossy().as_ref()]
    });
    std::fs::write(
        project_cfg.join("settings.json"),
        pi_js::json::stringify_pretty(&settings, "  "),
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_pi"))
        .args(args)
        .current_dir(&project_dir)
        .env(&*ENV_AGENT_DIR, &agent_dir)
        .output()
        .unwrap();

    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.code(),
    )
}

#[test]
fn prints_plain_help_to_stdout_when_stdout_is_redirected() {
    let (stdout, stderr, code) = run_cli(&["--help"]);
    assert_eq!(code, Some(0));
    assert!(stdout.contains("Usage:"));
    assert!(!stderr.contains("Usage:"));
    assert!(!stderr.contains("changed 1 package in 471ms"));
    assert!(!stderr.contains("found 0 vulnerabilities"));
}

#[test]
fn keeps_stdout_empty_for_mode_json_help_while_routing_trusted_chatter_to_stderr() {
    let (stdout, stderr, code) = run_cli(&["--mode", "json", "--help", "--approve"]);
    assert_eq!(code, Some(0));
    assert_eq!(stdout, "");
    assert!(stderr.contains("changed 1 package in 471ms"));
    assert!(stderr.contains("found 0 vulnerabilities"));
    assert!(stderr.contains("Usage:"));
}
