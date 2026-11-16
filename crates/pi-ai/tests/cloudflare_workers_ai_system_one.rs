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
    ClassifierQuestion, ClassifierStopReason, FetchFunction, ModelType, ProviderEnv, ProviderRequestOptions,
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
    ClassifierContext { state, questions }
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
