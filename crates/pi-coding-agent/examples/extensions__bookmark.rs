//! Port of packages/coding-agent/examples/extensions/bookmark.ts

use std::process::ExitCode;
use std::sync::Arc;

use async_trait::async_trait;
use pi_coding_agent::core::extensions::types::{
    Extension, ExtensionApi, ExtensionFactory, InlineExtension, NotifyType, RegisteredCommandOptions,
};

pub struct BookmarkExtension;

#[async_trait]
impl Extension for BookmarkExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        {
            let pi_bm = pi.clone();
            pi.register_command(
                "bookmark",
                RegisteredCommandOptions {
                    description: Some("Bookmark last message (usage: /bookmark [label])".to_string()),
                    get_argument_completions: None,
                    handler: Arc::new(move |args, ctx| {
                        let pi = pi_bm.clone();
                        Box::pin(async move {
                            let base = ctx.as_context();
                            let trimmed = args.trim();
                            let label = if trimmed.is_empty() {
                                format!("bookmark-{}", pi_js::time::now_ms())
                            } else {
                                trimmed.to_string()
                            };

                            let entries = base.session_manager()?.get_entries();
                            for entry in entries.iter().rev() {
                                if let pi_coding_agent::core::session_manager::SessionEntry::Message(msg_entry) = entry
                                    && msg_entry.message.role() == Some("assistant")
                                {
                                    pi.set_label(&msg_entry.id, Some(&label))?;
                                    base.ui()?
                                        .notify(&format!("Bookmarked as: {label}"), Some(NotifyType::Info));
                                    return Ok(());
                                }
                            }

                            base.ui()?
                                .notify("No assistant message to bookmark", Some(NotifyType::Warning));
                            Ok(())
                        })
                    }),
                },
            )?;
        }

        {
            let pi_ub = pi.clone();
            pi.register_command(
                "unbookmark",
                RegisteredCommandOptions {
                    description: Some("Remove bookmark from last labeled entry".to_string()),
                    get_argument_completions: None,
                    handler: Arc::new(move |_args, ctx| {
                        let pi = pi_ub.clone();
                        Box::pin(async move {
                            let base = ctx.as_context();
                            let sm = base.session_manager()?;
                            let entries = sm.get_entries();
                            for entry in entries.iter().rev() {
                                if let Some(label) = sm.get_label(entry.id()) {
                                    pi.set_label(entry.id(), None)?;
                                    base.ui()?
                                        .notify(&format!("Removed bookmark: {label}"), Some(NotifyType::Info));
                                    return Ok(());
                                }
                            }
                            base.ui()?
                                .notify("No bookmarked entry found", Some(NotifyType::Warning));
                            Ok(())
                        })
                    }),
                },
            )?;
        }

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(BookmarkExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
