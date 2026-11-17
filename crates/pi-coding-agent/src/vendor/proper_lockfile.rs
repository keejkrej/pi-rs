//! Port of vendor proper-lockfile@4.1.2 (PORTING.md §2.6, §13.1).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use indexmap::IndexMap;
use pi_js::{Error, Result};

#[derive(Clone, Debug, Default)]
pub struct RetryOptions {
    pub retries: u32,
    pub factor: f64,
    pub min_timeout: u64,
    pub max_timeout: u64,
    pub randomize: bool,
}

impl RetryOptions {
    pub fn with_count(retries: u32) -> Self {
        Self {
            retries,
            factor: 2.0,
            min_timeout: 1000,
            max_timeout: u64::MAX,
            randomize: false,
        }
    }
}

#[derive(Clone, Default)]
pub struct LockOptions {
    pub stale: Option<u64>,
    pub update: Option<u64>,
    pub realpath: Option<bool>,
    pub retries: Option<RetryOptions>,
    pub lockfile_path: Option<String>,
    pub on_compromised: Option<Arc<dyn Fn(Error) + Send + Sync>>,
}

struct LockEntry {
    lockfile_path: String,
    mtime_ms: i64,
    precision_s: bool,
    stale: u64,
    update: u64,
    last_update_ms: i64,
    released: Arc<AtomicBool>,
    timer: Option<pi_js::time::Timeout>,
    on_compromised: Arc<dyn Fn(Error) + Send + Sync>,
}

static LOCKS: LazyLock<Mutex<IndexMap<String, Arc<Mutex<LockEntry>>>>> = LazyLock::new(|| Mutex::new(IndexMap::new()));

pub struct LockRelease {
    canonical_file: String,
    lockfile_path: String,
    released: Arc<AtomicBool>,
}

impl LockRelease {
    pub async fn release(&self) -> Result<()> {
        if self.released.swap(true, Ordering::AcqRel) {
            return Err(Error::js_with_code("Error", "Lock is already released", "ERELEASED"));
        }
        unlock_internal(&self.canonical_file, &self.lockfile_path)
    }

    pub fn release_sync(&self) -> Result<()> {
        if self.released.swap(true, Ordering::AcqRel) {
            return Err(Error::js_with_code("Error", "Lock is already released", "ERELEASED"));
        }
        unlock_internal(&self.canonical_file, &self.lockfile_path)
    }
}

fn default_on_compromised() -> Arc<dyn Fn(Error) + Send + Sync> {
    Arc::new(|err| {
        pi_js::console::error(&format!("Lock compromised: {err}"));
    })
}

fn get_lock_file(file: &str, lockfile_path: Option<&str>) -> String {
    match lockfile_path {
        Some(p) if !p.is_empty() => p.to_owned(),
        _ => format!("{file}.lock"),
    }
}

fn resolve_canonical_path(file: &str, realpath: bool) -> Result<String> {
    if !realpath {
        return Ok(pi_js::path::resolve(&[file]));
    }
    pi_js::fs::realpath(file)
}

fn probe_mtime_precision(lock_dir: &str) -> Result<(i64, bool)> {
    let now = pi_js::time::now_ms();
    let target_ms = ((now + 999) / 1000) * 1000 + 5;
    let secs = (target_ms as f64) / 1000.0;
    pi_js::fs::utimes(lock_dir, secs, secs)?;
    let stats = pi_js::fs::stat(lock_dir)?;
    let stat_ms = stats.mtime();
    let precision_s = stat_ms % 1000 == 0;
    Ok((stat_ms, precision_s))
}

fn next_mtime_ms(precision_s: bool) -> i64 {
    let now = pi_js::time::now_ms();
    if precision_s { ((now + 999) / 1000) * 1000 } else { now }
}

fn is_lock_stale(stat: &pi_js::fs::Stats, stale: u64) -> bool {
    stat.mtime() < pi_js::time::now_ms().saturating_sub(stale as i64)
}

fn acquire_lock(file: &str, lockfile_path: &str, stale: u64) -> Result<(i64, bool)> {
    match pi_js::fs::mkdir(lockfile_path, false, None) {
        Ok(()) => match probe_mtime_precision(lockfile_path) {
            Ok(res) => Ok(res),
            Err(err) => {
                let _ = pi_js::fs::rmdir(lockfile_path);
                Err(err)
            }
        },
        Err(err) if err.code() != Some("EEXIST") => Err(err),
        Err(_) => {
            if stale == 0 {
                return Err(Error::js_with_code(
                    "Error",
                    format!("Lock file is already being held ({file})"),
                    "ELOCKED",
                ));
            }
            match pi_js::fs::stat(lockfile_path) {
                Err(stat_err) if stat_err.code() == Some("ENOENT") => acquire_lock(file, lockfile_path, 0),
                Err(stat_err) => Err(stat_err),
                Ok(stat) => {
                    if !is_lock_stale(&stat, stale) {
                        return Err(Error::js_with_code(
                            "Error",
                            format!("Lock file is already being held ({file})"),
                            "ELOCKED",
                        ));
                    }
                    remove_lock_dir(lockfile_path)?;
                    acquire_lock(file, lockfile_path, 0)
                }
            }
        }
    }
}

