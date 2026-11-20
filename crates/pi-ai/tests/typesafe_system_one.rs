//! Port of packages/ai/test/typesafe-system-one.test.ts

use std::future::pending;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use serde_json::{Value, json};

use pi_ai::api::typesafe_system_one::classify;
use pi_ai::types::{
    ClassifierAnswer, ClassifierBoolCriteria, ClassifierContext, ClassifierModel, ClassifierOptions,
    ClassifierQuestion, ClassifierStopReason, FetchFunction, ImageContent, ModelCost, ModelModality, ProviderHeaders,
    SystemOneCompat, SystemOneImageInput,
};
use pi_js::abort::AbortSignal;
use pi_js::fetch::{Headers, Response};

fn base_model() -> ClassifierModel {
    ClassifierModel {
        id: "jev-latest".to_owned(),
        name: "Jev".to_owned(),
        api: "typesafe-system-one".to_owned(),
        provider: "typesafe".to_owned(),
        base_url: "https://api.typesafe.ai/v1/".to_owned(),
        input: vec![ModelModality::Text],
        input_limits: None,
        context_window: 64000,
        cost: ModelCost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            tiers: None,
        },
        headers: None,
        compat: None,
    }
}

fn sample_context() -> ClassifierContext {
    let mut state = IndexMap::new();
    state.insert(
        "text".to_owned(),
        Value::String("The deployment succeeded, thank you.".to_owned()),
    );

    let mut category_criteria = IndexMap::new();
    category_criteria.insert("success".to_owned(), "Successful".to_owned());
    category_criteria.insert("failure".to_owned(), "Failed".to_owned());

    let mut questions = IndexMap::new();
    questions.insert(
        "category".to_owned(),
        ClassifierQuestion::Choice {
            instructions: "Classify the message".to_owned(),
            criteria: category_criteria,
        },
    );
    questions.insert(
        "satisfaction".to_owned(),
        ClassifierQuestion::Score {
            instructions: "Score satisfaction".to_owned(),
            criteria: vec!["low".to_owned(), "neutral".to_owned(), "high".to_owned()],
        },
    );
    questions.insert(
        "approved".to_owned(),
        ClassifierQuestion::Bool {
            instructions: "Does the user approve?".to_owned(),
            criteria: ClassifierBoolCriteria {
                r#true: "Approval".to_owned(),
                r#false: "No approval".to_owned(),
            },
        },
    );

    ClassifierContext {
        state,
        questions,
        images: None,
    }
}

fn wire_answers() -> Value {
    json!({
        "category": {
            "type": "choice",
            "choice": "success",
            "probabilities": { "success": 0.9, "failure": 0.1 },
            "confidence": 0.8,
        },
        "satisfaction": { "type": "score", "score": 2, "confidence": 0.7 },
        "approved": { "type": "noul", "noul": 0.95 },
    })
}

fn opts_with_fetch(api_key: &str, fetch_fn: FetchFunction) -> ClassifierOptions {
    let mut opts = ClassifierOptions::default();
    opts.base.api_key = Some(api_key.to_owned());
    opts.base.fetch = Some(fetch_fn);
    opts
}

