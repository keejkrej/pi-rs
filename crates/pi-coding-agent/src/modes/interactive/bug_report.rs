//! Port of packages/coding-agent/src/modes/interactive/bug-report.ts

use std::sync::Arc;

use crate::core::keybindings::KeybindingsManager;
use pi_ai::providers::radius_config::DEFAULT_RADIUS_GATEWAY;
use pi_js::abort::AbortSignal;
use pi_js::{BoxFuture, Result};
use pi_tui::{Component, Container, TUI};
use serde_json::Value;
use tokio::sync::oneshot;
use url::Url;

use super::components::bordered_loader::BorderedLoader;
use super::components::extension_editor::{ExtensionEditorComponent, ExtensionEditorOptions};
use super::components::extension_selector::{ExtensionSelectorComponent, ExtensionSelectorOptions};
use super::theme::theme::theme;

// WIRE(@tui-w3): Replace BugReportSession trait and local Radius/archive helpers with concrete
// crate::core::agent_session::AgentSession, crate::core::bug_report, crate::core::bug_report_upload,
// crate::core::crash_log, crate::core::radius, and crate::core::session_export once @mcp & @ca-cli land on main.

pub const BUG_REPORT_CUSTOM_ENTRY_TYPE: &str = "pi.bug-report";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BugReportDelivery {
    Upload,
    Zip,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BugReportOptions {
    pub hint: Option<String>,
    pub include_session: bool,
    pub include_summary: bool,
    pub delivery: BugReportDelivery,
}

#[derive(Clone, Debug)]
pub struct BugReportBundleData {
    pub id: String,
    pub created_at: String,
    pub hint: Option<String>,
    pub session_included: bool,
    pub summary_included: bool,
    pub crash_count: usize,
    pub payload: Value,
}

pub trait BugReportSession: Send + Sync {
    fn model_name(&self) -> Option<String>;
    fn model_provider(&self) -> Option<String>;
    fn external_editor_command(&self) -> Option<String>;
    fn summarize_for_bug_report(&self, hint: Option<&str>, signal: AbortSignal) -> BoxFuture<Result<String>>;
    fn build_bundle(&self, options: &BugReportOptions, summary: Option<&str>) -> Result<BugReportBundleData>;
    fn upload_bundle(&self, bundle: &BugReportBundleData, signal: AbortSignal) -> BoxFuture<Result<String>>;
    fn write_archive(&self, bundle: &BugReportBundleData, archive_path: &str) -> BoxFuture<Result<()>>;
    fn archive_file_name(&self, id: &str) -> String;
    fn record_in_session(&self, bundle: &BugReportBundleData, delivery: BugReportDelivery, archive_path: Option<&str>);
    fn clear_crash_log(&self);
}

pub struct BugReportContext {
    pub session: Arc<dyn BugReportSession>,
    pub ui: Arc<dyn TUI + Send + Sync>,
    pub editor_container: Container,
    pub editor: Arc<dyn Component>,
    pub keybindings: KeybindingsManager,
    pub show_status: Arc<dyn Fn(&str) + Send + Sync>,
    pub show_error: Arc<dyn Fn(&str) + Send + Sync>,
}

const DISCLAIMER: &str = "This report goes to the Pi developers (Earendil) and is not shared publicly. It includes your pi version, operating system, the current model and provider configuration (without API keys), loaded extensions, settings, and provider error diagnostics from this session.";
const TRANSCRIPT_NOTE: &str = "The transcript contains your messages, model output, tool calls and their results, including file contents and command output read during this session.";

fn radius_gateway_host() -> String {
    let raw = pi_js::env::var("PI_RADIUS_GATEWAY")
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_RADIUS_GATEWAY.to_string());
    Url::parse(&raw)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_else(|| "radius.pi.dev".to_string())
}

