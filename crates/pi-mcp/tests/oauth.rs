mod support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use indexmap::IndexMap;
use pi_js::fetch::{Headers, Request, Response};
use pi_js::{BoxFuture, Error};
use pi_mcp::auth_provider::McpFetchInit;
use pi_mcp::oauth::provider::McpOAuthProviderClientMetadata;
use pi_mcp::oauth::{
    AuthorizationServerMetadata, McpOAuthAuthorizationRequiredError, McpOAuthProvider, McpOAuthProviderOptions,
    McpOAuthStateStore, MemoryOAuthStateStore, OAuthCallbackPage, OAuthCallbackServer, OAuthCallbackServerOptions,
    OAuthClientInformationMixed, OAuthClientMetadata, OAuthClientProvider, OAuthDiscoveryState, OAuthFlowOptions,
    OAuthFlowResult, OAuthInsecureEndpointError, OAuthIssuerMismatchError, OAuthTokens, adapt_oauth_provider,
    authorize_mcp, discover_authorization_server_metadata,
};
use pi_mcp::{
    McpClient, McpClientOptions, McpFetch, StreamableHttpTransport, StreamableHttpTransportOptions, Tool,
    UnauthorizedContext,
};
use pretty_assertions::assert_eq;
use serde_json::{from_value, json};
use support::helpers::TestServers;
use url::Url;

#[derive(Default)]
struct TestOAuthState {
    client: Option<OAuthClientInformationMixed>,
    token_set: Option<OAuthTokens>,
    verifier: Option<String>,
    discovery: Option<OAuthDiscoveryState>,
    authorization_url: Option<Url>,
}

struct TestOAuthProvider {
    redirect_url: String,
    client_metadata: OAuthClientMetadata,
    state: Mutex<TestOAuthState>,
}

impl TestOAuthProvider {
    fn new(redirect_url: impl Into<String>) -> Self {
        let redirect_url = redirect_url.into();
        let client_metadata: OAuthClientMetadata = from_value(json!({
            "redirect_uris": [redirect_url],
            "client_name": "pi-mcp-test",
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "none"
        }))
        .expect("valid OAuthClientMetadata");
        Self {
            redirect_url,
            client_metadata,
            state: Mutex::new(TestOAuthState::default()),
        }
    }

    fn authorization_url(&self) -> Option<Url> {
        self.state.lock().unwrap().authorization_url.clone()
    }

    fn token_set(&self) -> Option<OAuthTokens> {
        self.state.lock().unwrap().token_set.clone()
    }

    fn set_token_set(&self, tokens: Option<OAuthTokens>) {
        self.state.lock().unwrap().token_set = tokens;
    }

    fn set_client(&self, client: Option<OAuthClientInformationMixed>) {
        self.state.lock().unwrap().client = client;
    }

    fn set_verifier(&self, verifier: Option<String>) {
        self.state.lock().unwrap().verifier = verifier;
    }

    fn set_discovery(&self, discovery: Option<OAuthDiscoveryState>) {
        self.state.lock().unwrap().discovery = discovery;
    }
}

#[async_trait::async_trait]
impl OAuthClientProvider for TestOAuthProvider {
    fn redirect_url(&self) -> String {
        self.redirect_url.clone()
    }

    fn client_metadata(&self) -> &OAuthClientMetadata {
        &self.client_metadata
    }

    async fn state(&self) -> pi_js::Result<Option<String>> {
        Ok(Some("expected-state".to_string()))
    }

    async fn client_information(&self) -> pi_js::Result<Option<OAuthClientInformationMixed>> {
        Ok(self.state.lock().unwrap().client.clone())
    }

    async fn save_client_information(&self, information: OAuthClientInformationMixed) -> pi_js::Result<()> {
        self.state.lock().unwrap().client = Some(information);
        Ok(())
    }

    async fn tokens(&self) -> pi_js::Result<Option<OAuthTokens>> {
        Ok(self.state.lock().unwrap().token_set.clone())
    }

    async fn save_tokens(&self, tokens: OAuthTokens) -> pi_js::Result<()> {
        self.state.lock().unwrap().token_set = Some(tokens);
        Ok(())
    }

    async fn redirect_to_authorization(&self, url: Url) -> pi_js::Result<()> {
        self.state.lock().unwrap().authorization_url = Some(url);
        Ok(())
    }

