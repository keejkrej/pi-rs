//! Port of `packages/coding-agent/test/cache-warmer.test.ts`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use indexmap::IndexMap;
use pi_agent_core::AgentMessage;
use pi_ai::compat::get_model;
use pi_ai::models::{ModelsSimpleStreamOptions, Provider};
use pi_ai::providers::all::builtin_providers;
use pi_ai::{
    ApiStreamOptions, AssistantMessage, AssistantMessageEvent, AssistantMessageEventStream, CacheRetention, Context,
    Message, Model, ModelPromptCache, ProviderAuth, SimpleStreamOptions, StopReason, ThinkingLevel, TranscriptContext,
    Usage, UsageCost,
};
use pi_coding_agent::core::auth_storage::{AuthStorage, AuthStorageData, make_api_key_credential};
use pi_coding_agent::core::cache_warmer::{
    CacheWarmRequest, CacheWarmer, CacheWarmerSessionStore, CacheWarmerStreamSource, CacheWarmingAction,
    CacheWarmingDecider, CacheWarmingDecision, CacheWarmingDecisionEvent, CacheWarmingPhase, CacheWarmingRunState,
    format_cache_warming_status, format_cache_warming_usage, get_cache_warming_delay_ms, get_prompt_cache_ttl_ms,
    is_replayable,
};
use pi_coding_agent::core::event_bus::{EventBus, create_event_bus};
use pi_coding_agent::core::extensions::{
    CacheWarmingDecisionEventResult, ExtensionApi, ExtensionRunner, create_extension_runtime,
    load_extension_from_factory,
};
use pi_coding_agent::core::model_registry::ModelRegistry;
use pi_coding_agent::core::model_runtime::{CreateModelRuntimeOptions, ModelRuntime};
use pi_coding_agent::core::session_manager::{SessionEntry, SessionManager, SessionMessageEntry, UsageEntry};
use pi_coding_agent::core::settings_manager::CacheWarmingMode;
use pi_js::error::Result;

fn adaptive_model() -> Model {
    let mut m = get_model("anthropic", "claude-opus-4-6").expect("builtin claude-opus-4-6");
    m.prompt_cache = Some(ModelPromptCache {
        short: Some(300),
        long: Some(3600),
    });
    m
}

fn budget_model() -> Model {
    let mut m = get_model("anthropic", "claude-sonnet-4-5").expect("builtin claude-sonnet-4-5");
    m.prompt_cache = Some(ModelPromptCache {
        short: Some(300),
        long: Some(3600),
    });
    m
}

fn openai_model() -> Model {
    let mut m = get_model("openai", "gpt-5").expect("builtin gpt-5");
    m.prompt_cache = Some(ModelPromptCache {
        short: Some(300),
        long: Some(86_400),
    });
    m
}

fn warm_usage() -> Usage {
    Usage {
        input: 0,
        output: 1,
        cache_read: 100,
        cache_write: 0,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: 101,
        cost: UsageCost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.01,
            cache_write: 0.0,
            total: 0.01,
        },
    }
}

fn response(model: &Model, stop_reason: StopReason) -> AssistantMessage {
    AssistantMessage {
        content: Vec::new(),
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: warm_usage(),
        stop_reason,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 0,
    }
}

fn branch_with_prompt(prompt_tokens: i64) -> Vec<SessionEntry> {
    let m = adaptive_model();
    let mut msg = response(&m, StopReason::Length);
    msg.usage.output = 10;
    msg.usage.cache_read = prompt_tokens;
    msg.usage.total_tokens = prompt_tokens + 10;
    vec![SessionEntry::Message(SessionMessageEntry {
        id: "a".to_owned(),
        parent_id: None,
        timestamp: pi_js::time::iso_string(0),
        message: AgentMessage::Message(Message::Assistant(msg)),
    })]
}

#[derive(Clone)]
struct RecordedStreamCall {
    model: Model,
    options: Option<ModelsSimpleStreamOptions>,
}

struct MockStreamSource {
    calls: Arc<Mutex<Vec<RecordedStreamCall>>>,
    stop_reason: StopReason,
}

