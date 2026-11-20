//! Port of packages/ai/test/cloudflare-workers-ai-system-one.test.ts (unit ai-providers-builtin)

// @generated-mods begin (scaffold-owned, do not edit)
#[path = "cloudflare_workers_ai_system_one/part_2.rs"]
mod part_2;
// @generated-mods end

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use serde_json::{Value, json};

use pi_ai::models::{ModelsClassifierOptions, MutableModels, create_models};
use pi_ai::providers::all::get_builtin_classifier_model;
use pi_ai::providers::cloudflare_workers_ai::cloudflare_workers_ai_provider;
use pi_ai::types::{
    AnyModel, ClassifierAnswer, ClassifierBoolCriteria, ClassifierContext, ClassifierModel, ClassifierOptions,
    ClassifierQuestion, ClassifierStopReason, FetchFunction, ImageContent, ModelModality, ModelType, ProviderEnv,
    ProviderRequestOptions, SystemOneCompat, SystemOneImageInput,
};
use pi_js::fetch::{Headers, Response};

fn sample_context() -> ClassifierContext {
    let mut state = IndexMap::new();
    state.insert(
        "message".to_owned(),
        Value::String("Help! My payouts have been failing for 3 days.".to_owned()),
    );
    let mut choice_criteria = IndexMap::new();
    choice_criteria.insert("billing".to_owned(), "Payments".to_owned());
    choice_criteria.insert("technical".to_owned(), "Bugs".to_owned());

    let mut questions = IndexMap::new();
    questions.insert(
        "is_urgent".to_owned(),
        ClassifierQuestion::Bool {
            instructions: "Does this convey urgency?".to_owned(),
            criteria: ClassifierBoolCriteria {
                r#true: "Explicitly time-sensitive".to_owned(),
                r#false: "No urgency expressed".to_owned(),
            },
        },
    );
    questions.insert(
        "department".to_owned(),
        ClassifierQuestion::Choice {
            instructions: "Which team should handle this?".to_owned(),
            criteria: choice_criteria,
        },
    );
    ClassifierContext {
        state,
        questions,
        images: None,
    }
}

fn jev_output() -> Value {
    json!({
        "model": "jev-1.13.0",
        "answers": {
            "is_urgent": { "type": "noul", "noul": 0.95 },
            "department": {
                "type": "choice",
                "choice": "billing",
                "confidence": 0.8,
                "probabilities": { "billing": 0.87, "technical": 0.13 },
            },
        },
        "usage": { "input_tokens": 426, "output_tokens": 73 },
    })
}

fn rest_response(state: &str, result: Value) -> Value {
    json!({
        "result": { "state": state, "result": result, "gatewayMetadata": { "keySource": "Unified" } },
        "success": true,
        "errors": [],
        "messages": [],
    })
}

fn setup() -> (MutableModels, ClassifierModel) {
    let models = create_models(None);
    models.set_provider(cloudflare_workers_ai_provider());
    let jev = match models.get_model_of_type(ModelType::Classifier, "cloudflare-workers-ai", "typesafe/jev") {
        Some(AnyModel::Classifier(model)) => model,
        other => panic!("missing Cloudflare Jev model: {other:?}"),
    };
    (models, jev)
}

fn auth_options(fetch_fn: FetchFunction) -> ModelsClassifierOptions {
    let mut env = ProviderEnv::new();
    env.insert("CLOUDFLARE_ACCOUNT_ID".to_owned(), "account-id".to_owned());
    ModelsClassifierOptions {
        base: ClassifierOptions {
            base: ProviderRequestOptions {
                api_key: Some("cf-key".to_owned()),
                env: Some(env),
                fetch: Some(fetch_fn),
                ..Default::default()
            },
            temperature: None,
        },
        transform_headers: None,
    }
}

#[test]
fn exposes_jev_only_through_classifier_catalog_accessors() {
    let (models, jev) = setup();
    assert_eq!(
        Some(&jev),
        get_builtin_classifier_model("cloudflare-workers-ai", "typesafe/jev").as_ref()
    );
    assert_eq!(jev.api, "cloudflare-workers-ai-system-one");
    assert!(models.get_model("cloudflare-workers-ai", "typesafe/jev").is_none());
}

