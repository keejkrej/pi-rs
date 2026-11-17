//! Port of packages/evals/evals/models.docs.eval.ts

use std::sync::Arc;

use pi_ai::{ModelCostRates, ModelModality};

use crate::evals::configured_runtime::{AddedModelOutput, AddedModelResult, ModelFields, inspect_added_model};
use crate::harness::{
    EvalCaseDef, EvalSuite, EvalSuiteKind, PiCodingAgentHarnessOptions, PiCodingAgentInput, PiCodingAgentStep,
    StructuredOutputJudgeOptions, StructuredOutputMatchMode, create_pi_documentation_eval_harness, run_eval_case,
    serialize_to_json_value, structured_output_judge,
};

const PROVIDER_ID: &str = "openai";
const MODEL_ID: &str = "fixture-chat";
const MODEL_NAME: &str = "Fixture Chat";

fn expected_output() -> AddedModelOutput {
    AddedModelOutput {
        result: AddedModelResult::Success {
            model: ModelFields {
                id: MODEL_ID.to_string(),
                name: MODEL_NAME.to_string(),
                provider: PROVIDER_ID.to_string(),
                reasoning: true,
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
            existing_models_preserved: true,
        },
    }
}

/// Returns the documentation-lift eval suite for adding a model to an existing provider.
pub fn suite() -> EvalSuite {
    let eval_set = "Add model to existing provider".to_string();
    let case_id = "adds the model without replacing existing models".to_string();
    let file = "evals/models.docs.eval.ts".to_string();
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
                    let harness = create_pi_documentation_eval_harness(PiCodingAgentHarnessOptions {
                        output: Some(Arc::new(|session, _response, _system_prompt, _agent_dir| {
                            let model_runtime = session.model_runtime().clone();
                            Box::pin(async move {
                                let out = inspect_added_model(&model_runtime, PROVIDER_ID, MODEL_ID).await?;
                                Ok(serialize_to_json_value(&out))
                            })
                        })),
                        ..Default::default()
                    })?;

                    let mut judge_opts = StructuredOutputJudgeOptions::new(serialize_to_json_value(&expected_output()));
                    judge_opts.match_mode = StructuredOutputMatchMode::Strict;
                    judge_opts.allow_extras = false;
                    let judge = structured_output_judge(judge_opts);

                    let steps = vec![
                        PiCodingAgentStep::Prompt {
                            content: format!(
                                "Configure this running Pi installation with a new `{PROVIDER_ID}/{MODEL_ID}` model. Do not create project-local configuration. Show it as \u{201c}{MODEL_NAME}\u{201d}. It accepts text, supports reasoning, has a 32,768-token context window and a 4,096-token maximum output, and has no usage cost."
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
                })
            }),
        }],
    }
}
