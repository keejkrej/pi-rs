//! Direct unit tests for `pi_ai::api::cloudflare_workers_ai_system_one::classify` (unit ai-api-classifiers-images).

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use serde_json::json;

use pi_ai::api::cloudflare_workers_ai_system_one::classify;
use pi_ai::types::{
    ClassifierAnswer, ClassifierBoolCriteria, ClassifierContext, ClassifierModel, ClassifierOptions,
    ClassifierQuestion, ClassifierStopReason, FetchFunction, ModelCost, ModelModality, ProviderHeaders,
};
use pi_js::fetch::{Headers, Response};

fn cf_model() -> ClassifierModel {
    ClassifierModel {
        id: "typesafe/jev".to_owned(),
        name: "Jev".to_owned(),
        api: "cloudflare-workers-ai-system-one".to_owned(),
        provider: "cloudflare-workers-ai".to_owned(),
        base_url: "https://api.cloudflare.com/client/v4/accounts/acct-1/ai/".to_owned(),
        input: vec![ModelModality::Text],
        input_limits: None,
        context_window: 32000,
        cost: ModelCost::default(),
        headers: None,
    }
}

fn bool_context() -> ClassifierContext {
    let mut state = IndexMap::new();
    state.insert("text".to_owned(), json!("urgent outage"));
    let mut questions = IndexMap::new();
    questions.insert(
        "urgent".to_owned(),
        ClassifierQuestion::Bool {
            instructions: "Urgent?".to_owned(),
            criteria: ClassifierBoolCriteria {
                r#true: "Yes".to_owned(),
                r#false: "No".to_owned(),
            },
        },
    );
    ClassifierContext { state, questions }
}

#[tokio::test]
async fn direct_api_normalizes_trailing_slash_and_hooks() {
    let seen_urls = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen_urls_ref = Arc::clone(&seen_urls);
    let seen_statuses = Arc::new(Mutex::new(Vec::<u16>::new()));
    let seen_statuses_ref = Arc::clone(&seen_statuses);

    let fetch_fn: FetchFunction = Arc::new(move |req| {
        let seen_urls_ref = Arc::clone(&seen_urls_ref);
        Box::pin(async move {
            seen_urls_ref.lock().unwrap().push(req.url.clone());
            let body = req.body_json().expect("json");
            assert_eq!(body["input"]["questions"]["urgent"]["type"], "noul");
            assert_eq!(body["trace"], true);
            assert_eq!(req.headers.get("x-custom").as_deref(), Some("1"));
            Ok(Response::new(
                200,
                Headers::from_pairs([("content-type", "application/json")]),
                pi_js::json::stringify(&json!({
                    "success": true,
                    "result": {
                        "state": "Completed",
                        "result": {
                            "answers": { "urgent": { "type": "noul", "noul": 0.91 } },
                        },
                    },
                    "errors": [],
                })),
            ))
        })
    });

    let mut headers = ProviderHeaders::new();
    headers.insert("x-custom".to_owned(), Some("1".to_owned()));

    let mut options = ClassifierOptions::default();
    options.base.api_key = Some("cf-token".to_owned());
    options.base.headers = Some(headers);
    options.base.fetch = Some(fetch_fn);
    options.base.on_payload = Some(Arc::new(|payload, _model| {
        Box::pin(async move {
            let mut obj = payload.as_object().cloned().unwrap_or_default();
            obj.insert("trace".to_owned(), json!(true));
            Ok(Some(serde_json::Value::Object(obj)))
        })
    }));
    options.base.on_response = Some(Arc::new(move |resp, _model| {
        let seen_statuses_ref = Arc::clone(&seen_statuses_ref);
        Box::pin(async move {
            seen_statuses_ref.lock().unwrap().push(resp.status);
            Ok(())
        })
    }));

    let result = classify(&cf_model(), &bool_context(), Some(options)).await;
    assert_eq!(result.stop_reason, ClassifierStopReason::Stop);
    assert_eq!(
        *seen_urls.lock().unwrap(),
        vec!["https://api.cloudflare.com/client/v4/accounts/acct-1/ai/run".to_owned()]
    );
    assert_eq!(*seen_statuses.lock().unwrap(), vec![200]);
    assert_eq!(
        result.answers.get("urgent"),
        Some(&ClassifierAnswer::Bool { probability: 0.91 })
    );
}

#[tokio::test]
async fn direct_api_reports_http_error_body() {
    let fetch_fn: FetchFunction = Arc::new(move |_req| {
        Box::pin(async move {
            Ok(Response::new(
                400,
                Headers::from_pairs([("content-type", "application/json")]),
                "bad payload",
            ))
        })
    });
    let mut options = ClassifierOptions::default();
    options.base.api_key = Some("cf-token".to_owned());
    options.base.fetch = Some(fetch_fn);
    options.base.max_retries = Some(0);

    let result = classify(&cf_model(), &bool_context(), Some(options)).await;
    assert_eq!(result.stop_reason, ClassifierStopReason::Error);
    assert_eq!(
        result.error_message.as_deref(),
        Some("Cloudflare Workers AI error (400): bad payload")
    );
}
