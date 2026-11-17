//! Port of packages/coding-agent/examples/extensions/git-checkpoint.ts

use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::IndexMap;
use pi_coding_agent::core::extensions::types::{
    AgentSettledEvent, Extension, ExtensionApi, ExtensionFactory, InlineExtension, NotifyType, SessionBeforeForkEvent,
    ToolResultEvent, TurnStartEvent,
};

pub struct GitCheckpointExtension;

#[async_trait]
impl Extension for GitCheckpointExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        let checkpoints: Arc<Mutex<IndexMap<String, String>>> = Arc::new(Mutex::new(IndexMap::new()));
        let current_entry_id: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

        {
            let entry_ref = Arc::clone(&current_entry_id);
            pi.on::<ToolResultEvent>(move |_event, ctx| {
                let entry_ref = Arc::clone(&entry_ref);
                Box::pin(async move {
                    if let Some(leaf_id) = ctx.session_manager()?.get_leaf_id() {
                        let mut guard = entry_ref.lock().unwrap_or_else(|e| e.into_inner());
                        *guard = Some(leaf_id);
                    }
                    Ok(None)
                })
            });
        }

        {
            let pi_ts = pi.clone();
            let cp_ts = Arc::clone(&checkpoints);
            let entry_ts = Arc::clone(&current_entry_id);
            pi.on::<TurnStartEvent>(move |_event, _ctx| {
                let pi = pi_ts.clone();
                let cp = Arc::clone(&cp_ts);
                let entry_ref = Arc::clone(&entry_ts);
                Box::pin(async move {
                    let res = pi
                        .exec("git", &["stash".to_string(), "create".to_string()], None)
                        .await?;
                    let git_ref = res.stdout.trim().to_string();
                    let entry_id = entry_ref.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    if !git_ref.is_empty()
                        && let Some(id) = entry_id
                    {
                        let mut guard = cp.lock().unwrap_or_else(|e| e.into_inner());
                        guard.insert(id, git_ref);
                    }
                    Ok(None)
                })
            });
        }

        {
            let pi_fk = pi.clone();
            let cp_fk = Arc::clone(&checkpoints);
            pi.on::<SessionBeforeForkEvent>(move |event, ctx| {
                let pi = pi_fk.clone();
                let cp = Arc::clone(&cp_fk);
                Box::pin(async move {
                    let git_ref = cp
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .get(&event.entry_id)
                        .cloned();
                    let Some(git_ref) = git_ref else {
                        return Ok(None);
                    };

                    if !ctx.has_ui()? {
                        return Ok(None);
                    }

                    let ui = ctx.ui()?;
                    let choice = ui
                        .select(
                            "Restore code state?",
                            vec![
                                "Yes, restore code to that point".to_string(),
                                "No, keep current code".to_string(),
                            ],
                            None,
                        )
                        .await?;

                    if choice.as_deref().is_some_and(|c| c.starts_with("Yes")) {
                        pi.exec("git", &["stash".to_string(), "apply".to_string(), git_ref], None)
                            .await?;
                        ui.notify("Code restored to checkpoint", Some(NotifyType::Info));
                    }
                    Ok(None)
                })
            });
        }

        {
            let cp_as = Arc::clone(&checkpoints);
            pi.on::<AgentSettledEvent>(move |_event, _ctx| {
                let cp = Arc::clone(&cp_as);
                Box::pin(async move {
                    cp.lock().unwrap_or_else(|e| e.into_inner()).clear();
                    Ok(None)
                })
            });
        }

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(GitCheckpointExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
