//! Port of packages/ai/test/model-types.test.ts (unit ai-providers-builtin)

use std::sync::Arc;

use pi_ai::auth::types::{ApiKeyAuth, ApiKeyAuthInput, AuthResult, ProviderAuth};
use pi_ai::compat::get_model as get_compat_model;
use pi_ai::models::{
    CreateProviderApi, CreateProviderOptions, ModelsRefreshOptions, create_models, create_provider, get_model_type,
    has_api, is_model_type, models_are_equal,
};
use pi_ai::models_store::{InMemoryModelsStore, ModelsStore, ModelsStoreEntry};
use pi_ai::providers::faux::{
    FauxAssistantMessageContent, FauxResponseStep, RegisterFauxProviderOptions, faux_assistant_message, faux_provider,
};
use pi_ai::types::{
    AnyModel, Context, ImageModel, Message, Model, ModelCost, ModelInput, ModelOutput, ModelType, ProviderStreams,
    StopReason, UserMessage, UserMessageContent,
};
use pi_ai::utils::event_stream::AssistantMessageEventStream;

fn chat_model(provider: &str, id: &str) -> Model {
    Model {
        r#type: None,
        id: id.to_string(),
        name: id.to_string(),
        api: "test-chat".to_string(),
        provider: provider.into(),
        base_url: "https://example.test/v1".to_string(),
        reasoning: false,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text],
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

fn image_model(provider: &str, id: &str) -> ImageModel {
    ImageModel {
        id: id.to_string(),
        name: id.to_string(),
        api: "test-images".to_string(),
        provider: provider.into(),
        base_url: "https://example.test/v1".to_string(),
        input: vec![ModelInput::Text],
        input_limits: None,
        output: vec![ModelOutput::Image],
        cost: ModelCost::default(),
        headers: None,
    }
}

struct TestNoAuth;

#[async_trait::async_trait]
impl ApiKeyAuth for TestNoAuth {
    fn name(&self) -> &str {
        "Test"
    }

    async fn resolve(&self, _input: ApiKeyAuthInput) -> pi_js::Result<Option<AuthResult>> {
        Ok(Some(AuthResult::default()))
    }
}

fn no_auth() -> ProviderAuth {
    ProviderAuth {
        api_key: Some(Arc::new(TestNoAuth)),
        oauth: None,
    }
}

#[tokio::test]
async fn chat_models_without_type_work_and_narrow_with_is_model_type() {
    let faux = faux_provider(Some(RegisterFauxProviderOptions {
        provider: Some("handwritten".to_string()),
        ..Default::default()
    }));
    let base_prov = faux.provider();
    let p_stream = Arc::clone(&base_prov);
    let p_simple = Arc::clone(&base_prov);
    let prov = create_provider(CreateProviderOptions {
        id: "handwritten".to_string(),
        name: Some("Handwritten".to_string()),
        base_url: None,
        headers: None,
        auth: base_prov.auth().clone(),
        models: faux.models().iter().cloned().map(AnyModel::Chat).collect(),
        fetch_models: None,
        filter_models: None,
        filter_all_models: None,
        api: Some(CreateProviderApi::Single(ProviderStreams {
            stream: Arc::new(move |m, c, o| p_stream.stream(m, c, o)),
            stream_simple: Arc::new(move |m, c, o| p_simple.stream_simple(m, c, o)),
            fetch_deferred: None,
            cancel_deferred: None,
        })),
        images: None,
        classifiers: None,
    })
    .unwrap();

    let models = create_models(None);
    models.set_provider(prov);

    let first_id = faux.models()[0].id.clone();
    let model = models.get_model("handwritten", &first_id).unwrap();
    assert_eq!(model.r#type, None);

    let any_chat = AnyModel::Chat(model.clone());
    assert_eq!(get_model_type(&any_chat), ModelType::Chat);
    assert!(has_api(&any_chat, &model.api));

    let mut typed_chat = model.clone();
    typed_chat.r#type = Some(pi_ai::types::ChatModelType::Chat);
    assert!(models_are_equal(
        Some(&any_chat),
        Some(&AnyModel::Chat(typed_chat.clone()))
    ));
    assert!(!models_are_equal(
        Some(&any_chat),
        Some(&AnyModel::Image(image_model(&model.provider, &model.id)))
    ));

    assert_eq!(
        models.get_models_of_type(ModelType::Chat, Some("handwritten")).len(),
        faux.models().len()
    );
    assert_eq!(models.get_all_models(Some("handwritten")).len(), faux.models().len());
    assert!(
        models
            .get_models_of_type(ModelType::Image, Some("handwritten"))
            .is_empty()
    );

    faux.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text("hi".to_string()),
        None,
    ))]);
    let res = models
        .complete(
            &model,
            &Context {
                system_prompt: None,
                messages: vec![Message::User(UserMessage {
                    content: UserMessageContent::Text("hi".to_string()),
                    timestamp: 0,
                })],
                tools: None,
            },
            None,
        )
        .await;
    assert_eq!(res.stop_reason, StopReason::Stop);

    let mixed = vec![
        AnyModel::Chat(chat_model("p", "c")),
        AnyModel::Chat(Model {
            r#type: Some(pi_ai::types::ChatModelType::Chat),
            ..chat_model("p", "typed")
        }),
        AnyModel::Image(image_model("p", "i")),
    ];
    let chat_ids: Vec<_> = mixed
        .iter()
        .filter(|m| is_model_type(m, ModelType::Chat))
        .map(|m| m.id().to_string())
        .collect();
    assert_eq!(chat_ids, vec!["c", "typed"]);
    let image_ids: Vec<_> = mixed
        .iter()
        .filter(|m| is_model_type(m, ModelType::Image))
        .map(|m| m.id().to_string())
        .collect();
    assert_eq!(image_ids, vec!["i"]);
    assert!(mixed.iter().all(|m| !is_model_type(m, ModelType::Classifier)));
}