/// Run the `/bug` flow: consent, optional summary, then upload or export.
pub async fn report_bug(context: &BugReportContext, initial_hint: Option<&str>) {
    let Some(options) = prompt_for_options(context, initial_hint).await else {
        (context.show_status)("Bug report cancelled");
        return;
    };

    if options.delivery == BugReportDelivery::Upload && pi_js::env::var("PI_OFFLINE").is_some_and(|v| !v.is_empty()) {
        (context.show_error)("Uploading bug reports requires online mode. Use Export as Zip instead.");
        return;
    }

    let mut summary: Option<String> = None;
    if options.include_summary {
        let model_label = context
            .session
            .model_name()
            .unwrap_or_else(|| "the current model".to_string());
        let loader = show_loader(context, &format!("Writing summary with {model_label}..."));
        let res = context
            .session
            .summarize_for_bug_report(options.hint.as_deref(), loader.signal())
            .await;
        restore_loader(context, &loader);
        match res {
            Ok(text) => {
                if loader.signal().aborted() {
                    (context.show_status)("Bug report cancelled");
                    return;
                }
                summary = Some(text);
            }
            Err(err) => {
                if loader.signal().aborted() {
                    (context.show_status)("Bug report cancelled");
                } else {
                    (context.show_error)(&format!("Failed to write bug report summary: {err}"));
                }
                return;
            }
        }
    }

    let bundle = match context.session.build_bundle(&options, summary.as_deref()) {
        Ok(b) => b,
        Err(err) => {
            (context.show_error)(&format!("Failed to build bug report: {err}"));
            return;
        }
    };

    if options.delivery == BugReportDelivery::Upload {
        let Some(failure) = upload(context, &bundle).await else {
            return;
        };
        let fallback = choose(
            context,
            "Upload failed",
            vec!["Export as Zip".to_string(), "Cancel".to_string()],
            Some(&format!("{failure}\n\nExport the report as a zip archive instead?")),
        )
        .await;
        if fallback.as_deref() != Some("Export as Zip") {
            (context.show_status)("Bug report cancelled");
            return;
        }
    }

    export_zip(context, &bundle).await;
}

async fn prompt_for_options(context: &BugReportContext, initial_hint: Option<&str>) -> Option<BugReportOptions> {
    let hint = input(
        context,
        "Report a bug",
        &format!("{DISCLAIMER}\n\nWhat went wrong? (optional)"),
        initial_hint,
    )
    .await?;

    let transcript = choose(
        context,
        "Include the session transcript?",
        vec!["Yes, include the transcript".to_string(), "No".to_string()],
        Some(TRANSCRIPT_NOTE),
    )
    .await?;

    let include_session = transcript != "No";
    let mut include_summary = false;
    if !include_session {
        let model_name = context
            .session
            .model_name()
            .unwrap_or_else(|| "the current model".to_string());
        let provider_name = context
            .session
            .model_provider()
            .unwrap_or_else(|| "your provider".to_string());
        let summary_choice = choose(
            context,
            &format!("Attach a summary written by {model_name} instead?"),
            vec!["Yes, generate a summary".to_string(), "No".to_string()],
            Some(&format!(
                "The transcript is sent to {provider_name} with your credentials and tokens. Only the generated summary is attached; the transcript stays on your machine."
            )),
        )
        .await?;
        include_summary = summary_choice != "No";
    }

    let description = hint.trim();
    let desc_display = if description.is_empty() { "none" } else { description };
    let transcript_display = if include_session { "included" } else { "not included" };
    let summary_display = if include_summary {
        let model_name = context
            .session
            .model_name()
            .unwrap_or_else(|| "the current model".to_string());
        format!("written by {model_name}")
    } else {
        "none".to_string()
    };
    let host = radius_gateway_host();
    let delivery_desc = format!(
        "Description: {desc_display}\nTranscript: {transcript_display}\nSummary: {summary_display}\n\nUpload sends the report to {host}. Export writes a zip archive to the current directory instead."
    );

    let delivery = choose(
        context,
        "Bug report",
        vec![
            "Upload Report".to_string(),
            "Export as Zip".to_string(),
            "Cancel".to_string(),
        ],
        Some(&delivery_desc),
    )
    .await?;
    if delivery == "Cancel" {
        return None;
    }

    Some(BugReportOptions {
        hint: if description.is_empty() {
            None
        } else {
            Some(description.to_string())
        },
        include_session,
        include_summary,
        delivery: if delivery == "Upload Report" {
            BugReportDelivery::Upload
        } else {
            BugReportDelivery::Zip
        },
    })
}

async fn upload(context: &BugReportContext, bundle: &BugReportBundleData) -> Option<String> {
    let loader = show_loader(context, "Uploading bug report...");
    let res = context.session.upload_bundle(bundle, loader.signal()).await;
    let aborted = loader.signal().aborted();
    restore_loader(context, &loader);
    match res {
        Ok(report_id) => {
            record_in_session(context.session.as_ref(), bundle, BugReportDelivery::Upload, None);
            (context.show_status)(&format!("Bug report uploaded. Report ID: {report_id}"));
            None
        }
        Err(err) => {
            if aborted {
                (context.show_status)("Bug report cancelled");
                None
            } else {
                Some(err.to_string())
            }
        }
    }
}

