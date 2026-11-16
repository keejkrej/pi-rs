//! Port of packages/durable/src/harness/inbox.ts

use std::sync::LazyLock;

use pi_ai::types::{Message, UserMessage};
use serde::{Deserialize, Serialize};

use crate::documents::{ConversationDocToken, DocDefinitionInput, define_conversation_doc};
use crate::entries::{TypedEntryDraft, USER_ENTRY};
use crate::harness::types::{QueueMode, Settings, UserInput};
use crate::session::transaction::{DocDraft, Tx};
use crate::types::{
    ConversationId, EntryDraft, EntryDraftHead, EntryId, LatestFork, SubmissionId, SubmissionSettlement,
};

/// A queued submission: user input for a run, or a passive entry write.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "camelCase")]
pub enum InboxItem {
    Steer { id: SubmissionId, content: UserInput },
    FollowUp { id: SubmissionId, content: UserInput },
    Write { id: SubmissionId, entry: EntryDraft },
}

/// Built-in queue of one conversation's submissions waiting for a boundary, in ID order.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InboxState {
    pub items: Vec<InboxItem>,
}

pub static INBOX_DOC: LazyLock<ConversationDocToken<InboxState>> = LazyLock::new(|| {
    define_conversation_doc(
        LatestFork::Initial,
        DocDefinitionInput {
            kind: "pi.inbox",
            version: 1,
            initial: InboxState::default,
            migrate: None,
            checkpoint_when: Some(|value, _, _| value.items.is_empty()),
        },
    )
    .expect("valid built-in pi.inbox doc definition")
});

/// The settings a boundary reads, on the Session line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QueueModes {
    pub steering_mode: QueueMode,
    pub follow_up_mode: QueueMode,
}

impl From<&Settings> for QueueModes {
    fn from(settings: &Settings) -> Self {
        Self {
            steering_mode: settings.steering_mode,
            follow_up_mode: settings.follow_up_mode,
        }
    }
}

/// Boundary placement timing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoundaryAt {
    PostTools,
    Final,
}

/// What a boundary reads before the commit's first table write, and the newest head it has seen so far.
pub struct Boundary {
    pub conversation_id: ConversationId,
    pub inbox: DocDraft<InboxState>,
    pub steering_mode: QueueMode,
    pub follow_up_mode: QueueMode,
    /// Start of the active range, the newest head marker's `head`; advanced by heads written in this commit.
    pub head: Option<EntryId>,
}

/// Selected user items, in ID order, and whether a `head: "self"` write (a reset) was placed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BoundaryResult {
    pub users: Vec<SubmissionId>,
    pub reset: bool,
}

/// Read what a boundary needs. Table reads must precede the commit's first table write, so callers
/// prepare the boundary at the start of their commit.
pub async fn prepare_boundary(tx: &Tx, conversation_id: ConversationId, modes: QueueModes) -> pi_js::Result<Boundary> {
    let head = tx
        .latest_head_marker(conversation_id)
        .await?
        .and_then(|record| record.head);
    let inbox = tx.doc_conversation(&*INBOX_DOC, conversation_id).await?;
    Ok(Boundary {
        conversation_id,
        inbox,
        steering_mode: modes.steering_mode,
        follow_up_mode: modes.follow_up_mode,
        head,
    })
}

