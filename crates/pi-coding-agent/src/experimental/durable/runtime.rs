//! Port of packages/coding-agent/src/experimental/durable/runtime.ts

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use indexmap::IndexMap;
use pi_ai::models::{clamp_thinking_level, get_supported_thinking_levels};
use pi_ai::types::{ModelThinkingLevel, UserContentBlock, UserMessageContent};
use pi_chord::context::BACKGROUND_CONTEXT;
use pi_durable::harness::harness::{ConversationEntriesQuery, RootConversationOptions};
use pi_durable::harness::registry::RegistryImpl;
use pi_durable::harness::submissions::SubmissionHandle;
use pi_durable::harness::types::{
    AgentChange, AgentState, EnvFactory, FieldChange, HarnessOptions, ModelRef, WhenBusy,
};
use pi_durable::storage::sqlite::node::{NodeSqliteStorageOptions, open_node_sqlite_storage};
use pi_durable::types::{
    AttachedState, CommitChange, ConversationId, ConversationQuery, Cursor, EntryRecord, ROOT_CONVERSATION_ID,
    SubmissionStatus, TableCommitChange, TaskOutcome,
};
use pi_durable::{Conversation, ConversationView, Harness, Registry, SubmissionDraft, TaskGraph};
use pi_js::{BoxFuture, Unsubscribe};
use serde_json::Value;

