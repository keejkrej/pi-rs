//! Prompt cache warmer keeping live caches alive across idle/streaming pauses.
//!
//! Port of `packages/coding-agent/src/core/cache-warmer.ts`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use pi_agent_core::AgentMessage;
use pi_ai::models::ModelsSimpleStreamOptions;
use pi_ai::utils::provider_env::get_provider_env_value;
use pi_ai::{
    AnyModel, AssistantMessageEventStream, CacheRetention, Context, Message, Model, SimpleStreamOptions, StopReason,
    Usage, calculate_cost,
};
use pi_js::abort::AbortController;
use pi_js::error::Result;
use serde::{Deserialize, Serialize};
use tokio::task::AbortHandle;

use crate::core::session_manager::{SessionEntry, SessionManager, UsageEntry};
use crate::core::settings_manager::CacheWarmingMode;

/// Streaming warming never continues past this long after the real request that started it.
const MAX_WARMING_AGE_MS: i64 = 60 * 60_000;
/// Idle warming uses a shorter horizon because continuation estimates become less reliable with age.
const MAX_IDLE_WARMING_AGE_MS: i64 = 30 * 60_000;
/// A refresh is sent only when it is expected to save at least this many dollars.
const CACHE_WARMING_MINIMUM_EXPECTED_SAVINGS: f64 = 0.05;
/// Chance that a real request arrives before the cache entry expires while idle.
const IDLE_CONTINUATION_PROBABILITY: f64 = 0.15;

/// Refresh at 90% of the TTL while preserving at least ten seconds of margin.
pub fn get_cache_warming_delay_ms(ttl_ms: i64) -> Option<i64> {
    if ttl_ms <= 10_000 {
        return None;
    }
    let candidate = (((ttl_ms as f64) * 0.9).floor() as i64).min(ttl_ms - 10_000);
    Some(candidate.max(1))
}

/// Lifetime of the prompt cache entry a request writes, from the model's `prompt_cache`
/// tier for the retention the request used.
pub fn get_prompt_cache_ttl_ms(model: &Model, options: Option<&SimpleStreamOptions>) -> Option<i64> {
    let retention = match options.and_then(|o| o.base.cache_retention) {
        Some(r) => r,
        None => {
            let env_val = get_provider_env_value("PI_CACHE_RETENTION", options.and_then(|o| o.base.env.as_ref()));
            if env_val.as_deref() == Some("long") {
                CacheRetention::Long
            } else {
                CacheRetention::Short
            }
        }
    };
    let seconds = match retention {
        CacheRetention::None => return None,
        CacheRetention::Short => model.prompt_cache.as_ref()?.short?,
        CacheRetention::Long => model.prompt_cache.as_ref()?.long?,
    };
    Some(seconds * 1000)
}

/// Whether replaying the request with a one-token output cap leaves its cache entry untouched.
pub fn is_replayable(model: &Model, options: Option<&SimpleStreamOptions>) -> bool {
    if options.and_then(|o| o.reasoning).is_none() || model.api != "anthropic-messages" {
        return true;
    }
    model
        .compat
        .as_ref()
        .and_then(|c| c.as_anthropic_messages())
        .and_then(|c| c.force_adaptive_thinking)
        == Some(true)
}

fn last_prompt_tokens(entries: &[SessionEntry]) -> i64 {
    for entry in entries.iter().rev() {
        if let SessionEntry::Message(msg_entry) = entry
            && let AgentMessage::Message(Message::Assistant(assistant)) = &msg_entry.message
        {
            let usage = &assistant.usage;
            return usage.input + usage.cache_read + usage.cache_write;
        }
    }
    0
}

fn price(model: &Model, input: i64, output: i64, cache_read: i64, cache_write: i64) -> f64 {
    let mut usage = Usage {
        input,
        output,
        cache_read,
        cache_write,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: 0,
        cost: Default::default(),
    };
    calculate_cost(&AnyModel::Chat(model.clone()), &mut usage).total
}

/// Action chosen for a scheduled cache refresh.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CacheWarmingAction {
    Warm,
    Stop,
}

