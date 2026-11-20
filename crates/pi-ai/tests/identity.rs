//! Port of packages/ai/test/identity.test.ts

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use pi_ai::api::openai_completions::{stream as stream_completions, stream_simple as stream_simple_completions};
use pi_ai::auth::types::{ApiKeyAuth, ApiKeyAuthInput, AuthResult, ModelAuth, ProviderAuth};
use pi_ai::compat::normalize_context;
use pi_ai::models::{
    CreateModelsOptions, CreateProviderApi, CreateProviderOptions, Models, ModelsSimpleStreamOptions, create_models,
    create_provider,
};
use pi_ai::types::{
    Context, FetchFunction, Message, Model, ModelCost, ModelInput, ProviderHeaders, ProviderStreams,
    SimpleStreamOptions, StreamOptions, UserMessage, UserMessageContent,
};
use pi_ai::utils::identity::{
    ClientAttribution, ClientIdentity, ClientIdentityTarget, UserAgentMode, format_user_agent, get_attribution_headers,
    get_identity_headers, merge_client_identity, pi_attribution, with_client_identity,
};
use pi_ai::utils::version::PI_AI_VERSION;
use pi_js::env::{arch, platform};
use pi_js::fetch::{Headers, Request, Response};
use serde_json::{Value, json};

fn acme() -> ClientAttribution {
    ClientAttribution {
        name: "acme".to_owned(),
        url: "https://acme.example".to_owned(),
        categories: None,
    }
}