fn remove_lock_dir(lockfile_path: &str) -> Result<()> {
    match pi_js::fs::rmdir(lockfile_path) {
        Ok(()) => Ok(()),
        Err(err) if err.code() == Some("ENOENT") => Ok(()),
        Err(err) => Err(err),
    }
}

fn schedule_update(canonical_file: String, entry: Arc<Mutex<LockEntry>>) {
    let delay = match entry.lock() {
        Ok(guard) => {
            if guard.released.load(Ordering::Acquire) {
                return;
            }
            guard.update
        }
        Err(_) => return,
    };

    let entry_clone = entry.clone();
    let canonical_clone = canonical_file.clone();
    let timer = pi_js::time::set_timeout(delay, move || {
        let (lockfile_path, expected_mtime, precision_s, stale, last_update_ms, on_comp, released) =
            match entry_clone.lock() {
                Ok(mut guard) => {
                    guard.timer = None;
                    if guard.released.load(Ordering::Acquire) {
                        return;
                    }
                    (
                        guard.lockfile_path.clone(),
                        guard.mtime_ms,
                        guard.precision_s,
                        guard.stale,
                        guard.last_update_ms,
                        guard.on_compromised.clone(),
                        guard.released.clone(),
                    )
                }
                Err(_) => return,
            };

        let over_threshold = last_update_ms + (stale as i64) < pi_js::time::now_ms();
        match pi_js::fs::stat(&lockfile_path) {
            Err(err) => {
                if err.code() == Some("ENOENT") || over_threshold {
                    released.store(true, Ordering::Release);
                    if let Ok(mut locks) = LOCKS.lock() {
                        locks.shift_remove(&canonical_clone);
                    }
                    on_comp(Error::js_with_code("Error", err.message(), "ECOMPROMISED"));
                    return;
                }
                if let Ok(mut guard) = entry_clone.lock() {
                    guard.update = 1000;
                }
                schedule_update(canonical_clone, entry_clone);
            }
            Ok(stat) => {
                if stat.mtime() != expected_mtime {
                    released.store(true, Ordering::Release);
                    if let Ok(mut locks) = LOCKS.lock() {
                        locks.shift_remove(&canonical_clone);
                    }
                    on_comp(Error::js_with_code(
                        "Error",
                        "Unable to update lock within the stale threshold",
                        "ECOMPROMISED",
                    ));
                    return;
                }

                let mtime_ms = next_mtime_ms(precision_s);
                let secs = (mtime_ms as f64) / 1000.0;
                match pi_js::fs::utimes(&lockfile_path, secs, secs) {
                    Err(err) => {
                        if released.load(Ordering::Acquire) {
                            return;
                        }
                        if err.code() == Some("ENOENT") || over_threshold {
                            released.store(true, Ordering::Release);
                            if let Ok(mut locks) = LOCKS.lock() {
                                locks.shift_remove(&canonical_clone);
                            }
                            on_comp(Error::js_with_code("Error", err.message(), "ECOMPROMISED"));
                            return;
                        }
                        if let Ok(mut guard) = entry_clone.lock() {
                            guard.update = 1000;
                        }
                        schedule_update(canonical_clone, entry_clone);
                    }
                    Ok(()) => {
                        if released.load(Ordering::Acquire) {
                            return;
                        }
                        if let Ok(mut guard) = entry_clone.lock() {
                            guard.mtime_ms = mtime_ms;
                            guard.last_update_ms = pi_js::time::now_ms();
                        }
                        schedule_update(canonical_clone, entry_clone);
                    }
                }
            }
        }
    });

    if let Ok(mut guard) = entry.lock() {
        guard.timer = Some(timer);
    }
}

fn register_lock(
    canonical_file: String,
    lockfile_path: String,
    mtime_ms: i64,
    precision_s: bool,
    stale: u64,
    update: u64,
    on_compromised: Arc<dyn Fn(Error) + Send + Sync>,
    start_timer: bool,
) -> LockRelease {
    let released = Arc::new(AtomicBool::new(false));
    let entry = Arc::new(Mutex::new(LockEntry {
        lockfile_path: lockfile_path.clone(),
        mtime_ms,
        precision_s,
        stale,
        update,
        last_update_ms: pi_js::time::now_ms(),
        released: released.clone(),
        timer: None,
        on_compromised,
    }));

    if let Ok(mut locks) = LOCKS.lock() {
        locks.insert(canonical_file.clone(), entry.clone());
    }
    if start_timer {
        schedule_update(canonical_file.clone(), entry);
    }

    LockRelease {
        canonical_file,
        lockfile_path,
        released,
    }
}

