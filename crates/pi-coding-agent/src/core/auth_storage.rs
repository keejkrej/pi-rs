//! Port of packages/coding-agent/src/core/auth-storage.ts

use std::future::Future;
use std::sync::{Arc, LazyLock, Mutex, PoisonError};

use indexmap::IndexMap;
use pi_ai::{
    ApiKeyCredential, AuthOperationOptions, Credential, CredentialInfo, CredentialStore, ModifyCredentialFn,
    OAuthCredential,
};
use pi_js::abort::{AbortController, AbortSignal};
use pi_js::{Error, Result};
use serde_json::Value;

use crate::config::get_agent_dir;
use crate::core::resolve_config_value::{is_command_config_value, resolve_config_value};
use crate::utils::abort::race_with_abort_signal;
use crate::utils::paths::{get_file_revision, normalize_path};
use crate::utils::text::strip_bom;
use crate::vendor::proper_lockfile::{LockOptions, LockRelease, lock, lock_sync};

pub type AuthStorageData = IndexMap<String, Credential>;

// On Windows, a just-removed lock directory can stay "delete pending" while another process
// (antivirus, indexer, another pi checking the lock) still has it open. mkdir then fails with
// EPERM until that handle closes. Retry this briefly so real permission errors still fail fast.
const MAX_EPERM_RETRIES: u32 = 5;

fn get_retryable_lock_error_code(error: &Error) -> Option<&'static str> {
    let code = error.code()?;
    if code == "ELOCKED" {
        return Some("ELOCKED");
    }
    if code == "EPERM" && pi_js::env::platform() == "win32" {
        return Some("EPERM");
    }
    None
}

pub enum LockNext {
    Unchanged,
    Write(String),
}

pub struct LockResult<T> {
    pub result: T,
    pub next: LockNext,
}

impl<T> LockResult<T> {
    pub fn unchanged(result: T) -> Self {
        Self {
            result,
            next: LockNext::Unchanged,
        }
    }

    pub fn write(result: T, next: String) -> Self {
        Self {
            result,
            next: LockNext::Write(next),
        }
    }
}

type BoxedLockResult<'a, T> = std::pin::Pin<Box<dyn Future<Output = Result<LockResult<T>>> + Send + 'a>>;

#[async_trait::async_trait]
pub trait AuthStorageBackend: Send + Sync {
    fn with_lock_dyn(&self, f: &mut dyn FnMut(Option<&str>) -> Result<LockResult<Value>>) -> Result<Value>;

    async fn with_lock_async_dyn(
        &self,
        f: Box<dyn FnOnce(Option<String>) -> BoxedLockResult<'static, Value> + Send>,
        options: Option<AuthOperationOptions>,
    ) -> Result<Value>;
}

struct AuthFileReload {
    controller: AbortController,
    task: tokio::sync::Mutex<
        Option<futures::future::Shared<pi_js::BoxFuture<std::result::Result<AuthStorageData, String>>>>,
    >,
    readers: usize,
}

struct AuthFileReadState {
    data: AuthStorageData,
    revision: Option<String>,
    reload: Option<Arc<Mutex<AuthFileReload>>>,
}

impl AuthFileReadState {
    fn new() -> Self {
        Self {
            data: AuthStorageData::new(),
            revision: None,
            reload: None,
        }
    }
}

static SHARED_AUTH_FILE_READ_STATE: LazyLock<Mutex<Option<(String, Arc<Mutex<AuthFileReadState>>)>>> =
    LazyLock::new(|| Mutex::new(None));

fn shared_read_state_for_path(auth_path: &str) -> Arc<Mutex<AuthFileReadState>> {
    let mut guard = SHARED_AUTH_FILE_READ_STATE
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if let Some((ref existing_path, ref state)) = *guard
        && existing_path == auth_path
    {
        return state.clone();
    }
    let state = Arc::new(Mutex::new(AuthFileReadState::new()));
    if guard.is_none() {
        *guard = Some((auth_path.to_owned(), state.clone()));
    }
    state
}

pub struct FileAuthStorageBackend {
    auth_path: String,
}

impl FileAuthStorageBackend {
    pub fn new(auth_path: Option<&str>) -> Self {
        let default_path;
        let raw = match auth_path {
            Some(p) => p,
            None => {
                default_path = pi_js::path::join(&[&get_agent_dir(), "auth.json"]);
                &default_path
            }
        };
        Self {
            auth_path: normalize_path(raw, Default::default()),
        }
    }

