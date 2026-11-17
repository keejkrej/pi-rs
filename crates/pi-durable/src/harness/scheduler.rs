//! Port of packages/durable/src/harness/scheduler.ts

use std::any::Any;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use async_trait::async_trait;
use futures::FutureExt;
use futures::future::Shared;
use indexmap::{IndexMap, IndexSet};
use pi_ai::models::Models;
use pi_chord::Context;
use pi_chord::context::{await_with_context, with_abort_signal};
use pi_chord::json::copy_json;
use pi_js::abort::{AbortController, AbortReason, AbortSignal};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use crate::documents::ErasedDocDefinition;
use crate::env::ExecutionEnv;
use crate::harness::agent::agent_hooks;
use crate::harness::context::read_context;
use crate::harness::types::{
    Agent, ConversationHandle, DocumentObserverBackend, DocumentReaderBackend, HookApi, HookApiBackend, RegistryReader,
    RegistrySnapshot, SchedulingStatus, Settings, SettledTask, TaskBlockedReason, TaskInspection, TaskInspectionState,
    TaskRuntimeBackend,
};
use crate::harness::util::{Waiters, closed_error, scan_all};
use crate::session::session::SessionImpl;
use crate::session::transaction::{Transaction, TransactionScope, Tx, clone_pi_error};
use crate::tasks::{AnyTask, ErasedTaskRuntime, checkpoint_phase};
use crate::types::{
    CommitChange, CommitPublication, ConversationId, DocumentWatch, EntryId, EntryRecord, ErasedTaskRecord, JoinPolicy,
    JsonObject, JsonValue, NextTaskState, RunningTask, Storage, SubmissionStatus, SubmissionType, TableCommitChange,
    TaskId, TaskOutcome, TaskOutcomeError, TaskQuery, TaskState, TaskStatus,
};

const SCAN_PAGE_SIZE: usize = 256;
const MAX_TIMER_DELAY_MS: i64 = 2_147_483_647;
const LIVE_STATUSES: [TaskStatus; 4] = [
    TaskStatus::Pending,
    TaskStatus::Running,
    TaskStatus::Waiting,
    TaskStatus::Completing,
];

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Result of `Harness::abort_task` / `TaskScheduler::abort`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AbortTaskResult {
    Marked,
    Terminal,
}

/// Terminal outcomes the scheduler writes without running task code.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum SchedulerOutcome {
    Faulted { error: TaskOutcomeError },
    Orphaned { reason: String },
}

impl SchedulerOutcome {
    fn into_task_outcome(self) -> TaskOutcome<JsonValue> {
        match self {
            Self::Faulted { error } => TaskOutcome::Faulted { error },
            Self::Orphaned { reason } => TaskOutcome::Orphaned { reason },
        }
    }
}

/// An invocation a conversation handle is bound to: its signal, and a check that fails once it ended.
#[derive(Clone)]
pub struct InvocationBinding {
    pub signal: AbortSignal,
    check_fn: Arc<dyn Fn() -> pi_js::Result<()> + Send + Sync>,
}

impl InvocationBinding {
    pub fn check(&self) -> pi_js::Result<()> {
        (self.check_fn)()
    }
}

/// Immutable ownership fields of a task.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TaskNode {
    conversation_id: ConversationId,
    owner: Option<TaskId>,
    background: bool,
}

/// Where a walk up the ownership tree continues.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Up {
    Task(TaskId),
    Conversation(ConversationId),
}

/// One step of a walk up.
enum Step {
    Task { id: TaskId, node: TaskNode },
    Conversation(ConversationId),
    Unknown,
}

/// Candidate records a commit staged; they override committed records in ownership walks.
struct Overlay {
    tasks: IndexMap<TaskId, ErasedTaskRecord>,
    edges: IndexMap<ConversationId, Option<TaskId>>,
}

#[derive(Clone, Copy)]
enum Scope {
    Conversation(ConversationId),
    Roots,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum InvocationMode {
    Run,
    Abort,
}

type SharedAgentFuture = Shared<Pin<Box<dyn std::future::Future<Output = Result<Agent, Arc<pi_js::Error>>> + Send>>>;

struct PhaseState {
    snapshot: Arc<dyn RegistrySnapshot>,
    task: AnyTask,
    agent: Option<SharedAgentFuture>,
}

struct Invocation {
    task_id: TaskId,
    conversation_id: ConversationId,
    mode: InvocationMode,
    controller: Arc<AbortController>,
    context: Context,
    next_watch_id: AtomicU64,
    watches: Mutex<IndexMap<u64, DocumentWatch<JsonObject>>>,
    ended: AtomicBool,
    done_tx: watch::Sender<bool>,
    done_rx: watch::Receiver<bool>,
}

impl Invocation {
    async fn wait_done(&self) {
        let mut rx = self.done_rx.clone();
        while !*rx.borrow_and_update() {
            if rx.changed().await.is_err() {
                break;
            }
        }
    }
}

struct Reservation {
    invocation: Arc<Invocation>,
    task: AnyTask,
    snapshot: Arc<dyn RegistrySnapshot>,
}

enum Fit {
    Match {
        task: AnyTask,
        migrates: bool,
    },
    Blocked {
        reason: TaskBlockedReason,
        error: Option<Arc<pi_js::Error>>,
    },
}

enum Resolution {
    Ready { task: AnyTask, record: ErasedTaskRecord },
    Blocked { reason: TaskBlockedReason },
}

struct FailedMigration {
    task: AnyTask,
    error: Arc<pi_js::Error>,
}

struct PhaseResult {
    checkpoint: JsonValue,
    failure: Option<pi_js::Error>,
}

enum Decision {
    Continue,
    End,
    Fault(pi_js::Error),
}

pub type AgentResolverFn = Arc<
    dyn Fn(ConversationId, Arc<dyn RegistrySnapshot>, Context) -> pi_js::BoxFuture<pi_js::Result<Agent>> + Send + Sync,
>;
pub type SettingsFn = Arc<dyn Fn() -> Settings + Send + Sync>;
pub type EnvResolverFn = Arc<
    dyn Fn(ConversationId, Context) -> pi_js::BoxFuture<pi_js::Result<Option<Arc<dyn ExecutionEnv>>>> + Send + Sync,
>;
pub type NowFn = Arc<dyn Fn() -> i64 + Send + Sync>;
pub type ReportFn = Arc<dyn Fn(pi_js::Error) + Send + Sync>;
pub type SettleOutcomeFn = Arc<
    dyn Fn(Arc<Transaction>, ErasedTaskRecord, SchedulerOutcome) -> pi_js::BoxFuture<pi_js::Result<()>> + Send + Sync,
>;
pub type WithdrawInputsFn =
    Arc<dyn Fn(Arc<Transaction>, ConversationId) -> pi_js::BoxFuture<pi_js::Result<()>> + Send + Sync>;
pub type ConversationResolverFn = Arc<
    dyn Fn(ConversationId, InvocationBinding, Context) -> pi_js::BoxFuture<pi_js::Result<Option<ConversationHandle>>>
        + Send
        + Sync,
>;

pub struct TaskSchedulerOptions {
    pub session: Arc<SessionImpl>,
    pub storage: Arc<dyn Storage>,
    pub registry: Arc<dyn RegistryReader>,
    pub models: Models,
    pub agent: AgentResolverFn,
    pub settings: SettingsFn,
    pub env: EnvResolverFn,
    pub now: NowFn,
    pub report: ReportFn,
    pub settle_outcome: SettleOutcomeFn,
    pub withdraw_inputs: WithdrawInputsFn,
    pub conversation: ConversationResolverFn,
    pub context: Context,
}

struct SchedulerState {
    live: IndexMap<TaskId, ErasedTaskRecord>,
    invocations: IndexMap<TaskId, Arc<Invocation>>,
    failed_migrations: IndexMap<TaskId, FailedMigration>,
    edges: IndexMap<ConversationId, Option<TaskId>>,
    conversation_owners: IndexSet<TaskId>,
    settled: IndexMap<TaskId, TaskNode>,
    fail_fast_checks: IndexSet<TaskId>,
    reconcile_scheduled: bool,
    cascade_pending: bool,
    enabled: bool,
    closing: bool,
    dirty: bool,
    draining: bool,
}

impl SchedulerState {
    fn new() -> Self {
        Self {
            live: IndexMap::new(),
            invocations: IndexMap::new(),
            failed_migrations: IndexMap::new(),
            edges: IndexMap::new(),
            conversation_owners: IndexSet::new(),
            settled: IndexMap::new(),
            fail_fast_checks: IndexSet::new(),
            reconcile_scheduled: false,
            cascade_pending: false,
            enabled: false,
            closing: false,
            dirty: false,
            draining: false,
        }
    }

    fn set_edge(&mut self, conversation_id: ConversationId, owner: Option<TaskId>) {
        self.edges.insert(conversation_id, owner);
        if let Some(task_id) = owner {
            self.conversation_owners.insert(task_id);
        }
    }

    fn edge(&self, id: ConversationId, overlay: Option<&Overlay>) -> Option<Option<TaskId>> {
        if let Some(ov) = overlay
            && let Some(owner) = ov.edges.get(&id)
        {
            return Some(*owner);
        }
        self.edges.get(&id).copied()
    }

    fn node(&self, id: TaskId, overlay: Option<&Overlay>) -> Option<TaskNode> {
        if let Some(ov) = overlay
            && let Some(rec) = ov.tasks.get(&id)
        {
            return Some(node_of(rec));
        }
        self.live
            .get(&id)
            .map(node_of)
            .or_else(|| self.settled.get(&id).copied())
    }

