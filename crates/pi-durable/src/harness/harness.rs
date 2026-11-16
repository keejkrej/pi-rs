//! Port of packages/durable/src/harness/harness.ts

use std::future::Future;
use std::ops::Deref;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};

use async_trait::async_trait;
use pi_ai::types::{Message, UserMessage};
use pi_chord::context::{with_abort_signal, without_abort_signal};
use pi_chord::{AttachedReplicatedState, Context, JsonValue};
use serde::de::DeserializeOwned;

use crate::documents::ErasedDocDefinition;
use crate::entries::RESET_ENTRY;
use crate::env::ExecutionEnv;
use crate::harness::agent::{AGENT_DOC, configure, create_agent, resolve_agent, resolve_settings};
use crate::harness::compaction::create_compaction;
use crate::harness::context::read_context;
use crate::harness::events::{AgentEventStream, watch_events_on_views};
use crate::harness::inbox::{INBOX_DOC, QueueModes, withdraw_queued_inputs};
use crate::harness::live::{LIVE_DOC, settle_scheduler_outcome};
use crate::harness::registry::BUILTIN_TASKS;
use crate::harness::scheduler::{AbortTaskResult, InvocationBinding, TaskScheduler, TaskSchedulerOptions};
use crate::harness::submissions::{SubmissionHandle, Submissions};
use crate::harness::task_graph::{TaskGraph, TaskGraphView, TaskGraphWatch};
use crate::harness::types::{
    AbortSubmissionResult, Agent, AgentChange, CompactionInput, CompactionReason, CompactionResult, ContextView,
    ConversationAbortOptions, ConversationCreateOptions, ConversationHandle, ConversationHandleBackend,
    ConversationInitFn, ConversationView, ConversationWatch, DocumentObserverBackend, DocumentReader,
    DocumentReaderBackend, EnvTarget, HarnessInspection, HarnessOptions, InputSubmissionDraft, RegistrySnapshot,
    SettledTask, SubmissionDraft, TaskInspection, UsageState, UserInput,
};
use crate::harness::usage::{USAGE_DOC, add_usage_state};
use crate::harness::util::{closed_error, scan_all};
use crate::harness::view::ConversationViews;
use crate::session::session::{Session, SessionHooks, SessionImpl};
use crate::session::transaction::{Transaction, TransactionScope, Tx};
use crate::tasks::decode_task_record;
use crate::types::{
    ConversationId, ConversationOwnership, ConversationQuery, ConversationRecord, Cursor, DocumentWatch, EntryDraft,
    EntryDraftHead, EntryId, EntryQuery, EntryRecord, ErasedTaskRecord, JsonObject, Page, ROOT_CONVERSATION_ID,
    SelfHead, Storage, SubmissionId, SubmissionQuery, SubmissionStatus, TaskId, TaskRecord,
};

const SCAN_PAGE_SIZE: usize = 256;

/// Options passed to [`Harness::root`].
#[derive(Clone, Default)]
pub struct RootConversationOptions {
    pub agent: Option<AgentChange>,
    pub init: Option<ConversationInitFn>,
}

/// Range filter for [`Conversation::entries`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ConversationEntriesQuery {
    pub min_entry_id: Option<EntryId>,
    pub max_entry_id: Option<EntryId>,
}

enum CreateTarget {
    Root,
    Independent {
        ownership: ConversationOwnership,
    },
    Fork {
        parent_id: ConversationId,
        at: EntryId,
        ownership: ConversationOwnership,
    },
}

#[derive(Clone, Default)]
struct CreateOptions {
    agent: Option<AgentChange>,
    init: Option<ConversationInitFn>,
}

struct HarnessSessionHooks {
    conversation_created: Option<crate::harness::types::ConversationCreatedHook>,
    scheduler: Mutex<Option<Weak<TaskScheduler>>>,
}

#[async_trait]
impl SessionHooks for HarnessSessionHooks {
    async fn conversation_created(&self, tx: &Arc<Transaction>, record: &ConversationRecord) -> pi_js::Result<()> {
        let tx_handle = tx.as_tx();
        tx_handle.doc_conversation(&*LIVE_DOC, record.id).await?;
        tx_handle.doc_conversation(&*INBOX_DOC, record.id).await?;
        tx_handle.doc_conversation(&*USAGE_DOC, record.id).await?;
        create_agent(&tx_handle, record).await?;
        if let Some(ref hook) = self.conversation_created {
            hook(tx_handle, record.clone()).await?;
        }
        Ok(())
    }

