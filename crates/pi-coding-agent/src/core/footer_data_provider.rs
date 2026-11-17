//! Port of packages/coding-agent/src/core/footer-data-provider.ts

use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Duration;

use indexmap::IndexMap;
use pi_js::Unsubscribe;
use pi_js::fs::{exists, read_to_string, stat};
use pi_js::path::{dirname, join, resolve};

use crate::utils::child_process::{SpawnProcessOptions, spawn_process, spawn_process_sync, wait_for_child_process};
use crate::utils::fs_watch::{FS_WATCH_RETRY_DELAY_MS, FsWatcher, close_watcher, watch_with_error_handler};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitPaths {
    pub repo_dir: String,
    pub common_git_dir: String,
    pub head_path: String,
}

/// Find git metadata paths by walking up from `cwd`.
/// Handles both regular git repos (`.git` is a directory) and worktrees (`.git` is a file).
pub fn find_git_paths(cwd: &str) -> Option<GitPaths> {
    let mut dir = cwd.to_owned();
    loop {
        let git_path = join(&[&dir, ".git"]);
        if exists(&git_path) {
            let Ok(stats) = stat(&git_path) else {
                return None;
            };
            if stats.is_file() {
                let Ok(content) = read_to_string(&git_path) else {
                    return None;
                };
                let trimmed = content.trim();
                if let Some(rest) = trimmed.strip_prefix("gitdir: ") {
                    let git_dir = resolve(&[&dir, rest.trim()]);
                    let head_path = join(&[&git_dir, "HEAD"]);
                    if !exists(&head_path) {
                        return None;
                    }
                    let common_dir_path = join(&[&git_dir, "commondir"]);
                    let common_git_dir = if exists(&common_dir_path) {
                        match read_to_string(&common_dir_path) {
                            Ok(cd) => resolve(&[&git_dir, cd.trim()]),
                            Err(_) => return None,
                        }
                    } else {
                        git_dir
                    };
                    return Some(GitPaths {
                        repo_dir: dir,
                        common_git_dir,
                        head_path,
                    });
                }
            } else if stats.is_directory() {
                let head_path = join(&[&git_path, "HEAD"]);
                if !exists(&head_path) {
                    return None;
                }
                return Some(GitPaths {
                    repo_dir: dir,
                    common_git_dir: git_path,
                    head_path,
                });
            }
        }
        let parent = dirname(&dir);
        if parent == dir {
            return None;
        }
        dir = parent;
    }
}

fn resolve_branch_with_git_sync(repo_dir: &str) -> Option<String> {
    let res = spawn_process_sync(
        "git",
        &["--no-optional-locks", "symbolic-ref", "--quiet", "--short", "HEAD"],
        SpawnProcessOptions {
            cwd: Some(repo_dir.to_owned()),
            stdout_piped: true,
            ..Default::default()
        },
    );
    if res.status != Some(0) {
        return None;
    }
    let branch = res.stdout.trim();
    if branch.is_empty() {
        None
    } else {
        Some(branch.to_owned())
    }
}

async fn resolve_branch_with_git_async(repo_dir: &str) -> Option<String> {
    let child = spawn_process(
        "git",
        &["--no-optional-locks", "symbolic-ref", "--quiet", "--short", "HEAD"],
        SpawnProcessOptions {
            cwd: Some(repo_dir.to_owned()),
            stdout_piped: true,
            ..Default::default()
        },
    )
    .ok()?;
    let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let buf_cb = buf.clone();
    let status = wait_for_child_process(
        child,
        Some(Box::new(move |chunk| {
            buf_cb
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .extend_from_slice(chunk);
        })),
        None,
    )
    .await
    .ok()?;
    if status != Some(0) {
        return None;
    }
    let raw = buf.lock().unwrap_or_else(PoisonError::into_inner);
    let text = String::from_utf8_lossy(&raw);
    let branch = text.trim();
    if branch.is_empty() {
        None
    } else {
        Some(branch.to_owned())
    }
}

fn is_wsl_environment() -> bool {
    pi_js::env::platform() == "linux"
        && (pi_js::env::var("WSL_DISTRO_NAME").is_some_and(|v| !v.is_empty())
            || pi_js::env::var("WSL_INTEROP").is_some_and(|v| !v.is_empty()))
}

