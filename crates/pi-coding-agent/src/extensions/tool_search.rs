//! Port of packages/coding-agent/src/extensions/tool-search/index.ts

// @generated-mods begin (scaffold-owned, do not edit)
pub mod tool;
// @generated-mods end

use std::sync::Arc;

use crate::core::extensions::types::{ExtensionApi, ExtensionFactory};
use crate::extensions::tool_search::tool::{ToolSearchToolOptions, create_tool_search_tool_definition};

pub fn create_tool_search_extension() -> ExtensionFactory {
    Arc::new(|pi: ExtensionApi| {
        Box::pin(async move {
            let mut def = create_tool_search_tool_definition(ToolSearchToolOptions {
                tools: Some(Arc::new(pi.clone())),
            });
            def.default_active = Some(false);
            pi.register_tool(def)
        })
    })
}

pub fn default_export() -> ExtensionFactory {
    create_tool_search_extension()
}