use crate::core::model_runtime::{ModelRuntime, ModelRuntimeCreateOptions};
use crate::core::settings_manager::SettingsManager;
use crate::experimental::durable::harness_setup::{
    ExecutionEnvs, configure_harness_http, create_coding_registry, create_harness_settings, find_initial_agent_model,
};
use crate::experimental::durable::sessions::{SessionLocation, select_session};
use crate::experimental::durable::subagent::SUBAGENT;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelSummary {
    pub provider: String,
    pub model_id: String,
    pub name: String,
    pub context_window: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeLevel {
    Info,
    Warning,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub id: u64,
    pub level: NoticeLevel,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConversationSummary {
    pub id: ConversationId,
    pub label: String,
    pub title: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DurableSessionMeta {
    pub id: String,
    pub directory: String,
    pub cwd: String,
}

#[derive(Clone)]
pub struct DurableView {
    pub session: DurableSessionMeta,
    pub conversation: ConversationView,
    pub conversations: Vec<ConversationSummary>,
    pub models: Vec<ModelSummary>,
    pub notices: Vec<Notice>,
    pub tasks: Option<TaskGraph>,
}

#[derive(Clone)]
pub struct DurableViewSource {
    state: Arc<Mutex<ViewState>>,
}

struct ViewState {
    current: DurableView,
    listeners: IndexMap<u64, Arc<dyn Fn() + Send + Sync>>,
    next_id: u64,
    notifying: bool,
}

impl DurableViewSource {
    pub fn current(&self) -> DurableView {
        lock(&self.state).current.clone()
    }

    pub fn subscribe(&self, listener: Arc<dyn Fn() + Send + Sync>) -> Unsubscribe {
        let id = {
            let mut st = lock(&self.state);
            let id = st.next_id;
            st.next_id += 1;
            st.listeners.insert(id, listener);
            id
        };
        let state = Arc::clone(&self.state);
        Unsubscribe::new(move || {
            lock(&state).listeners.shift_remove(&id);
        })
    }
}

#[derive(Clone)]
pub struct DurableController {
    inner: Arc<ControllerInner>,
}

struct ControllerInner {
    harness: Harness,
    model_runtime: Arc<ModelRuntime>,
    current: Arc<Mutex<Conversation>>,
    view_state_handle: Arc<Mutex<AttachedState<ConversationView>>>,
    view_unsub: Arc<Mutex<Option<Unsubscribe>>>,
    tasks_state: Arc<Mutex<Option<(AttachedState<TaskGraph>, Unsubscribe)>>>,
    view_source: Arc<Mutex<ViewState>>,
    next_notice_id: Arc<Mutex<u64>>,
    queue: Arc<tokio::sync::Mutex<()>>,
}

impl DurableController {
    pub async fn submit(&self, text: String, when_busy: WhenBusy) -> pi_js::Result<()> {
        let _guard = self.inner.queue.lock().await;
        let conv = lock(&self.inner.current).clone();
        let res = conv
            .submit(
                SubmissionDraft::Input {
                    request_id: None,
                    content: UserMessageContent::Text(text),
                    when_busy: Some(when_busy),
                },
                BACKGROUND_CONTEXT.clone(),
            )
            .await;
        match res {
            Ok(submission) => {
                self.watch_answer(submission);
                Ok(())
            }
            Err(err) => {
                self.fail_notice(&err);
                Ok(())
            }
        }
    }

    pub async fn compact(&self, instructions: Option<String>) -> pi_js::Result<()> {
        let _guard = self.inner.queue.lock().await;
        let conv = lock(&self.inner.current).clone();
        let task_id = match conv.compact(instructions, BACKGROUND_CONTEXT.clone()).await {
            Ok(id) => id,
            Err(err) => {
                self.fail_notice(&err);
                return Ok(());
            }
        };
        let inner = Arc::clone(&self.inner);
        tokio::spawn(async move {
            match inner.harness.wait_for_task(task_id, BACKGROUND_CONTEXT.clone()).await {
                Ok(receipt) => match receipt.outcome {
                    TaskOutcome::Completed { result } => {
                        let status = match result.submission_id {
                            None => None,
                            Some(sid) => match inner.harness.submission(sid, BACKGROUND_CONTEXT.clone()).await {
                                Ok(Some(handle)) => {
                                    handle.status(BACKGROUND_CONTEXT.clone()).await.ok().map(|r| r.status())
                                }
                                _ => None,
                            },
                        };
                        let msg = if result.entry_id.is_some() || status == Some(SubmissionStatus::Done) {
                            "Compacted."
                        } else if status == Some(SubmissionStatus::Queued) {
                            "Compaction summary queued; it is placed at the next turn boundary."
                        } else if status == Some(SubmissionStatus::Unanswered) {
                            "Compaction summary dropped: the context changed under it."
                        } else {
                            "Nothing to compact: the context fits in the recent window that is kept verbatim."
                        };
                        push_notice(&inner.view_source, &inner.next_notice_id, NoticeLevel::Info, msg);
                    }
                    TaskOutcome::Aborted { .. } => {
                        push_notice(
                            &inner.view_source,
                            &inner.next_notice_id,
                            NoticeLevel::Info,
                            "Compaction aborted.",
                        );
                    }
                    TaskOutcome::Failed { error, .. } | TaskOutcome::Faulted { error } => {
                        push_notice(
                            &inner.view_source,
                            &inner.next_notice_id,
                            NoticeLevel::Error,
                            &format!("Compaction {}: {}", receipt.kind, error.message),
                        );
                    }
                    TaskOutcome::Orphaned { reason } => {
                        push_notice(
                            &inner.view_source,
                            &inner.next_notice_id,
                            NoticeLevel::Error,
                            &format!("Compaction orphaned: {reason}"),
                        );
                    }
                },
                Err(err) => {
                    push_notice(
                        &inner.view_source,
                        &inner.next_notice_id,
                        NoticeLevel::Error,
                        &err.message(),
                    );
                }
            }
        });
        Ok(())
    }

    pub async fn abort(&self) -> pi_js::Result<()> {
        let conv = lock(&self.inner.current).clone();
        if let Err(err) = conv.abort(BACKGROUND_CONTEXT.clone(), None).await {
            self.fail_notice(&err);
        }
        Ok(())
    }

    pub async fn cycle_thinking(&self) -> pi_js::Result<()> {
        let _guard = self.inner.queue.lock().await;
        let conv = lock(&self.inner.current).clone();
        let current_view = lock(&self.inner.view_source).current.conversation.clone();
        let agent = agent_of(&current_view);
        let model = match agent.model {
            None => {
                push_notice(
                    &self.inner.view_source,
                    &self.inner.next_notice_id,
                    NoticeLevel::Error,
                    "No model selected",
                );
                return Ok(());
            }
            Some(ref r) => match self.inner.model_runtime.get_model(&r.provider, &r.model_id) {
                Some(m) => m,
                None => {
                    push_notice(
                        &self.inner.view_source,
                        &self.inner.next_notice_id,
                        NoticeLevel::Error,
                        "Current model is unavailable",
                    );
                    return Ok(());
                }
            },
        };
        if !model.reasoning {
            push_notice(
                &self.inner.view_source,
                &self.inner.next_notice_id,
                NoticeLevel::Error,
                "Current model does not support thinking",
            );
            return Ok(());
        }
        let levels = get_supported_thinking_levels(&model);
        let level = agent.thinking_level.unwrap_or(ModelThinkingLevel::Off);
        let idx = levels.iter().position(|l| *l == level).unwrap_or(0);
        let next = levels
            .get((idx + 1) % levels.len().max(1))
            .copied()
            .unwrap_or(ModelThinkingLevel::Off);
        if let Err(err) = conv
            .configure(
                AgentChange {
                    thinking_level: FieldChange::set(next),
                    ..Default::default()
                },
                BACKGROUND_CONTEXT.clone(),
            )
            .await
        {
            self.fail_notice(&err);
        }
        Ok(())
    }

    pub async fn set_model(&self, model_ref: ModelRef) -> pi_js::Result<()> {
        let _guard = self.inner.queue.lock().await;
        let Some(model) = self
            .inner
            .model_runtime
            .get_model(&model_ref.provider, &model_ref.model_id)
        else {
            push_notice(
                &self.inner.view_source,
                &self.inner.next_notice_id,
                NoticeLevel::Error,
                &format!("Unknown model: {}/{}", model_ref.provider, model_ref.model_id),
            );
            return Ok(());
        };
        let current_view = lock(&self.inner.view_source).current.conversation.clone();
        let thinking = agent_of(&current_view)
            .thinking_level
            .unwrap_or(ModelThinkingLevel::Off);
        let clamped = clamp_thinking_level(&model, thinking);
        let conv = lock(&self.inner.current).clone();
        if let Err(err) = conv
            .configure(
                AgentChange {
                    model: FieldChange::set(model_ref),
                    thinking_level: FieldChange::set(clamped),
                    ..Default::default()
                },
                BACKGROUND_CONTEXT.clone(),
            )
            .await
        {
            self.fail_notice(&err);
        }
        Ok(())
    }

    pub async fn toggle_tasks(&self) -> pi_js::Result<()> {
        let _guard = self.inner.queue.lock().await;
        let closed_existing = {
            let mut slot = lock(&self.inner.tasks_state);
            if let Some((graph, unsub)) = slot.take() {
                unsub.call();
                let _ = graph.dispose();
                true
            } else {
                false
            }
        };
        if closed_existing {
            schedule_update(&self.inner.view_source, |st| {
                st.tasks = None;
            });
            return Ok(());
        }
        match self.inner.harness.task_graph(BACKGROUND_CONTEXT.clone()).await {
            Ok(graph) => {
                let vs = Arc::clone(&self.inner.view_source);
                let unsub = graph.subscribe(Arc::new(move |val, _ctx, _del| {
                    schedule_update(&vs, |st| {
                        st.tasks = Some(val);
                    });
                    Box::pin(futures::future::ready(Ok(())))
                }));
                *lock(&self.inner.tasks_state) = Some((graph, unsub));
            }
            Err(err) => self.fail_notice(&err),
        }
        Ok(())
    }

    pub async fn switch_conversation(&self, id: ConversationId) -> pi_js::Result<()> {
        let _guard = self.inner.queue.lock().await;
        let next = match self.inner.harness.conversation(id, BACKGROUND_CONTEXT.clone()).await {
            Ok(Some(c)) => c,
            Ok(None) => {
                push_notice(
                    &self.inner.view_source,
                    &self.inner.next_notice_id,
                    NoticeLevel::Error,
                    &format!("Conversation {} does not exist", id.as_i64()),
                );
                return Ok(());
            }
            Err(err) => {
                self.fail_notice(&err);
                return Ok(());
            }
        };
        let next_state = match next.view_state(BACKGROUND_CONTEXT.clone()).await {
            Ok(s) => s,
            Err(err) => {
                self.fail_notice(&err);
                return Ok(());
            }
        };
        if let Some(prev_unsub) = lock(&self.inner.view_unsub).take() {
            prev_unsub.call();
        }
        let _ = lock(&self.inner.view_state_handle).dispose();
        *lock(&self.inner.current) = next;
        *lock(&self.inner.view_state_handle) = next_state.clone();
        let vs = Arc::clone(&self.inner.view_source);
        let unsub = next_state.subscribe(Arc::new(move |val, _ctx, _del| {
            schedule_update(&vs, |st| {
                st.conversation = val;
            });
            Box::pin(futures::future::ready(Ok(())))
        }));
        *lock(&self.inner.view_unsub) = Some(unsub);
        Ok(())
    }

    fn watch_answer(&self, submission: SubmissionHandle) {
        let inner = Arc::clone(&self.inner);
        tokio::spawn(async move {
            match submission.wait(BACKGROUND_CONTEXT.clone()).await {
                Ok(settled) => {
                    if let pi_durable::types::SubmissionState::Input(
                        pi_durable::types::InputSubmissionState::Unanswered { reason, detail, .. },
                    ) = settled.record.state
                        && reason != "aborted"
                    {
                        let suffix = detail
                            .as_ref()
                            .map(|d| format!(" {}", pi_js::json::stringify(d)))
                            .unwrap_or_default();
                        push_notice(
                            &inner.view_source,
                            &inner.next_notice_id,
                            NoticeLevel::Error,
                            &format!("No answer: {reason}{suffix}"),
                        );
                    }
                }
                Err(err) => push_notice(
                    &inner.view_source,
                    &inner.next_notice_id,
                    NoticeLevel::Error,
                    &err.message(),
                ),
            }
        });
    }

    fn fail_notice(&self, err: &pi_js::Error) {
        push_notice(
            &self.inner.view_source,
            &self.inner.next_notice_id,
            NoticeLevel::Error,
            &err.message(),
        );
    }
}

#[derive(Clone, Debug, Default)]
pub struct OpenDurableOptions {
    pub cwd: Option<String>,
    pub continue_session: bool,
}

pub struct OpenDurableResult {
    pub view: DurableViewSource,
    pub controller: DurableController,
    pub settings: Arc<SettingsManager>,
    close_fn: Arc<Mutex<Option<BoxFuture<pi_js::Result<()>>>>>,
    close_lock: tokio::sync::Mutex<()>,
}

impl OpenDurableResult {
    pub async fn close(&self) -> pi_js::Result<()> {
        let _guard = self.close_lock.lock().await;
        let fut = lock(&self.close_fn).take();
        if let Some(f) = fut {
            f.await?;
        }
        Ok(())
    }
}

/// The agent document of a view; default while the conversation has none.
pub fn agent_of(view: &ConversationView) -> AgentState {
    let Some(raw) = view.docs.get("pi.agent") else {
        return AgentState::default();
    };
    serde_json::from_value(Value::Object(raw.clone())).unwrap_or_default()
}

async fn first_input(harness: &Harness, id: ConversationId) -> pi_js::Result<Option<String>> {
    if id == ROOT_CONVERSATION_ID {
        return Ok(None);
    }
    let conversation = harness
        .conversation(id, BACKGROUND_CONTEXT.clone())
        .await?
        .ok_or_else(|| pi_js::Error::msg(format!("Missing conversation {}", id.as_i64())))?;
    let mut first: Option<EntryRecord> = None;
    let mut cursor: Option<Cursor> = None;
    loop {
        let page = conversation
            .entries(
                ConversationEntriesQuery::default(),
                256,
                cursor,
                BACKGROUND_CONTEXT.clone(),
            )
            .await?;
        if let Some(found) = page.items.into_iter().rev().find(|e| e.kind == "pi.user") {
            first = Some(found);
        }
        cursor = page.next;
        if cursor.is_none() {
            break;
        }
    }
    Ok(title_of(first.as_ref()))
}

fn title_of(entry: Option<&EntryRecord>) -> Option<String> {
    let msg = entry?.model.as_ref()?.first()?;
    let pi_ai::types::Message::User(user) = msg else {
        return None;
    };
    let text = match &user.content {
        UserMessageContent::Text(s) => s.clone(),
        UserMessageContent::Blocks(blocks) => blocks
            .iter()
            .filter_map(|b| match b {
                UserContentBlock::Text(t) => Some(t.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" "),
    };
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    Some(collapsed)
}

pub(crate) struct DurableProfileConfig {
    pub select_location: fn(String, bool) -> BoxFuture<pi_js::Result<SessionLocation>>,
    pub build_registry: fn(Arc<SettingsManager>, String) -> Arc<RegistryImpl>,
    pub build_env: Option<fn(String) -> (EnvFactory, Arc<ExecutionEnvs>)>,
    pub root_extensions: Option<pi_durable::harness::types::AgentExtensionsChange>,
    pub root_cwd: bool,
}

pub(crate) async fn open_durable_with(
    options: OpenDurableOptions,
    config: DurableProfileConfig,
) -> pi_js::Result<OpenDurableResult> {
    let cwd = match options.cwd {
        Some(c) => c,
        None => pi_js::env::cwd(),
    };
    let location = (config.select_location)(cwd, options.continue_session).await?;
    let loc_id = location.id.clone();
    let loc_dir = location.directory.clone();
    let loc_cwd = location.cwd.clone();
    let loc_database = location.database.clone();
    let loc_created = location.created;
    let location_slot = Arc::new(tokio::sync::Mutex::new(Some(location)));
    let envs_cleanup = config.build_env.map(|builder| builder(loc_cwd.clone()).1);
    let mut opened_harness: Option<Harness> = None;

    let envs_for_async = envs_cleanup.clone();
    let location_for_close = Arc::clone(&location_slot);
    let result = async {
        let model_runtime = Arc::new(ModelRuntime::create(ModelRuntimeCreateOptions::default()).await?);
        let settings_manager = Arc::new(SettingsManager::create(&loc_cwd, None, None));
        configure_harness_http(&settings_manager);
        let harness_settings = create_harness_settings(Arc::clone(&settings_manager));
        let registry = (config.build_registry)(Arc::clone(&settings_manager), loc_cwd.clone());

        let pending_reports: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let report_sink: Arc<Mutex<Option<Arc<dyn Fn(String) + Send + Sync>>>> = Arc::new(Mutex::new(None));
        let pending_clone = Arc::clone(&pending_reports);
        let sink_clone = Arc::clone(&report_sink);

        let env_opt_for_harness = envs_for_async.as_ref().map(|e| e.env_factory());

        let storage = Arc::new(open_node_sqlite_storage(&loc_database, NodeSqliteStorageOptions::default()).await?);
        let harness = Harness::open(
            storage,
            HarnessOptions {
                models: model_runtime.as_models().clone(),
                registry,
                settings: Some(harness_settings),
                env: env_opt_for_harness,
                conversation_created: None,
                now: None,
                on_report: Some(Arc::new(move |err| {
                    let msg = err.message().to_string();
                    if let Some(ref sink) = *lock(&sink_clone) {
                        sink(msg);
                    } else {
                        lock(&pending_clone).push(msg);
                    }
                })),
            },
            BACKGROUND_CONTEXT.clone(),
        )
        .await?;
        opened_harness = Some(harness.clone());

        let initial = if loc_created {
            Some(find_initial_agent_model(&settings_manager, &model_runtime, None).await?)
        } else {
            None
        };

        let root_agent = AgentChange {
            cwd: if config.root_cwd {
                FieldChange::set(loc_cwd.clone())
            } else {
                FieldChange::Unchanged
            },
            extensions: match config.root_extensions {
                Some(exts) => FieldChange::set(exts),
                None => FieldChange::Unchanged,
            },
            model: initial
                .as_ref()
                .and_then(|i| i.model.clone())
                .map_or(FieldChange::Unchanged, FieldChange::set),
            thinking_level: initial
                .as_ref()
                .and_then(|i| i.thinking_level)
                .map_or(FieldChange::Unchanged, FieldChange::set),
            ..Default::default()
        };

        let root = harness
            .root(
                BACKGROUND_CONTEXT.clone(),
                Some(RootConversationOptions {
                    agent: Some(root_agent),
                    init: None,
                }),
            )
            .await?;

        let label = |id: ConversationId| -> String {
            if id == root.id() {
                "main".to_string()
            } else {
                format!("subagent {}", id.as_i64())
            }
        };

        let mut summaries = Vec::new();
        let mut cursor: Option<Cursor> = None;
        loop {
            let page = harness
                .session()
                .commit(
                    move |tx| async move { tx.scan_conversations(ConversationQuery::default(), 256, cursor).await },
                    BACKGROUND_CONTEXT.clone(),
                )
                .await?;
            for rec in page.items {
                let title = first_input(&harness, rec.id).await?;
                summaries.push(ConversationSummary {
                    id: rec.id,
                    label: label(rec.id),
                    title,
                });
            }
            cursor = page.next;
            if cursor.is_none() {
                break;
            }
        }

        let conv_state = root.view_state(BACKGROUND_CONTEXT.clone()).await?;
        let model_summaries: Vec<ModelSummary> = model_runtime
            .get_available_snapshot()
            .into_iter()
            .map(|m| ModelSummary {
                provider: m.provider,
                model_id: m.id,
                name: m.name,
                context_window: u64::try_from(m.context_window).unwrap_or(0),
            })
            .collect();

        let initial_view = DurableView {
            session: DurableSessionMeta {
                id: loc_id,
                directory: loc_dir,
                cwd: loc_cwd.clone(),
            },
            conversation: conv_state.value(),
            conversations: summaries,
            models: model_summaries,
            notices: Vec::new(),
            tasks: None,
        };

        let view_state = Arc::new(Mutex::new(ViewState {
            current: initial_view,
            listeners: IndexMap::new(),
            next_id: 1,
            notifying: false,
        }));
        let next_notice_id = Arc::new(Mutex::new(1u64));

        {
            let vs = Arc::clone(&view_state);
            let nid = Arc::clone(&next_notice_id);
            *lock(&report_sink) = Some(Arc::new(move |msg: String| {
                push_notice(&vs, &nid, NoticeLevel::Warning, &msg);
            }));
            for msg in lock(&pending_reports).drain(..) {
                push_notice(&view_state, &next_notice_id, NoticeLevel::Warning, &msg);
            }
        }

        let vs_sub = Arc::clone(&view_state);
        let initial_unsub = conv_state.subscribe(Arc::new(move |val, _ctx, _del| {
            schedule_update(&vs_sub, |st| {
                st.conversation = val;
            });
            Box::pin(futures::future::ready(Ok(())))
        }));

        let root_id = root.id();
        let vs_commits = Arc::clone(&view_state);
        let unsub_commits = harness.session().subscribe_commits(move |pub_, _ctx| {
            let changes = pub_.changes.clone();
            schedule_update(&vs_commits, move |st| {
                for change in &changes {
                    if let CommitChange::Table(TableCommitChange::Conversation(rec)) = change {
                        let lbl = if rec.id == root_id {
                            "main".to_string()
                        } else {
                            format!("subagent {}", rec.id.as_i64())
                        };
                        st.conversations.push(ConversationSummary {
                            id: rec.id,
                            label: lbl,
                            title: None,
                        });
                    } else if let CommitChange::Table(TableCommitChange::Entry(entry)) = change
                        && entry.kind == "pi.user"
                        && let Some(title) = title_of(Some(entry))
                    {
                        for s in &mut st.conversations {
                            if s.id == entry.conversation_id && s.title.is_none() {
                                s.title = Some(title.clone());
                            }
                        }
                    }
                }
            });
        })?;

        let view_unsub_slot = Arc::new(Mutex::new(Some(initial_unsub)));
        let view_state_handle = Arc::new(Mutex::new(conv_state));
        let tasks_state = Arc::new(Mutex::new(None));

        let saved_model = agent_of(&lock(&view_state).current.conversation).model;
        if let Some(ref saved) = saved_model {
            if model_runtime.get_model(&saved.provider, &saved.model_id).is_none() {
                push_notice(
                    &view_state,
                    &next_notice_id,
                    NoticeLevel::Warning,
                    &format!("Saved model is unavailable: {}/{}", saved.provider, saved.model_id),
                );
            }
        } else {
            push_notice(
                &view_state,
                &next_notice_id,
                NoticeLevel::Warning,
                "No model configured; select one with /model.",
            );
        }
        if let Some(ref fallback) = initial.as_ref().and_then(|i| i.fallback_message.as_deref()) {
            push_notice(&view_state, &next_notice_id, NoticeLevel::Info, fallback);
        }

        let controller = DurableController {
            inner: Arc::new(ControllerInner {
                harness: harness.clone(),
                model_runtime,
                current: Arc::new(Mutex::new(root)),
                view_state_handle: Arc::clone(&view_state_handle),
                view_unsub: Arc::clone(&view_unsub_slot),
                tasks_state: Arc::clone(&tasks_state),
                view_source: Arc::clone(&view_state),
                next_notice_id,
                queue: Arc::new(tokio::sync::Mutex::new(())),
            }),
        };

        controller.toggle_tasks().await?;
        harness.resume()?;

        let unsub_commits = Arc::new(Mutex::new(Some(unsub_commits)));
        let close_fn: BoxFuture<pi_js::Result<()>> = Box::pin(async move {
            if let Some(u) = lock(&view_unsub_slot).take() {
                u.call();
            }
            if let Some(u) = lock(&unsub_commits).take() {
                u.call();
            }
            let _ = lock(&view_state_handle).dispose();
            if let Some((graph, unsub)) = lock(&tasks_state).take() {
                unsub.call();
                let _ = graph.dispose();
            }
            let close_res = harness.close(BACKGROUND_CONTEXT.clone()).await;
            if let Some(envs) = envs_for_async {
                envs.cleanup(&BACKGROUND_CONTEXT.clone()).await;
            }
            let release_res = if let Some(mut loc) = location_for_close.lock().await.take() {
                loc.release().await
            } else {
                Ok(())
            };
            close_res?;
            release_res
        });

        Ok(OpenDurableResult {
            view: DurableViewSource { state: view_state },
            controller,
            settings: settings_manager,
            close_fn: Arc::new(Mutex::new(Some(close_fn))),
            close_lock: tokio::sync::Mutex::new(()),
        })
    }
    .await;

    if result.is_err() {
        if let Some(harness) = opened_harness {
            let _ = harness.close(BACKGROUND_CONTEXT.clone()).await;
        }
        if let Some(envs) = envs_cleanup {
            envs.cleanup(&BACKGROUND_CONTEXT.clone()).await;
        }
        if let Some(mut loc) = location_slot.lock().await.take() {
            let _ = loc.release().await;
        }
    }

    result
}

pub async fn open_durable(options: OpenDurableOptions) -> pi_js::Result<OpenDurableResult> {
    open_durable_with(
        options,
        DurableProfileConfig {
            select_location: |cwd, cont| Box::pin(async move { select_session(&cwd, cont).await }),
            build_registry: |settings, cwd| {
                let reg = create_coding_registry(settings, &cwd);
                let _ = reg.install(SUBAGENT.clone());
                reg
            },
            build_env: Some(|cwd| {
                let envs = Arc::new(ExecutionEnvs::new(cwd));
                (envs.env_factory(), envs)
            }),
            root_extensions: None,
            root_cwd: true,
        },
    )
    .await
}

fn schedule_update<F>(view_state: &Arc<Mutex<ViewState>>, mutate: F)
where
    F: FnOnce(&mut DurableView),
{
    let should_spawn = {
        let mut guard = lock(view_state);
        mutate(&mut guard.current);
        if guard.notifying {
            false
        } else {
            guard.notifying = true;
            true
        }
    };
    if should_spawn {
        let vs = Arc::clone(view_state);
        tokio::spawn(async move {
            let listeners: Vec<Arc<dyn Fn() + Send + Sync>> = {
                let mut guard = lock(&vs);
                guard.notifying = false;
                guard.listeners.values().cloned().collect()
            };
            for listener in listeners {
                listener();
            }
        });
    }
}

fn push_notice(view_state: &Arc<Mutex<ViewState>>, next_id: &Arc<Mutex<u64>>, level: NoticeLevel, message: &str) {
    let id = {
        let mut g = lock(next_id);
        let id = *g;
        *g += 1;
        id
    };
    let message = message.to_string();
    schedule_update(view_state, move |st| {
        st.notices.push(Notice { id, level, message });
        if st.notices.len() > 20 {
            let excess = st.notices.len() - 20;
            st.notices.drain(0..excess);
        }
    });
}
