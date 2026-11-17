//! Port of packages/coding-agent/examples/extensions/file-trigger.ts

use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_coding_agent::core::extensions::types::{
    CustomMessageContent, CustomMessageDraft, Extension, ExtensionApi, ExtensionFactory, InlineExtension, NotifyType,
    SendMessageOptions, SessionStartEvent,
};
use pi_js::fs::StatWatcher;

const TRIGGER_FILE: &str = "/tmp/agent-trigger.txt";

pub struct FileTriggerExtension;

#[async_trait]
impl Extension for FileTriggerExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        let watcher_slot: Arc<Mutex<Option<StatWatcher>>> = Arc::new(Mutex::new(None));
        let pi_ss = pi.clone();

        pi.on::<SessionStartEvent>(Arc::new(move |_event, ctx| {
            let pi = pi_ss.clone();
            let watcher_slot = Arc::clone(&watcher_slot);
            Box::pin(async move {
                let pi_watch = pi.clone();
                if let Ok(watcher) = pi_js::fs::watch_file(TRIGGER_FILE, 500, move |_curr, _prev| {
                    if let Ok(raw) = pi_js::fs::read_to_string(TRIGGER_FILE) {
                        let content = raw.trim().to_string();
                        if !content.is_empty() {
                            let _ = pi_watch.send_message(
                                CustomMessageDraft {
                                    custom_type: "file-trigger".to_string(),
                                    content: CustomMessageContent::Text(format!("External trigger: {content}")),
                                    display: true,
                                    details: None,
                                },
                                Some(SendMessageOptions {
                                    trigger_turn: Some(true),
                                    deliver_as: None,
                                }),
                            );
                            let _ = pi_js::fs::write_file(TRIGGER_FILE, b"", None);
                        }
                    }
                }) {
                    let old = watcher_slot.lock().unwrap_or_else(|e| e.into_inner()).replace(watcher);
                    if let Some(prev) = old {
                        prev.stop();
                    }
                }

                if ctx.has_ui()? {
                    ctx.ui()?
                        .notify(&format!("Watching {TRIGGER_FILE}"), Some(NotifyType::Info));
                }
                Ok(None)
            })
        }));

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(FileTriggerExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
