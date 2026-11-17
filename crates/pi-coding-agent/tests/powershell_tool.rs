//! Port of packages/coding-agent/test/powershell-tool.test.ts

use pi_ai::types::UserContentBlock;
use pi_coding_agent::core::tools::powershell::{create_local_power_shell_operations, create_power_shell_tool};
use pi_coding_agent::utils::shell::{POWERSHELL_ARGS, get_power_shell_config};
use serde_json::json;

#[test]
fn uses_process_local_execution_policy_bypass() {
    assert_eq!(
        POWERSHELL_ARGS,
        [
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command"
        ]
    );
}

#[test]
fn reports_windows_only_on_non_windows() {
    if pi_js::env::platform() != "win32" {
        let err = get_power_shell_config().unwrap_err();
        assert!(err.to_string().contains("Windows"), "{err}");
    }
}

#[tokio::test]
async fn executes_powershell_commands_on_windows() {
    if pi_js::env::platform() != "win32" {
        return;
    }

    let config = get_power_shell_config().unwrap();
    assert_eq!(config.args, POWERSHELL_ARGS);

    let tool = create_power_shell_tool(&pi_js::env::cwd(), None);
    let result = (tool.execute)(
        "powershell-test".into(),
        json!({ "command": "Write-Output 'héllo €'; Get-ExecutionPolicy -Scope Process" }),
        None,
        None,
    )
    .await
    .unwrap();

    let output: String = result
        .content
        .iter()
        .filter_map(|c| match c {
            UserContentBlock::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(output.contains("héllo €"), "{output}");
    assert!(output.contains("Bypass"), "{output}");

    let _ops = create_local_power_shell_operations();
}
