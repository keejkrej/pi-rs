mod support;

use std::sync::Arc;

use pi_js::fetch::{Headers, Request, Response};
use pi_mcp::auth_provider::{AuthProvider, McpFetch, UnauthorizedContext};
use pi_mcp::oauth::{
    CreateMcpAuthProviderOptions, McpAuthProvider, McpClientMetadataDocuments, McpClientRegistration,
    McpOAuthAuthorizationRequiredError, McpOAuthSettings, McpOAuthStateStore, McpSignInOptions, McpSignInRedirect,
    McpSignInStart, complete_mcp_sign_in, create_mcp_auth_provider, start_mcp_sign_in,
};
use pretty_assertions::assert_eq;
use serde_json::json;
use support::auth_server::{AuthorizationServer, MemoryServerStore, StartAuthServerOptions, authorize_in_browser};
use url::Url;

const ROUTE: &str = "https://app.example/mcp/callback";
const LOOPBACK: &str = "http://127.0.0.1:1/callback";

struct DocList {
    base_url: String,
    predicate: Arc<dyn Fn(&str) -> bool + Send + Sync>,
}

impl McpClientMetadataDocuments for DocList {
    fn base_url(&self) -> &str {
        &self.base_url
    }

    fn lists_redirect_url(&self, redirect_url: &str) -> bool {
        (self.predicate)(redirect_url)
    }
}

async fn start_redirect(options: McpSignInOptions) -> McpSignInRedirect {
    match start_mcp_sign_in(options).await.expect("start_mcp_sign_in") {
        McpSignInStart::Redirect(r) => r,
        McpSignInStart::Authorized => panic!("expected browser step"),
    }
}

async fn browse(authorization: &McpSignInRedirect) -> pi_mcp::oauth::McpAuthorizationResponse {
    let redirected = authorize_in_browser(&authorization.authorization_url).await;
    let url = Url::parse(&redirected).expect("valid redirect location");
    let qp = |name: &str| url.query_pairs().find(|(k, _)| k == name).map(|(_, v)| v.into_owned());
    pi_mcp::oauth::McpAuthorizationResponse {
        code: qp("code").unwrap_or_default(),
        state: qp("state").unwrap_or_default(),
        iss: qp("iss").filter(|s| !s.is_empty()),
    }
}

async fn sign_in(options: McpSignInOptions) {
    let authorization = start_redirect(options.clone()).await;
    let response = browse(&authorization).await;
    complete_mcp_sign_in(options, authorization, response)
        .await
        .expect("complete sign-in");
}

fn default_fetch() -> McpFetch {
    Arc::new(|url, init| {
        let url = url.to_string();
        Box::pin(async move {
            let init = init.unwrap_or_default();
            let mut req = Request::new(init.method.unwrap_or_else(|| "GET".to_string()), url);
            if let Some(h) = init.headers {
                req = req.with_headers(h);
            }
            if let Some(b) = init.body {
                req = req.body(b);
            }
            if let Some(s) = init.signal {
                req = req.signal(s);
            }
            pi_js::fetch::fetch(req).await
        })
    })
}

fn unauthorized(server: &AuthorizationServer, token: &str, custom_fetch: Option<McpFetch>) -> UnauthorizedContext {
    UnauthorizedContext {
        response: Response::null_body(401, Headers::new()),
        server_url: Url::parse(&server.server_url).unwrap(),
        fetch: custom_fetch.unwrap_or_else(default_fetch),
        token: Some(token.to_string()),
    }
}

fn make_provider(
    server: &AuthorizationServer,
    store: Arc<MemoryServerStore>,
    settings: McpOAuthSettings,
    docs: Option<Arc<dyn McpClientMetadataDocuments>>,
) -> Arc<dyn McpAuthProvider + Send + Sync> {
    create_mcp_auth_provider(CreateMcpAuthProviderOptions {
        server_url: server.server_url.clone(),
        store,
        settings: Arc::new(move || Ok(settings.clone())),
        client_name: "app".to_string(),
        client_metadata_documents: docs,
        on_challenge: Arc::new(|_| {}),
        redirect_url: Some(ROUTE.to_string()),
    })
}

async fn run_unauthorized(provider: &dyn AuthProvider, ctx: UnauthorizedContext) -> pi_js::Result<()> {
    provider.on_unauthorized(ctx).expect("on_unauthorized present").await
}

#[tokio::test]
async fn refreshes_a_rejected_token() {
    let mut server = AuthorizationServer::start(Default::default()).await;
    let store = Arc::new(MemoryServerStore::new());
    sign_in(McpSignInOptions {
        server_url: server.server_url.clone(),
        store: store.clone(),
        settings: Default::default(),
        client_name: "app".to_string(),
        client_metadata_documents: None,
        redirect_url: ROUTE.to_string(),
        challenge: None,
        signal: None,
    })
    .await;

    let provider = make_provider(&server, store, Default::default(), None);
    run_unauthorized(provider.as_ref(), unauthorized(&server, "access-1", None))
        .await
        .expect("refresh succeeds");
    assert_eq!(provider.token().await.unwrap().as_deref(), Some("access-2"));
    assert_eq!(
        server.token_requests()[1].get("client_id").map(String::as_str),
        Some("client-1")
    );
    server.close().await;
}