struct StaticKeyAuth(&'static str);

#[async_trait::async_trait]
impl ApiKeyAuth for StaticKeyAuth {
    fn name(&self) -> &str {
        "Test"
    }

    async fn resolve(&self, _input: ApiKeyAuthInput) -> pi_js::Result<Option<AuthResult>> {
        Ok(Some(AuthResult {
            auth: ModelAuth {
                api_key: Some(self.0.to_owned()),
                headers: None,
                base_url: None,
            },
            env: None,
            source: None,
        }))
    }
}

fn target<'a>(provider: &'a str, base_url: &'a str) -> ClientIdentityTarget<'a> {
    ClientIdentityTarget { provider, base_url }
}

fn completions_model(provider: &str, base_url: &str, headers: Option<IndexMap<String, String>>) -> Model {
    Model {
        r#type: None,
        id: "test-model".to_owned(),
        name: "Test Model".to_owned(),
        api: "openai-completions".to_owned(),
        provider: provider.into(),
        base_url: base_url.to_owned(),
        reasoning: false,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: ModelCost::default(),
        context_window: 128_000,
        max_tokens: 4096,
        headers,
        sampling_params: None,
        compat: None,
        sampling_params_by_thinking_level: None,
    }
}

fn capturing_fetch() -> (FetchFunction, Arc<Mutex<Option<Headers>>>) {
    let captured = Arc::new(Mutex::new(None::<Headers>));
    let cap_c = Arc::clone(&captured);
    let fetch: FetchFunction = Arc::new(move |req: Request| {
        *cap_c.lock().unwrap() = Some(req.headers);
        let c1 = json!({
            "id": "chatcmpl-identity",
            "choices": [{ "delta": { "content": "ok" }, "finish_reason": Value::Null, "index": 0 }]
        });
        let c2 = json!({
            "id": "chatcmpl-identity",
            "choices": [{ "delta": {}, "finish_reason": "stop", "index": 0 }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 1 }
        });
        let body = format!("data: {c1}\n\ndata: {c2}\n\ndata: [DONE]\n\n");
        Box::pin(async move {
            Ok(Response::new(
                200,
                Headers::from_pairs([("content-type", "text/event-stream")]),
                body,
            ))
        })
    });
    (fetch, captured)
}

#[test]
fn formats_user_agents_merges_identities_and_attributes_providers() {
    assert_eq!(PI_AI_VERSION, "1.5.2");

    assert_eq!(
        format_user_agent(None),
        format!("pi/{PI_AI_VERSION} ({}; rust; {})", platform(), arch())
    );
    assert_eq!(
        format_user_agent(Some(&ClientIdentity {
            version: Some("9.9.9".to_owned()),
            ..Default::default()
        })),
        format!("pi/9.9.9 ({}; rust; {})", platform(), arch())
    );
    assert_eq!(
        format_user_agent(Some(&ClientIdentity {
            user_agent: Some(UserAgentMode::SHORT),
            version: Some("9.9.9".to_owned()),
            ..Default::default()
        })),
        "pi"
    );
    assert_eq!(
        format_user_agent(Some(&ClientIdentity {
            user_agent: Some(UserAgentMode::custom("acme/1.0")),
            ..Default::default()
        })),
        "acme/1.0"
    );

    assert_eq!(merge_client_identity(&[None, None]), None);
    let i1 = ClientIdentity {
        attribution: Some(Some(pi_attribution())),
        user_agent: Some(UserAgentMode::SHORT),
        version: None,
    };
    let i2 = ClientIdentity {
        attribution: None,
        user_agent: None,
        version: Some("1.0.0".to_owned()),
    };
    assert_eq!(
        merge_client_identity(&[Some(&i1), Some(&i2)]),
        Some(ClientIdentity {
            attribution: Some(Some(pi_attribution())),
            user_agent: Some(UserAgentMode::SHORT),
            version: Some("1.0.0".to_owned()),
        })
    );
    let clear = ClientIdentity {
        attribution: Some(None),
        ..Default::default()
    };
    assert_eq!(
        merge_client_identity(&[
            Some(&ClientIdentity {
                attribution: Some(Some(pi_attribution())),
                ..Default::default()
            }),
            Some(&clear),
        ]),
        Some(clear)
    );

    let or_t = target("openrouter", "https://openrouter.ai/api/v1");
    assert!(get_attribution_headers(or_t, None).is_empty());
    assert!(
        get_attribution_headers(
            or_t,
            Some(&ClientIdentity {
                attribution: Some(None),
                ..Default::default()
            })
        )
        .is_empty()
    );
    assert_eq!(
        get_identity_headers(or_t, None).get("User-Agent").map(String::as_str),
        Some(format_user_agent(None).as_str())
    );

    let or_pi = get_attribution_headers(
        or_t,
        Some(&ClientIdentity {
            attribution: Some(Some(pi_attribution())),
            ..Default::default()
        }),
    );
    assert_eq!(or_pi.get("HTTP-Referer").map(String::as_str), Some("https://pi.dev"));
    assert_eq!(or_pi.get("X-OpenRouter-Title").map(String::as_str), Some("pi"));
    assert_eq!(
        or_pi.get("X-OpenRouter-Categories").map(String::as_str),
        Some("cli-agent")
    );

    let mut acme_cats = acme();
    acme_cats.categories = Some(vec!["cli-agent".to_owned(), "cloud-agent".to_owned()]);
    let or_acme = get_attribution_headers(
        or_t,
        Some(&ClientIdentity {
            attribution: Some(Some(acme_cats)),
            ..Default::default()
        }),
    );
    assert_eq!(
        or_acme.get("X-OpenRouter-Categories").map(String::as_str),
        Some("cli-agent,cloud-agent")
    );

    let mut acme_empty = acme();
    acme_empty.categories = Some(Vec::new());
    let or_empty = get_attribution_headers(
        or_t,
        Some(&ClientIdentity {
            attribution: Some(Some(acme_empty)),
            ..Default::default()
        }),
    );
    assert!(!or_empty.contains_key("X-OpenRouter-Categories"));

    assert_eq!(
        get_attribution_headers(
            target("custom", "not-a-url-openrouter.ai"),
            Some(&ClientIdentity {
                attribution: Some(Some(pi_attribution())),
                ..Default::default()
            })
        )
        .get("X-OpenRouter-Title")
        .map(String::as_str),
        Some("pi")
    );

    // NVIDIA NIM & OpenCode
    assert_eq!(
        get_attribution_headers(
            target("nvidia", "https://example.test/v1"),
            Some(&ClientIdentity {
                attribution: Some(Some(pi_attribution())),
                ..Default::default()
            })
        )
        .get("X-BILLING-INVOKE-ORIGIN")
        .map(String::as_str),
        Some("Pi")
    );
    assert_eq!(
        get_attribution_headers(
            target("custom-nim", "https://integrate.api.nvidia.com/v1"),
            Some(&ClientIdentity {
                attribution: Some(Some(acme())),
                ..Default::default()
            })
        )
        .get("X-BILLING-INVOKE-ORIGIN")
        .map(String::as_str),
        Some("Acme")
    );
    assert_eq!(
        get_attribution_headers(
            target("opencode", "https://opencode.ai/zen/v1"),
            Some(&ClientIdentity {
                attribution: Some(Some(pi_attribution())),
                ..Default::default()
            })
        )
        .get("x-opencode-client")
        .map(String::as_str),
        Some("pi")
    );
    assert_eq!(
        get_attribution_headers(
            target("custom", "https://opencode.ai/zen/v1"),
            Some(&ClientIdentity {
                attribution: Some(Some(acme())),
                ..Default::default()
            })
        )
        .get("x-opencode-client")
        .map(String::as_str),
        Some("acme")
    );

    assert!(
        !get_attribution_headers(
            or_t,
            Some(&ClientIdentity {
                attribution: Some(Some(pi_attribution())),
                ..Default::default()
            })
        )
        .contains_key("X-BILLING-INVOKE-ORIGIN")
    );
    assert!(
        get_attribution_headers(
            target("vercel-ai-gateway", "https://ai-gateway.vercel.sh/v1"),
            Some(&ClientIdentity {
                attribution: Some(Some(pi_attribution())),
                ..Default::default()
            })
        )
        .is_empty()
    );
}

#[tokio::test]
async fn applies_identity_layering_in_requests_and_models_handles() {
    let context = Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("hello".to_owned()),
            timestamp: 1,
        })],
        tools: None,
    };
    let tx = normalize_context(&context);

    // 1. Model & request headers override identity
    let (f1, c1) = capturing_fetch();
    let mut model_hdrs = IndexMap::new();
    model_hdrs.insert("X-OpenRouter-Title".to_owned(), "model-title".to_owned());
    let model1 = completions_model("openrouter", "https://openrouter.ai/api/v1", Some(model_hdrs));
    let mut req_hdrs: ProviderHeaders = IndexMap::new();
    req_hdrs.insert("HTTP-Referer".to_owned(), None);
    stream_completions(
        &model1,
        &tx,
        Some(
            StreamOptions {
                api_key: Some("key".to_owned()),
                fetch: Some(f1),
                identity: Some(ClientIdentity {
                    attribution: Some(Some(pi_attribution())),
                    user_agent: Some(UserAgentMode::SHORT),
                    version: None,
                }),
                headers: Some(req_hdrs),
                ..Default::default()
            }
            .into(),
        ),
    )
    .result()
    .await;
    let h1 = c1.lock().unwrap().take().unwrap();
    assert_eq!(h1.get("user-agent").as_deref(), Some("pi"));
    assert_eq!(h1.get("x-openrouter-title").as_deref(), Some("model-title"));
    assert!(h1.get("http-referer").is_none());

    // 2. Copilot custom model header beats identity User-Agent
    let (f2, c2) = capturing_fetch();
    let mut copilot_hdrs = IndexMap::new();
    copilot_hdrs.insert("User-Agent".to_owned(), "GitHubCopilotChat/0.35.0".to_owned());
    let copilot_model = completions_model("github-copilot", "https://api.githubcopilot.com", Some(copilot_hdrs));
    stream_completions(
        &copilot_model,
        &tx,
        Some(
            StreamOptions {
                api_key: Some("key".to_owned()),
                fetch: Some(f2),
                identity: Some(ClientIdentity {
                    user_agent: Some(UserAgentMode::custom("acme")),
                    ..Default::default()
                }),
                ..Default::default()
            }
            .into(),
        ),
    )
    .result()
    .await;
    assert_eq!(
        c2.lock().unwrap().take().unwrap().get("user-agent").as_deref(),
        Some("GitHubCopilotChat/0.35.0")
    );

    // 3. Request > wrapper > Models identity precedence + MutableModels stays mutable
    let model = completions_model("openrouter", "https://openrouter.ai/api/v1", None);
    let make_attribution = |name: &str| ClientAttribution {
        name: name.to_owned(),
        url: "https://acme.example".to_owned(),
        categories: None,
    };
    let models = create_models(Some(CreateModelsOptions {
        identity: Some(ClientIdentity {
            attribution: Some(Some(make_attribution("models"))),
            user_agent: Some(UserAgentMode::SHORT),
            version: None,
        }),
        ..Default::default()
    }));
    let register_or = |registry: &pi_ai::models::MutableModels, m: &Model| {
        registry.set_provider(
            create_provider(CreateProviderOptions {
                id: "openrouter".to_owned(),
                name: None,
                base_url: None,
                headers: None,
                auth: ProviderAuth {
                    api_key: Some(Arc::new(StaticKeyAuth("key"))),
                    oauth: None,
                },
                models: vec![pi_ai::types::AnyModel::Chat(m.clone())],
                fetch_models: None,
                filter_models: None,
                filter_all_models: None,
                api: Some(CreateProviderApi::Single(ProviderStreams {
                    stream: Arc::new(|m, c, o| stream_completions(m, c, o)),
                    stream_simple: Arc::new(|m, c, o| stream_simple_completions(m, c, o)),
                    fetch_deferred: None,
                    cancel_deferred: None,
                })),
                images: None,
                classifiers: None,
            })
            .unwrap(),
        );
    };
    register_or(&models, &model);

    let send = |target_models: Models, identity: Option<ClientIdentity>| {
        let model = model.clone();
        let context = context.clone();
        async move {
            let (fetch, cap) = capturing_fetch();
            target_models
                .stream_simple(
                    &model,
                    &context,
                    Some(ModelsSimpleStreamOptions {
                        base: SimpleStreamOptions {
                            base: StreamOptions {
                                fetch: Some(fetch),
                                identity,
                                ..Default::default()
                            },
                            ..Default::default()
                        },
                        transform_headers: None,
                    }),
                )
                .result()
                .await;
            cap.lock().unwrap().take().unwrap()
        }
    };

    let from_models = send(models.clone(), None).await;
    assert_eq!(from_models.get("user-agent").as_deref(), Some("pi"));
    assert_eq!(from_models.get("x-openrouter-title").as_deref(), Some("models"));

    let wrapped = with_client_identity(
        &models,
        ClientIdentity {
            attribution: Some(Some(make_attribution("wrapper"))),
            ..Default::default()
        },
    );
    let from_wrapper = send(wrapped.clone(), None).await;
    assert_eq!(from_wrapper.get("user-agent").as_deref(), Some("pi"));
    assert_eq!(from_wrapper.get("x-openrouter-title").as_deref(), Some("wrapper"));
    assert_eq!(wrapped.get_model("openrouter", "test-model").as_ref(), Some(&model));

    models.delete_provider("openrouter");
    assert!(models.get_provider("openrouter").is_none());
    register_or(&models, &model);

    let from_request = send(
        wrapped,
        Some(ClientIdentity {
            attribution: Some(None),
            ..Default::default()
        }),
    )
    .await;
    assert!(from_request.get("x-openrouter-title").is_none());
    assert_eq!(from_request.get("user-agent").as_deref(), Some("pi"));

    // 4. OpenCode session header on custom provider pointing at opencode.ai
    let custom_oc = completions_model("custom-opencode", "https://opencode.ai/zen/v1", None);
    models.set_provider(
        create_provider(CreateProviderOptions {
            id: "custom-opencode".to_owned(),
            name: None,
            base_url: None,
            headers: None,
            auth: ProviderAuth {
                api_key: Some(Arc::new(StaticKeyAuth("key"))),
                oauth: None,
            },
            models: vec![pi_ai::types::AnyModel::Chat(custom_oc.clone())],
            fetch_models: None,
            filter_models: None,
            filter_all_models: None,
            api: Some(CreateProviderApi::Single(ProviderStreams {
                stream: Arc::new(|m, c, o| stream_completions(m, c, o)),
                stream_simple: Arc::new(|m, c, o| stream_simple_completions(m, c, o)),
                fetch_deferred: None,
                cancel_deferred: None,
            })),
            images: None,
            classifiers: None,
        })
        .unwrap(),
    );
    let (f_oc1, c_oc1) = capturing_fetch();
    models
        .stream_simple(
            &custom_oc,
            &context,
            Some(ModelsSimpleStreamOptions {
                base: SimpleStreamOptions {
                    base: StreamOptions {
                        fetch: Some(f_oc1),
                        session_id: Some("session-1".to_owned()),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                transform_headers: None,
            }),
        )
        .result()
        .await;
    assert_eq!(
        c_oc1
            .lock()
            .unwrap()
            .take()
            .unwrap()
            .get("x-opencode-session")
            .as_deref(),
        Some("session-1")
    );

    let (f_oc2, c_oc2) = capturing_fetch();
    let mut configured = IndexMap::new();
    configured.insert("X-OpenCode-Session".to_owned(), Some("configured".to_owned()));
    models
        .stream_simple(
            &custom_oc,
            &context,
            Some(ModelsSimpleStreamOptions {
                base: SimpleStreamOptions {
                    base: StreamOptions {
                        fetch: Some(f_oc2),
                        session_id: Some("session-1".to_owned()),
                        headers: Some(configured),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                transform_headers: None,
            }),
        )
        .result()
        .await;
    assert_eq!(
        c_oc2
            .lock()
            .unwrap()
            .take()
            .unwrap()
            .get("x-opencode-session")
            .as_deref(),
        Some("configured")
    );
}
