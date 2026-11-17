//! Port of packages/coding-agent/examples/extensions/message-renderer.ts

use std::process::ExitCode;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Local};
use pi_coding_agent::core::extensions::types::{
    ContentBlock, CustomMessageContent, CustomMessageDraft, Extension, ExtensionApi, ExtensionFactory, InlineExtension,
    RegisteredCommandOptions,
};
use pi_tui::{Box as TuiBox, Component, Text};
use serde::Deserialize;

#[derive(Deserialize)]
struct StatusDetails {
    level: Option<String>,
    timestamp: Option<i64>,
}

fn format_local_time(ms: i64) -> String {
    DateTime::from_timestamp_millis(ms)
        .map(|dt| dt.with_timezone(&Local).format("%H:%M:%S").to_string())
        .unwrap_or_else(|| ms.to_string())
}

fn content_to_string(content: &CustomMessageContent) -> String {
    match content {
        CustomMessageContent::Text(s) => s.clone(),
        CustomMessageContent::Blocks(blocks) => blocks
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text(t) => Some(t.text.as_str()),
                ContentBlock::Image(_) => None,
            })
            .collect::<Vec<_>>()
            .join(""),
    }
}

pub struct MessageRendererExtension;

#[async_trait]
impl Extension for MessageRendererExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        pi.register_message_renderer(
            "status-update",
            Arc::new(|message, opts, theme| {
                let details = message
                    .details
                    .and_then(|v| serde_json::from_value::<StatusDetails>(v).ok());
                let level = details.as_ref().and_then(|d| d.level.as_deref()).unwrap_or("info");

                let color = match level {
                    "error" => "error",
                    "warn" => "warning",
                    _ => "success",
                };
                let prefix = theme.fg(color, &format!("[{}]", level.to_uppercase()));
                let mut text = format!("{prefix} {}", content_to_string(&message.content));

                if opts.expanded
                    && let Some(ts) = details.as_ref().and_then(|d| d.timestamp)
                {
                    let time_str = format_local_time(ts);
                    text.push_str(&format!("\n{}", theme.fg("dim", &format!("  at {time_str}"))));
                }

                let theme_bg = theme.clone();
                let tui_box = TuiBox::new(
                    Some(opts.output_pad),
                    Some(1),
                    Some(Arc::new(move |t| theme_bg.bg("customMessageBg", t))),
                );
                tui_box.add_child(Arc::new(Text::new(Some(&text), Some(0), Some(0), None)));
                let comp: Arc<dyn Component> = Arc::new(tui_box);
                Some(comp)
            }),
        );

        let pi_cmd = pi.clone();
        pi.register_command(
            "status",
            RegisteredCommandOptions {
                description: Some("Send a status message (usage: /status [warn|error] message)".to_string()),
                get_argument_completions: None,
                handler: Arc::new(move |args, _ctx| {
                    let pi = pi_cmd.clone();
                    Box::pin(async move {
                        let trimmed = args.trim();
                        let mut parts = trimmed.split_whitespace();
                        let (level, content) = match parts.next() {
                            Some(first @ ("warn" | "error")) => {
                                let rest: Vec<&str> = parts.collect();
                                let msg = if rest.is_empty() {
                                    "Status update".to_string()
                                } else {
                                    rest.join(" ")
                                };
                                (first, msg)
                            }
                            _ => ("info", trimmed.to_string()),
                        };

                        pi.send_message(
                            CustomMessageDraft {
                                custom_type: "status-update".to_string(),
                                content: CustomMessageContent::Text(content),
                                display: true,
                                details: Some(serde_json::json!({
                                    "level": level,
                                    "timestamp": pi_js::time::now_ms(),
                                })),
                            },
                            None,
                        )
                    })
                }),
            },
        )?;

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(MessageRendererExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