#[tokio::test]
async fn maps_public_bool_questions_and_answers_to_typesafe_noul_values() {
    let model = base_model();
    let context = sample_context();
    let urls = Arc::new(Mutex::new(Vec::<String>::new()));
    let urls_ref = Arc::clone(&urls);

    let fetch_fn: FetchFunction = Arc::new(move |req| {
        let urls_ref = Arc::clone(&urls_ref);
        Box::pin(async move {
            urls_ref.lock().unwrap().push(req.url.clone());
            let payload = req.body_json().expect("json payload");
            assert_eq!(payload["model"], "jev-latest");
            assert_eq!(payload["questions"]["category"]["type"], "choice");
            assert_eq!(payload["questions"]["satisfaction"]["type"], "score");
            assert_eq!(payload["questions"]["approved"]["type"], "noul");
            assert!(payload.as_object().unwrap().get("temperature").is_none());
            assert_eq!(req.headers.get("authorization").as_deref(), Some("Bearer secret"));
            Ok(Response::new(
                200,
                Headers::from_pairs([("content-type", "application/json")]),
                pi_js::json::stringify(&json!({ "answers": wire_answers() })),
            ))
        })
    });

    let mut opts = opts_with_fetch("secret", fetch_fn);
    opts.temperature = Some(1.5);
    let result = classify(&model, &context, Some(opts)).await;

    let mut priced_model = model.clone();
    priced_model.cost.input = 0.042;
    let priced_fetch: FetchFunction = Arc::new(move |_req| {
        Box::pin(async move {
            Ok(Response::new(
                200,
                Headers::from_pairs([("content-type", "application/json")]),
                pi_js::json::stringify(&json!({
                    "answers": wire_answers(),
                    "usage": { "input_tokens": 308, "output_tokens": 23 },
                })),
            ))
        })
    });
    let priced_result = classify(&priced_model, &context, Some(opts_with_fetch("secret", priced_fetch))).await;

    assert_eq!(
        *urls.lock().unwrap(),
        vec!["https://api.typesafe.ai/v1/systemone".to_owned()]
    );
    assert_eq!(result.stop_reason, ClassifierStopReason::Stop);
    assert_eq!(
        result.answers.get("approved"),
        Some(&ClassifierAnswer::Bool { probability: 0.95 })
    );
    match result.answers.get("category") {
        Some(ClassifierAnswer::Choice { choice, .. }) => assert_eq!(choice, "success"),
        other => panic!("expected choice answer, got {other:?}"),
    }
    assert_eq!(
        result.answers.get("satisfaction"),
        Some(&ClassifierAnswer::Score {
            score: 2.0,
            confidence: 0.7,
        })
    );
    assert!(result.usage.is_none());

    let usage = priced_result.usage.expect("priced usage");
    assert_eq!(usage.input, 308);
    assert_eq!(usage.output, 23);
    assert_eq!(usage.total_tokens, 331);
    assert!((usage.cost.total - 0.000012936).abs() < 1e-12);
}

#[tokio::test]
async fn posts_openrouter_system_one_requests_to_its_typesafe_compatible_endpoint() {
    let context = sample_context();
    let expected_state = context.state.clone();
    let urls = Arc::new(Mutex::new(Vec::<String>::new()));
    let urls_ref = Arc::clone(&urls);

    let fetch_fn: FetchFunction = Arc::new(move |req| {
        let urls_ref = Arc::clone(&urls_ref);
        let expected_state = expected_state.clone();
        Box::pin(async move {
            urls_ref.lock().unwrap().push(req.url.clone());
            let body = req.body_json().expect("json payload");
            assert_eq!(body["model"], "typesafe/jev-1.13");
            assert_eq!(body["state"], json!(expected_state));
            Ok(Response::new(
                200,
                Headers::from_pairs([("content-type", "application/json")]),
                pi_js::json::stringify(&json!({
                    "id": "gen-dec-1",
                    "provider": "TypeSafe",
                    "answers": wire_answers(),
                    "usage": { "input_tokens": 308, "output_tokens": 23, "cost": 0.000012936 },
                })),
            ))
        })
    });

    let mut openrouter_model = base_model();
    openrouter_model.id = "typesafe/jev-1.13".to_owned();
    openrouter_model.provider = "openrouter".to_owned();
    openrouter_model.base_url = "https://openrouter.ai/api/v1".to_owned();
    openrouter_model.cost.input = 0.042;

    let result = classify(&openrouter_model, &context, Some(opts_with_fetch("secret", fetch_fn))).await;

    assert_eq!(
        *urls.lock().unwrap(),
        vec!["https://openrouter.ai/api/v1/systemone".to_owned()]
    );
    assert_eq!(result.stop_reason, ClassifierStopReason::Stop);
    assert_eq!(
        result.answers.get("approved"),
        Some(&ClassifierAnswer::Bool { probability: 0.95 })
    );
    let total_cost = result.usage.expect("usage").cost.total;
    assert!((total_cost - 0.000012936).abs() < 1e-12);
}

