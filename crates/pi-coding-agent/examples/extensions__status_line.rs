//! Port of packages/coding-agent/examples/extensions/status-line.ts

use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use pi_coding_agent::core::extensions::types::{
    Extension, ExtensionApi, ExtensionFactory, InlineExtension, SessionStartEvent, TurnEndEvent, TurnStartEvent,
};

pub struct StatusLineExtension;

#[async_trait]
impl Extension for StatusLineExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        let turn_count = Arc::new(AtomicUsize::new(0));

        pi.on::<SessionStartEvent>(|_event, ctx| {
            Box::pin(async move {
                let ui = ctx.ui()?;
                let theme = ui.theme();
                ui.set_status("status-demo", Some(&theme.fg("dim", "Ready")));
                Ok(None)
            })
        });

        {
            let tc = Arc::clone(&turn_count);
            pi.on::<TurnStartEvent>(move |_event, ctx| {
                let tc = Arc::clone(&tc);
                Box::pin(async move {
                    let count = tc.fetch_add(1, Ordering::SeqCst) + 1;
                    let ui = ctx.ui()?;
                    let theme = ui.theme();
                    let spinner = theme.fg("accent", "●");
                    let text = theme.fg("dim", &format!(" Turn {count}..."));
                    ui.set_status("status-demo", Some(&format!("{spinner}{text}")));
                    Ok(None)
                })
            });
        }

        {
            let tc = Arc::clone(&turn_count);
            pi.on::<TurnEndEvent>(move |_event, ctx| {
                let tc = Arc::clone(&tc);
                Box::pin(async move {
                    let count = tc.load(Ordering::SeqCst);
                    let ui = ctx.ui()?;
                    let theme = ui.theme();
                    let check = theme.fg("success", "✓");
                    let text = theme.fg("dim", &format!(" Turn {count} complete"));
                    ui.set_status("status-demo", Some(&format!("{check}{text}")));
                    Ok(None)
                })
            });
        }

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(StatusLineExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
