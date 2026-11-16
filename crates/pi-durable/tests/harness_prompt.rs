//! Port of packages/durable/test/harness-prompt.test.ts

mod support;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::IndexMap;
use pi_ai::types::{Message, SystemMessage, SystemMessageContent, Tool};
use pi_ai::{Type, get_current_tools, to_tool_declaration};
use pi_chord::Context;
use pi_chord::context::with_abort_signal;
use pi_durable::documents::ErasedDocDefinition;
use pi_durable::entries::SYSTEM_ENTRY;
use pi_durable::harness::agent::{resolve_agent, resolve_settings};
use pi_durable::harness::define::{SectionOptions, define_extension, wrap_section};
use pi_durable::harness::harness::Conversation;
use pi_durable::harness::prompt::{plan_system_entries, render_sections, replay_sections};
use pi_durable::harness::registry::{Registry, create_registry};
use pi_durable::harness::types::{Agent, DocumentReader, DocumentReaderBackend, Extension, PromptInput, PromptSection};
use pi_durable::storage::memory::MemoryStorage;
use pi_durable::types::{
    ContextEdit, ConversationId, EntryDraft, EntryDraftHead, EntryId, JsonObject, SelfHead, TaskId, TypedEntryDraft,
};
use pi_js::abort::{AbortController, AbortReason};
use pretty_assertions::assert_eq;
use support::harness_support::{OpenHarnessOptions, add_section, open_harness, user};
use support::session_support::context;

#[derive(Debug, PartialEq)]
struct Planned {
    sections: Option<IndexMap<String, Option<String>>>,
    added: Option<Vec<String>>,
    removed: Option<Vec<String>>,
    omit: Option<Vec<EntryId>>,
}

struct NoopReaderBackend;

#[async_trait]
impl DocumentReaderBackend for NoopReaderBackend {
    async fn snapshot_erased(
        &self,
        _def: &Arc<ErasedDocDefinition>,
        _conversation_id: Option<ConversationId>,
        _task_id: Option<TaskId>,
        _key: Option<&str>,
        _context: Context,
    ) -> pi_js::Result<Option<JsonObject>> {
        Ok(None)
    }

    async fn snapshot_as_of_erased(
        &self,
        _def: &Arc<ErasedDocDefinition>,
        _conversation_id: ConversationId,
        _key: Option<&str>,
        _at: EntryId,
        _context: Context,
    ) -> pi_js::Result<Option<JsonObject>> {
        Ok(None)
    }
}

fn dummy_input() -> PromptInput {
    PromptInput {
        conversation_id: ConversationId::new_unchecked(1),
        agent: Agent {
            model: None,
            thinking_level: pi_ai::types::ModelThinkingLevel::Off,
            extensions: Vec::new(),
            tools: Vec::new(),
            sections: Vec::new(),
            instructions: None,
            cwd: None,
        },
        env: None,
        shown: IndexMap::new(),
        read: DocumentReader::from_backend(Arc::new(NoopReaderBackend)),
    }
}

fn map_of(pairs: &[(&str, &str)]) -> IndexMap<String, String> {
    pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
}

fn nullable_map(pairs: &[(&str, Option<&str>)]) -> IndexMap<String, Option<String>> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.map(ToOwned::to_owned)))
        .collect()
}

async fn apply(conversation: &Conversation, desired: &[(&str, &str)]) -> Vec<Planned> {
    let desired_map = map_of(desired);
    let view = conversation.context(context()).await.expect("context");
    let drafts = plan_system_entries(&view, &desired_map, &[], 7);
    let conv_id = conversation.id();
    let drafts_for_commit = drafts.clone();
    conversation
        .commit(
            move |tx| async move {
                for draft in drafts_for_commit {
                    tx.append_typed_entry(&SYSTEM_ENTRY, conv_id, draft).await?;
                }
                Ok(())
            },
            context(),
        )
        .await
        .expect("commit drafts");

    let after_view = conversation.context(context()).await.expect("after context");
    let replayed = replay_sections(&after_view.messages);
    let replayed_vec: Vec<(String, String)> = replayed.into_iter().collect();
    let desired_vec: Vec<(String, String)> = desired_map.into_iter().collect();
    assert_eq!(replayed_vec, desired_vec);

    drafts
        .into_iter()
        .map(|draft| {
            let model = draft.model.expect("system draft model");
            let msg = match &model[0] {
                Message::System(sys) => sys,
                other => panic!("expected system message, got {other:?}"),
            };
            assert_eq!(msg.content, SystemMessageContent::Text(String::new()));
            assert_eq!(msg.timestamp, 7);
            let omit = draft.edits.map(|edits| {
                edits
                    .into_iter()
                    .map(|edit| match edit {
                        ContextEdit::Omit { target } => target,
                        other => panic!("expected omit edit, got {other:?}"),
                    })
                    .collect()
            });
            Planned {
                sections: msg.sections.clone(),
                added: None,
                removed: None,
                omit,
            }
        })
        .collect()
}