    fn collect_above(&self, start: Up, overlay: Option<&Overlay>) -> Vec<Step> {
        let mut steps = Vec::new();
        let mut at = Some(start);
        while let Some(curr) = at {
            match curr {
                Up::Task(task_id) => {
                    let Some(node) = self.node(task_id, overlay) else {
                        steps.push(Step::Unknown);
                        break;
                    };
                    steps.push(Step::Task { id: task_id, node });
                    at = Some(parent_of_node(node));
                }
                Up::Conversation(conversation_id) => {
                    steps.push(Step::Conversation(conversation_id));
                    let Some(edge) = self.edge(conversation_id, overlay) else {
                        steps.push(Step::Unknown);
                        break;
                    };
                    at = edge.map(Up::Task);
                }
            }
        }
        steps
    }

    fn chain_known(&self, start: Up, overlay: Option<&Overlay>) -> bool {
        !self
            .collect_above(start, overlay)
            .into_iter()
            .any(|s| matches!(s, Step::Unknown))
    }

    fn live_records(&self, overlay: Option<&Overlay>) -> Vec<ErasedTaskRecord> {
        let mut out = Vec::new();
        for record in self.live.values() {
            let candidate = overlay.and_then(|ov| ov.tasks.get(&record.id)).unwrap_or(record);
            if !candidate.state.is_terminal() {
                out.push(candidate.clone());
            }
        }
        if let Some(ov) = overlay {
            for record in ov.tasks.values() {
                if !self.live.contains_key(&record.id) && !record.state.is_terminal() {
                    out.push(record.clone());
                }
            }
        }
        out
    }

    fn owned_live(&self, overlay: Option<&Overlay>) -> IndexMap<TaskId, Vec<TaskId>> {
        let mut owned: IndexMap<TaskId, Vec<TaskId>> = IndexMap::new();
        for record in self.live_records(overlay) {
            if record.background {
                continue;
            }
            for step in self.collect_above(parent_of_record(&record), overlay) {
                match step {
                    Step::Unknown => break,
                    Step::Conversation(_) => continue,
                    Step::Task { id, node } => {
                        owned.entry(id).or_default().push(record.id);
                        if node.background {
                            break;
                        }
                    }
                }
            }
        }
        owned
    }

    fn in_scope(&self, start: Up, scope: Scope, cross_background: bool) -> Option<bool> {
        for step in self.collect_above(start, None) {
            match step {
                Step::Unknown => return None,
                Step::Conversation(cid) => {
                    if let Scope::Conversation(target) = scope
                        && cid == target
                    {
                        return Some(true);
                    }
                }
                Step::Task { node, .. } => {
                    if node.background && !cross_background {
                        return Some(false);
                    }
                }
            }
        }
        Some(matches!(scope, Scope::Roots))
    }

    fn below_cancelled(&self, start: Up) -> bool {
        for step in self.collect_above(start, None) {
            match step {
                Step::Unknown => return false,
                Step::Conversation(_) => continue,
                Step::Task { id, node } => {
                    if let Some(live) = self.live.get(&id)
                        && cancellation_intent(live)
                    {
                        return true;
                    }
                    if node.background {
                        return false;
                    }
                }
            }
        }
        false
    }

    fn idle(&self, conversation_id: Option<ConversationId>) -> bool {
        let scope = match conversation_id {
            None => Scope::Roots,
            Some(cid) => Scope::Conversation(cid),
        };
        for record in self.live.values() {
            if !record.background && self.in_scope(parent_of_record(record), scope, false) != Some(false) {
                return false;
            }
        }
        true
    }

    fn waiting_on(&self, record: &ErasedTaskRecord, owned: &IndexMap<TaskId, Vec<TaskId>>) -> Vec<TaskId> {
        if record.abort_requested {
            return owned.get(&record.id).cloned().unwrap_or_default();
        }
        match &record.state {
            TaskState::Waiting { on, .. } => on.iter().copied().filter(|id| self.live.contains_key(id)).collect(),
            _ => Vec::new(),
        }
    }

    fn fit(&self, record: &ErasedTaskRecord, task: Option<&AnyTask>) -> Fit {
        let Some(task) = task else {
            return Fit::Blocked {
                reason: TaskBlockedReason::MissingTask,
                error: None,
            };
        };
        let version = task.version();
        if version == record.version {
            return Fit::Match {
                task: task.clone(),
                migrates: false,
            };
        }
        if version < record.version {
            return Fit::Blocked {
                reason: TaskBlockedReason::TaskTooOld,
                error: None,
            };
        }
        if let Some(failed) = self.failed_migrations.get(&record.id)
            && Arc::ptr_eq(&failed.task.definition, &task.definition)
        {
            return Fit::Blocked {
                reason: TaskBlockedReason::MigrationFailed,
                error: Some(Arc::clone(&failed.error)),
            };
        }
        Fit::Match {
            task: task.clone(),
            migrates: true,
        }
    }

    fn inspect_task(
        &self,
        record: &ErasedTaskRecord,
        snapshot: &dyn RegistrySnapshot,
        owned: &IndexMap<TaskId, Vec<TaskId>>,
    ) -> TaskInspectionState {
        if self.invocations.contains_key(&record.id) {
            return TaskInspectionState::Running;
        }
        if record.state.status() == TaskStatus::Completing {
            return TaskInspectionState::Completing;
        }
        let on = self.waiting_on(record, owned);
        if !on.is_empty() {
            return TaskInspectionState::Waiting { on };
        }
        match self.fit(record, snapshot.task(&record.kind)) {
            Fit::Blocked { reason, error } => TaskInspectionState::Blocked { reason, error },
            Fit::Match { task, migrates } => {
                if migrates && task.definition.migrate.is_none() {
                    return TaskInspectionState::Blocked {
                        reason: TaskBlockedReason::MigrationFailed,
                        error: Some(Arc::new(missing_migration(record, task.version()))),
                    };
                }
                TaskInspectionState::Ready { migrates }
            }
        }
    }

    fn create_invocation(
        &mut self,
        record: &ErasedTaskRecord,
        mode: InvocationMode,
        base_context: &Context,
    ) -> Arc<Invocation> {
        let controller = Arc::new(AbortController::new());
        let (done_tx, done_rx) = watch::channel(false);
        let invocation = Arc::new(Invocation {
            task_id: record.id,
            conversation_id: record.conversation_id,
            mode,
            context: with_abort_signal(controller.signal(), base_context.clone()),
            controller,
            next_watch_id: AtomicU64::new(1),
            watches: Mutex::new(IndexMap::new()),
            ended: AtomicBool::new(false),
            done_tx,
            done_rx,
        });
        self.invocations.insert(record.id, Arc::clone(&invocation));
        invocation
    }
}

/// Durable task scheduler of one Harness.
pub struct TaskScheduler {
    session: Arc<SessionImpl>,
    storage: Arc<dyn Storage>,
    registry: Arc<dyn RegistryReader>,
    models: Models,
    agent: AgentResolverFn,
    settings: SettingsFn,
    env: EnvResolverFn,
    now: NowFn,
    report: ReportFn,
    settle_outcome: SettleOutcomeFn,
    withdraw_inputs: WithdrawInputsFn,
    conversation: ConversationResolverFn,
    context: Context,
    state: Mutex<SchedulerState>,
    task_waiters: Arc<Waiters<TaskId, SettledTask<JsonValue>>>,
    idle_waiters: Arc<Waiters<Option<ConversationId>, ()>>,
    subscriptions: Mutex<(
        Option<pi_js::Unsubscribe>,
        Option<pi_js::Unsubscribe>,
        Option<pi_js::Unsubscribe>,
    )>,
}

impl TaskScheduler {
    pub fn new(options: TaskSchedulerOptions) -> Arc<Self> {
        Arc::new(Self {
            session: options.session,
            storage: options.storage,
            registry: options.registry,
            models: options.models,
            agent: options.agent,
            settings: options.settings,
            env: options.env,
            now: options.now,
            report: options.report,
            settle_outcome: options.settle_outcome,
            withdraw_inputs: options.withdraw_inputs,
            conversation: options.conversation,
            context: options.context,
            state: Mutex::new(SchedulerState::new()),
            task_waiters: Arc::new(Waiters::new()),
            idle_waiters: Arc::new(Waiters::new()),
            subscriptions: Mutex::new((None, None, None)),
        })
    }

