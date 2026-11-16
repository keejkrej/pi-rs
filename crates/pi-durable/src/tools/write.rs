//! Port of packages/durable/src/tools/write.ts

use std::sync::Arc;

use pi_js::vendor::typebox::t;
use serde::{Deserialize, Serialize};

use crate::env::{FileContent, get_or_throw};
use crate::harness::define::define_tool;
use crate::harness::types::{ToolExecutionResult, ToolRegistration, ToolResultContentBlock};
use crate::tools::env::require_env;
use crate::tools::file_mutation_queue::with_file_mutation_queue;
use crate::tools::path_utils::resolve_tool_path;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteToolInput {
    pub path: String,
    pub content: String,
}

fn write_schema() -> pi_js::vendor::typebox::Schema {
    t::object([
        (
            "path",
            t::string().with("description", "Path to the file to write (relative or absolute)"),
        ),
        (
            "content",
            t::string().with("description", "Content to write to the file"),
        ),
    ])
}

pub fn create_write_tool() -> ToolRegistration {
    define_tool(ToolRegistration {
        name: "write".to_owned(),
        description: "Write content to a file. Creates the file if it doesn't exist, overwrites if it does. Automatically creates parent directories.".to_owned(),
        parameters: write_schema().to_value(),
        replay: None,
        execution_mode: None,
        prepare_arguments: None,
        output_limits: None,
        execute: Arc::new(|args_val, api, context| {
            Box::pin(async move {
                let args: WriteToolInput = serde_json::from_value(args_val)
                    .map_err(|err| pi_js::Error::js("TypeError", err.to_string()))?;
                let WriteToolInput { path, content } = args;
                let env = require_env(&api)?;
                let absolute_path = resolve_tool_path(env.as_ref(), &path, &context).await?;
                let absolute_path_task = absolute_path.clone();
                let env_for_queue = Arc::clone(&env);
                let queue_ctx = context.clone();
                with_file_mutation_queue(
                    env_for_queue.as_ref(),
                    &absolute_path,
                    move || {
                        let env = Arc::clone(&env);
                        let context = queue_ctx;
                        async move {
                            if context.abort_signal().is_some_and(|s| s.aborted()) {
                                return Err(pi_js::Error::msg("Operation aborted"));
                            }
                            get_or_throw(
                                env.write_file(&absolute_path_task, FileContent::Text(&content), &context)
                                    .await,
                            )?;
                            if context.abort_signal().is_some_and(|s| s.aborted()) {
                                return Err(pi_js::Error::msg("Operation aborted"));
                            }
                            Ok(ToolExecutionResult {
                                content: Some(vec![ToolResultContentBlock::Text(
                                    pi_ai::types::TextContent::new(format!("Successfully wrote to {path}")),
                                )]),
                                ..Default::default()
                            })
                        }
                    },
                    &context,
                )
                .await
            })
        }),
    })
}
