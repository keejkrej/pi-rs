//! Port of packages/coding-agent/examples/extensions/auto-commit-on-exit.ts

use std::process::ExitCode;
use std::sync::Arc;

use async_trait::async_trait;
use pi_agent_core::types::AgentMessage;
use pi_ai::types::{AssistantContentBlock, Message};
use pi_coding_agent::core::extensions::types::{
    Extension, ExtensionApi, ExtensionFactory, InlineExtension, NotifyType, SessionShutdownEvent,
};

pub struct AutoCommitOnExitExtension;

#[async_trait]
impl Extension for AutoCommitOnExitExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        let pi_hook = pi.clone();
        pi.on::<SessionShutdownEvent>(Arc::new(move |_event, ctx| {
            let pi = pi_hook.clone();
            Box::pin(async move {
                let status_res = pi
                    .exec("git", &["status".to_string(), "--porcelain".to_string()], None)
                    .await?;
                if status_res.code != 0 || status_res.stdout.trim().is_empty() {
                    return Ok(None);
                }

                let entries = ctx.session_manager()?.get_entries();
                let mut last_assistant_text = String::new();
                for entry in entries.iter().rev() {
                    if entry.r#type == "message"
                        && let Some(msg_val) = entry.fields.get("message")
                        && let Ok(AgentMessage::Message(Message::Assistant(assistant))) =
                            serde_json::from_value::<AgentMessage>(msg_val.clone())
                    {
                        last_assistant_text = assistant
                            .content
                            .iter()
                            .filter_map(|c| match c {
                                AssistantContentBlock::Text(t) => Some(t.text.as_str()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("\n");
                        break;
                    }
                }

                let first_line = last_assistant_text.lines().next().unwrap_or("");
                let first_line = if first_line.is_empty() {
                    "Work in progress"
                } else {
                    first_line
                };
                let prefix: String = first_line.chars().take(50).collect();
                let ellipsis = if first_line.chars().count() > 50 { "..." } else { "" };
                let commit_message = format!("[pi] {prefix}{ellipsis}");

                pi.exec("git", &["add".to_string(), "-A".to_string()], None).await?;
                let commit_res = pi
                    .exec(
                        "git",
                        &["commit".to_string(), "-m".to_string(), commit_message.clone()],
                        None,
                    )
                    .await?;

                if commit_res.code == 0 && ctx.has_ui()? {
                    ctx.ui()?
                        .notify(&format!("Auto-committed: {commit_message}"), Some(NotifyType::Info));
                }

                Ok(None)
            })
        }));

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(AutoCommitOnExitExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