/// Place the queued items a boundary selects (spec §6): every write, the first or all steers, and at
/// `Final` the first or all follow-ups. A selected reset turns a `PostTools` boundary into `Final`.
/// Writes are placed first and user items after them, each in ID order, so user items queued before
/// a reset run in the new context. A write whose head targets an entry before the active range,
/// including a range started earlier in this commit, is stale. Selected and stale items are removed
/// positionally.
pub async fn apply_boundary(
    tx: &Tx,
    boundary: &mut Boundary,
    at: BoundaryAt,
    now: i64,
) -> pi_js::Result<BoundaryResult> {
    let conversation_id = boundary.conversation_id;
    let state = boundary.inbox.get()?;
    let items = &state.items;

    let reset = items.iter().any(|item| {
        matches!(
            item,
            InboxItem::Write {
                entry: EntryDraft {
                    head: Some(EntryDraftHead::SelfId(_)),
                    ..
                },
                ..
            }
        )
    });
    let is_final = at == BoundaryAt::Final || reset;

    let pick = |is_match: fn(&InboxItem) -> bool, queue_mode: QueueMode| -> Vec<usize> {
        let mut indexes: Vec<usize> = items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| is_match(item).then_some(index))
            .collect();
        if queue_mode != QueueMode::All {
            indexes.truncate(1);
        }
        indexes
    };

    let writes: Vec<usize> = items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| matches!(item, InboxItem::Write { .. }).then_some(index))
        .collect();
    let mut users = pick(|item| matches!(item, InboxItem::Steer { .. }), boundary.steering_mode);
    if is_final {
        users.extend(pick(
            |item| matches!(item, InboxItem::FollowUp { .. }),
            boundary.follow_up_mode,
        ));
    }
    users.sort_unstable();

    for &index in &writes {
        let InboxItem::Write { id, entry: draft } = &items[index] else {
            continue;
        };
        if is_stale(boundary, draft) {
            tx.settle_submission(
                *id,
                SubmissionSettlement::Unanswered {
                    reason: "stale".to_owned(),
                    detail: None,
                },
            )?;
            continue;
        }
        let appended = tx.append_entry(conversation_id, draft.clone()).await?;
        match draft.head {
            Some(EntryDraftHead::SelfId(_)) => boundary.head = Some(appended.id),
            Some(EntryDraftHead::Entry(target)) => boundary.head = Some(target),
            None => {}
        }
        tx.place_submission(*id, appended.id)?;
    }

    let mut placed = Vec::with_capacity(users.len());
    for &index in &users {
        let (id, content) = match &items[index] {
            InboxItem::Steer { id, content } | InboxItem::FollowUp { id, content } => (*id, content.clone()),
            InboxItem::Write { .. } => continue,
        };
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
        tx.place_submission(id, appended.record.id)?;
        placed.push(id);
    }

    let mut removed: Vec<usize> = writes.into_iter().chain(users).collect();
    removed.sort_unstable_by(|a, b| b.cmp(a));
    removed.dedup();
    boundary.inbox.update(|state| {
        for index in removed {
            if index < state.items.len() {
                state.items.remove(index);
            }
        }
    })?;

    Ok(BoundaryResult { users: placed, reset })
}

/// Whether a head write targets an entry before the active range, so placing it would bring back cut history.
pub fn is_stale(boundary: &Boundary, entry: &EntryDraft) -> bool {
    match (entry.head, boundary.head) {
        (Some(EntryDraftHead::Entry(target)), Some(active_head)) => target < active_head,
        _ => false,
    }
}

/// Remove a withdrawn submission's item; the caller settles the submission.
pub async fn remove_inbox_item(tx: &Tx, conversation_id: ConversationId, id: SubmissionId) -> pi_js::Result<()> {
    let draft = tx.doc_conversation(&*INBOX_DOC, conversation_id).await?;
    draft.update(|state| {
        if let Some(index) = state.items.iter().position(|item| item_id(item) == id) {
            state.items.remove(index);
        }
    })
}

/// Withdraw every queued input of a conversation, as `Conversation.abort()` and abort cascades do:
/// each settles `unanswered` with `aborted` and leaves the inbox; queued writes stay for later placement.
pub async fn withdraw_queued_inputs(tx: &Tx, conversation_id: ConversationId) -> pi_js::Result<()> {
    let draft = tx.doc_conversation(&*INBOX_DOC, conversation_id).await?;
    let current = draft.get()?;
    let mut keep = Vec::with_capacity(current.items.len());
    for item in current.items {
        match item {
            InboxItem::Write { .. } => keep.push(item),
            InboxItem::Steer { id, .. } | InboxItem::FollowUp { id, .. } => {
                tx.settle_submission(
                    id,
                    SubmissionSettlement::Unanswered {
                        reason: "aborted".to_owned(),
                        detail: None,
                    },
                )?;
            }
        }
    }
    draft.update(|state| {
        state.items = keep;
    })
}

fn item_id(item: &InboxItem) -> SubmissionId {
    match item {
        InboxItem::Steer { id, .. } | InboxItem::FollowUp { id, .. } | InboxItem::Write { id, .. } => *id,
    }
}