async fn apply_tools(conversation: &Conversation, tools: &[Tool], sections: &[(&str, &str)]) -> Vec<Planned> {
    let desired_map = map_of(sections);
    let view = conversation.context(context()).await.expect("context");
    let drafts = plan_system_entries(&view, &desired_map, tools, 7);
    let conv_id = conversation.id();
    let drafts_for_commit = drafts.clone();
    conversation
        .commit(
            move |tx| async move {
                for draft in drafts_for_commit {
                    tx.append_typed_entry(&SYSTEM_ENTRY, conv_id, draft).await?;
                }
                Ok(())
            },
            context(),
        )
        .await
        .expect("commit tool drafts");

    let after_view = conversation.context(context()).await.expect("after context");
    let offered = get_current_tools(&after_view.messages);
    let expected: Vec<Tool> = tools.iter().map(to_tool_declaration).collect();
    assert_eq!(offered, expected);

    drafts
        .into_iter()
        .map(|draft| {
            let model = draft.model.expect("system model");
            let msg = match &model[0] {
                Message::System(sys) => sys,
                other => panic!("expected system message, got {other:?}"),
            };
            Planned {
                sections: msg.sections.clone(),
                added: msg
                    .tools_added
                    .as_ref()
                    .map(|list| list.iter().map(|t| t.name.clone()).collect()),
                removed: msg
                    .tools_removed
                    .as_ref()
                    .map(|list| list.iter().map(|t| t.name.clone()).collect()),
                omit: None,
            }
        })
        .collect()
}

async fn root() -> Conversation {
    let opened = open_harness(Arc::new(MemoryStorage::new()), &[], OpenHarnessOptions::default())
        .await
        .expect("open harness");
    opened.harness.root(context(), None).await.expect("root")
}

async fn last_system_id(conversation: &Conversation) -> EntryId {
    let page = conversation
        .entries(Default::default(), 100, None, context())
        .await
        .expect("entries");
    page.items
        .into_iter()
        .find(|entry| entry.kind == "pi.system")
        .expect("system entry")
        .id
}

async fn marker(conversation: &Conversation, head: EntryDraftHead) -> EntryId {
    let conv_id = conversation.id();
    conversation
        .commit(
            move |tx| async move {
                Ok(tx
                    .append_entry(
                        conv_id,
                        EntryDraft {
                            kind: "summary".to_owned(),
                            model: Some(vec![user("summary").into()]),
                            data: None,
                            head: Some(head),
                            edits: None,
                        },
                    )
                    .await?
                    .id)
            },
            context(),
        )
        .await
        .expect("marker")
}

