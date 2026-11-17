//! Port of packages/coding-agent/test/tool-system-prompt-contributions.test.ts

use pi_coding_agent::core::tools::bash::{
    BashToolOptions, bash_tool_system_prompt_contribution, create_bash_tool_definition,
};
use pi_coding_agent::core::tools::edit::{create_edit_tool_definition, edit_tool_system_prompt_contribution};
use pi_coding_agent::core::tools::find::{create_find_tool_definition, find_tool_system_prompt_contribution};
use pi_coding_agent::core::tools::grep::{create_grep_tool_definition, grep_tool_system_prompt_contribution};
use pi_coding_agent::core::tools::ls::{create_ls_tool_definition, ls_tool_system_prompt_contribution};
use pi_coding_agent::core::tools::powershell::{
    PowerShellToolOptions, create_power_shell_tool_definition, powershell_tool_system_prompt_contribution,
};
use pi_coding_agent::core::tools::read::{create_read_tool_definition, read_tool_system_prompt_contribution};
use pi_coding_agent::core::tools::write::{create_write_tool_definition, write_tool_system_prompt_contribution};

#[test]
fn keeps_builtin_tool_definitions_aligned_with_contributions() {
    let read_def = create_read_tool_definition("/workspace", None);
    let read_contrib = read_tool_system_prompt_contribution();
    assert_eq!(
        read_def.prompt_snippet.as_deref(),
        Some(read_contrib.prompt_snippet.as_str())
    );
    assert_eq!(
        read_def.prompt_guidelines.unwrap_or_default(),
        read_contrib.prompt_guidelines
    );

    let bash_def = create_bash_tool_definition("/workspace", None);
    let bash_contrib = bash_tool_system_prompt_contribution(None);
    assert_eq!(
        bash_def.prompt_snippet.as_deref(),
        Some(bash_contrib.prompt_snippet.as_str())
    );
    assert_eq!(bash_def.prompt_guidelines, bash_contrib.prompt_guidelines);

    let ps_def = create_power_shell_tool_definition("/workspace", None);
    let ps_contrib = powershell_tool_system_prompt_contribution(None);
    assert_eq!(
        ps_def.prompt_snippet.as_deref(),
        Some(ps_contrib.prompt_snippet.as_str())
    );
    assert_eq!(ps_def.prompt_guidelines, ps_contrib.prompt_guidelines);

    let edit_def = create_edit_tool_definition("/workspace", None);
    let edit_contrib = edit_tool_system_prompt_contribution();
    assert_eq!(
        edit_def.prompt_snippet.as_deref(),
        Some(edit_contrib.prompt_snippet.as_str())
    );
    assert_eq!(
        edit_def.prompt_guidelines.unwrap_or_default(),
        edit_contrib.prompt_guidelines
    );

    let write_def = create_write_tool_definition("/workspace", None);
    let write_contrib = write_tool_system_prompt_contribution();
    assert_eq!(
        write_def.prompt_snippet.as_deref(),
        Some(write_contrib.prompt_snippet.as_str())
    );
    assert_eq!(
        write_def.prompt_guidelines.unwrap_or_default(),
        write_contrib.prompt_guidelines
    );

    let grep_def = create_grep_tool_definition("/workspace", None);
    let grep_contrib = grep_tool_system_prompt_contribution();
    assert_eq!(
        grep_def.prompt_snippet.as_deref(),
        Some(grep_contrib.prompt_snippet.as_str())
    );
    assert_eq!(
        grep_def.prompt_guidelines.unwrap_or_default(),
        grep_contrib.prompt_guidelines
    );

    let find_def = create_find_tool_definition("/workspace", None);
    let find_contrib = find_tool_system_prompt_contribution();
    assert_eq!(
        find_def.prompt_snippet.as_deref(),
        Some(find_contrib.prompt_snippet.as_str())
    );
    assert_eq!(
        find_def.prompt_guidelines.unwrap_or_default(),
        find_contrib.prompt_guidelines
    );

    let ls_def = create_ls_tool_definition("/workspace", None);
    let ls_contrib = ls_tool_system_prompt_contribution();
    assert_eq!(
        ls_def.prompt_snippet.as_deref(),
        Some(ls_contrib.prompt_snippet.as_str())
    );
    assert_eq!(
        ls_def.prompt_guidelines.unwrap_or_default(),
        ls_contrib.prompt_guidelines
    );
}

#[test]
fn keeps_shell_session_environment_guidance_conditional() {
    let bash_def = create_bash_tool_definition(
        "/workspace",
        Some(BashToolOptions {
            expose_session_environment: Some(false),
            ..Default::default()
        }),
    );
    assert!(bash_def.prompt_guidelines.is_none());

    let ps_def = create_power_shell_tool_definition(
        "/workspace",
        Some(PowerShellToolOptions {
            expose_session_environment: Some(false),
            ..Default::default()
        }),
    );
    assert!(ps_def.prompt_guidelines.is_none());
}