/// Cache warming lifecycle phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CacheWarmingPhase {
    Streaming,
    Idle,
}

/// Inputs and outcome of one warm-or-stop decision, as shown by `/session`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheWarmingDecision {
    pub phase: CacheWarmingPhase,
    pub warm_cost: f64,
    pub miss_cost: f64,
    pub continuation_probability: f64,
    pub expected_savings: f64,
    pub economics_available: bool,
    pub action: CacheWarmingAction,
}

/// Fired before each refresh with pi's decision filled in.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheWarmingDecisionEvent {
    pub r#type: &'static str,
    pub warm_cost: f64,
    pub miss_cost: f64,
    pub continuation_probability: f64,
    pub action: CacheWarmingAction,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CacheWarmingRunState {
    Inactive,
    Scheduled,
    Refreshing,
}

/// Current snapshot returned by [`CacheWarmer::status`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheWarmingStatus {
    pub state: CacheWarmingRunState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_warm_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<CacheWarmingDecision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extension_override: Option<bool>,
}

/// The request whose prompt cache entry should be kept warm, as it was sent.
#[derive(Clone)]
pub struct CacheWarmRequest {
    pub model: Model,
    pub context: Context,
    pub options: ModelsSimpleStreamOptions,
}

/// Minimal `ModelRuntime` stream hook used by [`CacheWarmer`].
pub trait CacheWarmerStreamSource: Send + Sync {
    fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: Option<ModelsSimpleStreamOptions>,
    ) -> AssistantMessageEventStream;
}

impl CacheWarmerStreamSource for crate::core::model_runtime::ModelRuntime {
    fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: Option<ModelsSimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        crate::core::model_runtime::ModelRuntime::stream_simple(self, model, context, options)
    }
}

impl CacheWarmerStreamSource for crate::core::model_registry::ModelRegistry {
    fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: Option<ModelsSimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        crate::core::model_registry::ModelRegistry::stream_simple(self, model, context, options)
    }
}

/// Minimal `SessionManager` hook used by [`CacheWarmer`].
pub trait CacheWarmerSessionStore: Send + Sync {
    fn append_usage(
        &self,
        kind: &str,
        provider: &str,
        model: &str,
        usage: Usage,
        note: Option<&str>,
    ) -> Result<UsageEntry>;

    fn get_branch(&self) -> Vec<SessionEntry>;
}

impl CacheWarmerSessionStore for SessionManager {
    fn append_usage(
        &self,
        kind: &str,
        provider: &str,
        model: &str,
        usage: Usage,
        note: Option<&str>,
    ) -> Result<UsageEntry> {
        SessionManager::append_usage(self, kind, provider, model, usage, note)
    }

    fn get_branch(&self) -> Vec<SessionEntry> {
        SessionManager::get_branch(self, None)
    }
}

/// Extension hook allowing custom override of `CacheWarmingDecisionEvent`.
#[async_trait]
pub trait CacheWarmingDecider: Send + Sync {
    async fn decide(&self, event: CacheWarmingDecisionEvent) -> Result<CacheWarmingAction>;
}

struct DefaultDecider;

#[async_trait]
impl CacheWarmingDecider for DefaultDecider {
    async fn decide(&self, event: CacheWarmingDecisionEvent) -> Result<CacheWarmingAction> {
        Ok(event.action)
    }
}

struct ActiveRun {
    run_id: u64,
    request: CacheWarmRequest,
    is_current: Arc<dyn Fn() -> bool + Send + Sync>,
    ttl_ms: i64,
    delay_ms: i64,
    refresh_deadline_at: i64,
    started_at: i64,
    controller: AbortController,
    phase: CacheWarmingPhase,
    next_warm_at: i64,
    extension_override: bool,
    scheduled: bool,
    timer_abort: Option<AbortHandle>,
    fallback_timer: Option<pi_js::time::Timeout>,
}