#[tokio::test]
async fn renders_sections_in_order_with_tags_omissions_wrappers_and_failures() {
    let registry = create_registry();
    add_section(
        &registry,
        "preamble",
        Arc::new(|_, _| Box::pin(async { Ok(Some("You are helpful.".to_owned())) })),
        Some(SectionOptions { tag: Some(false) }),
        None,
    )
    .expect("preamble");
    add_section(
        &registry,
        "cwd",
        Arc::new(|_, _| Box::pin(async { Ok(Some("/repo".to_owned())) })),
        None,
        None,
    )
    .expect("cwd");
    add_section(
        &registry,
        "skipped",
        Arc::new(|_, _| Box::pin(async { Ok(None) })),
        None,
        None,
    )
    .expect("skipped");
    add_section(
        &registry,
        "failing",
        Arc::new(|_, _| Box::pin(async { Err(pi_js::Error::msg("render failed")) })),
        None,
        None,
    )
    .expect("failing");
    add_section(
        &registry,
        "new-failing",
        Arc::new(|_, _| Box::pin(async { Err(pi_js::Error::msg("also failed")) })),
        None,
        None,
    )
    .expect("new-failing");
    registry
        .install(define_extension(Extension {
            name: "git".to_owned(),
            wraps: vec![wrap_section("cwd", |inner| {
                let inner_render = inner.render;
                PromptSection {
                    key: inner.key,
                    tag: inner.tag,
                    render: Arc::new(move |value, ctx| {
                        let inner_render = Arc::clone(&inner_render);
                        Box::pin(async move {
                            let text = inner_render(value, ctx).await?;
                            Ok(Some(format!("{} (git)", text.unwrap_or_default())))
                        })
                    }),
                }
            })],
            ..Default::default()
        }))
        .expect("install git");

    let reports: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let mut shown = IndexMap::new();
    shown.insert("failing".to_owned(), "<failing>\nold\n</failing>".to_owned());
    shown.insert("cwd".to_owned(), "stale".to_owned());

    let snapshot = registry.snapshot();
    let settings = resolve_settings(None);
    let agent = resolve_agent(None, snapshot.as_ref(), &settings, &|err| {
        panic!("unexpected resolve_agent error: {err}")
    });
    let reports_cb = Arc::clone(&reports);
    let desired = render_sections(
        &agent.sections,
        &dummy_input(),
        &shown,
        &move |err| {
            reports_cb
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(err.to_string());
        },
        context(),
    )
    .await
    .expect("render_sections");

    let desired_entries: Vec<(String, String)> = desired.into_iter().collect();
    assert_eq!(
        desired_entries,
        vec![
            ("preamble".to_owned(), "You are helpful.".to_owned()),
            ("cwd".to_owned(), "<cwd>\n/repo (git)\n</cwd>".to_owned()),
            ("failing".to_owned(), "<failing>\nold\n</failing>".to_owned()),
        ]
    );
    assert_eq!(
        *reports.lock().unwrap_or_else(|e| e.into_inner()),
        vec!["render failed".to_owned(), "also failed".to_owned()]
    );
}

#[tokio::test]
async fn propagates_section_errors_after_cancellation() {
    let controller = AbortController::new();
    controller.abort(Some(AbortReason::error("Error", "cancelled")));
    let cancelled = with_abort_signal(&context(), controller.signal());
    let failing = PromptSection {
        key: "a".to_owned(),
        tag: true,
        render: Arc::new(|_, _| Box::pin(async { Err(pi_js::Error::msg("cancelled")) })),
    };
    let err = render_sections(&[failing], &dummy_input(), &IndexMap::new(), &|_| {}, cancelled)
        .await
        .expect_err("should propagate error");
    assert!(err.to_string().contains("cancelled"), "got: {err}");
}

#[test]
fn replays_sections_in_place_deletes_on_null_and_appends_re_additions() {
    let make_sys = |pairs: &[(&str, Option<&str>)]| -> Message {
        Message::System(SystemMessage {
            content: SystemMessageContent::Text(String::new()),
            sections: Some(nullable_map(pairs)),
            tools_added: None,
            tools_removed: None,
            timestamp: 1,
        })
    };
    let shown = replay_sections(&[
        make_sys(&[("a", Some("1")), ("b", Some("2")), ("c", Some("3"))]),
        user("x").into(),
        make_sys(&[("b", Some("20")), ("a", None)]),
        make_sys(&[("a", Some("10"))]),
    ]);
    let actual: Vec<(String, String)> = shown.into_iter().collect();
    assert_eq!(
        actual,
        vec![
            ("b".to_owned(), "20".to_owned()),
            ("c".to_owned(), "3".to_owned()),
            ("a".to_owned(), "10".to_owned()),
        ]
    );
}

#[tokio::test]
async fn emits_minimal_value_patches_removals_and_additions() {
    let conversation = root().await;
    assert_eq!(
        apply(&conversation, &[("a", "1"), ("b", "2"), ("c", "3")]).await,
        vec![Planned {
            sections: Some(nullable_map(&[("a", Some("1")), ("b", Some("2")), ("c", Some("3"))])),
            added: None,
            removed: None,
            omit: None,
        }]
    );
    assert_eq!(
        apply(&conversation, &[("a", "1"), ("b", "20"), ("c", "3"), ("d", "4")]).await,
        vec![Planned {
            sections: Some(nullable_map(&[("b", Some("20")), ("d", Some("4"))])),
            added: None,
            removed: None,
            omit: None,
        }]
    );
    assert_eq!(
        apply(&conversation, &[("a", "1"), ("c", "3"), ("d", "4")]).await,
        vec![Planned {
            sections: Some(nullable_map(&[("b", None)])),
            added: None,
            removed: None,
            omit: None,
        }]
    );
    assert!(
        apply(&conversation, &[("a", "1"), ("c", "3"), ("d", "4")])
            .await
            .is_empty()
    );
    assert_eq!(
        apply(&conversation, &[]).await,
        vec![Planned {
            sections: Some(nullable_map(&[("a", None), ("c", None), ("d", None)])),
            added: None,
            removed: None,
            omit: None,
        }]
    );
}