    async fn save_code_verifier(&self, verifier: &str) -> pi_js::Result<()> {
        self.state.lock().unwrap().verifier = Some(verifier.to_string());
        Ok(())
    }

    async fn code_verifier(&self) -> pi_js::Result<String> {
        self.state
            .lock()
            .unwrap()
            .verifier
            .clone()
            .ok_or_else(|| Error::msg("Missing code verifier"))
    }

    async fn invalidate_credentials(&self, kind: pi_mcp::oauth::flow::InvalidateCredentialsKind) -> pi_js::Result<()> {
        use pi_mcp::oauth::flow::InvalidateCredentialsKind::*;
        let mut guard = self.state.lock().unwrap();
        match kind {
            All => {
                guard.client = None;
                guard.token_set = None;
                guard.verifier = None;
                guard.discovery = None;
            }
            Client => guard.client = None,
            Tokens => guard.token_set = None,
            Verifier => guard.verifier = None,
            Discovery => guard.discovery = None,
        }
        Ok(())
    }

    async fn save_discovery_state(&self, state: OAuthDiscoveryState) -> pi_js::Result<()> {
        self.state.lock().unwrap().discovery = Some(state);
        Ok(())
    }

    async fn discovery_state(&self) -> pi_js::Result<Option<OAuthDiscoveryState>> {
        Ok(self.state.lock().unwrap().discovery.clone())
    }
}

fn default_flow_options(server_url: impl Into<String>) -> OAuthFlowOptions {
    OAuthFlowOptions {
        server_url: server_url.into(),
        authorization_code: None,
        iss: None,
        scope: None,
        resource_metadata_url: None,
        authorization_server_metadata_url: None,
        fetch: None,
        skip_issuer_validation: None,
        skip_refresh: None,
    }
}

fn default_client(name: &str) -> McpClient {
    McpClient::new(McpClientOptions {
        name: name.to_string(),
        version: "1.0.0".to_string(),
        title: None,
        capabilities: None,
        protocol_version: None,
        request_timeout_ms: None,
        roots: None,
    })
}

fn default_fetch() -> McpFetch {
    Arc::new(|url, init: Option<McpFetchInit>| {
        let url = url.to_string();
        Box::pin(async move {
            let method = init
                .as_ref()
                .and_then(|i| i.method.clone())
                .unwrap_or_else(|| "GET".to_string());
            let mut req = Request::new(method, url);
            if let Some(init) = init {
                if let Some(h) = init.headers {
                    req = req.with_headers(h);
                }
                if let Some(b) = init.body {
                    req = req.body(b);
                }
                if let Some(s) = init.signal {
                    req = req.signal(s);
                }
            }
            pi_js::fetch::fetch(req).await
        })
    })
}

fn parse_form(body: &str) -> IndexMap<String, String> {
    url::form_urlencoded::parse(body.as_bytes()).into_owned().collect()
}

fn noop_redirect() -> Arc<dyn Fn(Url) -> BoxFuture<pi_js::Result<()>> + Send + Sync> {
    Arc::new(|_| Box::pin(async { Ok(()) }))
}

