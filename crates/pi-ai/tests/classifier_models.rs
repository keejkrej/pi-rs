//! Port of packages/ai/test/classifier-models.test.ts

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use serde_json::{Value, json};

use pi_ai::auth::credential_store::InMemoryCredentialStore;
use pi_ai::auth::types::{
    ApiKeyAuth, ApiKeyAuthInput, ApiKeyCredential, AuthResult, Credential, ModelAuth, OAuthCredential, ProviderAuth,
};
use pi_ai::models::{
    CreateModelsOptions, CreateProviderApi, CreateProviderOptions, ModelsClassifierOptions, create_models,
    create_provider, get_model_type,
};
use pi_ai::providers::all::{
    builtin_models, get_all_builtin_models, get_builtin_classifier_model, get_builtin_classifier_models,
};
use pi_ai::types::{
    AnyModel, ClassifierAnswer, ClassifierBoolCriteria, ClassifierContext, ClassifierModel, ClassifierOptions,
    ClassifierQuestion, ClassifierResult, ClassifierStopReason, FetchFunction, ImageContent, Model, ModelCost,
    ModelModality, ModelType, ProviderClassifier, ProviderRequestOptions, ProviderStreams, SystemOneCompat,
    SystemOneImageInput,
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
        compat: None,
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
        sampling_params_by_thinking_level: None,
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
    ClassifierContext {
        state,
        questions,
        images: None,
    }
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
        (
            "opencode",
            "jev-1.13",
            "https://opencode.ai/inference/systemone/v1/systemone",
        ),
        (
            "opencode",
            "jev-1.13-free",
            "https://opencode.ai/inference/systemone/v1/systemone",
        ),
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

        let opts = ModelsClassifierOptions {
            base: ClassifierOptions {
                base: ProviderRequestOptions {
                    api_key: Some("secret".to_owned()),
                    fetch: Some(fetch_fn),
                    ..Default::default()
                },
                temperature: None,
            },
            transform_headers: None,
        };

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
        assert_eq!(
            model.compat,
            Some(SystemOneCompat {
                image_input: Some(SystemOneImageInput::StateImageUrls),
                send_image_opt_in_header: Some(false),
            })
        );
        assert!(models.get_model("openrouter", &model.id).is_none());
        assert_eq!(
            models.get_model_of_type(ModelType::Classifier, "openrouter", &model.id),
            Some(AnyModel::Classifier(model))
        );
    }
}

#[tokio::test]
async fn rejects_images_for_classifier_models_without_image_input_before_calling_the_provider() {
    let classifier = classifier_model("test", "text-only");
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let calls_ref = Arc::clone(&calls);
    let id = classifier.id.clone();
    let api = classifier.api.clone();
    let provider = classifier.provider.clone();
    let mut classifiers = IndexMap::new();
    classifiers.insert(
        "test-classifier".to_owned(),
        ProviderClassifier {
            classify: Arc::new(move |_m, _c, _o| {
                calls_ref.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let id = id.clone();
                let api = api.clone();
                let provider = provider.clone();
                Box::pin(async move {
                    ClassifierResult {
                        api,
                        provider,
                        model: id,
                        answers: IndexMap::new(),
                        usage: None,
                        stop_reason: ClassifierStopReason::Stop,
                        error_message: None,
                        timestamp: pi_js::time::now_ms(),
                    }
                })
            }),
        },
    );
    let models = create_models(None);
    models.set_provider(
        create_provider(CreateProviderOptions {
            id: "test".to_owned(),
            name: None,
            base_url: None,
            headers: None,
            auth: ProviderAuth {
                api_key: Some(Arc::new(StaticOkAuth)),
                oauth: None,
            },
            models: vec![AnyModel::Classifier(classifier.clone())],
            fetch_models: None,
            filter_models: None,
            filter_all_models: None,
            api: None,
            images: None,
            classifiers: Some(classifiers),
        })
        .expect("create_provider"),
    );

    let mut with_img = bool_context();
    with_img.images = Some(vec![ImageContent::new("aW1hZ2U=", "image/png")]);
    let mut without_img = bool_context();
    without_img.images = Some(Vec::new());

    let result = models.classify(&classifier, &with_img, None).await;
    let ok = models.classify(&classifier, &without_img, None).await;

    assert_eq!(result.stop_reason, ClassifierStopReason::Error);
    assert_eq!(
        result.error_message.as_deref(),
        Some("Model test/text-only does not accept image input")
    );
    assert_eq!(ok.stop_reason, ClassifierStopReason::Stop);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test]