#[tokio::test]
async fn rejects_models_for_other_classifier_apis() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_ref = Arc::clone(&calls);
    let fetch_fn: FetchFunction = Arc::new(move |_req| {
        calls_ref.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            Ok(Response::new(
                200,
                Headers::new(),
                pi_js::json::stringify(&json!({ "answers": wire_answers() })),
            ))
        })
    });
    let mut wrong_model = base_model();
    wrong_model.api = "cloudflare-workers-ai-system-one".to_owned();

    let result = classify(
        &wrong_model,
        &sample_context(),
        Some(opts_with_fetch("secret", fetch_fn)),
    )
    .await;

    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(result.stop_reason, ClassifierStopReason::Error);
    assert!(
        result
            .error_message
            .as_deref()
            .unwrap_or_default()
            .contains("Unsupported classifier API: cloudflare-workers-ai-system-one")
    );
}

#[tokio::test]
async fn merges_headers_case_insensitively_and_supports_null_suppression() {
    let recorded = Arc::new(Mutex::new(Vec::<IndexMap<String, String>>::new()));
    let recorded_ref = Arc::clone(&recorded);
    let fetch_fn: FetchFunction = Arc::new(move |req| {
        let recorded_ref = Arc::clone(&recorded_ref);
        Box::pin(async move {
            let map: IndexMap<String, String> = req.headers.entries().into_iter().collect();
            recorded_ref.lock().unwrap().push(map);
            Ok(Response::new(
                200,
                Headers::from_pairs([("content-type", "application/json")]),
                pi_js::json::stringify(&json!({ "answers": wire_answers() })),
            ))
        })
    });

    let mut model = base_model();
    let mut model_headers = IndexMap::new();
    model_headers.insert("authorization".to_owned(), "Bearer model".to_owned());
    model_headers.insert("X-Source".to_owned(), "model".to_owned());
    model.headers = Some(model_headers);

    let mut h1 = ProviderHeaders::new();
    h1.insert("Authorization".to_owned(), Some("Bearer request".to_owned()));
    h1.insert("x-source".to_owned(), Some("request".to_owned()));
    let mut opts1 = opts_with_fetch("secret", Arc::clone(&fetch_fn));
    opts1.base.headers = Some(h1);
    classify(&model, &sample_context(), Some(opts1)).await;

    let mut h2 = ProviderHeaders::new();
    h2.insert("Authorization".to_owned(), None);
    let mut opts2 = opts_with_fetch("secret", fetch_fn);
    opts2.base.headers = Some(h2);
    classify(&model, &sample_context(), Some(opts2)).await;

    let guard = recorded.lock().unwrap();
    assert_eq!(
        guard[0].get("authorization").map(String::as_str),
        Some("Bearer request")
    );
    assert_eq!(guard[0].get("x-source").map(String::as_str), Some("request"));
    assert_eq!(
        guard[0]
            .keys()
            .filter(|name| name.to_ascii_lowercase() == "authorization")
            .count(),
        1
    );
    assert!(!guard[1].contains_key("authorization"));
}

#[tokio::test]
async fn preserves_prototype_sensitive_question_ids_in_answers() {
    let prototype_questions: IndexMap<String, ClassifierQuestion> = pi_js::json::parse_as(
        r#"{"__proto__":{"type":"bool","instructions":"Is this true?","criteria":{"true":"Yes","false":"No"}}}"#,
    )
    .unwrap();
    let proto_ctx = ClassifierContext {
        state: IndexMap::new(),
        questions: prototype_questions,
        images: None,
    };

    let fetch_fn: FetchFunction = Arc::new(move |_req| {
        Box::pin(async move {
            Ok(Response::new(
                200,
                Headers::from_pairs([("content-type", "application/json")]),
                r#"{"answers":{"__proto__":{"type":"noul","noul":0.75}}}"#,
            ))
        })
    });

    let result = classify(&base_model(), &proto_ctx, Some(opts_with_fetch("secret", fetch_fn))).await;

    assert_eq!(result.stop_reason, ClassifierStopReason::Stop);
    let keys: Vec<&String> = result.answers.keys().collect();
    assert_eq!(keys, vec!["__proto__"]);
    assert_eq!(
        result.answers.get("__proto__"),
        Some(&ClassifierAnswer::Bool { probability: 0.75 })
    );
    let serialized = pi_js::json::parse(&pi_js::json::stringify(&result.answers)).unwrap();
    assert_eq!(serialized["__proto__"], json!({ "type": "bool", "probability": 0.75 }));
}