impl CacheWarmerStreamSource for MockStreamSource {
    fn stream_simple(
        &self,
        model: &Model,
        _context: &Context,
        options: Option<ModelsSimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        self.calls.lock().unwrap().push(RecordedStreamCall {
            model: model.clone(),
            options,
        });
        let stream = AssistantMessageEventStream::new();
        let msg = response(model, self.stop_reason);
        if self.stop_reason == StopReason::Error || self.stop_reason == StopReason::Aborted {
            stream.push(AssistantMessageEvent::Error {
                reason: self.stop_reason,
                error: msg,
            });
        } else {
            stream.push(AssistantMessageEvent::Done {
                reason: self.stop_reason,
                message: msg,
            });
        }
        stream
    }
}

struct MockSessionStore {
    sm: SessionManager,
    branch: Arc<Mutex<Vec<SessionEntry>>>,
    append_calls: Arc<Mutex<Vec<(String, String, String, Usage, Option<String>)>>>,
}

impl CacheWarmerSessionStore for MockSessionStore {
    fn append_usage(
        &self,
        kind: &str,
        provider: &str,
        model: &str,
        usage: Usage,
        note: Option<&str>,
    ) -> Result<UsageEntry> {
        self.append_calls.lock().unwrap().push((
            kind.to_owned(),
            provider.to_owned(),
            model.to_owned(),
            usage,
            note.map(ToOwned::to_owned),
        ));
        self.sm.append_usage(kind, provider, model, usage, note)
    }

    fn get_branch(&self) -> Vec<SessionEntry> {
        self.branch.lock().unwrap().clone()
    }
}

struct MockDecider {
    events: Arc<Mutex<Vec<CacheWarmingDecisionEvent>>>,
    override_action: Option<CacheWarmingAction>,
    on_decide: Option<Arc<dyn Fn() + Send + Sync>>,
}

#[async_trait]
impl CacheWarmingDecider for MockDecider {
    async fn decide(&self, event: CacheWarmingDecisionEvent) -> Result<CacheWarmingAction> {
        self.events.lock().unwrap().push(event);
        if let Some(cb) = &self.on_decide {
            cb();
        }
        Ok(self.override_action.unwrap_or(event.action))
    }
}

struct TestRuntime {
    warmer: CacheWarmer,
    calls: Arc<Mutex<Vec<RecordedStreamCall>>>,
    events: Arc<Mutex<Vec<CacheWarmingDecisionEvent>>>,
    warmed_entries: Arc<Mutex<Vec<UsageEntry>>>,
    append_calls: Arc<Mutex<Vec<(String, String, String, Usage, Option<String>)>>>,
    mode: Arc<Mutex<CacheWarmingMode>>,
}

fn setup_runtime(
    mode: CacheWarmingMode,
    branch: Vec<SessionEntry>,
    override_action: Option<CacheWarmingAction>,
    stop_reason: StopReason,
    on_decide: Option<Arc<dyn Fn() + Send + Sync>>,
) -> TestRuntime {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let events = Arc::new(Mutex::new(Vec::new()));
    let warmed_entries = Arc::new(Mutex::new(Vec::new()));
    let append_calls = Arc::new(Mutex::new(Vec::new()));
    let mode_arc = Arc::new(Mutex::new(mode));

    let source = Arc::new(MockStreamSource {
        calls: Arc::clone(&calls),
        stop_reason,
    });
    let store = Arc::new(MockSessionStore {
        sm: SessionManager::in_memory(None, None, None).unwrap(),
        branch: Arc::new(Mutex::new(branch)),
        append_calls: Arc::clone(&append_calls),
    });
    let decider = Arc::new(MockDecider {
        events: Arc::clone(&events),
        override_action,
        on_decide,
    });

    let mode_ref = Arc::clone(&mode_arc);
    let warmer = CacheWarmer::new(
        source,
        store,
        Arc::new(move || *mode_ref.lock().unwrap()),
        Some(decider),
    );
    let warmed_ref = Arc::clone(&warmed_entries);
    warmer.set_on_warmed(Some(Arc::new(move |entry| {
        warmed_ref.lock().unwrap().push(entry.clone());
    })));

    TestRuntime {
        warmer,
        calls,
        events,
        warmed_entries,
        append_calls,
        mode: mode_arc,
    }
}

fn warm_req(model: Model, base: SimpleStreamOptions) -> CacheWarmRequest {
    CacheWarmRequest {
        model,
        context: Context {
            system_prompt: None,
            messages: Vec::new(),
            tools: None,
        },
        options: ModelsSimpleStreamOptions {
            base,
            transform_headers: None,
        },
    }
}

