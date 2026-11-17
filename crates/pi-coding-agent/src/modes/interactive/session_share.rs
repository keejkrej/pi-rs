//! Port of packages/coding-agent/src/modes/interactive/session-share.ts

use std::sync::Arc;

use pi_js::{BoxFuture, Result};
use pi_tui::{Component, Container, TUI, hyperlink};
use serde_json::{Value, json};
use url::Url;

use crate::config::get_share_viewer_url;
use crate::core::radius::get_radius_gateway_url;

use super::components::bordered_loader::BorderedLoader;
use super::theme::theme::theme;

// WIRE(@tui-w3): Replace ShareSession trait with concrete crate::core::agent_session::AgentSession,
// crate::cli::auth_command::get_auth_credential, and crate::core::session_export::export_session_to_jsonl
// once @ca-cli lands on main.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SessionShareDestination {
    #[default]
    Auto,
    Radius,
    Gist,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ShareToolSummary {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

pub trait ShareSession: Send + Sync {
    fn system_prompt(&self) -> String;
    fn state_tools(&self) -> Vec<ShareToolSummary>;
    fn export_session_jsonl(
        &self,
        file_path: &str,
        trailing_entries_fn: Arc<dyn Fn(Option<&str>, &str) -> Vec<Value> + Send + Sync>,
    ) -> Result<()>;
    fn export_to_html(&self, html_path: &str, theme_name: &str) -> BoxFuture<Result<()>>;
    fn session_share_destination(&self) -> SessionShareDestination;
    fn has_radius_provider(&self) -> bool;
    fn get_radius_auth_token(&self, min_oauth_validity_ms: i64) -> BoxFuture<Option<String>>;
}

pub struct SessionShareContext {
    pub session: Arc<dyn ShareSession>,
    pub ui: Arc<dyn TUI + Send + Sync>,
    pub editor_container: Container,
    pub editor: Arc<dyn Component>,
    pub show_status: Arc<dyn Fn(&str) + Send + Sync>,
    pub show_error: Arc<dyn Fn(&str) + Send + Sync>,
}

/// Trailing `pi.share` entry carrying the system prompt and tool schemas for the session viewer.
pub fn create_share_trailing_entries(
    session: &dyn ShareSession,
    parent_id: Option<&str>,
    timestamp: &str,
) -> Vec<Value> {
    let tools: Vec<Value> = session
        .state_tools()
        .into_iter()
        .map(|tool| {
            json!({
                "name": tool.name,
                "description": tool.description,
                "parameters": tool.parameters,
            })
        })
        .collect();
    let uuid = pi_js::crypto::random_uuid();
    let short_id: String = uuid.chars().take(8).collect();
    vec![json!({
        "type": "custom",
        "customType": "pi.share",
        "id": short_id,
        "parentId": parent_id,
        "timestamp": timestamp,
        "data": {
            "systemPrompt": session.system_prompt(),
            "tools": tools,
        }
    })]
}

/// Export the current branch with presentation metadata for Radius.
pub fn export_session_for_share(file_path: &str, session: Arc<dyn ShareSession>) -> Result<()> {
    let session_c = Arc::clone(&session);
    session.export_session_jsonl(
        file_path,
        Arc::new(move |parent_id, timestamp| create_share_trailing_entries(session_c.as_ref(), parent_id, timestamp)),
    )
}

/// Share the current session through Radius, falling back to a private gist.
pub async fn share_session(context: &SessionShareContext) {
    let tmp_base = pi_js::env::tmp_dir();
    let Ok(temp_dir) = tempfile::Builder::new().prefix("pi-share-").tempdir_in(&tmp_base) else {
        (context.show_error)("Failed to export session: could not create temporary directory");
        return;
    };
    let jsonl_file = temp_dir.path().join("session.jsonl");
    let html_file = temp_dir.path().join("session.html");
    let jsonl_str = jsonl_file.to_string_lossy().into_owned();
    let html_str = html_file.to_string_lossy().into_owned();

    if let Err(err) = export_session_for_share(&jsonl_str, Arc::clone(&context.session)) {
        (context.show_error)(&format!("Failed to export session: {err}"));
        return;
    }

    let destination = context.session.session_share_destination();
    if destination != SessionShareDestination::Gist {
        if try_share_via_radius(&jsonl_str, context).await {
            return;
        }
        if destination == SessionShareDestination::Radius {
            (context.show_error)("Radius authentication is not configured. Run '/login radius' first.");
            return;
        }
    }

    let auth_status = tokio::process::Command::new("gh")
        .args(["auth", "status"])
        .env_clear()
        .envs(pi_js::env::vars())
        .output()
        .await;
    match auth_status {
        Ok(out) if out.status.success() => {}
        Ok(_) => {
            (context.show_error)("GitHub CLI is not logged in. Run 'gh auth login' first.");
            return;
        }
        Err(_) => {
            (context.show_error)("GitHub CLI (gh) is not installed. Install it from https://cli.github.com/");
            return;
        }
    }

    let theme_name = theme().name().unwrap_or("dark").to_string();
    if let Err(err) = context.session.export_to_html(&html_str, &theme_name).await {
        (context.show_error)(&format!("Failed to export session: {err}"));
        return;
    }

    share_via_gist(&html_str, context).await;
}

async fn try_share_via_radius(tmp_file: &str, context: &SessionShareContext) -> bool {
    if !context.session.has_radius_provider() {
        return false;
    }
    let Some(token) = context.session.get_radius_auth_token(5 * 60_000).await else {
        return false;
    };

    let t = theme();
    let loader = BorderedLoader::new(Arc::clone(&context.ui), &t, "Uploading to Radius...", None);
    context.editor_container.clear();
    context.editor_container.add_child(Arc::new(loader.clone()));
    context.ui.set_focus(Some(Arc::new(loader.clone())));
    context.ui.request_render(None);

    let cancel_loader = loader.clone();
    let cancel_container = context.editor_container.clone();
    let cancel_editor = Arc::clone(&context.editor);
    let cancel_ui = Arc::clone(&context.ui);
    let cancel_status = Arc::clone(&context.show_status);
    loader.set_on_abort(Some(Arc::new(move || {
        restore_editor(&cancel_loader, &cancel_container, &cancel_editor, &cancel_ui);
        cancel_status("Share cancelled");
    })));

    let body = match tokio::fs::read(tmp_file).await {
        Ok(b) => b,
        Err(err) => {
            if !loader.signal().aborted() {
                restore_editor(&loader, &context.editor_container, &context.editor, &context.ui);
                (context.show_error)(&format!("Failed to upload Radius artifact: {err}"));
            }
            return true;
        }
    };

    let Ok(mut url) = Url::parse(&get_radius_gateway_url()).and_then(|u| u.join("/v1/artifacts")) else {
        restore_editor(&loader, &context.editor_container, &context.editor, &context.ui);
        (context.show_error)("Failed to upload Radius artifact: invalid gateway URL");
        return true;
    };
    url.query_pairs_mut()
        .append_pair("visibility", "organization")
        .append_pair("title", "Pi session");

    let signal = loader.signal();
    let client = reqwest::Client::new();
    let content_len = body.len().to_string();
    let req_fut = client
        .post(url)
        .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"))
        .header(reqwest::header::CONTENT_TYPE, "application/x-ndjson")
        .header(reqwest::header::CONTENT_LENGTH, content_len)
        .body(body)
        .send();

    let response = tokio::select! {
        res = req_fut => res,
        _ = signal.cancelled() => {
            return true;
        }
    };

    if signal.aborted() {
        return true;
    }

    let resp = match response {
        Ok(r) => r,
        Err(err) => {
            restore_editor(&loader, &context.editor_container, &context.editor, &context.ui);
            (context.show_error)(&format!("Failed to upload Radius artifact: {err}"));
            return true;
        }
    };

    let status = resp.status();
    let json_val: Option<Value> = resp.text().await.ok().and_then(|s| pi_js::json::parse(&s).ok());
    if signal.aborted() {
        return true;
    }

    restore_editor(&loader, &context.editor_container, &context.editor, &context.ui);

    let canonical_url = json_val
        .as_ref()
        .and_then(|v| v.get("artifact"))
        .and_then(|a| a.get("canonical_url"))
        .and_then(|u| u.as_str());

    if !status.is_success() || canonical_url.is_none() {
        let err_detail = json_val
            .as_ref()
            .and_then(|v| v.get("error"))
            .and_then(|e| e.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| status.to_string());
        (context.show_error)(&format!("Failed to upload Radius artifact: {err_detail}"));
        return true;
    }

    if let Some(share_url) = canonical_url {
        (context.show_status)(&format!("Share URL: {}", hyperlink(share_url, share_url)));
    }
    true
}

async fn share_via_gist(tmp_file: &str, context: &SessionShareContext) {
    let t = theme();
    let loader = BorderedLoader::new(Arc::clone(&context.ui), &t, "Creating gist...", None);
    context.editor_container.clear();
    context.editor_container.add_child(Arc::new(loader.clone()));
    context.ui.set_focus(Some(Arc::new(loader.clone())));
    context.ui.request_render(None);

    let cancel_loader = loader.clone();
    let cancel_container = context.editor_container.clone();
    let cancel_editor = Arc::clone(&context.editor);
    let cancel_ui = Arc::clone(&context.ui);
    let cancel_status = Arc::clone(&context.show_status);
    loader.set_on_abort(Some(Arc::new(move || {
        restore_editor(&cancel_loader, &cancel_container, &cancel_editor, &cancel_ui);
        cancel_status("Share cancelled");
    })));

    let mut cmd = tokio::process::Command::new("gh");
    cmd.args(["gist", "create", "--public=false", tmp_file])
        .env_clear()
        .envs(pi_js::env::vars())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(err) => {
            if !loader.signal().aborted() {
                restore_editor(&loader, &context.editor_container, &context.editor, &context.ui);
                (context.show_error)(&format!("Failed to create gist: {err}"));
            }
            return;
        }
    };

    let signal = loader.signal();
    let output_res = tokio::select! {
        res = child.wait_with_output() => Some(res),
        _ = signal.cancelled() => None,
    };

    let Some(output_res) = output_res else {
        return;
    };

    if signal.aborted() {
        return;
    }
    restore_editor(&loader, &context.editor_container, &context.editor, &context.ui);

    let output = match output_res {
        Ok(o) => o,
        Err(err) => {
            (context.show_error)(&format!("Failed to create gist: {err}"));
            return;
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        (context.show_error)(&format!(
            "Failed to create gist: {}",
            if stderr.is_empty() { "Unknown error" } else { &stderr }
        ));
        return;
    }

    let gist_url = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let gist_id = gist_url.split('/').next_back().filter(|s| !s.is_empty());
    let Some(gist_id) = gist_id else {
        (context.show_error)("Failed to parse gist ID from gh output");
        return;
    };

    let preview_url = get_share_viewer_url(gist_id);
    (context.show_status)(&format!(
        "Share URL: {}\nGist: {}",
        hyperlink(&preview_url, &preview_url),
        hyperlink(&gist_url, &gist_url)
    ));
}

fn restore_editor(
    loader: &BorderedLoader,
    container: &Container,
    editor: &Arc<dyn Component>,
    ui: &Arc<dyn TUI + Send + Sync>,
) {
    loader.dispose();
    container.clear();
    container.add_child(Arc::clone(editor));
    ui.set_focus(Some(Arc::clone(editor)));
}
