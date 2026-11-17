//! Port of packages/ai/test/google-thinking-disable.test.ts

use futures::StreamExt;
use pi_ai::api::google_generative_ai::stream_simple as stream_simple_google;
use pi_ai::api::google_shared::{GoogleApiThinkingLevel, get_disabled_google_thinking_config};
use pi_ai::api::google_vertex::stream_simple as stream_simple_vertex;
use pi_ai::providers::google_models::google_models;
use pi_ai::providers::google_vertex_models::google_vertex_models;
use pi_ai::utils::transcript::normalize_context;
use pi_ai::{
    AssistantContentBlock, AssistantMessageEvent, Context, Message, Model, SimpleStreamOptions, StopReason,
    StreamOptions, UserMessage,
};
use pretty_assertions::assert_eq;

fn make_e2e_context() -> Context {
    Context {
        system_prompt: Some(
            "You are a precise assistant. Follow the requested output format exactly.".to_owned(),
        ),
        messages: vec![Message::User(UserMessage {
            content: "Before replying, carefully solve 36863 * 5279 internally. Then reply with the word pong repeated exactly 40 times, separated by single spaces. Do not add any other text.".into(),
            timestamp: pi_js::time::now_ms(),
        })],
        tools: None,
    }
}

fn count_pongs(text: &str) -> usize {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|tok| tok.eq_ignore_ascii_case("pong"))
        .count()
}

async fn run_without_reasoning(
    is_vertex: bool,
    model: &Model,
    options: SimpleStreamOptions,
) -> (usize, usize, String, i64, Vec<&'static str>) {
    let ctx = normalize_context(&make_e2e_context());
    let mut stream = if is_vertex {
        stream_simple_vertex(model, &ctx, Some(options))
    } else {
        stream_simple_google(model, &ctx, Some(options))
    };

    let mut thinking_event_count = 0usize;
    let mut thinking_char_count = 0usize;
    while let Some(event) = stream.next().await {
        match event {
            AssistantMessageEvent::ThinkingStart { .. } | AssistantMessageEvent::ThinkingEnd { .. } => {
                thinking_event_count += 1;
            }
            AssistantMessageEvent::ThinkingDelta { delta, .. } => {
                thinking_event_count += 1;
                thinking_char_count += delta.len();
            }
            _ => {}
        }
    }

    let response = stream.result().await;
    assert_eq!(
        response.stop_reason,
        StopReason::Stop,
        "error: {:?}",
        response.error_message
    );

    let text: String = response
        .content
        .iter()
        .filter_map(|b| match b {
            AssistantContentBlock::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");
    let content_types: Vec<&'static str> = response
        .content
        .iter()
        .map(|b| match b {
            AssistantContentBlock::Text(_) => "text",
            AssistantContentBlock::Thinking(_) => "thinking",
            AssistantContentBlock::ToolCall(_) => "toolCall",
        })
        .collect();

    (
        thinking_event_count,
        thinking_char_count,
        text.trim().to_owned(),
        response.usage.output,
        content_types,
    )
}

async fn assert_thinking_disabled_e2e(
    is_vertex: bool,
    model: &Model,
    mut options: SimpleStreamOptions,
    min_pongs: usize,
) {
    if options.base.max_tokens.is_none() {
        options.base.max_tokens = Some(160);
    }
    if options.base.temperature.is_none() {
        options.base.temperature = Some(0.0);
    }

    let (thinking_events, thinking_chars, text, _output_tokens, content_types) =
        run_without_reasoning(is_vertex, model, options).await;

    assert_eq!(thinking_events, 0);
    assert_eq!(thinking_chars, 0);
    assert!(!content_types.contains(&"thinking"));
    assert!(count_pongs(&text) >= min_pongs, "text={text}");
}

#[test]
fn disabled_thinking_config_selects_budget_zero_or_minimum_level() {
    let g25 = google_models().get("gemini-2.5-flash").expect("gemini-2.5-flash");
    let cfg_25 = get_disabled_google_thinking_config(g25).expect("config 2.5");
    assert_eq!(cfg_25.thinking_budget, Some(0));
    assert_eq!(cfg_25.thinking_level, None);

    let g31_pro = google_models()
        .get("gemini-3.1-pro-preview")
        .expect("gemini-3.1-pro-preview");
    let cfg_31 = get_disabled_google_thinking_config(g31_pro).expect("config 3.1 pro");
    assert_eq!(cfg_31.thinking_level, Some(GoogleApiThinkingLevel::Low));
}

#[tokio::test]
#[ignore = "live e2e requires GEMINI_API_KEY"]
async fn google_thinking_disable_e2e_gemini_2_5() {
    let Some(api_key) = pi_js::env::var("GEMINI_API_KEY").filter(|k| !k.is_empty()) else {
        return;
    };
    let model = google_models().get("gemini-2.5-flash").expect("gemini-2.5-flash");
    let opts = SimpleStreamOptions {
        base: StreamOptions {
            api_key: Some(api_key),
            ..Default::default()
        },
        ..Default::default()
    };
    assert_thinking_disabled_e2e(false, model, opts, 35).await;
}

#[tokio::test]
#[ignore = "live e2e requires GEMINI_API_KEY"]
async fn google_thinking_disable_e2e_gemini_3_flash() {
    let Some(api_key) = pi_js::env::var("GEMINI_API_KEY").filter(|k| !k.is_empty()) else {
        return;
    };
    let model = google_models()
        .get("gemini-3-flash-preview")
        .expect("gemini-3-flash-preview");
    let opts = SimpleStreamOptions {
        base: StreamOptions {
            api_key: Some(api_key),
            ..Default::default()
        },
        ..Default::default()
    };
    assert_thinking_disabled_e2e(false, model, opts, 35).await;
}

#[tokio::test]
#[ignore = "live e2e requires GEMINI_API_KEY"]
async fn google_thinking_disable_e2e_gemini_3_1_pro() {
    let Some(api_key) = pi_js::env::var("GEMINI_API_KEY").filter(|k| !k.is_empty()) else {
        return;
    };
    let model = google_models()
        .get("gemini-3.1-pro-preview")
        .expect("gemini-3.1-pro-preview");
    let opts = SimpleStreamOptions {
        base: StreamOptions {
            api_key: Some(api_key),
            max_tokens: Some(512),
            ..Default::default()
        },
        ..Default::default()
    };
    assert_thinking_disabled_e2e(false, model, opts, 20).await;
}

#[tokio::test]
#[ignore = "live e2e requires Google Vertex credentials"]
async fn google_vertex_thinking_disable_e2e() {
    let api_key = pi_js::env::var("GOOGLE_CLOUD_API_KEY").filter(|k| !k.is_empty());
    let has_adc = (pi_js::env::var("GOOGLE_CLOUD_PROJECT").is_some() || pi_js::env::var("GCLOUD_PROJECT").is_some())
        && pi_js::env::var("GOOGLE_CLOUD_LOCATION").is_some();
    if api_key.is_none() && !has_adc {
        return;
    }

    for model_id in ["gemini-2.5-flash", "gemini-3-flash-preview"] {
        let model = google_vertex_models()
            .get(model_id)
            .expect("vertex catalog model should exist");
        let opts = SimpleStreamOptions {
            base: StreamOptions {
                api_key: api_key.clone(),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_thinking_disabled_e2e(true, model, opts, 35).await;
    }
}