#[tokio::test]
async fn discovers_registers_authorizes_with_pkce_and_refreshes_on_401() {
    let mut servers = TestServers::new();
    let expected_challenge = Arc::new(Mutex::new(Option::<String>::None));
    let refreshes = Arc::new(AtomicUsize::new(0));
    let challenge_cb = expected_challenge.clone();
    let refreshes_cb = refreshes.clone();

    let origin = servers
        .listen(move |req, server_origin| {
            let expected_challenge = challenge_cb.clone();
            let refreshes = refreshes_cb.clone();
            Box::pin(async move {
                let url = Url::parse(&format!("{server_origin}{}", req.uri)).unwrap();
                let path = url.path();
                if path == "/.well-known/oauth-protected-resource/mcp" {
                    return support::helpers::TestResponse::json(
                        200,
                        &json!({
                            "resource": format!("{server_origin}/mcp"),
                            "authorization_servers": [server_origin],
                            "scopes_supported": ["org:read"]
                        }),
                    );
                }
                if path == "/.well-known/oauth-authorization-server" {
                    return support::helpers::TestResponse::json(
                        200,
                        &json!({
                            "issuer": server_origin,
                            "authorization_endpoint": format!("{server_origin}/authorize"),
                            "token_endpoint": format!("{server_origin}/token"),
                            "registration_endpoint": format!("{server_origin}/register"),
                            "response_types_supported": ["code"],
                            "grant_types_supported": ["authorization_code", "refresh_token"],
                            "token_endpoint_auth_methods_supported": ["none"],
                            "code_challenge_methods_supported": ["S256"]
                        }),
                    );
                }
                if path == "/register" {
                    let mut metadata = req.body_json().unwrap();
                    if let Some(obj) = metadata.as_object_mut() {
                        obj.insert("client_id".to_string(), json!("test-client"));
                        obj.insert("client_secret".to_string(), json!(""));
                    }
                    // Empty and null optional fields count as absent (#10266).
                    return support::helpers::TestResponse::json(201, &metadata);
                }
                if path == "/authorize" {
                    let params: IndexMap<String, String> = url.query_pairs().into_owned().collect();
                    *expected_challenge.lock().unwrap() = params.get("code_challenge").cloned();
                    let mut redirect = Url::parse(params.get("redirect_uri").unwrap()).unwrap();
                    {
                        let mut q = redirect.query_pairs_mut();
                        q.append_pair("code", "test-code");
                        q.append_pair(
                            "state",
                            params.get("state").map(String::as_str).unwrap_or(""),
                        );
                    }
                    return support::helpers::TestResponse::redirect(302, redirect.as_str());
                }
                if path == "/token" {
                    let params = parse_form(&req.body_text());
                    if params.get("grant_type").map(String::as_str) == Some("refresh_token") {
                        refreshes.fetch_add(1, Ordering::SeqCst);
                        return support::helpers::TestResponse::json(
                            200,
                            &json!({
                                "access_token": "refreshed-token",
                                "token_type": "Bearer",
                                "refresh_token": "",
                                "expires_in": null
                            }),
                        );
                    }
                    let verifier = params
                        .get("code_verifier")
                        .map(String::as_str)
                        .unwrap_or("");
                    let digest = pi_js::crypto::sha256(verifier.as_bytes());
                    let challenge = pi_js::b64::encode_url(&digest);
                    let expected = expected_challenge.lock().unwrap().clone();
                    if params.get("code").map(String::as_str) != Some("test-code")
                        || Some(challenge) != expected
                    {
                        return support::helpers::TestResponse::json(
                            400,
                            &json!({ "error": "invalid_grant" }),
                        );
                    }
                    return support::helpers::TestResponse::json(
                        200,
                        &json!({
                            "access_token": "first-token",
                            "refresh_token": "refresh-token",
                            "token_type": "Bearer",
                            "scope": ""
                        }),
                    );
                }
                if path != "/mcp" {
                    return support::helpers::TestResponse::empty(404);
                }
                if req.method == "GET" {
                    return support::helpers::TestResponse::empty(405);
                }
                if req.method == "DELETE" {
                    return support::helpers::TestResponse::empty(200);
                }
                let token = req.header("authorization").map(str::to_string);
                if token.as_deref() != Some("Bearer first-token")
                    && token.as_deref() != Some("Bearer refreshed-token")
                {
                    return support::helpers::TestResponse::with_headers(
                        401,
                        &[(
                            "www-authenticate",
                            &format!(
                                "Bearer resource_metadata=\"{server_origin}/.well-known/oauth-protected-resource/mcp\", scope=\"\""
                            ),
                        )],
                        "Unauthorized",
                    );
                }
                let message = req.body_json().unwrap();
                if message.get("id").is_none() {
                    return support::helpers::TestResponse::empty(202);
                }
                let method = message.get("method").and_then(|v| v.as_str()).unwrap_or("");
                let result = if method == "initialize" {
                    json!({
                        "protocolVersion": "2025-06-18",
                        "capabilities": { "tools": {} },
                        "serverInfo": { "name": "oauth-test", "version": "1.0.0" }
                    })
                } else {
                    json!({ "tools": [{ "name": "issues", "inputSchema": { "type": "object" } }] })
                };
                support::helpers::TestResponse::json(
                    200,
                    &json!({ "jsonrpc": "2.0", "id": message.get("id"), "result": result }),
                )
            })
        })
        .await;

    let callback = OAuthCallbackServer::listen(None).await.unwrap();
    let provider = Arc::new(TestOAuthProvider::new(callback.redirect_url()));
    let mut stale_headers = IndexMap::new();
    stale_headers.insert(
        "Authorization".to_string(),
        "Bearer caller-supplied-stale-token".to_string(),
    );

    let first_client = default_client("oauth-test");
    let err = first_client
        .connect(Arc::new(StreamableHttpTransport::new(StreamableHttpTransportOptions {
            url: format!("{origin}/mcp"),
            headers: Some(stale_headers.clone()),
            fetch: None,
            open_get_stream: Some(false),
            max_message_bytes: None,
            auth_provider: Some(adapt_oauth_provider(provider.clone())),
            reconnect: None,
        })))
        .await
        .unwrap_err();
    assert!(
        err.downcast_ref::<McpOAuthAuthorizationRequiredError>().is_some(),
        "expected McpOAuthAuthorizationRequiredError, got {err}"
    );
    let auth_url = provider.authorization_url().expect("authorization_url set");
    let auth_params: IndexMap<String, String> = auth_url.query_pairs().into_owned().collect();
    assert_eq!(auth_params.get("scope").map(String::as_str), Some("org:read"));
    assert_eq!(
        auth_params.get("resource").map(String::as_str),
        Some(format!("{origin}/mcp").as_str())
    );

    let (cb_res, _) = tokio::join!(callback.wait_for_callback("expected-state"), async {
        tokio::task::yield_now().await;
        let manual = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let auth_resp = manual.get(auth_url.as_str()).send().await.unwrap();
        let location = auth_resp
            .headers()
            .get("location")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        let _ = reqwest::Client::new().get(&location).send().await.unwrap();
    });
    let cb = cb_res.expect("callback completes");

    let mut flow_opts = default_flow_options(format!("{origin}/mcp"));
    flow_opts.authorization_code = Some(cb.code);
    flow_opts.fetch = Some(default_fetch());
    assert_eq!(
        authorize_mcp(provider.as_ref(), flow_opts).await.unwrap(),
        OAuthFlowResult::Authorized
    );

    let client = default_client("oauth-test");
    client
        .connect(Arc::new(StreamableHttpTransport::new(StreamableHttpTransportOptions {
            url: format!("{origin}/mcp"),
            headers: Some(stale_headers.clone()),
            fetch: None,
            open_get_stream: Some(false),
            max_message_bytes: None,
            auth_provider: Some(adapt_oauth_provider(provider.clone())),
            reconnect: None,
        })))
        .await
        .unwrap();
    let expected_tools: Vec<Tool> =
        from_value(json!([{ "name": "issues", "inputSchema": { "type": "object" } }])).unwrap();
    assert_eq!(client.list_tools(None).await.unwrap(), expected_tools);
    client.close().await.unwrap();

    let mut stale_tokens = provider.token_set().unwrap();
    stale_tokens.access_token = "stale-token".to_string();
    provider.set_token_set(Some(stale_tokens));

    let refreshed_client = default_client("oauth-test");
    refreshed_client
        .connect(Arc::new(StreamableHttpTransport::new(StreamableHttpTransportOptions {
            url: format!("{origin}/mcp"),
            headers: Some(stale_headers),
            fetch: None,
            open_get_stream: Some(false),
            max_message_bytes: None,
            auth_provider: Some(adapt_oauth_provider(provider.clone())),
            reconnect: None,
        })))
        .await
        .unwrap();
    // Neither token response names a scope, so the grant has the requested scope.
    assert_eq!(
        provider.token_set(),
        Some(OAuthTokens {
            access_token: "refreshed-token".to_string(),
            refresh_token: Some("refresh-token".to_string()),
            token_type: "Bearer".to_string(),
            scope: Some("org:read".to_string()),
            expires_in: None,
            id_token: None,
        })
    );
    assert_eq!(refreshes.load(Ordering::SeqCst), 1);
    refreshed_client.close().await.unwrap();
    callback.close().await.unwrap();
    servers.close().await;
}

