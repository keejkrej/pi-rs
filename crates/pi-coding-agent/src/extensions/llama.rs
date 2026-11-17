//! Port of packages/coding-agent/src/extensions/llama/index.ts

// @generated-mods begin (scaffold-owned, do not edit)
pub mod client;
pub mod huggingface;
pub mod provider;
pub mod ui;
// @generated-mods end

use std::sync::Arc;

use crate::core::extensions::types::{
    ExtensionApi, ExtensionCommandContext, ExtensionFactory, ExtensionMode, NotifyType, RegisteredCommandOptions,
};
use crate::extensions::llama::client::{
    ListOptions, LlamaClient, LlamaModelInfo, LlamaModelStatus, format_bytes, normalize_llama_server_url,
};
use crate::extensions::llama::huggingface::{HuggingFaceClient, HuggingFaceGated, find_hugging_face_token};
use crate::extensions::llama::provider::{
    LLAMA_PROVIDER_ID, LlamaProviderController, SetCatalogOptions, create_llama_provider,
};
use crate::extensions::llama::ui::{
    HfSearchFn, LlamaManagerAction, LlamaUi, ProgressOutcome, RunWithProgressOptions, show_llama_ui,
};
use pi_js::abort::AbortSignal;
use pi_js::{Error, Result};

fn model_is_loaded(model: &LlamaModelInfo) -> bool {
    matches!(
        model.status.value,
        LlamaModelStatus::Loaded | LlamaModelStatus::Sleeping
    )
}

fn is_connection_error(error: &Error) -> bool {
    let lower = format!("{} {}", error.name(), error.message()).to_ascii_lowercase();
    lower.contains("fetch failed")
        || lower.contains("timeout")
        || lower.contains("network")
        || lower.contains("could not reach")
}

fn connection_error_message(error: &Error) -> String {
    if is_connection_error(error) {
        "Could not connect to the server.".to_string()
    } else {
        error.message().to_string()
    }
}

fn parse_hugging_face_model(value: &str) -> (String, Option<String>) {
    let slash = value.find('/');
    let after_slash = slash.map(|i| i + 1).unwrap_or(0);
    if let Some(rel_colon) = value[after_slash..].find(':') {
        let colon = after_slash + rel_colon;
        (value[..colon].to_string(), Some(value[colon + 1..].to_string()))
    } else {
        (value.to_string(), None)
    }
}

async fn configured_client(ctx: &ExtensionCommandContext) -> Result<Option<LlamaClient>> {
    let Some(result) = ctx.base.model_registry()?.get_provider_auth(LLAMA_PROVIDER_ID).await? else {
        ctx.base.ui()?.notify(
            &format!("Configure llama.cpp with /login {LLAMA_PROVIDER_ID}"),
            Some(NotifyType::Warning),
        );
        return Ok(None);
    };
    let configured_url = result
        .env
        .as_ref()
        .and_then(|env| env.get("LLAMA_BASE_URL"))
        .map(String::as_str)
        .filter(|s| !s.is_empty())
        .or(result.auth.base_url.as_deref())
        .unwrap_or("");
    let server_url = normalize_llama_server_url(configured_url)?;
    Ok(Some(LlamaClient::new(&server_url, result.auth.api_key.as_deref())?))
}

async fn sync_catalog(
    ctx: &ExtensionCommandContext,
    provider: &LlamaProviderController,
    client: &LlamaClient,
    catalog: Option<Vec<LlamaModelInfo>>,
) -> Result<Vec<LlamaModelInfo>> {
    let signal = AbortSignal::timeout(15_000);
    let current = match catalog {
        Some(c) => c,
        None => {
            client
                .list(ListOptions {
                    reload: false,
                    signal: Some(signal.clone()),
                })
                .await?
        }
    };
    provider.set_catalog(&current, &client.server_url, SetCatalogOptions::default())?;
    let result = ctx
        .base
        .model_registry()?
        .refresh(Some(pi_ai::models::ModelsRefreshOptions {
            providers: Some(vec![LLAMA_PROVIDER_ID.to_string()]),
            allow_network: Some(true),
            signal: Some(signal),
            force: None,
        }))
        .await?;
    if result.aborted {
        return Err(Error::msg("Model catalog refresh timed out."));
    }
    if let Some(err) = result.errors.get(LLAMA_PROVIDER_ID) {
        return Err(Error::msg(err.to_string()));
    }
    Ok(current)
}