#[tokio::test]
async fn rewrites_order_only_changes_and_re_additions_as_two_entries() {
    let conversation = root().await;
    apply(&conversation, &[("a", "1"), ("b", "2")]).await;
    assert_eq!(
        apply(&conversation, &[("b", "2"), ("a", "1")]).await,
        vec![
            Planned {
                sections: Some(nullable_map(&[("a", None), ("b", None)])),
                added: None,
                removed: None,
                omit: None,
            },
            Planned {
                sections: Some(nullable_map(&[("b", Some("2")), ("a", Some("1"))])),
                added: None,
                removed: None,
                omit: None,
            },
        ]
    );

    let readded = root().await;
    apply(&readded, &[("a", "1"), ("b", "2"), ("c", "3")]).await;
    assert_eq!(
        apply(&readded, &[("a", "1"), ("c", "3")]).await,
        vec![Planned {
            sections: Some(nullable_map(&[("b", None)])),
            added: None,
            removed: None,
            omit: None,
        }]
    );
    // Patching would append `b` after `c`.
    assert_eq!(
        apply(&readded, &[("a", "1"), ("b", "2"), ("c", "3")]).await,
        vec![
            Planned {
                sections: Some(nullable_map(&[("a", None), ("c", None)])),
                added: None,
                removed: None,
                omit: None,
            },
            Planned {
                sections: Some(nullable_map(&[("a", Some("1")), ("b", Some("2")), ("c", Some("3"))])),
                added: None,
                removed: None,
                omit: None,
            },
        ]
    );
}