    async fn before_close(&self) {
        let scheduler = {
            let guard = self.scheduler.lock().unwrap_or_else(PoisonError::into_inner);
            guard.as_ref().and_then(Weak::upgrade)
        };
        if let Some(tasks) = scheduler {
            tasks.join().await;
        }
    }
}

struct HarnessHost {
    session: Arc<SessionImpl>,
    storage: Arc<dyn Storage>,
    options: HarnessOptions,
    now: Arc<dyn Fn() -> i64 + Send + Sync>,
    report: Arc<dyn Fn(pi_js::Error) + Send + Sync>,
    tasks: Arc<TaskScheduler>,
    submissions: Arc<Submissions>,
    views: Arc<ConversationViews>,
    task_graph: Arc<TaskGraphView>,
    closed: AtomicBool,
}

impl HarnessHost {
    fn assert_open(&self) -> pi_js::Result<()> {
        if self.closed.load(Ordering::Acquire) {
            Err(closed_error())
        } else {
            Ok(())
        }
    }

    async fn resolve_agent(
        &self,
        id: ConversationId,
        snapshot: Option<&dyn RegistrySnapshot>,
        context: Context,
    ) -> pi_js::Result<Agent> {
        let owned_snap = if snapshot.is_none() {
            Some(self.options.registry.snapshot())
        } else {
            None
        };
        let snap = snapshot.or(owned_snap.as_deref()).ok_or_else(|| closed_error())?;
        let state = Session::from_impl(Arc::clone(&self.session))
            .snapshot_conversation(&*AGENT_DOC, id, context)
            .await?;
        Ok(resolve_agent(
            state.as_ref(),
            snap,
            &resolve_settings(self.options.settings.as_deref()),
            self.report.as_ref(),
        ))
    }

    async fn build_env(&self, id: ConversationId, context: Context) -> pi_js::Result<Option<Arc<dyn ExecutionEnv>>> {
        let Some(ref build) = self.options.env else {
            return Ok(None);
        };
        let cwd = Session::from_impl(Arc::clone(&self.session))
            .snapshot_conversation(&*AGENT_DOC, id, context.clone())
            .await?
            .and_then(|s| s.cwd);
        let read = DocumentReader::from_backend(Arc::clone(&self.session) as Arc<dyn DocumentReaderBackend>);
        build(
            EnvTarget {
                conversation_id: id,
                cwd,
                read,
            },
            context,
        )
        .await
    }

    async fn create(
        self: &Arc<Self>,
        target: CreateTarget,
        options: CreateOptions,
        context: Context,
    ) -> pi_js::Result<Conversation> {
        self.assert_open()?;
        let id = self
            .session
            .commit_with(
                move |tx| async move {
                    if matches!(target, CreateTarget::Root) && tx.conversation(ROOT_CONVERSATION_ID).await?.is_some() {
                        return Ok(ROOT_CONVERSATION_ID);
                    }
                    let record = match target {
                        CreateTarget::Root => tx.create_root_conversation().await?,
                        CreateTarget::Independent { ownership } => tx.create_conversation(ownership).await?,
                        CreateTarget::Fork {
                            parent_id,
                            at,
                            ownership,
                        } => tx.fork_conversation(parent_id, at, ownership).await?,
                    };
                    let tx_handle = tx.as_tx();
                    if let Some(agent_change) = options.agent {
                        configure(&tx_handle, record.id, &agent_change).await?;
                    }
                    if let Some(init) = options.init {
                        init(tx_handle, record.id).await?;
                    }
                    Ok(record.id)
                },
                context,
                TransactionScope::default(),
            )
            .await?;
        Ok(Conversation {
            id,
            host: Arc::clone(self),
        })
    }
}

/// Handle to one durable conversation managed by a [`Harness`].
#[derive(Clone)]
pub struct Conversation {
    id: ConversationId,
    host: Arc<HarnessHost>,
}

impl Conversation {
    pub fn id(&self) -> ConversationId {
        self.id
    }

    pub fn views(&self) -> &Arc<ConversationViews> {
        &self.host.views
    }

    pub async fn agent(&self, context: Context) -> pi_js::Result<Agent> {
        self.host.resolve_agent(self.id, None, context).await
    }

