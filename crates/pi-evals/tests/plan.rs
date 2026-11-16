//! Port of packages/evals/test/plan.test.ts

use pi_evals::plan::{DiscoveredEvalCase, DocumentationVariant, EvalTask, create_task_plan, parse_discovered_cases};
use pretty_assertions::assert_eq;
use serde_json::json;

fn discovered() -> serde_json::Value {
    json!([{ "name": "Add model > adds the model", "file": "evals/models.docs.eval.ts" }])
}

mod parse_discovered_cases_tests {
    use super::*;

    #[test]
    fn derives_stable_case_identity_from_ordinary_vitest_names() {
        let parsed = parse_discovered_cases(&discovered()).unwrap();
        assert_eq!(
            parsed,
            vec![DiscoveredEvalCase {
                full_name: "Add model > adds the model".to_string(),
                eval_set: "Add model".to_string(),
                case_id: "adds the model".to_string(),
                file: "evals/models.docs.eval.ts".to_string(),
            }]
        );
    }

    #[test]
    fn rejects_ambiguous_and_duplicate_identities() {
        let err = parse_discovered_cases(&json!([{ "name": "adds the model", "file": "model.ts" }])).unwrap_err();
        assert!(err.to_string().contains("<eval set> > <case>"), "{err}");

        let dup = json!([
            { "name": "Add model > adds the model", "file": "evals/models.docs.eval.ts" },
            { "name": "Add model > adds the model", "file": "evals/models.docs.eval.ts" }
        ]);
        let dup_err = parse_discovered_cases(&dup).unwrap_err();
        assert!(
            dup_err.to_string().contains("Duplicate eval case identity"),
            "{dup_err}"
        );
    }
}

mod create_task_plan_tests {
    use super::*;

    #[test]
    fn creates_one_isolated_task_per_case_variant_model_and_repetition() {
        let cases = parse_discovered_cases(&discovered()).unwrap();
        let tasks: Vec<EvalTask> = create_task_plan(&cases, "fixture/model", 2).unwrap();
        assert_eq!(tasks.len(), 4);
        let order: Vec<(DocumentationVariant, i64)> = tasks.iter().map(|t| (t.variant, t.run_number)).collect();
        assert_eq!(
            order,
            vec![
                (DocumentationVariant::WithoutDocs, 1),
                (DocumentationVariant::WithDocs, 1),
                (DocumentationVariant::WithDocs, 2),
                (DocumentationVariant::WithoutDocs, 2),
            ]
        );
    }

    #[test]
    fn rejects_invalid_model_identities_and_repetitions() {
        let cases = parse_discovered_cases(&discovered()).unwrap();
        let bad_model = create_task_plan(&cases, "model", 1).unwrap_err();
        assert!(bad_model.to_string().contains("provider and model"), "{bad_model}");

        let bad_runs = create_task_plan(&cases, "fixture/model", 0).unwrap_err();
        assert!(bad_runs.to_string().contains("positive integer"), "{bad_runs}");
    }
}
