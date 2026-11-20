//! Port of packages/coding-agent/src/extensions/mcp/oauth.ts

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::config::{APP_NAME, get_agent_dir};
use crate::core::auth_storage::{AuthStorageBackend, FileAuthStorageBackend, LockResult};
pub use crate::core::mcp_servers::{McpClientRegistration, is_client_metadata_document_callback};
use crate::vendor::proper_lockfile::{LockOptions, LockRelease, RetryOptions, lock};
use pi_ai::utils::oauth_page::{oauth_error_html, oauth_success_html};
use pi_js::abort::AbortSignal;
use pi_js::{BoxFuture, Error, Result};
pub use pi_mcp::mcp_namespace;
pub use pi_mcp::oauth::{
    CreateMcpAuthProviderOptions, ListenForLoopbackSignInOptions, McpAuthProvider, McpClientMetadataDocuments,
    McpOAuthServerStore, McpOAuthSettings, McpOAuthState, McpOAuthStateStore, McpRefreshLockBusyError,
    McpSignInOptions, McpSignInStart, OAuthCallbackPage, OAuthChallenge, OAuthTokens, WaitForLoopbackSignInOptions,
    complete_mcp_sign_in, listen_for_loopback_sign_in, start_mcp_sign_in, wait_for_loopback_sign_in,
};

pub use pi_mcp::oauth::McpSignInCancelledError;

/// A refresh lock that its holder stopped renewing (the process was killed) is taken over after this.
const REFRESH_LOCK_STALE_MS: u64 = 20_000;
/// How long to wait for another process's refresh: longer than a stale lock lives.
const REFRESH_LOCK_WAIT_MS: u64 = 25_000;
const REFRESH_LOCK_RETRY_MS: u64 = 100;

fn auth_with_lock<T, F>(backend: &dyn AuthStorageBackend, mut f: F) -> Result<T>
where
    F: FnMut(Option<&str>) -> Result<(T, Option<String>)>,
{
    let mut out: Option<T> = None;
    backend.with_lock_dyn(&mut |current| {
        let (result, next) = f(current)?;
        out = Some(result);
        Ok(match next {
            Some(content) => LockResult::write(Value::Null, content),
            None => LockResult::unchanged(Value::Null),
        })
    })?;
    out.ok_or_else(|| Error::msg("Auth lock callback did not return a result"))
}

