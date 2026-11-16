//! Port of packages/ai/test/opencode-provider-headers.test.ts

use std::sync::{Arc, Mutex};

use pi_ai::providers::faux::{FauxAssistantMessageContent, faux_assistant_message};
use pi_ai::providers::opencode_headers::with_open_code_session_header;
use pi_ai::types::{
    ApiStreamOptions, AssistantMessageEvent, CacheRetention, Context, Message, Model, ModelCost, ModelInput,
    ProviderHeaders, ProviderStreams, SimpleStreamFunction, SimpleStreamOptions, StopReason, StreamFunction,
    StreamOptions, TranscriptContext, UserMessage, UserMessageContent,
};
use pi_ai::utils::event_stream::{AssistantMessageEventStream, create_assistant_message_event_stream};
use pi_ai::utils::transcript::normalize_context;

fn fixture_model() -> Model {
    Model {
        r#type: None,
        id: "test-model".to_owned(),
        name: "Test model".to_owned(),
        api: "test-api".to_owned(),
        provider: "opencode".to_owned(),
        base_url: "https://opencode.ai/zen/v1".to_owned(),
        reasoning: false,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: ModelCost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            tiers: None,
        },
        context_window: 1000,
        max_tokens: 100,
        headers: None,
        sampling_params: None,
        compat: None,
    }
}

fn fixture_context() -> TranscriptContext {
    normalize_context(&Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("hi".to_owned()),
            timestamp: 0,
        })],
        tools: None,
    })
}

fn completed_stream() -> AssistantMessageEventStream {
    let stream = create_assistant_message_event_stream();
    let message = faux_assistant_message(FauxAssistantMessageContent::Text("ok".to_owned()), None);
    stream.push(AssistantMessageEvent::Start {
        partial: message.clone(),
    });
    stream.push(AssistantMessageEvent::Done {
        reason: StopReason::Stop,
        message: message.clone(),
    });
    stream.end(Some(message));
    stream
}

fn recording_streams(captured: Arc<Mutex<Option<StreamOptions>>>) -> ProviderStreams {
    let c1 = Arc::clone(&captured);
    let stream: StreamFunction = Arc::new(move |_model, _context, options: Option<ApiStreamOptions>| {
        *c1.lock().unwrap() = options.map(|o| o.base);
        completed_stream()
    });

    let c2 = captured;
    let stream_simple: SimpleStreamFunction =
        Arc::new(move |_model, _context, options: Option<SimpleStreamOptions>| {
            *c2.lock().unwrap() = options.map(|o| o.base);
            completed_stream()
        });

    ProviderStreams {
        stream,
        stream_simple,
        fetch_deferred: None,
        cancel_deferred: None,
    }
}

#[test]
fn maps_session_id_for_stream_and_stream_simple_even_without_cache_retention() {
    let model = fixture_model();
    let context = fixture_context();

    let captured_stream = Arc::new(Mutex::new(None));
    let streams = with_open_code_session_header(recording_streams(Arc::clone(&captured_stream)));
    let mut base = StreamOptions::default();
    base.session_id = Some("conversation-1".to_owned());
    base.cache_retention = Some(CacheRetention::None);
    let _ = streams.stream(&model, &context, Some(ApiStreamOptions::from(base)));
    let expected = Some("conversation-1".to_owned());
    assert_eq!(
        captured_stream
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|o| o.headers.as_ref())
            .and_then(|h| h.get("x-opencode-session")),
        Some(&expected)
    );

    let captured_simple = Arc::new(Mutex::new(None));
    let streams_simple = with_open_code_session_header(recording_streams(Arc::clone(&captured_simple)));
    let mut simple = SimpleStreamOptions::default();
    simple.base.session_id = Some("conversation-1".to_owned());
    simple.base.cache_retention = Some(CacheRetention::None);
    let _ = streams_simple.stream_simple(&model, &context, Some(simple));
    assert_eq!(
        captured_simple
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|o| o.headers.as_ref())
            .and_then(|h| h.get("x-opencode-session")),
        Some(&expected)
    );
}

#[test]
fn preserves_case_insensitive_caller_override() {
    let model = fixture_model();
    let context = fixture_context();

    for override_value in [Some("caller-value".to_owned()), None] {
        let captured = Arc::new(Mutex::new(None));
        let streams = with_open_code_session_header(recording_streams(Arc::clone(&captured)));
        let mut headers = ProviderHeaders::new();
        headers.insert("X-OpenCode-Session".to_owned(), override_value.clone());

        let mut opts = SimpleStreamOptions::default();
        opts.base.session_id = Some("generated-value".to_owned());
        opts.base.headers = Some(headers.clone());

        let _ = streams.stream_simple(&model, &context, Some(opts));
        assert_eq!(
            captured.lock().unwrap().as_ref().and_then(|o| o.headers.clone()),
            Some(headers)
        );
    }
}

#[test]
fn does_not_fabricate_session_header_when_session_id_is_absent() {
    let model = fixture_model();
    let context = fixture_context();
    let captured = Arc::new(Mutex::new(None));
    let streams = with_open_code_session_header(recording_streams(Arc::clone(&captured)));

    let mut headers = ProviderHeaders::new();
    headers.insert("x-custom".to_owned(), Some("value".to_owned()));
    let mut opts = SimpleStreamOptions::default();
    opts.base.headers = Some(headers.clone());

    let _ = streams.stream_simple(&model, &context, Some(opts));
    assert_eq!(
        captured.lock().unwrap().as_ref().and_then(|o| o.headers.clone()),
        Some(headers)
    );
}
