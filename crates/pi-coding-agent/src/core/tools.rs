//! Port of packages/coding-agent/src/core/tools/index.ts

// @generated-mods begin (scaffold-owned, do not edit)
pub mod bash;
pub mod edit;
pub mod edit_diff;
pub mod file_mutation_queue;
pub mod find;
pub mod grep;
pub mod ls;
pub mod output_accumulator;
pub mod path_utils;
pub mod powershell;
pub mod read;
pub mod render_utils;
pub mod renderers;
pub mod tool_definition_wrapper;
pub mod truncate;
pub mod write;
// @generated-mods end

use std::sync::LazyLock;

use indexmap::{IndexMap, IndexSet};
use pi_agent_core::types::AgentTool;
use serde::{Deserialize, Serialize};

use crate::core::extensions::types::ToolDefinition;

pub type Tool = AgentTool;
pub type ToolDef = ToolDefinition;

pub use self::bash::{
    BashChunkCallback, BashExecOptions, BashExecResult, BashOperations, BashSpawnContext, BashSpawnHook,
    BashToolDetails, BashToolInput, BashToolOptions, BashToolOutput, DEFAULT_SHELL_MAX_OUTPUT_BYTES,
    bash_tool_system_prompt_contribution, create_bash_tool, create_bash_tool_definition, create_local_bash_operations,
};
pub use self::edit::{
    EditOperations, EditReplacement, EditToolDetails, EditToolInput, EditToolOptions, create_edit_tool,
    create_edit_tool_definition, create_local_edit_operations, edit_tool_system_prompt_contribution,
    prepare_edit_arguments, validate_edit_input,
};
pub use self::file_mutation_queue::with_file_mutation_queue;
pub use self::find::{
    FindGlobOptions, FindOperations, FindPathMode, FindToolDetails, FindToolInput, FindToolOptions, create_find_tool,
    create_find_tool_definition, create_local_find_operations, find_tool_system_prompt_contribution,
    relativize_find_result_path,
};
pub use self::grep::{
    GrepOperations, GrepToolDetails, GrepToolInput, GrepToolOptions, create_grep_tool, create_grep_tool_definition,
    create_local_grep_operations, grep_tool_system_prompt_contribution,
};
pub use self::ls::{
    LsOperations, LsStat, LsToolDetails, LsToolInput, LsToolOptions, create_local_ls_operations, create_ls_tool,
    create_ls_tool_definition, ls_tool_system_prompt_contribution,
};
pub use self::powershell::{
    PowerShellOperations, PowerShellSpawnContext, PowerShellSpawnHook, PowerShellToolDetails, PowerShellToolInput,
    PowerShellToolOptions, create_local_power_shell_operations, create_power_shell_tool,
    create_power_shell_tool_definition, powershell_tool_system_prompt_contribution,
};
pub use self::read::{
    ReadOperations, ReadToolDetails, ReadToolInput, ReadToolOptions, create_local_read_operations, create_read_tool,
    create_read_tool_definition, read_tool_system_prompt_contribution,
};
pub use self::tool_definition_wrapper::{wrap_tool_definition, wrap_tool_definitions};
pub use self::truncate::{
    DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, GREP_MAX_LINE_LENGTH, MiddleTruncationResult, TruncateLineResult,
    TruncatedBy, TruncationOptions, TruncationResult, format_size, truncate_head, truncate_line, truncate_middle,
    truncate_tail,
};
pub use self::write::{
    WriteOperations, WriteToolInput, WriteToolOptions, create_local_write_operations, create_write_tool,
    create_write_tool_definition, write_tool_system_prompt_contribution,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolName {
    Read,
    Bash,
    Powershell,
    Edit,
    Write,
    Grep,
    Find,
    Ls,
}

impl ToolName {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Bash => "bash",
            Self::Powershell => "powershell",
            Self::Edit => "edit",
            Self::Write => "write",
            Self::Grep => "grep",
            Self::Find => "find",
            Self::Ls => "ls",
        }
    }
}

pub static ALL_TOOL_NAMES: LazyLock<IndexSet<ToolName>> = LazyLock::new(|| {
    let mut set = IndexSet::new();
    set.insert(ToolName::Read);
    set.insert(ToolName::Bash);
    set.insert(ToolName::Powershell);
    set.insert(ToolName::Edit);
    set.insert(ToolName::Write);
    set.insert(ToolName::Grep);
    set.insert(ToolName::Find);
    set.insert(ToolName::Ls);
    set
});

