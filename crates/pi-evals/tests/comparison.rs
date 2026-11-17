//! Port of packages/evals/test/comparison.test.ts

use pi_evals::plan::DocumentationVariant;
use pi_evals::report::{
    EvalMetrics, EvalObservation, EvalOutcome, EvalRunIdentity, ExpectedEvalRun, OperationalMetricTotal,
    PairedMetricSummary, format_eval_comparison_report, summarize_eval_observations,
};
use pi_tui::utils::strip_terminal_sequences;
use pretty_assertions::assert_eq;

fn scored(
    variant: DocumentationVariant,
    run_number: i64,
    score: f64,
    total_tokens: Option<f64>,
    tool_calls: Option<f64>,
    total_ms: Option<f64>,
) -> EvalObservation {
    EvalObservation::from_identity(
        EvalRunIdentity {
            eval_set: "tool access".to_string(),
            case_id: "create".to_string(),
            variant,
            model: "fixture/model".to_string(),
            run_number,
        },
        EvalMetrics {
            total_tokens: Some(total_tokens.unwrap_or(100.0)),
            tool_calls: Some(tool_calls.unwrap_or(2.0)),
            total_ms: Some(total_ms.unwrap_or(1000.0)),
            estimated_cost_usd: Some(0.01),
            ..Default::default()
        },
        EvalOutcome::Scored { score },
    )
}

fn errored(variant: DocumentationVariant, run_number: i64, total_tokens: Option<f64>) -> EvalObservation {
    EvalObservation::from_identity(
        EvalRunIdentity {
            eval_set: "tool access".to_string(),
            case_id: "create".to_string(),
            variant,
            model: "fixture/model".to_string(),
            run_number,
        },
        EvalMetrics {
            total_tokens,
            ..Default::default()
        },
        EvalOutcome::Errored,
    )
}

fn expected_for(run_numbers: &[i64]) -> Vec<ExpectedEvalRun> {
    run_numbers
        .iter()
        .flat_map(|&run_number| {
            [DocumentationVariant::WithoutDocs, DocumentationVariant::WithDocs]
                .into_iter()
                .map(move |variant| EvalRunIdentity {
                    eval_set: "tool access".to_string(),
                    case_id: "create".to_string(),
                    variant,
                    model: "fixture/model".to_string(),
                    run_number,
                })
        })
        .collect()
}

mod summarize_eval_observations_tests {
    use super::*;

    #[test]
    fn computes_paired_lift_and_efficiency_deltas() {
        let observations = vec![
            scored(
                DocumentationVariant::WithoutDocs,
                1,
                0.0,
                Some(100.0),
                Some(3.0),
                Some(1000.0),
            ),
            scored(
                DocumentationVariant::WithDocs,
                1,
                1.0,
                Some(120.0),
                Some(2.0),
                Some(800.0),
            ),
            scored(DocumentationVariant::WithoutDocs, 2, 1.0, Some(200.0), None, None),
            scored(DocumentationVariant::WithDocs, 2, 1.0, Some(180.0), None, None),
        ];
        let report = summarize_eval_observations("digest", &expected_for(&[1, 2]), &observations);
        assert_eq!(report.comparisons.len(), 1);
        let cmp = &report.comparisons[0];
        assert_eq!(cmp.eval_set, "tool access");
        assert_eq!(cmp.total_pairs, 2);
        assert_eq!(cmp.eligible_pairs, 2);
        assert_eq!(cmp.blocked_pairs, 0);
        assert_eq!(cmp.control_pass_rate, Some(0.5));
        assert_eq!(cmp.treatment_pass_rate, Some(1.0));
        assert_eq!(cmp.lift, Some(0.5));
        assert_eq!(
            cmp.total_tokens,
            PairedMetricSummary {
                eligible_pairs: 2,
                control_mean: Some(150.0),
                treatment_mean: Some(150.0),
                mean_delta: Some(0.0),
            }
        );
        assert_eq!(
            cmp.tool_calls,
            PairedMetricSummary {
                eligible_pairs: 2,
                control_mean: Some(2.5),
                treatment_mean: Some(2.0),
                mean_delta: Some(-0.5),
            }
        );
        assert_eq!(
            cmp.total_ms,
            PairedMetricSummary {
                eligible_pairs: 2,
                control_mean: Some(1000.0),
                treatment_mean: Some(900.0),
                mean_delta: Some(-100.0),
            }
        );
        assert_eq!(
            cmp.estimated_cost_usd,
            PairedMetricSummary {
                eligible_pairs: 2,
                control_mean: Some(0.01),
                treatment_mean: Some(0.01),
                mean_delta: Some(0.0),
            }
        );
    }