    /// Load live tasks and change surviving `running` tasks back to `pending`. Dispatches nothing.
    pub async fn open(self: &Arc<Self>, context: Context) -> pi_js::Result<()> {
        let weak_commits = Arc::downgrade(self);
        let sub_commits = self.session.subscribe_commits(Arc::new(move |publication, _| {
            if let Some(scheduler) = weak_commits.upgrade() {
                scheduler.observe(publication);
            }
        }))?;

        let weak_close = Arc::downgrade(self);
        let sub_close = self.session.subscribe_close(Arc::new(move || {
            if let Some(scheduler) = weak_close.upgrade() {
                scheduler.seal();
            }
        }))?;

        let weak_reg = Arc::downgrade(self);
        let sub_reg = self.registry.subscribe(Arc::new(move || {
            if let Some(scheduler) = weak_reg.upgrade() {
                scheduler.kick();
            }
        }));

        *lock(&self.subscriptions) = (Some(sub_commits), Some(sub_close), Some(sub_reg));

        let this = Arc::clone(self);
        let storage = Arc::clone(&self.storage);
        self.session
            .commit_with(
                move |tx| async move {
                    let mut scans = Vec::with_capacity(LIVE_STATUSES.len());
                    for status in LIVE_STATUSES {
                        let query = TaskQuery {
                            status: Some(status),
                            ..Default::default()
                        };
                        let tx_ref = &tx;
                        let records = scan_all(|cursor| {
                            let q = query.clone();
                            async move { tx_ref.scan_tasks(q, SCAN_PAGE_SIZE, cursor).await }
                        })
                        .await?;
                        scans.push(records);
                    }
                    let mut updates = Vec::new();
                    {
                        let mut st = lock(&this.state);
                        for records in scans {
                            for record in records {
                                st.live.insert(record.id, record.clone());
                                match &record.state {
                                    TaskState::Running { checkpoint } => {
                                        updates.push(with_state(
                                            record.clone(),
                                            TaskState::Pending {
                                                checkpoint: checkpoint.clone(),
                                            },
                                        )?);
                                    }
                                    TaskState::Waiting {
                                        policy: JoinPolicy::FailFast,
                                        ..
                                    } => {
                                        st.fail_fast_checks.insert(record.id);
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                    drop(storage);
                    for updated in updates {
                        tx.set_task(updated)?;
                    }
                    Ok(())
                },
                context,
                TransactionScope::default(),
            )
            .await?;

        lock(&self.state).cascade_pending = true;
        self.schedule_reconcile();
        Ok(())
    }

    /// Enable scheduling. Idempotent; the kick does nothing once closing.
    pub fn resume(self: &Arc<Self>) {
        lock(&self.state).enabled = true;
        self.kick();
    }

    /// Wait for every invocation signalled by `seal()`. Writes nothing.
    pub async fn join(&self) {
        let invocations: Vec<Arc<Invocation>> = lock(&self.state).invocations.values().cloned().collect();
        for invocation in invocations {
            invocation.wait_done().await;
        }
    }

    /// Commit the abort mark, or settle a task that no registered definition can take as `orphaned`
    /// when nothing it owns is live, then join the run invocation seen on the line.
    pub async fn abort(self: &Arc<Self>, id: TaskId, context: Context) -> pi_js::Result<AbortTaskResult> {
        let this = Arc::clone(self);
        let (result, run_invocation) = self
            .session
            .commit_with(
                move |tx| async move {
                    let Some(current) = tx.task(id).await? else {
                        return Err(pi_js::Error::msg(format!("Task {id} does not exist")));
                    };
                    if current.state.is_terminal() {
                        return Ok((AbortTaskResult::Terminal, None));
                    }
                    let existing_inv = lock(&this.state).invocations.get(&id).cloned();
                    if existing_inv.is_none() && current.state.status() != TaskStatus::Completing {
                        this.load_scopes(false).await?;
                        let should_orphan = {
                            let st = lock(&this.state);
                            let owned = st.owned_live(None);
                            if owned.contains_key(&id) {
                                None
                            } else {
                                let snapshot = this.registry.snapshot();
                                let res = this.resolve(&st, &current, snapshot.as_ref()).0;
                                match res {
                                    Resolution::Blocked { reason } => Some(reason),
                                    Resolution::Ready { .. } => None,
                                }
                            }
                        };
                        if let Some(reason) = should_orphan {
                            this.terminate(
                                &tx,
                                current,
                                SchedulerOutcome::Orphaned {
                                    reason: block_reason_str(reason).to_owned(),
                                },
                            )
                            .await?;
                            return Ok((AbortTaskResult::Marked, None));
                        }
                    }
                    if !current.abort_requested {
                        let mut marked = current;
                        marked.abort_requested = true;
                        tx.set_task(marked)?;
                    }
                    let run_inv = existing_inv.filter(|inv| inv.mode == InvocationMode::Run);
                    Ok((AbortTaskResult::Marked, run_inv))
                },
                context.clone(),
                TransactionScope::default(),
            )
            .await?;

        if let Some(run) = run_invocation {
            await_with_context(
                async move {
                    run.wait_done().await;
                    Ok(())
                },
                context,
            )
            .await?;
        }
        Ok(result)
    }

    pub async fn wait_for_task(
        self: &Arc<Self>,
        id: TaskId,
        context: Context,
    ) -> pi_js::Result<SettledTask<JsonValue>> {
        let this = Arc::clone(self);
        let storage = Arc::clone(&self.storage);
        let task_waiters = Arc::clone(&self.task_waiters);
        let ctx_for_line = context.clone();
        let outcome = self
            .session
            .read_on_line(move || async move {
                {
                    let st = lock(&this.state);
                    if st.closing {
                        return Err(closed_error());
                    }
                    if st.live.contains_key(&id) {
                        let fut = task_waiters.register(id, ctx_for_line)?;
                        return Ok(WaitOutcome::Pending(fut));
                    }
                }
                let record = storage
                    .task(id, ctx_for_line)
                    .await?
                    .ok_or_else(|| pi_js::Error::msg(format!("Task {id} does not exist")))?;
                let settled = SettledTask::from_record(record)?;
                Ok(WaitOutcome::Settled(settled))
            })
            .await?;
        match outcome {
            WaitOutcome::Settled(settled) => Ok(settled),
            WaitOutcome::Pending(fut) => fut.await,
        }
    }

    pub async fn wait_for_idle(
        self: &Arc<Self>,
        conversation_id: Option<ConversationId>,
        context: Context,
    ) -> pi_js::Result<()> {
        let fut = {
            let st = lock(&self.state);
            if st.closing {
                return Err(closed_error());
            }
            if st.idle(conversation_id) {
                return Ok(());
            }
            self.idle_waiters.register(conversation_id, context)?
        };
        self.schedule_reconcile();
        fut.await
    }

    pub async fn abort_conversation(
        self: &Arc<Self>,
        conversation_id: ConversationId,
        background: bool,
        context: Context,
    ) -> pi_js::Result<()> {
        let this = Arc::clone(self);
        let reached = self
            .session
            .commit_with(
                move |tx| async move {
                    let queued = this.load_scopes(true).await?;
                    let scope = Scope::Conversation(conversation_id);
                    let (to_mark, to_withdraw) = {
                        let st = lock(&this.state);
                        let mut to_mark = Vec::new();
                        let mut reached = Vec::new();
                        for record in st.live.values() {
                            if record.background && !background {
                                continue;
                            }
                            if st.in_scope(parent_of_record(record), scope, background) != Some(true) {
                                continue;
                            }
                            reached.push(record.id);
                            if !record.abort_requested {
                                let mut next = record.clone();
                                next.abort_requested = true;
                                to_mark.push(next);
                            }
                        }
                        let mut to_withdraw = Vec::new();
                        for cid in queued {
                            if st.in_scope(Up::Conversation(cid), scope, background) == Some(true) {
                                to_withdraw.push(cid);
                            }
                        }
                        ((reached, to_mark), to_withdraw)
                    };
                    for rec in to_mark.1 {
                        tx.set_task(rec)?;
                    }
                    for cid in to_withdraw {
                        (this.withdraw_inputs)(Arc::clone(&tx), cid).await?;
                    }
                    Ok(to_mark.0)
                },
                context.clone(),
                TransactionScope::default(),
            )
            .await?;

        if background {
            for id in reached {
                self.wait_for_task(id, context.clone()).await?;
            }
        }
        self.wait_for_idle(Some(conversation_id), context).await
    }

    /// Inspect scheduling state and every live task (must be called on the Session line).
    pub async fn inspect(
        self: &Arc<Self>,
        snapshot: &dyn RegistrySnapshot,
    ) -> pi_js::Result<(SchedulingStatus, Vec<TaskInspection>)> {
        self.load_scopes(false).await?;
        let st = lock(&self.state);
        let owned = st.owned_live(None);
        let mut tasks = Vec::with_capacity(st.live.len());
        for record in st.live.values() {
            let state = st.inspect_task(record, snapshot, &owned);
            tasks.push(TaskInspection {
                record: record.clone(),
                state,
            });
        }
        let scheduling = if st.closing {
            SchedulingStatus::Closing
        } else if st.enabled {
            SchedulingStatus::Running
        } else {
            SchedulingStatus::Paused
        };
        Ok((scheduling, tasks))
    }

    // ─── Scheduling & Publication Observer ───────────────────────────────────

    fn observe(self: &Arc<Self>, publication: &CommitPublication) {
        let mut changed = false;
        let mut to_abort: Vec<Arc<AbortController>> = Vec::new();
        let mut to_resolve: Vec<(TaskId, SettledTask<JsonValue>)> = Vec::new();
        let mut schedule_reconcile = false;

        {
            let mut st = lock(&self.state);
            let mut updated: Vec<ErasedTaskRecord> = Vec::new();
            let mut failed: Vec<TaskId> = Vec::new();

            for change in &publication.changes {
                let CommitChange::Table(TableCommitChange::Task(record)) = change else {
                    continue;
                };
                changed = true;
                let previous = st.live.get(&record.id).cloned();
                if failed_outcome(record) && previous.as_ref().is_none_or(|p| !failed_outcome(p)) {
                    failed.push(record.id);
                }
                if record.state.is_terminal() {
                    st.live.shift_remove(&record.id);
                    st.failed_migrations.shift_remove(&record.id);
                    st.fail_fast_checks.shift_remove(&record.id);
                    if st.conversation_owners.contains(&record.id) {
                        st.settled.insert(record.id, node_of(record));
                    }
                    if let Ok(settled) = SettledTask::from_record(record.clone()) {
                        to_resolve.push((record.id, settled));
                    }
                    schedule_reconcile = true;
                    continue;
                }
                if record.abort_requested && previous.as_ref().is_none_or(|p| !p.abort_requested) {
                    st.cascade_pending = true;
                    if let Some(inv) = st.invocations.get(&record.id)
                        && inv.mode == InvocationMode::Run
                    {
                        to_abort.push(Arc::clone(&inv.controller));
                    }
                }
                let status = record.state.status();
                if status == TaskStatus::Completing
                    && previous
                        .as_ref()
                        .is_none_or(|p| p.state.status() != TaskStatus::Completing)
                {
                    if cancellation_intent(record) {
                        st.cascade_pending = true;
                    }
                    schedule_reconcile = true;
                }
                if matches!(
                    &record.state,
                    TaskState::Waiting {
                        policy: JoinPolicy::FailFast,
                        ..
                    }
                ) && previous
                    .as_ref()
                    .is_none_or(|p| p.state.status() != TaskStatus::Waiting)
                {
                    st.fail_fast_checks.insert(record.id);
                    schedule_reconcile = true;
                }
                st.live.insert(record.id, record.clone());
                updated.push(record.clone());
            }

            let mut to_mark = Vec::new();
            for id in failed {
                for record in st.live.values() {
                    if let TaskState::Waiting {
                        on,
                        policy: JoinPolicy::FailFast,
                        ..
                    } = &record.state
                        && on.contains(&id)
                    {
                        to_mark.push(record.id);
                    }
                }
            }
            for id in to_mark {
                st.fail_fast_checks.insert(id);
                schedule_reconcile = true;
            }

            for change in &publication.changes {
                if let CommitChange::Table(TableCommitChange::Conversation(conv)) = change
                    && !st.edges.contains_key(&conv.id)
                {
                    st.set_edge(conv.id, conv.owner.map(|o| o.task_id));
                }
            }

            for change in &publication.changes {
                if let CommitChange::Table(TableCommitChange::Submission(sub)) = change
                    && sub.status() == SubmissionStatus::Queued
                    && sub.kind() == SubmissionType::Input
                {
                    let up = Up::Conversation(sub.conversation_id);
                    if !st.chain_known(up, None) || st.below_cancelled(up) {
                        st.cascade_pending = true;
                    }
                }
            }

            for record in updated {
                let parent = parent_of_record(&record);
                if !st.chain_known(parent, None) {
                    schedule_reconcile = true;
                } else if !record.background && !record.abort_requested && st.below_cancelled(parent) {
                    st.cascade_pending = true;
                }
            }

            if st.cascade_pending {
                schedule_reconcile = true;
            }
        }

        for (task_id, settled) in to_resolve {
            self.task_waiters.resolve(&task_id, settled);
        }
        for controller in to_abort {
            controller.abort(Some(AbortReason::abort()));
        }
        if schedule_reconcile {
            self.schedule_reconcile();
        }
        if changed {
            self.resolve_idle_waiters();
            self.kick();
        }
    }

    fn resolve_idle_waiters(&self) {
        let keys = self.idle_waiters.keys();
        let resolved: Vec<Option<ConversationId>> = {
            let st = lock(&self.state);
            keys.into_iter().filter(|cid| st.idle(*cid)).collect()
        };
        for cid in resolved {
            self.idle_waiters.resolve(&cid, ());
        }
    }

    fn schedule_reconcile(self: &Arc<Self>) {
        let should_spawn = {
            let mut st = lock(&self.state);
            if st.reconcile_scheduled || st.closing {
                false
            } else {
                st.reconcile_scheduled = true;
                true
            }
        };
        if should_spawn && let Ok(handle) = tokio::runtime::Handle::try_current() {
            let this = Arc::clone(self);
            handle.spawn(async move {
                this.reconcile().await;
            });
        }
    }

    async fn reconcile(self: &Arc<Self>) {
        let (cascade, checks) = {
            let mut st = lock(&self.state);
            st.reconcile_scheduled = false;
            let cascade = st.cascade_pending;
            st.cascade_pending = false;
            let checks: Vec<TaskId> = st.fail_fast_checks.drain(..).collect();
            (cascade, checks)
        };

        let this = Arc::clone(self);
        let checks_for_commit = checks.clone();
        let res = self
            .session
            .commit_with(
                move |tx| async move {
                    if lock(&this.state).closing {
                        return Ok(());
                    }
                    let queued = this.load_scopes(cascade).await?;

                    let mut marked: IndexSet<TaskId> = IndexSet::new();
                    let mut marks_to_write: Vec<ErasedTaskRecord> = Vec::new();
                    let mut fail_fast_candidates: Vec<(Vec<TaskId>, Vec<TaskId>)> = Vec::new();
                    let withdraw_list: Vec<ConversationId>;

                    {
                        let st = lock(&this.state);
                        for record in st.live.values() {
                            if !record.background
                                && st.below_cancelled(parent_of_record(record))
                                && !record.abort_requested
                                && marked.insert(record.id)
                            {
                                let mut next = record.clone();
                                next.abort_requested = true;
                                marks_to_write.push(next);
                            }
                        }
                        for id in &checks_for_commit {
                            if let Some(waiter) = st.live.get(id)
                                && let TaskState::Waiting { on, .. } = &waiter.state
                            {
                                fail_fast_candidates.push((on.clone(), on.clone()));
                            }
                        }
                        withdraw_list = queued
                            .into_iter()
                            .filter(|cid| st.below_cancelled(Up::Conversation(*cid)))
                            .collect();
                    }

                    for (on_check, members) in fail_fast_candidates {
                        if this.any_failed(&on_check).await? {
                            let st = lock(&this.state);
                            for member in members {
                                if let Some(record) = st.live.get(&member)
                                    && !failed_outcome(record)
                                    && !record.abort_requested
                                    && marked.insert(record.id)
                                {
                                    let mut next = record.clone();
                                    next.abort_requested = true;
                                    marks_to_write.push(next);
                                }
                            }
                        }
                    }

                    for record in marks_to_write {
                        tx.set_task(record)?;
                    }
                    for cid in withdraw_list {
                        (this.withdraw_inputs)(Arc::clone(&tx), cid).await?;
                    }
                    this.finalize(&tx).await?;
                    Ok(())
                },
                self.context.clone(),
                TransactionScope::default(),
            )
            .await;

        if let Err(err) = res {
            let report_err = {
                let mut st = lock(&self.state);
                st.cascade_pending = true;
                for id in checks {
                    st.fail_fast_checks.insert(id);
                }
                !st.closing
            };
            if report_err {
                (self.report)(err);
            }
        }
        self.resolve_idle_waiters();
    }

    async fn any_failed(&self, ids: &[TaskId]) -> pi_js::Result<bool> {
        for &id in ids {
            let cached = lock(&self.state).live.get(&id).cloned();
            let record = match cached {
                Some(rec) => Some(rec),
                None => self.storage.task(id, self.context.clone()).await?,
            };
            if record.as_ref().is_some_and(failed_outcome) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn finalize(&self, tx: &Arc<Transaction>) -> pi_js::Result<()> {
        loop {
            let overlay = overlay_of(tx);
            let done = {
                let st = lock(&self.state);
                let owned = st.owned_live(Some(&overlay));
                st.live_records(Some(&overlay))
                    .into_iter()
                    .filter(|record| record.state.status() == TaskStatus::Completing && !owned.contains_key(&record.id))
                    .collect::<Vec<_>>()
            };
            if done.is_empty() {
                return Ok(());
            }
            for record in done {
                let Some(outcome) = record.state.outcome().cloned() else {
                    continue;
                };
                let terminal = with_state(
                    record.clone(),
                    TaskState::Terminal {
                        outcome: outcome.clone(),
                    },
                )?;
                tx.set_task(terminal)?;
                match outcome {
                    TaskOutcome::Faulted { error } => {
                        (self.settle_outcome)(Arc::clone(tx), record, SchedulerOutcome::Faulted { error }).await?;
                    }
                    TaskOutcome::Orphaned { reason } => {
                        (self.settle_outcome)(Arc::clone(tx), record, SchedulerOutcome::Orphaned { reason }).await?;
                    }
                    _ => {}
                }
            }
        }
    }

    async fn load_scopes(&self, queued: bool) -> pi_js::Result<Vec<ConversationId>> {
        let starts: Vec<Up> = {
            let st = lock(&self.state);
            st.live
                .values()
                .map(parent_of_record)
                .filter(|parent| !st.chain_known(*parent, None))
                .collect()
        };
        for start in starts {
            self.load_chain(start, None).await?;
        }
        if !queued {
            return Ok(Vec::new());
        }
        let submissions = scan_all(|cursor| {
            self.storage.scan_submissions(
                crate::types::SubmissionQuery {
                    conversation_id: None,
                    status: Some(SubmissionStatus::Queued),
                },
                SCAN_PAGE_SIZE,
                cursor,
                self.context.clone(),
            )
        })
        .await?;
        let mut conversations = IndexSet::new();
        for submission in submissions {
            conversations.insert(submission.conversation_id);
        }
        let list: Vec<ConversationId> = conversations.into_iter().collect();
        for &cid in &list {
            self.load_chain(Up::Conversation(cid), None).await?;
        }
        Ok(list)
    }

    async fn load_chain(&self, start: Up, overlay: Option<&Overlay>) -> pi_js::Result<()> {
        let mut at = Some(start);
        while let Some(curr) = at {
            match curr {
                Up::Task(task_id) => {
                    let existing = lock(&self.state).node(task_id, overlay);
                    let node = match existing {
                        Some(n) => n,
                        None => {
                            let Some(record) = self.storage.task(task_id, self.context.clone()).await? else {
                                return Ok(());
                            };
                            let n = node_of(&record);
                            if record.state.is_terminal() {
                                lock(&self.state).settled.insert(record.id, n);
                            }
                            n
                        }
                    };
                    at = Some(parent_of_node(node));
                }
                Up::Conversation(conversation_id) => {
                    let existing = lock(&self.state).edge(conversation_id, overlay);
                    let edge = match existing {
                        Some(e) => e,
                        None => {
                            let record = self.storage.conversation(conversation_id, self.context.clone()).await?;
                            let e = record.and_then(|r| r.owner.map(|o| o.task_id));
                            lock(&self.state).set_edge(conversation_id, e);
                            e
                        }
                    };
                    at = edge.map(Up::Task);
                }
            }
        }
        Ok(())
    }

    fn resolve(
        &self,
        st: &SchedulerState,
        record: &ErasedTaskRecord,
        snapshot: &dyn RegistrySnapshot,
    ) -> (Resolution, Option<(TaskId, FailedMigration)>, Option<pi_js::Error>) {
        match st.fit(record, snapshot.task(&record.kind)) {
            Fit::Blocked { reason, .. } => (Resolution::Blocked { reason }, None, None),
            Fit::Match { task, migrates } => {
                if !migrates {
                    return (
                        Resolution::Ready {
                            task,
                            record: record.clone(),
                        },
                        None,
                        None,
                    );
                }
                let Some(migrate) = &task.definition.migrate else {
                    let err = Arc::new(missing_migration(record, task.version()));
                    return (
                        Resolution::Blocked {
                            reason: TaskBlockedReason::MigrationFailed,
                        },
                        Some((
                            record.id,
                            FailedMigration {
                                task,
                                error: Arc::clone(&err),
                            },
                        )),
                        Some(clone_pi_error(&err)),
                    );
                };
                let old_cp = record.state.checkpoint().cloned().unwrap_or(JsonValue::Null);
                match migrate(record.input.clone(), old_cp, record.version) {
                    Ok((new_input, new_cp)) => {
                        let copied_input = match copy_json(&new_input, None) {
                            Ok(v) => v,
                            Err(err) => {
                                let arc_err = Arc::new(clone_pi_error(&err));
                                return (
                                    Resolution::Blocked {
                                        reason: TaskBlockedReason::MigrationFailed,
                                    },
                                    Some((
                                        record.id,
                                        FailedMigration {
                                            task,
                                            error: Arc::clone(&arc_err),
                                        },
                                    )),
                                    Some(err),
                                );
                            }
                        };
                        let copied_cp = match copy_json(&new_cp, None) {
                            Ok(v) => v,
                            Err(err) => {
                                let arc_err = Arc::new(clone_pi_error(&err));
                                return (
                                    Resolution::Blocked {
                                        reason: TaskBlockedReason::MigrationFailed,
                                    },
                                    Some((
                                        record.id,
                                        FailedMigration {
                                            task,
                                            error: Arc::clone(&arc_err),
                                        },
                                    )),
                                    Some(err),
                                );
                            }
                        };
                        let mut next_state = record.state.clone();
                        match &mut next_state {
                            TaskState::Pending { checkpoint }
                            | TaskState::Running { checkpoint }
                            | TaskState::Waiting { checkpoint, .. } => *checkpoint = copied_cp,
                            TaskState::Completing { .. } | TaskState::Terminal { .. } => {}
                        }
                        let mut migrated_record = record.clone();
                        migrated_record.version = task.version();
                        migrated_record.input = copied_input;
                        migrated_record.state = next_state;
                        (
                            Resolution::Ready {
                                task,
                                record: migrated_record,
                            },
                            None,
                            None,
                        )
                    }
                    Err(err) => {
                        let arc_err = Arc::new(clone_pi_error(&err));
                        (
                            Resolution::Blocked {
                                reason: TaskBlockedReason::MigrationFailed,
                            },
                            Some((
                                record.id,
                                FailedMigration {
                                    task,
                                    error: Arc::clone(&arc_err),
                                },
                            )),
                            Some(err),
                        )
                    }
                }
            }
        }
    }

    fn seal(self: &Arc<Self>) {
        let (controllers, done_error) = {
            let mut st = lock(&self.state);
            st.closing = true;
            let ctrls: Vec<Arc<AbortController>> =
                st.invocations.values().map(|inv| Arc::clone(&inv.controller)).collect();
            (ctrls, closed_error())
        };
        let (u0, u1, u2) = {
            let mut subs = lock(&self.subscriptions);
            (subs.0.take(), subs.1.take(), subs.2.take())
        };
        for u in [u0, u1, u2].into_iter().flatten() {
            u.call();
        }
        self.task_waiters.reject_all(&done_error);
        self.idle_waiters.reject_all(&done_error);
        for ctrl in controllers {
            ctrl.abort(Some(AbortReason::abort()));
        }
    }

    fn kick(self: &Arc<Self>) {
        let should_spawn = {
            let mut st = lock(&self.state);
            st.dirty = true;
            if st.draining || !st.enabled || st.closing {
                false
            } else {
                st.draining = true;
                true
            }
        };
        if should_spawn && let Ok(handle) = tokio::runtime::Handle::try_current() {
            let this = Arc::clone(self);
            handle.spawn(async move {
                tokio::task::yield_now().await;
                this.drain().await;
            });
        }
    }

    async fn drain(self: &Arc<Self>) {
        let result: pi_js::Result<()> = async {
            loop {
                {
                    let mut st = lock(&self.state);
                    if !st.dirty || !st.enabled || st.closing {
                        st.draining = false;
                        return Ok(());
                    }
                    st.dirty = false;
                }
                for reservation in self.reserve().await? {
                    self.start(reservation);
                }
            }
        }
        .await;

        let report_err = {
            let mut st = lock(&self.state);
            st.draining = false;
            !st.closing
        };
        if let Err(err) = result
            && report_err
        {
            (self.report)(err);
        }
        let rekick = {
            let st = lock(&self.state);
            st.dirty && st.enabled && !st.closing
        };
        if rekick {
            self.kick();
        }
    }

    async fn reserve(self: &Arc<Self>) -> pi_js::Result<Vec<Reservation>> {
        let this = Arc::clone(self);
        let reserved = Arc::new(Mutex::new(Vec::<Reservation>::new()));
        let reserved_cb = Arc::clone(&reserved);

        let res = self
            .session
            .commit_with(
                move |tx| async move {
                    let (enabled, closing) = {
                        let st = lock(&this.state);
                        (st.enabled, st.closing)
                    };
                    if !enabled || closing {
                        return Ok(());
                    }
                    this.load_scopes(false).await?;
                    let mut snapshot_opt: Option<Arc<dyn RegistrySnapshot>> = None;
                    let mut candidates_to_orphan = Vec::new();
                    let mut tasks_to_write = Vec::new();
                    let mut new_reservations = Vec::new();

                    {
                        let mut st = lock(&this.state);
                        let owned = st.owned_live(None);
                        let live_snapshot: Vec<ErasedTaskRecord> = st.live.values().cloned().collect();
                        let mut pending_migrations = Vec::new();
                        let mut reports = Vec::new();

                        for record in live_snapshot {
                            if st.invocations.contains_key(&record.id) || !st.waiting_on(&record, &owned).is_empty() {
                                continue;
                            }
                            if record.state.status() == TaskStatus::Completing {
                                continue;
                            }
                            let mode = if record.abort_requested {
                                InvocationMode::Abort
                            } else {
                                InvocationMode::Run
                            };
                            let snapshot = snapshot_opt.get_or_insert_with(|| this.registry.snapshot()).clone();
                            let (resolution, failed_mig, rep_err) = this.resolve(&st, &record, snapshot.as_ref());
                            if let Some((tid, fm)) = failed_mig {
                                pending_migrations.push((tid, fm));
                            }
                            if let Some(err) = rep_err {
                                reports.push(err);
                            }
                            match resolution {
                                Resolution::Blocked { reason } => {
                                    if mode == InvocationMode::Abort {
                                        candidates_to_orphan.push((record, reason));
                                    }
                                    continue;
                                }
                                Resolution::Ready {
                                    task,
                                    record: resolved_rec,
                                } => {
                                    let changed_record = resolved_rec != record;
                                    if changed_record || record.state.status() != TaskStatus::Running {
                                        let cp = resolved_rec.state.checkpoint().cloned().unwrap_or(JsonValue::Null);
                                        tasks_to_write
                                            .push(with_state(resolved_rec, TaskState::Running { checkpoint: cp })?);
                                    }
                                    let invocation = st.create_invocation(&record, mode, &this.context);
                                    new_reservations.push(Reservation {
                                        invocation,
                                        task,
                                        snapshot,
                                    });
                                }
                            }
                        }

                        for (tid, fm) in pending_migrations {
                            st.failed_migrations.insert(tid, fm);
                        }
                        drop(st);
                        for err in reports {
                            (this.report)(err);
                        }
                    }

                    for (record, reason) in candidates_to_orphan {
                        this.terminate(
                            &tx,
                            record,
                            SchedulerOutcome::Orphaned {
                                reason: block_reason_str(reason).to_owned(),
                            },
                        )
                        .await?;
                    }
                    for updated in tasks_to_write {
                        tx.set_task(updated)?;
                    }
                    *lock(&reserved_cb) = new_reservations;
                    Ok(())
                },
                self.context.clone(),
                TransactionScope::default(),
            )
            .await;

        match res {
            Ok(()) => Ok(std::mem::take(&mut *lock(&reserved))),
            Err(err) => {
                let to_cleanup = std::mem::take(&mut *lock(&reserved));
                for r in to_cleanup {
                    self.end_invocation(&r.invocation);
                    let _ = r.invocation.done_tx.send(true);
                }
                Err(err)
            }
        }
    }

    fn start(self: &Arc<Self>, reservation: Reservation) {
        let this = Arc::clone(self);
        let invocation = Arc::clone(&reservation.invocation);
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let res = match invocation.mode {
                    InvocationMode::Run => this.run(reservation).await,
                    InvocationMode::Abort => this.run_abort(reservation).await,
                };
                if let Err(err) = res {
                    (this.report)(err);
                }
                this.end_invocation(&invocation);
                let _ = invocation.done_tx.send(true);
                this.kick();
            });
        } else {
            self.end_invocation(&invocation);
            let _ = invocation.done_tx.send(true);
        }
    }

    async fn run(self: &Arc<Self>, reservation: Reservation) -> pi_js::Result<()> {
        let invocation = reservation.invocation;
        let phase_state = Arc::new(Mutex::new(PhaseState {
            snapshot: reservation.snapshot,
            task: reservation.task,
            agent: None,
        }));
        let mut reported: Option<Option<AnyTask>> = None;
        let runtime_erased = self.make_erased_runtime(Arc::clone(&invocation), Arc::clone(&phase_state));
        let mut previous: Option<PhaseResult> = None;

        loop {
            let current = self
                .step(&invocation, |tx, current| {
                    self.decide(tx, current, previous.as_ref(), &phase_state, &mut reported)
                })
                .await;
            let Some(current_record) = current else {
                return Ok(());
            };
            if lock(&self.state).closing {
                return Ok(());
            }
            let checkpoint = current_record.state.checkpoint().cloned().unwrap_or(JsonValue::Null);
            let task_def = {
                let mut ps = lock(&phase_state);
                ps.agent = None;
                ps.task.clone()
            };
            let phase_name = checkpoint_phase(&checkpoint).map(str::to_owned);
            let running = to_running_task(&current_record)?;
            let outcome = match phase_name
                .as_deref()
                .and_then(|name| task_def.definition.phases.get(name))
            {
                Some(handler) => handler(running, Arc::clone(&runtime_erased), invocation.context.clone()).await,
                None => Err(pi_js::Error::msg(format!(
                    "Task {} has no phase {:?}",
                    current_record.kind,
                    phase_name.as_deref().unwrap_or("")
                ))),
            };
            previous = Some(match outcome {
                Ok(()) => PhaseResult {
                    checkpoint,
                    failure: None,
                },
                Err(err) => PhaseResult {
                    checkpoint,
                    failure: Some(err),
                },
            });
        }
    }

    fn decide(
        &self,
        tx: &Arc<Transaction>,
        current: &ErasedTaskRecord,
        previous: Option<&PhaseResult>,
        phase_state: &Arc<Mutex<PhaseState>>,
        reported: &mut Option<Option<AnyTask>>,
    ) -> pi_js::Result<Decision> {
        if current.abort_requested {
            return Ok(Decision::End);
        }
        let Some(prev) = previous else {
            return Ok(Decision::Continue);
        };
        if let Some(err) = &prev.failure {
            return Ok(Decision::Fault(clone_pi_error(err)));
        }
        let current_cp = current.state.checkpoint().unwrap_or(&JsonValue::Null);
        if json_equal(current_cp, &prev.checkpoint) {
            let phase_name = checkpoint_phase(&prev.checkpoint).unwrap_or("");
            return Ok(Decision::Fault(pi_js::Error::msg(format!(
                "Task {} phase {} returned without durable progress",
                current.kind, phase_name
            ))));
        }

        let next_snapshot = self.registry.snapshot();
        let next_task = next_snapshot.task(&current.kind).cloned();
        let prev_task = {
            let mut ps = lock(phase_state);
            ps.snapshot = Arc::clone(&next_snapshot);
            ps.task.clone()
        };

        let same_def = next_task
            .as_ref()
            .is_some_and(|next| Arc::ptr_eq(&next.definition, &prev_task.definition));
        if !same_def {
            if let Some(ref candidate) = next_task
                && can_reserve(candidate, current)
            {
                tx.set_task(with_state(
                    current.clone(),
                    TaskState::Pending {
                        checkpoint: current_cp.clone(),
                    },
                )?)?;
                return Ok(Decision::End);
            }
            let already_reported = match reported {
                None => false,
                Some(prev_rep) => {
                    prev_rep.as_ref().map(|t| Arc::as_ptr(&t.definition))
                        == next_task.as_ref().map(|t| Arc::as_ptr(&t.definition))
                }
            };
            if !already_reported {
                *reported = Some(next_task.clone());
                let cause = if next_task.is_none() {
                    "missing_task"
                } else {
                    "incompatible_task"
                };
                (self.report)(pi_js::Error::msg(format!(
                    "Task {} keeps running under its old {} definition ({cause})",
                    current.id, current.kind
                )));
            }
        }
        Ok(Decision::Continue)
    }

    async fn run_abort(self: &Arc<Self>, reservation: Reservation) -> pi_js::Result<()> {
        let invocation = reservation.invocation;
        let current_opt = {
            let st = lock(&self.state);
            if st.closing {
                None
            } else {
                st.live.get(&invocation.task_id).cloned()
            }
        };
        let Some(current) = current_opt else {
            return Ok(());
        };
        let phase_state = Arc::new(Mutex::new(PhaseState {
            snapshot: reservation.snapshot,
            task: reservation.task.clone(),
            agent: None,
        }));
        let runtime_erased = self.make_erased_runtime(Arc::clone(&invocation), phase_state);
        let running = to_running_task(&current)?;
        let abort_res = (reservation.task.definition.abort)(running, runtime_erased, invocation.context.clone()).await;
        let fault = match abort_res {
            Err(err) => err,
            Ok(()) => pi_js::Error::msg(format!(
                "Abort handler of task {} returned without a terminal outcome",
                invocation.task_id
            )),
        };
        let _ = self.step(&invocation, |_tx, _current| Ok(Decision::Fault(fault))).await;
        Ok(())
    }

    async fn step<F>(self: &Arc<Self>, invocation: &Arc<Invocation>, decide: F) -> Option<ErasedTaskRecord>
    where
        F: FnOnce(&Arc<Transaction>, &ErasedTaskRecord) -> pi_js::Result<Decision> + Send,
    {
        let this = Arc::clone(self);
        let inv = Arc::clone(invocation);
        let res = self
            .session
            .commit_with(
                move |tx| async move {
                    let (current_opt, closing) = {
                        let st = lock(&this.state);
                        let found = st.live.get(&inv.task_id).cloned();
                        let current = found.filter(|r| r.state.status() == TaskStatus::Running);
                        (current, st.closing)
                    };
                    let decision = match (&current_opt, closing) {
                        (Some(curr), false) => decide(&tx, curr)?,
                        _ => Decision::End,
                    };
                    match decision {
                        Decision::Continue => Ok(current_opt),
                        Decision::End => {
                            this.end_invocation(&inv);
                            Ok(None)
                        }
                        Decision::Fault(err) => {
                            this.end_invocation(&inv);
                            if let Some(curr) = current_opt {
                                this.terminate(
                                    &tx,
                                    curr,
                                    SchedulerOutcome::Faulted {
                                        error: TaskOutcomeError {
                                            message: err.message(),
                                            detail: None,
                                        },
                                    },
                                )
                                .await?;
                            }
                            Ok(None)
                        }
                    }
                },
                self.context.clone(),
                TransactionScope::default(),
            )
            .await;

        match res {
            Ok(current) => current,
            Err(err) => {
                self.end_invocation(invocation);
                if !lock(&self.state).closing {
                    (self.report)(err);
                }
                None
            }
        }
    }

    async fn terminate(
        &self,
        tx: &Arc<Transaction>,
        record: ErasedTaskRecord,
        outcome: SchedulerOutcome,
    ) -> pi_js::Result<()> {
        self.load_scopes(false).await?;
        let overlay = overlay_of(tx);
        let has_owned_live = lock(&self.state).owned_live(Some(&overlay)).contains_key(&record.id);
        let raw_outcome = outcome.clone().into_task_outcome();
        if has_owned_live {
            tx.set_task(with_state(record, TaskState::Completing { outcome: raw_outcome })?)?;
            return Ok(());
        }
        tx.set_task(with_state(
            record.clone(),
            TaskState::Terminal { outcome: raw_outcome },
        )?)?;
        (self.settle_outcome)(Arc::clone(tx), record, outcome).await
    }

    async fn commit_state(
        &self,
        tx: &Arc<Transaction>,
        invocation: &Arc<Invocation>,
        current: &ErasedTaskRecord,
        next: NextTaskState<JsonValue, JsonValue>,
    ) -> pi_js::Result<()> {
        if let NextTaskState::Waiting { ref on, policy, .. } = next {
            self.validate_wait(tx, invocation, current, on, policy).await?;
        }
        if let NextTaskState::Terminal { outcome } = next {
            let overlay = overlay_of(tx);
            self.load_scopes(false).await?;
            for rec in overlay.tasks.values() {
                self.load_chain(parent_of_record(rec), Some(&overlay)).await?;
            }
            let has_owned_live = lock(&self.state).owned_live(Some(&overlay)).contains_key(&current.id);
            if has_owned_live {
                tx.set_task(with_state(current.clone(), TaskState::Completing { outcome })?)?;
            } else {
                tx.set_task(with_state(current.clone(), TaskState::Terminal { outcome })?)?;
            }
            return Ok(());
        }
        let next_state = match next {
            NextTaskState::Running { checkpoint } => TaskState::Running { checkpoint },
            NextTaskState::Waiting { checkpoint, on, policy } => TaskState::Waiting { checkpoint, on, policy },
            NextTaskState::Terminal { outcome } => TaskState::Terminal { outcome },
        };
        tx.set_task(with_state(current.clone(), next_state)?)
    }

    async fn validate_wait(
        &self,
        tx: &Arc<Transaction>,
        invocation: &Arc<Invocation>,
        current: &ErasedTaskRecord,
        on: &[TaskId],
        policy: JoinPolicy,
    ) -> pi_js::Result<()> {
        if invocation.mode == InvocationMode::Abort {
            return Err(pi_js::Error::msg(format!(
                "Abort handler of task {} cannot wait",
                current.id
            )));
        }
        let overlay = overlay_of(tx);
        self.load_chain(parent_of_record(current), None).await?;
        let owners: IndexSet<TaskId> = {
            let st = lock(&self.state);
            st.collect_above(parent_of_record(current), None)
                .into_iter()
                .filter_map(|step| match step {
                    Step::Task { id, .. } => Some(id),
                    _ => None,
                })
                .collect()
        };
        for &id in on {
            if id == current.id || owners.contains(&id) {
                return Err(pi_js::Error::msg(format!(
                    "Task {} cannot wait on itself or its owner {}",
                    current.id, id
                )));
            }
            let member = if let Some(staged) = overlay.tasks.get(&id).cloned() {
                staged
            } else if let Some(live) = lock(&self.state).live.get(&id).cloned() {
                live
            } else {
                self.storage
                    .task(id, self.context.clone())
                    .await?
                    .ok_or_else(|| pi_js::Error::msg(format!("Task {id} does not exist")))?
            };
            if policy == JoinPolicy::FailFast && member.owner != Some(current.id) {
                return Err(pi_js::Error::msg(format!(
                    "Task {} can wait failFast only on tasks it owns; {} is not one",
                    current.id, id
                )));
            }
        }
        Ok(())
    }

    fn end_invocation(&self, invocation: &Arc<Invocation>) {
        if invocation.ended.swap(true, Ordering::AcqRel) {
            return;
        }
        {
            let mut st = lock(&self.state);
            if st
                .invocations
                .get(&invocation.task_id)
                .is_some_and(|curr| Arc::ptr_eq(curr, invocation))
            {
                st.invocations.shift_remove(&invocation.task_id);
            }
        }
        let watches: Vec<DocumentWatch<JsonObject>> = {
            let mut guard = lock(&invocation.watches);
            guard.drain(..).map(|(_, w)| w).collect()
        };
        for w in watches {
            w.stop_now();
        }
        invocation
            .controller
            .abort(Some(AbortReason::from_error(&ended_error(invocation.task_id))));
    }

    fn make_erased_runtime(
        self: &Arc<Self>,
        invocation: Arc<Invocation>,
        phase: Arc<Mutex<PhaseState>>,
    ) -> Arc<dyn ErasedTaskRuntime> {
        let backend: Arc<dyn TaskRuntimeBackend> = Arc::new(SchedulerRuntime {
            scheduler: Arc::clone(self),
            invocation,
            phase,
        });
        Arc::new(ErasedRuntimeAdapter { backend })
    }
}

enum WaitOutcome {
    Pending(pi_js::BoxFuture<pi_js::Result<SettledTask<JsonValue>>>),
    Settled(SettledTask<JsonValue>),
}

struct ErasedRuntimeAdapter {
    backend: Arc<dyn TaskRuntimeBackend>,
}

#[async_trait]
impl ErasedTaskRuntime for ErasedRuntimeAdapter {
    fn as_any(&self) -> &(dyn Any + Send + Sync) {
        &self.backend
    }
}

#[derive(Clone)]
struct SchedulerRuntime {
    scheduler: Arc<TaskScheduler>,
    invocation: Arc<Invocation>,
    phase: Arc<Mutex<PhaseState>>,
}

impl SchedulerRuntime {
    fn check_active(&self) -> pi_js::Result<()> {
        if self.invocation.ended.load(Ordering::Acquire) {
            Err(ended_error(self.invocation.task_id))
        } else {
            Ok(())
        }
    }

    async fn read<T, F, Fut>(&self, read: F) -> pi_js::Result<T>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = pi_js::Result<T>>,
    {
        self.check_active()?;
        read().await
    }

    async fn resolve_phase_agent(&self, caller_context: Context) -> pi_js::Result<Agent> {
        self.check_active()?;
        let shared = {
            let mut ps = lock(&self.phase);
            if let Some(ref fut) = ps.agent {
                fut.clone()
            } else {
                let agent_fn = Arc::clone(&self.scheduler.agent);
                let cid = self.invocation.conversation_id;
                let snapshot = Arc::clone(&ps.snapshot);
                let inv_ctx = self.invocation.context.clone();
                let fut: SharedAgentFuture = async move { (agent_fn)(cid, snapshot, inv_ctx).await.map_err(Arc::new) }
                    .boxed()
                    .shared();
                ps.agent = Some(fut.clone());
                fut
            }
        };
        await_with_context(
            async move { shared.await.map_err(|e| clone_pi_error(&e)) },
            caller_context,
        )
        .await
    }

    async fn gated<T, F, Fut>(&self, change: F, context: Context) -> pi_js::Result<T>
    where
        F: FnOnce(Arc<Transaction>, ErasedTaskRecord) -> Fut + Send,
        Fut: std::future::Future<Output = pi_js::Result<T>> + Send,
        T: Send,
    {
        self.check_active()?;
        let scheduler = Arc::clone(&self.scheduler);
        let inv = Arc::clone(&self.invocation);
        let scope = TransactionScope {
            conversation_id: Some(inv.conversation_id),
            task_id: Some(inv.task_id),
        };
        self.scheduler
            .session
            .commit_with(
                move |tx| async move {
                    if inv.ended.load(Ordering::Acquire) {
                        return Err(ended_error(inv.task_id));
                    }
                    let current = {
                        let st = lock(&scheduler.state);
                        if st.closing {
                            return Err(closed_error());
                        }
                        let Some(found) = st.live.get(&inv.task_id).cloned() else {
                            return Err(pi_js::Error::msg(format!("Task {} is terminal", inv.task_id)));
                        };
                        if found.state.status() != TaskStatus::Running {
                            return Err(pi_js::Error::msg(format!(
                                "Task {} is {:?}",
                                inv.task_id,
                                found.state.status()
                            )));
                        }
                        if inv.mode == InvocationMode::Run && found.abort_requested {
                            return Err(pi_js::Error::msg(format!(
                                "Task {} has a durable abort mark",
                                inv.task_id
                            )));
                        }
                        found
                    };
                    change(tx, current).await
                },
                context,
                scope,
            )
            .await
    }

    async fn sleep_until(&self, until: i64, context: Context) -> pi_js::Result<()> {
        self.check_active()?;
        let inv_sig = self.invocation.controller.signal();
        let signal = match context.abort_signal() {
            Some(ctx_sig) => AbortSignal::any(&[inv_sig, ctx_sig]),
            None => inv_sig,
        };
        loop {
            signal.throw_if_aborted()?;
            let remaining = until - (self.scheduler.now)();
            if remaining <= 0 {
                return Ok(());
            }
            let delay_ms = remaining.min(MAX_TIMER_DELAY_MS) as u64;
            pi_js::time::sleep(delay_ms, Some(&signal)).await?;
        }
    }
}

#[async_trait]
impl DocumentReaderBackend for SchedulerRuntime {
    async fn snapshot_erased(
        &self,
        def: &Arc<ErasedDocDefinition>,
        conversation_id: Option<ConversationId>,
        task_id: Option<TaskId>,
        key: Option<&str>,
        context: Context,
    ) -> pi_js::Result<Option<JsonObject>> {
        self.read(|| {
            self.scheduler
                .session
                .snapshot_erased(def, conversation_id, task_id, key, context)
        })
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
        self.read(|| {
            self.scheduler
                .session
                .snapshot_as_of_erased(def, conversation_id, key, at, context)
        })
        .await
    }
}

#[async_trait]
impl DocumentObserverBackend for SchedulerRuntime {
    async fn watch_erased(
        &self,
        def: &Arc<ErasedDocDefinition>,
        conversation_id: Option<ConversationId>,
        task_id: Option<TaskId>,
        key: Option<&str>,
        context: Context,
    ) -> pi_js::Result<Option<DocumentWatch<JsonObject>>> {
        self.check_active()?;
        let Some(watch) = self
            .scheduler
            .session
            .watch_doc_erased(def, conversation_id, task_id, key, context)
            .await?
        else {
            return Ok(None);
        };
        if self.invocation.ended.load(Ordering::Acquire) {
            let _ = watch.stop().await;
            return Err(ended_error(self.invocation.task_id));
        }
        let watch_id = self.invocation.next_watch_id.fetch_add(1, Ordering::AcqRel);
        lock(&self.invocation.watches).insert(watch_id, watch.clone());
        let inv = Arc::clone(&self.invocation);
        let watch_for_close = watch.clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let _ = watch_for_close.closed().await;
                lock(&inv.watches).shift_remove(&watch_id);
            });
        }
        Ok(Some(watch))
    }
}

#[async_trait]
impl HookApiBackend for SchedulerRuntime {
    fn task_id(&self) -> TaskId {
        self.invocation.task_id
    }