async fn advance_clock(now_ms: &mut i64, delta_ms: u64) {
    *now_ms += delta_ms as i64;
    tokio::time::advance(Duration::from_millis(delta_ms)).await;
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;
}

#[test]
fn derives_eligibility_and_timing_from_retention_and_provider_behavior() {
    let adaptive = adaptive_model();
    let budget = budget_model();
    let openai = openai_model();
    let mut unknown = adaptive.clone();
    unknown.prompt_cache = None;

    let mut long_opts = SimpleStreamOptions::default();
    long_opts.base.cache_retention = Some(CacheRetention::Long);
    let mut none_opts = SimpleStreamOptions::default();
    none_opts.base.cache_retention = Some(CacheRetention::None);
    let mut env_opts = SimpleStreamOptions::default();
    let mut env_map = IndexMap::new();
    env_map.insert("PI_CACHE_RETENTION".to_owned(), "long".to_owned());
    env_opts.base.env = Some(env_map);

    assert_eq!(get_prompt_cache_ttl_ms(&adaptive, None), Some(300_000));
    assert_eq!(get_prompt_cache_ttl_ms(&adaptive, Some(&long_opts)), Some(3_600_000));
    assert_eq!(get_prompt_cache_ttl_ms(&adaptive, Some(&none_opts)), None);
    assert_eq!(get_prompt_cache_ttl_ms(&adaptive, Some(&env_opts)), Some(3_600_000));
    assert_eq!(get_prompt_cache_ttl_ms(&openai, Some(&long_opts)), Some(86_400_000));
    assert_eq!(get_prompt_cache_ttl_ms(&unknown, None), None);

    assert_eq!(get_cache_warming_delay_ms(300_000), Some(270_000));
    assert_eq!(get_cache_warming_delay_ms(60_000), Some(50_000));
    assert_eq!(get_cache_warming_delay_ms(10_000), None);

    let mut reason_opts = SimpleStreamOptions::default();
    reason_opts.reasoning = Some(ThinkingLevel::Medium);
    assert!(!is_replayable(&budget, Some(&reason_opts)));
    assert!(is_replayable(&budget, None));
    assert!(is_replayable(&adaptive, Some(&reason_opts)));
    assert!(is_replayable(&openai, Some(&reason_opts)));
}

#[tokio::test(start_paused = true)]
async fn replays_profitable_requests_and_preserves_options() {
    let mut now = 0i64;
    pi_js::time::testing::set_system_time(now);

    let rt = setup_runtime(
        CacheWarmingMode::Idle,
        branch_with_prompt(100_000),
        None,
        StopReason::Length,
        None,
    );
    let orig_ctrl = pi_js::abort::AbortController::new();
    let orig_sig = orig_ctrl.signal();
    let mut opts = SimpleStreamOptions::default();
    opts.reasoning = Some(ThinkingLevel::High);
    opts.base.signal = Some(orig_sig.clone());
    opts.base.session_id = Some("s".to_owned());

    let adaptive = adaptive_model();
    rt.warmer.start(warm_req(adaptive.clone(), opts), Arc::new(|| true));
    advance_clock(&mut now, 270_000).await;

    {
        let calls = rt.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].model.id, adaptive.id);
        let sent = calls[0].options.as_ref().unwrap();
        assert_eq!(sent.base.reasoning, Some(ThinkingLevel::High));
        assert_eq!(sent.base.base.session_id.as_deref(), Some("s"));
        assert_eq!(sent.base.base.max_tokens, Some(1));
        assert_eq!(sent.base.base.max_retries, Some(0));
        assert!(sent.base.base.signal.is_some());
        assert!(!sent.base.base.signal.as_ref().unwrap().aborted());
        // Original caller signal should not be shared with warmer abort controller
        orig_ctrl.abort(None);
        assert!(!sent.base.base.signal.as_ref().unwrap().aborted());
    }

    {
        let events = rt.events.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].action, CacheWarmingAction::Warm);
        assert_eq!(events[0].continuation_probability, 1.0);
        assert!((events[0].miss_cost - 0.575).abs() < 1e-4);
        assert!((events[0].warm_cost - 0.050025).abs() < 1e-5);
    }
    assert_eq!(rt.append_calls.lock().unwrap().len(), 1);
    assert_eq!(rt.warmed_entries.lock().unwrap().len(), 1);

    advance_clock(&mut now, 270_000).await;
    assert_eq!(rt.calls.lock().unwrap().len(), 2);
    rt.warmer.cancel();
    pi_js::time::testing::clear_system_time();
}