#[tokio::test]
async fn reports_request_timeouts_separately_from_caller_cancellation() {
    let fetch_fn: FetchFunction = Arc::new(move |req| {
        Box::pin(async move {
            let signal = req.signal.expect("missing request signal");
            signal.cancelled().await;
            let _ = signal;
            pending::<pi_js::Result<Response>>().await
        })
    });

    let mut opts = opts_with_fetch("secret", fetch_fn);
    opts.base.timeout_ms = Some(5);
    opts.base.max_retries = Some(0);

    let result = classify(&base_model(), &sample_context(), Some(opts)).await;

    assert_eq!(result.stop_reason, ClassifierStopReason::Error);
    assert_eq!(result.error_message.as_deref(), Some("Request timed out after 5ms"));
}

#[tokio::test]
async fn creates_a_fresh_timeout_for_every_retry_attempt() {
    let signals = Arc::new(Mutex::new(Vec::<AbortSignal>::new()));
    let signals_ref = Arc::clone(&signals);
    let attempt = Arc::new(AtomicUsize::new(0));
    let attempt_ref = Arc::clone(&attempt);

    let fetch_fn: FetchFunction = Arc::new(move |req| {
        let signals_ref = Arc::clone(&signals_ref);
        let attempt_ref = Arc::clone(&attempt_ref);
        Box::pin(async move {
            let sig = req.signal.expect("missing request signal");
            // When attempt 2 runs after a 40ms retry delay (> 25ms timeout), attempt 1's signal
            // has aborted while attempt 2's fresh timeout signal must still be active.
            let n = attempt_ref.fetch_add(1, Ordering::SeqCst) + 1;
            if n == 2 {
                assert!(signals_ref.lock().unwrap()[0].aborted());
                assert!(!sig.aborted());
            }
            signals_ref.lock().unwrap().push(sig);
            if n == 1 {
                Ok(Response::new(
                    500,
                    Headers::from_pairs([("retry-after-ms", "40")]),
                    "retry",
                ))
            } else {
                Ok(Response::new(
                    200,
                    Headers::from_pairs([("content-type", "application/json")]),
                    pi_js::json::stringify(&json!({ "answers": wire_answers() })),
                ))
            }
        })
    });

    let mut opts = opts_with_fetch("secret", fetch_fn);
    opts.base.timeout_ms = Some(25);
    opts.base.max_retries = Some(1);

    let result = classify(&base_model(), &sample_context(), Some(opts)).await;

    assert_eq!(result.stop_reason, ClassifierStopReason::Stop);
    assert_eq!(signals.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn returns_malformed_responses_as_classifier_errors() {
    let fetch_fn: FetchFunction = Arc::new(move |_req| {
        Box::pin(async move {
            Ok(Response::new(
                200,
                Headers::from_pairs([("content-type", "application/json")]),
                pi_js::json::stringify(&json!({
                    "answers": {},
                    "usage": { "input_tokens": 10, "output_tokens": 2 },
                })),
            ))
        })
    });

    let result = classify(
        &base_model(),
        &sample_context(),
        Some(opts_with_fetch("secret", fetch_fn)),
    )
    .await;

    assert_eq!(result.stop_reason, ClassifierStopReason::Error);
    assert!(result.answers.is_empty());
    assert!(
        result
            .error_message
            .as_deref()
            .unwrap_or_default()
            .contains("did not return an answer for category")
    );
    let usage = result.usage.expect("billed usage preserved");
    assert_eq!(usage.input, 10);
    assert_eq!(usage.output, 2);
}

#[tokio::test]
async fn ignores_malformed_usage() {
    let fetch_bad: FetchFunction = Arc::new(move |_req| {
        Box::pin(async move {
            Ok(Response::new(
                200,
                Headers::from_pairs([("content-type", "application/json")]),
                pi_js::json::stringify(&json!({
                    "answers": wire_answers(),
                    "usage": { "input_tokens": "many", "output_tokens": 3 },
                })),
            ))
        })
    });
    let fetch_no_tokens: FetchFunction = Arc::new(move |_req| {
        Box::pin(async move {
            Ok(Response::new(
                200,
                Headers::from_pairs([("content-type", "application/json")]),
                pi_js::json::stringify(&json!({
                    "answers": wire_answers(),
                    "usage": { "cost": 0.1 },
                })),
            ))
        })
    });

    let result = classify(
        &base_model(),
        &sample_context(),
        Some(opts_with_fetch("secret", fetch_bad)),
    )
    .await;
    let without_tokens = classify(
        &base_model(),
        &sample_context(),
        Some(opts_with_fetch("secret", fetch_no_tokens)),
    )
    .await;

    assert_eq!(result.stop_reason, ClassifierStopReason::Stop);
    let usage = result.usage.expect("usage");
    assert_eq!(usage.input, 0);
    assert_eq!(usage.output, 3);
    assert_eq!(usage.total_tokens, 3);
    assert_eq!(without_tokens.stop_reason, ClassifierStopReason::Stop);
    assert!(without_tokens.usage.is_none());
}

#[tokio::test]
async fn rejects_image_input_for_text_only_models_before_sending() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_ref = Arc::clone(&calls);
    let fetch_fn: FetchFunction = Arc::new(move |_req| {
        calls_ref.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            Ok(Response::new(
                200,
                Headers::new(),
                pi_js::json::stringify(&json!({ "answers": wire_answers() })),
            ))
        })
    });
    let mut ctx = sample_context();
    ctx.images = Some(vec![ImageContent::new("aW1hZ2U=", "image/png")]);
    let result = classify(&base_model(), &ctx, Some(opts_with_fetch("secret", fetch_fn))).await;

    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(result.stop_reason, ClassifierStopReason::Error);
    assert_eq!(
        result.error_message.as_deref(),
        Some("System One API does not support image input for jev-latest")
    );
}