    fn conversation_id(&self) -> ConversationId {
        self.invocation.conversation_id
    }

    async fn memo_get(&self, name: &str, context: Context) -> pi_js::Result<Option<JsonValue>> {
        <Self as TaskRuntimeBackend>::memo_get(self, name, context).await
    }

    async fn memo_set(&self, name: &str, candidate: JsonValue, context: Context) -> pi_js::Result<JsonValue> {
        <Self as TaskRuntimeBackend>::memo_set(self, name, candidate, context).await
    }
}

#[async_trait]
impl TaskRuntimeBackend for SchedulerRuntime {
    fn task_id(&self) -> TaskId {
        self.invocation.task_id
    }

    fn conversation_id(&self) -> ConversationId {
        self.invocation.conversation_id
    }

    fn signal(&self) -> AbortSignal {
        self.invocation.controller.signal()
    }

    fn registry(&self) -> Arc<dyn RegistrySnapshot> {
        Arc::clone(&lock(&self.phase).snapshot)
    }

    async fn agent(&self, context: Context) -> pi_js::Result<Agent> {
        self.resolve_phase_agent(context).await
    }

    fn settings(&self) -> Settings {
        (self.scheduler.settings)()
    }

    fn models(&self) -> &Models {
        &self.scheduler.models
    }