#[test]
fn builtin_catalog_getters_return_model_shapes_that_can_be_reassigned() {
    let mut compat = get_compat_model("openai", "gpt-4o-mini").unwrap();
    assert_eq!(compat.id, "gpt-4o-mini");
    compat = get_compat_model("openai", "gpt-4o").unwrap();
    assert_eq!(compat.id, "gpt-4o");

    let mut builtin = pi_ai::providers::all::get_builtin_model("openai", "gpt-4o-mini").unwrap();
    assert_eq!(builtin.id, "gpt-4o-mini");
    builtin = pi_ai::providers::all::get_builtin_model("openai", "gpt-4o").unwrap();
    assert_eq!(builtin.id, "gpt-4o");
}

#[tokio::test]
async fn stored_and_fetched_models_work_cleanly_in_refresh() {
    let models_store = Arc::new(InMemoryModelsStore::new());
    models_store
        .write(
            "dyn",
            ModelsStoreEntry {
                models: vec![
                    AnyModel::Chat(chat_model("dyn", "stored-chat")),
                    AnyModel::Image(image_model("dyn", "stored-image")),
                ],
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();

    let models = create_models(Some(pi_ai::models::CreateModelsOptions {
        credentials: None,
        models_store: Some(models_store.clone()),
        auth_context: None,
        identity: None,
    }));
    let chat_streams = ProviderStreams {
        stream: Arc::new(|_, _, _| AssistantMessageEventStream::new()),
        stream_simple: Arc::new(|_, _, _| AssistantMessageEventStream::new()),
        fetch_deferred: None,
        cancel_deferred: None,
    };
    let prov = create_provider(CreateProviderOptions {
        id: "dyn".to_string(),
        name: None,
        base_url: None,
        headers: None,
        auth: no_auth(),
        models: Vec::new(),
        fetch_models: Some(Arc::new(|_input| {
            Box::pin(async move { Ok(vec![AnyModel::Chat(chat_model("dyn", "fetched-chat"))]) })
        })),
        filter_models: None,
        filter_all_models: None,
        api: Some(CreateProviderApi::Single(chat_streams)),
        images: None,
        classifiers: None,
    })
    .unwrap();
    models.set_provider(prov);

    let restored = models
        .refresh(Some(ModelsRefreshOptions {
            providers: Some(vec!["dyn".to_string()]),
            allow_network: Some(false),
            ..Default::default()
        }))
        .await
        .unwrap();
    assert!(restored.errors.is_empty());
    let restored_ids: Vec<_> = models
        .get_all_models(Some("dyn"))
        .into_iter()
        .map(|m| m.id().to_string())
        .collect();
    assert_eq!(restored_ids, vec!["stored-chat", "stored-image"]);

    let refreshed = models
        .refresh(Some(ModelsRefreshOptions {
            providers: Some(vec!["dyn".to_string()]),
            ..Default::default()
        }))
        .await
        .unwrap();
    assert!(refreshed.errors.is_empty());
    let refreshed_ids: Vec<_> = models
        .get_all_models(Some("dyn"))
        .into_iter()
        .map(|m| m.id().to_string())
        .collect();
    assert_eq!(refreshed_ids, vec!["fetched-chat"]);
}