    pub async fn configure(&self, change: AgentChange, context: Context) -> pi_js::Result<()> {
        let id = self.id;
        self.host
            .session
            .commit(move |tx| async move { configure(&tx, id, &change).await }, context)
            .await
    }

    pub async fn submit(&self, submission: SubmissionDraft, context: Context) -> pi_js::Result<SubmissionHandle> {
        self.host.submissions.submit(self.id, submission, context).await
    }

    pub async fn compact(
        &self,
        instructions: Option<String>,
        context: Context,
    ) -> pi_js::Result<TaskId<CompactionResult>> {
        self.host.tasks.resume();
        let id = self.id;
        let input = CompactionInput {
            reason: CompactionReason::Manual,
            instructions,
        };
        self.host
            .session
            .commit(
                move |tx| async move { create_compaction(&tx, id, input, None).await },
                context,
            )
            .await
    }

    pub async fn reset(&self, handoff: Option<String>, context: Context) -> pi_js::Result<()> {
        let now = (self.host.now)();
        let model = handoff.map(|text| {
            vec![Message::User(UserMessage {
                content: UserInput::Text(text),
                timestamp: now,
            })]
        });
        let entry = EntryDraft {
            kind: RESET_ENTRY.kind.to_string(),
            model,
            data: None,
            head: Some(EntryDraftHead::SelfId(SelfHead)),
            edits: None,
        };
        self.host
            .submissions
            .submit(
                self.id,
                SubmissionDraft::Write {
                    request_id: None,
                    entry,
                },
                context,
            )
            .await?;
        Ok(())
    }

    pub async fn commit<T, F, Fut>(&self, change: F, context: Context) -> pi_js::Result<T>
    where
        F: FnOnce(Tx) -> Fut + Send,
        Fut: Future<Output = pi_js::Result<T>> + Send,
        T: Send,
    {
        self.host
            .session
            .commit_with(
                move |tx| change(tx.as_tx()),
                context,
                TransactionScope {
                    conversation_id: Some(self.id),
                    task_id: None,
                },
            )
            .await
    }

    pub async fn context(&self, context: Context) -> pi_js::Result<ContextView> {
        read_context(&self.host.session, &self.host.storage, self.id, context, None).await
    }

    pub async fn entries(
        &self,
        query: ConversationEntriesQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: Context,
    ) -> pi_js::Result<Page<EntryRecord, Cursor>> {
        let storage = Arc::clone(&self.host.storage);
        let bounded = EntryQuery {
            conversation_id: self.id,
            min_entry_id: query.min_entry_id,
            max_entry_id: query.max_entry_id,
        };
        self.host
            .session
            .read_on_line(move || async move { storage.scan_entries(bounded, limit, cursor, context).await })
            .await
    }

    pub async fn fork(
        &self,
        at: EntryId,
        options: ConversationCreateOptions,
        context: Context,
    ) -> pi_js::Result<Conversation> {
        self.host
            .create(
                CreateTarget::Fork {
                    parent_id: self.id,
                    at,
                    ownership: options.ownership,
                },
                CreateOptions {
                    agent: options.agent,
                    init: options.init,
                },
                context,
            )
            .await
    }

    pub async fn abort(&self, context: Context, options: Option<ConversationAbortOptions>) -> pi_js::Result<()> {
        self.host.tasks.resume();
        let background = options.is_some_and(|o| o.background);
        self.host.tasks.abort_conversation(self.id, background, context).await
    }

    pub async fn wait_for_idle(&self, context: Context) -> pi_js::Result<()> {
        self.host.tasks.resume();
        self.host.tasks.wait_for_idle(Some(self.id), context).await
    }

    pub async fn view_state(&self, context: Context) -> pi_js::Result<AttachedReplicatedState<ConversationView>> {
        self.host.views.state(self.id, context).await
    }

    pub async fn watch(&self, context: Context) -> pi_js::Result<ConversationWatch> {
        self.host.views.watch(self.id, context).await
    }

    pub async fn watch_events(&self, context: Context) -> pi_js::Result<AgentEventStream> {
        watch_events_on_views(&self.host.views, self.id, context).await
    }
}

/// Durable agent harness over one Session.
#[derive(Clone)]
pub struct Harness {
    session: Session,
    host: Arc<HarnessHost>,
}

impl Deref for Harness {
    type Target = Session;

    fn deref(&self) -> &Self::Target {
        &self.session
    }
}

