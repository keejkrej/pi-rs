//! Port of packages/durable/test/examples/09-context.ts
#![allow(dead_code)]

// Transcript history and model context.
// Run via `spec_usage::examples_smoke`.

use std::sync::Arc;

use indexmap::IndexMap;
use pi_ai::create_models;
use pi_ai::types::{
    AssistantContentBlock, AssistantMessage, Message, StopReason, SystemMessage, SystemMessageContent, TextContent,
    ToolCall, ToolResultMessage, Usage, UserContentBlock, UserMessage, UserMessageContent,
};
use pi_chord::Context;
use pi_chord::context::BACKGROUND_CONTEXT;
use pi_durable::types::{
    ContextEdit, ConversationOwnership, EntryDraft, EntryDraftHead, EntryRecord, JsonValue, SelfHead,
};
use pi_durable::{
    Conversation, ConversationCreateOptions, ConversationEntriesQuery, Harness, HarnessOptions, MemoryStorage,
    RegistryReader, create_registry,
};

// Entries are immutable. `model` holds the messages an entry contributes to
// the next model request; `data` is for the app only. context() turns the
// stored transcript into those request messages:
//   - an entry with `head` starts a new context; older entries stay stored,
//   - `edits` replace or omit what an earlier entry contributes,
//   - aborted, error, and deferred assistant messages are not sent,
//   - tool results are sent right after their call, in call order,
//   - a call without a result gets a synthesized error result.
fn assistant_message(text: &str, calls: &[&str], stop_reason: Option<StopReason>) -> AssistantMessage {
    let mut content = vec![AssistantContentBlock::Text(TextContent::new(text))];
    for id in calls {
        content.push(AssistantContentBlock::ToolCall(ToolCall::new(
            *id,
            "read",
            IndexMap::new(),
        )));
    }
    AssistantMessage {
        content,
        api: "example".to_owned(),
        provider: "example".to_owned(),
        model: "example".to_owned(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason: stop_reason.unwrap_or(if calls.is_empty() {
            StopReason::Stop
        } else {
            StopReason::ToolUse
        }),
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 2,
        duration_ms: None,
    }
}

fn tool_result_message(id: &str) -> ToolResultMessage {
    ToolResultMessage {
        tool_call_id: id.to_owned(),
        tool_name: "read".to_owned(),
        content: vec![UserContentBlock::Text(TextContent::new(format!("file {id}")))],
        details: None,
        usage: None,
        nested_calls: None,
        is_error: false,
        timestamp: 3,
        duration_ms: None,
    }
}

fn show(message: &Message) -> String {
    match message {
        Message::User(u) => match &u.content {
            UserMessageContent::Text(text) => format!("user: {text}"),
            UserMessageContent::Blocks(blocks) => {
                let text = blocks
                    .iter()
                    .filter_map(|b| match b {
                        UserContentBlock::Text(t) => Some(t.text.as_str()),
                        UserContentBlock::Image(_) => None,
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                format!("user: {text}")
            }
        },
        Message::System(s) => format!("system: {}", pi_js::json::stringify(&s.sections)),
        Message::Assistant(a) => {
            let parts = a
                .content
                .iter()
                .map(|part| match part {
                    AssistantContentBlock::Text(t) => t.text.clone(),
                    AssistantContentBlock::ToolCall(tc) => format!("call({})", tc.id),
                    AssistantContentBlock::Thinking(_) => String::new(),
                })
                .collect::<Vec<_>>()
                .join(" ");
            format!("assistant: {parts}")
        }
        Message::ToolResult(r) => format!("result({}){}", r.tool_call_id, if r.is_error { " error" } else { "" }),
    }
}

async fn say(
    transcript: &Conversation,
    kind: &str,
    model: Vec<Message>,
    context: Context,
) -> pi_js::Result<EntryRecord> {
    let cid = transcript.id();
    let kind = kind.to_owned();
    transcript
        .commit(
            move |tx| async move {
                tx.append_entry(
                    cid,
                    EntryDraft {
                        kind,
                        model: Some(model),
                        ..Default::default()
                    },
                )
                .await
            },
            context,
        )
        .await
}

pub async fn run() -> pi_js::Result<()> {
    let context = BACKGROUND_CONTEXT.clone();
    let registry = create_registry();
    let harness = Harness::open(
        Arc::new(MemoryStorage::new()),
        HarnessOptions {
            models: create_models(None),
            registry: Arc::clone(&registry) as Arc<dyn RegistryReader>,
            settings: None,
            env: None,
            conversation_created: None,
            now: None,
            on_report: None,
        },
        context.clone(),
    )
    .await?;

    let transcript = harness
        .create_conversation(
            ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                agent: None,
                init: None,
            },
            context.clone(),
        )
        .await?;
    let tid = transcript.id();

    let question = say(
        &transcript,
        "message",
        vec![Message::User(UserMessage {
            content: UserMessageContent::Text("read a and b".to_owned()),
            timestamp: 1,
        })],
        context.clone(),
    )
    .await?;
    say(
        &transcript,
        "message",
        vec![Message::Assistant(assistant_message(
            "I crashed",
            &[],
            Some(StopReason::Aborted),
        ))], // stored, never sent
        context.clone(),
    )
    .await?;
    let calls = say(
        &transcript,
        "message",
        vec![Message::Assistant(assistant_message("reading", &["a", "b"], None))],
        context.clone(),
    )
    .await?;
    say(
        &transcript,
        "message",
        vec![Message::ToolResult(tool_result_message("b"))], // results finish out of order
        context.clone(),
    )
    .await?;
    say(
        &transcript,
        "pi.system",
        vec![Message::System(SystemMessage {
            content: SystemMessageContent::Text(String::new()),
            sections: Some(IndexMap::from([("cwd".to_owned(), Some("<cwd>/repo".to_owned()))])),
            tools_added: None,
            tools_removed: None,
            timestamp: 4,
        })],
        context.clone(),
    )
    .await?;
    say(
        &transcript,
        "message",
        vec![Message::ToolResult(tool_result_message("a"))],
        context.clone(),
    )
    .await?;
    say(
        &transcript,
        "message",
        vec![Message::Assistant(assistant_message("a and b look fine", &[], None))],
        context.clone(),
    )
    .await?;

    transcript
        .commit(
            move |tx| async move {
                tx.append_entry(
                    tid,
                    EntryDraft {
                        kind: "edit".to_owned(),
                        data: Some(JsonValue::String("user fixed a typo".to_owned())),
                        edits: Some(vec![ContextEdit::Replace {
                            target: question.id,
                            messages: vec![Message::User(UserMessage {
                                content: UserMessageContent::Text("read files a and b".to_owned()),
                                timestamp: 1,
                            })],
                        }]),
                        ..Default::default()
                    },
                )
                .await
            },
            context.clone(),
        )
        .await?;
    transcript
        .commit(
            move |tx| async move {
                tx.append_entry(
                    tid,
                    EntryDraft {
                        kind: "note".to_owned(),
                        data: Some(JsonValue::String("display only".to_owned())),
                        ..Default::default()
                    },
                )
                .await
            },
            context.clone(),
        )
        .await?;

    let mut view = transcript.context(context.clone()).await?;
    let raw_kinds: Vec<&str> = view.entries.iter().map(|entry| entry.kind.as_str()).collect();
    let req_msgs: Vec<String> = view.messages.iter().map(show).collect();
    pi_js::console::log(&format!("raw active entries: {raw_kinds:?}"));
    pi_js::console::log(&format!("request messages: {req_msgs:?}"));

    // A fork at the tool call has no results yet; context() fills them in.
    let cut = transcript
        .fork(
            calls.id,
            ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                agent: None,
                init: None,
            },
            context.clone(),
        )
        .await?;
    let cut_msgs: Vec<String> = cut.context(context.clone()).await?.messages.iter().map(show).collect();
    pi_js::console::log(&format!("fork messages: {cut_msgs:?}"));

    // A headed summary replaces everything before the entry it points at.
    // EntryDraftHead::SelfId(SelfHead) points the head at the summary entry itself.
    transcript
        .commit(
            move |tx| async move {
                tx.append_entry(
                    tid,
                    EntryDraft {
                        kind: "summary".to_owned(),
                        head: Some(EntryDraftHead::SelfId(SelfHead)),
                        model: Some(vec![Message::User(UserMessage {
                            content: UserMessageContent::Text("Summary: a and b are fine.".to_owned()),
                            timestamp: 5,
                        })]),
                        ..Default::default()
                    },
                )
                .await
            },
            context.clone(),
        )
        .await?;
    view = transcript.context(context.clone()).await?;
    let after_msgs: Vec<String> = view.messages.iter().map(show).collect();
    pi_js::console::log(&format!(
        "after summary: {:?} {after_msgs:?}",
        view.head.as_ref().map(|h| h.kind.as_str())
    ));

    // entries() pages the stored transcript, newest first, including inherited
    // parent entries. Nothing is ever deleted by heads or edits.
    let history = transcript
        .entries(ConversationEntriesQuery::default(), 3, None, context.clone())
        .await?;
    let newest_kinds: Vec<&str> = history.items.iter().map(|entry| entry.kind.as_str()).collect();
    pi_js::console::log(&format!(
        "newest stored entries: {newest_kinds:?} more: {}",
        history.next.is_some()
    ));

    harness.close(context).await?;
    Ok(())
}