#[tokio::test]
async fn rebaselines_after_a_head_marker_omitting_retained_deltas_on_both_sides_of_it() {
    let conversation = root().await;
    apply(&conversation, &[("a", "1"), ("b", "2")]).await;
    let conv_id = conversation.id();
    conversation
        .commit(
            move |tx| async move {
                tx.append_entry(
                    conv_id,
                    EntryDraft {
                        kind: "pi.user".to_owned(),
                        model: Some(vec![user("hi").into()]),
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
        .expect("append user");
    apply(&conversation, &[("a", "1"), ("b", "20")]).await;
    let delta = last_system_id(&conversation).await;

    // The head keeps the delta but cuts its baseline: replay alone would show only `b`.
    marker(&conversation, EntryDraftHead::Entry(delta)).await;
    assert_eq!(
        apply(&conversation, &[("a", "1"), ("b", "20")]).await,
        vec![Planned {
            sections: Some(nullable_map(&[("a", Some("1")), ("b", Some("20"))])),
            added: None,
            removed: None,
            omit: Some(vec![delta]),
        }]
    );
    let baseline = last_system_id(&conversation).await;

    // A system entry follows the marker now, so later changes are ordinary patches.
    assert_eq!(
        apply(&conversation, &[("a", "1"), ("b", "21")]).await,
        vec![Planned {
            sections: Some(nullable_map(&[("b", Some("21"))])),
            added: None,
            removed: None,
            omit: None,
        }]
    );
    let after = last_system_id(&conversation).await;

    // A second marker keeps deltas from both sides of the first one.
    marker(&conversation, EntryDraftHead::Entry(delta)).await;
    assert_eq!(
        apply(&conversation, &[("a", "1"), ("b", "21")]).await,
        vec![Planned {
            sections: Some(nullable_map(&[("a", Some("1")), ("b", Some("21"))])),
            added: None,
            removed: None,
            omit: Some(vec![delta, baseline, after]),
        }]
    );
}

#[tokio::test]
async fn writes_a_complete_post_head_baseline_even_when_replay_already_matches() {
    let conversation = root().await;
    apply(&conversation, &[("a", "1")]).await;
    let baseline = last_system_id(&conversation).await;
    marker(&conversation, EntryDraftHead::Entry(baseline)).await;
    assert_eq!(
        apply(&conversation, &[("a", "1")]).await,
        vec![Planned {
            sections: Some(nullable_map(&[("a", Some("1"))])),
            added: None,
            removed: None,
            omit: Some(vec![baseline]),
        }]
    );
    marker(&conversation, EntryDraftHead::SelfId(SelfHead)).await;
    assert_eq!(
        apply(&conversation, &[]).await,
        vec![Planned {
            sections: Some(IndexMap::new()),
            added: None,
            removed: None,
            omit: None,
        }]
    );
    assert!(apply(&conversation, &[]).await.is_empty());
}

fn declaration(name: &str, description: &str) -> Tool {
    Tool {
        name: name.to_owned(),
        description: description.to_owned(),
        parameters: Type::object([]),
        constrained_sampling: None,
    }
}

#[tokio::test]
async fn adds_removes_replaces_changed_declarations_and_rewrites_the_order_when_needed() {
    let conversation = root().await;
    let a = declaration("a", "a");
    let b = declaration("b", "b");
    let c = declaration("c", "c");

    assert_eq!(
        apply_tools(&conversation, &[a.clone(), b.clone()], &[]).await,
        vec![Planned {
            sections: None,
            added: Some(vec!["a".to_owned(), "b".to_owned()]),
            removed: None,
            omit: None,
        }]
    );
    assert!(
        apply_tools(&conversation, &[a.clone(), b.clone()], &[])
            .await
            .is_empty()
    );
    assert_eq!(
        apply_tools(&conversation, &[a.clone(), b.clone(), c.clone()], &[]).await,
        vec![Planned {
            sections: None,
            added: Some(vec!["c".to_owned()]),
            removed: None,
            omit: None,
        }]
    );
    assert_eq!(
        apply_tools(&conversation, &[a.clone(), c.clone()], &[]).await,
        vec![Planned {
            sections: None,
            added: None,
            removed: Some(vec!["b".to_owned()]),
            omit: None,
        }]
    );
    // A changed declaration at the end is removed and re-added in place.
    let c2 = declaration("c", "changed");
    assert_eq!(
        apply_tools(&conversation, &[a.clone(), c2.clone()], &[]).await,
        vec![Planned {
            sections: None,
            added: Some(vec!["c".to_owned()]),
            removed: Some(vec!["c".to_owned()]),
            omit: None,
        }]
    );
    // A changed declaration in the middle would move to the end, so the whole order is rewritten.
    let a2 = declaration("a", "changed");
    assert_eq!(
        apply_tools(&conversation, &[a2.clone(), c2.clone()], &[]).await,
        vec![Planned {
            sections: None,
            added: Some(vec!["a".to_owned(), "c".to_owned()]),
            removed: Some(vec!["a".to_owned(), "c".to_owned()]),
            omit: None,
        }]
    );
    // Order-only change.
    assert_eq!(
        apply_tools(&conversation, &[c2.clone(), a2.clone()], &[]).await,
        vec![Planned {
            sections: None,
            added: Some(vec!["c".to_owned(), "a".to_owned()]),
            removed: Some(vec!["a".to_owned(), "c".to_owned()]),
            omit: None,
        }]
    );
    assert_eq!(
        apply_tools(&conversation, &[], &[]).await,
        vec![Planned {
            sections: None,
            added: None,
            removed: Some(vec!["c".to_owned(), "a".to_owned()]),
            omit: None,
        }]
    );
}

#[tokio::test]
async fn puts_tool_changes_on_the_last_section_entry_and_re_declares_every_tool_after_a_head_cut() {
    let conversation = root().await;
    let a = declaration("a", "a");
    let b = declaration("b", "b");

    assert_eq!(
        apply_tools(&conversation, &[a.clone()], &[("x", "1"), ("y", "2")]).await,
        vec![Planned {
            sections: Some(nullable_map(&[("x", Some("1")), ("y", Some("2"))])),
            added: Some(vec!["a".to_owned()]),
            removed: None,
            omit: None,
        }]
    );
    // Section order changes need two entries; the tool change rides on the second.
    assert_eq!(
        apply_tools(&conversation, &[a.clone(), b.clone()], &[("y", "2"), ("x", "1")]).await,
        vec![
            Planned {
                sections: Some(nullable_map(&[("x", None), ("y", None)])),
                added: None,
                removed: None,
                omit: None,
            },
            Planned {
                sections: Some(nullable_map(&[("y", Some("2")), ("x", Some("1"))])),
                added: Some(vec!["b".to_owned()]),
                removed: None,
                omit: None,
            },
        ]
    );
    marker(&conversation, EntryDraftHead::SelfId(SelfHead)).await;
    assert_eq!(
        apply_tools(&conversation, &[a, b], &[("y", "2"), ("x", "1")]).await,
        vec![Planned {
            sections: Some(nullable_map(&[("y", Some("2")), ("x", Some("1"))])),
            added: Some(vec!["a".to_owned(), "b".to_owned()]),
            removed: None,
            omit: None,
        }]
    );
}
