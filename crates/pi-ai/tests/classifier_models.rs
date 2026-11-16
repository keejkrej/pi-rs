//! Port of packages/ai/test/classifier-models.test.ts

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use serde_json::{Value, json};

use pi_ai::auth::types::{ApiKeyAuth, ApiKeyAuthInput, AuthResult, ModelAuth, ProviderAuth};
use pi_ai::models::{
    CreateProviderApi, CreateProviderOptions, ModelsClassifierOptions, create_models, create_provider, get_model_type,
};
use pi_ai::providers::all::{
    builtin_models, get_all_builtin_models, get_builtin_classifier_model, get_builtin_classifier_models,
};
use pi_ai::types::{
    AnyModel, ClassifierAnswer, ClassifierBoolCriteria, ClassifierContext, ClassifierModel, ClassifierQuestion,
    ClassifierResult, ClassifierStopReason, FetchFunction, Model, ModelCost, ModelModality, ModelType,
    ProviderClassifier, ProviderStreams,
};
use pi_ai::utils::event_stream::AssistantMessageEventStream;
use pi_ai::utils::model_operations::assert_classifier_model;
use pi_js::Result;
use pi_js::fetch::{Headers, Response};

struct StaticOkAuth;

#[async_trait::async_trait]
impl ApiKeyAuth for StaticOkAuth {
    fn name(&self) -> &str {
        "Test"
    }

    async fn resolve(&self, _input: ApiKeyAuthInput) -> Result<Option<AuthResult>> {
        Ok(Some(AuthResult {
            auth: ModelAuth::default(),
            env: None,
            source: None,
        }))
    }
}

fn classifier_model(provider: &str, id: &str) -> ClassifierModel {
    ClassifierModel {
        id: id.to_owned(),
        name: id.to_owned(),
        api: "test-classifier".to_owned(),
        provider: provider.to_owned(),
        base_url: "https://example.test/v1".to_owned(),
        input: vec![ModelModality::Text],
        input_limits: None,
        context_window: 1000,
        cost: ModelCost::default(),
        headers: None,
    }
}

fn chat_model(provider: &str, id: &str) -> Model {
    Model {
        r#type: None,
        id: id.to_owned(),
        name: id.to_owned(),
        api: "test-chat".to_owned(),
        provider: provider.to_owned(),
        base_url: "https://example.test/v1".to_owned(),
        reasoning: false,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelModality::Text],
        input_limits: None,
        cost: ModelCost::default(),
        context_window: 1000,
        max_tokens: 100,
        headers: None,
        sampling_params: None,
        compat: None,
    }
}

fn bool_context() -> ClassifierContext {
    let mut state = IndexMap::new();
    state.insert("text".to_owned(), Value::String("yes".to_owned()));
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
    ClassifierContext { state, questions }
}

fn dummy_streams() -> ProviderStreams {
    ProviderStreams {
        stream: Arc::new(|_, _, _| AssistantMessageEventStream::new()),
        stream_simple: Arc::new(|_, _, _| AssistantMessageEventStream::new()),
        fetch_deferred: None,
        cancel_deferred: None,
    }
}

#[tokio::test]
async fn keeps_chat_and_classifier_entries_with_the_same_provider_and_id_separate() {
    let chat = chat_model("test", "shared");
    let classifier = classifier_model("test", "shared");

    let mut apis = IndexMap::new();
    apis.insert("test-chat".to_owned(), dummy_streams());

    let mut classifiers = IndexMap::new();
    classifiers.insert(
        "test-classifier".to_owned(),
        ProviderClassifier {
            classify: Arc::new(|model, _ctx, _opts| {
                let model = model.clone();
                Box::pin(async move {
                    let mut answers = IndexMap::new();
                    answers.insert("approved".to_owned(), ClassifierAnswer::Bool { probability: 0.9 });
                    ClassifierResult {
                        api: model.api,
                        provider: model.provider,
                        model: model.id,
                        answers,
                        usage: None,
                        stop_reason: ClassifierStopReason::Stop,
                        error_message: None,
                        timestamp: 0,
                    }
                })
            }),
        },
    );

    let provider = create_provider(CreateProviderOptions {
        id: "test".to_owned(),
        name: None,
        base_url: None,
        headers: None,
        auth: ProviderAuth {
            api_key: Some(Arc::new(StaticOkAuth) as Arc<dyn ApiKeyAuth>),
            oauth: None,
        },
        models: vec![AnyModel::Chat(chat), AnyModel::Classifier(classifier.clone())],
        fetch_models: None,
        filter_models: None,
        filter_all_models: None,
        api: Some(CreateProviderApi::ByApi(apis)),
        images: None,
        classifiers: Some(classifiers),
    })
    .unwrap();

    let models = create_models(None);
    models.set_provider(provider);

    let listed_chat = AnyModel::Chat(models.get_model("test", "shared").unwrap());
    assert_eq!(get_model_type(&listed_chat), ModelType::Chat);
    assert_eq!(
        models
            .get_model_of_type(ModelType::Classifier, "test", "shared")
            .as_ref()
            .map(get_model_type),
        Some(ModelType::Classifier)
    );
    assert_eq!(
        models.get_models_of_type(ModelType::Classifier, None),
        vec![AnyModel::Classifier(classifier.clone())]
    );
    assert_eq!(models.get_all_models(None).len(), 2);
    assert_eq!(
        models
            .get_available_of_type(ModelType::Classifier, None, None)
            .await
            .unwrap(),
        vec![AnyModel::Classifier(classifier.clone())]
    );
    let result = models.classify(&classifier, &bool_context(), None).await;
    assert_eq!(
        result.answers.get("approved"),
        Some(&ClassifierAnswer::Bool { probability: 0.9 })
    );
}