    pub fn auth_path(&self) -> &str {
        &self.auth_path
    }

    fn ensure_parent_dir(&self) -> Result<()> {
        let dir = pi_js::path::dirname(&self.auth_path);
        if !pi_js::fs::exists(&dir) {
            pi_js::fs::mkdir(&dir, true, Some(0o700))?;
        }
        Ok(())
    }

    fn ensure_file_exists(&self) -> Result<()> {
        if !pi_js::fs::exists(&self.auth_path) {
            pi_js::fs::write_file(&self.auth_path, "{}", Some(0o600))?;
        }
        Ok(())
    }

    fn acquire_lock_sync_with_retry(&self, path: &str) -> Result<LockRelease> {
        let max_attempts = 10u32;
        let delay_ms = 20i64;
        let mut eperm_retries = 0u32;

        for attempt in 1u32.. {
            match lock_sync(path, LockOptions::default()) {
                Ok(release) => return Ok(release),
                Err(error) => {
                    let Some(code) = get_retryable_lock_error_code(&error) else {
                        return Err(error);
                    };
                    if attempt == max_attempts
                        || (code == "EPERM" && {
                            eperm_retries += 1;
                            eperm_retries > MAX_EPERM_RETRIES
                        })
                    {
                        return Err(error);
                    }
                    let start = pi_js::time::now_ms();
                    while pi_js::time::now_ms() - start < delay_ms {
                        std::hint::spin_loop();
                    }
                }
            }
        }
        Err(Error::msg("Failed to acquire auth storage lock"))
    }

    async fn acquire_lock_async(
        &self,
        signal: Option<&AbortSignal>,
        on_compromised: Arc<dyn Fn(Error) + Send + Sync>,
    ) -> Result<LockRelease> {
        let stale_ms = 30_000u64;
        let max_delay_ms = 2_000.0f64;
        let deadline = pi_js::time::now_ms() + stale_ms as i64;
        let mut retry = 0i32;
        let mut eperm_retries = 0u32;

        loop {
            if let Some(sig) = signal {
                sig.throw_if_aborted()?;
            }
            let opts = LockOptions {
                stale: Some(stale_ms),
                retries: Some(Default::default()),
                on_compromised: Some(on_compromised.clone()),
                ..Default::default()
            };
            match lock(&self.auth_path, opts).await {
                Ok(release) => {
                    if let Some(sig) = signal
                        && sig.aborted()
                    {
                        let _ = release.release().await;
                        sig.throw_if_aborted()?;
                    }
                    return Ok(release);
                }
                Err(error) => {
                    if let Some(sig) = signal {
                        sig.throw_if_aborted()?;
                    }
                    let Some(code) = get_retryable_lock_error_code(&error) else {
                        return Err(error);
                    };
                    let remaining_ms = deadline - pi_js::time::now_ms();
                    if remaining_ms <= 0 {
                        return Err(error);
                    }
                    if code == "EPERM" {
                        eperm_retries += 1;
                        if eperm_retries > MAX_EPERM_RETRIES {
                            return Err(error);
                        }
                    }
                    let base_delay_ms = (10.0 * 2.0f64.powi(retry)).min(max_delay_ms / 2.0);
                    retry += 1;
                    let delay_ms = (base_delay_ms * (1.0 + pi_js::crypto::math_random()))
                        .round()
                        .min(remaining_ms as f64)
                        .max(0.0) as u64;
                    pi_js::time::sleep(delay_ms, signal).await?;
                }
            }
        }
    }

    pub fn with_lock<T>(&self, mut f: impl FnMut(Option<&str>) -> Result<LockResult<T>>) -> Result<T> {
        self.ensure_parent_dir()?;
        self.ensure_file_exists()?;

        let release = self.acquire_lock_sync_with_retry(&self.auth_path)?;
        let res = (|| {
            let current = if pi_js::fs::exists(&self.auth_path) {
                Some(pi_js::fs::read_to_string(&self.auth_path)?)
            } else {
                None
            };
            let LockResult { result, next } = f(current.as_deref())?;
            if let LockNext::Write(content) = next {
                pi_js::fs::write_file(&self.auth_path, content, Some(0o600))?;
            }
            Ok(result)
        })();
        let release_res = release.release_sync();
        match res {
            Ok(v) => {
                release_res?;
                Ok(v)
            }
            Err(e) => {
                let _ = release_res;
                Err(e)
            }
        }
    }