#[tokio::test]
async fn leaves_pending_sign_in_usable_when_refresh_fails() {
    let mut server = AuthorizationServer::start(Default::default()).await;
    let store = Arc::new(MemoryServerStore::new());
    let base_opts = McpSignInOptions {
        server_url: server.server_url.clone(),
        store: store.clone(),
        settings: Default::default(),
        client_name: "app".to_string(),
        client_metadata_documents: None,
        redirect_url: ROUTE.to_string(),
        challenge: None,
        signal: None,
    };
    sign_in(base_opts.clone()).await;

    let mut step_up = base_opts;
    step_up.challenge = Some(pi_mcp::oauth::OAuthChallenge {
        resource_metadata_url: None,
        scope: Some("more".to_string()),
        error: Some("insufficient_scope".to_string()),
        error_description: None,
    });
    let authorization = start_redirect(step_up.clone()).await;
    let pending = store.load().await.unwrap().unwrap();

    server.revoke_refresh_tokens();
    let provider = make_provider(&server, store.clone(), Default::default(), None);
    let err = run_unauthorized(provider.as_ref(), unauthorized(&server, "access-1", None))
        .await
        .unwrap_err();
    assert!(err.downcast_ref::<McpOAuthAuthorizationRequiredError>().is_some());

    let state = store.load().await.unwrap().unwrap();
    assert_eq!(state.oauth_state, pending.oauth_state);
    assert_eq!(state.code_verifier, pending.code_verifier);
    assert_eq!(server.registrations().len(), 1);

    let response = browse(&authorization).await;
    complete_mcp_sign_in(step_up, authorization, response)
        .await
        .expect("complete step-up");
    assert_eq!(
        store.load().await.unwrap().unwrap().tokens.unwrap().access_token,
        "access-2"
    );
    server.close().await;
}

#[tokio::test]
async fn does_not_register_a_client_to_refresh() {
    let mut server = AuthorizationServer::start(StartAuthServerOptions {
        cimd: true,
        iss_parameter: true,
    })
    .await;
    let store = Arc::new(MemoryServerStore::new());
    let docs: Arc<dyn McpClientMetadataDocuments> = Arc::new(DocList {
        base_url: "https://app.example/oauth".to_string(),
        predicate: Arc::new(|_| true),
    });
    sign_in(McpSignInOptions {
        server_url: server.server_url.clone(),
        store: store.clone(),
        settings: Default::default(),
        client_name: "app".to_string(),
        client_metadata_documents: Some(docs),
        redirect_url: LOOPBACK.to_string(),
        challenge: None,
        signal: None,
    })
    .await;
    assert!(store.load().await.unwrap().unwrap().client_information.is_none());

    let provider = make_provider(&server, store, Default::default(), None);
    let err = run_unauthorized(provider.as_ref(), unauthorized(&server, "access-1", None))
        .await
        .unwrap_err();
    assert!(err.downcast_ref::<McpOAuthAuthorizationRequiredError>().is_some());
    assert!(server.registrations().is_empty());
    assert_eq!(server.authorizations().len(), 1);
    server.close().await;
}

#[tokio::test]
async fn refreshes_tokens_of_document_listing_redirect_uri_for_auto_and_cimd() {
    for registration in [McpClientRegistration::Auto, McpClientRegistration::Cimd] {
        let mut server = AuthorizationServer::start(StartAuthServerOptions {
            cimd: true,
            iss_parameter: true,
        })
        .await;
        let store = Arc::new(MemoryServerStore::new());
        let docs: Arc<dyn McpClientMetadataDocuments> = Arc::new(DocList {
            base_url: "https://app.example/oauth".to_string(),
            predicate: Arc::new(|u| u == ROUTE),
        });
        let settings = McpOAuthSettings {
            client_registration: Some(registration),
            ..Default::default()
        };
        sign_in(McpSignInOptions {
            server_url: server.server_url.clone(),
            store: store.clone(),
            settings: settings.clone(),
            client_name: "app".to_string(),
            client_metadata_documents: Some(docs.clone()),
            redirect_url: ROUTE.to_string(),
            challenge: None,
            signal: None,
        })
        .await;
        assert!(store.load().await.unwrap().unwrap().client_information.is_none());

        let provider = make_provider(&server, store, settings, Some(docs));
        run_unauthorized(provider.as_ref(), unauthorized(&server, "access-1", None))
            .await
            .expect("refresh succeeds");
        assert_eq!(provider.token().await.unwrap().as_deref(), Some("access-2"));
        assert_eq!(
            server.token_requests()[1].get("client_id").map(String::as_str),
            Some("https://app.example/oauth/client.json")
        );
        assert_eq!(server.authorizations().len(), 1);
        server.close().await;
    }
}