#[tokio::test]
async fn shares_one_refresh_between_concurrent_401s_when_refresh_tokens_rotate() {
    let mut servers = TestServers::new();
    let grants = Arc::new(Mutex::new(Vec::<String>::new()));
    let grants_cb = grants.clone();

    let origin = servers
        .listen(move |req, server_origin| {
            let grants = grants_cb.clone();
            Box::pin(async move {
                let url = Url::parse(&format!("{server_origin}{}", req.uri)).unwrap();
                match url.path() {
                    "/.well-known/oauth-protected-resource/mcp" => {
                        // Invalid resource metadata falls back to the server origin instead of failing discovery.
                        support::helpers::TestResponse::json(
                            200,
                            &json!({
                                "resource": format!("{server_origin}/mcp"),
                                "authorization_servers": ["not a url"]
                            }),
                        )
                    }
                    "/.well-known/oauth-authorization-server" => {
                        // Issuer without the trailing slash that URL parsing adds to the fallback server URL.
                        support::helpers::TestResponse::json(
                            200,
                            &json!({
                                "issuer": server_origin,
                                "authorization_endpoint": format!("{server_origin}/authorize"),
                                "token_endpoint": format!("{server_origin}/token"),
                                "response_types_supported": ["code"]
                            }),
                        )
                    }
                    "/token" => {
                        let params = parse_form(&req.body_text());
                        let refresh_token = params.get("refresh_token").cloned().unwrap_or_default();
                        grants.lock().unwrap().push(refresh_token.clone());
                        if refresh_token != "r1" {
                            return support::helpers::TestResponse::json(400, &json!({ "error": "invalid_grant" }));
                        }
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        support::helpers::TestResponse::json(
                            200,
                            &json!({
                                "access_token": "a2",
                                "refresh_token": "r2",
                                "token_type": "Bearer",
                                "expires_in": 3600
                            }),
                        )
                    }
                    _ => support::helpers::TestResponse::empty(404),
                }
            })
        })
        .await;

    let store = Arc::new(MemoryOAuthStateStore::new());
    let provider = Arc::new(McpOAuthProvider::new(McpOAuthProviderOptions {
        server_url: format!("{origin}/mcp"),
        redirect_url: "http://127.0.0.1/callback".to_string(),
        client_metadata: McpOAuthProviderClientMetadata {
            client_name: Some("test".to_string()),
            ..Default::default()
        },
        client_id: Some("client".to_string()),
        client_secret: None,
        store: Some(store.clone()),
        on_redirect: noop_redirect(),
    }));
    provider
        .save_tokens(OAuthTokens {
            access_token: "a1".to_string(),
            refresh_token: Some("r1".to_string()),
            token_type: "Bearer".to_string(),
            expires_in: None,
            scope: None,
            id_token: None,
        })
        .await
        .unwrap();

    let auth = adapt_oauth_provider(provider);
    let make_ctx = || UnauthorizedContext {
        response: Response::null_body(401, Headers::from_pairs([("www-authenticate", "Bearer")])),
        server_url: Url::parse(&format!("{origin}/mcp")).unwrap(),
        fetch: default_fetch(),
        token: Some("a1".to_string()),
    };

    let (r1, r2) = tokio::join!(
        auth.on_unauthorized(make_ctx()).unwrap(),
        auth.on_unauthorized(make_ctx()).unwrap()
    );
    r1.unwrap();
    r2.unwrap();

    // A late 401 for a request that still carried the old token must not refresh again.
    auth.on_unauthorized(make_ctx()).unwrap().await.unwrap();

    assert_eq!(*grants.lock().unwrap(), vec!["r1".to_string()]);
    assert_eq!(auth.token().await.unwrap().as_deref(), Some("a2"));
    let state = McpOAuthStateStore::load(store.as_ref()).await.unwrap().unwrap();
    assert_eq!(
        state.tokens.as_ref().and_then(|t| t.refresh_token.as_deref()),
        Some("r2")
    );
    assert!(state.tokens_expire_at.unwrap_or(0) > pi_js::time::now_ms() + 3_500_000);
    servers.close().await;
}