    async fn env(&self, context: Context) -> pi_js::Result<Option<Arc<dyn ExecutionEnv>>> {
        self.read(|| (self.scheduler.env)(self.invocation.conversation_id, context))
            .await
    }

    async fn hooks_for_task(&self, task_name: &str) -> pi_js::Result<Vec<Arc<dyn Any + Send + Sync>>> {
        let agent = self.resolve_phase_agent(self.invocation.context.clone()).await?;
        Ok(agent_hooks(&agent, task_name))
    }

    async fn commit_erased(
        &self,
        change: Box<
            dyn FnOnce(
                    Tx,
                    RunningTask<JsonValue, JsonValue, JsonValue>,
                ) -> pi_js::BoxFuture<pi_js::Result<Option<NextTaskState<JsonValue, JsonValue>>>>
                + Send,
        >,
        context: Context,
    ) -> pi_js::Result<()> {
        let scheduler = Arc::clone(&self.scheduler);
        let inv = Arc::clone(&self.invocation);
        self.gated(
            move |tx, current_record| async move {
                let running = to_running_task(&current_record)?;
                if let Some(next) = change(Tx::from_transaction(Arc::clone(&tx)), running).await? {
                    scheduler.commit_state(&tx, &inv, &current_record, next).await?;
                }
                Ok(())
            },
            context,
        )
        .await
    }