#[async_trait]
impl DocumentReaderBackend for Harness {
    async fn snapshot_erased(
        &self,
        def: &Arc<ErasedDocDefinition>,
        conversation_id: Option<ConversationId>,
        task_id: Option<TaskId>,
        key: Option<&str>,
        context: Context,
    ) -> pi_js::Result<Option<JsonObject>> {
        self.session
            .snapshot_erased(def, conversation_id, task_id, key, context)
            .await
    }

    async fn snapshot_as_of_erased(
        &self,
        def: &Arc<ErasedDocDefinition>,
        conversation_id: ConversationId,
        key: Option<&str>,
        at: EntryId,
        context: Context,
    ) -> pi_js::Result<Option<JsonObject>> {
        self.session
            .snapshot_as_of_erased(def, conversation_id, key, at, context)
            .await
    }
}

#[async_trait]
impl DocumentObserverBackend for Harness {
    async fn watch_erased(
        &self,
        def: &Arc<ErasedDocDefinition>,
        conversation_id: Option<ConversationId>,
        task_id: Option<TaskId>,
        key: Option<&str>,
        context: Context,
    ) -> pi_js::Result<Option<DocumentWatch<JsonObject>>> {
        self.session
            .watch_erased(def, conversation_id, task_id, key, context)
            .await
    }
}

