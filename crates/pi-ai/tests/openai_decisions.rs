//! Port of packages/ai/test/openai-decisions.test.ts

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use indexmap::IndexMap;
use pi_ai::api::openai_decisions::classify;
use pi_ai::types::{
    ClassifierAnswer, ClassifierBoolCriteria, ClassifierContext, ClassifierModel, ClassifierOptions,
    ClassifierQuestion, ClassifierStopReason, ImageContent, ModelCost, ModelCostTier, ModelInput,
    ProviderRequestOptions, UsageCost,
};
use pi_js::fetch::{Headers, Request, Response};
use serde_json::{Value, json};

fn decisions_model() -> ClassifierModel {
    ClassifierModel {
        id: "gpt-6-luna".to_owned(),
        name: "GPT-6 Luna".to_owned(),
        api: "openai-decisions".to_owned(),
        provider: "openai".into(),
        base_url: "https://api.openai.com/v1".to_owned(),
        input: vec![ModelInput::Text, ModelInput::Image],
        input_limits: None,
        cost: ModelCost {
            input: 0.1,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            tiers: Some(vec![ModelCostTier {
                input_tokens_above: 272_000,
                input: 0.2,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
            }]),
        },
        context_window: 922_000,
        headers: None,
        compat: None,
    }
}

fn sample_context() -> ClassifierContext {
    let mut state = IndexMap::new();
    state.insert("text".to_owned(), json!("The deployment succeeded, thank you."));

    let mut cat_criteria = IndexMap::new();
    cat_criteria.insert("success".to_owned(), "Successful".to_owned());
    cat_criteria.insert("failure".to_owned(), String::new());

    let mut questions = IndexMap::new();
    questions.insert(
        "category".to_owned(),
        ClassifierQuestion::Choice {
            instructions: "Classify the message".to_owned(),
            criteria: cat_criteria,
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

fn wire_answers() -> Vec<Value> {
    vec![
        json!({
            "type": "choice",
            "name": "category",
            "choice": "success",
            "probabilities": [
                { "value": "success", "probability": 0.9 },
                { "value": "failure", "probability": 0.1 },
            ],
            "confidence": 0.8,
        }),
        json!({
            "type": "score",
            "name": "satisfaction",
            "score": 1.8,
            "probabilities": [
                { "value": 0, "label": "low", "probability": 0.05 },
                { "value": 1, "label": "neutral", "probability": 0.1 },
                { "value": 2, "label": "high", "probability": 0.85 },
            ],
            "confidence": 0.7,
        }),
        json!({ "type": "predicate", "name": "approved", "probability": 0.95 }),
    ]
}

fn wire_usage() -> Value {
    json!({
        "input_tokens": 164,
        "input_tokens_details": { "cached_tokens": 0, "cache_write_tokens": 0 },
        "output_tokens": 0,
        "output_tokens_details": { "reasoning_tokens": 0 },
        "total_tokens": 164,
    })
}

fn json_response(status: u16, body: Value) -> Response {
    Response::new(
        status,
        Headers::from_pairs([("content-type", "application/json")]),
        pi_js::json::stringify(&body),
    )
}

fn opts_with_fetch(
    f: impl Fn(Request) -> pi_js::BoxFuture<pi_js::Result<Response>> + Send + Sync + 'static,
) -> ClassifierOptions {
    ClassifierOptions {
        base: ProviderRequestOptions {
            api_key: Some("secret".to_owned()),
            fetch: Some(Arc::new(f)),
            ..Default::default()
        },
        temperature: None,
    }
}

#[tokio::test]
async fn maps_questions_answers_tiers_images_limits_and_errors() {
    let model = decisions_model();
    let ctx = sample_context();

    // 1. Question mapping & reverse-order answer matching & usage
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_ref = Arc::clone(&calls);
    let captured_url = Arc::new(std::sync::Mutex::new(String::new()));
    let cu_ref = Arc::clone(&captured_url);
    let captured_auth = Arc::new(std::sync::Mutex::new(None::<String>));
    let ca_ref = Arc::clone(&captured_auth);
    let captured_body = Arc::new(std::sync::Mutex::new(None::<Value>));
    let cb_ref = Arc::clone(&captured_body);

    let mut opts = opts_with_fetch(move |req: Request| {
        calls_ref.fetch_add(1, Ordering::SeqCst);
        *cu_ref.lock().unwrap() = req.url.clone();
        *ca_ref.lock().unwrap() = req.headers.get("authorization");
        *cb_ref.lock().unwrap() = req.body_json();
        let mut ans = wire_answers();
        ans.reverse();
        let body = json!({ "model": "gpt-6-luna", "answers": ans, "usage": wire_usage() });
        Box::pin(async move { Ok(json_response(200, body)) })
    });
    opts.temperature = Some(1.5);

    let res = classify(&model, &ctx, Some(opts)).await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        captured_url.lock().unwrap().as_str(),
        "https://api.openai.com/v1/decisions"
    );
    assert_eq!(captured_auth.lock().unwrap().as_deref(), Some("Bearer secret"));
    let body = captured_body.lock().unwrap().take().unwrap();
    assert_eq!(
        body,
        json!({
            "model": "gpt-6-luna",
            "input": pi_js::json::stringify(&ctx.state),
            "questions": [
                {
                    "type": "choice",
                    "name": "category",
                    "instructions": "Classify the message",
                    "choices": [{ "value": "success", "description": "Successful" }, { "value": "failure" }],
                },
                {
                    "type": "score",
                    "name": "satisfaction",
                    "instructions": "Score satisfaction",
                    "levels": [{ "label": "low" }, { "label": "neutral" }, { "label": "high" }],
                },
                {
                    "type": "predicate",
                    "name": "approved",
                    "instructions": "Does the user approve?\n\nTrue means: Approval\nFalse means: No approval",
                },
            ],
        })
    );
    assert_eq!(res.stop_reason, ClassifierStopReason::Stop);
    let mut expected_probs = IndexMap::new();
    expected_probs.insert("success".to_owned(), 0.9);
    expected_probs.insert("failure".to_owned(), 0.1);
    assert_eq!(
        res.answers.get("category"),
        Some(&ClassifierAnswer::Choice {
            choice: "success".to_owned(),
            probabilities: expected_probs,
            confidence: 0.8,
        })
    );
    assert_eq!(
        res.answers.get("satisfaction"),
        Some(&ClassifierAnswer::Score {
            score: 1.8,
            confidence: 0.7,
        })
    );
    assert_eq!(
        res.answers.get("approved"),
        Some(&ClassifierAnswer::Bool { probability: 0.95 })
    );
    let u = res.usage.as_ref().unwrap();
    assert_eq!(u.input, 164);
    assert_eq!(u.output, 0);
    assert_eq!(u.cache_read, 0);
    assert_eq!(u.total_tokens, 164);
    assert!((u.cost.total - 0.0000164).abs() < 1e-12);

    // 2. Long-context pricing tier
    let long_res = classify(
        &model,
        &ctx,
        Some(opts_with_fetch(|_| {
            Box::pin(async {
                Ok(json_response(
                    200,
                    json!({ "answers": wire_answers(), "usage": { "input_tokens": 300_000, "output_tokens": 0 } }),
                ))
            })
        })),
    )
    .await;
    assert!((long_res.usage.unwrap().cost.total - 0.06).abs() < 1e-12);

    // 3. Images
    let img = ImageContent {
        data: "aW1hZ2U=".to_owned(),
        mime_type: "image/png".to_owned(),
    };
    let mut img2 = img.clone();
    img2.mime_type = "image/jpeg".to_owned();
    let mut img_ctx = ctx.clone();
    img_ctx.images = Some(vec![img.clone(), img2]);
    let img_body = Arc::new(std::sync::Mutex::new(None::<Value>));
    let ib_ref = Arc::clone(&img_body);
    let img_res = classify(
        &model,
        &img_ctx,
        Some(opts_with_fetch(move |req| {
            *ib_ref.lock().unwrap() = req.body_json();
            Box::pin(async { Ok(json_response(200, json!({ "answers": wire_answers() }))) })
        })),
    )
    .await;
    assert_eq!(img_res.stop_reason, ClassifierStopReason::Stop);
    assert_eq!(
        img_body.lock().unwrap().take().unwrap()["input"],
        json!([{
            "role": "user",
            "content": [
                { "type": "input_text", "text": pi_js::json::stringify(&ctx.state) },
                { "type": "input_image", "image_url": "data:image/png;base64,aW1hZ2U=" },
                { "type": "input_image", "image_url": "data:image/jpeg;base64,aW1hZ2U=" },
            ],
        }])
    );

    // 4. Rejects >128 images before sending
    let too_many_calls = Arc::new(AtomicUsize::new(0));
    let tmc_ref = Arc::clone(&too_many_calls);
    let mut over_ctx = ctx.clone();
    over_ctx.images = Some(vec![img; 129]);
    let over_res = classify(
        &model,
        &over_ctx,
        Some(opts_with_fetch(move |_| {
            tmc_ref.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(json_response(200, json!({ "answers": wire_answers() }))) })
        })),
    )
    .await;
    assert_eq!(too_many_calls.load(Ordering::SeqCst), 0);
    assert_eq!(over_res.stop_reason, ClassifierStopReason::Error);
    assert!(over_res.error_message.unwrap().contains("at most 128 images, got 129"));

    // 5. Refusal keeps billed usage & clears answers
    let ans = wire_answers();
    let refused_res = classify(
        &model,
        &ctx,
        Some(opts_with_fetch(move |_| {
            let body = json!({
                "answers": [ans[0].clone(), ans[1].clone(), { "type": "refusal", "name": "approved" }],
                "usage": wire_usage(),
            });
            Box::pin(async move { Ok(json_response(200, body)) })
        })),
    )
    .await;
    assert_eq!(refused_res.stop_reason, ClassifierStopReason::Error);
    assert!(refused_res.answers.is_empty());
    assert_eq!(
        refused_res.error_message.as_deref(),
        Some("OpenAI Decisions refused to answer approved")
    );
    assert_eq!(refused_res.usage.unwrap().input, 164);

    // 6. Missing and mistyped answers
    let ans_m = wire_answers();
    let missing = classify(
        &model,
        &ctx,
        Some(opts_with_fetch(move |_| {
            let body = json!({ "answers": [ans_m[0].clone(), ans_m[1].clone()] });
            Box::pin(async move { Ok(json_response(200, body)) })
        })),
    )
    .await;
    assert_eq!(missing.stop_reason, ClassifierStopReason::Error);
    assert!(
        missing
            .error_message
            .unwrap()
            .contains("did not return an answer for approved")
    );

    let ans_t = wire_answers();
    let mistyped = classify(
        &model,
        &ctx,
        Some(opts_with_fetch(move |_| {
            let body =
                json!({ "answers": [ans_t[0].clone(), ans_t[1].clone(), { "type": "score", "name": "approved" }] });
            Box::pin(async move { Ok(json_response(200, body)) })
        })),
    )
    .await;
    assert_eq!(mistyped.stop_reason, ClassifierStopReason::Error);
    assert!(
        mistyped
            .error_message
            .unwrap()
            .contains("did not return a predicate answer for approved")
    );

    // 7. `__proto__` question ID preserved
    let mut proto_q = IndexMap::new();
    proto_q.insert(
        "__proto__".to_owned(),
        ClassifierQuestion::Bool {
            instructions: "Is this true?".to_owned(),
            criteria: ClassifierBoolCriteria {
                r#true: "Yes".to_owned(),
                r#false: "No".to_owned(),
            },
        },
    );
    let proto_ctx = ClassifierContext {
        state: IndexMap::new(),
        questions: proto_q,
        images: None,
    };
    let proto_res = classify(
        &model,
        &proto_ctx,
        Some(opts_with_fetch(|_| {
            Box::pin(async {
                Ok(json_response(
                    200,
                    json!({ "answers": [{ "type": "predicate", "name": "__proto__", "probability": 0.75 }] }),
                ))
            })
        })),
    )
    .await;
    assert_eq!(proto_res.stop_reason, ClassifierStopReason::Stop);
    assert_eq!(
        proto_res.answers.get("__proto__"),
        Some(&ClassifierAnswer::Bool { probability: 0.75 })
    );

    // 8. 504 does not retry & explains timeout instead of HTML
    let t504_calls = Arc::new(AtomicUsize::new(0));
    let t504_ref = Arc::clone(&t504_calls);
    let t504_res = classify(
        &model,
        &ctx,
        Some(opts_with_fetch(move |_| {
            t504_ref.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {
                Ok(Response::new(
                    504,
                    Headers::from_pairs([("retry-after-ms", "0")]),
                    "<!DOCTYPE html><html>Gateway time-out</html>",
                ))
            })
        })),
    )
    .await;
    assert_eq!(t504_calls.load(Ordering::SeqCst), 1);
    assert_eq!(t504_res.stop_reason, ClassifierStopReason::Error);
    let t504_msg = t504_res.error_message.unwrap();
    assert!(
        t504_msg.contains("OpenAI Decisions error (504): the request timed out at the gateway"),
        "{t504_msg}"
    );
    assert!(!t504_msg.contains("<html>"), "{t504_msg}");

    // 9. 503 retries
    let attempts = Arc::new(AtomicUsize::new(0));
    let att_ref = Arc::clone(&attempts);
    let r503 = classify(
        &model,
        &ctx,
        Some(opts_with_fetch(move |_| {
            let n = att_ref.fetch_add(1, Ordering::SeqCst) + 1;
            Box::pin(async move {
                if n == 1 {
                    Ok(Response::new(
                        503,
                        Headers::from_pairs([("retry-after-ms", "0")]),
                        "busy",
                    ))
                } else {
                    Ok(json_response(200, json!({ "answers": wire_answers() })))
                }
            })
        })),
    )
    .await;
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert_eq!(r503.stop_reason, ClassifierStopReason::Stop);

    // 10. 400 includes API error body; unsupported API & missing key fail without fetch
    let mut opts_400 = opts_with_fetch(|_| {
        Box::pin(async {
            Ok(json_response(
                400,
                json!({ "error": { "message": "Decision input exceeds the token limit.", "type": "invalid_request_error" } }),
            ))
        })
    });
    opts_400.max_retries = Some(0);
    let r400 = classify(&model, &ctx, Some(opts_400)).await;
    assert_eq!(r400.stop_reason, ClassifierStopReason::Error);
    let m400 = r400.error_message.unwrap();
    assert!(m400.contains("OpenAI Decisions error (400)"), "{m400}");
    assert!(m400.contains("Decision input exceeds the token limit."), "{m400}");

    let nf_calls = Arc::new(AtomicUsize::new(0));
    let nfc1 = Arc::clone(&nf_calls);
    let other_api = classify(
        &ClassifierModel {
            api: "typesafe-system-one".to_owned(),
            ..model.clone()
        },
        &ctx,
        Some(opts_with_fetch(move |_| {
            nfc1.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(json_response(200, json!({}))) })
        })),
    )
    .await;
    let nfc2 = Arc::clone(&nf_calls);
    let no_key = classify(
        &model,
        &ctx,
        Some(ClassifierOptions {
            base: ProviderRequestOptions {
                fetch: Some(Arc::new(move |_| {
                    nfc2.fetch_add(1, Ordering::SeqCst);
                    Box::pin(async { Ok(json_response(200, json!({}))) })
                })),
                ..Default::default()
            },
            temperature: None,
        }),
    )
    .await;
    assert_eq!(nf_calls.load(Ordering::SeqCst), 0);
    assert!(
        other_api
            .error_message
            .unwrap()
            .contains("Unsupported classifier API: typesafe-system-one")
    );
    assert!(
        no_key
            .error_message
            .unwrap()
            .contains("No API key for provider: openai")
    );
    let _ = UsageCost::default();
}