struct WarmerInner {
    next_run_id: u64,
    run: Option<ActiveRun>,
    inactive: CacheWarmingStatus,
    models: Arc<dyn CacheWarmerStreamSource>,
    session_manager: Arc<dyn CacheWarmerSessionStore>,
    get_mode: Arc<dyn Fn() -> CacheWarmingMode + Send + Sync>,
    decide: Arc<dyn CacheWarmingDecider>,
    on_warmed: Option<Arc<dyn Fn(&UsageEntry) + Send + Sync>>,
}

/// Keeps one prompt cache entry alive by re-sending its request with a
/// one-token output cap before the entry expires.
#[derive(Clone)]
pub struct CacheWarmer {
    inner: Arc<Mutex<WarmerInner>>,
}

impl Drop for CacheWarmer {
    fn drop(&mut self) {
        if Arc::strong_count(&self.inner) <= 1 {
            clear_active_run(&self.inner);
        }
    }
}

fn clear_active_run(inner: &Arc<Mutex<WarmerInner>>) {
    let cleared = {
        let mut g = inner.lock().unwrap_or_else(|e| e.into_inner());
        g.run.take()
    };
    if let Some(run) = cleared {
        if let Some(handle) = run.timer_abort {
            handle.abort();
        }
        if let Some(timer) = run.fallback_timer {
            timer.clear();
        }
        run.controller.abort(None);
    }
}

fn stop_with_reason(
    inner: &Arc<Mutex<WarmerInner>>,
    reason: impl Into<String>,
    decision: Option<CacheWarmingDecision>,
    extension_override: Option<bool>,
) {
    let cleared = {
        let mut g = inner.lock().unwrap_or_else(|e| e.into_inner());
        g.inactive = CacheWarmingStatus {
            state: CacheWarmingRunState::Inactive,
            reason: Some(reason.into()),
            next_warm_at: None,
            decision,
            extension_override,
        };
        g.run.take()
    };
    if let Some(run) = cleared {
        if let Some(handle) = run.timer_abort {
            handle.abort();
        }
        if let Some(timer) = run.fallback_timer {
            timer.clear();
        }
        run.controller.abort(None);
    }
}

fn mode_stop_reason(mode: CacheWarmingMode, phase: CacheWarmingPhase) -> Option<&'static str> {
    if mode == CacheWarmingMode::Off {
        return Some("cache warming disabled");
    }
    if mode == CacheWarmingMode::Streaming && phase == CacheWarmingPhase::Idle {
        return Some("agent run settled");
    }
    None
}

fn evaluate_decision(
    model: &Model,
    phase: CacheWarmingPhase,
    session_manager: &dyn CacheWarmerSessionStore,
) -> CacheWarmingDecision {
    let prompt_tokens = last_prompt_tokens(&session_manager.get_branch());
    let cache_hit_cost = price(model, 0, 0, prompt_tokens, 0);
    let cache_miss_cost = if model.cost.cache_write > 0.0 {
        price(model, 0, 0, 0, prompt_tokens)
    } else {
        price(model, prompt_tokens, 0, 0, 0)
    };
    let warm_cost = price(model, 0, 1, prompt_tokens, 0);
    let miss_cost = (cache_miss_cost - cache_hit_cost).max(0.0);
    let continuation_probability = match phase {
        CacheWarmingPhase::Idle => IDLE_CONTINUATION_PROBABILITY,
        CacheWarmingPhase::Streaming => 1.0,
    };
    let economics_available = prompt_tokens > 0 && (cache_hit_cost > 0.0 || cache_miss_cost > 0.0);
    let expected_savings = continuation_probability * miss_cost - warm_cost;
    let action = if expected_savings >= CACHE_WARMING_MINIMUM_EXPECTED_SAVINGS {
        CacheWarmingAction::Warm
    } else {
        CacheWarmingAction::Stop
    };
    CacheWarmingDecision {
        phase,
        warm_cost,
        miss_cost,
        continuation_probability,
        expected_savings,
        economics_available,
        action,
    }
}