async fn routes_openai_gpt_6_luna_through_the_decisions_api_with_images() {
    let models = builtin_models(None);
    let luna = match models.get_model_of_type(ModelType::Classifier, "openai", "gpt-6-luna") {
        Some(AnyModel::Classifier(m)) => m,
        other => panic!("missing OpenAI Decisions model: {other:?}"),
    };
    assert_eq!(luna.api, "openai-decisions");
    assert_eq!(luna.input, vec![ModelModality::Text, ModelModality::Image]);
    assert_eq!(luna.context_window, 922_000);
    assert_eq!(
        models.get_model("openai", "gpt-6-luna").map(|m| m.api),
        Some("openai-responses".to_owned())
    );

    let urls = Arc::new(Mutex::new(Vec::<String>::new()));
    let urls_ref = Arc::clone(&urls);
    let fetch_fn: FetchFunction = Arc::new(move |req| {
        let urls_ref = Arc::clone(&urls_ref);
        Box::pin(async move {
            urls_ref.lock().unwrap().push(req.url.clone());
            Ok(Response::new(
                200,
                Headers::from_pairs([("content-type", "application/json")]),
                pi_js::json::stringify(&json!({
                    "answers": [{ "type": "predicate", "name": "approved", "probability": 0.8 }]
                })),
            ))
        })
    });
    let mut ctx = bool_context();
    ctx.images = Some(vec![ImageContent::new("aW1hZ2U=", "image/png")]);
    let opts = ModelsClassifierOptions {
        base: ClassifierOptions {
            base: ProviderRequestOptions {
                api_key: Some("secret".to_owned()),
                fetch: Some(fetch_fn),
                ..Default::default()
            },
            temperature: None,
        },
        transform_headers: None,
    };

    let result = models.classify(&luna, &ctx, Some(opts)).await;
    assert_eq!(
        *urls.lock().unwrap(),
        vec!["https://api.openai.com/v1/decisions".to_owned()]
    );
    assert_eq!(result.stop_reason, ClassifierStopReason::Stop);
    assert_eq!(
        result.answers.get("approved"),
        Some(&ClassifierAnswer::Bool { probability: 0.8 })
    );
}

#[tokio::test]
async fn lists_openai_decisions_models_only_for_api_key_credentials() {
    let api_key_store = InMemoryCredentialStore::new();
    api_key_store
        .modify_with(
            "openai",
            |_| async move {
                Ok(Some(Credential::ApiKey(ApiKeyCredential {
                    key: Some("secret".to_owned()),
                    env: None,
                })))
            },
            None,
        )
        .await
        .unwrap();

    let oauth_store = InMemoryCredentialStore::new();
    oauth_store
        .modify_with(
            "openai",
            |_| async move {
                Ok(Some(Credential::OAuth(OAuthCredential::new(
                    "refresh",
                    "access",
                    pi_js::time::now_ms() + 3_600_000,
                ))))
            },
            None,
        )
        .await
        .unwrap();

    let with_api_key = builtin_models(Some(CreateModelsOptions {
        credentials: Some(Arc::new(api_key_store)),
        ..Default::default()
    }));
    let with_oauth = builtin_models(Some(CreateModelsOptions {
        credentials: Some(Arc::new(oauth_store)),
        ..Default::default()
    }));

    let api_key_classifiers: Vec<String> = with_api_key
        .get_available_of_type(ModelType::Classifier, Some("openai"), None)
        .await
        .unwrap()
        .into_iter()
        .map(|m| m.id().to_owned())
        .collect();
    assert_eq!(api_key_classifiers, vec!["gpt-6-luna".to_owned()]);
    assert!(
        with_oauth
            .get_available_of_type(ModelType::Classifier, Some("openai"), None)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        with_oauth
            .get_available(Some("openai"), None)
            .await
            .unwrap()
            .iter()
            .any(|m| m.id == "gpt-6-luna")
    );
}

