//! Port of packages/coding-agent/src/extensions/mcp/oauth.ts

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::{IndexMap, IndexSet};
use sha2::{Digest, Sha256};

use crate::core::extensions::types::mcp_wire::mcp_namespace;
use crate::extensions::wire::{
    APP_NAME, AuthStorageBackend, FileAuthStorageBackend, acquire_lock_async, auth_with_lock, get_agent_dir,
};
use pi_ai::utils::oauth_page::{oauth_error_html, oauth_success_html};
use pi_js::abort::{AbortController, AbortReason, AbortSignal};
use pi_js::{BoxFuture, Error, Result};
use pi_mcp::oauth::provider::McpOAuthProviderClientMetadata;
use pi_mcp::oauth::{
    McpOAuthAuthorizationRequiredError, McpOAuthProvider, McpOAuthProviderOptions, McpOAuthState, McpOAuthStateStore,
    OAuthCallback, OAuthCallbackPage, OAuthCallbackServer, OAuthCallbackServerOptions, OAuthChallenge,
    OAuthClientInformationMixed, OAuthFlowOptions, OAuthFlowResult, authorize_mcp, parse_www_authenticate,
    step_up_scope,
};
use pi_mcp::{AuthProvider, McpFetch, UnauthorizedContext};

const CALLBACK_HOST: &str = "127.0.0.1";
const FALLBACK_REDIRECT_URL: &str = "http://127.0.0.1/callback";
const REFRESH_SKEW_MS: i64 = 30_000;
const REFRESH_REQUEST_TIMEOUT_MS: u64 = 15_000;

#[derive(Clone, Debug, Default)]
pub struct McpOAuthSettings {
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    pub callback_port: Option<i64>,
    pub callback_url: Option<String>,
    pub scope: Option<String>,
    pub client_name: Option<String>,
    pub auth_server_metadata_url: Option<url::Url>,
}

struct CallbackSettings {
    host: String,
    redirect_host: String,
    port: Option<i64>,
    path: String,
    fixed_redirect_url: Option<String>,
}

fn callback_settings(settings: &McpOAuthSettings) -> Result<CallbackSettings> {
    let raw = settings.callback_url.as_deref().unwrap_or("http://127.0.0.1/callback");
    let mut parsed = url::Url::parse(raw).map_err(|e| Error::js("TypeError", format!("Invalid URL: {e}")))?;
    let host_str = parsed.host_str().unwrap_or(CALLBACK_HOST).to_string();
    let address = host_str.trim_start_matches('[').trim_end_matches(']').to_string();
    let url_port = parsed.port().map(i64::from);
    let port = url_port.or(settings.callback_port);
    let fixed_redirect_url = if url_port.is_some() {
        settings.callback_url.clone()
    } else if let Some(p) = port {
        let _ = parsed.set_port(Some(p as u16));
        Some(parsed.to_string())
    } else {
        None
    };
    Ok(CallbackSettings {
        host: if address == "localhost" {
            CALLBACK_HOST.to_string()
        } else {
            address.clone()
        },
        redirect_host: address,
        port,
        path: parsed.path().to_string(),
        fixed_redirect_url,
    })
}

fn merge_scopes(a: Option<&str>, b: Option<&str>) -> Option<String> {
    let mut set = IndexSet::new();
    for src in [a, b].into_iter().flatten() {
        for word in src.split(|c: char| pi_js::str16::is_js_whitespace(c)) {
            if !word.is_empty() {
                set.insert(word.to_string());
            }
        }
    }
    if set.is_empty() {
        None
    } else {
        Some(set.into_iter().collect::<Vec<_>>().join(" "))
    }
}

type StoredStates = IndexMap<String, McpOAuthState>;

fn parse_states(content: Option<&str>) -> Result<StoredStates> {
    let Some(content) = content else {
        return Ok(IndexMap::new());
    };
    if pi_js::str16::trim(content).is_empty() {
        return Ok(IndexMap::new());
    }
    let val = pi_js::json::parse(content)?;
    let Some(obj) = val.as_object() else {
        return Ok(IndexMap::new());
    };
    let mut out = IndexMap::new();
    for (k, v) in obj {
        if let Ok(state) = pi_js::json::parse_as::<McpOAuthState>(&pi_js::json::stringify(v)) {
            out.insert(k.clone(), state);
        }
    }
    Ok(out)
}

fn serialize_states(states: &StoredStates) -> String {
    format!("{}\n", pi_js::json::stringify_pretty(states, "  "))
}

