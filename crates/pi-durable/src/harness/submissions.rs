//! Port of packages/durable/src/harness/submissions.ts

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use pi_ai::types::{Message, UserMessage};
use pi_chord::Context;
use pi_chord::json::{copy_json, is_json_value};

use crate::entries::USER_ENTRY;
use crate::errors::ConversationBusy;
use crate::harness::generation::start_run;
use crate::harness::inbox::{
    BoundaryAt, INBOX_DOC, InboxItem, QueueModes, apply_boundary, is_stale, prepare_boundary, remove_inbox_item,
};
use crate::harness::live::LIVE_DOC;
use crate::harness::scheduler::InvocationBinding;
use pi_chord::context::with_abort_signal;

use crate::harness::types::{AbortSubmissionResult, SettledSubmissionRecord, SubmissionDraft, WhenBusy};
use crate::harness::util::{Waiters, closed_error};
use crate::session::session::SessionImpl;
use crate::session::transaction::Tx;
use crate::types::{
    CommitChange, CommitPublication, ConversationId, EntryDraft, InputSubmissionState, Storage, SubmissionCreate,
    SubmissionId, SubmissionRecord, SubmissionSettlement, SubmissionState, SubmissionStatus, SubmissionType,
    TableCommitChange, TypedEntryDraft, WriteSubmissionState,
};

pub type QueueModesFn = Arc<dyn Fn() -> QueueModes + Send + Sync>;
pub type ResumeFn = Arc<dyn Fn() + Send + Sync>;
pub type NowFn = Arc<dyn Fn() -> i64 + Send + Sync>;

/// Admission, waits, and withdrawal of the durable submissions of one Harness.
pub struct Submissions {
    session: Arc<SessionImpl>,
    storage: Arc<dyn Storage>,
    now: NowFn,
    queue_modes: QueueModesFn,
    resume: ResumeFn,
    waiters: Arc<Waiters<SubmissionId, SubmissionRecord>>,
    closed: Arc<AtomicBool>,
    _sub_commits: pi_js::Unsubscribe,
    _sub_close: pi_js::Unsubscribe,
}

impl Submissions {
    pub fn new(
        session: Arc<SessionImpl>,
        storage: Arc<dyn Storage>,
        now: NowFn,
        queue_modes: QueueModesFn,
        resume: ResumeFn,
    ) -> pi_js::Result<Arc<Self>> {
        let waiters = Arc::new(Waiters::new());
        let closed = Arc::new(AtomicBool::new(false));

        let waiters_for_commits = Arc::clone(&waiters);
        let sub_commits = session.subscribe_commits(Arc::new(move |publication, _context| {
            observe_publication(&waiters_for_commits, publication);
        }))?;

        let waiters_for_close = Arc::clone(&waiters);
        let closed_for_close = Arc::clone(&closed);
        let sub_close = session.subscribe_close(Arc::new(move || {
            closed_for_close.store(true, Ordering::Release);
            waiters_for_close.reject_all(&closed_error());
        }))?;

        Ok(Arc::new(Self {
            session,
            storage,
            now,
            queue_modes,
            resume,
            waiters,
            closed,
            _sub_commits: sub_commits,
            _sub_close: sub_close,
        }))
    }

    /// Admit a submission in one commit; see `admit_submission()`.
    pub async fn submit(
        self: &Arc<Self>,
        conversation_id: ConversationId,
        draft: SubmissionDraft,
        context: Context,
    ) -> pi_js::Result<SubmissionHandle> {
        (self.resume)();
        let now = (self.now)();
        let queue_modes = Arc::clone(&self.queue_modes);
        let id = self
            .session
            .commit(
                move |tx| async move {
                    let modes = (queue_modes)();
                    admit_submission(&tx, conversation_id, draft, now, modes).await
                },
                context,
            )
            .await?;
        Ok(SubmissionHandle {
            id,
            submissions: Arc::clone(self),
            binding: None,
        })
    }

    /// Handle for an existing submission, or `None`.
    pub async fn get(self: &Arc<Self>, id: SubmissionId, context: Context) -> pi_js::Result<Option<SubmissionHandle>> {
        let storage = Arc::clone(&self.storage);
        let record = self
            .session
            .read_on_line(move || async move { storage.submission(id, context).await })
            .await?;
        Ok(record.map(|r| SubmissionHandle {
            id: r.id,
            submissions: Arc::clone(self),
            binding: None,
        }))
    }

    pub async fn status(&self, id: SubmissionId, context: Context) -> pi_js::Result<SubmissionRecord> {
        let storage = Arc::clone(&self.storage);
        let record = self
            .session
            .read_on_line(move || async move { storage.submission(id, context).await })
            .await?;
        record.ok_or_else(|| pi_js::Error::msg(format!("Submission {id} does not exist")))
    }