#[tokio::test]
async fn sends_images_to_typesafe_as_assets_with_the_opt_in_header() {
    let captured = Arc::new(Mutex::new(Vec::<(Value, Headers)>::new()));
    let cap_ref = Arc::clone(&captured);
    let fetch_fn: FetchFunction = Arc::new(move |req| {
        let cap_ref = Arc::clone(&cap_ref);
        Box::pin(async move {
            cap_ref
                .lock()
                .unwrap()
                .push((req.body_json().unwrap_or(Value::Null), req.headers));
            Ok(Response::new(
                200,
                Headers::from_pairs([("content-type", "application/json")]),
                pi_js::json::stringify(&json!({ "answers": wire_answers() })),
            ))
        })
    });
    let mut image_model = base_model();
    image_model.input = vec![ModelModality::Text, ModelModality::Image];
    let base_ctx = sample_context();
    let mut with_images = base_ctx.clone();
    with_images.images = Some(vec![
        ImageContent::new("cG5n", "image/png"),
        ImageContent::new("anBlZw==", "image/jpeg"),
    ]);

    let result = classify(
        &image_model,
        &with_images,
        Some(opts_with_fetch("secret", Arc::clone(&fetch_fn))),
    )
    .await;
    let without_images = classify(&image_model, &base_ctx, Some(opts_with_fetch("secret", fetch_fn))).await;

    assert_eq!(result.stop_reason, ClassifierStopReason::Stop);
    assert_eq!(without_images.stop_reason, ClassifierStopReason::Stop);
    let guard = captured.lock().unwrap();
    assert_eq!(guard[0].0["state"], json!(["<image-1>", "<image-2>", base_ctx.state]));
    assert_eq!(
        guard[0].0["assets"],
        json!({
            "<image-1>": { "type": "image", "format": "png", "data_base64": "cG5n" },
            "<image-2>": { "type": "image", "format": "jpeg", "data_base64": "anBlZw==" },
        })
    );
    assert_eq!(guard[0].1.get("typesafe-danger-zone-images").as_deref(), Some("true"));
    assert_eq!(guard[1].0["state"], json!(base_ctx.state));
    assert!(guard[1].0.get("assets").is_none());
    assert!(guard[1].1.get("typesafe-danger-zone-images").is_none());
}

