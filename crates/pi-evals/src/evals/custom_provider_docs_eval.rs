//! Port of packages/evals/evals/custom-provider.docs.eval.ts

use std::sync::Arc;

use indexmap::IndexMap;
use pi_ai::{
    Context, Message, ModelCostRates, ModelModality, ModelsSimpleStreamOptions, SimpleStreamOptions, StreamOptions,
    UserMessage, UserMessageContent,
};

use crate::evals::acme_server::{
    AcmeServerMode, STREAM_API_DOCUMENTATION, STREAM_MODEL_ID, STREAM_PROBE_PROMPT, STREAM_PROBE_RESPONSE,
    STREAM_PROVIDER_ID, create_acme_server,
};
use crate::evals::configured_runtime::{
    ModelFields, ProbeResponseFields, ProviderRuntimeOutput, ProviderRuntimeResult, ProviderScenario, inspect_provider,
};
use crate::harness::{
    EvalCaseDef, EvalSuite, EvalSuiteKind, PiCodingAgentHarnessOptions, PiCodingAgentInput, PiCodingAgentStep,
    StructuredOutputJudgeOptions, StructuredOutputMatchMode, create_pi_documentation_eval_harness, run_eval_case,
    serialize_to_json_value, structured_output_judge,
};

const DOCUMENTATION_PATH: &str = "fixtures/acme-stream-api.json";
const STREAM_API_KEY: &str = "resolved-stream-key";

struct StreamKeyEnvGuard {
    previous: Option<String>,
}

impl StreamKeyEnvGuard {
    fn install() -> Self {
        let previous = pi_js::env::var("ACME_STREAM_API_KEY");
        pi_js::env::set_var("ACME_STREAM_API_KEY", STREAM_API_KEY);
        Self { previous }
    }
}

impl Drop for StreamKeyEnvGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(v) => pi_js::env::set_var("ACME_STREAM_API_KEY", v),
            None => pi_js::env::remove_var("ACME_STREAM_API_KEY"),
        }
    }
}

fn expected_output() -> ProviderRuntimeOutput {
    ProviderRuntimeOutput {
        result: ProviderRuntimeResult::Success {
            valid_request_received: true,
            model: ModelFields {
                id: STREAM_MODEL_ID.to_string(),
                name: "Acme Stream Chat".to_string(),
                provider: STREAM_PROVIDER_ID.to_string(),
                reasoning: false,
                input: vec![ModelModality::Text],
                cost: ModelCostRates {
                    input: 0.0,
                    output: 0.0,
                    cache_read: 0.0,
                    cache_write: 0.0,
                },
                context_window: 16_384,
                max_tokens: 2_048,
            },
            response: ProbeResponseFields {
                text: STREAM_PROBE_RESPONSE.to_string(),
                stop_reason: "stop".to_string(),
                input_tokens: 4,
                output_tokens: 3,
            },
        },
    }
}

/// Returns the documentation-lift eval suite for custom streaming providers.
pub fn suite() -> EvalSuite {
    let eval_set = "Add custom streaming provider".to_string();
    let case_id = "implements a provider from an API fixture".to_string();
    let file = "evals/custom-provider.docs.eval.ts".to_string();
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
                    let _env_guard = StreamKeyEnvGuard::install();
                    let server = create_acme_server(AcmeServerMode::Stream);
                    server.start().await?;
                    server.reset();

                    let run_outcome = async {
                        let mut workspace_files = IndexMap::new();
                        workspace_files.insert(DOCUMENTATION_PATH.to_string(), STREAM_API_DOCUMENTATION.clone());

                        let server_for_output = server.clone();
                        let harness = create_pi_documentation_eval_harness(PiCodingAgentHarnessOptions {
                            workspace_files: Some(workspace_files),
                            output: Some(Arc::new(move |session, _response, _system_prompt, _agent_dir| {
                                let model_runtime = session.model_runtime().clone();
                                let server_ref = server_for_output.clone();
                                Box::pin(async move {
                                    let mut env_map = IndexMap::new();
                                    env_map.insert("ACME_STREAM_API_KEY".to_string(), STREAM_API_KEY.to_string());
                                    let options = ModelsSimpleStreamOptions {
                                        base: SimpleStreamOptions {
                                            base: StreamOptions {
                                                env: Some(env_map),
                                                max_tokens: Some(32),
                                                ..Default::default()
                                            },
                                            ..Default::default()
                                        },
                                        transform_headers: None,
                                    };
                                    let out = inspect_provider(
                                        &model_runtime,
                                        ProviderScenario {
                                            provider_id: STREAM_PROVIDER_ID.to_string(),
                                            model_id: STREAM_MODEL_ID.to_string(),
                                            create_context: Box::new(|| Context {
                                                system_prompt: None,
                                                messages: vec![Message::User(UserMessage {
                                                    content: UserMessageContent::Text(STREAM_PROBE_PROMPT.to_string()),
                                                    timestamp: 0,
                                                })],
                                                tools: None,
                                            }),
                                            options: Some(options),
                                            valid_request_received: Box::new(move || {
                                                server_ref.valid_request_received()
                                            }),
                                        },
                                    )
                                    .await?;
                                    Ok(serialize_to_json_value(&out))
                                })
                            })),
                            ..Default::default()
                        })?;

                        let mut judge_opts =
                            StructuredOutputJudgeOptions::new(serialize_to_json_value(&expected_output()));
                        judge_opts.match_mode = StructuredOutputMatchMode::Strict;
                        judge_opts.allow_extras = false;
                        let judge = structured_output_judge(judge_opts);

                        let steps = vec![
                            PiCodingAgentStep::Prompt {
                                content: format!(
                                    "Configure this running Pi installation with Acme Stream as a provider. Do not create a project package or modify project source. Its provider ID is {STREAM_PROVIDER_ID}, its API is at {}, and its API documentation is in ./{DOCUMENTATION_PATH}. Read its credential from the ACME_STREAM_API_KEY environment variable.\n\nIt offers one model, {STREAM_MODEL_ID}, shown as \u{201c}Acme Stream Chat\u{201d}. The model accepts text, does not support reasoning, has a 16,384-token context window and a 2,048-token maximum output, and has no usage cost.",
                                    server.origin()
                                ),
                            },
                            PiCodingAgentStep::Reload,
                        ];
                        let input_json = serialize_to_json_value(&steps);
                        run_eval_case::<_, fn(&crate::report::HarnessRun) -> pi_js::Result<()>>(
                            &eval_set,
                            &case_id,
                            &harness,
                            PiCodingAgentInput::Steps(steps),
                            input_json,
                            &[judge],
                            Some(None),
                            signal,
                            None,
                        )
                        .await
                    }
                    .await;

                    let _ = server.stop().await;
                    run_outcome
                })
            }),
        }],
    }
}
