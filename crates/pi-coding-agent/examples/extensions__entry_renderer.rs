//! Port of packages/coding-agent/examples/extensions/entry-renderer.ts

use std::process::ExitCode;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Local};
use pi_coding_agent::core::extensions::types::{
    Extension, ExtensionApi, ExtensionFactory, InlineExtension, RegisteredCommandOptions,
};
use pi_tui::{Box as TuiBox, Component, Text};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StatusCardData {
    message: String,
    timestamp: i64,
}

fn format_local_timestamp(ms: i64) -> String {
    DateTime::from_timestamp_millis(ms)
        .map(|dt| dt.with_timezone(&Local).format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| ms.to_string())
}

pub struct EntryRendererExtension;

#[async_trait]
impl Extension for EntryRendererExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        pi.register_entry_renderer(
            "status-card",
            Arc::new(|entry, opts, theme| {
                let data = entry
                    .data
                    .and_then(|v| serde_json::from_value::<StatusCardData>(v).ok())
                    .unwrap_or_else(|| StatusCardData {
                        message: "No data".to_string(),
                        timestamp: pi_js::time::now_ms(),
                    });

                let theme_bg = theme.clone();
                let tui_box = TuiBox::new(
                    Some(1),
                    Some(1),
                    Some(Arc::new(move |text| theme_bg.bg("customMessageBg", text))),
                );
                tui_box.add_child(Arc::new(Text::new(
                    Some(&format!("{} {}", theme.fg("accent", "[status]"), data.message)),
                    Some(0),
                    Some(0),
                    None,
                )));

                if opts.expanded {
                    let formatted = format_local_timestamp(data.timestamp);
                    tui_box.add_child(Arc::new(Text::new(
                        Some(&theme.fg("dim", &formatted)),
                        Some(0),
                        Some(0),
                        None,
                    )));
                }

                let comp: Arc<dyn Component> = Arc::new(tui_box);
                Some(comp)
            }),
        );

        let pi_cmd = pi.clone();
        pi.register_command(
            "status-card",
            RegisteredCommandOptions {
                description: Some("Render a durable status card that is not sent to the LLM".to_string()),
                get_argument_completions: None,
                handler: Arc::new(move |args, _ctx| {
                    let pi = pi_cmd.clone();
                    Box::pin(async move {
                        let trimmed = args.trim();
                        let message = if trimmed.is_empty() {
                            "Status card".to_string()
                        } else {
                            trimmed.to_string()
                        };
                        let data = StatusCardData {
                            message,
                            timestamp: pi_js::time::now_ms(),
                        };
                        let value = serde_json::to_value(data).map_err(|e| pi_js::Error::msg(e.to_string()))?;
                        pi.append_entry("status-card", Some(value))
                    })
                }),
            },
        )?;

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(EntryRendererExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