fn schedule_run(inner: &Arc<Mutex<WarmerInner>>, run_id: u64) {
    let (next_warm_at, wait_ms, overflow_reason) = {
        let mut g = inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(run) = g.run.as_mut().filter(|r| r.run_id == run_id) else {
            return;
        };
        let now = pi_js::time::now_ms();
        run.extension_override = false;
        run.next_warm_at = now + run.delay_ms;
        run.refresh_deadline_at = run.next_warm_at + ((run.ttl_ms - run.delay_ms) / 2);
        let max_age = match run.phase {
            CacheWarmingPhase::Idle => MAX_IDLE_WARMING_AGE_MS,
            CacheWarmingPhase::Streaming => MAX_WARMING_AGE_MS,
        };
        let deadline = run.started_at + max_age;
        if run.next_warm_at > deadline || now >= deadline {
            let reason = match run.phase {
                CacheWarmingPhase::Idle => "30-minute idle safety limit reached",
                CacheWarmingPhase::Streaming => "one-hour safety limit reached",
            };
            (run.next_warm_at, 0u64, Some(reason))
        } else {
            run.scheduled = true;
            let wait_ms = (run.next_warm_at - now).max(0) as u64;
            (run.next_warm_at, wait_ms, None)
        }
    };

    if let Some(reason) = overflow_reason {
        stop_with_reason(inner, reason, None, None);
        return;
    }

    let _ = next_warm_at;
    let inner_cloned = Arc::clone(inner);
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        let sleep_deadline = tokio::time::Instant::now() + Duration::from_millis(wait_ms);
        let jh = handle.spawn(async move {
            tokio::time::sleep_until(sleep_deadline).await;
            execute_refresh(inner_cloned, run_id).await;
        });
        let abort_handle = jh.abort_handle();
        let mut g = inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(run) = g.run.as_mut().filter(|r| r.run_id == run_id) {
            if let Some(prev) = run.timer_abort.replace(abort_handle) {
                prev.abort();
            }
        } else {
            abort_handle.abort();
        }
    } else {
        let timer = pi_js::time::set_timeout(wait_ms, move || {
            let _ = &inner_cloned;
        });
        let mut g = inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(run) = g.run.as_mut().filter(|r| r.run_id == run_id) {
            run.fallback_timer = Some(timer);
        } else {
            timer.clear();
        }
    }
}

fn validate_active_run(inner: &Arc<Mutex<WarmerInner>>, run_id: u64) -> bool {
    let stop_reason = {
        let g = inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(run) = g.run.as_ref().filter(|r| r.run_id == run_id) else {
            return false;
        };
        let mode = (g.get_mode)();
        if let Some(reason) = mode_stop_reason(mode, run.phase) {
            Some(reason)
        } else if !(run.is_current)() {
            Some("conversation context changed")
        } else {
            None
        }
    };
    if let Some(reason) = stop_reason {
        stop_with_reason(inner, reason, None, None);
        return false;
    }
    true
}

fn check_deadline_missed(inner: &Arc<Mutex<WarmerInner>>, run_id: u64) -> bool {
    let missed = {
        let g = inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(run) = g.run.as_ref().filter(|r| r.run_id == run_id) else {
            return true;
        };
        pi_js::time::now_ms() > run.refresh_deadline_at
    };
    if missed {
        stop_with_reason(inner, "cache refresh deadline missed", None, None);
        return true;
    }
    false
}