async fn export_zip(context: &BugReportContext, bundle: &BugReportBundleData) {
    let cwd = pi_js::env::cwd();
    let archive_name = context.session.archive_file_name(&bundle.id);
    let archive_path = pi_js::path::join(&[&cwd, &archive_name]);
    if let Err(err) = context.session.write_archive(bundle, &archive_path).await {
        (context.show_error)(&format!("Failed to write bug report: {err}"));
        return;
    }
    record_in_session(
        context.session.as_ref(),
        bundle,
        BugReportDelivery::Zip,
        Some(&archive_path),
    );
    (context.show_status)(&format!(
        "Bug report exported to: {archive_path}\nReport ID: {}",
        bundle.id
    ));
}

fn record_in_session(
    session: &dyn BugReportSession,
    bundle: &BugReportBundleData,
    delivery: BugReportDelivery,
    archive_path: Option<&str>,
) {
    session.record_in_session(bundle, delivery, archive_path);
    if bundle.crash_count > 0 {
        session.clear_crash_log();
    }
}

async fn input(
    context: &BugReportContext,
    title: &str,
    description: &str,
    initial_value: Option<&str>,
) -> Option<String> {
    let (tx, rx) = oneshot::channel::<Option<String>>();
    let tx_slot = Arc::new(std::sync::Mutex::new(Some(tx)));

    let tx_submit = Arc::clone(&tx_slot);
    let tx_cancel = Arc::clone(&tx_slot);
    let component = ExtensionEditorComponent::new(
        Arc::clone(&context.ui),
        context.keybindings.clone(),
        title,
        initial_value,
        Arc::new(move |value| {
            if let Some(sender) = tx_submit.lock().unwrap_or_else(|e| e.into_inner()).take() {
                let _ = sender.send(Some(value.to_string()));
            }
        }),
        Arc::new(move || {
            if let Some(sender) = tx_cancel.lock().unwrap_or_else(|e| e.into_inner()).take() {
                let _ = sender.send(None);
            }
        }),
        Some(ExtensionEditorOptions {
            description: Some(description.to_string()),
            ..Default::default()
        }),
        context.session.external_editor_command().as_deref(),
    );

    show_overlay(context, Arc::new(component));
    let result = rx.await.ok().flatten();
    restore_editor(context);
    result
}

async fn choose(
    context: &BugReportContext,
    title: &str,
    options: Vec<String>,
    description: Option<&str>,
) -> Option<String> {
    let (tx, rx) = oneshot::channel::<Option<String>>();
    let tx_slot = Arc::new(std::sync::Mutex::new(Some(tx)));

    let tx_select = Arc::clone(&tx_slot);
    let tx_cancel = Arc::clone(&tx_slot);
    let component = ExtensionSelectorComponent::new(
        title,
        options,
        Arc::new(move |value| {
            if let Some(sender) = tx_select.lock().unwrap_or_else(|e| e.into_inner()).take() {
                let _ = sender.send(Some(value.to_string()));
            }
        }),
        Arc::new(move || {
            if let Some(sender) = tx_cancel.lock().unwrap_or_else(|e| e.into_inner()).take() {
                let _ = sender.send(None);
            }
        }),
        Some(ExtensionSelectorOptions {
            tui: Some(Arc::clone(&context.ui)),
            description: description.map(str::to_string),
            ..Default::default()
        }),
    );

    show_overlay(context, Arc::new(component.clone()));
    let result = rx.await.ok().flatten();
    component.dispose();
    restore_editor(context);
    result
}

fn show_loader(context: &BugReportContext, message: &str) -> BorderedLoader {
    let t = theme();
    let loader = BorderedLoader::new(Arc::clone(&context.ui), &t, message, None);
    show_overlay(context, Arc::new(loader.clone()));
    loader
}

fn restore_loader(context: &BugReportContext, loader: &BorderedLoader) {
    loader.dispose();
    restore_editor(context);
}

fn show_overlay(context: &BugReportContext, component: Arc<dyn Component>) {
    context.editor_container.clear();
    context.editor_container.add_child(Arc::clone(&component));
    context.ui.set_focus(Some(component));
    context.ui.request_render(None);
}

fn restore_editor(context: &BugReportContext) {
    context.editor_container.clear();
    context.editor_container.add_child(Arc::clone(&context.editor));
    context.ui.set_focus(Some(Arc::clone(&context.editor)));
    context.ui.request_render(None);
}