fn is_windows_mounted_repo_path(repo_dir: &str) -> bool {
    let bytes = repo_dir.as_bytes();
    bytes.len() >= 6
        && bytes[..5].eq_ignore_ascii_case(b"/mnt/")
        && bytes[5].is_ascii_alphabetic()
        && (bytes.len() == 6 || bytes[6] == b'/')
}

fn should_poll_git_head(repo_dir: &str) -> bool {
    is_wsl_environment() && is_windows_mounted_repo_path(repo_dir)
}

fn resolve_git_branch_sync(git_paths: Option<&GitPaths>) -> Option<String> {
    let gp = git_paths?;
    let content = read_to_string(&gp.head_path).ok()?;
    let trimmed = content.trim();
    if let Some(branch) = trimmed.strip_prefix("ref: refs/heads/") {
        if branch == ".invalid" {
            Some(resolve_branch_with_git_sync(&gp.repo_dir).unwrap_or_else(|| "detached".to_owned()))
        } else {
            Some(branch.to_owned())
        }
    } else {
        Some("detached".to_owned())
    }
}

async fn resolve_git_branch_async(git_paths: Option<&GitPaths>) -> Option<String> {
    let gp = git_paths?;
    let content = read_to_string(&gp.head_path).ok()?;
    let trimmed = content.trim();
    if let Some(branch) = trimmed.strip_prefix("ref: refs/heads/") {
        if branch == ".invalid" {
            Some(
                resolve_branch_with_git_async(&gp.repo_dir)
                    .await
                    .unwrap_or_else(|| "detached".to_owned()),
            )
        } else {
            Some(branch.to_owned())
        }
    } else {
        Some("detached".to_owned())
    }
}

const WATCH_DEBOUNCE_MS: u64 = 500;

/// Read-only view for extensions - excludes mutators and `dispose`.
pub trait ReadonlyFooterDataProvider: Send + Sync {
    fn get_git_branch(&self) -> Option<String>;
    fn get_extension_statuses(&self) -> IndexMap<String, String>;
    fn get_available_provider_count(&self) -> usize;
    fn on_branch_change(&self, callback: Arc<dyn Fn() + Send + Sync>) -> Unsubscribe;
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct FileStamp {
    mtime_ns: i128,
    ctime_ns: i128,
    size: u64,
}

fn read_file_stamp(path: &str) -> Option<FileStamp> {
    let s = stat(path).ok()?;
    Some(FileStamp {
        mtime_ns: s.mtime_ns,
        ctime_ns: s.ctime_ns,
        size: s.size,
    })
}

struct FooterDataState {
    cwd: String,
    extension_statuses: IndexMap<String, String>,
    cached_branch: Option<Option<String>>,
    git_paths: Option<GitPaths>,
    head_watcher: Option<Arc<FsWatcher>>,
    reftable_watcher: Option<Arc<FsWatcher>>,
    reftable_tables_list_watcher: Option<Arc<FsWatcher>>,
    poll_tasks: Vec<tokio::task::JoinHandle<()>>,
    branch_change_callbacks: IndexMap<u64, Arc<dyn Fn() + Send + Sync>>,
    next_callback_id: u64,
    available_provider_count: usize,
    refresh_timer: Option<tokio::task::JoinHandle<()>>,
    git_watcher_retry_timer: Option<tokio::task::JoinHandle<()>>,
    runtime_handle: Option<tokio::runtime::Handle>,
    refresh_in_flight: bool,
    refresh_pending: bool,
    disposed: bool,
}

struct FooterDataInner {
    state: Mutex<FooterDataState>,
}

impl Drop for FooterDataInner {
    fn drop(&mut self) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.disposed = true;
        if let Some(handle) = st.refresh_timer.take() {
            handle.abort();
        }
        FooterDataProvider::clear_git_watchers_locked(&mut st);
        st.branch_change_callbacks.clear();
    }
}

/// Provides git branch and extension statuses — data not otherwise accessible to extensions.
#[derive(Clone)]
pub struct FooterDataProvider {
    inner: Arc<FooterDataInner>,
}