async fn execute_refresh(inner: Arc<Mutex<WarmerInner>>, run_id: u64) {
    {
        let mut g = inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(run) = g.run.as_mut().filter(|r| r.run_id == run_id) {
            run.scheduled = false;
            run.timer_abort = None;
            run.fallback_timer = None;
        } else {
            return;
        }
    }

    if !validate_active_run(&inner, run_id) || check_deadline_missed(&inner, run_id) {
        return;
    }

    let (decision, decide, models, session_manager, on_warmed, request, signal) = {
        let g = inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(run) = g.run.as_ref().filter(|r| r.run_id == run_id) else {
            return;
        };
        let dec = evaluate_decision(&run.request.model, run.phase, g.session_manager.as_ref());
        (
            dec,
            Arc::clone(&g.decide),
            Arc::clone(&g.models),
            Arc::clone(&g.session_manager),
            g.on_warmed.clone(),
            run.request.clone(),
            run.controller.signal(),
        )
    };

    let event = CacheWarmingDecisionEvent {
        r#type: "cache_warming_decision",
        warm_cost: decision.warm_cost,
        miss_cost: decision.miss_cost,
        continuation_probability: decision.continuation_probability,
        action: decision.action,
    };
    let action = decide.decide(event).await.unwrap_or(decision.action);

    if !validate_active_run(&inner, run_id) || check_deadline_missed(&inner, run_id) {
        return;
    }

    let extension_override = action != decision.action;
    if action == CacheWarmingAction::Stop {
        let reason = if extension_override {
            "stopped by extension"
        } else if decision.economics_available {
            "expected savings below threshold"
        } else {
            "cache economics unavailable"
        };
        stop_with_reason(&inner, reason, Some(decision), Some(extension_override));
        return;
    }

    {
        let mut g = inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(run) = g.run.as_mut().filter(|r| r.run_id == run_id) {
            run.extension_override = extension_override;
        } else {
            return;
        }
    }

    let mut replay_opts = request.options.clone();
    replay_opts.base.base.max_tokens = Some(1);
    replay_opts.base.base.max_retries = Some(0);
    replay_opts.base.base.signal = Some(signal);

    let stream = models.stream_simple(&request.model, &request.context, Some(replay_opts));
    let message = stream.result().await;

    if !validate_active_run(&inner, run_id) {
        return;
    }
    if message.stop_reason != StopReason::Error && message.stop_reason != StopReason::Aborted {
        let model_id = message.response_model.as_deref().unwrap_or(&message.model);
        let note = if extension_override {
            Some("extension override")
        } else {
            None
        };
        if let Ok(entry) = session_manager.append_usage("cache_warm", &message.provider, model_id, message.usage, note)
            && let Some(cb) = on_warmed
        {
            cb(&entry);
        }
    }

    let still_same_run = {
        let g = inner.lock().unwrap_or_else(|e| e.into_inner());
        g.run.as_ref().is_some_and(|r| r.run_id == run_id)
    };
    if still_same_run {
        schedule_run(&inner, run_id);
    }
}

impl CacheWarmer {
    pub fn new(
        models: Arc<dyn CacheWarmerStreamSource>,
        session_manager: Arc<dyn CacheWarmerSessionStore>,
        get_mode: Arc<dyn Fn() -> CacheWarmingMode + Send + Sync>,
        decide: Option<Arc<dyn CacheWarmingDecider>>,
    ) -> Self {
        Self {
            inner: Arc::new(Mutex::new(WarmerInner {
                next_run_id: 1,
                run: None,
                inactive: CacheWarmingStatus {
                    state: CacheWarmingRunState::Inactive,
                    reason: Some("waiting for first request".to_owned()),
                    next_warm_at: None,
                    decision: None,
                    extension_override: None,
                },
                models,
                session_manager,
                get_mode,
                decide: decide.unwrap_or_else(|| Arc::new(DefaultDecider)),
                on_warmed: None,
            })),
        }
    }