    pub async fn with_lock_async<T, F, Fut>(&self, f: F, options: Option<AuthOperationOptions>) -> Result<T>
    where
        F: FnOnce(Option<String>) -> Fut + Send,
        Fut: Future<Output = Result<LockResult<T>>> + Send,
    {
        let signal = options.as_ref().and_then(|o| o.signal.as_ref());
        if let Some(sig) = signal {
            sig.throw_if_aborted()?;
        }
        self.ensure_parent_dir()?;
        self.ensure_file_exists()?;

        let compromised_state: Arc<Mutex<(bool, Option<String>)>> = Arc::new(Mutex::new((false, None)));
        let comp_clone = compromised_state.clone();
        let on_comp: Arc<dyn Fn(Error) + Send + Sync> = Arc::new(move |err| {
            if let Ok(mut g) = comp_clone.lock() {
                g.0 = true;
                g.1 = Some(err.message().to_owned());
            }
        });
        let throw_if_compromised = |state: &Arc<Mutex<(bool, Option<String>)>>| -> Result<()> {
            let guard = state.lock().unwrap_or_else(PoisonError::into_inner);
            if guard.0 {
                return Err(Error::msg(
                    guard
                        .1
                        .clone()
                        .unwrap_or_else(|| "Auth storage lock was compromised".to_owned()),
                ));
            }
            Ok(())
        };

        let release = self.acquire_lock_async(signal, on_comp).await?;
        let res = async {
            throw_if_compromised(&compromised_state)?;
            if let Some(sig) = signal {
                sig.throw_if_aborted()?;
            }
            let current = if pi_js::fs::exists(&self.auth_path) {
                Some(pi_js::fs::read_to_string(&self.auth_path)?)
            } else {
                None
            };
            let LockResult { result, next } = f(current).await?;
            throw_if_compromised(&compromised_state)?;
            if let Some(sig) = signal {
                sig.throw_if_aborted()?;
            }
            if let LockNext::Write(content) = next {
                pi_js::fs::write_file(&self.auth_path, content, Some(0o600))?;
            }
            throw_if_compromised(&compromised_state)?;
            Ok(result)
        }
        .await;

        let _ = release.release().await;
        res
    }
}

#[async_trait::async_trait]
impl AuthStorageBackend for FileAuthStorageBackend {
    fn with_lock_dyn(&self, f: &mut dyn FnMut(Option<&str>) -> Result<LockResult<Value>>) -> Result<Value> {
        self.with_lock(|cur| f(cur))
    }

    async fn with_lock_async_dyn(
        &self,
        f: Box<dyn FnOnce(Option<String>) -> BoxedLockResult<'static, Value> + Send>,
        options: Option<AuthOperationOptions>,
    ) -> Result<Value> {
        self.with_lock_async(|cur| f(cur), options).await
    }
}

pub struct InMemoryAuthStorageBackend {
    value: Arc<Mutex<Option<String>>>,
    async_lock: Arc<tokio::sync::Mutex<()>>,
}

