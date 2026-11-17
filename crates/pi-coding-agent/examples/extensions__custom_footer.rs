//! Port of packages/coding-agent/examples/extensions/custom-footer.ts

use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use pi_agent_core::types::AgentMessage;
use pi_ai::types::Message;
use pi_coding_agent::core::extensions::types::{
    Extension, ExtensionApi, ExtensionContext, ExtensionFactory, InlineExtension, NotifyType, RegisteredCommandOptions,
    Theme, mcp_wire::ReadonlyFooterDataProvider,
};
use pi_tui::{Component, truncate_to_width, visible_width};

struct CustomFooterComponent {
    ctx: ExtensionContext,
    theme: Theme,
    footer_data: Arc<dyn ReadonlyFooterDataProvider>,
}

impl Component for CustomFooterComponent {
    fn render(&self, width: usize) -> Vec<String> {
        let mut input: i64 = 0;
        let mut output: i64 = 0;
        let mut cost: f64 = 0.0;

        if let Ok(sm) = self.ctx.session_manager() {
            for e in sm.get_branch(None) {
                if e.r#type == "message"
                    && let Some(msg_val) = e.fields.get("message")
                    && let Ok(AgentMessage::Message(Message::Assistant(m))) =
                        serde_json::from_value::<AgentMessage>(msg_val.clone())
                {
                    input += m.usage.input;
                    output += m.usage.output;
                    cost += m.usage.cost.total;
                }
            }
        }

        let branch = self.footer_data.get_git_branch();
        let fmt = |n: i64| -> String {
            if n < 1000 {
                n.to_string()
            } else {
                format!("{:.1}k", (n as f64) / 1000.0)
            }
        };

        let left = self
            .theme
            .fg("dim", &format!("↑{} ↓{} ${cost:.3}", fmt(input), fmt(output)));
        let model_id = self
            .ctx
            .model()
            .ok()
            .flatten()
            .map(|m| m.id)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "no-model".to_string());
        let branch_str = branch.map(|b| format!(" ({b})")).unwrap_or_default();
        let right = self.theme.fg("dim", &format!("{model_id}{branch_str}"));

        let left_w = visible_width(&left).max(0) as usize;
        let right_w = visible_width(&right).max(0) as usize;
        let pad_len = width.saturating_sub(left_w + right_w).max(1);
        let pad = " ".repeat(pad_len);
        vec![truncate_to_width(
            &format!("{left}{pad}{right}"),
            width as i64,
            None,
            None,
        )]
    }

    fn invalidate(&self) {}
}

pub struct CustomFooterExtension;

#[async_trait]
impl Extension for CustomFooterExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        let enabled = Arc::new(AtomicBool::new(false));

        pi.register_command(
            "footer",
            RegisteredCommandOptions {
                description: Some("Toggle custom footer".to_string()),
                get_argument_completions: None,
                handler: Arc::new(move |_args, ctx| {
                    let enabled = Arc::clone(&enabled);
                    Box::pin(async move {
                        let next = !enabled.load(Ordering::SeqCst);
                        enabled.store(next, Ordering::SeqCst);

                        let base = ctx.as_context().clone();
                        let ui = base.ui()?;
                        if next {
                            ui.set_footer(Some(Arc::new(move |_tui, theme, footer_data| {
                                // WIRE(@tui-w3): subscribe via footer_data.on_branch_change(...) once @tui merges.
                                Arc::new(CustomFooterComponent {
                                    ctx: base.clone(),
                                    theme,
                                    footer_data,
                                })
                            })));
                            ui.notify("Custom footer enabled", Some(NotifyType::Info));
                        } else {
                            ui.set_footer(None);
                            ui.notify("Default footer restored", Some(NotifyType::Info));
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
    Arc::new(CustomFooterExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
