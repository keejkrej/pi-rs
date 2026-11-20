//! Port of packages/evals/evals/extensions.docs.eval.ts

use std::sync::Arc;

use indexmap::IndexMap;
use pi_agent_core::AgentMessage;
use pi_ai::content_text;
use serde::Serialize;
use serde_json::{Value, json};

use crate::harness::{
    DOCUMENTATION_EVAL_TOOLS, EvalCaseDef, EvalSuite, EvalSuiteKind, ExpectedToolCall, PiCodingAgentHarnessOptions,
    PiCodingAgentInput, PiCodingAgentStep, StructuredOutputJudgeOptions, StructuredOutputMatchMode,
    ToolCallJudgeOptions, create_pi_documentation_eval_harness, run_eval_case, serialize_to_json_value,
    structured_output_judge, tool_call_judge,
};

const TOOL_NAME: &str = "hello";
const TOOL_RESULT: &str = "Hello, Bob!";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExtensionEvalOutput {
    response: String,
    extension_errors: Vec<Value>,
    tool_result: Option<String>,
}

/// Returns the documentation-lift eval suite for creating and invoking a Pi tool extension.
pub fn suite() -> EvalSuite {
    let eval_set = "Create and use a tool extension".to_string();
    let case_id = "creates, reloads, and invokes the extension".to_string();
    let file = "evals/extensions.docs.eval.ts".to_string();
    let eval_set_clone = eval_set.clone();
    let case_id_clone = case_id.clone();

    EvalSuite {
        file: file.clone(),
        kind: EvalSuiteKind::Docs,
        cases: vec![EvalCaseDef {
            eval_set,
            case_id,
            file,
            execute: Arc::new(move |signal| {
                let eval_set = eval_set_clone.clone();
                let case_id = case_id_clone.clone();
                Box::pin(async move {
                    let mut tools: Vec<String> = DOCUMENTATION_EVAL_TOOLS.iter().map(|s| (*s).to_string()).collect();
                    tools.push(TOOL_NAME.to_string());

                    let harness = create_pi_documentation_eval_harness(PiCodingAgentHarnessOptions {
                        tools: Some(tools),
                        output: Some(Arc::new(|session, response, _system_prompt, _agent_dir| {
                            let response = response.to_string();
                            let extensions = session.resource_loader().get_extensions();
                            let messages = session.messages();
                            let result_text = messages
                                .iter()
                                .rev()
                                .filter_map(AgentMessage::as_tool_result)
                                .find(|tr| tr.tool_name == TOOL_NAME && !tr.is_error)
                                .map(|tr| content_text(&tr.content, None));
                            let extension_errors = extensions
                                .errors
                                .iter()
                                .map(|e| json!({ "path": e.path, "error": e.error }))
                                .collect();
                            let out = ExtensionEvalOutput {
                                response,
                                extension_errors,
                                tool_result: result_text,
                            };
                            Box::pin(async move { Ok(serialize_to_json_value(&out)) })
                        })),
                        ..Default::default()
                    })?;

                    let mut structured_opts = StructuredOutputJudgeOptions::new(json!({
                        "response": TOOL_RESULT,
                        "extensionErrors": [],
                        "toolResult": TOOL_RESULT,
                    }));
                    structured_opts.match_mode = StructuredOutputMatchMode::Strict;
                    structured_opts.allow_extras = false;

                    let mut tool_args = IndexMap::new();
                    tool_args.insert("name".to_string(), Value::String("Bob".to_string()));
                    let judges = vec![
                        structured_output_judge(structured_opts),
                        tool_call_judge(ToolCallJudgeOptions {
                            expected_tools: vec![ExpectedToolCall {
                                name: TOOL_NAME.to_string(),
                                arguments: Some(tool_args),
                            }],
                            ..Default::default()
                        }),
                    ];

                    let steps = vec![
                        PiCodingAgentStep::Prompt {
                            content: "Configure this running Pi installation with an extension containing a hello tool that takes a name and returns a greeting. Do not create project source. For example, passing Bob should return `Hello, Bob!`.".to_string(),
                        },
                        PiCodingAgentStep::Reload,
                        PiCodingAgentStep::Prompt {
                            content: "Use the hello tool to greet Bob. Respond with exactly the tool's greeting and nothing else.".to_string(),
                        },
                    ];
                    let input_json = serialize_to_json_value(&steps);
                    run_eval_case::<_, fn(&crate::report::HarnessRun) -> pi_js::Result<()>>(
                        &eval_set,
                        &case_id,
                        &harness,
                        PiCodingAgentInput::Steps(steps),
                        input_json,
                        &judges,
                        Some(None),
                        signal,
                        None,
                    )
                    .await
                })
            }),
        }],
    }
}