#[tokio::test]
async fn runs_jev_through_the_account_scoped_ai_run_endpoint() {
    let (models, jev) = setup();
    let context = sample_context();
    let urls = Arc::new(Mutex::new(Vec::<String>::new()));
    let urls_ref = Arc::clone(&urls);
    let expected_state = context.state.clone();

    let fetch_fn: FetchFunction = Arc::new(move |req| {
        let urls_ref = Arc::clone(&urls_ref);
        let expected_state = expected_state.clone();
        Box::pin(async move {
            urls_ref.lock().unwrap().push(req.url.clone());
            let payload = req.body_json().expect("json payload");
            assert_eq!(payload["model"], "typesafe/jev");
            assert_eq!(payload["input"]["state"], json!(expected_state));
            assert_eq!(payload["input"]["questions"]["is_urgent"]["type"], "noul");
            assert_eq!(payload["input"]["questions"]["department"]["type"], "choice");
            assert_eq!(req.headers.get("authorization").as_deref(), Some("Bearer cf-key"));
            Ok(Response::new(
                200,
                Headers::from_pairs([("content-type", "application/json")]),
                pi_js::json::stringify(&rest_response("Completed", jev_output())),
            ))
        })
    });

    let result = models.classify(&jev, &context, Some(auth_options(fetch_fn))).await;

    assert_eq!(
        *urls.lock().unwrap(),
        vec!["https://api.cloudflare.com/client/v4/accounts/account-id/ai/run".to_owned()]
    );
    assert_eq!(result.stop_reason, ClassifierStopReason::Stop);
    assert_eq!(
        result.answers.get("is_urgent"),
        Some(&ClassifierAnswer::Bool { probability: 0.95 })
    );
    match result.answers.get("department") {
        Some(ClassifierAnswer::Choice {
            choice,
            confidence,
            probabilities,
        }) => {
            assert_eq!(choice, "billing");
            assert!((*confidence - 0.8).abs() < 1e-12);
            assert_eq!(probabilities.get("billing"), Some(&0.87));
            assert_eq!(probabilities.get("technical"), Some(&0.13));
        }
        other => panic!("expected choice answer, got {other:?}"),
    }
    let usage = result.usage.expect("usage should be present");
    assert_eq!(usage.input, 426);
    assert_eq!(usage.output, 73);
    assert_eq!(usage.total_tokens, 499);
}

#[tokio::test]
async fn reports_runs_that_did_not_complete() {
    let (models, jev) = setup();
    let context = sample_context();
    let fetch_fn: FetchFunction = Arc::new(move |_req| {
        Box::pin(async move {
            Ok(Response::new(
                200,
                Headers::from_pairs([("content-type", "application/json")]),
                pi_js::json::stringify(&rest_response("Queued", Value::Null)),
            ))
        })
    });

    let result = models.classify(&jev, &context, Some(auth_options(fetch_fn))).await;

    assert_eq!(result.stop_reason, ClassifierStopReason::Error);
    assert!(
        result
            .error_message
            .as_deref()
            .unwrap_or_default()
            .contains("run did not complete (state: Queued)")
    );
}

#[tokio::test]
async fn reports_cloudflare_envelope_errors() {
    let (models, jev) = setup();
    let context = sample_context();
    let fetch_fn: FetchFunction = Arc::new(move |_req| {
        Box::pin(async move {
            Ok(Response::new(
                200,
                Headers::from_pairs([("content-type", "application/json")]),
                pi_js::json::stringify(&json!({
                    "success": false,
                    "errors": [{ "code": 5007, "message": "No such model" }],
                    "result": Value::Null,
                })),
            ))
        })
    });

    let result = models.classify(&jev, &context, Some(auth_options(fetch_fn))).await;

    assert_eq!(result.stop_reason, ClassifierStopReason::Error);
    assert!(
        result
            .error_message
            .as_deref()
            .unwrap_or_default()
            .contains("Cloudflare Workers AI error: No such model")
    );
}

fn clef_output() -> Value {
    json!({
        "model": "clef",
        "answers": {
            "is_urgent": { "type": "noul", "noul": 0.9912 },
            "department": {
                "type": "choice",
                "choice": "technical",
                "probabilities": { "billing": 0.1632, "technical": 0.8368 },
                "confidence": 0.4538,
            },
        },
        "usage": { "input_tokens": 222, "output_tokens": 0 },
    })
}