    pub async fn wait(&self, id: SubmissionId, context: Context) -> pi_js::Result<SettledSubmissionRecord> {
        (self.resume)();
        let storage = Arc::clone(&self.storage);
        let waiters = Arc::clone(&self.waiters);
        let closed = Arc::clone(&self.closed);
        let ctx_for_line = context.clone();
        let outcome = self
            .session
            .read_on_line(move || async move {
                let record = storage
                    .submission(id, ctx_for_line.clone())
                    .await?
                    .ok_or_else(|| pi_js::Error::msg(format!("Submission {id} does not exist")))?;
                if record.is_settled() {
                    return Ok(Either::Left(record));
                }
                if closed.load(Ordering::Acquire) {
                    return Err(closed_error());
                }
                let fut = waiters.register(id, ctx_for_line)?;
                Ok(Either::Right(fut))
            })
            .await?;
        match outcome {
            Either::Left(record) => Ok(SettledSubmissionRecord { record }),
            Either::Right(fut) => Ok(SettledSubmissionRecord { record: fut.await? }),
        }
    }

    /// Withdraw a queued submission and remove its inbox item; placed inputs and settled submissions are reported.
    pub async fn abort(
        &self,
        id: SubmissionId,
        conversation_id: Option<ConversationId>,
        context: Context,
    ) -> pi_js::Result<AbortSubmissionResult> {
        self.session
            .commit(
                move |tx| async move {
                    let Some(record) = tx.submission(id).await? else {
                        return Ok(AbortSubmissionResult::NotFound);
                    };
                    if let Some(cid) = conversation_id
                        && record.conversation_id != cid
                    {
                        return Ok(AbortSubmissionResult::NotFound);
                    }
                    match record.status() {
                        SubmissionStatus::Queued => {
                            tx.settle_submission(
                                id,
                                SubmissionSettlement::Unanswered {
                                    reason: "aborted".to_owned(),
                                    detail: None,
                                },
                            )?;
                            remove_inbox_item(&tx, record.conversation_id, id).await?;
                            Ok(AbortSubmissionResult::Aborted)
                        }
                        SubmissionStatus::Placed => Ok(AbortSubmissionResult::AlreadyPlaced),
                        SubmissionStatus::Done | SubmissionStatus::Unanswered => Ok(AbortSubmissionResult::Settled),
                    }
                },
                context,
            )
            .await
    }
}

enum Either<L, R> {
    Left(L),
    Right(R),
}

fn observe_publication(waiters: &Waiters<SubmissionId, SubmissionRecord>, publication: &CommitPublication) {
    for change in &publication.changes {
        if let CommitChange::Table(TableCommitChange::Submission(record)) = change
            && record.is_settled()
        {
            waiters.resolve(&record.id, record.clone());
        }
    }
}

/// Invocation-safe handle to a submitted conversation request.
#[derive(Clone)]
pub struct SubmissionHandle {
    id: SubmissionId,
    submissions: Arc<Submissions>,
    binding: Option<InvocationBinding>,
}

impl SubmissionHandle {
    pub fn id(&self) -> SubmissionId {
        self.id
    }

    pub fn with_binding(mut self, binding: InvocationBinding) -> Self {
        self.binding = Some(binding);
        self
    }

    fn bind_context(&self, context: Context) -> pi_js::Result<Context> {
        if let Some(ref binding) = self.binding {
            binding.check()?;
            Ok(with_abort_signal(binding.signal.clone(), context))
        } else {
            Ok(context)
        }
    }

    pub async fn status(&self, context: Context) -> pi_js::Result<SubmissionRecord> {
        let context = self.bind_context(context)?;
        self.submissions.status(self.id, context).await
    }

    pub async fn wait(&self, context: Context) -> pi_js::Result<SettledSubmissionRecord> {
        let context = self.bind_context(context)?;
        self.submissions.wait(self.id, context).await
    }

    pub async fn abort(&self, context: Context) -> pi_js::Result<AbortSubmissionResult> {
        let context = self.bind_context(context)?;
        let result = self.submissions.abort(self.id, None, context).await?;
        if result == AbortSubmissionResult::NotFound {
            return Err(pi_js::Error::msg(format!("Submission {} does not exist", self.id)));
        }
        Ok(result)
    }
}