async fn load_model(
    ctx: &ExtensionCommandContext,
    provider: &LlamaProviderController,
    ui: &Arc<dyn LlamaUi>,
    client: &LlamaClient,
    catalog: &[LlamaModelInfo],
    target: &LlamaModelInfo,
) -> Result<()> {
    let loaded: Vec<LlamaModelInfo> = catalog
        .iter()
        .filter(|m| m.id != target.id && model_is_loaded(m))
        .cloned()
        .collect();
    let mut replace = false;
    if !loaded.is_empty() {
        let noun = if loaded.len() == 1 { "model is" } else { "models are" };
        let choice = ui
            .select(
                &format!("{} {noun} loaded", loaded.len()),
                &[
                    "Unload all and load".to_string(),
                    "Keep loaded and load".to_string(),
                    "Cancel".to_string(),
                ],
            )
            .await;
        match choice.as_deref() {
            Some("Unload all and load") => replace = true,
            Some("Keep loaded and load") => replace = false,
            _ => return Ok(()),
        }
    }

    if replace {
        for model in &loaded {
            client.unload_and_wait(&model.id, None).await?;
        }
    }

    let client_run = client.clone();
    let target_id_run = target.id.clone();
    let client_cancel = client.clone();
    let target_id_cancel = target.id.clone();

    let run_res = crate::extensions::llama::ui::run_with_progress(
        ui,
        RunWithProgressOptions {
            title: "Loading model".to_string(),
            model: target.id.clone(),
            initial_message: "Starting…".to_string(),
            cancel_title: "Stop loading?".to_string(),
            cancel_message: target.id.clone(),
            run: Box::new(move |sig, update| {
                Box::pin(async move { client_run.load_and_wait(&target_id_run, update, Some(sig)).await })
            }),
            cancel: Box::new(move || {
                let c = client_cancel.clone();
                let id = target_id_cancel.clone();
                Box::pin(async move { c.unload(&id, None).await })
            }),
        },
    )
    .await;

    let restore = |loaded_list: Vec<LlamaModelInfo>| {
        let c = client.clone();
        let p = provider.clone();
        let ctx_c = ctx.clone();
        async move {
            ctx_c.base.ui()?.notify("Restoring previously loaded models", None);
            let noop = Arc::new(|_| {});
            for m in loaded_list {
                c.load_and_wait(&m.id, noop.clone(), None).await?;
            }
            let _ = sync_catalog(&ctx_c, &p, &c, None).await?;
            Ok::<_, Error>(())
        }
    };

    match run_res {
        Ok(ProgressOutcome::Cancelled) => {
            if replace {
                restore(loaded).await?;
            }
            Ok(())
        }
        Ok(ProgressOutcome::Completed(_)) => {
            let refreshed = sync_catalog(ctx, provider, client, None).await?;
            let loaded_model = refreshed.into_iter().find(|m| m.id == target.id);
            let msg = if loaded_model
                .as_ref()
                .is_some_and(|m| m.status.value == LlamaModelStatus::Loaded)
            {
                format!("Loaded {}", target.id)
            } else {
                format!("Load started for {}", target.id)
            };
            ctx.base.ui()?.notify(&msg, None);
            Ok(())
        }
        Err(err) => {
            if replace {
                let _ = restore(loaded).await;
            }
            Err(err)
        }
    }
}

async fn unload_model(
    ctx: &ExtensionCommandContext,
    provider: &LlamaProviderController,
    ui: &Arc<dyn LlamaUi>,
    client: &LlamaClient,
    model: &LlamaModelInfo,
) -> Result<()> {
    if !ui.confirm("Unload model?", &model.id).await {
        return Ok(());
    }
    client.unload_and_wait(&model.id, None).await?;
    sync_catalog(ctx, provider, client, None).await?;
    ctx.base.ui()?.notify(&format!("Unloaded {}", model.id), None);
    Ok(())
}

async fn download_model(
    ctx: &ExtensionCommandContext,
    provider: &LlamaProviderController,
    ui: &Arc<dyn LlamaUi>,
    client: &LlamaClient,
) -> Result<()> {
    let hf = Arc::new(HuggingFaceClient::new(find_hugging_face_token(None).await, None));
    let hf_search = Arc::clone(&hf);
    let search_fn: HfSearchFn = Arc::new(move |query, sig| {
        let hf_c = Arc::clone(&hf_search);
        Box::pin(async move { hf_c.search(&query, Some(sig)).await })
    });
    let Some(selected) = ui.search_models(search_fn).await else {
        return Ok(());
    };
    let (repo, mut quantization) = parse_hugging_face_model(&selected);
    ui.show_status("Loading model details", &repo);
    let details = hf.details(&repo, None).await?;
    if details.gated != HuggingFaceGated::False {
        let approval = if details.gated == HuggingFaceGated::Manual {
            "Manual approval is required"
        } else {
            "Accept the access terms"
        };
        let choice = ui
            .select(
                &format!(
                    "Hugging Face access required\n{}\n\n{approval} at:\nhttps://huggingface.co/{}\n\nThe llama.cpp server needs HF_TOKEN with access.",
                    details.id, details.id
                ),
                &["Continue".to_string(), "Back".to_string()],
            )
            .await;
        if choice.as_deref() != Some("Continue") {
            return Ok(());
        }
    }
    if quantization.is_none() && !details.quantizations.is_empty() {
        let options: Vec<String> = details
            .quantizations
            .iter()
            .map(|entry| {
                let mut parts = Vec::new();
                if let Some(sz) = entry.size
                    && sz >= 0
                {
                    parts.push(format_bytes(sz as u64));
                }
                if entry.name == "Q4_K_M" {
                    parts.push("recommended".to_string());
                }
                if parts.is_empty() {
                    entry.name.clone()
                } else {
                    format!("{} · {}", entry.name, parts.join(" · "))
                }
            })
            .collect();
        let choice = ui
            .select(&format!("Select quantization\n{}", details.id), &options)
            .await;
        let Some(chosen) = choice else {
            return Ok(());
        };
        let Some(idx) = options.iter().position(|o| o == &chosen) else {
            return Ok(());
        };
        quantization = Some(details.quantizations[idx].name.clone());
    }
    let model = match quantization {
        Some(q) => format!("{}:{q}", details.id),
        None => details.id.clone(),
    };
    let client_run = client.clone();
    let model_run = model.clone();
    let client_cancel = client.clone();
    let model_cancel = model.clone();
    let outcome = crate::extensions::llama::ui::run_with_progress(
        ui,
        RunWithProgressOptions {
            title: "Downloading model".to_string(),
            model: model.clone(),
            initial_message: "Starting…".to_string(),
            cancel_title: "Stop download?".to_string(),
            cancel_message: model.clone(),
            run: Box::new(move |sig, update| {
                Box::pin(async move { client_run.download_and_wait(&model_run, update, Some(sig)).await })
            }),
            cancel: Box::new(move || {
                let c = client_cancel.clone();
                let m = model_cancel.clone();
                Box::pin(async move { c.unload(&m, None).await })
            }),
        },
    )
    .await?;
    let ProgressOutcome::Completed(downloaded_catalog) = outcome else {
        return Ok(());
    };
    sync_catalog(ctx, provider, client, Some(downloaded_catalog)).await?;
    ctx.base.ui()?.notify(&format!("Downloaded {model}"), None);
    Ok(())
}