    async fn memo_get(&self, name: &str, _context: Context) -> pi_js::Result<Option<JsonValue>> {
        self.read(|| async {
            let st = lock(&self.scheduler.state);
            Ok(st.live.get(&self.invocation.task_id).and_then(|rec| memo_of(rec, name)))
        })
        .await
    }

    async fn memo_set(&self, name: &str, candidate: JsonValue, context: Context) -> pi_js::Result<JsonValue> {
        let key = name.to_owned();
        self.gated(
            move |tx, mut current| async move {
                if let Some(winner) = memo_of(&current, &key) {
                    return Ok(winner);
                }
                let mut memos = current.memos.take().unwrap_or_default();
                memos.insert(key, candidate.clone());
                current.memos = Some(memos);
                tx.set_task(current)?;
                Ok(candidate)
            },
            context,
        )
        .await
    }

    async fn get_task(&self, id: TaskId, context: Context) -> pi_js::Result<Option<ErasedTaskRecord>> {
        self.read(|| {
            let session = Arc::clone(&self.scheduler.session);
            let storage = Arc::clone(&self.scheduler.storage);
            async move {
                session
                    .read_on_line(move || async move { storage.task(id, context).await })
                    .await
            }
        })
        .await
    }

    async fn wait_for_task(&self, id: TaskId, context: Context) -> pi_js::Result<SettledTask<JsonValue>> {
        self.read(|| {
            let bound_ctx = with_abort_signal(self.invocation.controller.signal(), context);
            self.scheduler.wait_for_task(id, bound_ctx)
        })
        .await
    }