#[tokio::test]
async fn asks_for_authorization_instead_of_refreshing_when_the_server_needs_more_scope() {
    let mut servers = TestServers::new();
    let provider = Arc::new(TestOAuthProvider::new("http://127.0.0.1/callback"));
    provider.set_client(Some(from_value(json!({ "client_id": "client" })).unwrap()));
    provider.set_token_set(Some(
        from_value(json!({
            "access_token": "a1",
            "refresh_token": "r1",
            "token_type": "Bearer",
            "scope": "repo read:org"
        }))
        .unwrap(),
    ));

    let origin = servers
        .listen(|req, server_origin| {
            Box::pin(async move {
                let url = Url::parse(&format!("{server_origin}{}", req.uri)).unwrap();
                if url.path() == "/.well-known/oauth-authorization-server" {
                    return support::helpers::TestResponse::json(
                        200,
                        &json!({
                            "issuer": server_origin,
                            "authorization_endpoint": format!("{server_origin}/authorize"),
                            "token_endpoint": format!("{server_origin}/token"),
                            "response_types_supported": ["code"]
                        }),
                    );
                }
                support::helpers::TestResponse::empty(if url.path() == "/token" { 500 } else { 404 })
            })
        })
        .await;

    let auth = adapt_oauth_provider(provider.clone());
    let ctx = UnauthorizedContext {
        response: Response::null_body(
            403,
            Headers::from_pairs([(
                "www-authenticate",
                "Bearer error=\"insufficient_scope\", scope=\"repo admin\"",
            )]),
        ),
        server_url: Url::parse(&format!("{origin}/mcp")).unwrap(),
        fetch: default_fetch(),
        token: Some("a1".to_string()),
    };
    let err = auth.on_unauthorized(ctx).unwrap().await.unwrap_err();
    assert!(
        err.downcast_ref::<McpOAuthAuthorizationRequiredError>().is_some(),
        "expected McpOAuthAuthorizationRequiredError, got {err}"
    );
    // The challenge may list only the missing scopes; the new grant keeps the old ones too.
    let auth_url = provider.authorization_url().unwrap();
    let params: IndexMap<String, String> = auth_url.query_pairs().into_owned().collect();
    assert_eq!(params.get("scope").map(String::as_str), Some("repo read:org admin"));
    // The working grant is kept until the user authorizes the new scope.
    assert_eq!(
        provider.token_set().as_ref().map(|t| t.access_token.as_str()),
        Some("a1")
    );
    servers.close().await;
}

