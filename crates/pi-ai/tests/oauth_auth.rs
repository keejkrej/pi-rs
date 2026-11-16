//! Port of packages/ai/test/oauth-auth.test.ts

use std::sync::{Arc, Mutex};

use pi_ai::auth::credential_store::InMemoryCredentialStore;
use pi_ai::auth::helpers::lazy_oauth;
use pi_ai::auth::oauth::anthropic::anthropic_oauth;
use pi_ai::auth::oauth::github_copilot::github_copilot_oauth;
use pi_ai::auth::oauth::kimi_coding::kimi_coding_oauth;
use pi_ai::auth::oauth::load::load_github_copilot_oauth;
use pi_ai::auth::oauth::openai_chatgpt::openai_chatgpt_oauth;
use pi_ai::auth::oauth::openai_codex::openai_codex_oauth;
use pi_ai::auth::oauth::openrouter::openrouter_oauth;
use pi_ai::auth::oauth::xai::xai_oauth;
use pi_ai::auth::types::{AuthOperationOptions, Credential, CredentialStore, ModelAuth, OAuthCredential, ProviderAuth};
use pi_ai::models::{CreateModelsOptions, Provider, create_models};
use pi_ai::oauth as extension_oauth_compatibility;
use pi_ai::providers::anthropic::anthropic_provider;
use pi_ai::providers::openai::openai_provider;
use pi_ai::types::{ApiStreamOptions, Model, ModelCost, ModelInput, SimpleStreamOptions, TranscriptContext};
use pi_ai::utils::event_stream::AssistantMessageEventStream;
use pi_js::abort::AbortController;
use pi_js::fetch::testing::{mock_fetch, response};
use pi_js::time;
use serde_json::{Value, json};
use serial_test::serial;

fn json_resp(body: &Value) -> pi_js::fetch::Response {
    response(
        200,
        &[("Content-Type", "application/json")],
        pi_js::json::stringify(body),
    )
}

#[test]
fn keeps_the_extension_oauth_barrel_free_of_built_in_flow_implementations() {
    // `pi_ai::oauth` re-exports only `extension_oauth_types` (enforced statically at compile time).
    let _: fn() -> std::marker::PhantomData<(
        extension_oauth_compatibility::OAuthAuthInfo,
        extension_oauth_compatibility::OAuthCredentials,
        extension_oauth_compatibility::OAuthDeviceCodeInfo,
        extension_oauth_compatibility::OAuthPrompt,
    )> = || std::marker::PhantomData;
}

#[test]
fn identifies_only_subscription_backed_oauth_flows_as_subscriptions() {
    for oauth in [
        anthropic_oauth(),
        openai_chatgpt_oauth(),
        openai_codex_oauth(),
        github_copilot_oauth(),
        kimi_coding_oauth(),
        xai_oauth(),
    ] {
        assert!(oauth.is_subscription(), "{} should be subscription", oauth.name());
    }
    assert!(!openrouter_oauth().is_subscription());
}

#[tokio::test]
async fn anthropic_to_auth_derives_the_api_key_from_the_access_token() {
    let auth = anthropic_oauth()
        .to_auth(OAuthCredential::new("r", "token", 0))
        .await
        .unwrap();
    assert_eq!(
        auth,
        ModelAuth {
            api_key: Some("token".to_owned()),
            headers: None,
            base_url: None,
        }
    );
}

#[test]
fn openai_exposes_chatgpt_oauth_alongside_api_key_auth() {
    let provider = openai_provider().unwrap();
    let auth = provider.auth();
    assert!(auth.api_key.is_some());
    let oauth = auth.oauth.as_ref().unwrap();
    assert_eq!(oauth.name(), "OpenAI (ChatGPT subscription)");
    assert!(oauth.is_subscription());
    assert_eq!(oauth.login_label(), Some("Sign in with ChatGPT"));
}

#[tokio::test]
async fn openai_codex_to_auth_derives_the_api_key_from_the_access_token() {
    let auth = openai_codex_oauth()
        .to_auth(OAuthCredential::new("r", "token", 0))
        .await
        .unwrap();
    assert_eq!(
        auth,
        ModelAuth {
            api_key: Some("token".to_owned()),
            headers: None,
            base_url: None,
        }
    );
}

