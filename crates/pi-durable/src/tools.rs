//! Port of packages/durable/src/tools/index.ts

// @generated-mods begin (scaffold-owned, do not edit)
pub mod bash;
pub mod edit;
pub mod edit_diff;
pub mod env;
pub mod file_mutation_queue;
pub mod image;
pub mod path_utils;
pub mod read;
pub mod write;
// @generated-mods end

pub use bash::{BashExecution, BashPrepare, BashToolInput, BashToolOptions, create_bash_tool};
pub use edit::{EditToolDetails, EditToolInput, create_edit_tool};
pub use read::{ReadToolDetails, ReadToolInput, ReadTruncationDetails, create_read_tool};
pub use write::{WriteToolInput, create_write_tool};

use crate::harness::define::define_extension;
use crate::harness::types::Extension;

/// `read`, `write`, `edit`, and `bash`; nothing installs it automatically.
pub fn coding_tools() -> Extension {
    define_extension(Extension {
        name: "coding-tools".to_owned(),
        tools: vec![
            create_read_tool(),
            create_write_tool(),
            create_edit_tool(),
            create_bash_tool(None),
        ],
        ..Default::default()
    })
}