fn store_keys(name: &str, server_url: &str) -> Result<(String, String)> {
    let legacy_key = url::Url::parse(server_url)
        .map_err(|e| Error::js("TypeError", format!("Invalid URL: {e}")))?
        .to_string();
    let key = format!("{}|{legacy_key}", mcp_namespace(name));
    Ok((key, legacy_key))
}

#[async_trait]
pub trait McpOAuthServerStore: McpOAuthStateStore {
    async fn with_refresh_lock_erased(&self, f: Box<dyn FnOnce() -> BoxFuture<Result<()>> + Send>) -> Result<()>;
}

pub async fn with_refresh_lock<T, F>(store: &dyn McpOAuthServerStore, f: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> BoxFuture<Result<T>> + Send + 'static,
{
    let slot: Arc<Mutex<Option<T>>> = Arc::new(Mutex::new(None));
    let slot_cb = Arc::clone(&slot);
    store
        .with_refresh_lock_erased(Box::new(move || {
            Box::pin(async move {
                let val = f().await?;
                *slot_cb.lock().unwrap_or_else(|e| e.into_inner()) = Some(val);
                Ok(())
            })
        }))
        .await?;
    let val = slot.lock().unwrap_or_else(|e| e.into_inner()).take();
    val.ok_or_else(|| Error::msg("Refresh lock closure completed without result"))
}

#[derive(Clone)]
pub struct McpOAuthCredentialStore {
    backend: Arc<dyn AuthStorageBackend>,
    lock_dir: Option<String>,
    in_process_locks: Arc<Mutex<IndexMap<String, Arc<tokio::sync::Mutex<()>>>>>,
}

impl McpOAuthCredentialStore {
    pub fn new(backend: Option<Arc<dyn AuthStorageBackend>>, lock_dir: Option<String>) -> Self {
        match backend {
            Some(b) => Self {
                backend: b,
                lock_dir,
                in_process_locks: Arc::new(Mutex::new(IndexMap::new())),
            },
            None => {
                let agent_dir = get_agent_dir();
                let auth_path = pi_js::path::join(&[&agent_dir, "mcp-auth.json"]);
                Self {
                    backend: Arc::new(FileAuthStorageBackend::new(Some(&auth_path))),
                    lock_dir: Some(agent_dir),
                    in_process_locks: Arc::new(Mutex::new(IndexMap::new())),
                }
            }
        }
    }

    pub fn for_server(&self, name: &str, server_url: &str) -> Result<Arc<dyn McpOAuthServerStore>> {
        let (key, legacy_key) = store_keys(name, server_url)?;
        Ok(Arc::new(ServerOAuthStore {
            store: self.clone(),
            key,
            legacy_key,
        }))
    }

    pub fn tokens(&self, name: &str, server_url: &str) -> Option<pi_mcp::oauth::OAuthTokens> {
        let (key, legacy_key) = store_keys(name, server_url).ok()?;
        auth_with_lock(self.backend.as_ref(), |current| {
            let states = parse_states(current)?;
            let found = states
                .get(&key)
                .or_else(|| states.get(&legacy_key))
                .and_then(|s| s.tokens.clone());
            Ok((found, None))
        })
        .ok()
        .flatten()
    }

    pub fn remove(&self, name: &str, server_url: &str) -> bool {
        let Ok((key, legacy_key)) = store_keys(name, server_url) else {
            return false;
        };
        auth_with_lock(self.backend.as_ref(), |current| {
            let mut states = parse_states(current)?;
            let stored_key = if states.contains_key(&key) {
                Some(key.clone())
            } else if states.contains_key(&legacy_key) {
                Some(legacy_key.clone())
            } else {
                None
            };
            let Some(k) = stored_key else {
                return Ok((false, None));
            };
            states.shift_remove(&k);
            Ok((true, Some(serialize_states(&states))))
        })
        .unwrap_or(false)
    }

    fn write_with<F>(&self, update: F) -> Result<()>
    where
        F: Fn(&mut StoredStates),
    {
        auth_with_lock(self.backend.as_ref(), |current| {
            let mut states = parse_states(current)?;
            update(&mut states);
            Ok(((), Some(serialize_states(&states))))
        })
    }

    async fn run_with_refresh_lock(
        &self,
        key: &str,
        f: Box<dyn FnOnce() -> BoxFuture<Result<()>> + Send>,
    ) -> Result<()> {
        let in_proc_mutex = {
            let mut map = self.in_process_locks.lock().unwrap_or_else(|e| e.into_inner());
            Arc::clone(
                map.entry(key.to_string())
                    .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
            )
        };
        let _in_proc_guard = in_proc_mutex.lock().await;
        let _dir_guard = if let Some(ref lock_dir) = self.lock_dir {
            std::fs::create_dir_all(lock_dir).map_err(|e| Error::from_io(e, "mkdir", Some(lock_dir), None))?;
            let mut hasher = Sha256::new();
            hasher.update(key.as_bytes());
            let hash = &hex::encode(hasher.finalize())[..16];
            let lock_path = PathBuf::from(lock_dir).join(format!("mcp-auth-refresh-{hash}"));
            Some(acquire_lock_async(&lock_path, None).await?)
        } else {
            None
        };
        f().await
    }
}

struct ServerOAuthStore {
    store: McpOAuthCredentialStore,
    key: String,
    legacy_key: String,
}

#[async_trait]
impl McpOAuthStateStore for ServerOAuthStore {
    async fn load(&self) -> Result<Option<McpOAuthState>> {
        let key = self.key.clone();
        let legacy = self.legacy_key.clone();
        auth_with_lock(self.store.backend.as_ref(), |current| {
            let mut states = parse_states(current)?;
            if let Some(existing) = states.get(&key).cloned() {
                return Ok((Some(existing), None));
            }
            if let Some(legacy_state) = states.shift_remove(&legacy) {
                states.insert(key.clone(), legacy_state.clone());
                return Ok((Some(legacy_state), Some(serialize_states(&states))));
            }
            Ok((None, None))
        })
    }

    async fn save(&self, state: McpOAuthState) -> Result<()> {
        let key = self.key.clone();
        self.store.write_with(|states| {
            states.insert(key.clone(), state.clone());
        })
    }
}

#[async_trait]
impl McpOAuthServerStore for ServerOAuthStore {
    async fn with_refresh_lock_erased(&self, f: Box<dyn FnOnce() -> BoxFuture<Result<()>> + Send>) -> Result<()> {
        self.store.run_with_refresh_lock(&self.key, f).await
    }
}

fn registered_redirect_urls(client: Option<&OAuthClientInformationMixed>) -> Vec<String> {
    client.and_then(|c| c.redirect_uris.clone()).unwrap_or_default()
}

fn create_provider<F>(
    server_url: &str,
    store: Arc<dyn McpOAuthStateStore>,
    settings: &McpOAuthSettings,
    redirect_url: String,
    on_redirect: F,
) -> McpOAuthProvider
where
    F: Fn(url::Url) + Send + Sync + 'static,
{
    McpOAuthProvider::new(McpOAuthProviderOptions {
        server_url: server_url.to_string(),
        redirect_url,
        client_metadata: McpOAuthProviderClientMetadata {
            client_name: Some(settings.client_name.clone().unwrap_or_else(|| APP_NAME.to_string())),
            ..McpOAuthProviderClientMetadata::default()
        },
        client_id: settings.client_id.clone(),
        client_secret: settings.client_secret.clone(),
        store: Some(store),
        on_redirect: Arc::new(move |url| {
            on_redirect(url);
            Box::pin(async { Ok(()) })
        }),
    })
}

#[async_trait]
pub trait McpAuthProvider: AuthProvider {
    async fn settled(&self);
}

struct SharedMcpAuthProvider {
    server_url: String,
    store: Arc<dyn McpOAuthServerStore>,
    settings_fn: Arc<dyn Fn() -> Result<McpOAuthSettings> + Send + Sync>,
    on_challenge: Arc<dyn Fn(OAuthChallenge) + Send + Sync>,
    refreshing: Arc<Mutex<Option<futures::future::Shared<BoxFuture<std::result::Result<(), Arc<Error>>>>>>>,
}

impl SharedMcpAuthProvider {
    fn refresh(
        &self,
        stale_token: Option<String>,
        custom_fetch: Option<McpFetch>,
        challenge: Option<OAuthChallenge>,
    ) -> futures::future::Shared<BoxFuture<std::result::Result<(), Arc<Error>>>> {
        use futures::FutureExt;
        let mut guard = self.refreshing.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(ref existing) = *guard {
            return existing.clone();
        }
        let store = Arc::clone(&self.store);
        let server_url = self.server_url.clone();
        let settings_fn = Arc::clone(&self.settings_fn);
        let slot = Arc::clone(&self.refreshing);

        let fut: BoxFuture<std::result::Result<(), Arc<Error>>> = Box::pin(async move {
            let store_in = Arc::clone(&store);
            let result = with_refresh_lock(store.as_ref(), move || {
                let store = store_in;
                Box::pin(async move {
                    let state = store.load().await?;
                    let current_token = state
                        .as_ref()
                        .and_then(|s| s.tokens.as_ref())
                        .map(|t| t.access_token.clone());
                    if current_token != stale_token {
                        return Ok(());
                    }
                    let has_refresh = state
                        .as_ref()
                        .and_then(|s| s.tokens.as_ref())
                        .and_then(|t| t.refresh_token.as_ref())
                        .is_some_and(|rt| !rt.is_empty());
                    if !has_refresh {
                        return Err(Error::typed(
                            "McpOAuthAuthorizationRequiredError",
                            McpOAuthAuthorizationRequiredError::new(),
                        ));
                    }
                    let settings = settings_fn()?;
                    let cb_settings = callback_settings(&settings)?;
                    let registered =
                        registered_redirect_urls(state.as_ref().and_then(|s| s.client_information.as_ref()));
                    let redirect_url = cb_settings
                        .fixed_redirect_url
                        .or_else(|| registered.into_iter().next())
                        .unwrap_or_else(|| FALLBACK_REDIRECT_URL.to_string());
                    let provider = create_provider(
                        &server_url,
                        Arc::clone(&store) as Arc<dyn McpOAuthStateStore>,
                        &settings,
                        redirect_url,
                        |_| {},
                    );
                    let wrapped_fetch: Option<McpFetch> = custom_fetch.map(|base| {
                        Arc::new(move |url: &str, init: Option<pi_mcp::auth_provider::McpFetchInit>| {
                            let mut init = init.unwrap_or_default();
                            init.signal = Some(AbortSignal::timeout(REFRESH_REQUEST_TIMEOUT_MS));
                            base(url, Some(init))
                        }) as McpFetch
                    });
                    let flow_res = authorize_mcp(
                        &provider,
                        OAuthFlowOptions {
                            server_url,
                            authorization_code: None,
                            iss: None,
                            scope: challenge.as_ref().and_then(|c| c.scope.clone()),
                            resource_metadata_url: challenge.as_ref().and_then(|c| c.resource_metadata_url.clone()),
                            authorization_server_metadata_url: settings.auth_server_metadata_url,
                            fetch: wrapped_fetch,
                            skip_issuer_validation: None,
                            skip_refresh: None,
                        },
                    )
                    .await?;
                    if flow_res == OAuthFlowResult::Redirect {
                        return Err(Error::typed(
                            "McpOAuthAuthorizationRequiredError",
                            McpOAuthAuthorizationRequiredError::new(),
                        ));
                    }
                    Ok(())
                })
            })
            .await;
            *slot.lock().unwrap_or_else(|e| e.into_inner()) = None;
            result.map_err(Arc::new)
        });
        let shared = fut.shared();
        *guard = Some(shared.clone());
        shared
    }
}

fn clone_arc_err(err: &Arc<Error>) -> Error {
    if let Some(e) = err.downcast_ref::<McpOAuthAuthorizationRequiredError>() {
        return Error::typed("McpOAuthAuthorizationRequiredError", e.clone());
    }
    Error::msg(err.message())
}

#[async_trait]
impl AuthProvider for SharedMcpAuthProvider {
    async fn token(&self) -> Result<Option<String>> {
        let pending = self.refreshing.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(fut) = pending {
            let _ = fut.await;
        }
        let state = self.store.load().await?;
        let token = state
            .as_ref()
            .and_then(|s| s.tokens.as_ref())
            .map(|t| t.access_token.clone());
        let expired = state
            .as_ref()
            .and_then(|s| s.tokens_expire_at)
            .is_some_and(|exp| exp - REFRESH_SKEW_MS <= pi_js::time::now_ms());
        let has_refresh = state
            .as_ref()
            .and_then(|s| s.tokens.as_ref())
            .and_then(|t| t.refresh_token.as_ref())
            .is_some_and(|rt| !rt.is_empty());
        if !expired || !has_refresh {
            return Ok(token);
        }
        let _ = self.refresh(token, None, None).await;
        Ok(self.store.load().await?.and_then(|s| s.tokens).map(|t| t.access_token))
    }

    fn on_unauthorized(&self, context: UnauthorizedContext) -> Option<BoxFuture<Result<()>>> {
        let challenge = parse_www_authenticate(context.response.headers().get("www-authenticate").as_deref());
        (self.on_challenge)(challenge.clone());
        if challenge.error.as_deref() == Some("insufficient_scope") {
            return Some(Box::pin(async {
                Err(Error::typed(
                    "McpOAuthAuthorizationRequiredError",
                    McpOAuthAuthorizationRequiredError::new(),
                ))
            }));
        }
        let fut = self.refresh(context.token, Some(context.fetch), Some(challenge));
        Some(Box::pin(async move { fut.await.map_err(|e| clone_arc_err(&e)) }))
    }
}

#[async_trait]
impl McpAuthProvider for SharedMcpAuthProvider {
    async fn settled(&self) {
        let pending = self.refreshing.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(fut) = pending {
            let _ = fut.await;
        }
    }
}

pub fn create_mcp_auth_provider(
    server_url: impl Into<String>,
    store: Arc<dyn McpOAuthServerStore>,
    settings: Arc<dyn Fn() -> Result<McpOAuthSettings> + Send + Sync>,
    on_challenge: Arc<dyn Fn(OAuthChallenge) + Send + Sync>,
) -> Arc<dyn McpAuthProvider> {
    Arc::new(SharedMcpAuthProvider {
        server_url: server_url.into(),
        store,
        settings_fn: settings,
        on_challenge,
        refreshing: Arc::new(Mutex::new(None)),
    })
}

#[async_trait]
pub trait McpSignInPrompt: Send + Sync {
    fn show_authorization_url(&self, url: &url::Url);
    async fn prompt_for_redirect_url(&self, signal: AbortSignal) -> Result<Option<String>>;
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("Sign-in cancelled")]
pub struct McpSignInCancelledError;

impl McpSignInCancelledError {
    pub fn new() -> Self {
        Self
    }
}

impl Default for McpSignInCancelledError {
    fn default() -> Self {
        Self::new()
    }
}

fn response_from_redirect_url(input: &str, state: &str) -> Result<OAuthCallback> {
    let url = url::Url::parse(pi_js::str16::trim(input))
        .map_err(|_| Error::msg("Expected the full redirect URL from the browser address bar"))?;
    let param = |k: &str| url.query_pairs().find(|(key, _)| key == k).map(|(_, v)| v.into_owned());
    if let Some(err) = param("error") {
        return Err(Error::msg(param("error_description").unwrap_or(err)));
    }
    if param("state").as_deref() != Some(state) {
        return Err(Error::msg("The redirect URL belongs to a different sign-in"));
    }
    let code = param("code").ok_or_else(|| Error::msg("The redirect URL does not contain an authorization code"))?;
    Ok(OAuthCallback {
        code,
        state: state.to_string(),
        iss: param("iss"),
    })
}

async fn wait_for_authorization_response(
    callback: &OAuthCallbackServer,
    state: &str,
    prompt: &dyn McpSignInPrompt,
) -> Result<OAuthCallback> {
    let controller = AbortController::new();
    let browser_fut = callback.wait_for_callback(state);
    let user_fut = prompt.prompt_for_redirect_url(controller.signal());
    let res = tokio::select! {
        res = browser_fut => res,
        user_res = user_fut => match user_res {
            Ok(Some(input)) if !pi_js::str16::trim(&input).is_empty() => {
                response_from_redirect_url(&input, state)
            }
            Ok(_) => Err(Error::typed("McpSignInCancelledError", McpSignInCancelledError::new())),
            Err(e) => Err(e),
        }
    };
    controller.abort(Some(AbortReason::error("AbortError", "settled")));
    res
}

async fn listen_for_callback(
    settings: &CallbackSettings,
    port: Option<i64>,
    required: bool,
) -> Result<OAuthCallbackServer> {
    let render_page: Arc<dyn Fn(&OAuthCallbackPage) -> String + Send + Sync> =
        Arc::new(|page: &OAuthCallbackPage| match page {
            OAuthCallbackPage::Ok => oauth_success_html("Signed in to the MCP server. You may now close this page."),
            OAuthCallbackPage::Err { message, details } => oauth_error_html(message, details.as_deref()),
        });
    let opts = OAuthCallbackServerOptions {
        host: Some(settings.host.clone()),
        redirect_host: Some(settings.redirect_host.clone()),
        port,
        path: Some(settings.path.clone()),
        timeout_ms: None,
        render_page: Some(Arc::clone(&render_page)),
    };
    match OAuthCallbackServer::listen(Some(opts)).await {
        Ok(srv) => Ok(srv),
        Err(err) => {
            if required || port.is_none() {
                return Err(err);
            }
            OAuthCallbackServer::listen(Some(OAuthCallbackServerOptions {
                host: Some(settings.host.clone()),
                redirect_host: Some(settings.redirect_host.clone()),
                port: None,
                path: Some(settings.path.clone()),
                timeout_ms: None,
                render_page: Some(render_page),
            }))
            .await
        }
    }
}

pub struct SignInMcpServerOptions {
    pub server_url: String,
    pub store: Arc<dyn McpOAuthStateStore>,
    pub settings: McpOAuthSettings,
    pub challenge: Option<OAuthChallenge>,
    pub prompt: Arc<dyn McpSignInPrompt>,
}

pub async fn sign_in_mcp_server(options: SignInMcpServerOptions) -> Result<()> {
    let stored = options.store.load().await?;
    let step_up = options.challenge.as_ref().and_then(|c| c.error.as_deref()) == Some("insufficient_scope");
    let cb_settings = callback_settings(&options.settings)?;
    let registered = registered_redirect_urls(stored.as_ref().and_then(|s| s.client_information.as_ref()));
    let preferred_port = cb_settings.port.or_else(|| {
        registered
            .first()
            .and_then(|r| url::Url::parse(r).ok())
            .and_then(|u| u.port())
            .map(i64::from)
    });
    let callback = listen_for_callback(&cb_settings, preferred_port, cb_settings.port.is_some()).await?;
    let redirect_url = cb_settings
        .fixed_redirect_url
        .unwrap_or_else(|| callback.redirect_url().to_string());

    let run = async {
        if let Some(ref existing) = stored {
            let mut next = existing.clone();
            next.oauth_state = None;
            if options.settings.client_id.is_none() && !registered.contains(&redirect_url) {
                next.client_information = None;
                next.tokens = None;
                next.tokens_expire_at = None;
            }
            options.store.save(next).await?;
        }

        let auth_url_slot: Arc<Mutex<Option<url::Url>>> = Arc::new(Mutex::new(None));
        let auth_url_cb = Arc::clone(&auth_url_slot);
        let provider = create_provider(
            &options.server_url,
            Arc::clone(&options.store),
            &options.settings,
            redirect_url,
            move |url| {
                *auth_url_cb.lock().unwrap_or_else(|e| e.into_inner()) = Some(url);
            },
        );
        let scope = merge_scopes(
            options.settings.scope.as_deref(),
            if step_up {
                let granted = stored
                    .as_ref()
                    .and_then(|s| s.tokens.as_ref())
                    .and_then(|t| t.scope.as_deref());
                let challenged = options.challenge.as_ref().and_then(|c| c.scope.as_deref());
                step_up_scope(granted, challenged).as_deref().map(String::from)
            } else {
                options.challenge.as_ref().and_then(|c| c.scope.clone())
            }
            .as_deref(),
        );
        let flow = OAuthFlowOptions {
            server_url: options.server_url.clone(),
            authorization_code: None,
            iss: None,
            scope: scope.clone(),
            resource_metadata_url: options.challenge.as_ref().and_then(|c| c.resource_metadata_url.clone()),
            authorization_server_metadata_url: options.settings.auth_server_metadata_url.clone(),
            fetch: None,
            skip_issuer_validation: None,
            skip_refresh: Some(step_up),
        };
        if authorize_mcp(&provider, flow.clone()).await? == OAuthFlowResult::Authorized {
            return Ok(());
        }
        let auth_url = auth_url_slot
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or_else(|| Error::msg("OAuth flow did not produce an authorization URL"))?;
        let state = pi_mcp::oauth::flow::OAuthClientProvider::state(&provider)
            .await?
            .ok_or_else(|| Error::msg("OAuth state is missing"))?;
        options.prompt.show_authorization_url(&auth_url);
        let resp = wait_for_authorization_response(&callback, &state, options.prompt.as_ref()).await?;
        authorize_mcp(
            &provider,
            OAuthFlowOptions {
                authorization_code: Some(resp.code),
                iss: resp.iss,
                ..flow
            },
        )
        .await?;
        Ok(())
    }
    .await;

    let _ = callback.close().await;
    run
}
