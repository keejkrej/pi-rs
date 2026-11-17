//! Port of packages/evals/evals/openai-provider.docs.eval.ts

use std::sync::Arc;

use indexmap::IndexMap;
use pi_ai::{
    Context, Message, ModelCostRates, ModelModality, ModelsSimpleStreamOptions, SimpleStreamOptions, StreamOptions,
    UserMessage, UserMessageContent,
};

use crate::evals::acme_server::{
    AcmeServerMode, OPENAI_MODEL_ID, OPENAI_PROBE_PROMPT, OPENAI_PROBE_RESPONSE, OPENAI_PROVIDER_ID, create_acme_server,
};
use crate::evals::configured_runtime::{
    ModelFields, ProbeResponseFields, ProviderRuntimeOutput, ProviderRuntimeResult, ProviderScenario, inspect_provider,
};
use crate::harness::{
    EvalCaseDef, EvalSuite, EvalSuiteKind, PiCodingAgentHarnessOptions, PiCodingAgentInput, PiCodingAgentStep,
    StructuredOutputJudgeOptions, StructuredOutputMatchMode, create_pi_documentation_eval_harness, run_eval_case,
    serialize_to_json_value, structured_output_judge,
};

fn expected_output() -> ProviderRuntimeOutput {
    ProviderRuntimeOutput {
        result: ProviderRuntimeResult::Success {
            valid_request_received: true,
            model: ModelFields {
                id: OPENAI_MODEL_ID.to_string(),
                name: "Acme Chat".to_string(),
                provider: OPENAI_PROVIDER_ID.to_string(),
                reasoning: false,
                input: vec![ModelModality::Text],
                cost: ModelCostRates {
                    input: 0.0,
                    output: 0.0,
                    cache_read: 0.0,
                    cache_write: 0.0,
                },
                context_window: 32_768,
                max_tokens: 4_096,
            },
            response: ProbeResponseFields {
                text: OPENAI_PROBE_RESPONSE.to_string(),
                stop_reason: "stop".to_string(),
                input_tokens: 3,
                output_tokens: 2,
            },
        },
    }
}

/// Returns the documentation-lift eval suite for OpenAI-compatible custom providers.
pub fn suite() -> EvalSuite {
    let eval_set = "Add OpenAI-compatible provider".to_string();
    let case_id = "configures a provider that works through Pi".to_string();
    let file = "evals/openai-provider.docs.eval.ts".to_string();
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
                    let server = create_acme_server(AcmeServerMode::OpenAi);
                    server.start().await?;
                    server.reset();

                    let run_outcome = async {
                        let server_for_output = server.clone();
                        let harness = create_pi_documentation_eval_harness(PiCodingAgentHarnessOptions {
                            output: Some(Arc::new(move |session, _response, _system_prompt, _agent_dir| {
                                let model_runtime = session.model_runtime().clone();
                                let server_ref = server_for_output.clone();
                                Box::pin(async move {
                                    let mut env_map = IndexMap::new();
                                    env_map.insert("ACME_API_KEY".to_string(), "resolved-acme-key".to_string());
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
                                            provider_id: OPENAI_PROVIDER_ID.to_string(),
                                            model_id: OPENAI_MODEL_ID.to_string(),
                                            create_context: Box::new(|| Context {
                                                system_prompt: None,
                                                messages: vec![Message::User(UserMessage {
                                                    content: UserMessageContent::Text(OPENAI_PROBE_PROMPT.to_string()),
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
                                    "Configure this running Pi installation with Acme as a provider. Do not create project-local configuration. Its provider ID is {OPENAI_PROVIDER_ID}, its API is at {}, and it uses OpenAI Chat Completions. Read its API key from the ACME_API_KEY environment variable.\n\nThe provider offers one model, {OPENAI_MODEL_ID}, shown as \u{201c}Acme Chat\u{201d}. It accepts text, does not support reasoning, has a 32,768-token context window and a 4,096-token maximum output, and has no usage cost.",
                                    server.base_url()
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