impl Default for InMemoryAuthStorageBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryAuthStorageBackend {
    pub fn new() -> Self {
        Self {
            value: Arc::new(Mutex::new(None)),
            async_lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    pub fn with_lock<T>(&self, mut f: impl FnMut(Option<&str>) -> Result<LockResult<T>>) -> Result<T> {
        let mut guard = self.value.lock().unwrap_or_else(PoisonError::into_inner);
        let LockResult { result, next } = f(guard.as_deref())?;
        if let LockNext::Write(next_val) = next {
            *guard = Some(next_val);
        }
        Ok(result)
    }

    pub async fn with_lock_async<T, F, Fut>(&self, f: F, options: Option<AuthOperationOptions>) -> Result<T>
    where
        F: FnOnce(Option<String>) -> Fut + Send + 'static,
        Fut: Future<Output = Result<LockResult<T>>> + Send + 'static,
        T: Send + 'static,
    {
        let signal = options.and_then(|o| o.signal);
        let value = self.value.clone();
        let async_lock = self.async_lock.clone();
        let sig_for_inner = signal.clone();

        let operation = async move {
            let _turn_guard = async_lock.lock().await;
            if let Some(ref sig) = sig_for_inner {
                sig.throw_if_aborted()?;
            }
            let current = value.lock().unwrap_or_else(PoisonError::into_inner).clone();
            let LockResult { result, next } = f(current).await?;
            if let Some(ref sig) = sig_for_inner {
                sig.throw_if_aborted()?;
            }
            if let LockNext::Write(next_val) = next {
                *value.lock().unwrap_or_else(PoisonError::into_inner) = Some(next_val);
            }
            Ok(result)
        };

        race_with_abort_signal(operation, signal.as_ref()).await
    }
}

#[async_trait::async_trait]
impl AuthStorageBackend for InMemoryAuthStorageBackend {
    fn with_lock_dyn(&self, f: &mut dyn FnMut(Option<&str>) -> Result<LockResult<Value>>) -> Result<Value> {
        self.with_lock(|cur| f(cur))
    }

    async fn with_lock_async_dyn(
        &self,
        f: Box<dyn FnOnce(Option<String>) -> BoxedLockResult<'static, Value> + Send>,
        options: Option<AuthOperationOptions>,
    ) -> Result<Value> {
        self.with_lock_async(move |cur| f(cur), options).await
    }
}

fn parse_auth_storage_object(content: &str) -> Result<AuthStorageData> {
    let parsed = pi_js::json::parse(strip_bom(content))?;
    let Some(obj) = parsed.as_object() else {
        return Err(Error::msg("Invalid auth.json: expected an object"));
    };
    let mut data = AuthStorageData::new();
    for (provider_id, val) in obj {
        let Some(cred_obj) = val.as_object() else {
            return Err(Error::msg(format!(
                "Invalid auth.json credential for provider \"{provider_id}\""
            )));
        };
        let cred_type = cred_obj.get("type").and_then(Value::as_str);
        match cred_type {
            Some("api_key") => {
                let valid_key = match cred_obj.get("key") {
                    None | Some(Value::String(_)) => true,
                    Some(_) => false,
                };
                let valid_env = match cred_obj.get("env") {
                    None => true,
                    Some(Value::Object(env_map)) => env_map.values().all(|v| v.is_string()),
                    Some(_) => false,
                };
                if !valid_key || !valid_env {
                    return Err(Error::msg(format!(
                        "Invalid auth.json credential for provider \"{provider_id}\""
                    )));
                }
                let cred: Credential = serde_json::from_value(val.clone())
                    .map_err(|_| Error::msg(format!("Invalid auth.json credential for provider \"{provider_id}\"")))?;
                data.insert(provider_id.clone(), cred);
            }
            Some("oauth") => {
                let valid = cred_obj.get("access").is_some_and(Value::is_string)
                    && cred_obj.get("refresh").is_some_and(Value::is_string)
                    && cred_obj
                        .get("expires")
                        .and_then(Value::as_f64)
                        .is_some_and(f64::is_finite);
                if !valid {
                    return Err(Error::msg(format!(
                        "Invalid auth.json credential for provider \"{provider_id}\""
                    )));
                }
                let cred: Credential = serde_json::from_value(val.clone())
                    .map_err(|_| Error::msg(format!("Invalid auth.json credential for provider \"{provider_id}\"")))?;
                data.insert(provider_id.clone(), cred);
            }
            _ => {
                return Err(Error::msg(format!(
                    "Invalid auth.json credential for provider \"{provider_id}\""
                )));
            }
        }
    }
    Ok(data)
}

pub struct ReadOnlyAuthStorage {
    auth_path: String,
    data: Mutex<Option<AuthStorageData>>,
}

impl ReadOnlyAuthStorage {
    pub fn new(auth_path: Option<&str>) -> Self {
        let default_path;
        let raw = match auth_path {
            Some(p) => p,
            None => {
                default_path = pi_js::path::join(&[&get_agent_dir(), "auth.json"]);
                &default_path
            }
        };
        Self {
            auth_path: normalize_path(raw, Default::default()),
            data: Mutex::new(None),
        }
    }

    fn load(&self) -> Result<AuthStorageData> {
        {
            let guard = self.data.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(ref existing) = *guard {
                return Ok(existing.clone());
            }
        }

        let content = match pi_js::fs::read_to_string(&self.auth_path) {
            Ok(c) => c,
            Err(err) if err.code() == Some("ENOENT") => {
                let empty = AuthStorageData::new();
                *self.data.lock().unwrap_or_else(PoisonError::into_inner) = Some(empty.clone());
                return Ok(empty);
            }
            Err(err) => {
                return Err(Error::msg(format!("Failed to read auth.json: {}", err.message())));
            }
        };

        let parsed = match parse_auth_storage_object(&content) {
            Ok(d) => d,
            Err(err) => {
                let msg = err.message().to_owned();
                if msg.starts_with("Invalid auth.json") {
                    return Err(err);
                }
                return Err(Error::msg(format!("Failed to read auth.json: {msg}")));
            }
        };

        *self.data.lock().unwrap_or_else(PoisonError::into_inner) = Some(parsed.clone());
        Ok(parsed)
    }
}

#[async_trait::async_trait]
impl CredentialStore for ReadOnlyAuthStorage {
    async fn read(&self, provider_id: &str, options: Option<AuthOperationOptions>) -> Result<Option<Credential>> {
        if let Some(ref sig) = options.as_ref().and_then(|o| o.signal.as_ref()) {
            sig.throw_if_aborted()?;
        }
        let data = self.load()?;
        if let Some(ref sig) = options.as_ref().and_then(|o| o.signal.as_ref()) {
            sig.throw_if_aborted()?;
        }
        let Some(cred) = data.get(provider_id).cloned() else {
            return Ok(None);
        };
        match cred {
            Credential::ApiKey(mut api) => {
                let Some(ref raw_key) = api.key else {
                    return Ok(Some(Credential::ApiKey(api)));
                };
                if raw_key.is_empty() || is_command_config_value(raw_key) {
                    return Ok(Some(Credential::ApiKey(api)));
                }
                api.key = resolve_config_value(raw_key, api.env.as_ref());
                Ok(Some(Credential::ApiKey(api)))
            }
            Credential::OAuth(oauth) => Ok(Some(Credential::OAuth(oauth))),
        }
    }

    async fn list(&self, options: Option<AuthOperationOptions>) -> Result<Vec<CredentialInfo>> {
        if let Some(ref sig) = options.as_ref().and_then(|o| o.signal.as_ref()) {
            sig.throw_if_aborted()?;
        }
        let data = self.load()?;
        let creds = data
            .iter()
            .map(|(provider_id, cred)| CredentialInfo {
                provider_id: provider_id.clone(),
                credential_type: cred.auth_type(),
            })
            .collect();
        if let Some(ref sig) = options.as_ref().and_then(|o| o.signal.as_ref()) {
            sig.throw_if_aborted()?;
        }
        Ok(creds)
    }

    async fn modify(
        &self,
        _provider_id: &str,
        _f: ModifyCredentialFn,
        _options: Option<AuthOperationOptions>,
    ) -> Result<Option<Credential>> {
        Err(Error::msg("Read-only credential storage cannot modify auth.json"))
    }

    async fn delete(&self, _provider_id: &str, _options: Option<AuthOperationOptions>) -> Result<()> {
        Err(Error::msg("Read-only credential storage cannot modify auth.json"))
    }
}

pub struct AuthStorage {
    storage: Arc<dyn AuthStorageBackend>,
    auth_path: Option<String>,
    read_state: Arc<Mutex<AuthFileReadState>>,
}

impl Clone for AuthStorage {
    fn clone(&self) -> Self {
        Self {
            storage: self.storage.clone(),
            auth_path: self.auth_path.clone(),
            read_state: self.read_state.clone(),
        }
    }
}

impl AuthStorage {
    fn new(storage: Arc<dyn AuthStorageBackend>, auth_path: Option<String>) -> Self {
        let read_state = match auth_path.as_deref() {
            Some(path) => shared_read_state_for_path(path),
            None => Arc::new(Mutex::new(AuthFileReadState::new())),
        };
        let instance = Self {
            storage,
            auth_path,
            read_state,
        };

        if let Some(ref path) = instance.auth_path {
            let revision = get_file_revision(path);
            let same_rev = {
                let guard = instance.read_state.lock().unwrap_or_else(PoisonError::into_inner);
                revision.is_some() && revision == guard.revision
            };
            if same_rev {
                return instance;
            }
        }
        instance.reload();
        instance
    }

    pub fn create(auth_path: Option<&str>) -> Self {
        let default_path;
        let raw = match auth_path {
            Some(p) => p,
            None => {
                default_path = pi_js::path::join(&[&get_agent_dir(), "auth.json"]);
                &default_path
            }
        };
        let normalized = normalize_path(raw, Default::default());
        let backend = Arc::new(FileAuthStorageBackend::new(Some(&normalized)));
        Self::new(backend, Some(normalized))
    }

    pub fn from_storage(storage: Arc<dyn AuthStorageBackend>) -> Self {
        Self::new(storage, None)
    }

    pub fn in_memory(data: Option<AuthStorageData>) -> Self {
        let storage = Arc::new(InMemoryAuthStorageBackend::new());
        if let Some(d) = data
            && let Ok(serialized) = pi_js::json::try_stringify_pretty(&d, "  ")
        {
            let _ = storage.with_lock(|_| Ok(LockResult::write((), serialized.clone())));
        }
        Self::from_storage(storage)
    }

    fn parse_storage_data(content: Option<&str>) -> Result<AuthStorageData> {
        let Some(text) = content else {
            return Ok(AuthStorageData::new());
        };
        if text.is_empty() {
            return Ok(AuthStorageData::new());
        }
        let parsed = pi_js::json::parse(strip_bom(text))?;
        let data: AuthStorageData = serde_json::from_value(parsed).map_err(|err| Error::msg(err.to_string()))?;
        Ok(data)
    }

    fn update_read_state(&self, data: AuthStorageData, revision: Option<String>) {
        let mut guard = self.read_state.lock().unwrap_or_else(PoisonError::into_inner);
        guard.data = data;
        guard.revision = revision;
    }

    pub fn reload(&self) {
        let mut content: Option<String> = None;
        let mut revision: Option<String> = None;
        let lock_res = self.storage.with_lock_dyn(&mut |current| {
            content = current.map(str::to_owned);
            revision = self.auth_path.as_deref().and_then(get_file_revision);
            Ok(LockResult::unchanged(Value::Null))
        });
        if lock_res.is_ok()
            && let Ok(data) = Self::parse_storage_data(content.as_deref())
        {
            self.update_read_state(data, revision);
        }
    }

    async fn reload_from_storage_async(
        storage: Arc<dyn AuthStorageBackend>,
        auth_path: Option<String>,
        read_state: Arc<Mutex<AuthFileReadState>>,
        options: Option<AuthOperationOptions>,
    ) -> Result<AuthStorageData> {
        let auth_path_for_rev = auth_path.clone();
        let val = storage
            .with_lock_async_dyn(
                Box::new(move |content| {
                    Box::pin(async move {
                        let data = Self::parse_storage_data(content.as_deref())?;
                        let revision = auth_path_for_rev.as_deref().and_then(get_file_revision);
                        {
                            let mut guard = read_state.lock().unwrap_or_else(PoisonError::into_inner);
                            guard.data = data.clone();
                            guard.revision = revision;
                        }
                        let serialized = serde_json::to_value(data).map_err(|err| Error::msg(err.to_string()))?;
                        Ok(LockResult::unchanged(serialized))
                    })
                }),
                options,
            )
            .await?;
        serde_json::from_value(val).map_err(|err| Error::msg(err.to_string()))
    }

    async fn read_latest_data(&self, options: Option<&AuthOperationOptions>) -> Result<AuthStorageData> {
        let signal = options.and_then(|o| o.signal.as_ref());
        if let Some(sig) = signal {
            sig.throw_if_aborted()?;
        }

        let Some(ref auth_path) = self.auth_path else {
            let fut =
                Self::reload_from_storage_async(self.storage.clone(), None, self.read_state.clone(), options.cloned());
            let res = fut.await;
            return if signal.is_some() {
                res
            } else {
                Ok(res.unwrap_or_else(|_| {
                    self.read_state
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .data
                        .clone()
                }))
            };
        };

        let revision = get_file_revision(auth_path);
        {
            let guard = self.read_state.lock().unwrap_or_else(PoisonError::into_inner);
            if revision.is_some() && revision == guard.revision {
                return Ok(guard.data.clone());
            }
        }

        use futures::FutureExt as _;
        let reload_arc = {
            let mut guard = self.read_state.lock().unwrap_or_else(PoisonError::into_inner);
            if revision.is_some() && revision == guard.revision {
                return Ok(guard.data.clone());
            }
            if let Some(ref existing) = guard.reload {
                let mut r_guard = existing.lock().unwrap_or_else(PoisonError::into_inner);
                r_guard.readers += 1;
                existing.clone()
            } else {
                let controller = AbortController::new();
                let internal_opts = AuthOperationOptions {
                    signal: Some(controller.signal()),
                };
                let storage = self.storage.clone();
                let path = self.auth_path.clone();
                let rs = self.read_state.clone();
                let boxed_fut: pi_js::BoxFuture<std::result::Result<AuthStorageData, String>> = Box::pin(async move {
                    Self::reload_from_storage_async(storage, path, rs, Some(internal_opts))
                        .await
                        .map_err(|e| e.message().to_owned())
                });
                let shared_fut = boxed_fut.shared();

                let reload = Arc::new(Mutex::new(AuthFileReload {
                    controller,
                    task: tokio::sync::Mutex::new(Some(shared_fut)),
                    readers: 1,
                }));
                guard.reload = Some(reload.clone());
                reload
            }
        };

        let shared = {
            let reload_guard = reload_arc.lock().unwrap_or_else(PoisonError::into_inner);
            let opt_guard = reload_guard.task.try_lock();
            opt_guard.ok().and_then(|g| g.clone())
        };

        let race_res = if let Some(fut) = shared {
            let fut_wrapped = async move { fut.await.map_err(Error::msg) };
            race_with_abort_signal(fut_wrapped, signal).await
        } else {
            Self::reload_from_storage_async(
                self.storage.clone(),
                self.auth_path.clone(),
                self.read_state.clone(),
                options.cloned(),
            )
            .await
        };

        {
            let mut state_guard = self.read_state.lock().unwrap_or_else(PoisonError::into_inner);
            let mut r_guard = reload_arc.lock().unwrap_or_else(PoisonError::into_inner);
            r_guard.readers = r_guard.readers.saturating_sub(1);
            if let Some(ref current) = state_guard.reload
                && Arc::ptr_eq(current, &reload_arc)
            {
                if race_res.is_ok() {
                    state_guard.reload = None;
                } else if r_guard.readers == 0 {
                    state_guard.reload = None;
                    r_guard.controller.abort(None);
                }
            }
        }

        if signal.is_some() {
            race_res
        } else {
            Ok(race_res.unwrap_or_else(|_| {
                self.read_state
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .data
                    .clone()
            }))
        }
    }

    pub async fn modify_with<F, Fut>(
        &self,
        provider_id: &str,
        f: F,
        options: Option<AuthOperationOptions>,
    ) -> Result<Option<Credential>>
    where
        F: FnOnce(Option<Credential>) -> Fut + Send + 'static,
        Fut: Future<Output = Result<Option<Credential>>> + Send + 'static,
    {
        self.modify(provider_id, Box::new(move |cur| Box::pin(f(cur))), options)
            .await
    }
}

#[async_trait::async_trait]
impl CredentialStore for AuthStorage {
    async fn read(&self, provider: &str, options: Option<AuthOperationOptions>) -> Result<Option<Credential>> {
        let data = self.read_latest_data(options.as_ref()).await?;
        if let Some(ref sig) = options.as_ref().and_then(|o| o.signal.as_ref()) {
            sig.throw_if_aborted()?;
        }
        let Some(cred) = data.get(provider).cloned() else {
            return Ok(None);
        };
        match cred {
            Credential::ApiKey(mut api) => {
                let Some(ref raw_key) = api.key else {
                    return Ok(Some(Credential::ApiKey(api)));
                };
                api.key = resolve_config_value(raw_key, api.env.as_ref());
                Ok(Some(Credential::ApiKey(api)))
            }
            Credential::OAuth(oauth) => Ok(Some(Credential::OAuth(oauth))),
        }
    }

    async fn modify(
        &self,
        provider: &str,
        fn_cb: ModifyCredentialFn,
        options: Option<AuthOperationOptions>,
    ) -> Result<Option<Credential>> {
        let initial_data = {
            self.read_state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .data
                .clone()
        };
        let state_holder: Arc<Mutex<(AuthStorageData, Option<String>)>> = Arc::new(Mutex::new((initial_data, None)));
        let holder_clone = state_holder.clone();
        let provider_owned = provider.to_owned();
        let auth_path_clone = self.auth_path.clone();

        let result_val = self
            .storage
            .with_lock_async_dyn(
                Box::new(move |content| {
                    Box::pin(async move {
                        let current_data = Self::parse_storage_data(content.as_deref())?;
                        let current_cred = current_data.get(&provider_owned).cloned();
                        let next_cred = fn_cb(current_cred.clone()).await?;
                        let mut guard = holder_clone.lock().unwrap_or_else(PoisonError::into_inner);
                        if let Some(next) = next_cred {
                            let mut merged = current_data;
                            merged.insert(provider_owned, next.clone());
                            guard.0 = merged.clone();
                            guard.1 = None;
                            let next_json = pi_js::json::try_stringify_pretty(&merged, "  ")?;
                            let res_val =
                                serde_json::to_value(Some(next)).map_err(|err| Error::msg(err.to_string()))?;
                            Ok(LockResult::write(res_val, next_json))
                        } else {
                            guard.0 = current_data;
                            guard.1 = auth_path_clone.as_deref().and_then(get_file_revision);
                            let res_val =
                                serde_json::to_value(current_cred).map_err(|err| Error::msg(err.to_string()))?;
                            Ok(LockResult::unchanged(res_val))
                        }
                    })
                }),
                options,
            )
            .await?;

        let (latest_data, revision) = {
            let guard = state_holder.lock().unwrap_or_else(PoisonError::into_inner);
            (guard.0.clone(), guard.1.clone())
        };
        self.update_read_state(latest_data, revision);
        serde_json::from_value(result_val).map_err(|err| Error::msg(err.to_string()))
    }

    async fn delete(&self, provider: &str, options: Option<AuthOperationOptions>) -> Result<()> {
        let initial_data = {
            self.read_state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .data
                .clone()
        };
        let latest_holder: Arc<Mutex<AuthStorageData>> = Arc::new(Mutex::new(initial_data));
        let holder_clone = latest_holder.clone();
        let provider_owned = provider.to_owned();

        self.storage
            .with_lock_async_dyn(
                Box::new(move |content| {
                    Box::pin(async move {
                        let mut current_data = Self::parse_storage_data(content.as_deref())?;
                        current_data.shift_remove(&provider_owned);
                        *holder_clone.lock().unwrap_or_else(PoisonError::into_inner) = current_data.clone();
                        let next_json = pi_js::json::try_stringify_pretty(&current_data, "  ")?;
                        Ok(LockResult::write(Value::Null, next_json))
                    })
                }),
                options,
            )
            .await?;

        let latest_data = latest_holder.lock().unwrap_or_else(PoisonError::into_inner).clone();
        self.update_read_state(latest_data, None);
        Ok(())
    }

    async fn list(&self, options: Option<AuthOperationOptions>) -> Result<Vec<CredentialInfo>> {
        let data = self.read_latest_data(options.as_ref()).await?;
        if let Some(ref sig) = options.as_ref().and_then(|o| o.signal.as_ref()) {
            sig.throw_if_aborted()?;
        }
        Ok(data
            .iter()
            .map(|(provider_id, cred)| CredentialInfo {
                provider_id: provider_id.clone(),
                credential_type: cred.auth_type(),
            })
            .collect())
    }
}

/// One-off synchronous read of a stored credential from an `auth.json` file,
/// without instantiating a store or resolving configured key values.
pub fn read_stored_credential(provider_id: &str, auth_path: Option<&str>) -> Option<Credential> {
    let default_path;
    let raw = match auth_path {
        Some(p) => p,
        None => {
            default_path = pi_js::path::join(&[&get_agent_dir(), "auth.json"]);
            &default_path
        }
    };
    let normalized = normalize_path(raw, Default::default());
    let content = pi_js::fs::read_to_string(&normalized).ok()?;
    let parsed = pi_js::json::parse(strip_bom(&content)).ok()?;
    let val = parsed.get(provider_id)?;
    serde_json::from_value(val.clone()).ok()
}

pub fn make_oauth_credential(raw: OAuthCredential) -> Credential {
    Credential::OAuth(raw)
}

pub fn make_api_key_credential(key: Option<String>, env: Option<IndexMap<String, String>>) -> Credential {
    Credential::ApiKey(ApiKeyCredential { key, env })
}
