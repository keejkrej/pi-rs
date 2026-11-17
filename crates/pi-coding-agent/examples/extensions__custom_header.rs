//! Port of packages/coding-agent/examples/extensions/custom-header.ts

use std::process::ExitCode;
use std::sync::Arc;

use async_trait::async_trait;
use pi_coding_agent::VERSION;
use pi_coding_agent::core::extensions::types::{
    Extension, ExtensionApi, ExtensionFactory, ExtensionMode, InlineExtension, NotifyType, RegisteredCommandOptions,
    SessionStartEvent, Theme,
};
use pi_tui::Component;

fn get_pi_mascot(theme: &Theme) -> Vec<String> {
    let pi_blue = |text: &str| theme.fg("accent", text);
    let black = |text: &str| theme.fg("dim", text);

    let block = "█";
    let pupil = "▌";
    let eye = format!("{block}{}", black(pupil));
    let line_eyes = format!("     {eye}  {eye}");
    let line_bar = format!("  {}", pi_blue(&block.repeat(14)));
    let two_blocks = block.repeat(2);
    let line_leg = format!("     {}    {}", pi_blue(&two_blocks), pi_blue(&two_blocks));

    vec![
        String::new(),
        line_eyes,
        line_bar,
        line_leg.clone(),
        line_leg.clone(),
        line_leg.clone(),
        line_leg,
        String::new(),
    ]
}

struct MascotHeaderComponent {
    theme: Theme,
}

impl Component for MascotHeaderComponent {
    fn render(&self, _width: usize) -> Vec<String> {
        let mut lines = get_pi_mascot(&self.theme);
        let subtitle = format!(
            "{}{}",
            self.theme.fg("muted", "   shitty coding agent"),
            self.theme.fg("dim", &format!(" v{}", *VERSION))
        );
        lines.push(subtitle);
        lines
    }

    fn invalidate(&self) {}
}

pub struct CustomHeaderExtension;

#[async_trait]
impl Extension for CustomHeaderExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        pi.on::<SessionStartEvent>(|_event, ctx| {
            Box::pin(async move {
                if ctx.mode()? == ExtensionMode::Tui {
                    ctx.ui()?
                        .set_header(Some(Arc::new(|_tui, theme| Arc::new(MascotHeaderComponent { theme }))));
                }
                Ok(None)
            })
        });

        pi.register_command(
            "builtin-header",
            RegisteredCommandOptions {
                description: Some("Restore built-in header with keybinding hints".to_string()),
                get_argument_completions: None,
                handler: Arc::new(|_args, ctx| {
                    Box::pin(async move {
                        let ui = ctx.as_context().ui()?;
                        ui.set_header(None);
                        ui.notify("Built-in header restored", Some(NotifyType::Info));
                        Ok(())
                    })
                }),
            },
        )?;

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(CustomHeaderExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