fn unlock_internal(canonical_file: &str, _lockfile_path: &str) -> Result<()> {
    let entry = match LOCKS.lock() {
        Ok(mut locks) => locks.shift_remove(canonical_file),
        Err(_) => None,
    };
    let Some(entry) = entry else {
        return Err(Error::js_with_code(
            "Error",
            "Lock is not acquired/owned by you",
            "ENOTACQUIRED",
        ));
    };
    let lockfile_path = match entry.lock() {
        Ok(mut guard) => {
            guard.released.store(true, Ordering::Release);
            if let Some(t) = guard.timer.take() {
                t.clear();
            }
            guard.lockfile_path.clone()
        }
        Err(_) => {
            return Err(Error::msg("Lock entry poisoned"));
        }
    };
    remove_lock_dir(&lockfile_path)
}

fn normalized_stale_update(options: &LockOptions) -> (u64, u64) {
    let stale = options.stale.unwrap_or(10_000).max(2000);
    let raw_update = options.update.unwrap_or(stale / 2);
    let update = raw_update.min(stale / 2).max(1000);
    (stale, update)
}

pub async fn lock(file: &str, options: LockOptions) -> Result<LockRelease> {
    let realpath = options.realpath.unwrap_or(true);
    let canonical = resolve_canonical_path(file, realpath)?;
    let lockfile_path = get_lock_file(&canonical, options.lockfile_path.as_deref());
    let (stale, update) = normalized_stale_update(&options);
    let retries = options.retries.clone().unwrap_or_default();
    let on_comp = options.on_compromised.clone().unwrap_or_else(default_on_compromised);

    let mut attempt = 0u32;
    loop {
        match acquire_lock(&canonical, &lockfile_path, stale) {
            Ok((mtime_ms, precision_s)) => {
                return Ok(register_lock(
                    canonical,
                    lockfile_path,
                    mtime_ms,
                    precision_s,
                    stale,
                    update,
                    on_comp,
                    true,
                ));
            }
            Err(_err) if attempt < retries.retries => {
                attempt += 1;
                let factor = if retries.factor > 0.0 { retries.factor } else { 2.0 };
                let min_t = if retries.min_timeout > 0 {
                    retries.min_timeout
                } else {
                    1000
                } as f64;
                let max_t = if retries.max_timeout > 0 {
                    retries.max_timeout
                } else {
                    u64::MAX
                } as f64;
                let rand_factor = if retries.randomize {
                    pi_js::crypto::math_random() + 1.0
                } else {
                    1.0
                };
                let delay = (rand_factor * min_t * factor.powi((attempt - 1) as i32))
                    .round()
                    .min(max_t) as u64;
                pi_js::time::sleep(delay, None).await?;
            }
            Err(err) => return Err(err),
        }
    }
}

pub fn lock_sync(file: &str, options: LockOptions) -> Result<LockRelease> {
    let realpath = options.realpath.unwrap_or(true);
    let canonical = resolve_canonical_path(file, realpath)?;
    let lockfile_path = get_lock_file(&canonical, options.lockfile_path.as_deref());
    let (stale, update) = normalized_stale_update(&options);
    let on_comp = options.on_compromised.clone().unwrap_or_else(default_on_compromised);
    let (mtime_ms, precision_s) = acquire_lock(&canonical, &lockfile_path, stale)?;
    Ok(register_lock(
        canonical,
        lockfile_path,
        mtime_ms,
        precision_s,
        stale,
        update,
        on_comp,
        false,
    ))
}

pub async fn unlock(file: &str, options: LockOptions) -> Result<()> {
    unlock_sync(file, options)
}

pub fn unlock_sync(file: &str, options: LockOptions) -> Result<()> {
    let realpath = options.realpath.unwrap_or(true);
    let canonical = resolve_canonical_path(file, realpath)?;
    let lockfile_path = get_lock_file(&canonical, options.lockfile_path.as_deref());
    unlock_internal(&canonical, &lockfile_path)
}

pub async fn check(file: &str, options: LockOptions) -> Result<bool> {
    check_sync(file, options)
}

pub fn check_sync(file: &str, options: LockOptions) -> Result<bool> {
    let realpath = options.realpath.unwrap_or(true);
    let canonical = resolve_canonical_path(file, realpath)?;
    let stale = options.stale.unwrap_or(10_000).max(2000);
    let lockfile_path = get_lock_file(&canonical, options.lockfile_path.as_deref());
    match pi_js::fs::stat(&lockfile_path) {
        Err(err) if err.code() == Some("ENOENT") => Ok(false),
        Err(err) => Err(err),
        Ok(stat) => Ok(!is_lock_stale(&stat, stale)),
    }
}
