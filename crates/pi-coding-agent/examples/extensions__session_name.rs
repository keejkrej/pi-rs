//! Port of packages/coding-agent/examples/extensions/session-name.ts

use std::process::ExitCode;
use std::sync::Arc;

use async_trait::async_trait;
use pi_coding_agent::core::extensions::types::{
    Extension, ExtensionApi, ExtensionFactory, InlineExtension, NotifyType, RegisteredCommandOptions,
};

pub struct SessionNameExtension;

#[async_trait]
impl Extension for SessionNameExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        let pi_cmd = pi.clone();
        pi.register_command(
            "session-name",
            RegisteredCommandOptions {
                description: Some("Set or show session name (usage: /session-name [new name])".to_string()),
                get_argument_completions: None,
                handler: Arc::new(move |args, ctx| {
                    let pi = pi_cmd.clone();
                    Box::pin(async move {
                        let name = args.trim();
                        let ui = ctx.as_context().ui()?;
                        if !name.is_empty() {
                            pi.set_session_name(name)?;
                            ui.notify(&format!("Session named: {name}"), Some(NotifyType::Info));
                        } else {
                            let current = pi.get_session_name()?;
                            let msg = match current {
                                Some(s) if !s.is_empty() => format!("Session: {s}"),
                                _ => "No session name set".to_string(),
                            };
                            ui.notify(&msg, Some(NotifyType::Info));
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
    Arc::new(SessionNameExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
