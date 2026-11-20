//! Port of packages/ai/test/azure-models.test.ts

use pi_ai::providers::azure_models::azure_models;
use pi_ai::providers::openai_models::openai_models;
use pi_ai::types::AnyModel;

#[test]
fn azure_catalog_keeps_openai_long_context_price_tier_on_gpt_6_sol() {
    let openai_sol = openai_models()
        .into_iter()
        .find_map(|m| match m {
            AnyModel::Chat(model) if model.id == "gpt-6-sol" => Some(model),
            _ => None,
        })
        .expect("openai gpt-6-sol model in catalog");

    let azure_sol = azure_models()
        .get("gpt-6-sol")
        .expect("azure gpt-6-sol model in catalog");

    assert_eq!(azure_sol.provider, "azure");
    assert_eq!(azure_sol.api, "azure-openai-responses");
    assert_eq!(azure_sol.context_window, 1_050_000);
    assert_eq!(azure_sol.cost, openai_sol.cost);
    assert!(azure_sol.cost.tiers.is_some());
}