    async fn outcomes(&self, ids: &[TaskId], context: Context) -> pi_js::Result<Vec<TaskOutcome<JsonValue>>> {
        self.read(|| {
            let session = Arc::clone(&self.scheduler.session);
            let storage = Arc::clone(&self.scheduler.storage);
            let ids = ids.to_vec();
            async move {
                session
                    .read_on_line(move || async move {
                        let mut outcomes = Vec::with_capacity(ids.len());
                        for id in ids {
                            let rec = storage.task(id, context.clone()).await?;
                            match rec.map(|r| r.state) {
                                Some(TaskState::Terminal { outcome }) => outcomes.push(outcome),
                                _ => return Err(pi_js::Error::msg(format!("Task {id} is not terminal"))),
                            }
                        }
                        Ok(outcomes)
                    })
                    .await
            }
        })
        .await
    }

    async fn conversation(&self, id: ConversationId, context: Context) -> pi_js::Result<Option<ConversationHandle>> {
        self.read(|| {
            let inv_check = Arc::clone(&self.invocation);
            let binding = InvocationBinding {
                signal: self.invocation.controller.signal(),
                check_fn: Arc::new(move || {
                    if inv_check.ended.load(Ordering::Acquire) {
                        Err(ended_error(inv_check.task_id))
                    } else {
                        Ok(())
                    }
                }),
            };
            (self.scheduler.conversation)(id, binding, context)
        })
        .await
    }