#[tokio::test]
async fn openrouter_derives_the_api_key_and_keeps_the_permanent_credential_on_refresh() {
    let controller = AbortController::new();
    let signal = controller.signal();
    let credential = OAuthCredential::new("", "token", i64::MAX);
    assert_eq!(
        openrouter_oauth().to_auth(credential.clone()).await.unwrap(),
        ModelAuth {
            api_key: Some("token".to_owned()),
            headers: None,
            base_url: None,
        }
    );
    assert_eq!(
        openrouter_oauth().refresh(credential.clone(), signal).await.unwrap(),
        credential
    );
}

#[tokio::test]
async fn xai_to_auth_derives_the_api_key_from_the_access_token() {
    let auth = xai_oauth()
        .to_auth(OAuthCredential::new("r", "token", 0))
        .await
        .unwrap();
    assert_eq!(
        auth,
        ModelAuth {
            api_key: Some("token".to_owned()),
            headers: None,
            base_url: None,
        }
    );
}

#[tokio::test]
async fn github_copilot_to_auth_derives_base_url_from_the_token_proxy_endpoint() {
    let access = "tid=abc;exp=123;proxy-ep=proxy.enterprise.example;rest";
    let auth = github_copilot_oauth()
        .to_auth(OAuthCredential::new("r", access, 0))
        .await
        .unwrap();
    assert_eq!(
        auth,
        ModelAuth {
            api_key: Some(access.to_owned()),
            headers: None,
            base_url: Some("https://api.enterprise.example".to_owned()),
        }
    );
}

#[tokio::test]
async fn github_copilot_to_auth_falls_back_to_the_enterprise_domain_then_the_individual_endpoint() {
    let mut ent_cred = OAuthCredential::new("r", "no-proxy-ep", 0);
    ent_cred.set_extra("enterpriseUrl", json!("https://company.ghe.com"));
    let enterprise = github_copilot_oauth().to_auth(ent_cred).await.unwrap();
    assert_eq!(
        enterprise.base_url.as_deref(),
        Some("https://copilot-api.company.ghe.com")
    );

    let individual = github_copilot_oauth()
        .to_auth(OAuthCredential::new("r", "no-proxy-ep", 0))
        .await
        .unwrap();
    assert_eq!(
        individual.base_url.as_deref(),
        Some("https://api.individual.githubcopilot.com")
    );
}

#[tokio::test]
#[serial]
async fn anthropic_refresh_exchanges_the_refresh_token_and_returns_a_typed_credential() {
    let _guard = mock_fetch(|_req| {
        Box::pin(async {
            Ok(json_resp(&json!({
                "access_token": "new-access",
                "refresh_token": "new-refresh",
                "expires_in": 3600
            })))
        })
    });

    let signal = AbortController::new().signal();
    let refreshed = anthropic_oauth()
        .refresh(OAuthCredential::new("old-r", "old", 0), signal)
        .await
        .unwrap();
    assert_eq!(refreshed.access, "new-access");
    assert_eq!(refreshed.refresh, "new-refresh");
    assert!(refreshed.expires > time::now_ms());
}

#[tokio::test]
#[serial]
async fn github_copilot_refresh_preserves_the_enterprise_domain() {
    let fetched_urls = Arc::new(Mutex::new(Vec::new()));
    let urls_c = fetched_urls.clone();
    let _guard = mock_fetch(move |req| {
        let u = urls_c.clone();
        Box::pin(async move {
            u.lock().unwrap().push(req.url.clone());
            if req.url.ends_with("/models") {
                return Ok(json_resp(&json!({ "data": [] })));
            }
            Ok(json_resp(&json!({
                "token": "new-token",
                "expires_at": 9_999_999_999_i64
            })))
        })
    });

    let signal = AbortController::new().signal();
    let mut cred = OAuthCredential::new("gh-token", "old", 0);
    cred.set_extra("enterpriseUrl", json!("company.ghe.com"));
    let refreshed = github_copilot_oauth().refresh(cred, signal).await.unwrap();
    assert_eq!(refreshed.access, "new-token");
    assert_eq!(refreshed.get_extra_str("enterpriseUrl"), Some("company.ghe.com"));
    assert!(fetched_urls.lock().unwrap()[0].contains("api.company.ghe.com"));
}