#[tokio::test]
async fn binds_persisted_credentials_to_the_exact_mcp_server_url() {
    let store = Arc::new(MemoryOAuthStateStore::new());
    let first = McpOAuthProvider::new(McpOAuthProviderOptions {
        server_url: "https://one.example/mcp".to_string(),
        redirect_url: "http://127.0.0.1/callback".to_string(),
        client_metadata: McpOAuthProviderClientMetadata {
            client_name: Some("test".to_string()),
            ..Default::default()
        },
        client_id: None,
        client_secret: None,
        store: Some(store.clone()),
        on_redirect: noop_redirect(),
    });
    first
        .save_tokens(OAuthTokens {
            access_token: "secret".to_string(),
            token_type: "Bearer".to_string(),
            expires_in: None,
            scope: None,
            refresh_token: None,
            id_token: None,
        })
        .await
        .unwrap();
    assert_eq!(
        first.tokens().await.unwrap().map(|t| t.access_token),
        Some("secret".to_string())
    );

    let second = McpOAuthProvider::new(McpOAuthProviderOptions {
        server_url: "https://two.example/mcp".to_string(),
        redirect_url: "http://127.0.0.1/callback".to_string(),
        client_metadata: McpOAuthProviderClientMetadata {
            client_name: Some("test".to_string()),
            ..Default::default()
        },
        client_id: None,
        client_secret: None,
        store: Some(store),
        on_redirect: noop_redirect(),
    });
    assert!(second.tokens().await.unwrap().is_none());
}

#[tokio::test]
async fn rejects_authorization_metadata_whose_issuer_does_not_match_discovery() {
    let mut servers = TestServers::new();
    let origin = servers
        .listen(|req, server_origin| {
            Box::pin(async move {
                let url = Url::parse(&format!("{server_origin}{}", req.uri)).unwrap();
                if url.path() == "/.well-known/oauth-authorization-server" {
                    return support::helpers::TestResponse::json(
                        200,
                        &json!({
                            "issuer": "https://attacker.example",
                            "authorization_endpoint": format!("{server_origin}/authorize"),
                            "token_endpoint": format!("{server_origin}/token"),
                            "response_types_supported": ["code"]
                        }),
                    );
                }
                support::helpers::TestResponse::empty(404)
            })
        })
        .await;

    let err = discover_authorization_server_metadata(&origin, None).await.unwrap_err();
    assert!(
        err.downcast_ref::<OAuthIssuerMismatchError>().is_some(),
        "expected OAuthIssuerMismatchError, got {err}"
    );
    servers.close().await;
}