pub fn create_llama_extension() -> ExtensionFactory {
    Arc::new(|pi: ExtensionApi| {
        Box::pin(async move {
            let provider = create_llama_provider();
            pi.register_native_provider(Arc::clone(&provider.provider))?;

            pi.register_command(
                "llama",
                RegisteredCommandOptions {
                    description: Some("Manage llama.cpp router models".to_string()),
                    get_argument_completions: None,
                    handler: Arc::new(move |_args, ctx| {
                        let provider = provider.clone();
                        Box::pin(async move {
                            if ctx.base.mode()? != ExtensionMode::Tui {
                                ctx.base
                                    .ui()?
                                    .notify("/llama is available in interactive mode", Some(NotifyType::Warning));
                                return Ok(());
                            }
                            let Some(client) = configured_client(&ctx).await? else {
                                return Ok(());
                            };
                            let ctx_ui = ctx.clone();
                            show_llama_ui(&ctx, move |ui| {
                                Box::pin(async move {
                                    let read_catalog = |c: LlamaClient| {
                                        let ui_r = Arc::clone(&ui);
                                        let ctx_r = ctx_ui.clone();
                                        let p_r = provider.clone();
                                        async move {
                                            loop {
                                                match sync_catalog(&ctx_r, &p_r, &c, None).await {
                                                    Ok(list) => return Ok::<_, pi_js::Error>(Some(list)),
                                                    Err(err) => {
                                                        if !ui_r
                                                            .connection_error(
                                                                &c.server_url,
                                                                &connection_error_message(&err),
                                                            )
                                                            .await
                                                        {
                                                            return Ok::<_, pi_js::Error>(None);
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    };

                                    let Some(mut catalog) = read_catalog(client.clone()).await? else {
                                        return Ok(());
                                    };
                                    loop {
                                        let action = ui.show_models(&client.server_url, &catalog).await;
                                        let action_err = match action {
                                            LlamaManagerAction::Close => return Ok(()),
                                            LlamaManagerAction::Download => {
                                                download_model(&ctx_ui, &provider, &ui, &client).await.err()
                                            }
                                            LlamaManagerAction::Model(model) => {
                                                if model_is_loaded(&model) {
                                                    unload_model(&ctx_ui, &provider, &ui, &client, &model).await.err()
                                                } else if model.status.value == LlamaModelStatus::Unloaded {
                                                    load_model(&ctx_ui, &provider, &ui, &client, &catalog, &model)
                                                        .await
                                                        .err()
                                                } else {
                                                    let status_label = match model.status.value {
                                                        LlamaModelStatus::Loading => "loading",
                                                        LlamaModelStatus::Downloading => "downloading",
                                                        LlamaModelStatus::Loaded
                                                        | LlamaModelStatus::Sleeping
                                                        | LlamaModelStatus::Unloaded => "busy",
                                                    };
                                                    ctx_ui.base.ui()?.notify(
                                                        &format!("{} is {status_label}", model.id),
                                                        Some(NotifyType::Warning),
                                                    );
                                                    None
                                                }
                                            }
                                        };
                                        let Some(refreshed) = read_catalog(client.clone()).await? else {
                                            return Ok(());
                                        };
                                        catalog = refreshed;
                                        if let Some(err) = action_err
                                            && !is_connection_error(&err)
                                        {
                                            ctx_ui.base.ui()?.notify(&err.message(), Some(NotifyType::Error));
                                        }
                                    }
                                })
                            })
                            .await
                        })
                    }),
                },
            )
        })
    })
}

pub fn default_export() -> ExtensionFactory {
    create_llama_extension()
}