async fn acquire_refresh_lock_async(lock_dir: &str, hash: &str) -> Result<LockRelease> {
    let lockfile_path = pi_js::path::join(&[lock_dir, &format!("mcp-auth-refresh-{hash}.lock")]);
    lock(
        lock_dir,
        LockOptions {
            stale: Some(REFRESH_LOCK_STALE_MS),
            realpath: Some(false),
            lockfile_path: Some(lockfile_path),
            retries: Some(RetryOptions {
                retries: (REFRESH_LOCK_WAIT_MS / REFRESH_LOCK_RETRY_MS) as u32,
                factor: 1.0,
                min_timeout: REFRESH_LOCK_RETRY_MS,
                max_timeout: REFRESH_LOCK_RETRY_MS,
                randomize: false,
            }),
            ..Default::default()
        },
    )
    .await
    .map_err(|err| {
        if err.code() == Some("ELOCKED") {
            Error::typed("McpRefreshLockBusyError", McpRefreshLockBusyError::new())
        } else {
            err
        }
    })
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

    pub fn tokens(&self, name: &str, server_url: &str) -> Option<OAuthTokens> {
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
        f: Box<dyn FnOnce() -> BoxFuture<Result<Box<dyn std::any::Any + Send>>> + Send>,
    ) -> Result<Box<dyn std::any::Any + Send>> {
        let in_proc_mutex = {
            let mut map = self.in_process_locks.lock().unwrap_or_else(|e| e.into_inner());
            Arc::clone(
                map.entry(key.to_string())
                    .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
            )
        };
        let _in_proc_guard = in_proc_mutex.lock().await;
        let lock_release = if let Some(ref lock_dir) = self.lock_dir {
            std::fs::create_dir_all(lock_dir).map_err(|e| Error::from_io(e, "mkdir", Some(lock_dir), None))?;
            let mut hasher = Sha256::new();
            hasher.update(key.as_bytes());
            let hash = &hex::encode(hasher.finalize())[..16];
            Some(acquire_refresh_lock_async(lock_dir, hash).await?)
        } else {
            None
        };
        let res = f().await;
        if let Some(release) = lock_release {
            let _ = release.release().await;
        }
        res
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
    async fn with_refresh_lock_boxed(
        &self,
        work: Box<dyn FnOnce() -> BoxFuture<Result<Box<dyn std::any::Any + Send>>> + Send>,
    ) -> Result<Box<dyn std::any::Any + Send>> {
        self.store.run_with_refresh_lock(&self.key, work).await
    }
}

struct PiClientMetadataDocuments;

impl McpClientMetadataDocuments for PiClientMetadataDocuments {
    fn base_url(&self) -> &str {
        "https://pi.dev/oauth"
    }

    fn lists_redirect_url(&self, redirect_url: &str) -> bool {
        is_client_metadata_document_callback(Some(redirect_url))
    }
}

/// `settings` for `pi-mcp`: a rebranded build would show up as `pi` with pi's documents, so `auto` registers.
fn pi_settings(mut settings: McpOAuthSettings) -> McpOAuthSettings {
    let auto = settings.client_registration.unwrap_or(McpClientRegistration::Auto) == McpClientRegistration::Auto;
    if auto && *APP_NAME != "pi" {
        settings.client_registration = Some(McpClientRegistration::Dcr);
    }
    settings
}

pub fn create_mcp_auth_provider(
    server_url: impl Into<String>,
    store: Arc<dyn McpOAuthServerStore>,
    settings: Arc<dyn Fn() -> Result<McpOAuthSettings> + Send + Sync>,
    on_challenge: Arc<dyn Fn(OAuthChallenge) + Send + Sync>,
) -> Arc<dyn McpAuthProvider + Send + Sync> {
    pi_mcp::oauth::create_mcp_auth_provider(CreateMcpAuthProviderOptions {
        server_url: server_url.into(),
        store,
        settings: Arc::new(move || Ok(pi_settings(settings()?))),
        client_name: (*APP_NAME).to_string(),
        client_metadata_documents: Some(Arc::new(PiClientMetadataDocuments)),
        on_challenge: Arc::new(move |challenge| on_challenge(challenge.clone())),
        redirect_url: None,
    })
}

#[async_trait]
pub trait McpSignInPrompt: Send + Sync {
    fn show_authorization_url(&self, url: &url::Url);
    async fn prompt_for_redirect_url(&self, signal: AbortSignal) -> Result<Option<String>>;
}

fn render_callback_page(page: &OAuthCallbackPage) -> String {
    match page {
        OAuthCallbackPage::Ok => oauth_success_html("Signed in to the MCP server. You may now close this page."),
        OAuthCallbackPage::Err { message, details } => oauth_error_html(message, details.as_deref()),
    }
}

pub struct SignInMcpServerOptions {
    pub server_url: String,
    pub store: Arc<dyn McpOAuthServerStore>,
    pub settings: McpOAuthSettings,
    pub challenge: Option<OAuthChallenge>,
    pub prompt: Arc<dyn McpSignInPrompt>,
    pub signal: Option<AbortSignal>,
}

pub async fn sign_in_mcp_server(options: SignInMcpServerOptions) -> Result<()> {
    let settings = pi_settings(options.settings);
    let client_docs: Arc<dyn McpClientMetadataDocuments> = Arc::new(PiClientMetadataDocuments);
    let loopback = listen_for_loopback_sign_in(ListenForLoopbackSignInOptions {
        server_url: options.server_url.clone(),
        store: Arc::clone(&options.store) as Arc<dyn McpOAuthStateStore>,
        settings: settings.clone(),
        client_metadata_documents: Some(Arc::clone(&client_docs)),
        render_page: Some(Arc::new(render_callback_page)),
    })
    .await?;

    let sign_in_opts = McpSignInOptions {
        server_url: options.server_url,
        store: options.store,
        settings,
        client_name: (*APP_NAME).to_string(),
        client_metadata_documents: Some(client_docs),
        redirect_url: loopback.redirect_url,
        challenge: options.challenge,
        signal: options.signal.clone(),
    };

    let res = async {
        let start = start_mcp_sign_in(sign_in_opts.clone()).await?;
        let McpSignInStart::Redirect(authorization) = start else {
            return Ok(());
        };
        let auth_url = url::Url::parse(&authorization.authorization_url)
            .map_err(|e| Error::js("TypeError", format!("Invalid URL: {e}")))?;
        options.prompt.show_authorization_url(&auth_url);
        let prompt = Arc::clone(&options.prompt);
        let response = wait_for_loopback_sign_in(WaitForLoopbackSignInOptions {
            callback: loopback.callback.clone(),
            authorization: authorization.clone(),
            prompt_for_redirect_url: Some(Arc::new(move |sig| {
                let p = Arc::clone(&prompt);
                Box::pin(async move { p.prompt_for_redirect_url(sig).await })
            })),
            signal: options.signal,
        })
        .await?;
        complete_mcp_sign_in(sign_in_opts, authorization, response).await?;
        Ok(())
    }
    .await;

    let _ = loopback.callback.close().await;
    res
}
