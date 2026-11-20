//! Port of packages/ai/test/supports-xhigh.test.ts (unit ai-providers-builtin)

use pi_ai::compat::{get_model, get_supported_thinking_levels};
use pi_ai::types::{ModelInput, ModelThinkingLevel};

#[test]
fn supported_thinking_levels_across_anthropic_openai_codex_and_catalog_providers() {
    let opus46 = get_model("anthropic", "claude-opus-4-6").unwrap();
    let l46 = get_supported_thinking_levels(&opus46);
    assert!(l46.contains(&ModelThinkingLevel::Max));
    assert!(!l46.contains(&ModelThinkingLevel::XHigh));

    for id in ["claude-opus-4-8", "claude-opus-5", "claude-sonnet-5"] {
        let m = get_model("anthropic", id).unwrap();
        let levels = get_supported_thinking_levels(&m);
        assert!(levels.contains(&ModelThinkingLevel::XHigh), "{id}");
        assert!(levels.contains(&ModelThinkingLevel::Max), "{id}");
    }

    let opus55 = get_model("anthropic", "claude-opus-5-5").unwrap();
    assert_eq!(opus55.cost.input, 4.0);
    assert_eq!(opus55.cost.output, 20.0);
    assert_eq!(opus55.cost.cache_read, 0.2);
    assert_eq!(opus55.cost.cache_write, 5.0);
    assert_eq!(opus55.context_window, 1_000_000);
    assert_eq!(opus55.max_tokens, 128_000);
    let c55 = opus55.compat.as_ref().unwrap().as_anthropic_messages().unwrap();
    assert_eq!(c55.force_adaptive_thinking, Some(true));
    assert_eq!(c55.supports_mid_convo_effort, Some(true));
    assert_eq!(c55.supports_mid_convo_system_messages, Some(true));
    assert_eq!(c55.supports_mid_convo_tool_changes, Some(true));
    assert_eq!(
        get_supported_thinking_levels(&opus55),
        vec![
            ModelThinkingLevel::Low,
            ModelThinkingLevel::Medium,
            ModelThinkingLevel::High,
            ModelThinkingLevel::XHigh,
            ModelThinkingLevel::Max,
        ]
    );

    let sonnet55 = get_model("anthropic", "claude-sonnet-5-5").unwrap();
    assert_eq!(sonnet55.cost.input, 2.0);
    assert_eq!(sonnet55.cost.output, 10.0);
    assert_eq!(sonnet55.cost.cache_read, 0.1);
    assert_eq!(sonnet55.cost.cache_write, 2.5);
    let cs55 = sonnet55.compat.as_ref().unwrap().as_anthropic_messages().unwrap();
    assert_eq!(cs55.supports_temperature, Some(false));
    assert_eq!(
        get_supported_thinking_levels(&sonnet55),
        vec![
            ModelThinkingLevel::Low,
            ModelThinkingLevel::Medium,
            ModelThinkingLevel::High,
            ModelThinkingLevel::XHigh,
            ModelThinkingLevel::Max,
        ]
    );

    let sonnet46 = get_model("anthropic", "claude-sonnet-4-6").unwrap();
    assert!(get_supported_thinking_levels(&sonnet46).contains(&ModelThinkingLevel::Max));
    assert!(!get_supported_thinking_levels(&sonnet46).contains(&ModelThinkingLevel::XHigh));

    let fable5 = get_model("anthropic", "claude-fable-5").unwrap();
    let lf = get_supported_thinking_levels(&fable5);
    assert!(lf.contains(&ModelThinkingLevel::XHigh));
    assert!(lf.contains(&ModelThinkingLevel::Max));
    assert!(!lf.contains(&ModelThinkingLevel::Off));

    let sonnet45 = get_model("anthropic", "claude-sonnet-4-5").unwrap();
    assert!(!get_supported_thinking_levels(&sonnet45).contains(&ModelThinkingLevel::XHigh));
    assert!(!get_supported_thinking_levels(&sonnet45).contains(&ModelThinkingLevel::Max));

    for model_id in [
        "gpt-5.6-sol",
        "gpt-5.6-terra",
        "gpt-5.6-luna",
        "gpt-6-astra",
        "gpt-6-sol",
        "gpt-6-luna",
        "gpt-6.1-sol",
        "gpt-6.1-terra",
    ] {
        let m = get_model("openai-codex", model_id).unwrap();
        assert!(
            get_supported_thinking_levels(&m).contains(&ModelThinkingLevel::XHigh),
            "{model_id}"
        );
    }

    for model_id in [
        "gpt-5.6-sol",
        "gpt-5.6-terra",
        "gpt-5.6-luna",
        "gpt-6-sol",
        "gpt-6-luna",
        "gpt-6.1-terra",
    ] {
        let m = get_model("openai", model_id).unwrap();
        assert_eq!(
            get_supported_thinking_levels(&m),
            vec![
                ModelThinkingLevel::Off,
                ModelThinkingLevel::Low,
                ModelThinkingLevel::Medium,
                ModelThinkingLevel::High,
                ModelThinkingLevel::XHigh,
                ModelThinkingLevel::Max,
            ]
        );
    }

    let expected_61 = [
        (
            "openai",
            vec![
                ModelThinkingLevel::Low,
                ModelThinkingLevel::Medium,
                ModelThinkingLevel::High,
                ModelThinkingLevel::XHigh,
                ModelThinkingLevel::Max,
            ],
        ),
        (
            "azure",
            vec![
                ModelThinkingLevel::Low,
                ModelThinkingLevel::Medium,
                ModelThinkingLevel::High,
                ModelThinkingLevel::XHigh,
                ModelThinkingLevel::Max,
            ],
        ),
        (
            "openai-codex",
            vec![
                ModelThinkingLevel::Minimal,
                ModelThinkingLevel::Low,
                ModelThinkingLevel::Medium,
                ModelThinkingLevel::High,
                ModelThinkingLevel::XHigh,
                ModelThinkingLevel::Max,
            ],
        ),
    ];
    for (prov, levels) in expected_61 {
        let m = get_model(prov, "gpt-6.1-sol").unwrap();
        assert_eq!(get_supported_thinking_levels(&m), levels);
        assert_eq!(
            m.thinking_level_map
                .as_ref()
                .and_then(|t| t.get(&ModelThinkingLevel::Off)),
            Some(&None)
        );
    }

    for prov in ["openai", "azure", "openai-codex"] {
        let terra = get_model(prov, "gpt-6.1-terra").unwrap();
        let tmap = terra.thinking_level_map.as_ref().unwrap();
        assert_eq!(tmap.get(&ModelThinkingLevel::Off), Some(&Some("none".to_owned())));
        assert_eq!(
            tmap.get(&ModelThinkingLevel::Minimal),
            Some(&if prov == "openai-codex" {
                Some("low".to_owned())
            } else {
                None
            })
        );
    }

    for (model_id, input_c, output_c, cr, cw, codex_ctx) in [
        ("gpt-6-sol", 2.0, 10.0, 0.2, 2.5, 272_000),
        ("gpt-6-luna", 0.1, 0.5, 0.01, 0.125, 272_000),
        ("gpt-6.1-sol", 2.0, 10.0, 0.1, 2.5, 500_000),
        ("gpt-6.1-terra", 1.0, 5.0, 0.05, 1.25, 500_000),
    ] {
        for provider in ["openai", "openai-codex"] {
            let m = get_model(provider, model_id).unwrap();
            assert_eq!(m.input, vec![ModelInput::Text, ModelInput::Image]);
            assert_eq!(m.cost.input, input_c);
            assert_eq!(m.cost.output, output_c);
            assert_eq!(m.cost.cache_read, cr);
            assert_eq!(m.cost.cache_write, cw);
            let tier = &m.cost.tiers.as_ref().unwrap()[0];
            assert_eq!(tier.input_tokens_above, 272000);
            assert_eq!(tier.input, input_c * 2.0);
            assert_eq!(tier.output, output_c * 1.5);
            assert_eq!(
                m.context_window,
                if provider == "openai-codex" { codex_ctx } else { 272000 }
            );
            assert_eq!(m.max_tokens, 128000);
            let rc = m.compat.as_ref().unwrap().as_openai_responses().unwrap();
            assert_eq!(rc.supports_additional_tools, Some(true));
            assert_eq!(rc.supports_mid_convo_system_messages, Some(true));
            assert_eq!(rc.supports_open_ai_grammar_tools, Some(true));
            assert_eq!(rc.supports_tool_search, Some(true));
        }
    }

    for model_id in ["gpt-6-astra", "gpt-6-luna", "gpt-6-sol", "gpt-6.1-sol", "gpt-6.1-terra"] {
        assert_eq!(get_model("azure", model_id).unwrap().context_window, 1_050_000);
    }

    assert_eq!(
        get_supported_thinking_levels(&get_model("openai", "gpt-5.5-pro").unwrap()),
        vec![
            ModelThinkingLevel::Medium,
            ModelThinkingLevel::High,
            ModelThinkingLevel::XHigh
        ]
    );
    assert_eq!(
        get_supported_thinking_levels(&get_model("openrouter", "openai/gpt-5.5-pro").unwrap()),
        vec![
            ModelThinkingLevel::Medium,
            ModelThinkingLevel::High,
            ModelThinkingLevel::XHigh
        ]
    );
    assert_eq!(
        get_supported_thinking_levels(&get_model("deepseek", "deepseek-flash").unwrap()),
        vec![
            ModelThinkingLevel::Off,
            ModelThinkingLevel::Low,
            ModelThinkingLevel::High,
            ModelThinkingLevel::Max
        ]
    );
    assert_eq!(
        get_supported_thinking_levels(&get_model("openrouter", "deepseek/deepseek-v4.1-flash").unwrap()),
        vec![
            ModelThinkingLevel::Off,
            ModelThinkingLevel::Low,
            ModelThinkingLevel::High,
            ModelThinkingLevel::Max
        ]
    );
    assert_eq!(
        get_supported_thinking_levels(&get_model("opencode-go", "deepseek-v4.1-flash").unwrap()),
        vec![
            ModelThinkingLevel::Low,
            ModelThinkingLevel::High,
            ModelThinkingLevel::Max
        ]
    );
    for prov in ["moonshotai", "moonshotai-cn"] {
        assert_eq!(
            get_supported_thinking_levels(&get_model(prov, "kimi-k2.7-code").unwrap()),
            vec![
                ModelThinkingLevel::Minimal,
                ModelThinkingLevel::Low,
                ModelThinkingLevel::Medium,
                ModelThinkingLevel::High
            ]
        );
        assert_eq!(
            get_supported_thinking_levels(&get_model(prov, "kimi-k3").unwrap()),
            vec![
                ModelThinkingLevel::Low,
                ModelThinkingLevel::High,
                ModelThinkingLevel::Max
            ]
        );
    }
    assert_eq!(
        get_supported_thinking_levels(&get_model("kimi-coding", "k3").unwrap()),
        vec![
            ModelThinkingLevel::Low,
            ModelThinkingLevel::High,
            ModelThinkingLevel::Max
        ]
    );
    assert!(
        !get_supported_thinking_levels(&get_model("kimi-coding", "kimi-for-coding-highspeed").unwrap())
            .contains(&ModelThinkingLevel::Off)
    );
    assert_eq!(
        get_supported_thinking_levels(&get_model("opencode", "grok-build-0.1").unwrap()),
        vec![ModelThinkingLevel::High]
    );
    assert_eq!(
        get_supported_thinking_levels(&get_model("openrouter", "deepseek/deepseek-v4-flash").unwrap()),
        vec![
            ModelThinkingLevel::Off,
            ModelThinkingLevel::High,
            ModelThinkingLevel::XHigh
        ]
    );
    let or_opus = get_model("openrouter", "anthropic/claude-opus-4.6").unwrap();
    assert!(get_supported_thinking_levels(&or_opus).contains(&ModelThinkingLevel::Max));
    assert!(!get_supported_thinking_levels(&or_opus).contains(&ModelThinkingLevel::XHigh));

    let bedrock_opus5 = get_model("amazon-bedrock", "global.anthropic.claude-opus-5").unwrap();
    assert!(get_supported_thinking_levels(&bedrock_opus5).contains(&ModelThinkingLevel::XHigh));
    assert!(get_supported_thinking_levels(&bedrock_opus5).contains(&ModelThinkingLevel::Max));

    assert_eq!(
        get_supported_thinking_levels(&get_model("xai", "grok-4.6").unwrap()),
        vec![
            ModelThinkingLevel::Low,
            ModelThinkingLevel::Medium,
            ModelThinkingLevel::High,
            ModelThinkingLevel::XHigh
        ]
    );

    let bedrock_fable5 = get_model("amazon-bedrock", "global.anthropic.claude-fable-5").unwrap();
    let bf = get_supported_thinking_levels(&bedrock_fable5);
    assert!(bf.contains(&ModelThinkingLevel::XHigh));
    assert!(bf.contains(&ModelThinkingLevel::Max));
    assert!(!bf.contains(&ModelThinkingLevel::Off));
}
