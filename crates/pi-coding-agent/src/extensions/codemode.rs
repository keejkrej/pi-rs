//! Port of packages/coding-agent/src/extensions/codemode/index.ts

// @generated-mods begin (scaffold-owned, do not edit)
pub mod execute;
pub mod execute_lazy;
pub mod renderer;
pub mod tool;
// @generated-mods end

use std::sync::Arc;

use crate::core::extensions::types::mcp_wire::CodemodeMode;
use crate::core::extensions::types::{ExtensionApi, ExtensionFactory};
use crate::extensions::codemode::tool::{CodemodeModelRuntime, CodemodeToolOptions, create_codemode_tool_definition};

#[derive(Clone, Default)]
pub struct CodemodeExtensionOptions {
    /// Overrides the `codemode.mode` setting.
    pub mode: Option<CodemodeMode>,
    /// Overrides the `codemode.inlineBudget` setting.
    pub inline_budget: Option<usize>,
    /// Expose the model catalog and classifiers to scripts as `models`. Default: `true`.
    pub models: Option<bool>,
    /// Optional explicit model runtime override.
    pub model_runtime: Option<Arc<dyn CodemodeModelRuntime>>,
}

fn read_mode(pi: &ExtensionApi) -> CodemodeMode {
    pi.get_settings()
        .ok()
        .and_then(|s| s.codemode)
        .and_then(|c| c.mode)
        .unwrap_or(CodemodeMode::On)
}

fn read_inline_budget(pi: &ExtensionApi) -> Option<usize> {
    pi.get_settings()
        .ok()
        .and_then(|s| s.codemode)
        .and_then(|c| c.inline_budget)
        .filter(|b| *b >= 0)
        .map(|b| b as usize)
}

pub fn create_codemode_extension(options: CodemodeExtensionOptions) -> ExtensionFactory {
    Arc::new(move |pi: ExtensionApi| {
        let options = options.clone();
        Box::pin(async move {
            let pi_append = pi.clone();
            let pi_ns = pi.clone();
            let pi_mode = pi.clone();
            let pi_budget = pi.clone();
            let explicit_mode = options.mode;
            let explicit_budget = options.inline_budget;
            let mut def = create_codemode_tool_definition(CodemodeToolOptions {
                append_entry: Some(Arc::new(move |custom_type, data| {
                    let val = pi_js::json::parse(&pi_js::json::stringify(&data)).ok();
                    let _ = pi_append.append_entry(custom_type, val);
                })),
                models: options.models.unwrap_or(true),
                model_runtime: options.model_runtime.clone(),
                get_tool_namespace: Some(Arc::new(move |name| {
                    pi_ns
                        .get_all_tools()
                        .ok()
                        .and_then(|tools| tools.into_iter().find(|t| t.name == name))
                        .and_then(|t| t.namespace)
                })),
                get_mode: Some(Arc::new(move || explicit_mode.unwrap_or_else(|| read_mode(&pi_mode)))),
                get_inline_budget: Some(Arc::new(move || {
                    explicit_budget.or_else(|| read_inline_budget(&pi_budget))
                })),
            });
            def.default_active = Some(false);
            pi.register_tool(def)
        })
    })
}

pub fn default_export() -> ExtensionFactory {
    create_codemode_extension(CodemodeExtensionOptions::default())
}