#[tokio::test]
async fn runs_clef_models_through_ai_run_and_parses_direct_output() {
    let (models, _) = setup();
    let context = sample_context();
    for (id, input_price) in [
        ("@cf/cloudflare/clef", 0.24_f64),
        ("@cf/cloudflare/clef-flash", 0.038_f64),
        ("@cf/cloudflare/clef-omni", 0.15_f64),
    ] {
        let clef = match models.get_model_of_type(ModelType::Classifier, "cloudflare-workers-ai", id) {
            Some(AnyModel::Classifier(m)) => m,
            other => panic!("missing Cloudflare {id} model: {other:?}"),
        };
        let urls = Arc::new(Mutex::new(Vec::<String>::new()));
        let urls_ref = Arc::clone(&urls);
        let expected_state = context.state.clone();
        let expected_id = id.to_owned();
        let fetch_fn: FetchFunction = Arc::new(move |req| {
            let urls_ref = Arc::clone(&urls_ref);
            let expected_state = expected_state.clone();
            let expected_id = expected_id.clone();
            Box::pin(async move {
                urls_ref.lock().unwrap().push(req.url.clone());
                let payload = req.body_json().expect("json payload");
                assert_eq!(payload["model"], expected_id);
                assert_eq!(payload["input"]["state"], json!(expected_state));
                assert_eq!(payload["input"]["questions"]["is_urgent"]["type"], "noul");
                Ok(Response::new(
                    200,
                    Headers::from_pairs([("content-type", "application/json")]),
                    pi_js::json::stringify(&json!({
                        "result": clef_output(),
                        "success": true,
                        "errors": [],
                        "messages": [],
                    })),
                ))
            })
        });

        let result = models.classify(&clef, &context, Some(auth_options(fetch_fn))).await;
        assert_eq!(
            *urls.lock().unwrap(),
            vec!["https://api.cloudflare.com/client/v4/accounts/account-id/ai/run".to_owned()]
        );
        assert_eq!(result.stop_reason, ClassifierStopReason::Stop);
        assert_eq!(
            result.answers.get("is_urgent"),
            Some(&ClassifierAnswer::Bool { probability: 0.9912 })
        );
        match result.answers.get("department") {
            Some(ClassifierAnswer::Choice { choice, confidence, .. }) => {
                assert_eq!(choice, "technical");
                assert!((*confidence - 0.4538).abs() < 1e-12);
            }
            other => panic!("expected choice answer, got {other:?}"),
        }
        let usage = result.usage.expect("usage");
        assert_eq!(usage.input, 222);
        assert_eq!(usage.output, 0);
        assert_eq!(usage.total_tokens, 222);
        assert!((usage.cost.input - (222.0 * input_price) / 1_000_000.0).abs() < 1e-12);
    }
}

#[tokio::test]
async fn sends_images_to_clef_as_data_urls() {
    let (models, _) = setup();
    let clef = match models.get_model_of_type(ModelType::Classifier, "cloudflare-workers-ai", "@cf/cloudflare/clef") {
        Some(AnyModel::Classifier(m)) => m,
        other => panic!("missing Clef: {other:?}"),
    };
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
                pi_js::json::stringify(&json!({
                    "result": clef_output(),
                    "success": true,
                    "errors": [],
                    "messages": [],
                })),
            ))
        })
    });

    let mut ctx = sample_context();
    ctx.images = Some(vec![
        ImageContent::new("cG5n", "image/png"),
        ImageContent::new("d2VicA==", "image/webp"),
    ]);
    let result = models.classify(&clef, &ctx, Some(auth_options(fetch_fn))).await;

    assert_eq!(result.stop_reason, ClassifierStopReason::Stop);
    let guard = captured.lock().unwrap();
    assert_eq!(guard[0].0["input"]["state"], json!(ctx.state));
    assert!(guard[0].0["input"]["questions"].is_object());
    assert_eq!(
        guard[0].0["input"]["images"],
        json!(["data:image/png;base64,cG5n", "data:image/webp;base64,d2VicA=="])
    );
    assert!(guard[0].1.get("typesafe-danger-zone-images").is_none());
}

