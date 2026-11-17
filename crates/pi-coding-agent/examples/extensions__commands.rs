//! Port of packages/coding-agent/examples/extensions/commands.ts

use std::process::ExitCode;
use std::sync::Arc;

use async_trait::async_trait;
use pi_coding_agent::core::extensions::types::{
    Extension, ExtensionApi, ExtensionFactory, InlineExtension, NotifyType, RegisteredCommandOptions, SlashCommandInfo,
    SlashCommandSource,
};
use pi_tui::AutocompleteItem;

fn format_command(cmd: &SlashCommandInfo) -> String {
    match cmd.description.as_deref() {
        Some(desc) if !desc.is_empty() => format!("/{} - {desc}", cmd.name),
        _ => format!("/{}", cmd.name),
    }
}

pub struct CommandsExtension;

#[async_trait]
impl Extension for CommandsExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        let pi_cmd = pi.clone();
        pi.register_command(
            "commands",
            RegisteredCommandOptions {
                description: Some("List available slash commands".to_string()),
                get_argument_completions: Some(Arc::new(|prefix| {
                    let prefix = prefix.to_string();
                    Box::pin(async move {
                        let sources = ["extension", "prompt", "skill"];
                        let items: Vec<AutocompleteItem> = sources
                            .into_iter()
                            .filter(|s| s.starts_with(&prefix))
                            .map(|s| AutocompleteItem {
                                value: s.to_string(),
                                label: s.to_string(),
                                description: None,
                            })
                            .collect();
                        if items.is_empty() { None } else { Some(items) }
                    })
                })),
                handler: Arc::new(move |args, ctx| {
                    let pi = pi_cmd.clone();
                    Box::pin(async move {
                        let base = ctx.as_context();
                        let ui = base.ui()?;
                        let commands = pi.get_commands()?;
                        let source_filter = args.trim();
                        let filtered_source = match source_filter {
                            "extension" => Some(SlashCommandSource::Extension),
                            "prompt" => Some(SlashCommandSource::Prompt),
                            "skill" => Some(SlashCommandSource::Skill),
                            _ => None,
                        };

                        let filtered: Vec<&SlashCommandInfo> = if source_filter.is_empty() {
                            commands.iter().collect()
                        } else if let Some(target) = filtered_source {
                            commands.iter().filter(|c| c.source == target).collect()
                        } else {
                            Vec::new()
                        };

                        if filtered.is_empty() {
                            let msg = if source_filter.is_empty() {
                                "No commands found".to_string()
                            } else {
                                format!("No {source_filter} commands found")
                            };
                            ui.notify(&msg, Some(NotifyType::Info));
                            return Ok(());
                        }

                        let mut items = Vec::new();
                        let groups = [
                            (SlashCommandSource::Extension, "Extensions"),
                            (SlashCommandSource::Prompt, "Prompts"),
                            (SlashCommandSource::Skill, "Skills"),
                        ];
                        for (key, label) in groups {
                            let cmds: Vec<&&SlashCommandInfo> = filtered.iter().filter(|c| c.source == key).collect();
                            if !cmds.is_empty() {
                                items.push(format!("--- {label} ---"));
                                for c in cmds {
                                    items.push(format_command(c));
                                }
                            }
                        }

                        let selected = ui.select("Available Commands", items, None).await?;
                        if let Some(sel) = selected
                            && !sel.starts_with("---")
                        {
                            let first_part = sel.split(" - ").next().unwrap_or("");
                            let cmd_name = first_part.strip_prefix('/').unwrap_or(first_part);
                            if let Some(cmd) = commands.iter().find(|c| c.name == cmd_name)
                                && !cmd.source_info.path.is_empty()
                            {
                                let show_path = ui
                                    .confirm(&cmd.name, &format!("View source path?\n{}", cmd.source_info.path), None)
                                    .await?;
                                if show_path {
                                    ui.notify(&cmd.source_info.path, Some(NotifyType::Info));
                                }
                            }
                        }

                        Ok(())
                    })
                }),
            },
        )?;

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(CommandsExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