    /// Set callback invoked with each persisted `UsageEntry` after a successful refresh.
    pub fn set_on_warmed(&self, cb: Option<Arc<dyn Fn(&UsageEntry) + Send + Sync>>) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.on_warmed = cb;
    }

    /// Current status snapshot shown in `/session`.
    pub fn status(&self) -> CacheWarmingStatus {
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if (g.get_mode)() == CacheWarmingMode::Off {
            return CacheWarmingStatus {
                state: CacheWarmingRunState::Inactive,
                reason: Some("cache warming disabled".to_owned()),
                next_warm_at: None,
                decision: None,
                extension_override: None,
            };
        }
        let Some(run) = &g.run else {
            return g.inactive.clone();
        };
        if !(run.is_current)() {
            return CacheWarmingStatus {
                state: CacheWarmingRunState::Inactive,
                reason: Some("conversation context changed".to_owned()),
                next_warm_at: None,
                decision: None,
                extension_override: None,
            };
        }
        let decision = evaluate_decision(&run.request.model, run.phase, g.session_manager.as_ref());
        let refreshing = !run.scheduled;
        if !decision.economics_available && !refreshing {
            return CacheWarmingStatus {
                state: CacheWarmingRunState::Inactive,
                reason: Some("cache economics unavailable".to_owned()),
                next_warm_at: None,
                decision: None,
                extension_override: None,
            };
        }
        CacheWarmingStatus {
            state: if refreshing {
                CacheWarmingRunState::Refreshing
            } else {
                CacheWarmingRunState::Scheduled
            },
            reason: None,
            next_warm_at: Some(run.next_warm_at),
            decision: Some(decision),
            extension_override: Some(run.extension_override),
        }
    }

    /// Keep the prompt cache entry written by `request` warm while `is_current` holds.
    pub fn start(&self, request: CacheWarmRequest, is_current: Arc<dyn Fn() -> bool + Send + Sync>) {
        clear_active_run(&self.inner);

        let mode = {
            let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            (g.get_mode)()
        };
        if mode == CacheWarmingMode::Off {
            stop_with_reason(&self.inner, "cache warming disabled", None, None);
            return;
        }
        if !is_replayable(&request.model, Some(&request.options.base)) {
            stop_with_reason(&self.inner, "request cannot be replayed safely", None, None);
            return;
        }
        let Some(ttl_ms) = get_prompt_cache_ttl_ms(&request.model, Some(&request.options.base)) else {
            let reason = if request.options.base.base.cache_retention == Some(CacheRetention::None) {
                "request disabled prompt caching"
            } else {
                "cache lifetime unavailable"
            };
            stop_with_reason(&self.inner, reason, None, None);
            return;
        };
        let Some(delay_ms) = get_cache_warming_delay_ms(ttl_ms) else {
            stop_with_reason(&self.inner, "cache lifetime unavailable", None, None);
            return;
        };

        let run_id = {
            let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let run_id = g.next_run_id;
            g.next_run_id += 1;
            g.run = Some(ActiveRun {
                run_id,
                request,
                is_current,
                ttl_ms,
                delay_ms,
                refresh_deadline_at: 0,
                started_at: pi_js::time::now_ms(),
                controller: AbortController::new(),
                phase: CacheWarmingPhase::Streaming,
                next_warm_at: 0,
                extension_override: false,
                scheduled: false,
                timer_abort: None,
                fallback_timer: None,
            });
            run_id
        };

        schedule_run(&self.inner, run_id);
    }

    pub fn on_agent_settled(&self) {
        let stop_reason = {
            let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let mode = (g.get_mode)();
            let Some(run) = g.run.as_mut() else {
                return;
            };
            if mode == CacheWarmingMode::Streaming {
                Some("agent run settled")
            } else {
                run.phase = CacheWarmingPhase::Idle;
                let deadline = run.started_at + MAX_IDLE_WARMING_AGE_MS;
                if run.next_warm_at > deadline || pi_js::time::now_ms() >= deadline {
                    Some("30-minute idle safety limit reached")
                } else {
                    None
                }
            }
        };
        if let Some(reason) = stop_reason {
            stop_with_reason(&self.inner, reason, None, None);
        }
    }

    /// Reconcile an active run after the persisted warming mode changes.
    pub fn on_mode_changed(&self) {
        let stop_reason = {
            let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let Some(run) = g.run.as_ref() else {
                return;
            };
            let mode = (g.get_mode)();
            mode_stop_reason(mode, run.phase)
        };
        if let Some(reason) = stop_reason {
            stop_with_reason(&self.inner, reason, None, None);
        }
    }

    pub fn cancel(&self) {
        stop_with_reason(&self.inner, "inactive", None, None);
    }

    /// Cancel any pending timer handle and execute the active run's refresh immediately (for deadline tests).
    pub async fn trigger_pending_refresh(&self) {
        let active_id = {
            let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let Some(run) = g.run.as_mut() else {
                return;
            };
            if let Some(handle) = run.timer_abort.take() {
                handle.abort();
            }
            if let Some(timer) = run.fallback_timer.take() {
                timer.clear();
            }
            run.run_id
        };
        execute_refresh(Arc::clone(&self.inner), active_id).await;
    }
}