#[tokio::test(start_paused = true)]
async fn does_not_issue_refreshes_after_safe_deadline_and_rechecks_after_decide() {
    pi_js::time::testing::set_system_time(0);
    let rt = setup_runtime(
        CacheWarmingMode::Idle,
        branch_with_prompt(100_000),
        None,
        StopReason::Length,
        None,
    );
    rt.warmer
        .start(warm_req(adaptive_model(), Default::default()), Arc::new(|| true));
    pi_js::time::testing::set_system_time(285_001);
    rt.warmer.trigger_pending_refresh().await;
    assert_eq!(rt.calls.lock().unwrap().len(), 0);
    let st = rt.warmer.status();
    assert_eq!(st.state, CacheWarmingRunState::Inactive);
    assert_eq!(st.reason.as_deref(), Some("cache refresh deadline missed"));

    pi_js::time::testing::set_system_time(0);
    let rt2 = setup_runtime(
        CacheWarmingMode::Idle,
        branch_with_prompt(100_000),
        Some(CacheWarmingAction::Warm),
        StopReason::Length,
        Some(Arc::new(|| pi_js::time::testing::set_system_time(285_001))),
    );
    rt2.warmer
        .start(warm_req(adaptive_model(), Default::default()), Arc::new(|| true));
    rt2.warmer.trigger_pending_refresh().await;
    assert_eq!(rt2.calls.lock().unwrap().len(), 0);
    let st2 = rt2.warmer.status();
    assert_eq!(st2.state, CacheWarmingRunState::Inactive);
    assert_eq!(st2.reason.as_deref(), Some("cache refresh deadline missed"));
    pi_js::time::testing::clear_system_time();
}

#[tokio::test(start_paused = true)]
async fn applies_economic_decisions_extension_overrides_and_stop_reasons() {
    let mut now = 0i64;
    pi_js::time::testing::set_system_time(now);

    let unprofitable = setup_runtime(
        CacheWarmingMode::Idle,
        branch_with_prompt(5_000),
        None,
        StopReason::Length,
        None,
    );
    unprofitable
        .warmer
        .start(warm_req(adaptive_model(), Default::default()), Arc::new(|| true));
    advance_clock(&mut now, 270_000).await;
    assert_eq!(unprofitable.calls.lock().unwrap().len(), 0);
    assert_eq!(unprofitable.warmer.status().state, CacheWarmingRunState::Inactive);
    assert_eq!(
        unprofitable.warmer.status().decision.unwrap().action,
        CacheWarmingAction::Stop
    );
    assert_eq!(unprofitable.warmer.status().extension_override, Some(false));

    let forced = setup_runtime(
        CacheWarmingMode::Idle,
        branch_with_prompt(5_000),
        Some(CacheWarmingAction::Warm),
        StopReason::Length,
        None,
    );
    forced
        .warmer
        .start(warm_req(adaptive_model(), Default::default()), Arc::new(|| true));
    advance_clock(&mut now, 270_000).await;
    assert_eq!(forced.calls.lock().unwrap().len(), 1);
    assert_eq!(
        forced.warmed_entries.lock().unwrap()[0].note.as_deref(),
        Some("extension override")
    );
    forced.warmer.cancel();

    let vetoed = setup_runtime(
        CacheWarmingMode::Idle,
        branch_with_prompt(100_000),
        Some(CacheWarmingAction::Stop),
        StopReason::Length,
        None,
    );
    vetoed
        .warmer
        .start(warm_req(adaptive_model(), Default::default()), Arc::new(|| true));
    advance_clock(&mut now, 270_000).await;
    assert_eq!(vetoed.calls.lock().unwrap().len(), 0);
    assert_eq!(vetoed.warmer.status().extension_override, Some(true));

    let unavailable = setup_runtime(
        CacheWarmingMode::Idle,
        branch_with_prompt(0),
        None,
        StopReason::Length,
        None,
    );
    unavailable
        .warmer
        .start(warm_req(adaptive_model(), Default::default()), Arc::new(|| true));
    assert_eq!(
        unavailable.warmer.status().reason.as_deref(),
        Some("cache economics unavailable")
    );
    advance_clock(&mut now, 270_000).await;
    assert_eq!(unavailable.calls.lock().unwrap().len(), 0);

    // Unsupported requests, context change, mode change, settled streaming
    let unsupported = setup_runtime(
        CacheWarmingMode::Off,
        branch_with_prompt(100_000),
        None,
        StopReason::Length,
        None,
    );
    unsupported
        .warmer
        .start(warm_req(adaptive_model(), Default::default()), Arc::new(|| true));
    assert_eq!(
        unsupported.warmer.status().reason.as_deref(),
        Some("cache warming disabled")
    );

    *unsupported.mode.lock().unwrap() = CacheWarmingMode::Idle;
    let mut unknown = adaptive_model();
    unknown.prompt_cache = None;
    unsupported
        .warmer
        .start(warm_req(unknown, Default::default()), Arc::new(|| true));
    assert_eq!(
        unsupported.warmer.status().reason.as_deref(),
        Some("cache lifetime unavailable")
    );

    let mut high_opts = SimpleStreamOptions::default();
    high_opts.reasoning = Some(ThinkingLevel::High);
    unsupported
        .warmer
        .start(warm_req(budget_model(), high_opts), Arc::new(|| true));
    assert_eq!(
        unsupported.warmer.status().reason.as_deref(),
        Some("request cannot be replayed safely")
    );

    let still_current = Arc::new(AtomicBool::new(true));
    let sc_ref = Arc::clone(&still_current);
    unsupported.warmer.start(
        warm_req(adaptive_model(), Default::default()),
        Arc::new(move || sc_ref.load(Ordering::Relaxed)),
    );
    still_current.store(false, Ordering::Relaxed);
    assert_eq!(
        unsupported.warmer.status().reason.as_deref(),
        Some("conversation context changed")
    );
    advance_clock(&mut now, 270_000).await;
    assert_eq!(unsupported.calls.lock().unwrap().len(), 0);

    let streaming = setup_runtime(
        CacheWarmingMode::Streaming,
        branch_with_prompt(400_000),
        None,
        StopReason::Length,
        None,
    );
    streaming
        .warmer
        .start(warm_req(adaptive_model(), Default::default()), Arc::new(|| true));
    streaming.warmer.on_agent_settled();
    assert_eq!(streaming.warmer.status().reason.as_deref(), Some("agent run settled"));

    let failed = setup_runtime(
        CacheWarmingMode::Idle,
        branch_with_prompt(100_000),
        None,
        StopReason::Error,
        None,
    );
    failed
        .warmer
        .start(warm_req(adaptive_model(), Default::default()), Arc::new(|| true));
    advance_clock(&mut now, 270_000).await;
    assert_eq!(failed.append_calls.lock().unwrap().len(), 0);
    failed.warmer.cancel();

    pi_js::time::testing::clear_system_time();
}

