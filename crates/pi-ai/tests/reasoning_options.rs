//! Port of packages/ai/test/reasoning-options.test.ts (unit ai-providers-builtin)

use pi_ai::compat::{get_model, get_supported_thinking_levels};
use pi_ai::types::ModelThinkingLevel;

#[test]
fn verifies_catalog_thinking_level_maps_and_disabling_semantics() {
    // Models exposing verified effort values and explicit off: "none"
    let copilot_sol = get_model("github-copilot", "gpt-6-sol").expect("gpt-6-sol");
    let tmap = copilot_sol.thinking_level_map.as_ref().expect("thinking_level_map");
    assert_eq!(
        tmap.get(&ModelThinkingLevel::Off).and_then(|v| v.as_deref()),
        Some("none")
    );
    assert!(get_supported_thinking_levels(&copilot_sol).contains(&ModelThinkingLevel::Off));

    // Models marking thinking off as unavailable (null -> None in ThinkingLevelMap)
    let gpt61 = get_model("openai", "gpt-6.1-sol").expect("gpt-6.1-sol");
    assert_eq!(
        gpt61
            .thinking_level_map
            .as_ref()
            .and_then(|m| m.get(&ModelThinkingLevel::Off)),
        Some(&None)
    );
    assert!(!get_supported_thinking_levels(&gpt61).contains(&ModelThinkingLevel::Off));
}