impl FooterDataProvider {
    pub fn new(cwd: &str) -> Self {
        let git_paths = find_git_paths(cwd);
        let provider = Self {
            inner: Arc::new(FooterDataInner {
                state: Mutex::new(FooterDataState {
                    cwd: cwd.to_owned(),
                    extension_statuses: IndexMap::new(),
                    cached_branch: None,
                    git_paths,
                    head_watcher: None,
                    reftable_watcher: None,
                    reftable_tables_list_watcher: None,
                    poll_tasks: Vec::new(),
                    branch_change_callbacks: IndexMap::new(),
                    next_callback_id: 1,
                    available_provider_count: 0,
                    refresh_timer: None,
                    git_watcher_retry_timer: None,
                    runtime_handle: tokio::runtime::Handle::try_current().ok(),
                    refresh_in_flight: false,
                    refresh_pending: false,
                    disposed: false,
                }),
            }),
        };
        provider.setup_git_watcher();
        provider
    }

    pub fn get_git_branch(&self) -> Option<String> {
        let mut st = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
        if st.cached_branch.is_none() {
            let resolved = resolve_git_branch_sync(st.git_paths.as_ref());
            st.cached_branch = Some(resolved);
        }
        st.cached_branch.clone().flatten()
    }

    pub fn get_extension_statuses(&self) -> IndexMap<String, String> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extension_statuses
            .clone()
    }

    pub fn on_branch_change(&self, callback: Arc<dyn Fn() + Send + Sync>) -> Unsubscribe {
        let id = {
            let mut st = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
            let id = st.next_callback_id;
            st.next_callback_id += 1;
            st.branch_change_callbacks.insert(id, callback);
            id
        };
        let weak: Weak<FooterDataInner> = Arc::downgrade(&self.inner);
        Unsubscribe::new(move || {
            if let Some(inner) = weak.upgrade() {
                inner
                    .state
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .branch_change_callbacks
                    .shift_remove(&id);
            }
        })
    }

    pub fn set_extension_status(&self, key: &str, text: Option<&str>) {
        let mut st = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
        match text {
            None => {
                st.extension_statuses.shift_remove(key);
            }
            Some(val) => {
                st.extension_statuses.insert(key.to_owned(), val.to_owned());
            }
        }
    }

    pub fn clear_extension_statuses(&self) {
        self.inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extension_statuses
            .clear();
    }

    pub fn get_available_provider_count(&self) -> usize {
        self.inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .available_provider_count
    }

    pub fn set_available_provider_count(&self, count: usize) {
        self.inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .available_provider_count = count;
    }

    pub fn set_cwd(&self, cwd: &str) {
        let callbacks = {
            let mut st = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
            if st.cwd == cwd {
                return;
            }
            st.cwd = cwd.to_owned();
            if let Some(handle) = st.refresh_timer.take() {
                handle.abort();
            }
            Self::clear_git_watchers_locked(&mut st);
            st.cached_branch = None;
            st.git_paths = find_git_paths(cwd);
            st.branch_change_callbacks.values().cloned().collect::<Vec<_>>()
        };
        self.setup_git_watcher();
        for cb in callbacks {
            cb();
        }
    }

    pub fn dispose(&self) {
        let mut st = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.disposed = true;
        if let Some(handle) = st.refresh_timer.take() {
            handle.abort();
        }
        Self::clear_git_watchers_locked(&mut st);
        st.branch_change_callbacks.clear();
    }

    fn notify_branch_change(&self) {
        let callbacks: Vec<Arc<dyn Fn() + Send + Sync>> = self
            .inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .branch_change_callbacks
            .values()
            .cloned()
            .collect();
        for cb in callbacks {
            cb();
        }
    }

    fn clear_git_watchers_locked(st: &mut FooterDataState) {
        close_watcher(st.head_watcher.as_deref());
        st.head_watcher = None;
        close_watcher(st.reftable_watcher.as_deref());
        st.reftable_watcher = None;
        close_watcher(st.reftable_tables_list_watcher.as_deref());
        st.reftable_tables_list_watcher = None;
        for task in st.poll_tasks.drain(..) {
            task.abort();
        }
        if let Some(handle) = st.git_watcher_retry_timer.take() {
            handle.abort();
        }
    }

    fn schedule_refresh(&self) {
        let mut st = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
        if st.disposed || st.refresh_timer.is_some() {
            return;
        }
        if st.refresh_in_flight {
            st.refresh_pending = true;
            return;
        }
        let handle = match tokio::runtime::Handle::try_current() {
            Ok(h) => {
                if st.runtime_handle.is_none() {
                    st.runtime_handle = Some(h.clone());
                }
                h
            }
            Err(_) => match st.runtime_handle.clone() {
                Some(h) => h,
                None => return,
            },
        };
        let weak = Arc::downgrade(&self.inner);
        st.refresh_timer = Some(handle.spawn(async move {
            tokio::time::sleep(Duration::from_millis(WATCH_DEBOUNCE_MS)).await;
            let Some(inner) = weak.upgrade() else {
                return;
            };
            {
                let mut st = inner.state.lock().unwrap_or_else(PoisonError::into_inner);
                st.refresh_timer = None;
            }
            FooterDataProvider { inner }.refresh_git_branch_async().await;
        }));
    }

    async fn refresh_git_branch_async(&self) {
        let git_paths = {
            let mut st = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
            if st.disposed {
                return;
            }
            if st.refresh_in_flight {
                st.refresh_pending = true;
                return;
            }
            st.refresh_in_flight = true;
            st.git_paths.clone()
        };

        let next_branch = resolve_git_branch_async(git_paths.as_ref()).await;

        let (should_notify, should_reschedule) = {
            let mut st = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
            st.refresh_in_flight = false;
            if st.disposed {
                (false, false)
            } else {
                let changed = if let Some(ref prev) = st.cached_branch {
                    *prev != next_branch
                } else {
                    false
                };
                st.cached_branch = Some(next_branch);
                let pending = st.refresh_pending;
                if pending {
                    st.refresh_pending = false;
                }
                (changed, !changed && pending)
            }
        };

        if should_notify {
            self.notify_branch_change();
        } else if should_reschedule {
            self.schedule_refresh();
        }
    }

    fn schedule_git_watcher_retry(&self) {
        let mut st = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
        if st.disposed || st.git_watcher_retry_timer.is_some() {
            return;
        }
        let handle = match tokio::runtime::Handle::try_current() {
            Ok(h) => {
                if st.runtime_handle.is_none() {
                    st.runtime_handle = Some(h.clone());
                }
                h
            }
            Err(_) => match st.runtime_handle.clone() {
                Some(h) => h,
                None => return,
            },
        };
        let weak = Arc::downgrade(&self.inner);
        st.git_watcher_retry_timer = Some(handle.spawn(async move {
            tokio::time::sleep(Duration::from_millis(FS_WATCH_RETRY_DELAY_MS)).await;
            let Some(inner) = weak.upgrade() else {
                return;
            };
            {
                let mut st = inner.state.lock().unwrap_or_else(PoisonError::into_inner);
                st.git_watcher_retry_timer = None;
            }
            FooterDataProvider { inner }.setup_git_watcher();
        }));
    }

    fn handle_git_watcher_error(&self) {
        {
            let mut st = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
            Self::clear_git_watchers_locked(&mut st);
        }
        self.schedule_git_watcher_retry();
    }

    fn spawn_file_poll(
        &self,
        handle: &tokio::runtime::Handle,
        path: String,
        interval_ms: u64,
    ) -> tokio::task::JoinHandle<()> {
        let mut prev = read_file_stamp(&path);
        let weak = Arc::downgrade(&self.inner);
        handle.spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(interval_ms)).await;
                let Some(inner) = weak.upgrade() else {
                    break;
                };
                let disposed = inner.state.lock().unwrap_or_else(PoisonError::into_inner).disposed;
                if disposed {
                    break;
                }
                let curr = read_file_stamp(&path);
                if curr != prev {
                    prev = curr;
                    FooterDataProvider { inner }.schedule_refresh();
                }
            }
        })
    }

    fn setup_git_watcher(&self) {
        let (git_paths, runtime_handle) = {
            let mut st = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
            Self::clear_git_watchers_locked(&mut st);
            if st.disposed {
                return;
            }
            if st.runtime_handle.is_none()
                && let Ok(h) = tokio::runtime::Handle::try_current()
            {
                st.runtime_handle = Some(h);
            }
            match st.git_paths.clone() {
                Some(gp) => (gp, st.runtime_handle.clone()),
                None => return,
            }
        };

        let poll_git_head = should_poll_git_head(&git_paths.repo_dir);
        let head_dir = dirname(&git_paths.head_path);
        let weak_ev = Arc::downgrade(&self.inner);
        let weak_err = Arc::downgrade(&self.inner);
        let head_watcher = watch_with_error_handler(
            &head_dir,
            move |_event_type, filename| {
                if (filename.is_empty() || filename == "HEAD")
                    && let Some(inner) = weak_ev.upgrade()
                {
                    FooterDataProvider { inner }.schedule_refresh();
                }
            },
            move || {
                if let Some(inner) = weak_err.upgrade() {
                    FooterDataProvider { inner }.handle_git_watcher_error();
                }
            },
        );

        let mut poll_tasks = Vec::new();
        if poll_git_head && let Some(ref handle) = runtime_handle {
            poll_tasks.push(self.spawn_file_poll(handle, git_paths.head_path.clone(), 1000));
        }

        if head_watcher.is_none() && !poll_git_head {
            let mut st = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
            st.poll_tasks.extend(poll_tasks);
            return;
        }

        let mut reftable_watcher = None;
        let mut reftable_tables_list_watcher = None;

        let reftable_dir = join(&[&git_paths.common_git_dir, "reftable"]);
        if exists(&reftable_dir) {
            let weak_rt_ev = Arc::downgrade(&self.inner);
            let weak_rt_err = Arc::downgrade(&self.inner);
            reftable_watcher = watch_with_error_handler(
                &reftable_dir,
                move |_ev, _fn| {
                    if let Some(inner) = weak_rt_ev.upgrade() {
                        FooterDataProvider { inner }.schedule_refresh();
                    }
                },
                move || {
                    if let Some(inner) = weak_rt_err.upgrade() {
                        FooterDataProvider { inner }.handle_git_watcher_error();
                    }
                },
            );
            if reftable_watcher.is_some() {
                let tables_list_path = join(&[&reftable_dir, "tables.list"]);
                if exists(&tables_list_path) {
                    let weak_tl_ev = Arc::downgrade(&self.inner);
                    let weak_tl_err = Arc::downgrade(&self.inner);
                    reftable_tables_list_watcher = watch_with_error_handler(
                        &tables_list_path,
                        move |_ev, _fn| {
                            if let Some(inner) = weak_tl_ev.upgrade() {
                                FooterDataProvider { inner }.schedule_refresh();
                            }
                        },
                        move || {
                            if let Some(inner) = weak_tl_err.upgrade() {
                                FooterDataProvider { inner }.handle_git_watcher_error();
                            }
                        },
                    );
                    if reftable_tables_list_watcher.is_some()
                        && let Some(ref handle) = runtime_handle
                    {
                        poll_tasks.push(self.spawn_file_poll(handle, tables_list_path, 250));
                    }
                }
            }
        }

        let mut st = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.head_watcher = head_watcher;
        st.reftable_watcher = reftable_watcher;
        st.reftable_tables_list_watcher = reftable_tables_list_watcher;
        st.poll_tasks = poll_tasks;
    }
}

impl ReadonlyFooterDataProvider for FooterDataProvider {
    fn get_git_branch(&self) -> Option<String> {
        FooterDataProvider::get_git_branch(self)
    }

    fn get_extension_statuses(&self) -> IndexMap<String, String> {
        FooterDataProvider::get_extension_statuses(self)
    }

    fn get_available_provider_count(&self) -> usize {
        FooterDataProvider::get_available_provider_count(self)
    }

    fn on_branch_change(&self, callback: Arc<dyn Fn() + Send + Sync>) -> Unsubscribe {
        FooterDataProvider::on_branch_change(self, callback)
    }
}