    async fn entry(&self, id: EntryId, context: Context) -> pi_js::Result<Option<EntryRecord>> {
        self.read(|| {
            let session = Arc::clone(&self.scheduler.session);
            let storage = Arc::clone(&self.scheduler.storage);
            let cid = self.invocation.conversation_id;
            async move {
                let lookup = session
                    .read_on_line(move || async move { storage.entry_in_conversation(cid, id, context).await })
                    .await?;
                Ok(lookup.map(|l| l.entry))
            }
        })
        .await
    }

    async fn context_view(
        &self,
        conversation_id: ConversationId,
        context: Context,
        at: Option<EntryId>,
    ) -> pi_js::Result<crate::harness::types::ContextView> {
        self.read(|| {
            read_context(
                &self.scheduler.session,
                &self.scheduler.storage,
                conversation_id,
                context,
                at,
            )
        })
        .await
    }

    fn now(&self) -> pi_js::Result<i64> {
        self.check_active()?;
        Ok((self.scheduler.now)())
    }

    fn report(&self, error: pi_js::Error) {
        if self.check_active().is_ok() {
            (self.scheduler.report)(error);
        }
    }

    async fn sleep(&self, until: i64, context: Context) -> pi_js::Result<()> {
        self.sleep_until(until, context).await
    }

    fn hook_api(&self) -> HookApi {
        HookApi::from_backend(Arc::new(self.clone()))
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn cancellation_intent(record: &ErasedTaskRecord) -> bool {
    !record.state.is_terminal() && (record.abort_requested || failed_outcome(record))
}

fn failed_outcome(record: &ErasedTaskRecord) -> bool {
    matches!(
        &record.state,
        TaskState::Completing { outcome } | TaskState::Terminal { outcome }
            if !matches!(outcome, TaskOutcome::Completed { .. })
    )
}

fn parent_of_node(node: TaskNode) -> Up {
    match node.owner {
        Some(owner) => Up::Task(owner),
        None => Up::Conversation(node.conversation_id),
    }
}

fn parent_of_record(record: &ErasedTaskRecord) -> Up {
    match record.owner {
        Some(owner) => Up::Task(owner),
        None => Up::Conversation(record.conversation_id),
    }
}

fn node_of(record: &ErasedTaskRecord) -> TaskNode {
    TaskNode {
        conversation_id: record.conversation_id,
        owner: record.owner,
        background: record.background,
    }
}

fn overlay_of(tx: &Arc<Transaction>) -> Overlay {
    let mut tasks = IndexMap::new();
    for rec in tx.staged_tasks() {
        tasks.insert(rec.id, rec);
    }
    let mut edges = IndexMap::new();
    for conv in tx.staged_conversations() {
        edges.insert(conv.id, conv.owner.map(|o| o.task_id));
    }
    Overlay { tasks, edges }
}

fn memo_of(record: &ErasedTaskRecord, name: &str) -> Option<JsonValue> {
    record.memos.as_ref().and_then(|m| m.get(name)).cloned()
}

fn to_running_task(record: &ErasedTaskRecord) -> pi_js::Result<RunningTask<JsonValue, JsonValue, JsonValue>> {
    let checkpoint = record
        .state
        .checkpoint()
        .cloned()
        .ok_or_else(|| pi_js::Error::msg(format!("Task {} has no active checkpoint", record.id)))?;
    Ok(RunningTask {
        id: record.id,
        conversation_id: record.conversation_id,
        kind: record.kind.clone(),
        version: record.version,
        input: record.input.clone(),
        owner: record.owner,
        background: record.background,
        abort_requested: record.abort_requested,
        checkpoint,
        memos: record.memos.clone(),
    })
}

fn missing_migration(record: &ErasedTaskRecord, version: i64) -> pi_js::Error {
    pi_js::Error::msg(format!(
        "Task {} version {} has no migration from {}",
        record.kind, version, record.version
    ))
}

fn ended_error(task_id: TaskId) -> pi_js::Error {
    pi_js::Error::msg(format!("Task {task_id} invocation has ended"))
}

fn block_reason_str(reason: TaskBlockedReason) -> &'static str {
    match reason {
        TaskBlockedReason::MissingTask => "missing_task",
        TaskBlockedReason::TaskTooOld => "task_too_old",
        TaskBlockedReason::MigrationFailed => "migration_failed",
    }
}

fn with_state(mut record: ErasedTaskRecord, state: TaskState<JsonValue, JsonValue>) -> pi_js::Result<ErasedTaskRecord> {
    record.state = match state {
        TaskState::Pending { checkpoint } => TaskState::Pending {
            checkpoint: copy_json(&checkpoint, None)?,
        },
        TaskState::Running { checkpoint } => TaskState::Running {
            checkpoint: copy_json(&checkpoint, None)?,
        },
        TaskState::Waiting { checkpoint, on, policy } => TaskState::Waiting {
            checkpoint: copy_json(&checkpoint, None)?,
            on,
            policy,
        },
        TaskState::Completing { outcome } => {
            record.memos = None;
            TaskState::Completing { outcome }
        }
        TaskState::Terminal { outcome } => {
            record.memos = None;
            TaskState::Terminal { outcome }
        }
    };
    Ok(record)
}

fn can_reserve(task: &AnyTask, record: &ErasedTaskRecord) -> bool {
    let version = task.version();
    version == record.version || (version > record.version && task.definition.migrate.is_some())
}

fn json_equal(left: &JsonValue, right: &JsonValue) -> bool {
    match (left, right) {
        (JsonValue::Null, JsonValue::Null) => true,
        (JsonValue::Bool(a), JsonValue::Bool(b)) => a == b,
        (JsonValue::Number(a), JsonValue::Number(b)) => a == b || (a.as_f64().is_some() && a.as_f64() == b.as_f64()),
        (JsonValue::String(a), JsonValue::String(b)) => a == b,
        (JsonValue::Array(a), JsonValue::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| json_equal(x, y))
        }
        (JsonValue::Object(a), JsonValue::Object(b)) => {
            a.len() == b.len() && a.iter().all(|(k, v)| b.get(k).is_some_and(|rv| json_equal(v, rv)))
        }
        _ => false,
    }
}
