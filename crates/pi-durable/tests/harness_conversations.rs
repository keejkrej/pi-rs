//! Port of packages/durable/test/harness-conversations.test.ts

mod support;

use std::marker::PhantomData;
use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use pi_ai::create_models;
use pi_ai::types::{Message, ModelThinkingLevel};
use pi_durable::LiveState;
use pi_durable::documents::{DocDefinitionInput, define_rewindable_conversation_doc};
use pi_durable::entries::define_entry;
use pi_durable::harness::agent::{AGENT_DOC, configure};
use pi_durable::harness::define::define_extension;
use pi_durable::harness::harness::{Conversation, Harness, RootConversationOptions};
use pi_durable::harness::live::LIVE_DOC;
use pi_durable::harness::registry::{Registry, create_registry};
use pi_durable::harness::types::{
    AgentChange, AgentState, AgentToolsChange, AgentToolsFilter, ConversationCreateOptions, Extension, FieldChange,
    HarnessOptions, ModelRef,
};
use pi_durable::session::create_session;
use pi_durable::storage::memory::MemoryStorage;
use pi_durable::storage::sqlite::node::{NodeSqliteStorageOptions, open_node_sqlite_storage};
use pi_durable::tasks::{TaskDefinition, define_task};
use pi_durable::types::{
    ConversationId, ConversationOwnership, EntryDraft, EntryId, EntryRecord, ROOT_CONVERSATION_ID, RewindableFork,
    StorageWrite, TaskOptions, TaskOwnership,
};
use pretty_assertions::assert_eq;
use serde::{Deserialize, Serialize};
use support::harness_support::{OpenHarnessOptions, add_tool, open_harness, tool, user};
use support::session_support::{ControlledStorage, context};
use tempfile::TempDir;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct NoteState {
    text: String,
}

fn note_doc() -> pi_durable::documents::RewindableConversationDocToken<NoteState> {
    define_rewindable_conversation_doc(
        RewindableFork::AsOf,
        DocDefinitionInput {
            kind: "test.note",
            version: 1,
            initial: NoteState::default,
            migrate: None,
            checkpoint_when: None,
        },
    )
    .expect("note doc")
}

async fn append_text(conversation: &Conversation, text: &str) -> EntryRecord {
    let id = conversation.id();
    let msg: Message = user(text).into();
    conversation
        .commit(
            move |tx| async move {
                tx.append_entry(
                    id,
                    EntryDraft {
                        kind: "message".to_owned(),
                        model: Some(vec![msg]),
                        data: None,
                        head: None,
                        edits: None,
                    },
                )
                .await
            },
            context(),
        )
        .await
        .expect("append")
}

async fn all_entries_text(conversation: &Conversation) -> Vec<String> {
    let mut texts = Vec::new();
    let mut cursor = None;
    loop {
        let page = conversation
            .entries(Default::default(), 2, cursor, context())
            .await
            .expect("entries page");
        for entry in &page.items {
            if let Some(model) = &entry.model {
                if let Message::User(u) = &model[0] {
                    if let pi_ai::types::UserMessageContent::Text(s) = &u.content {
                        texts.push(s.clone());
                    }
                }
            }
        }
        cursor = page.next;
        if cursor.is_none() {
            break;
        }
    }
    texts
}

fn write_kind(write: &StorageWrite) -> &'static str {
    match write {
        StorageWrite::Conversation { .. } => "conversation",
        StorageWrite::Entry { .. } => "entry",
        StorageWrite::Task { .. } => "task",
        StorageWrite::Submission { .. } => "submission",
        StorageWrite::DocumentCreate { .. } => "document.create",
        StorageWrite::DocumentCopy { .. } => "document.copy",
        StorageWrite::DocumentChange { .. } => "document.change",
        StorageWrite::DocumentRetire { .. } => "document.retire",
    }
}