// #10172
#[tokio::test]
async fn uses_a_configured_authorization_server_metadata_document_as_is() {
    let mut servers = TestServers::new();
    let origin = servers
        .listen(|req, server_origin| {
            Box::pin(async move {
                let url = Url::parse(&format!("{server_origin}{}", req.uri)).unwrap();
                if url.path() == "/.well-known/oauth-protected-resource/mcp" {
                    // Names the MCP server itself, which serves no authorization server metadata.
                    return support::helpers::TestResponse::json(
                        200,
                        &json!({
                            "resource": format!("{server_origin}/mcp"),
                            "authorization_servers": [server_origin]
                        }),
                    );
                }
                if url.path() == "/idp/metadata.json" {
                    return support::helpers::TestResponse::json(
                        200,
                        &json!({
                            // Not derivable from the document URL; a configured document is not checked.
                            "issuer": "https://idp.example",
                            "authorization_endpoint": format!("{server_origin}/idp/authorize"),
                            "token_endpoint": format!("{server_origin}/idp/token"),
                            "response_types_supported": ["code"]
                        }),
                    );
                }
                support::helpers::TestResponse::empty(404)
            })
        })
        .await;

    let provider = TestOAuthProvider::new("http://127.0.0.1/callback");
    provider.set_client(Some(from_value(json!({ "client_id": "client" })).unwrap()));
    let mut options = default_flow_options(format!("{origin}/mcp"));
    options.authorization_server_metadata_url = Some(Url::parse(&format!("{origin}/idp/metadata.json")).unwrap());

    assert_eq!(
        authorize_mcp(&provider, options.clone()).await.unwrap(),
        OAuthFlowResult::Redirect
    );
    let authorization_url = provider.authorization_url().unwrap();
    assert_eq!(
        format!(
            "{}{}",
            authorization_url.origin().ascii_serialization(),
            authorization_url.path()
        ),
        format!("{origin}/idp/authorize")
    );
    let params: IndexMap<String, String> = authorization_url.query_pairs().into_owned().collect();
    assert_eq!(
        params.get("resource").map(String::as_str),
        Some(format!("{origin}/mcp").as_str())
    );

    let mut insecure = options;
    insecure.authorization_server_metadata_url = Some(Url::parse("http://idp.example/metadata.json").unwrap());
    let err = authorize_mcp(&provider, insecure).await.unwrap_err();
    assert!(
        err.downcast_ref::<OAuthInsecureEndpointError>().is_some(),
        "expected OAuthInsecureEndpointError, got {err}"
    );
    servers.close().await;
}

#[tokio::test]
async fn exchanges_a_code_only_when_its_iss_parameter_names_the_authorization_server() {
    let mut servers = TestServers::new();
    let codes = Arc::new(Mutex::new(Vec::<String>::new()));
    let codes_cb = codes.clone();
    let origin = servers
        .listen(move |req, _| {
            let codes = codes_cb.clone();
            Box::pin(async move {
                let form = parse_form(&req.body_text());
                codes
                    .lock()
                    .unwrap()
                    .push(form.get("code").cloned().unwrap_or_default());
                support::helpers::TestResponse::json(200, &json!({ "access_token": "token", "token_type": "Bearer" }))
            })
        })
        .await;

    let exchange = |code: &str, iss: Option<&str>, iss_parameter_supported: bool| {
        let origin = origin.clone();
        let code = code.to_string();
        let iss = iss.map(str::to_string);
        async move {
            let provider = TestOAuthProvider::new("http://127.0.0.1/callback");
            provider.set_client(Some(from_value(json!({ "client_id": "client" })).unwrap()));
            provider.set_verifier(Some("verifier".to_string()));
            let metadata: AuthorizationServerMetadata = from_value(json!({
                "issuer": origin,
                "authorization_endpoint": format!("{origin}/authorize"),
                "token_endpoint": format!("{origin}/token"),
                "response_types_supported": ["code"],
                "authorization_response_iss_parameter_supported": iss_parameter_supported
            }))
            .unwrap();
            provider.set_discovery(Some(OAuthDiscoveryState {
                authorization_server_url: origin.clone(),
                authorization_server_metadata: Some(metadata),
                resource_metadata: None,
                resource_metadata_url: None,
            }));
            let mut opts = default_flow_options(format!("{origin}/mcp"));
            opts.authorization_code = Some(code);
            opts.iss = iss;
            authorize_mcp(&provider, opts).await
        }
    };

    assert!(
        exchange("other", Some("https://attacker.example"), false)
            .await
            .unwrap_err()
            .downcast_ref::<OAuthIssuerMismatchError>()
            .is_some()
    );
    assert!(
        exchange("missing", None, true)
            .await
            .unwrap_err()
            .downcast_ref::<OAuthIssuerMismatchError>()
            .is_some()
    );
    assert_eq!(
        exchange("matching", Some(&origin), true).await.unwrap(),
        OAuthFlowResult::Authorized
    );
    // Servers that do not promise the parameter may omit it.
    assert_eq!(
        exchange("omitted", None, false).await.unwrap(),
        OAuthFlowResult::Authorized
    );
    assert_eq!(
        *codes.lock().unwrap(),
        vec!["matching".to_string(), "omitted".to_string()]
    );
    servers.close().await;
}