impl Harness {
    /// Open a Harness over storage. The registry may keep changing while the Harness runs.
    pub async fn open(storage: Arc<dyn Storage>, options: HarnessOptions, context: Context) -> pi_js::Result<Self> {
        if let Some(signal) = context.abort_signal() {
            signal.throw_if_aborted()?;
        }
        let snapshot = options.registry.snapshot();
        let missing: Vec<&str> = BUILTIN_TASKS
            .iter()
            .filter(|task| snapshot.task(task.name()).is_none())
            .map(|task| task.name())
            .collect();
        if !missing.is_empty() {
            return Err(pi_js::Error::msg(format!(
                "Registry lacks built-in tasks {}; create it with create_registry()",
                missing.join(", ")
            )));
        }

        let hooks = Arc::new(HarnessSessionHooks {
            conversation_created: options.conversation_created.clone(),
            scheduler: Mutex::new(None),
        });
        let session_impl = SessionImpl::with_hooks(Arc::clone(&storage), hooks.clone());
        let session = Session::from_impl(Arc::clone(&session_impl));

        let now: Arc<dyn Fn() -> i64 + Send + Sync> = options
            .now
            .clone()
            .unwrap_or_else(|| Arc::new(|| pi_js::time::now_ms()));
        let report: Arc<dyn Fn(pi_js::Error) + Send + Sync> =
            options.on_report.clone().unwrap_or_else(|| Arc::new(|_| {}));

        let settings_holder = options.settings.clone();
        let settings_fn: Arc<dyn Fn() -> crate::harness::types::Settings + Send + Sync> =
            Arc::new(move || resolve_settings(settings_holder.as_deref()));

        let submissions_slot: Arc<Mutex<Option<Arc<Submissions>>>> = Arc::new(Mutex::new(None));
        let tasks_slot: Arc<Mutex<Option<Weak<TaskScheduler>>>> = Arc::new(Mutex::new(None));

        let session_for_agent = Arc::clone(&session_impl);
        let settings_for_agent = options.settings.clone();
        let report_for_agent = Arc::clone(&report);
        let agent_fn = Arc::new(
            move |id: ConversationId, snap: Arc<dyn RegistrySnapshot>, call_ctx: Context| {
                let s = Arc::clone(&session_for_agent);
                let set = settings_for_agent.clone();
                let rep = Arc::clone(&report_for_agent);
                Box::pin(async move {
                    let state = Session::from_impl(s)
                        .snapshot_conversation(&*AGENT_DOC, id, call_ctx)
                        .await?;
                    Ok(resolve_agent(
                        state.as_ref(),
                        snap.as_ref(),
                        &resolve_settings(set.as_deref()),
                        rep.as_ref(),
                    ))
                }) as pi_js::BoxFuture<pi_js::Result<Agent>>
            },
        );

        let session_for_env = Arc::clone(&session_impl);
        let env_opt = options.env.clone();
        let env_fn = Arc::new(move |id: ConversationId, call_ctx: Context| {
            let s = Arc::clone(&session_for_env);
            let env_factory = env_opt.clone();
            Box::pin(async move {
                let Some(factory) = env_factory else {
                    return Ok(None);
                };
                let cwd = Session::from_impl(Arc::clone(&s))
                    .snapshot_conversation(&*AGENT_DOC, id, call_ctx.clone())
                    .await?
                    .and_then(|st| st.cwd);
                let read = DocumentReader::from_backend(s as Arc<dyn DocumentReaderBackend>);
                factory(
                    EnvTarget {
                        conversation_id: id,
                        cwd,
                        read,
                    },
                    call_ctx,
                )
                .await
            }) as pi_js::BoxFuture<pi_js::Result<Option<Arc<dyn ExecutionEnv>>>>
        });

        let session_for_conv = Arc::clone(&session_impl);
        let storage_for_conv = Arc::clone(&storage);
        let subs_for_conv = Arc::clone(&submissions_slot);
        let tasks_for_conv = Arc::clone(&tasks_slot);
        let conv_fn = Arc::new(
            move |id: ConversationId, binding: InvocationBinding, call_ctx: Context| {
                let s = Arc::clone(&session_for_conv);
                let st = Arc::clone(&storage_for_conv);
                let subs_holder = Arc::clone(&subs_for_conv);
                let tasks_holder = Arc::clone(&tasks_for_conv);
                Box::pin(async move {
                    let record = s
                        .read_on_line(move || async move { st.conversation(id, call_ctx).await })
                        .await?;
                    let Some(_) = record else {
                        return Ok(None);
                    };
                    let submissions = subs_holder
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .clone()
                        .ok_or_else(closed_error)?;
                    let tasks = tasks_holder
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .as_ref()
                        .and_then(Weak::upgrade)
                        .ok_or_else(closed_error)?;
                    Ok(Some(bound_conversation(id, binding, submissions, tasks)))
                }) as pi_js::BoxFuture<pi_js::Result<Option<ConversationHandle>>>
            },
        );

        let settle_outcome_fn = Arc::new(
            |tx: Arc<Transaction>, record: ErasedTaskRecord, outcome: crate::harness::scheduler::SchedulerOutcome| {
                Box::pin(async move { settle_scheduler_outcome(&tx, &record, &outcome).await })
                    as pi_js::BoxFuture<pi_js::Result<()>>
            },
        );

        let withdraw_inputs_fn = Arc::new(|tx: Arc<Transaction>, cid: ConversationId| {
            Box::pin(async move { withdraw_queued_inputs(&tx.as_tx(), cid).await })
                as pi_js::BoxFuture<pi_js::Result<()>>
        });

        let tasks = TaskScheduler::new(TaskSchedulerOptions {
            session: Arc::clone(&session_impl),
            storage: Arc::clone(&storage),
            registry: Arc::clone(&options.registry),
            models: options.models.clone(),
            agent: agent_fn,
            settings: Arc::clone(&settings_fn),
            env: env_fn,
            now: Arc::clone(&now),
            report: Arc::clone(&report),
            settle_outcome: settle_outcome_fn,
            withdraw_inputs: withdraw_inputs_fn,
            conversation: conv_fn,
            context: without_abort_signal(context.clone()),
        });

        *hooks.scheduler.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::downgrade(&tasks));
        *tasks_slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::downgrade(&tasks));

        let weak_tasks_for_resume = Arc::downgrade(&tasks);
        let resume_fn = Arc::new(move || {
            if let Some(sched) = weak_tasks_for_resume.upgrade() {
                sched.resume();
            }
        });
        let queue_modes_fn = Arc::new(move || {
            let s = settings_fn();
            QueueModes {
                steering_mode: s.steering_mode,
                follow_up_mode: s.follow_up_mode,
            }
        });
        let submissions = Submissions::new(
            Arc::clone(&session_impl),
            Arc::clone(&storage),
            Arc::clone(&now),
            queue_modes_fn,
            resume_fn,
        )?;
        *submissions_slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::clone(&submissions));

        let views = ConversationViews::new(Arc::clone(&session_impl), Arc::clone(&storage))?;
        let task_graph = TaskGraphView::new(Arc::clone(&session_impl), Arc::clone(&storage))?;

        let host = Arc::new(HarnessHost {
            session: session_impl,
            storage,
            options,
            now,
            report: Arc::clone(&report),
            tasks: Arc::clone(&tasks),
            submissions,
            views,
            task_graph,
            closed: AtomicBool::new(false),
        });
        let harness = Self { session, host };

        if let Err(err) = tasks.open(context.clone()).await {
            if let Err(close_err) = harness.close(without_abort_signal(context)).await {
                (report)(close_err);
            }
            return Err(err);
        }
        Ok(harness)
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    pub fn resume(&self) -> pi_js::Result<()> {
        self.host.assert_open()?;
        self.host.tasks.resume();
        Ok(())
    }

    pub async fn root(
        &self,
        context: Context,
        options: Option<RootConversationOptions>,
    ) -> pi_js::Result<Conversation> {
        let opts = options.unwrap_or_default();
        self.host
            .create(
                CreateTarget::Root,
                CreateOptions {
                    agent: opts.agent,
                    init: opts.init,
                },
                context,
            )
            .await
    }

    pub async fn conversation(&self, id: ConversationId, context: Context) -> pi_js::Result<Option<Conversation>> {
        self.host.assert_open()?;
        let storage = Arc::clone(&self.host.storage);
        let record = self
            .host
            .session
            .read_on_line(move || async move { storage.conversation(id, context).await })
            .await?;
        Ok(record.map(|rec| Conversation {
            id: rec.id,
            host: Arc::clone(&self.host),
        }))
    }

    pub async fn create_conversation(
        &self,
        options: ConversationCreateOptions,
        context: Context,
    ) -> pi_js::Result<Conversation> {
        self.host
            .create(
                CreateTarget::Independent {
                    ownership: options.ownership,
                },
                CreateOptions {
                    agent: options.agent,
                    init: options.init,
                },
                context,
            )
            .await
    }

    pub async fn get_erased_task(&self, id: TaskId, context: Context) -> pi_js::Result<Option<ErasedTaskRecord>> {
        let storage = Arc::clone(&self.host.storage);
        self.host
            .session
            .read_on_line(move || async move { storage.task(id, context).await })
            .await
    }

    pub async fn get_task<R: DeserializeOwned>(
        &self,
        id: TaskId<R>,
        context: Context,
    ) -> pi_js::Result<Option<TaskRecord<JsonValue, JsonValue, R>>> {
        let raw = self.get_erased_task(id.erased(), context).await?;
        raw.as_ref()
            .map(decode_task_record::<JsonValue, JsonValue, R>)
            .transpose()
    }

    pub async fn inspect(&self, context: Context) -> pi_js::Result<HarnessInspection> {
        let tasks = Arc::clone(&self.host.tasks);
        let registry = Arc::clone(&self.host.options.registry);
        let storage = Arc::clone(&self.host.storage);
        self.host
            .session
            .read_on_line(move || async move {
                let snapshot = registry.snapshot();
                let (scheduling, task_inspections) = tasks.inspect(snapshot.as_ref()).await?;
                let queued = scan_all(|cursor| {
                    let s = Arc::clone(&storage);
                    let c = context.clone();
                    async move {
                        s.scan_submissions(
                            SubmissionQuery {
                                conversation_id: None,
                                status: Some(SubmissionStatus::Queued),
                            },
                            SCAN_PAGE_SIZE,
                            cursor,
                            c,
                        )
                        .await
                    }
                })
                .await?;
                let placed = scan_all(|cursor| {
                    let s = Arc::clone(&storage);
                    let c = context.clone();
                    async move {
                        s.scan_submissions(
                            SubmissionQuery {
                                conversation_id: None,
                                status: Some(SubmissionStatus::Placed),
                            },
                            SCAN_PAGE_SIZE,
                            cursor,
                            c,
                        )
                        .await
                    }
                })
                .await?;
                let mut submissions = Vec::with_capacity(queued.len() + placed.len());
                submissions.extend(queued);
                submissions.extend(placed);
                submissions.sort_by_key(|sub| sub.id.as_i64());
                Ok(HarnessInspection {
                    scheduling,
                    tasks: task_inspections,
                    submissions,
                })
            })
            .await
    }

    pub async fn submission(&self, id: SubmissionId, context: Context) -> pi_js::Result<Option<SubmissionHandle>> {
        self.host.submissions.get(id, context).await
    }

    pub async fn abort_submission(
        &self,
        id: SubmissionId,
        context: Context,
        conversation_id: Option<ConversationId>,
    ) -> pi_js::Result<AbortSubmissionResult> {
        self.host.submissions.abort(id, conversation_id, context).await
    }

    pub async fn abort_task(&self, id: TaskId, context: Context) -> pi_js::Result<AbortTaskResult> {
        self.host.tasks.abort(id, context).await
    }

    pub async fn wait_for_task<R: DeserializeOwned>(
        &self,
        id: TaskId<R>,
        context: Context,
    ) -> pi_js::Result<SettledTask<R>> {
        self.host.tasks.resume();
        let raw = self.host.tasks.wait_for_task(id.erased(), context).await?;
        let outcome = crate::tasks::decode_outcome::<R>(&raw.outcome)?;
        Ok(SettledTask {
            id: raw.id.cast::<R>(),
            conversation_id: raw.conversation_id,
            kind: raw.kind,
            version: raw.version,
            input: raw.input,
            owner: raw.owner,
            background: raw.background,
            abort_requested: raw.abort_requested,
            outcome,
        })
    }

    pub async fn wait_for_idle(&self, context: Context) -> pi_js::Result<()> {
        self.host.tasks.resume();
        self.host.tasks.wait_for_idle(None, context).await
    }

    pub async fn usage(&self, context: Context) -> pi_js::Result<UsageState> {
        let storage = Arc::clone(&self.host.storage);
        let ctx_scan = context.clone();
        let conversations = self
            .host
            .session
            .read_on_line(move || async move {
                scan_all(|cursor| {
                    let s = Arc::clone(&storage);
                    let c = ctx_scan.clone();
                    async move {
                        s.scan_conversations(ConversationQuery::default(), SCAN_PAGE_SIZE, cursor, c)
                            .await
                    }
                })
                .await
            })
            .await?;

        let mut total = UsageState::default();
        for conv in conversations {
            if let Some(state) = self
                .session
                .snapshot_conversation(&*USAGE_DOC, conv.id, context.clone())
                .await?
            {
                add_usage_state(&mut total, &state);
            }
        }
        Ok(total)
    }

    pub async fn task_graph(&self, context: Context) -> pi_js::Result<AttachedReplicatedState<TaskGraph>> {
        self.host.task_graph.state(context).await
    }

    pub async fn watch_task_graph(&self, context: Context) -> pi_js::Result<TaskGraphWatch> {
        self.host.task_graph.watch(context).await
    }

    pub async fn close(&self, context: Context) -> pi_js::Result<()> {
        self.host.closed.store(true, Ordering::Release);
        self.session.close(context).await
    }
}