#[tokio::test]
async fn creates_the_root_lazily_with_its_agent_change_and_init_in_one_commit() {
    let storage = Arc::new(ControlledStorage::new());
    let opened = open_harness(
        Arc::clone(&storage) as _,
        &["read", "bash"],
        OpenHarnessOptions::default(),
    )
    .await
    .expect("open harness");
    assert_eq!(storage.commit_count(), 0);

    let note = note_doc();
    let note_init = note.clone();
    let root = opened
        .harness
        .root(
            context(),
            Some(RootConversationOptions {
                agent: Some(AgentChange {
                    thinking_level: FieldChange::Set(ModelThinkingLevel::High),
                    ..Default::default()
                }),
                init: Some(Arc::new(move |tx, id| {
                    let note_init = note_init.clone();
                    Box::pin(async move {
                        assert_eq!(
                            tx.doc_conversation(&*AGENT_DOC, id).await?.get()?.thinking_level,
                            Some(ModelThinkingLevel::High)
                        );
                        tx.doc_conversation(&note_init, id).await?.update(|n| {
                            n.text = "root note".to_owned();
                        })?;
                        Ok(())
                    })
                })),
            }),
        )
        .await
        .expect("root");
    assert_eq!(root.id(), ROOT_CONVERSATION_ID);
    assert_eq!(storage.commit_count(), 1);
    let kinds: Vec<&str> = storage.commits_snapshot()[0].iter().map(write_kind).collect();
    assert_eq!(
        kinds,
        vec![
            "conversation",
            "document.create",
            "document.create",
            "document.create",
            "document.create",
            "document.create",
        ]
    );
    assert_eq!(
        opened
            .harness
            .snapshot_conversation(&*LIVE_DOC, root.id(), context())
            .await
            .expect("live"),
        Some(LiveState::default())
    );
    assert_eq!(
        opened
            .harness
            .snapshot_conversation(&*AGENT_DOC, root.id(), context())
            .await
            .expect("agent"),
        Some(AgentState {
            thinking_level: Some(ModelThinkingLevel::High),
            ..Default::default()
        })
    );
    let agent = root.agent(context()).await.expect("agent");
    assert_eq!(agent.thinking_level, ModelThinkingLevel::High);
    assert_eq!(
        agent.tools.iter().map(|each| each.name.as_str()).collect::<Vec<_>>(),
        vec!["read", "bash"]
    );
    assert_eq!(
        opened
            .harness
            .snapshot_conversation(&note, root.id(), context())
            .await
            .expect("note"),
        Some(NoteState {
            text: "root note".to_owned()
        })
    );

    let again = opened
        .harness
        .root(
            context(),
            Some(RootConversationOptions {
                agent: Some(AgentChange {
                    thinking_level: FieldChange::Set(ModelThinkingLevel::Low),
                    ..Default::default()
                }),
                init: Some(Arc::new(|_, _| Box::pin(async { panic!("init should not run again") }))),
            }),
        )
        .await
        .expect("root again");
    assert_eq!(again.id(), root.id());
    assert_eq!(storage.commit_count(), 1);
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn keeps_root_and_conversation_identity_and_state_across_reopen() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("session.sqlite").to_string_lossy().into_owned();

    let first_storage = Arc::new(
        open_node_sqlite_storage(&path, NodeSqliteStorageOptions::default())
            .await
            .expect("open sqlite"),
    );
    let first = open_harness(first_storage, &["read"], OpenHarnessOptions::default())
        .await
        .expect("open first");
    let root = first.harness.root(context(), None).await.expect("root");
    root.configure(
        AgentChange {
            model: FieldChange::Set(ModelRef {
                provider: "anthropic".to_owned(),
                model_id: "claude".to_owned(),
            }),
            cwd: FieldChange::Set("/repo".to_owned()),
            ..Default::default()
        },
        context(),
    )
    .await
    .expect("configure");
    let entry = append_text(&root, "hello").await;
    let child = first
        .harness
        .create_conversation(
            ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                agent: None,
                init: None,
            },
            context(),
        )
        .await
        .expect("create child");
    let fork = root
        .fork(
            entry.id,
            ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                agent: None,
                init: None,
            },
            context(),
        )
        .await
        .expect("fork");
    first.harness.close(context()).await.expect("close first");
    assert!(root.agent(context()).await.is_err());

    let second_storage = Arc::new(
        open_node_sqlite_storage(&path, NodeSqliteStorageOptions::default())
            .await
            .expect("reopen sqlite"),
    );
    let second = open_harness(second_storage, &[], OpenHarnessOptions::default())
        .await
        .expect("open second");
    let reopened = second
        .harness
        .root(
            context(),
            Some(RootConversationOptions {
                agent: None,
                init: Some(Arc::new(|_, _| Box::pin(async { panic!("should not init") }))),
            }),
        )
        .await
        .expect("reopened root");
    assert_eq!(reopened.id(), ROOT_CONVERSATION_ID);
    let reopened_agent = reopened.agent(context()).await.expect("reopened agent");
    assert_eq!(
        reopened_agent.model,
        Some(ModelRef {
            provider: "anthropic".to_owned(),
            model_id: "claude".to_owned()
        })
    );
    assert_eq!(reopened_agent.cwd.as_deref(), Some("/repo"));
    assert!(reopened_agent.tools.is_empty());
    assert_eq!(all_entries_text(&reopened).await, vec!["hello"]);
    assert_eq!(
        second
            .harness
            .conversation(child.id(), context())
            .await
            .expect("lookup child")
            .map(|c| c.id()),
        Some(child.id())
    );
    let reopened_fork = second
        .harness
        .conversation(fork.id(), context())
        .await
        .expect("lookup fork")
        .expect("fork exists");
    assert_eq!(all_entries_text(&reopened_fork).await, vec!["hello"]);
    assert_eq!(
        reopened_fork.agent(context()).await.expect("fork agent").model,
        Some(ModelRef {
            provider: "anthropic".to_owned(),
            model_id: "claude".to_owned()
        })
    );
    assert!(
        second
            .harness
            .conversation(ConversationId::new_unchecked(999), context())
            .await
            .expect("missing")
            .is_none()
    );
    second.harness.close(context()).await.expect("close second");
}