#[tokio::test]
async fn rejects_image_formats_typesafe_does_not_accept_before_sending() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_ref = Arc::clone(&calls);
    let fetch_fn: FetchFunction = Arc::new(move |_req| {
        calls_ref.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            Ok(Response::new(
                200,
                Headers::new(),
                pi_js::json::stringify(&json!({ "answers": wire_answers() })),
            ))
        })
    });
    let mut image_model = base_model();
    image_model.input = vec![ModelModality::Text, ModelModality::Image];
    let mut ctx = sample_context();
    ctx.images = Some(vec![ImageContent::new("d2VicA==", "image/webp")]);

    let result = classify(&image_model, &ctx, Some(opts_with_fetch("secret", fetch_fn))).await;

    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(result.stop_reason, ClassifierStopReason::Error);
    assert_eq!(
        result.error_message.as_deref(),
        Some("System One API accepts PNG and JPEG images for jev-latest, got image/webp")
    );
}

#[tokio::test]
async fn sends_images_to_openrouter_as_image_url_parts_and_supports_proxy_overrides() {
    let base_ctx = sample_context();
    let cases = [
        ("openrouter", "https://openrouter.ai/api/v1", None),
        ("openrouter", "https://proxy.example.com/v1", None),
        ("custom", "https://us.openrouter.ai/api/v1", None),
        (
            "custom",
            "https://proxy.example.com/v1",
            Some(SystemOneCompat {
                image_input: Some(SystemOneImageInput::StateImageUrls),
                send_image_opt_in_header: Some(false),
            }),
        ),
    ];

    for (provider, base_url, compat) in cases {
        let captured = Arc::new(Mutex::new(Vec::<(Value, Headers)>::new()));
        let cap_ref = Arc::clone(&captured);
        let fetch_fn: FetchFunction = Arc::new(move |req| {
            let cap_ref = Arc::clone(&cap_ref);
            Box::pin(async move {
                cap_ref
                    .lock()
                    .unwrap()
                    .push((req.body_json().unwrap_or(Value::Null), req.headers));
                Ok(Response::new(
                    200,
                    Headers::from_pairs([("content-type", "application/json")]),
                    pi_js::json::stringify(&json!({ "answers": wire_answers() })),
                ))
            })
        });
        let mut m = base_model();
        m.id = "typesafe/jev-1.14".to_owned();
        m.provider = provider.to_owned();
        m.base_url = base_url.to_owned();
        m.input = vec![ModelModality::Text, ModelModality::Image];
        m.compat = compat;
        let mut ctx = base_ctx.clone();
        ctx.images = Some(vec![ImageContent::new("cG5n", "image/png")]);

        let result = classify(&m, &ctx, Some(opts_with_fetch("secret", fetch_fn))).await;

        assert_eq!(result.stop_reason, ClassifierStopReason::Stop);
        let guard = captured.lock().unwrap();
        assert_eq!(
            guard[0].0["state"],
            json!([{ "type": "image_url", "image_url": { "url": "data:image/png;base64,cG5n" } }, base_ctx.state])
        );
        assert!(guard[0].0.get("assets").is_none());
        assert!(guard[0].1.get("typesafe-danger-zone-images").is_none());
    }
}

#[tokio::test]
async fn lets_compat_override_detected_image_settings_per_field() {
    let captured = Arc::new(Mutex::new(Vec::<(Value, Headers)>::new()));
    let cap_ref = Arc::clone(&captured);
    let fetch_fn: FetchFunction = Arc::new(move |req| {
        let cap_ref = Arc::clone(&cap_ref);
        Box::pin(async move {
            cap_ref
                .lock()
                .unwrap()
                .push((req.body_json().unwrap_or(Value::Null), req.headers));
            Ok(Response::new(
                200,
                Headers::from_pairs([("content-type", "application/json")]),
                pi_js::json::stringify(&json!({ "answers": wire_answers() })),
            ))
        })
    });
    let mut m = base_model();
    m.input = vec![ModelModality::Text, ModelModality::Image];
    m.compat = Some(SystemOneCompat {
        image_input: None,
        send_image_opt_in_header: Some(false),
    });
    let mut ctx = sample_context();
    ctx.images = Some(vec![ImageContent::new("cG5n", "image/png")]);

    classify(&m, &ctx, Some(opts_with_fetch("secret", fetch_fn))).await;

    let guard = captured.lock().unwrap();
    assert!(guard[0].0.get("assets").is_some());
    assert!(guard[0].1.get("typesafe-danger-zone-images").is_none());
}
