//! Port of packages/coding-agent/examples/extensions/mac-system-theme.ts

use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use pi_coding_agent::core::extensions::types::{
    Extension, ExtensionApi, ExtensionFactory, InlineExtension, SessionShutdownEvent, SessionStartEvent,
};
use tokio::process::Command;
use tokio::task::JoinHandle;

async fn is_dark_mode() -> bool {
    let output = Command::new("osascript")
        .args([
            "-e",
            "tell application \"System Events\" to tell appearance preferences to return dark mode",
        ])
        .output()
        .await;
    match output {
        Ok(out) => String::from_utf8_lossy(&out.stdout).trim() == "true",
        Err(_) => false,
    }
}

pub struct MacSystemThemeExtension;

#[async_trait]
impl Extension for MacSystemThemeExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        let task_handle: Arc<Mutex<Option<JoinHandle<()>>>> = Arc::new(Mutex::new(None));

        {
            let handle_ss = Arc::clone(&task_handle);
            pi.on::<SessionStartEvent>(move |_event, ctx| {
                let handle_slot = Arc::clone(&handle_ss);
                Box::pin(async move {
                    let mut current_theme = if is_dark_mode().await { "dark" } else { "light" };
                    if let Ok(ui) = ctx.ui() {
                        ui.set_theme_by_name(current_theme);
                    }

                    let joined = tokio::spawn(async move {
                        let mut iv = tokio::time::interval(Duration::from_secs(2));
                        iv.tick().await;
                        loop {
                            iv.tick().await;
                            let new_theme = if is_dark_mode().await { "dark" } else { "light" };
                            if new_theme != current_theme {
                                current_theme = new_theme;
                                if let Ok(ui) = ctx.ui() {
                                    ui.set_theme_by_name(current_theme);
                                }
                            }
                        }
                    });

                    let old = handle_slot.lock().unwrap_or_else(|e| e.into_inner()).replace(joined);
                    if let Some(prev) = old {
                        prev.abort();
                    }
                    Ok(None)
                })
            });
        }

        {
            let handle_shut = Arc::clone(&task_handle);
            pi.on::<SessionShutdownEvent>(move |_event, _ctx| {
                let handle_slot = Arc::clone(&handle_shut);
                Box::pin(async move {
                    let old = handle_slot.lock().unwrap_or_else(|e| e.into_inner()).take();
                    if let Some(h) = old {
                        h.abort();
                    }
                    Ok(None)
                })
            });
        }

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(MacSystemThemeExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
