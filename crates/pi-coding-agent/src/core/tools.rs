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

use indexmap::IndexSet;
use serde::{Deserialize, Serialize};

pub use self::bash::{BashToolDetails, BashToolInput, BashToolOptions};
pub use self::edit::{EditToolDetails, EditToolInput, EditToolOptions};
pub use self::file_mutation_queue::with_file_mutation_queue;
pub use self::find::{FindToolDetails, FindToolInput, FindToolOptions};
pub use self::grep::{GrepToolDetails, GrepToolInput, GrepToolOptions};
pub use self::ls::{LsToolDetails, LsToolInput, LsToolOptions};
pub use self::powershell::{PowerShellToolDetails, PowerShellToolInput, PowerShellToolOptions};
pub use self::read::{ReadToolDetails, ReadToolInput, ReadToolOptions};
pub use self::write::{WriteToolInput, WriteToolOptions};

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