#[tokio::test]
async fn rejects_chat_models_at_the_classifier_entry_point_at_runtime() {
    let chat = chat_model("test", "chat");
    let err = assert_classifier_model(&AnyModel::Chat(chat.clone())).unwrap_err();
    assert!(err.to_string().contains("is not a classifier model"));

    let models = create_models(None);
    models.set_provider(
        create_provider(CreateProviderOptions {
            id: "test".to_owned(),
            name: None,
            base_url: None,
            headers: None,
            auth: ProviderAuth {
                api_key: Some(Arc::new(StaticOkAuth) as Arc<dyn ApiKeyAuth>),
                oauth: None,
            },
            models: vec![AnyModel::Chat(chat)],
            fetch_models: None,
            filter_models: None,
            filter_all_models: None,
            api: Some(CreateProviderApi::Single(dummy_streams())),
            images: None,
            classifiers: None,
        })
        .unwrap(),
    );

    let fake_classifier = classifier_model("test", "chat");
    let result = models.classify(&fake_classifier, &bool_context(), None).await;
    assert_eq!(result.stop_reason, ClassifierStopReason::Error);
    assert!(result.error_message.is_some());
}

#[test]
fn exposes_jev_only_through_classifier_catalog_accessors() {
    let jev = get_builtin_classifier_model("typesafe", "jev-latest").expect("missing typesafe/jev-latest");
    assert_eq!(jev.api, "typesafe-system-one");
    assert_eq!(jev.provider, "typesafe");
    assert_eq!(jev.context_window, 64000);
    assert_eq!(get_builtin_classifier_models("typesafe"), vec![jev.clone()]);
    assert_eq!(
        get_all_builtin_models("typesafe"),
        vec![AnyModel::Classifier(jev.clone())]
    );

    let models = builtin_models(None);
    assert!(models.get_model("typesafe", "jev-latest").is_none());
    assert_eq!(
        models.get_model_of_type(ModelType::Classifier, "typesafe", "jev-latest"),
        Some(AnyModel::Classifier(jev))
    );
}

#[tokio::test]
async fn routes_gateway_and_opencode_jev_to_typesafe_compatible_endpoints() {
    let cases = [
        (
            "vercel-ai-gateway",
            "typesafe-ai/jev",
            "https://ai-gateway.vercel.sh/typesafe/v1/systemone",
        ),
        ("opencode", "jev-1.13", "https://opencode.ai/zen/v1/systemone"),
        ("opencode", "jev-1.13-free", "https://opencode.ai/zen/v1/systemone"),
    ];
    let context = bool_context();

    for (provider, id, expected_url) in cases {
        let models = builtin_models(None);
        let jev = match models.get_model_of_type(ModelType::Classifier, provider, id) {
            Some(AnyModel::Classifier(m)) => m,
            other => panic!("missing {provider} Jev model {id}: {other:?}"),
        };
        assert_eq!(jev.api, "typesafe-system-one");
        assert_eq!(jev.context_window, 32000);
        assert!(models.get_model(provider, id).is_none());

        let requests = Arc::new(Mutex::new(Vec::<(String, Value, Option<String>)>::new()));
        let reqs_ref = Arc::clone(&requests);
        let model_id_owned = id.to_owned();

        let fetch_fn: FetchFunction = Arc::new(move |req| {
            let reqs_ref = Arc::clone(&reqs_ref);
            let model_id_owned = model_id_owned.clone();
            Box::pin(async move {
                let body = req.body_json().unwrap_or(Value::Null);
                let auth = req.headers.get("authorization");
                reqs_ref.lock().unwrap().push((req.url.clone(), body, auth));
                Ok(Response::new(
                    200,
                    Headers::from_pairs([("content-type", "application/json")]),
                    pi_js::json::stringify(&json!({
                        "model": model_id_owned,
                        "answers": { "approved": { "type": "noul", "noul": 0.8 } },
                    })),
                ))
            })
        });

        let mut opts = ModelsClassifierOptions::default();
        opts.base.base.api_key = Some("secret".to_owned());
        opts.base.base.fetch = Some(fetch_fn);

        let result = models.classify(&jev, &context, Some(opts)).await;
        let captured = requests.lock().unwrap();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].0, expected_url);
        assert_eq!(captured[0].1["model"], id);
        assert_eq!(captured[0].1["state"], json!(context.state));
        assert_eq!(captured[0].2.as_deref(), Some("Bearer secret"));
        assert_eq!(result.stop_reason, ClassifierStopReason::Stop);
        assert_eq!(
            result.answers.get("approved"),
            Some(&ClassifierAnswer::Bool { probability: 0.8 })
        );
    }
}

#[test]
fn routes_openrouter_classifier_models_through_the_system_one_api() {
    let models = builtin_models(None);
    for model in get_builtin_classifier_models("openrouter") {
        assert_eq!(model.api, "typesafe-system-one");
        assert_eq!(model.base_url, "https://openrouter.ai/api/v1");
        assert!(models.get_model("openrouter", &model.id).is_none());
        assert_eq!(
            models.get_model_of_type(ModelType::Classifier, "openrouter", &model.id),
            Some(AnyModel::Classifier(model))
        );
    }
}
