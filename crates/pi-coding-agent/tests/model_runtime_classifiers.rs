//! Port of packages/coding-agent/test/model-runtime-classifiers.test.ts

use std::sync::Arc;

use indexmap::IndexMap;
use pi_ai::models_store::InMemoryModelsStore;
use pi_ai::types::{
    ClassifierAnswer, ClassifierBoolCriteria, ClassifierContext, ClassifierQuestion, ClassifierStopReason, ModelType,
};
use pi_coding_agent::core::auth_storage::AuthStorage;
use pi_coding_agent::core::model_runtime::{CreateModelRuntimeOptions, ModelRuntime};
use pi_js::fetch::testing::{mock_fetch, response as fetch_response};
use serde_json::json;

#[tokio::test]
async fn lists_jev_separately_and_classifies_with_runtime_resolved_auth() {
    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(Arc::new(AuthStorage::in_memory(None))),
        models_store: Some(Arc::new(InMemoryModelsStore::new())),
        models_path: Some(None),
        allow_model_network: Some(false),
        ..Default::default()
    })
    .await
    .unwrap();

    let jev_any = runtime
        .get_model_of_type(ModelType::Classifier, "typesafe", "jev-latest")
        .expect("jev-latest classifier model");
    let jev = jev_any.into_classifier().expect("classifier model variant");
    assert!(runtime.get_model("typesafe", "jev-latest").is_none());

    let mut state = IndexMap::new();
    state.insert("text".to_owned(), json!("Looks good"));
    let mut questions = IndexMap::new();
    questions.insert(
        "approved".to_owned(),
        ClassifierQuestion::Bool {
            instructions: "Does this express approval?".to_owned(),
            criteria: ClassifierBoolCriteria {
                r#true: "Approval".to_owned(),
                r#false: "No approval".to_owned(),
            },
        },
    );
    let context = ClassifierContext { state, questions };

    let unconfigured = runtime.classify(&jev, &context, None).await;
    assert_eq!(unconfigured.stop_reason, ClassifierStopReason::Error);
    assert!(
        unconfigured
            .error_message
            .as_deref()
            .is_some_and(|m| m.contains("not configured"))
    );

    runtime
        .set_runtime_api_key("typesafe", "sk-typesafe", None)
        .await
        .unwrap();
    let available = runtime
        .get_available_of_type(ModelType::Classifier, Some("typesafe"), None)
        .await
        .unwrap();
    assert_eq!(available, vec![pi_ai::types::AnyModel::Classifier(jev.clone())]);

    let _guard = mock_fetch(|req| {
        Box::pin(async move {
            assert_eq!(req.headers.get("authorization").as_deref(), Some("Bearer sk-typesafe"));
            let body = pi_js::json::stringify(&json!({
                "answers": { "approved": { "type": "noul", "noul": 0.8 } }
            }));
            Ok(fetch_response(200, &[("content-type", "application/json")], body))
        })
    });

    let result = runtime.classify(&jev, &context, None).await;
    assert_eq!(result.stop_reason, ClassifierStopReason::Stop);
    assert_eq!(
        result.answers.get("approved"),
        Some(&ClassifierAnswer::Bool { probability: 0.8 })
    );
}