#[derive(Clone, Default)]
pub struct ToolsOptions {
    pub read: Option<ReadToolOptions>,
    pub bash: Option<BashToolOptions>,
    pub powershell: Option<PowerShellToolOptions>,
    pub write: Option<WriteToolOptions>,
    pub edit: Option<EditToolOptions>,
    pub grep: Option<GrepToolOptions>,
    pub find: Option<FindToolOptions>,
    pub ls: Option<LsToolOptions>,
}

impl std::fmt::Display for ToolName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for ToolName {
    type Err = pi_js::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "read" => Ok(Self::Read),
            "bash" => Ok(Self::Bash),
            "powershell" => Ok(Self::Powershell),
            "edit" => Ok(Self::Edit),
            "write" => Ok(Self::Write),
            "grep" => Ok(Self::Grep),
            "find" => Ok(Self::Find),
            "ls" => Ok(Self::Ls),
            _ => Err(pi_js::Error::msg(format!("Unknown built-in tool: {s}"))),
        }
    }
}

pub fn create_tool_definition(name: ToolName, cwd: &str, options: Option<&ToolsOptions>) -> ToolDefinition {
    match name {
        ToolName::Read => create_read_tool_definition(cwd, options.and_then(|o| o.read.clone())),
        ToolName::Bash => create_bash_tool_definition(cwd, options.and_then(|o| o.bash.clone())),
        ToolName::Powershell => create_power_shell_tool_definition(cwd, options.and_then(|o| o.powershell.clone())),
        ToolName::Edit => create_edit_tool_definition(cwd, options.and_then(|o| o.edit.clone())),
        ToolName::Write => create_write_tool_definition(cwd, options.and_then(|o| o.write.clone())),
        ToolName::Grep => create_grep_tool_definition(cwd, options.and_then(|o| o.grep.clone())),
        ToolName::Find => create_find_tool_definition(cwd, options.and_then(|o| o.find.clone())),
        ToolName::Ls => create_ls_tool_definition(cwd, options.and_then(|o| o.ls.clone())),
    }
}

pub fn create_tool(name: ToolName, cwd: &str, options: Option<&ToolsOptions>) -> Tool {
    wrap_tool_definition(create_tool_definition(name, cwd, options), None)
}

pub fn create_coding_tool_definitions(cwd: &str, options: Option<&ToolsOptions>) -> IndexMap<ToolName, ToolDefinition> {
    let mut map = IndexMap::new();
    for name in [ToolName::Read, ToolName::Bash, ToolName::Edit, ToolName::Write] {
        map.insert(name, create_tool_definition(name, cwd, options));
    }
    map
}

pub fn create_read_only_tool_definitions(
    cwd: &str,
    options: Option<&ToolsOptions>,
) -> IndexMap<ToolName, ToolDefinition> {
    let mut map = IndexMap::new();
    for name in [ToolName::Read, ToolName::Grep, ToolName::Find, ToolName::Ls] {
        map.insert(name, create_tool_definition(name, cwd, options));
    }
    map
}

pub fn create_all_tool_definitions(cwd: &str, options: Option<&ToolsOptions>) -> IndexMap<ToolName, ToolDefinition> {
    let mut map = IndexMap::new();
    for &name in ALL_TOOL_NAMES.iter() {
        map.insert(name, create_tool_definition(name, cwd, options));
    }
    map
}

pub fn create_coding_tools(cwd: &str, options: Option<&ToolsOptions>) -> Vec<Tool> {
    [ToolName::Read, ToolName::Bash, ToolName::Edit, ToolName::Write]
        .into_iter()
        .map(|name| create_tool(name, cwd, options))
        .collect()
}

pub fn create_read_only_tools(cwd: &str, options: Option<&ToolsOptions>) -> Vec<Tool> {
    [ToolName::Read, ToolName::Grep, ToolName::Find, ToolName::Ls]
        .into_iter()
        .map(|name| create_tool(name, cwd, options))
        .collect()
}

pub fn create_all_tools(cwd: &str, options: Option<&ToolsOptions>) -> IndexMap<ToolName, Tool> {
    let mut map = IndexMap::new();
    for &name in ALL_TOOL_NAMES.iter() {
        map.insert(name, create_tool(name, cwd, options));
    }
    map
}