#[tokio::test]
async fn renders_plain_text_by_default() {
    let callback = OAuthCallbackServer::listen(None).await.unwrap();
    let (cb_res, resp) = tokio::join!(callback.wait_for_callback("s1"), async {
        tokio::task::yield_now().await;
        pi_js::fetch::fetch(Request::new(
            "GET",
            format!("{}?code=abc&state=s1", callback.redirect_url()),
        ))
        .await
        .unwrap()
    });
    assert_eq!(
        resp.headers().get("content-type").as_deref(),
        Some("text/plain; charset=utf-8")
    );
    assert_eq!(
        resp.text().await.unwrap(),
        "Authorization complete. You may close this window."
    );
    assert_eq!(cb_res.unwrap().code, "abc");
    callback.close().await.unwrap();
}

#[tokio::test]
async fn renders_pages_through_render_page() {
    let pages = Arc::new(Mutex::new(Vec::<OAuthCallbackPage>::new()));
    let pages_cb = pages.clone();
    let callback = OAuthCallbackServer::listen(Some(OAuthCallbackServerOptions {
        render_page: Some(Arc::new(move |page| {
            pages_cb.lock().unwrap().push(page.clone());
            match page {
                OAuthCallbackPage::Ok => "<p>ok</p>".to_string(),
                OAuthCallbackPage::Err { message, .. } => format!("<p>{message}</p>"),
            }
        })),
        ..Default::default()
    }))
    .await
    .unwrap();

    let (denied_res, failure) = tokio::join!(callback.wait_for_callback("s1"), async {
        tokio::task::yield_now().await;
        pi_js::fetch::fetch(Request::new(
            "GET",
            format!(
                "{}?error=access_denied&error_description=Denied&state=s1",
                callback.redirect_url()
            ),
        ))
        .await
        .unwrap()
    });
    assert_eq!(
        failure.headers().get("content-type").as_deref(),
        Some("text/html; charset=utf-8")
    );
    let err = denied_res.unwrap_err();
    assert!(err.to_string().contains("Denied"), "unexpected error: {err}");
    assert_eq!(
        pages.lock().unwrap().last().cloned(),
        Some(OAuthCallbackPage::Err {
            message: "Authorization failed. You may close this window.".to_string(),
            details: Some("Denied".to_string()),
        })
    );

    let (pending_res, success) = tokio::join!(callback.wait_for_callback("s2"), async {
        tokio::task::yield_now().await;
        pi_js::fetch::fetch(Request::new(
            "GET",
            format!("{}?code=abc&state=s2", callback.redirect_url()),
        ))
        .await
        .unwrap()
    });
    assert_eq!(success.text().await.unwrap(), "<p>ok</p>");
    assert_eq!(pending_res.unwrap().code, "abc");
    callback.close().await.unwrap();
}

#[tokio::test]
async fn rejects_invalid_and_non_hierarchical_discovery_urls_with_type_error() {
    let invalid = pi_mcp::oauth::build_authorization_server_discovery_urls("not a url").unwrap_err();
    assert_eq!(invalid.name(), "TypeError");

    let opaque = pi_mcp::oauth::build_authorization_server_discovery_urls("urn:example:issuer").unwrap_err();
    assert_eq!(opaque.name(), "TypeError");

    let info_err = pi_mcp::oauth::discover_oauth_server_info("urn:example:server", None)
        .await
        .unwrap_err();
    assert_eq!(info_err.name(), "TypeError");
}