#[tokio::test]
async fn routes_github_copilot_microsoft_decision_1_to_the_accounts_v1_systemone_endpoint() {
    let credentials = InMemoryCredentialStore::new();
    credentials
        .modify_with(
            "github-copilot",
            |_| async move {
                let mut cred = OAuthCredential::new(
                    "refresh",
                    "tid=test;exp=9999999999;proxy-ep=proxy.enterprise.githubcopilot.com;",
                    pi_js::time::now_ms() + 3_600_000,
                );
                cred.set_extra("availableModelIds", json!(["gpt-6-luna"]));
                Ok(Some(Credential::OAuth(cred)))
            },
            None,
        )
        .await
        .unwrap();
    let models = builtin_models(Some(CreateModelsOptions {
        credentials: Some(Arc::new(credentials)),
        ..Default::default()
    }));
    let decision = match models.get_model_of_type(ModelType::Classifier, "github-copilot", "microsoft-decision-1") {
        Some(AnyModel::Classifier(m)) => m,
        other => panic!("missing GitHub Copilot decision model: {other:?}"),
    };
    assert_eq!(decision.api, "typesafe-system-one");
    assert_eq!(decision.base_url, "https://api.individual.githubcopilot.com");
    assert_eq!(decision.input, vec![ModelModality::Text]);
    assert_eq!(decision.context_window, 32768);
    assert!(models.get_model("github-copilot", "microsoft-decision-1").is_none());
    assert_eq!(
        models
            .get_available_of_type(ModelType::Classifier, Some("github-copilot"), None)
            .await
            .unwrap(),
        vec![AnyModel::Classifier(decision.clone())]
    );

    let requests = Arc::new(Mutex::new(Vec::<(String, Headers)>::new()));
    let reqs_ref = Arc::clone(&requests);
    let fetch_fn: FetchFunction = Arc::new(move |req| {
        let reqs_ref = Arc::clone(&reqs_ref);
        Box::pin(async move {
            reqs_ref.lock().unwrap().push((req.url.clone(), req.headers));
            Ok(Response::new(
                200,
                Headers::from_pairs([("content-type", "application/json")]),
                pi_js::json::stringify(&json!({ "answers": { "approved": { "type": "noul", "noul": 0.8 } } })),
            ))
        })
    });
    let opts = ModelsClassifierOptions {
        base: ClassifierOptions {
            base: ProviderRequestOptions {
                fetch: Some(fetch_fn),
                ..Default::default()
            },
            temperature: None,
        },
        transform_headers: None,
    };

    let result = models.classify(&decision, &bool_context(), Some(opts)).await;
    let guard = requests.lock().unwrap();
    assert_eq!(guard.len(), 1);
    assert_eq!(guard[0].0, "https://api.enterprise.githubcopilot.com/v1/systemone");
    assert_eq!(
        guard[0].1.get("authorization").as_deref(),
        Some("Bearer tid=test;exp=9999999999;proxy-ep=proxy.enterprise.githubcopilot.com;")
    );
    assert!(guard[0].1.get("editor-version").is_some_and(|v| !v.is_empty()));
    assert_eq!(guard[0].1.get("copilot-integration-id").as_deref(), Some("vscode-chat"));
    assert_eq!(result.stop_reason, ClassifierStopReason::Stop);
    assert_eq!(
        result.answers.get("approved"),
        Some(&ClassifierAnswer::Bool { probability: 0.8 })
    );
}

#[tokio::test]
async fn adds_the_system_one_version_prefix_to_the_github_copilot_catalog_base_url() {
    let models = builtin_models(None);
    let decision = match models.get_model_of_type(ModelType::Classifier, "github-copilot", "microsoft-decision-1") {
        Some(AnyModel::Classifier(m)) => m,
        other => panic!("missing GitHub Copilot decision model: {other:?}"),
    };
    let urls = Arc::new(Mutex::new(Vec::<String>::new()));
    let urls_ref = Arc::clone(&urls);
    let fetch_fn: FetchFunction = Arc::new(move |req| {
        let urls_ref = Arc::clone(&urls_ref);
        Box::pin(async move {
            urls_ref.lock().unwrap().push(req.url.clone());
            Ok(Response::new(
                200,
                Headers::from_pairs([("content-type", "application/json")]),
                pi_js::json::stringify(&json!({ "answers": { "approved": { "type": "noul", "noul": 0.8 } } })),
            ))
        })
    });
    let opts = ModelsClassifierOptions {
        base: ClassifierOptions {
            base: ProviderRequestOptions {
                api_key: Some("secret".to_owned()),
                fetch: Some(fetch_fn),
                ..Default::default()
            },
            temperature: None,
        },
        transform_headers: None,
    };
    models.classify(&decision, &bool_context(), Some(opts)).await;
    assert_eq!(
        *urls.lock().unwrap(),
        vec!["https://api.individual.githubcopilot.com/v1/systemone".to_owned()]
    );
}