async fn setup_pending_sign_in() -> (
    AuthorizationServer,
    Arc<MemoryServerStore>,
    pi_mcp::oauth::McpOAuthState,
    Arc<dyn McpAuthProvider + Send + Sync>,
) {
    let server = AuthorizationServer::start(Default::default()).await;
    let store = Arc::new(MemoryServerStore::new());
    let base_opts = McpSignInOptions {
        server_url: server.server_url.clone(),
        store: store.clone(),
        settings: Default::default(),
        client_name: "app".to_string(),
        client_metadata_documents: None,
        redirect_url: ROUTE.to_string(),
        challenge: None,
        signal: None,
    };
    sign_in(base_opts.clone()).await;
    let mut step_up = base_opts;
    step_up.challenge = Some(pi_mcp::oauth::OAuthChallenge {
        resource_metadata_url: None,
        scope: Some("more".to_string()),
        error: Some("insufficient_scope".to_string()),
        error_description: None,
    });
    let _authorization = start_redirect(step_up).await;
    let pending = store.load().await.unwrap().unwrap();
    let provider = make_provider(&server, store.clone(), Default::default(), None);
    (server, store, pending, provider)
}

#[tokio::test]
async fn drops_rejected_client_and_tokens_but_keeps_pending_sign_in() {
    let (mut server, store, pending, provider) = setup_pending_sign_in().await;
    server.fail_next_refresh("invalid_client");
    let err = run_unauthorized(provider.as_ref(), unauthorized(&server, "access-1", None))
        .await
        .unwrap_err();
    assert!(err.downcast_ref::<McpOAuthAuthorizationRequiredError>().is_some());

    let state = store.load().await.unwrap().unwrap();
    assert_eq!(state.oauth_state, pending.oauth_state);
    assert_eq!(state.code_verifier, pending.code_verifier);
    assert!(state.client_information.is_none());
    assert!(state.tokens.is_none());
    assert_eq!(server.registrations().len(), 1);
    server.close().await;
}

#[tokio::test]
async fn keeps_client_stored_while_rejected_refresh_ran() {
    let (mut server, store, _pending, provider) = setup_pending_sign_in().await;
    let store_for_hook = store.clone();
    server.fail_next_refresh_with_hook("invalid_client", move || {
        let store = store_for_hook.clone();
        async move {
            if let Some(mut state) = store.load().await.unwrap() {
                state.client_information = Some(
                    serde_json::from_value(json!({ "client_id": "client-new", "redirect_uris": [ROUTE] })).unwrap(),
                );
                store.save(state).await.unwrap();
            }
        }
    });

    run_unauthorized(provider.as_ref(), unauthorized(&server, "access-1", None))
        .await
        .expect("retries with new client");
    assert_eq!(
        store
            .load()
            .await
            .unwrap()
            .unwrap()
            .client_information
            .unwrap()
            .client_id,
        "client-new"
    );
    assert_eq!(
        server
            .token_requests()
            .last()
            .unwrap()
            .get("client_id")
            .map(String::as_str),
        Some("client-new")
    );
    assert_eq!(provider.token().await.unwrap().as_deref(), Some("access-2"));
    server.close().await;
}

#[tokio::test]
async fn keeps_tokens_and_pending_sign_in_after_server_and_network_errors() {
    let (mut server, store, pending, provider) = setup_pending_sign_in().await;
    server.fail_next_refresh("server_error");
    let err = run_unauthorized(provider.as_ref(), unauthorized(&server, "access-1", None))
        .await
        .unwrap_err();
    assert!(err.downcast_ref::<McpOAuthAuthorizationRequiredError>().is_some());
    let state = store.load().await.unwrap().unwrap();
    assert_eq!(state.oauth_state, pending.oauth_state);
    assert_eq!(state.code_verifier, pending.code_verifier);
    assert_eq!(state.tokens.unwrap().access_token, "access-1");

    let base_fetch = default_fetch();
    let offline: McpFetch = Arc::new(move |input, init| {
        if input.ends_with("/token") {
            return Box::pin(async { Err(pi_js::Error::js("TypeError", "fetch failed")) });
        }
        base_fetch(input, init)
    });
    let net_err = run_unauthorized(provider.as_ref(), unauthorized(&server, "access-1", Some(offline)))
        .await
        .unwrap_err();
    assert!(net_err.downcast_ref::<McpOAuthAuthorizationRequiredError>().is_some());
    let state = store.load().await.unwrap().unwrap();
    assert_eq!(state.oauth_state, pending.oauth_state);
    assert_eq!(state.code_verifier, pending.code_verifier);
    assert_eq!(state.tokens.unwrap().access_token, "access-1");
    server.close().await;
}