/// Admit a submission inside a commit (spec §6); `Conversation::submit` and conversation-owned
/// compactions share it.
pub async fn admit_submission(
    tx: &Tx,
    conversation_id: ConversationId,
    draft: SubmissionDraft,
    now: i64,
    queue_modes: QueueModes,
) -> pi_js::Result<SubmissionId> {
    let draft_type = match &draft {
        SubmissionDraft::Input { .. } => SubmissionType::Input,
        SubmissionDraft::Write { .. } => SubmissionType::Write,
    };
    let request_id = match &draft {
        SubmissionDraft::Input { request_id, .. } | SubmissionDraft::Write { request_id, .. } => request_id.clone(),
    };

    if let Some(ref req_id) = request_id
        && let Some(existing) = tx.submission_by_request(conversation_id, req_id).await?
    {
        if existing.kind() != draft_type {
            let label = match existing.kind() {
                SubmissionType::Input => "input",
                SubmissionType::Write => "write",
            };
            return Err(pi_js::Error::msg(format!(
                "Request {req_id} already identifies a submission of type {label}"
            )));
        }
        return Ok(existing.id);
    }

    let live_draft = tx.doc_conversation(&*LIVE_DOC, conversation_id).await?;
    let mut live = live_draft.get()?;
    let busy = live.run.is_some();
    if busy
        && let SubmissionDraft::Input {
            when_busy: Some(WhenBusy::Reject),
            ..
        } = &draft
    {
        return Err(ConversationBusy::new(conversation_id).into_error());
    }

    // A boundary reads the table, so it is prepared before the first table write; a busy one needs none.
    let mut boundary = if busy {
        None
    } else {
        Some(prepare_boundary(tx, conversation_id, queue_modes).await?)
    };
    let boundary_has_items = match &boundary {
        Some(b) => !b.inbox.get()?.items.is_empty(),
        None => false,
    };

    if boundary.is_none() || boundary_has_items {
        let state = match draft_type {
            SubmissionType::Input => SubmissionState::Input(InputSubmissionState::Queued),
            SubmissionType::Write => SubmissionState::Write(WriteSubmissionState::Queued),
        };
        let created = tx
            .create_submission(SubmissionCreate {
                conversation_id,
                request_id,
                state,
            })
            .await?;

        let inbox_draft = match &boundary {
            Some(b) => b.inbox.clone(),
            None => tx.doc_conversation(&*INBOX_DOC, conversation_id).await?,
        };
        let item = match draft {
            SubmissionDraft::Write { entry, .. } => {
                let raw = serde_json::to_value(&entry).map_err(|e| pi_js::Error::js("TypeError", e.to_string()))?;
                let copied = copy_json(&raw, None)?;
                if !is_json_value(&copied) {
                    return Err(pi_js::Error::js("TypeError", "Entry draft must be strict JSON"));
                }
                let cleaned: EntryDraft =
                    serde_json::from_value(copied).map_err(|e| pi_js::Error::js("TypeError", e.to_string()))?;
                InboxItem::Write {
                    id: created.id,
                    entry: cleaned,
                }
            }
            SubmissionDraft::Input { content, when_busy, .. } => {
                let raw = serde_json::to_value(&content).map_err(|e| pi_js::Error::js("TypeError", e.to_string()))?;
                let copied = copy_json(&raw, None)?;
                if !is_json_value(&copied) {
                    return Err(pi_js::Error::js("TypeError", "Submission input must be strict JSON"));
                }
                if when_busy == Some(WhenBusy::Steer) {
                    InboxItem::Steer {
                        id: created.id,
                        content,
                    }
                } else {
                    InboxItem::FollowUp {
                        id: created.id,
                        content,
                    }
                }
            }
        };
        let ov = inbox_draft.overlay()?;
        if let Some(items_ov) = ov.get("items")?.as_overlay() {
            let val = serde_json::to_value(&item).map_err(|e| pi_js::Error::js("TypeError", e.to_string()))?;
            items_ov.push(&[pi_chord::delta::tracker::DraftInput::Json(val)])?;
        }

        if let Some(ref mut b) = boundary {
            let res = apply_boundary(tx, b, BoundaryAt::Final, now).await?;
            if !res.users.is_empty() {
                start_run(tx, conversation_id, &mut live, res.users).await?;
                live_draft.replace(live)?;
            }
        }
        return Ok(created.id);
    }

    let Some(b) = boundary else {
        return Err(pi_js::Error::msg("Invariant violation: missing idle boundary"));
    };
    match draft {
        SubmissionDraft::Write { entry, .. } => {
            if is_stale(&b, &entry) {
                let created = tx
                    .create_submission(SubmissionCreate {
                        conversation_id,
                        request_id,
                        state: SubmissionState::Write(WriteSubmissionState::Unanswered {
                            reason: "stale".to_owned(),
                            detail: None,
                        }),
                    })
                    .await?;
                return Ok(created.id);
            }
            let appended = tx.append_entry(conversation_id, entry).await?;
            let created = tx
                .create_submission(SubmissionCreate {
                    conversation_id,
                    request_id,
                    state: SubmissionState::Write(WriteSubmissionState::Done { entry: appended.id }),
                })
                .await?;
            Ok(created.id)
        }
        SubmissionDraft::Input { content, .. } => {
            let message = Message::User(UserMessage {
                content,
                timestamp: now,
            });
            let appended = tx
                .append_typed_entry(
                    &USER_ENTRY,
                    conversation_id,
                    TypedEntryDraft {
                        model: Some(vec![message]),
                        data: (),
                        head: None,
                        edits: None,
                    },
                )
                .await?;
            let created = tx
                .create_submission(SubmissionCreate {
                    conversation_id,
                    request_id,
                    state: SubmissionState::Input(InputSubmissionState::Placed {
                        entry: appended.record.id,
                    }),
                })
                .await?;
            start_run(tx, conversation_id, &mut live, vec![created.id]).await?;
            live_draft.replace(live)?;
            Ok(created.id)
        }
    }
}
