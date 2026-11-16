//! Port of packages/ai/test/pre-generation-error.test.ts (unit ai-providers-builtin)

use pi_ai::api::anthropic_messages::stream_simple as stream_anthropic;
use pi_ai::api::azure_openai_responses::stream_simple as stream_azure;
use pi_ai::api::openai_codex_responses::stream_simple as stream_codex;
use pi_ai::api::openai_completions::stream_simple as stream_openai_completions;
use pi_ai::api::openai_responses::stream_simple as stream_openai_responses;
use pi_ai::api::pi_messages::stream_simple as stream_pi_messages;
use pi_ai::types::{Context, Model, ModelCost, ModelInput, StopReason};
use pi_ai::utils::transcript::normalize_context;

fn model(api: &str) -> Model {
    Model {
        r#type: None,
        id: "test-model".to_string(),
        name: "Test".to_string(),
        api: api.to_string(),
        provider: "test-provider".into(),
        base_url: "https://example.invalid".to_string(),
        reasoning: false,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: ModelCost::default(),
        context_window: 1_000,
        max_tokens: 100,
        headers: None,
        sampling_params: None,
        compat: None,
    }
}

#[tokio::test]
async fn direct_apis_report_missing_auth_without_panicking() {
    let context = normalize_context(&Context {
        system_prompt: None,
        messages: Vec::new(),
        tools: None,
    });

    // WIRE(@mcp-w2): also cover google_generative_ai and mistral_conversations
    let streams = [
        (
            "anthropic-messages",
            stream_anthropic(&model("anthropic-messages"), &context, None),
            "No API key for provider: test-provider",
        ),
        (
            "azure-openai-responses",
            stream_azure(&model("azure-openai-responses"), &context, None),
            "API key",
        ),
        (
            "openai-codex-responses",
            stream_codex(&model("openai-codex-responses"), &context, None),
            "No API key for provider: test-provider",
        ),
        (
            "openai-completions",
            stream_openai_completions(&model("openai-completions"), &context, None),
            "No API key for provider: test-provider",
        ),
        (
            "openai-responses",
            stream_openai_responses(&model("openai-responses"), &context, None),
            "No API key for provider: test-provider",
        ),
        (
            "pi-messages",
            stream_pi_messages(&model("pi-messages"), &context, None),
            "No API key",
        ),
    ];

    for (name, stream, expected_sub) in streams {
        let msg = stream.result().await;
        assert_eq!(msg.stop_reason, StopReason::Error, "expected error for {name}");
        let err = msg.error_message.as_deref().unwrap_or("");
        assert!(
            err.contains(expected_sub),
            "{name} error '{err}' did not contain '{expected_sub}'"
        );
    }
}