#[tokio::test]
async fn creates_independent_conversations_atomically_with_init_and_rolls_back_failures() {
    let storage = Arc::new(ControlledStorage::new());
    let opened = open_harness(Arc::clone(&storage) as _, &["read"], OpenHarnessOptions::default())
        .await
        .expect("open");
    let created = opened
        .harness
        .create_conversation(
            ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                agent: None,
                init: Some(Arc::new(|tx, id| {
                    Box::pin(async move {
                        tx.append_entry(
                            id,
                            EntryDraft {
                                kind: "message".to_owned(),
                                model: Some(vec![user("seed").into()]),
                                data: None,
                                head: None,
                                edits: None,
                            },
                        )
                        .await?;
                        Ok(())
                    })
                })),
            },
            context(),
        )
        .await
        .expect("create");
    assert_eq!(storage.commit_count(), 1);
    assert_eq!(all_entries_text(&created).await, vec!["seed"]);

    let before = storage.commit_count();
    let err = opened
        .harness
        .create_conversation(
            ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                agent: Some(AgentChange {
                    thinking_level: FieldChange::Set(ModelThinkingLevel::High),
                    ..Default::default()
                }),
                init: Some(Arc::new(|_, _| {
                    Box::pin(async { Err(pi_js::Error::msg("init failed")) })
                })),
            },
            context(),
        )
        .await
        .err()
        .expect("should roll back");
    assert!(err.to_string().contains("init failed"), "got: {err}");
    assert_eq!(storage.commit_count(), before);

    // A conversation on the default selection follows installs live.
    add_tool(&opened.registry, tool("bash", None), None).expect("add bash");
    let tool_list: Vec<String> = created
        .agent(context())
        .await
        .expect("agent")
        .tools
        .into_iter()
        .map(|t| t.name)
        .collect();
    assert_eq!(tool_list, vec!["read", "bash"]);
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn forks_at_a_concrete_entry_with_the_as_of_agent_and_applies_agent_and_init_overrides() {
    let opened = open_harness(Arc::new(MemoryStorage::new()), &["read"], OpenHarnessOptions::default())
        .await
        .expect("open");
    let legacy = tool("legacy", None);
    add_tool(&opened.registry, legacy.clone(), None).expect("add legacy");
    let root = opened.harness.root(context(), None).await.expect("root");
    root.configure(
        AgentChange {
            thinking_level: FieldChange::Set(ModelThinkingLevel::Low),
            ..Default::default()
        },
        context(),
    )
    .await
    .expect("low");
    let at = append_text(&root, "one").await;
    root.configure(
        AgentChange {
            thinking_level: FieldChange::Set(ModelThinkingLevel::High),
            tools: FieldChange::Set(AgentToolsChange::replace(&[tool("read", None)])),
            ..Default::default()
        },
        context(),
    )
    .await
    .expect("high");
    append_text(&root, "two").await;

    let child = root
        .fork(
            at.id,
            ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                agent: None,
                init: None,
            },
            context(),
        )
        .await
        .expect("fork child");
    assert_eq!(
        opened
            .harness
            .snapshot_conversation(&*AGENT_DOC, child.id(), context())
            .await
            .expect("child agent"),
        Some(AgentState {
            thinking_level: Some(ModelThinkingLevel::Low),
            ..Default::default()
        })
    );
    assert_eq!(all_entries_text(&child).await, vec!["one"]);

    let overridden = root
        .fork(
            at.id,
            ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                agent: Some(AgentChange {
                    thinking_level: FieldChange::Set(ModelThinkingLevel::Minimal),
                    tools: FieldChange::Set(AgentToolsChange::replace(&[legacy])),
                    ..Default::default()
                }),
                init: Some(Arc::new(|tx, id| {
                    Box::pin(async move {
                        assert_eq!(
                            tx.doc_conversation(&*AGENT_DOC, id).await?.get()?.thinking_level,
                            Some(ModelThinkingLevel::Minimal)
                        );
                        Ok(())
                    })
                })),
            },
            context(),
        )
        .await
        .expect("fork overridden");
    assert_eq!(
        opened
            .harness
            .snapshot_conversation(&*AGENT_DOC, overridden.id(), context())
            .await
            .expect("overridden"),
        Some(AgentState {
            thinking_level: Some(ModelThinkingLevel::Minimal),
            tools: Some(AgentToolsFilter::List(vec!["legacy".to_owned()])),
            ..Default::default()
        })
    );
    assert_eq!(
        opened
            .harness
            .snapshot_conversation(&*AGENT_DOC, root.id(), context())
            .await
            .expect("root agent"),
        Some(AgentState {
            thinking_level: Some(ModelThinkingLevel::High),
            tools: Some(AgentToolsFilter::List(vec!["read".to_owned()])),
            ..Default::default()
        })
    );

    let unrelated = opened
        .harness
        .create_conversation(
            ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                agent: None,
                init: None,
            },
            context(),
        )
        .await
        .expect("unrelated");
    assert!(
        root.fork(
            at.id,
            ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                agent: None,
                init: None,
            },
            context(),
        )
        .await
        .is_ok()
    );
    let err = unrelated
        .fork(
            at.id,
            ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                agent: None,
                init: None,
            },
            context(),
        )
        .await
        .err()
        .expect("should reject non-visible entry");
    assert!(err.to_string().contains("is not visible"), "got: {err}");
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn paginates_fork_aware_history_through_deep_ancestor_caps_and_same_commit_prefixes() {
    let opened = open_harness(Arc::new(MemoryStorage::new()), &[], OpenHarnessOptions::default())
        .await
        .expect("open");
    let root = opened.harness.root(context(), None).await.expect("root");
    append_text(&root, "r1").await;
    let root_id = root.id();
    let r2 = root
        .commit(
            move |tx| async move {
                let first = tx
                    .append_entry(
                        root_id,
                        EntryDraft {
                            kind: "message".to_owned(),
                            model: Some(vec![user("r2").into()]),
                            data: None,
                            head: None,
                            edits: None,
                        },
                    )
                    .await?;
                tx.append_entry(
                    root_id,
                    EntryDraft {
                        kind: "message".to_owned(),
                        model: Some(vec![user("r3").into()]),
                        data: None,
                        head: None,
                        edits: None,
                    },
                )
                .await?;
                Ok(first)
            },
            context(),
        )
        .await
        .expect("r2/r3");
    let child = root
        .fork(
            r2.id,
            ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                agent: None,
                init: None,
            },
            context(),
        )
        .await
        .expect("child fork");
    let c1 = append_text(&child, "c1").await;
    append_text(&child, "c2").await;
    let grandchild = child
        .fork(
            c1.id,
            ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                agent: None,
                init: None,
            },
            context(),
        )
        .await
        .expect("grandchild fork");
    append_text(&grandchild, "g1").await;

    assert_eq!(all_entries_text(&root).await, vec!["r3", "r2", "r1"]);
    assert_eq!(all_entries_text(&child).await, vec!["c2", "c1", "r2", "r1"]);
    assert_eq!(all_entries_text(&grandchild).await, vec!["g1", "c1", "r2", "r1"]);

    let bounded = grandchild
        .entries(
            pi_durable::harness::harness::ConversationEntriesQuery {
                min_entry_id: Some(r2.id),
                max_entry_id: Some(c1.id),
            },
            10,
            None,
            context(),
        )
        .await
        .expect("bounded");
    let bounded_ids: Vec<EntryId> = bounded.items.iter().map(|entry| entry.id).collect();
    assert_eq!(bounded_ids, vec![c1.id, r2.id]);
    let message_entry = define_entry::<()>("message").expect("message entry");
    assert!(message_entry.matches(bounded.items.first()));
    assert!(!message_entry.matches(None));
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn binds_commits_and_task_creation_to_the_conversation() {
    let opened = open_harness(Arc::new(MemoryStorage::new()), &[], OpenHarnessOptions::default())
        .await
        .expect("open");
    let conversation = opened
        .harness
        .create_conversation(
            ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                agent: None,
                init: None,
            },
            context(),
        )
        .await
        .expect("conv");
    let mut phases = IndexMap::new();
    phases.insert(
        "run".to_owned(),
        Arc::new(|_, _, _| Box::pin(async { Ok(()) }) as pi_js::BoxFuture<pi_js::Result<()>>) as _,
    );
    let task = define_task::<i64, String, (), ()>(TaskDefinition {
        name: "test.work".to_owned(),
        version: 1,
        initial: Arc::new(|_| Ok("run".to_owned())),
        phases,
        abort: Arc::new(|_, _, _| Box::pin(async { Ok(()) })),
        migrate: None,
        _hooks: PhantomData,
    });
    let task_ref = task.clone();
    let task_id = conversation
        .commit(
            move |tx| async move {
                tx.create_task(
                    &task_ref,
                    &1,
                    TaskOptions {
                        ownership: TaskOwnership::Conversation,
                        conversation_id: None,
                        background: false,
                    },
                )
                .await
            },
            context(),
        )
        .await
        .expect("create task");
    let record = opened
        .harness
        .commit(move |tx| async move { tx.task(task_id.erased()).await }, context())
        .await
        .expect("read task")
        .expect("task present");
    assert_eq!(record.conversation_id, conversation.id());

    let err = opened
        .harness
        .commit(
            move |tx| async move {
                tx.create_task(
                    &task,
                    &2,
                    TaskOptions {
                        ownership: TaskOwnership::Conversation,
                        conversation_id: None,
                        background: false,
                    },
                )
                .await
            },
            context(),
        )
        .await
        .err()
        .expect("should require conversation_id");
    assert!(err.to_string().contains("conversationId"), "got: {err}");
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn runs_conversation_created_in_every_creating_commit_after_built_ins_and_before_agent_and_init() {
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let note = note_doc();
    let seen_cb = Arc::clone(&seen);
    let note_cb = note.clone();

    let harness = Harness::open(
        Arc::new(MemoryStorage::new()),
        HarnessOptions {
            models: create_models(None),
            registry: create_registry() as Arc<dyn pi_durable::harness::types::RegistryReader>,
            settings: None,
            env: None,
            conversation_created: Some(Arc::new(move |tx, conversation| {
                let seen_cb = Arc::clone(&seen_cb);
                let note_cb = note_cb.clone();
                Box::pin(async move {
                    let agent = tx.doc_conversation(&*AGENT_DOC, conversation.id).await?.get()?;
                    let kind = if conversation.parent.is_none() { "new" } else { "fork" };
                    let cwd = agent.cwd.as_deref().unwrap_or("undefined");
                    seen_cb
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push(format!("{}:{kind}:{cwd}", conversation.id));
                    tx.doc_conversation(&note_cb, conversation.id).await?.update(|n| {
                        if n.text.is_empty() {
                            n.text = "created".to_owned();
                        }
                    })?;
                    if agent.cwd.as_deref() == Some("/fail") {
                        return Err(pi_js::Error::msg("no"));
                    }
                    Ok(())
                })
            })),
            now: None,
            on_report: None,
        },
        context(),
    )
    .await
    .expect("open");

    let seen_init = Arc::clone(&seen);
    let note_init = note.clone();
    let root = harness
        .root(
            context(),
            Some(RootConversationOptions {
                agent: Some(AgentChange {
                    cwd: FieldChange::Set("/root".to_owned()),
                    ..Default::default()
                }),
                init: Some(Arc::new(move |tx, id| {
                    let seen_init = Arc::clone(&seen_init);
                    let note_init = note_init.clone();
                    Box::pin(async move {
                        let text = tx.doc_conversation(&note_init, id).await?.get()?.text;
                        seen_init
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .push(format!("init:{text}"));
                        Ok(())
                    })
                })),
            }),
        )
        .await
        .expect("root");

    let raw = harness
        .commit(
            |tx| async move { Ok(tx.create_conversation(ConversationOwnership::Ownerless).await?.id) },
            context(),
        )
        .await
        .expect("raw");
    let entry = append_text(&root, "hello").await;
    let fork = root
        .fork(
            entry.id,
            ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                agent: None,
                init: None,
            },
            context(),
        )
        .await
        .expect("fork");

    assert_eq!(
        *seen.lock().unwrap_or_else(|e| e.into_inner()),
        vec![
            format!("{}:new:undefined", root.id()),
            "init:created".to_owned(),
            format!("{raw}:new:undefined"),
            format!("{}:fork:/root", fork.id()),
        ]
    );
    assert_eq!(
        harness
            .snapshot_conversation(&note, raw, context())
            .await
            .expect("note"),
        Some(NoteState {
            text: "created".to_owned()
        })
    );

    root.configure(
        AgentChange {
            cwd: FieldChange::Set("/fail".to_owned()),
            ..Default::default()
        },
        context(),
    )
    .await
    .expect("configure fail");
    assert!(
        root.fork(
            entry.id,
            ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                agent: None,
                init: None,
            },
            context(),
        )
        .await
        .is_ok()
    );
    let failing = append_text(&root, "after").await;
    let err = root
        .fork(
            failing.id,
            ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                agent: None,
                init: None,
            },
            context(),
        )
        .await
        .err()
        .expect("should reject");
    assert!(err.to_string().contains("no"), "got: {err}");
    harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn replaces_whole_fields_clears_them_with_null_and_leaves_undefined_fields_alone() {
    let opened = open_harness(Arc::new(MemoryStorage::new()), &[], OpenHarnessOptions::default())
        .await
        .expect("open");
    let read = tool("read", None);
    let bash = tool("bash", None);
    let edit = tool("edit", None);
    opened
        .registry
        .install(define_extension(Extension {
            name: "coding".to_owned(),
            tools: vec![read.clone(), bash.clone(), edit.clone()],
            ..Default::default()
        }))
        .expect("install");
    let root = opened.harness.root(context(), None).await.expect("root");
    let initial_agent = root.agent(context()).await.expect("agent");
    assert_eq!(initial_agent.thinking_level, ModelThinkingLevel::Off);
    assert_eq!(
        initial_agent.tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
        vec!["read", "bash", "edit"]
    );
    assert!(initial_agent.model.is_none());

    root.configure(
        AgentChange {
            model: FieldChange::Set(ModelRef {
                provider: "openai".to_owned(),
                model_id: "gpt".to_owned(),
            }),
            thinking_level: FieldChange::Set(ModelThinkingLevel::Medium),
            ..Default::default()
        },
        context(),
    )
    .await
    .expect("set model & thinking");
    root.configure(
        AgentChange {
            instructions: FieldChange::Set("Be terse.".to_owned()),
            ..Default::default()
        },
        context(),
    )
    .await
    .expect("set instructions");
    assert_eq!(
        opened
            .harness
            .snapshot_conversation(&*AGENT_DOC, root.id(), context())
            .await
            .expect("snapshot"),
        Some(AgentState {
            model: Some(ModelRef {
                provider: "openai".to_owned(),
                model_id: "gpt".to_owned(),
            }),
            thinking_level: Some(ModelThinkingLevel::Medium),
            instructions: Some("Be terse.".to_owned()),
            ..Default::default()
        })
    );

    root.configure(
        AgentChange {
            model: FieldChange::Clear,
            instructions: FieldChange::Clear,
            ..Default::default()
        },
        context(),
    )
    .await
    .expect("clear");
    assert_eq!(
        opened
            .harness
            .snapshot_conversation(&*AGENT_DOC, root.id(), context())
            .await
            .expect("snapshot"),
        Some(AgentState {
            thinking_level: Some(ModelThinkingLevel::Medium),
            ..Default::default()
        })
    );

    root.configure(
        AgentChange {
            tools: FieldChange::Set(AgentToolsChange::remove(&[edit.clone()])),
            ..Default::default()
        },
        context(),
    )
    .await
    .expect("remove edit");
    assert_eq!(
        root.agent(context())
            .await
            .expect("agent")
            .tools
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>(),
        vec!["read", "bash"]
    );
    root.configure(
        AgentChange {
            tools: FieldChange::Set(AgentToolsChange::remove(&[bash])),
            ..Default::default()
        },
        context(),
    )
    .await
    .expect("remove bash");
    assert_eq!(
        root.agent(context())
            .await
            .expect("agent")
            .tools
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>(),
        vec!["read", "edit"]
    );
    root.configure(
        AgentChange {
            tools: FieldChange::Set(AgentToolsChange::replace(&[edit, read.clone()])),
            ..Default::default()
        },
        context(),
    )
    .await
    .expect("replace");
    assert_eq!(
        root.agent(context())
            .await
            .expect("agent")
            .tools
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>(),
        vec!["edit", "read"]
    );
    root.configure(
        AgentChange {
            tools: FieldChange::Clear,
            ..Default::default()
        },
        context(),
    )
    .await
    .expect("clear tools");
    assert_eq!(
        root.agent(context())
            .await
            .expect("agent")
            .tools
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>(),
        vec!["read", "bash", "edit"]
    );
    root.configure(
        AgentChange {
            tools: FieldChange::Set(AgentToolsChange::replace(&[tool("missing", None), read])),
            ..Default::default()
        },
        context(),
    )
    .await
    .expect("set missing+read");
    assert_eq!(
        opened
            .harness
            .snapshot_conversation(&*AGENT_DOC, root.id(), context())
            .await
            .expect("snapshot")
            .and_then(|s| s.tools),
        Some(AgentToolsFilter::List(vec!["missing".to_owned(), "read".to_owned()]))
    );
    assert_eq!(
        root.agent(context())
            .await
            .expect("agent")
            .tools
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>(),
        vec!["read"]
    );
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn gives_conversations_created_through_tx_their_documents_empty_owner_copy_or_forks_as_of_copy() {
    let opened = open_harness(Arc::new(MemoryStorage::new()), &["read"], OpenHarnessOptions::default())
        .await
        .expect("open");
    let root = opened
        .harness
        .root(
            context(),
            Some(RootConversationOptions {
                agent: Some(AgentChange {
                    model: FieldChange::Set(ModelRef {
                        provider: "faux".to_owned(),
                        model_id: "m".to_owned(),
                    }),
                    instructions: FieldChange::Set("Main role.".to_owned()),
                    cwd: FieldChange::Set("/repo".to_owned()),
                    ..Default::default()
                }),
                init: None,
            }),
        )
        .await
        .expect("root");
    let mut phases = IndexMap::new();
    phases.insert(
        "never".to_owned(),
        Arc::new(|_, _, _| Box::pin(async { Ok(()) }) as pi_js::BoxFuture<pi_js::Result<()>>) as _,
    );
    let owner = define_task::<serde_json::Value, String, (), ()>(TaskDefinition {
        name: "test.owner".to_owned(),
        version: 1,
        initial: Arc::new(|_| Ok("never".to_owned())),
        phases,
        abort: Arc::new(|_, _, _| Box::pin(async { Ok(()) })),
        migrate: None,
        _hooks: PhantomData,
    });
    let (task_id, plain_id, owned_id) = root
        .commit(
            move |tx| async move {
                let task_id = tx
                    .create_task(
                        &owner,
                        &serde_json::json!({}),
                        TaskOptions {
                            ownership: TaskOwnership::Conversation,
                            conversation_id: None,
                            background: false,
                        },
                    )
                    .await?;
                let plain = tx.create_conversation(ConversationOwnership::Ownerless).await?;
                let owned = tx
                    .create_conversation(ConversationOwnership::Task {
                        task_id: task_id.erased(),
                    })
                    .await?;
                assert_eq!(
                    tx.doc_conversation(&*AGENT_DOC, owned.id).await?.get()?.cwd.as_deref(),
                    Some("/repo")
                );
                configure(
                    &tx,
                    owned.id,
                    &AgentChange {
                        cwd: FieldChange::Set("/worktree".to_owned()),
                        ..Default::default()
                    },
                )
                .await?;
                Ok((task_id, plain.id, owned.id))
            },
            context(),
        )
        .await
        .expect("commit ids");

    assert_eq!(
        opened
            .harness
            .snapshot_conversation(&*AGENT_DOC, plain_id, context())
            .await
            .expect("plain agent"),
        Some(AgentState::default())
    );
    assert_eq!(
        opened
            .harness
            .snapshot_conversation(&*LIVE_DOC, plain_id, context())
            .await
            .expect("plain live"),
        Some(LiveState::default())
    );
    assert_eq!(
        opened
            .harness
            .snapshot_conversation(&*AGENT_DOC, owned_id, context())
            .await
            .expect("owned agent"),
        Some(AgentState {
            model: Some(ModelRef {
                provider: "faux".to_owned(),
                model_id: "m".to_owned(),
            }),
            instructions: Some("Main role.".to_owned()),
            cwd: Some("/worktree".to_owned()),
            ..Default::default()
        })
    );

    // A later owner change does not reach the child.
    root.configure(
        AgentChange {
            thinking_level: FieldChange::Set(ModelThinkingLevel::High),
            ..Default::default()
        },
        context(),
    )
    .await
    .expect("root change");
    assert_eq!(
        opened
            .harness
            .snapshot_conversation(&*AGENT_DOC, owned_id, context())
            .await
            .expect("owned")
            .and_then(|s| s.thinking_level),
        None
    );

    // A task-owned fork keeps its as-of copy of its fork parent, not its owner's agent.
    let at = root
        .commit(
            move |tx| async move {
                Ok(tx
                    .append_entry(
                        plain_id,
                        EntryDraft {
                            kind: "note".to_owned(),
                            model: None,
                            data: None,
                            head: None,
                            edits: None,
                        },
                    )
                    .await?
                    .id)
            },
            context(),
        )
        .await
        .expect("at");
    let fork_id = root
        .commit(
            move |tx| async move {
                Ok(tx
                    .fork_conversation(
                        plain_id,
                        at,
                        ConversationOwnership::Task {
                            task_id: task_id.erased(),
                        },
                    )
                    .await?
                    .id)
            },
            context(),
        )
        .await
        .expect("fork");
    assert_eq!(
        opened
            .harness
            .snapshot_conversation(&*AGENT_DOC, fork_id, context())
            .await
            .expect("fork agent"),
        Some(AgentState::default())
    );
    assert_eq!(
        opened
            .harness
            .snapshot_conversation(&*LIVE_DOC, fork_id, context())
            .await
            .expect("fork live"),
        Some(LiveState::default())
    );
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn reads_an_absent_agent_for_conversations_a_plain_session_created_without_writing() {
    let storage = Arc::new(ControlledStorage::new());
    let id = create_session(Arc::clone(&storage) as _)
        .commit(
            |tx| async move { Ok(tx.create_conversation(ConversationOwnership::Ownerless).await?.id) },
            context(),
        )
        .await
        .expect("plain conv");
    let opened = open_harness(Arc::clone(&storage) as _, &["read"], OpenHarnessOptions::default())
        .await
        .expect("open");
    let raw = opened
        .harness
        .conversation(id, context())
        .await
        .expect("lookup")
        .expect("exists");
    assert_eq!(
        opened
            .harness
            .snapshot_conversation(&*LIVE_DOC, id, context())
            .await
            .expect("live"),
        None
    );
    let commits = storage.commit_count();
    let agent = raw.agent(context()).await.expect("agent");
    assert_eq!(agent.thinking_level, ModelThinkingLevel::Off);
    assert_eq!(
        agent.tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
        vec!["read"]
    );
    assert_eq!(storage.commit_count(), commits);

    raw.configure(
        AgentChange {
            thinking_level: FieldChange::Set(ModelThinkingLevel::Low),
            ..Default::default()
        },
        context(),
    )
    .await
    .expect("configure");
    assert_eq!(
        opened
            .harness
            .snapshot_conversation(&*AGENT_DOC, id, context())
            .await
            .expect("agent doc"),
        Some(AgentState {
            thinking_level: Some(ModelThinkingLevel::Low),
            ..Default::default()
        })
    );
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn returns_stateless_handles_and_rejects_operations_after_close() {
    let opened = open_harness(Arc::new(MemoryStorage::new()), &[], OpenHarnessOptions::default())
        .await
        .expect("open");
    let root = opened.harness.root(context(), None).await.expect("root");
    let again = opened.harness.root(context(), None).await.expect("root again");
    assert_eq!(again.id(), root.id());
    opened.harness.close(context()).await.expect("close");

    let root_err = opened.harness.root(context(), None).await.err().expect("closed root");
    assert!(root_err.to_string().contains("Harness is closed"), "got: {root_err}");
    let create_err = opened
        .harness
        .create_conversation(
            ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                agent: None,
                init: None,
            },
            context(),
        )
        .await
        .err()
        .expect("closed create");
    assert!(
        create_err.to_string().contains("Harness is closed"),
        "got: {create_err}"
    );
    let conv_err = opened
        .harness
        .conversation(root.id(), context())
        .await
        .err()
        .expect("closed conversation");
    assert!(conv_err.to_string().contains("Harness is closed"), "got: {conv_err}");
}

#[tokio::test]
async fn forwards_generic_session_document_apis() {
    let opened = open_harness(Arc::new(MemoryStorage::new()), &[], OpenHarnessOptions::default())
        .await
        .expect("open");
    let root = opened.harness.root(context(), None).await.expect("root");
    let note = note_doc();
    let note_first = note.clone();
    let root_id = root.id();
    let entry = root
        .commit(
            move |tx| async move {
                tx.doc_conversation(&note_first, root_id).await?.update(|n| {
                    n.text = "first".to_owned();
                })?;
                tx.append_entry(
                    root_id,
                    EntryDraft {
                        kind: "message".to_owned(),
                        model: Some(vec![user("m").into()]),
                        data: None,
                        head: None,
                        edits: None,
                    },
                )
                .await
            },
            context(),
        )
        .await
        .expect("first commit");

    let note_second = note.clone();
    opened
        .harness
        .commit(
            move |tx| async move {
                tx.doc_conversation(&note_second, root_id).await?.update(|n| {
                    n.text = "second".to_owned();
                })
            },
            context(),
        )
        .await
        .expect("second commit");

    assert_eq!(
        opened
            .harness
            .snapshot_conversation(&note, root_id, context())
            .await
            .expect("snapshot"),
        Some(NoteState {
            text: "second".to_owned()
        })
    );
    assert_eq!(
        opened
            .harness
            .snapshot_as_of(&note, root_id, entry.id, context())
            .await
            .expect("snapshot_as_of"),
        Some(NoteState {
            text: "first".to_owned()
        })
    );
    let state = opened
        .harness
        .document_state_conversation(&note, root_id, context())
        .await
        .expect("state")
        .expect("present");
    assert_eq!(
        state.value(),
        Some(NoteState {
            text: "second".to_owned()
        })
    );
    state.dispose().expect("dispose");
    opened.harness.close(context()).await.expect("close");
}