#[tokio::test]
async fn resolves_stored_anthropic_oauth_credentials_via_the_lazy_flow_import() {
    let credentials = Arc::new(InMemoryCredentialStore::new());
    let expires = time::now_ms() + 10 * 60_000;
    credentials
        .modify(
            "anthropic",
            Box::new(move |_| {
                Box::pin(async move {
                    Ok(Some(Credential::OAuth(OAuthCredential::new(
                        "r",
                        "oauth-access-token",
                        expires,
                    ))))
                })
            }),
            None::<AuthOperationOptions>,
        )
        .await
        .unwrap();
    let models = create_models(Some(CreateModelsOptions {
        credentials: Some(credentials),
        ..Default::default()
    }));
    models.set_provider(anthropic_provider().unwrap());

    let model = models.get_models(Some("anthropic"))[0].clone();
    let result = models
        .get_auth_provider_id(&model.provider, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.auth.api_key.as_deref(), Some("oauth-access-token"));
    assert_eq!(result.source.as_deref(), Some("OAuth"));
}

#[tokio::test]
async fn resolves_stored_github_copilot_oauth_credentials_including_per_credential_base_url() {
    // WIRE(@mcp-w2): use crate::providers::github_copilot::github_copilot_provider() once @mcp lands.
    struct CopilotTestProvider {
        auth: ProviderAuth,
        model: Model,
    }
    impl Provider for CopilotTestProvider {
        fn id(&self) -> &str {
            "github-copilot"
        }
        fn name(&self) -> &str {
            "GitHub Copilot"
        }
        fn auth(&self) -> &ProviderAuth {
            &self.auth
        }
        fn get_models(&self) -> Vec<Model> {
            vec![self.model.clone()]
        }
        fn stream(
            &self,
            _model: &Model,
            _context: &TranscriptContext,
            _options: Option<ApiStreamOptions>,
        ) -> AssistantMessageEventStream {
            AssistantMessageEventStream::new()
        }
        fn stream_simple(
            &self,
            _model: &Model,
            _context: &TranscriptContext,
            _options: Option<SimpleStreamOptions>,
        ) -> AssistantMessageEventStream {
            AssistantMessageEventStream::new()
        }
    }

    let access = "tid=abc;exp=123;proxy-ep=proxy.business.githubcopilot.com;rest";
    let credentials = Arc::new(InMemoryCredentialStore::new());
    let expires = time::now_ms() + 10 * 60_000;
    let access_owned = access.to_owned();
    credentials
        .modify(
            "github-copilot",
            Box::new(move |_| {
                Box::pin(async move {
                    Ok(Some(Credential::OAuth(OAuthCredential::new(
                        "r",
                        access_owned,
                        expires,
                    ))))
                })
            }),
            None::<AuthOperationOptions>,
        )
        .await
        .unwrap();

    let models = create_models(Some(CreateModelsOptions {
        credentials: Some(credentials),
        ..Default::default()
    }));
    models.set_provider(Arc::new(CopilotTestProvider {
        auth: ProviderAuth {
            api_key: None,
            oauth: Some(lazy_oauth(
                "GitHub Copilot",
                true,
                None::<&str>,
                Arc::new(|| Box::pin(load_github_copilot_oauth())),
            )),
        },
        model: Model {
            r#type: None,
            id: "gpt-4o".to_owned(),
            name: "GPT-4o".to_owned(),
            api: "openai-completions".to_owned(),
            provider: "github-copilot".to_owned(),
            base_url: "https://api.individual.githubcopilot.com".to_owned(),
            reasoning: false,
            thinking_level_map: None,
            prompt_cache: None,
            input: vec![ModelInput::Text],
            input_limits: None,
            cost: ModelCost::default(),
            context_window: 128000,
            max_tokens: 16384,
            headers: None,
            sampling_params: None,
            compat: None,
        },
    }));

    let model = models.get_models(Some("github-copilot"))[0].clone();
    let result = models
        .get_auth_provider_id(&model.provider, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.auth.api_key.as_deref(), Some(access));
    assert_eq!(
        result.auth.base_url.as_deref(),
        Some("https://api.business.githubcopilot.com")
    );
}