    #[test]
    fn fails_closed_for_incomplete_and_errored_pairs_while_retaining_totals() {
        let observations = vec![
            scored(DocumentationVariant::WithoutDocs, 1, 0.0, None, None, None),
            errored(DocumentationVariant::WithDocs, 1, Some(120.0)),
            scored(DocumentationVariant::WithoutDocs, 2, 1.0, Some(200.0), None, None),
        ];
        let report = summarize_eval_observations("digest", &expected_for(&[1, 2]), &observations);
        assert_eq!(report.comparisons[0].total_pairs, 2);
        assert_eq!(report.comparisons[0].eligible_pairs, 0);
        assert_eq!(report.comparisons[0].blocked_pairs, 2);
        assert_eq!(report.comparisons[0].lift, None);
        assert_eq!(report.blocked_pairs.len(), 2);
        assert_eq!(report.blocked_pairs[0].run_number, 1);
        assert_eq!(report.blocked_pairs[0].reasons, vec!["with_docs: errored"]);
        assert_eq!(report.blocked_pairs[1].run_number, 2);
        assert_eq!(
            report.blocked_pairs[1].reasons,
            vec!["with_docs: expected 1 observation, found 0"]
        );
        assert_eq!(
            report.operational_totals[0].total_tokens,
            OperationalMetricTotal {
                available_runs: 2,
                total: Some(300.0),
            }
        );
    }

    #[test]
    fn blocks_duplicate_observations_and_keeps_missing_metrics_distinct_from_zero() {
        let mut without_tokens = scored(DocumentationVariant::WithoutDocs, 1, 1.0, None, None, None);
        without_tokens.total_tokens = None;
        let report = summarize_eval_observations(
            "digest",
            &expected_for(&[1]),
            &[
                without_tokens.clone(),
                without_tokens,
                scored(DocumentationVariant::WithDocs, 1, 1.0, Some(0.0), None, None),
            ],
        );
        assert_eq!(
            report.blocked_pairs[0].reasons,
            vec!["without_docs: expected 1 observation, found 2"]
        );
        assert_eq!(
            report.operational_totals[0].total_tokens,
            OperationalMetricTotal {
                available_runs: 0,
                total: None,
            }
        );
        assert_eq!(
            report.operational_totals[1].total_tokens,
            OperationalMetricTotal {
                available_runs: 1,
                total: Some(0.0),
            }
        );
    }

    #[test]
    fn formats_blocked_comparisons_and_operational_totals() {
        let report = summarize_eval_observations(
            "digest",
            &expected_for(&[1, 2]),
            &[
                scored(DocumentationVariant::WithoutDocs, 1, 1.0, None, None, None),
                scored(DocumentationVariant::WithDocs, 1, 1.0, None, None, None),
            ],
        );
        let formatted = strip_terminal_sequences(&format_eval_comparison_report(&report));
        assert!(formatted.contains("Documentation Eval Comparisons"), "{formatted}");
        assert!(
            formatted.contains("Pass rate  withheld because pairs are blocked"),
            "{formatted}"
        );
        assert!(formatted.contains("without_docs: 1 runs"), "{formatted}");
    }
}
