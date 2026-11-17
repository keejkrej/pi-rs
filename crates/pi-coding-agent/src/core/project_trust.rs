//! Port of packages/coding-agent/src/core/project-trust.ts

use serde::{Deserialize, Serialize};

use crate::config::{APP_NAME, CONFIG_DIR_NAME};
use crate::core::settings_manager::DefaultProjectTrust;
use crate::core::trust_manager::{
    ProjectTrustOption, ProjectTrustStore, get_project_trust_options, has_trust_requiring_project_resources,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AppMode {
    Interactive,
    Print,
    Json,
    Rpc,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectTrustEvent {
    #[serde(rename = "type")]
    pub event_type: String,
    pub cwd: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProjectTrustHandlerDecision {
    Yes,
    No,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectTrustEventResult {
    pub trusted: ProjectTrustHandlerDecision,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remember: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectTrustExtensionError {
    pub extension_path: String,
    pub error: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProjectTrustEmitResult {
    pub result: Option<ProjectTrustEventResult>,
    pub errors: Vec<ProjectTrustExtensionError>,
}

// WIRE(@codemode-w3): wire to core::extensions::types::ProjectTrustContext once extensions/types.rs lands
#[async_trait::async_trait]
pub trait ProjectTrustUi: Send + Sync {
    async fn select(&self, prompt: &str, options: &[String]) -> Option<String>;
}

// WIRE(@codemode-w3): wire to core::extensions::types::ProjectTrustContext once extensions/types.rs lands
#[async_trait::async_trait]
pub trait ProjectTrustContext: Send + Sync {
    fn has_ui(&self) -> bool;
    fn ui(&self) -> &dyn ProjectTrustUi;
    fn mode(&self) -> AppMode;
}

// WIRE(@codemode-w3): wire to core::extensions::runner::emit_project_trust_event once extensions/runner.rs lands
#[async_trait::async_trait]
pub trait ProjectTrustExtensionHost: Send + Sync {
    async fn emit_project_trust(
        &self,
        event: ProjectTrustEvent,
        ctx: &dyn ProjectTrustContext,
    ) -> ProjectTrustEmitResult;
}

pub struct ResolveProjectTrustedOptions<'a> {
    pub cwd: &'a str,
    pub trust_store: &'a ProjectTrustStore,
    pub trust_override: Option<bool>,
    pub default_project_trust: Option<DefaultProjectTrust>,
    pub extension_host: Option<&'a dyn ProjectTrustExtensionHost>,
    pub project_trust_context: &'a dyn ProjectTrustContext,
    pub on_extension_error: Option<&'a (dyn Fn(&str) + Send + Sync)>,
}

fn format_project_trust_prompt(cwd: &str) -> String {
    let app_name = *APP_NAME;
    let config_dir = *CONFIG_DIR_NAME;
    format!(
        "Trust project folder?\n{cwd}\n\nThis allows {app_name} to load {config_dir} settings and resources, install missing project packages, and execute project extensions."
    )
}

async fn select_project_trust_option(cwd: &str, ctx: &dyn ProjectTrustContext) -> Option<ProjectTrustOption> {
    let options = get_project_trust_options(cwd, true);
    let labels: Vec<String> = options.iter().map(|opt| opt.label.clone()).collect();
    let selected = ctx.ui().select(&format_project_trust_prompt(cwd), &labels).await?;
    options.into_iter().find(|opt| opt.label == selected)
}

fn save_project_trust_prompt_result(trust_store: &ProjectTrustStore, result: &ProjectTrustOption) {
    if !result.updates.is_empty() {
        let _ = trust_store.set_many(&result.updates);
    }
}

pub async fn resolve_project_trusted(options: ResolveProjectTrustedOptions<'_>) -> bool {
    if let Some(ovr) = options.trust_override {
        return ovr;
    }
    if !has_trust_requiring_project_resources(options.cwd) {
        return true;
    }

    if let Some(host) = options.extension_host {
        let emitted = host
            .emit_project_trust(
                ProjectTrustEvent {
                    event_type: "project_trust".to_owned(),
                    cwd: options.cwd.to_owned(),
                },
                options.project_trust_context,
            )
            .await;
        for err in emitted.errors {
            if let Some(on_err) = options.on_extension_error {
                on_err(&format!(
                    "Extension \"{}\" project_trust error: {}",
                    err.extension_path, err.error
                ));
            }
        }
        if let Some(result) = emitted.result {
            let trusted = result.trusted == ProjectTrustHandlerDecision::Yes;
            if result.remember == Some(true) {
                let _ = options.trust_store.set(options.cwd, Some(trusted));
            }
            return trusted;
        }
    }

    if let Ok(Some(decision)) = options.trust_store.get(options.cwd) {
        return decision;
    }

    match options.default_project_trust.unwrap_or(DefaultProjectTrust::Ask) {
        DefaultProjectTrust::Always => return true,
        DefaultProjectTrust::Never => return false,
        DefaultProjectTrust::Ask => {}
    }

    if !options.project_trust_context.has_ui() {
        return false;
    }

    if let Some(selected) = select_project_trust_option(options.cwd, options.project_trust_context).await {
        save_project_trust_prompt_result(options.trust_store, &selected);
        return selected.trusted;
    }
    false
}
