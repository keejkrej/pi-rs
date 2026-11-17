//! Port of packages/coding-agent/src/experimental/durable/subagent.ts

use std::sync::{Arc, LazyLock};

use pi_ai::types::{AssistantContentBlock, Message, TextContent};
use pi_chord::Context;
use pi_durable::harness::agent::configure;
use pi_durable::harness::types::{
    AgentChange, AgentExtensionsChange, Extension, FieldChange, InputSubmissionDraft, ToolExecutionApi,
    ToolExecutionResult, ToolRegistration, ToolReplayMode, ToolResultContentBlock, UserInput,
};
use pi_durable::types::{
    ConversationId, ConversationOwnership, ConversationQuery, EntryId, InputSubmissionState, SubmissionState,
};
use pi_durable::{define_extension, define_tool};
use pi_js::vendor::typebox::t;
use serde::Deserialize;
use serde_json::{Map, Value};

#[derive(Deserialize)]
struct SubagentArgs {
    task: String,
}

async fn answer_text(api: &ToolExecutionApi, answer: EntryId, context: Context) -> pi_js::Result<String> {
    let entry = api
        .commit(move |tx| async move { tx.entry(answer).await }, context)
        .await?;
    let first_msg = entry
        .and_then(|e| e.model)
        .and_then(|mut m| if m.is_empty() { None } else { Some(m.swap_remove(0)) });
    match first_msg {
        Some(Message::Assistant(assistant)) => Ok(assistant
            .content
            .into_iter()
            .filter_map(|block| match block {
                AssistantContentBlock::Text(text) => Some(text.text),
                _ => None,
            })
            .collect()),
        _ => Ok(String::new()),
    }
}

/// Foreground subagent extension: delegates a self-contained task to a child conversation.
pub static SUBAGENT: LazyLock<Extension> = LazyLock::new(|| {
    let parameters = t::object([("task", t::string().with("description", "What the subagent should do"))]).to_value();

    let tool = define_tool(ToolRegistration {
        name: "subagent".to_string(),
        description: "Delegate a self-contained task to a subagent with the same tools and get its answer back. Give it everything it needs to know; it does not see this conversation.".to_string(),
        parameters,
        replay: Some(ToolReplayMode::Safe),
        execution_mode: None,
        prepare_arguments: None,
        output_limits: None,
        execute: Arc::new(|args_val, api, context: Context| {
            Box::pin(async move {
                let args: SubagentArgs = serde_json::from_value(args_val)
                    .map_err(|e| pi_js::Error::js("TypeError", e.to_string()))?;
                let task_id = api.task_id();
                let child: ConversationId = api
                    .commit(
                        move |tx| async move {
                            let existing = tx
                                .scan_conversations(
                                    ConversationQuery {
                                        owner_conversation_id: None,
                                        owner_task_id: Some(task_id),
                                    },
                                    1,
                                    None,
                                )
                                .await?;
                            if let Some(first) = existing.items.first() {
                                return Ok(first.id);
                            }
                            let created = tx
                                .create_conversation(ConversationOwnership::Task { task_id })
                                .await?;
                            configure(
                                &tx,
                                created.id,
                                &AgentChange {
                                    extensions: FieldChange::set(AgentExtensionsChange::Patch {
                                        add: None,
                                        remove: Some(vec!["subagent".to_string()]),
                                    }),
                                    ..Default::default()
                                },
                            )
                            .await?;
                            Ok(created.id)
                        },
                        context.clone(),
                    )
                    .await?;

                let mut details_map = Map::new();
                details_map.insert("conversationId".to_string(), Value::from(child.as_i64()));
                let details_val = Value::Object(details_map);
                api.details(&details_val, context.clone()).await?;

                let handle = api
                    .conversation(child, context.clone())
                    .await?
                    .ok_or_else(|| pi_js::Error::msg(format!("Subagent {} missing", child.as_i64())))?;
                let request = InputSubmissionDraft {
                    request_id: Some(format!("subagent:{}", task_id.as_i64())),
                    content: UserInput::Text(args.task),
                    when_busy: None,
                };
                let settled = handle.submit(request, context.clone()).await?.wait(context.clone()).await?;
                let answer = match settled.record.state {
                    SubmissionState::Input(InputSubmissionState::Done { answer, .. }) => answer,
                    _ => {
                        return Err(pi_js::Error::msg(format!(
                            "Subagent {} failed: {:?}",
                            child.as_i64(),
                            settled.record.status()
                        )));
                    }
                };
                let text = answer_text(&api, answer, context).await?;
                Ok(ToolExecutionResult {
                    content: Some(vec![ToolResultContentBlock::Text(TextContent::new(text))]),
                    details: Some(details_val),
                    ..Default::default()
                })
            })
        }),
    });

    define_extension(Extension {
        name: "subagent".to_string(),
        tools: vec![tool],
        ..Default::default()
    })
});