fn format_dollars(value: f64) -> String {
    if value < 0.0 {
        format!("-${:.3}", value.abs())
    } else {
        format!("${value:.3}")
    }
}

fn format_cache_warming_economics(decision: &CacheWarmingDecision) -> String {
    if !decision.economics_available {
        return "cache economics unavailable".to_owned();
    }
    let probability = (decision.continuation_probability * 100.0).round() as i64;
    let probability_text = match decision.phase {
        CacheWarmingPhase::Streaming => format!("{probability}% continuation probability while agent is running"),
        CacheWarmingPhase::Idle => format!("{probability}% continuation probability"),
    };
    let comparison = match decision.action {
        CacheWarmingAction::Warm => ">=",
        CacheWarmingAction::Stop => "<",
    };
    format!(
        "{probability_text}, expected savings {} {comparison} ${:.3}",
        format_dollars(decision.expected_savings),
        CACHE_WARMING_MINIMUM_EXPECTED_SAVINGS
    )
}

fn format_cache_warming_decision_time(next_warm_at: Option<i64>, now: i64) -> String {
    let Some(next) = next_warm_at.filter(|&t| t > now) else {
        return "Decision now".to_owned();
    };
    let mut remaining_seconds = ((next - now) as f64 / 1000.0).ceil() as i64;
    let hours = remaining_seconds / 3600;
    remaining_seconds %= 3600;
    let minutes = remaining_seconds / 60;
    let seconds = remaining_seconds % 60;
    let mut parts = Vec::new();
    if hours > 0 {
        parts.push(format!("{hours}h"));
    }
    if minutes > 0 {
        parts.push(format!("{minutes}m"));
    }
    if seconds > 0 || parts.is_empty() {
        parts.push(format!("{seconds}s"));
    }
    format!("Decision in {}", parts.join(" "))
}

/// One-line status for `/session`.
pub fn format_cache_warming_status(status: &CacheWarmingStatus, now: Option<i64>) -> String {
    let now_ms = now.unwrap_or_else(pi_js::time::now_ms);
    let Some(decision) = &status.decision else {
        let reason = status.reason.as_deref().unwrap_or("unknown reason");
        return format!("Inactive ({reason})");
    };
    if status.state == CacheWarmingRunState::Inactive
        && !decision.economics_available
        && !status.extension_override.unwrap_or(false)
    {
        let reason = status.reason.as_deref().unwrap_or("unknown reason");
        return format!("Inactive ({reason})");
    }

    let action_str = match decision.action {
        CacheWarmingAction::Warm => "warm",
        CacheWarmingAction::Stop => "stop",
    };
    let details = if status.extension_override.unwrap_or(false) {
        format!("extension override, {}", format_cache_warming_economics(decision))
    } else {
        format!("{} -> {action_str}", format_cache_warming_economics(decision))
    };

    match status.state {
        CacheWarmingRunState::Inactive => format!("Stopped ({details})"),
        CacheWarmingRunState::Refreshing => format!("Warming cache ({details})"),
        CacheWarmingRunState::Scheduled => {
            format!(
                "{} ({details})",
                format_cache_warming_decision_time(status.next_warm_at, now_ms)
            )
        }
    }
}

/// One-line transcript text for persisted cache-warming usage.
pub fn format_cache_warming_usage(entry: &UsageEntry) -> String {
    let note = entry
        .note
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(|n| format!(" ({n})"))
        .unwrap_or_default();
    let formatted = format!("{:.6}", entry.usage.cost.total);
    let cost = trim_cost_trailing_zeros(&formatted);
    format!("Cache warmed{note}: ${cost}")
}

fn trim_cost_trailing_zeros(formatted: &str) -> String {
    let Some((int_part, frac_part)) = formatted.split_once('.') else {
        return formatted.to_owned();
    };
    if frac_part.len() <= 3 {
        return formatted.to_owned();
    }
    let kept = frac_part.trim_end_matches('0');
    let final_frac = if kept.len() < 3 { &frac_part[..3] } else { kept };
    format!("{int_part}.{final_frac}")
}
