//! Port of packages/ai/test/generate-models-strict.test.ts
// PORT: scripts/generate-models.ts --strict CLI is not ported per PORTING.md §2.6 (`rust: null`);
// this suite enforces the strict Individual allowlist and catalog consistency invariants
// directly on `qwen-token-plan-individual.json`, `.manifest.json`, and loaded models.

use indexmap::IndexSet;

use pi_ai::providers::all::get_builtin_models;
use pi_ai::providers::qwen_token_plan_individual_models::qwen_token_plan_individual_models;

const EXPECTED_INDIVIDUAL_IDS: &[&str] = &[
    "deepseek-v4-flash-0731",
    "deepseek-v4-pro",
    "deepseek-v4-pro-0813",
    "glm-5.2",
    "qwen3.6-flash",
    "qwen3.7-max",
    "qwen3.7-plus",
    "qwen3.8-flash",
    "qwen3.8-max",
];

#[test]
fn qwen_token_plan_individual_matches_exact_strict_allowlist() {
    let catalog = qwen_token_plan_individual_models();
    let actual: IndexSet<&str> = catalog.keys().map(String::as_str).collect();
    let expected: IndexSet<&str> = EXPECTED_INDIVIDUAL_IDS.iter().copied().collect();

    let missing: Vec<&str> = expected.difference(&actual).copied().collect();
    let extra: Vec<&str> = actual.difference(&expected).copied().collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "qwen-token-plan-individual model IDs do not match (missing: {missing:?}, extra: {extra:?})"
    );

    let builtin_ids: IndexSet<String> = get_builtin_models("qwen-token-plan-individual")
        .into_iter()
        .map(|m| m.id)
        .collect();
    assert_eq!(builtin_ids.len(), expected.len());
    for &id in EXPECTED_INDIVIDUAL_IDS {
        assert!(builtin_ids.contains(id), "missing builtin id {id}");
        let model = catalog.get(id).expect("model present");
        assert!(
            model.reasoning,
            "strict Individual allowlist expects reasoning enabled for {id}"
        );
    }
}

#[test]
#[ignore = "generate-models CLI/network script not ported (PORTING.md §2.6)"]
fn cli_strict_script_execution_not_applicable_in_rust() {}
