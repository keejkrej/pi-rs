//! Port of packages/coding-agent/examples/extensions/custom-compaction.ts

use std::process::ExitCode;
use std::sync::Arc;

use async_trait::async_trait;
use pi_ai::compat::complete_simple;
use pi_ai::types::{
    AssistantContentBlock, CacheRetention, Context, Message, SimpleStreamOptions, StopReason, StreamOptions,
    TextContent, UserContentBlock, UserMessage, UserMessageContent,
};
use pi_coding_agent::core::compaction::{CompactionResult, serialize_conversation};
use pi_coding_agent::core::extensions::types::{
    Extension, ExtensionApi, ExtensionFactory, InlineExtension, NotifyType, SessionBeforeCompactEvent,
    SessionBeforeCompactResult,
};
use pi_coding_agent::core::messages::convert_to_llm;

pub struct CustomCompactionExtension;

#[async_trait]
impl Extension for CustomCompactionExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        pi.on::<SessionBeforeCompactEvent>(|event, ctx| {
            Box::pin(async move {
                if let Ok(ui) = ctx.ui() {
                    ui.notify("Custom compaction extension triggered", Some(NotifyType::Info));
                }

                let prep = &event.preparation;
                let signal = event.signal.clone();

                let registry = ctx.model_registry()?;
                let model = registry
                    .get_available()
                    .into_iter()
                    .find(|m| m.provider == "google" && m.id == "gemini-2.5-flash");

                let Some(model) = model else {
                    if let Ok(ui) = ctx.ui() {
                        ui.notify(
                            "Could not find Gemini Flash model, using default compaction",
                            Some(NotifyType::Warning),
                        );
                    }
                    return Ok(None);
                };

                let mut all_messages = prep.messages_to_summarize.clone();
                all_messages.extend(prep.turn_prefix_messages.clone());

                if let Ok(ui) = ctx.ui() {
                    ui.notify(
                        &format!(
                            "Custom compaction: summarizing {} messages ({} tokens) with {}...",
                            all_messages.len(),
                            prep.tokens_before,
                            model.id
                        ),
                        Some(NotifyType::Info),
                    );
                }

                let llm_messages = convert_to_llm(&all_messages);
                let conversation_text = serialize_conversation(&llm_messages);

                let previous_context = match &prep.previous_summary {
                    Some(prev) if !prev.is_empty() => format!("\n\nPrevious session summary for context:\n{prev}"),
                    _ => String::new(),
                };

                let prompt = format!(
                    "You are a conversation summarizer. Create a comprehensive summary of this conversation that captures:{previous_context}\n\n\
1. The main goals and objectives discussed\n\
2. Key decisions made and their rationale\n\
3. Important code changes, file modifications, or technical details\n\
4. Current state of any ongoing work\n\
5. Any blockers, issues, or open questions\n\
6. Next steps that were planned or suggested\n\n\
Be thorough but concise. The summary will replace the ENTIRE conversation history, so include all information needed to continue the work effectively.\n\n\
Format the summary as structured markdown with clear sections.\n\n\
<conversation>\n\
{conversation_text}\n\
</conversation>"
                );

                let summary_msg = Message::User(UserMessage {
                    content: UserMessageContent::Blocks(vec![UserContentBlock::Text(TextContent::new(prompt))]),
                    timestamp: pi_js::time::now_ms() as i64,
                });

                let api_key = match registry.get_api_key_and_headers(&model).await {
                    pi_coding_agent::core::model_registry::ResolvedRequestAuth::Ok { api_key, .. } => api_key,
                    pi_coding_agent::core::model_registry::ResolvedRequestAuth::Err { .. } => None,
                };
                let response = complete_simple(
                    &model,
                    &Context {
                        system_prompt: None,
                        messages: vec![summary_msg],
                        tools: None,
                    },
                    Some(SimpleStreamOptions {
                        base: StreamOptions {
                            api_key,
                            signal: signal.clone(),
                            max_tokens: Some(8192),
                            cache_retention: Some(CacheRetention::None),
                            session_id: pi_ai::uuidv7(None).ok(),
                            ..Default::default()
                        },
                        ..Default::default()
                    }),
                )
                .await;

                if response.stop_reason == StopReason::Error {
                    let message = response
                        .error_message
                        .unwrap_or_else(|| "Unknown model error".to_string());
                    if let Ok(ui) = ctx.ui() {
                        ui.notify(&format!("Compaction failed: {message}"), Some(NotifyType::Error));
                    }
                    return Ok(None);
                }

                let summary = response
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        AssistantContentBlock::Text(tc) => Some(tc.text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");

                if summary.trim().is_empty() {
                    let aborted = signal.as_ref().is_some_and(|s| s.aborted())
                        || response.stop_reason == StopReason::Aborted;
                    if !aborted && let Ok(ui) = ctx.ui() {
                        ui.notify(
                            "Compaction summary was empty, using default compaction",
                            Some(NotifyType::Warning),
                        );
                    }
                    return Ok(None);
                }

                Ok(Some(SessionBeforeCompactResult {
                    cancel: None,
                    compaction: Some(CompactionResult {
                        summary,
                        first_kept_entry_id: prep.first_kept_entry_id.clone(),
                        tokens_before: prep.tokens_before,
                        estimated_tokens_after: None,
                        usage: Some(response.usage),
                        details: None,
                    }),
                }))
            })
        });

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(CustomCompactionExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