#[tokio::test]
async fn reads_the_image_form_from_compat_before_the_model_id() {
    let (models, jev) = setup();
    let clef = match models.get_model_of_type(ModelType::Classifier, "cloudflare-workers-ai", "@cf/cloudflare/clef") {
        Some(AnyModel::Classifier(m)) => m,
        other => panic!("missing Clef: {other:?}"),
    };
    assert_eq!(
        clef.compat,
        Some(SystemOneCompat {
            image_input: Some(SystemOneImageInput::Images),
            send_image_opt_in_header: None,
        })
    );
    assert_eq!(
        jev.compat,
        Some(SystemOneCompat {
            image_input: Some(SystemOneImageInput::Assets),
            send_image_opt_in_header: None,
        })
    );

    let bodies = Arc::new(Mutex::new(Vec::<Value>::new()));
    let bodies_ref = Arc::clone(&bodies);
    let fetch_fn: FetchFunction = Arc::new(move |req| {
        let bodies_ref = Arc::clone(&bodies_ref);
        Box::pin(async move {
            bodies_ref.lock().unwrap().push(req.body_json().unwrap_or(Value::Null));
            Ok(Response::new(
                200,
                Headers::from_pairs([("content-type", "application/json")]),
                pi_js::json::stringify(&json!({
                    "result": clef_output(),
                    "success": true,
                    "errors": [],
                    "messages": [],
                })),
            ))
        })
    });

    let mut custom = jev.clone();
    custom.id = "@cf/acme/clef-finetune".to_owned();
    custom.input = vec![ModelModality::Text, ModelModality::Image];
    custom.compat = Some(SystemOneCompat {
        image_input: Some(SystemOneImageInput::Images),
        send_image_opt_in_header: None,
    });
    let mut ctx = sample_context();
    ctx.images = Some(vec![ImageContent::new("cG5n", "image/png")]);
    models.classify(&custom, &ctx, Some(auth_options(fetch_fn))).await;

    let guard = bodies.lock().unwrap();
    assert_eq!(guard[0]["input"]["images"], json!(["data:image/png;base64,cG5n"]));
    assert!(guard[0]["input"].get("assets").is_none());
}

#[tokio::test]
async fn rejects_more_than_four_images_for_clef_before_sending() {
    let (models, _) = setup();
    let clef = match models.get_model_of_type(
        ModelType::Classifier,
        "cloudflare-workers-ai",
        "@cf/cloudflare/clef-flash",
    ) {
        Some(AnyModel::Classifier(m)) => m,
        other => panic!("missing Clef Flash: {other:?}"),
    };
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let calls_ref = Arc::clone(&calls);
    let fetch_fn: FetchFunction = Arc::new(move |_req| {
        calls_ref.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async move {
            Ok(Response::new(
                200,
                Headers::from_pairs([("content-type", "application/json")]),
                pi_js::json::stringify(&json!({ "result": clef_output(), "success": true })),
            ))
        })
    });
    let mut ctx = sample_context();
    ctx.images = Some(vec![ImageContent::new("cG5n", "image/png"); 5]);

    let result = models.classify(&clef, &ctx, Some(auth_options(fetch_fn))).await;

    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(result.stop_reason, ClassifierStopReason::Error);
    assert_eq!(
        result.error_message.as_deref(),
        Some("Cloudflare Workers AI accepts at most 4 images for @cf/cloudflare/clef-flash, got 5")
    );
}

#[tokio::test]
async fn sends_images_to_jev_models_as_system_one_assets() {
    let (models, _) = setup();
    let base_ctx = sample_context();
    for id in ["typesafe/jev", "typesafe/jev-1.14.0"] {
        let jev = match models.get_model_of_type(ModelType::Classifier, "cloudflare-workers-ai", id) {
            Some(AnyModel::Classifier(m)) => m,
            other => panic!("missing {id}: {other:?}"),
        };
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
                    pi_js::json::stringify(&rest_response("Completed", jev_output())),
                ))
            })
        });

        let mut ctx = base_ctx.clone();
        ctx.images = Some(vec![ImageContent::new("cG5n", "image/png")]);
        let result = models.classify(&jev, &ctx, Some(auth_options(fetch_fn))).await;

        assert_eq!(result.stop_reason, ClassifierStopReason::Stop);
        let guard = captured.lock().unwrap();
        assert_eq!(guard[0].0["model"], id);
        assert_eq!(guard[0].0["input"]["state"], json!(["<image-1>", base_ctx.state]));
        assert_eq!(
            guard[0].0["input"]["assets"],
            json!({ "<image-1>": { "type": "image", "format": "png", "data_base64": "cG5n" } })
        );
        assert!(guard[0].1.get("typesafe-danger-zone-images").is_none());
    }
}