struct BoundConversationBackend {
    id: ConversationId,
    binding: InvocationBinding,
    submissions: Arc<Submissions>,
    tasks: Arc<TaskScheduler>,
}

#[async_trait]
impl ConversationHandleBackend for BoundConversationBackend {
    fn id(&self) -> ConversationId {
        self.id
    }

    async fn submit(&self, submission: InputSubmissionDraft, context: Context) -> pi_js::Result<SubmissionHandle> {
        self.binding.check()?;
        let bound_ctx = with_abort_signal(self.binding.signal.clone(), context);
        let draft = SubmissionDraft::Input {
            request_id: submission.request_id,
            content: submission.content,
            when_busy: submission.when_busy,
        };
        let handle = self.submissions.submit(self.id, draft, bound_ctx).await?;
        Ok(handle.with_binding(self.binding.clone()))
    }

    async fn abort(&self, context: Context, options: Option<ConversationAbortOptions>) -> pi_js::Result<()> {
        self.binding.check()?;
        let bound_ctx = with_abort_signal(self.binding.signal.clone(), context);
        let background = options.is_some_and(|o| o.background);
        self.tasks.abort_conversation(self.id, background, bound_ctx).await
    }

    async fn wait_for_idle(&self, context: Context) -> pi_js::Result<()> {
        self.binding.check()?;
        let bound_ctx = with_abort_signal(self.binding.signal.clone(), context);
        self.tasks.wait_for_idle(Some(self.id), bound_ctx).await
    }
}

fn bound_conversation(
    id: ConversationId,
    binding: InvocationBinding,
    submissions: Arc<Submissions>,
    tasks: Arc<TaskScheduler>,
) -> ConversationHandle {
    ConversationHandle::from_backend(Arc::new(BoundConversationBackend {
        id,
        binding,
        submissions,
        tasks,
    }))
}