#[test]
fn formats_status_and_usage_entries() {
    let decision = CacheWarmingDecision {
        phase: CacheWarmingPhase::Idle,
        warm_cost: 0.013,
        miss_cost: 0.621,
        continuation_probability: 0.6,
        expected_savings: 0.36,
        economics_available: true,
        action: CacheWarmingAction::Warm,
    };
    let status = pi_coding_agent::core::cache_warmer::CacheWarmingStatus {
        state: CacheWarmingRunState::Scheduled,
        reason: None,
        next_warm_at: Some(222_000),
        decision: Some(decision),
        extension_override: None,
    };
    assert_eq!(
        format_cache_warming_status(&status, Some(0)),
        "Decision in 3m 42s (60% continuation probability, expected savings $0.360 >= $0.050 -> warm)"
    );

    let mut usage = warm_usage();
    usage.cost = UsageCost {
        input: 0.00004,
        output: 0.00005,
        cache_read: 0.02940725,
        cache_write: 0.0,
        total: 0.02949725,
    };
    let adaptive = adaptive_model();
    let entry = SessionManager::in_memory(None, None, None)
        .unwrap()
        .append_usage(
            "cache_warm",
            &adaptive.provider,
            &adaptive.id,
            usage,
            Some("extension override"),
        )
        .unwrap();
    assert_eq!(
        format_cache_warming_usage(&entry),
        "Cache warmed (extension override): $0.029497"
    );
}

struct MockRuntimeWarmProvider {
    auth: ProviderAuth,
    calls: Arc<Mutex<Vec<String>>>,
}

impl Provider for MockRuntimeWarmProvider {
    fn id(&self) -> &str {
        "anthropic"
    }

    fn name(&self) -> &str {
        "Anthropic"
    }

    fn auth(&self) -> &ProviderAuth {
        &self.auth
    }

