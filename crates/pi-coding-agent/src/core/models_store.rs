//! Port of packages/coding-agent/src/core/models-store.ts

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, LazyLock, Mutex, PoisonError};

use futures::FutureExt as _;
use futures::future::Shared;
use indexmap::IndexMap;
use pi_ai::AuthOperationOptions;
use pi_ai::models_store::{ModelsStore, ModelsStoreEntry, ModelsStoreOperationOptions};
use pi_js::abort::{AbortController, AbortSignal};
use pi_js::{BoxFuture, Error, Result};
use serde_json::Value;

use crate::config::get_agent_dir;
use crate::core::auth_storage::{AuthStorageBackend, FileAuthStorageBackend, LockResult};

type BoxedLockResult<'a, T> = Pin<Box<dyn Future<Output = Result<LockResult<T>>> + Send + 'a>>;
use crate::utils::abort::race_with_abort_signal;
use crate::utils::paths::{get_file_revision, normalize_path};
use crate::utils::text::strip_bom;

type StoredModels = IndexMap<String, ModelsStoreEntry>;

struct ModelsFileReload {
    controller: AbortController,
    task: tokio::sync::Mutex<Option<Shared<BoxFuture<std::result::Result<StoredModels, String>>>>>,
    readers: usize,
}

struct ModelsFileReadState {
    data: StoredModels,
    revision: Option<String>,
    reload: Option<Arc<Mutex<ModelsFileReload>>>,
}

impl ModelsFileReadState {
    fn new() -> Self {
        Self {
            data: StoredModels::new(),
            revision: None,
            reload: None,
        }
    }
}

// Optimize the common path without retaining an unbounded set of custom paths.
static SHARED_MODELS_FILE_READ_STATE: LazyLock<Mutex<Option<(String, Arc<Mutex<ModelsFileReadState>>)>>> =
    LazyLock::new(|| Mutex::new(None));

fn shared_read_state_for_path(path: &str) -> Arc<Mutex<ModelsFileReadState>> {
    let mut guard = SHARED_MODELS_FILE_READ_STATE
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if let Some((ref existing_path, ref state)) = *guard
        && existing_path == path
    {
        return state.clone();
    }
    let state = Arc::new(Mutex::new(ModelsFileReadState::new()));
    if guard.is_none() {
        *guard = Some((path.to_owned(), state.clone()));
    }
    state
}

fn check_aborted(options: Option<&ModelsStoreOperationOptions>) -> Result<()> {
    if let Some(signal) = options.and_then(|o| o.signal.as_ref()) {
        signal.throw_if_aborted()?;
    }
    Ok(())
}

fn auth_opts(signal: Option<AbortSignal>) -> Option<AuthOperationOptions> {
    signal.map(|s| AuthOperationOptions { signal: Some(s) })
}

fn parse_stored_models(content: Option<&str>) -> Result<StoredModels> {
    let Some(raw) = content else {
        return Ok(StoredModels::new());
    };
    let parsed = pi_js::json::parse(strip_bom(raw))?;
    serde_json::from_value(parsed).map_err(|err| Error::msg(err.to_string()))
}

#[derive(Debug, Default)]
pub struct InMemoryCodingAgentModelsStore {
    entries: Mutex<IndexMap<String, ModelsStoreEntry>>,
}

impl InMemoryCodingAgentModelsStore {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(IndexMap::new()),
        }
    }
}

#[async_trait::async_trait]
impl ModelsStore for InMemoryCodingAgentModelsStore {
    async fn read(
        &self,
        provider_id: &str,
        options: Option<ModelsStoreOperationOptions>,
    ) -> Result<Option<ModelsStoreEntry>> {
        check_aborted(options.as_ref())?;
        let guard = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(guard.get(provider_id).cloned())
    }

    async fn write(
        &self,
        provider_id: &str,
        entry: ModelsStoreEntry,
        options: Option<ModelsStoreOperationOptions>,
    ) -> Result<()> {
        check_aborted(options.as_ref())?;
        let mut guard = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        guard.insert(provider_id.to_owned(), entry);
        Ok(())
    }

    async fn delete(&self, provider_id: &str, options: Option<ModelsStoreOperationOptions>) -> Result<()> {
        check_aborted(options.as_ref())?;
        let mut guard = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        guard.shift_remove(provider_id);
        Ok(())
    }
}

/// Locked JSON-backed storage for dynamically refreshed provider catalogs.
pub struct FileModelsStore {
    storage: Arc<dyn AuthStorageBackend>,
    path: String,
    read_state: Arc<Mutex<ModelsFileReadState>>,
}

