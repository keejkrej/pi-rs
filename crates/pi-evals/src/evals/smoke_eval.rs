//! Port of packages/evals/evals/smoke.eval.ts

use std::sync::Arc;

use pi_coding_agent::core::sdk::NoToolsMode;
use pi_js::Error;
use serde_json::Value;

use crate::harness::{
    EvalCaseDef, EvalSuite, EvalSuiteKind, PiCodingAgentHarnessOptions, PiCodingAgentInput,
    create_pi_coding_agent_harness, run_eval_case,
};

/// Returns the smoke eval suite executed on the host against configured credentials.
pub fn suite() -> EvalSuite {
    let eval_set = "Answer a basic prompt".to_string();
    let case_id = "returns the expected answer".to_string();
    let file = "evals/smoke.eval.ts".to_string();
    let eval_set_clone = eval_set.clone();
    let case_id_clone = case_id.clone();

    EvalSuite {
        file: file.clone(),
        kind: EvalSuiteKind::Host,
        cases: vec![EvalCaseDef {
            eval_set,
            case_id,
            file,
            execute: Arc::new(move |signal| {
                let eval_set = eval_set_clone.clone();
                let case_id = case_id_clone.clone();
                Box::pin(async move {
                    let harness = create_pi_coding_agent_harness(PiCodingAgentHarnessOptions {
                        no_tools: Some(NoToolsMode::All),
                        ..Default::default()
                    });
                    let prompt = "What's the capital of France? Respond with only the city name.";
                    run_eval_case(
                        &eval_set,
                        &case_id,
                        &harness,
                        PiCodingAgentInput::Text(prompt.to_string()),
                        Value::String(prompt.to_string()),
                        &[],
                        None,
                        signal,
                        Some(|run: &crate::report::HarnessRun| {
                            let output = run.output.as_ref().and_then(Value::as_str).unwrap_or("");
                            if pi_js::str16::trim(output) != "Paris" {
                                return Err(Error::msg(format!("Expected \"Paris\", got \"{output}\"")));
                            }
                            if !run.errors.is_empty() {
                                return Err(Error::msg("Expected no errors in smoke eval"));
                            }
                            let expected_provider = pi_js::env::var("PI_PROVIDER");
                            let expected_model = pi_js::env::var("PI_MODEL");
                            if run.usage.provider != expected_provider || run.usage.model != expected_model {
                                return Err(Error::msg("Smoke eval usage provider/model mismatch"));
                            }
                            if run.usage.total_tokens.unwrap_or(0.0) <= 0.0 {
                                return Err(Error::msg("Expected positive totalTokens in smoke eval"));
                            }
                            Ok(())
                        }),
                    )
                    .await
                })
            }),
        }],
    }
}