    fn get_models(&self) -> Vec<Model> {
        vec![adaptive_model()]
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
        model: &Model,
        _context: &TranscriptContext,
        _options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        self.calls.lock().unwrap().push(model.id.clone());
        let stream = AssistantMessageEventStream::new();
        stream.push(AssistantMessageEvent::Done {
            reason: StopReason::Stop,
            message: response(model, StopReason::Stop),
        });
        stream
    }
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn cache_warmer_routes_refresh_requests_through_model_runtime() {
    let mut now = 1_000_000_i64;
    pi_js::time::testing::set_system_time(now);

    let anthropic_auth = builtin_providers()
        .into_iter()
        .find(|p| p.id() == "anthropic")
        .expect("builtin anthropic provider")
        .auth()
        .clone();
    let mut creds = AuthStorageData::new();
    creds.insert(
        "anthropic".to_owned(),
        make_api_key_credential(Some("test-anthropic-key".to_owned()), None),
    );
    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(Arc::new(AuthStorage::in_memory(Some(creds)))),
        models_path: Some(None),
        allow_model_network: Some(false),
        refresh_on_create: Some(false),
        ..Default::default()
    })
    .await
    .unwrap();

    let stream_calls = Arc::new(Mutex::new(Vec::new()));
    runtime
        .register_native_provider(Arc::new(MockRuntimeWarmProvider {
            auth: anthropic_auth,
            calls: Arc::clone(&stream_calls),
        }))
        .unwrap();

    let append_calls = Arc::new(Mutex::new(Vec::new()));
    let store = Arc::new(MockSessionStore {
        sm: SessionManager::in_memory(None, None, None).unwrap(),
        branch: Arc::new(Mutex::new(branch_with_prompt(100_000))),
        append_calls: Arc::clone(&append_calls),
    });
    let warmer = CacheWarmer::new(Arc::new(runtime), store, Arc::new(|| CacheWarmingMode::Idle), None);

    warmer.start(warm_req(adaptive_model(), Default::default()), Arc::new(|| true));
    advance_clock(&mut now, 270_000).await;
    pi_js::time::testing::set_system_time(now);
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;

    assert_eq!(stream_calls.lock().unwrap().as_slice(), &["claude-opus-4-6"]);
    assert_eq!(append_calls.lock().unwrap().len(), 1);

    warmer.cancel();
    pi_js::time::testing::clear_system_time();
}

#[tokio::test]
async fn extension_runner_emit_cache_warming_decision_uses_last_extension_override() {
    let runtime = create_extension_runtime();
    let event_bus: Arc<dyn EventBus> = create_event_bus();
    let cwd = pi_js::env::cwd();

    let first = load_extension_from_factory(
        Arc::new(|pi: ExtensionApi| {
            Box::pin(async move {
                pi.on::<CacheWarmingDecisionEvent>(|_event, _ctx| {
                    Box::pin(async move {
                        Ok(Some(CacheWarmingDecisionEventResult {
                            action: Some(CacheWarmingAction::Warm),
                        }))
                    })
                });
                Ok(())
            })
        }),
        &cwd,
        Arc::clone(&event_bus),
        runtime.clone(),
        Some("<inline:warm>"),
    )
    .await
    .expect("load first extension");

    let second = load_extension_from_factory(
        Arc::new(|pi: ExtensionApi| {
            Box::pin(async move {
                pi.on::<CacheWarmingDecisionEvent>(|_event, _ctx| {
                    Box::pin(async move {
                        Ok(Some(CacheWarmingDecisionEventResult {
                            action: Some(CacheWarmingAction::Stop),
                        }))
                    })
                });
                Ok(())
            })
        }),
        &cwd,
        Arc::clone(&event_bus),
        runtime.clone(),
        Some("<inline:stop>"),
    )
    .await
    .expect("load second extension");

    let model_registry = ModelRegistry::in_memory();
    let runner = ExtensionRunner::new(
        vec![first, second],
        runtime,
        &cwd,
        SessionManager::default(),
        model_registry,
    );

    let decision = runner
        .emit_cache_warming_decision(CacheWarmingDecisionEvent {
            r#type: "cache_warming_decision",
            warm_cost: 0.05,
            miss_cost: 0.5,
            continuation_probability: 0.15,
            action: CacheWarmingAction::Warm,
        })
        .await;
    assert_eq!(decision, CacheWarmingAction::Stop);
}