impl FileModelsStore {
    pub fn new(path: Option<&str>) -> Self {
        let default_path;
        let raw = match path {
            Some(p) => p,
            None => {
                default_path = pi_js::path::join(&[&get_agent_dir(), "models-store.json"]);
                &default_path
            }
        };
        let normalized = normalize_path(raw, Default::default());
        let storage: Arc<dyn AuthStorageBackend> = Arc::new(FileAuthStorageBackend::new(Some(&normalized)));
        let read_state = shared_read_state_for_path(&normalized);
        Self {
            storage,
            path: normalized,
            read_state,
        }
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    fn update_read_state(read_state: &Arc<Mutex<ModelsFileReadState>>, data: StoredModels, revision: Option<String>) {
        let mut guard = read_state.lock().unwrap_or_else(PoisonError::into_inner);
        guard.data = data;
        guard.revision = revision;
    }

    async fn reload_from_storage(
        storage: Arc<dyn AuthStorageBackend>,
        path: String,
        read_state: Arc<Mutex<ModelsFileReadState>>,
        signal: Option<AbortSignal>,
    ) -> Result<StoredModels> {
        let path_for_rev = path.clone();
        let val = storage
            .with_lock_async_dyn(
                Box::new(move |content| -> BoxedLockResult<'static, Value> {
                    Box::pin(async move {
                        let data = parse_stored_models(content.as_deref())?;
                        Self::update_read_state(&read_state, data.clone(), get_file_revision(&path_for_rev));
                        let serialized = serde_json::to_value(data).map_err(|err| Error::msg(err.to_string()))?;
                        Ok(LockResult::unchanged(serialized))
                    })
                }),
                auth_opts(signal),
            )
            .await?;
        serde_json::from_value(val).map_err(|err| Error::msg(err.to_string()))
    }

    async fn read_latest(&self, signal: Option<&AbortSignal>) -> Result<StoredModels> {
        if let Some(sig) = signal {
            sig.throw_if_aborted()?;
        }

        let revision = get_file_revision(&self.path);
        {
            let guard = self.read_state.lock().unwrap_or_else(PoisonError::into_inner);
            if revision.is_some() && revision == guard.revision {
                return Ok(guard.data.clone());
            }
        }

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
                let internal_signal = controller.signal();
                let storage = self.storage.clone();
                let path = self.path.clone();
                let rs = self.read_state.clone();
                let boxed_fut: BoxFuture<std::result::Result<StoredModels, String>> = Box::pin(async move {
                    Self::reload_from_storage(storage, path, rs, Some(internal_signal))
                        .await
                        .map_err(|e| e.to_string())
                });
                let shared_fut = boxed_fut.shared();

                let reload = Arc::new(Mutex::new(ModelsFileReload {
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
            Self::reload_from_storage(
                self.storage.clone(),
                self.path.clone(),
                self.read_state.clone(),
                signal.cloned(),
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

        race_res
    }
}

#[async_trait::async_trait]
impl ModelsStore for FileModelsStore {
    async fn read(
        &self,
        provider_id: &str,
        options: Option<ModelsStoreOperationOptions>,
    ) -> Result<Option<ModelsStoreEntry>> {
        let signal = options.and_then(|o| o.signal);
        let data = self.read_latest(signal.as_ref()).await?;
        if let Some(sig) = signal.as_ref() {
            sig.throw_if_aborted()?;
        }
        Ok(data.get(provider_id).cloned())
    }

    async fn write(
        &self,
        provider_id: &str,
        entry: ModelsStoreEntry,
        options: Option<ModelsStoreOperationOptions>,
    ) -> Result<()> {
        let signal = options.and_then(|o| o.signal);
        let pid = provider_id.to_owned();
        let latest_slot: Arc<Mutex<Option<StoredModels>>> = Arc::new(Mutex::new(None));
        let latest_for_lock = latest_slot.clone();
        let storage = self.storage.clone();
        let signal_for_lock = signal.clone();

        race_with_abort_signal(
            async move {
                storage
                    .with_lock_async_dyn(
                        Box::new(move |content| -> BoxedLockResult<'static, Value> {
                            Box::pin(async move {
                                let mut current = parse_stored_models(content.as_deref())?;
                                current.insert(pid, entry);
                                let next = pi_js::json::try_stringify_pretty(&current, "  ")?;
                                *latest_for_lock.lock().unwrap_or_else(PoisonError::into_inner) = Some(current);
                                Ok(LockResult::write(Value::Null, next))
                            })
                        }),
                        auth_opts(signal_for_lock),
                    )
                    .await?;
                Ok(())
            },
            signal.as_ref(),
        )
        .await?;

        let latest = latest_slot.lock().unwrap_or_else(PoisonError::into_inner).take();
        if let Some(data) = latest {
            Self::update_read_state(&self.read_state, data, None);
        }
        Ok(())
    }

    async fn delete(&self, provider_id: &str, options: Option<ModelsStoreOperationOptions>) -> Result<()> {
        let signal = options.and_then(|o| o.signal);
        let pid = provider_id.to_owned();
        let latest_slot: Arc<Mutex<Option<StoredModels>>> = Arc::new(Mutex::new(None));
        let latest_for_lock = latest_slot.clone();
        let storage = self.storage.clone();
        let signal_for_lock = signal.clone();

        race_with_abort_signal(
            async move {
                storage
                    .with_lock_async_dyn(
                        Box::new(move |content| -> BoxedLockResult<'static, Value> {
                            Box::pin(async move {
                                let mut current = parse_stored_models(content.as_deref())?;
                                current.shift_remove(&pid);
                                let next = pi_js::json::try_stringify_pretty(&current, "  ")?;
                                *latest_for_lock.lock().unwrap_or_else(PoisonError::into_inner) = Some(current);
                                Ok(LockResult::write(Value::Null, next))
                            })
                        }),
                        auth_opts(signal_for_lock),
                    )
                    .await?;
                Ok(())
            },
            signal.as_ref(),
        )
        .await?;

        let latest = latest_slot.lock().unwrap_or_else(PoisonError::into_inner).take();
        if let Some(data) = latest {
            Self::update_read_state(&self.read_state, data, None);
        }
        Ok(())
    }
}
