//! Port of packages/coding-agent/src/modes/interactive/interactive-mode.ts#L5692-L7302

use std::sync::Arc;

use indexmap::IndexSet;
use pi_ai::Model;
use pi_ai::auth::types::{AuthCheck, AuthEvent, AuthInteraction, AuthPrompt, AuthPromptKind, AuthType};
use pi_js::abort::{AbortController, AbortSignal};
use pi_js::{BoxFuture, Result};
use pi_tui::{Component, Container, Markdown, Spacer, visible_width};

use crate::modes::interactive::bug_report::{BugReportContext, report_bug};
use crate::modes::interactive::components::armin::ArminComponent;
use crate::modes::interactive::components::armin_3d_lazy::play_armin_3d;
use crate::modes::interactive::components::bash_execution::{BashExecutionComponent, TruncationResult};
use crate::modes::interactive::components::dynamic_border::DynamicBorder;
use crate::modes::interactive::components::earendil_announcement::EarendilAnnouncementComponent;
use crate::modes::interactive::components::extension_selector::{ExtensionSelectorComponent, ExtensionSelectorOptions};
use crate::modes::interactive::components::keybinding_hints::{
    KeyTextFormatOptions, format_key_text, key_display_text,
};
use crate::modes::interactive::components::login_dialog::{DeviceCodeInfo, LoginDialogComponent};
use crate::modes::interactive::components::oauth_selector::provider_brand::{RADIUS_PROVIDER_ID, compare_providers};
use crate::modes::interactive::components::oauth_selector::{
    AuthSelectorProvider, OAuthSelectorComponent, OAuthSelectorMode, format_auth_selector_provider_status,
};
use crate::modes::interactive::components::radius_login_selector::{RadiusOption, create_login_menu_selector};
use crate::modes::interactive::components::session_selector::{SessionSelectorComponent, SessionSelectorOptions};
use crate::modes::interactive::components::settings_selector::{FullscreenExitOutput, TreeFilterMode};
use crate::modes::interactive::components::status_indicator::{StatusIndicator, StatusIndicatorKind};
use crate::modes::interactive::components::themed_text::ThemedText;
use crate::modes::interactive::components::tree_selector::{FilterMode, TreeSelectorComponent};
use crate::modes::interactive::components::user_message_selector::UserMessageSelectorComponent;
use crate::modes::interactive::session_share::{SessionShareContext, share_session};
use crate::modes::interactive::theme::theme::{ThemeColor, set_registered_themes, theme};

use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangelogEntryLocal {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub content: String,
}

impl ChangelogEntryLocal {
    pub fn version_string(&self) -> String {
        format!("{}.{}.{}", self.major, self.minor, self.patch)
    }
}

fn to_tree_filter_mode(mode: TreeFilterMode) -> FilterMode {
    match mode {
        TreeFilterMode::Default => FilterMode::Default,
        TreeFilterMode::NoTools => FilterMode::NoTools,
        TreeFilterMode::UserOnly => FilterMode::UserOnly,
        TreeFilterMode::LabeledOnly => FilterMode::LabeledOnly,
        TreeFilterMode::All => FilterMode::All,
    }
}

fn format_int_commas(value: i64) -> String {
    let neg = value < 0;
    let digits = value.unsigned_abs().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 + usize::from(neg));
    if neg {
        out.push('-');
    }
    for (idx, ch) in digits.chars().enumerate() {
        let rem = digits.len() - idx;
        if idx > 0 && rem % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

pub fn find_matching_providers_local(
    providers: &[AuthSelectorProvider],
    provider_ref: &str,
) -> Vec<AuthSelectorProvider> {
    let normalized = provider_ref.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return Vec::new();
    }
    providers
        .iter()
        .filter(|p| p.id.to_ascii_lowercase() == normalized || p.name.to_ascii_lowercase() == normalized)
        .cloned()
        .collect()
}

// WIRE(@tui-w3): Replace local changelog helpers with `crate::utils::changelog::{parse_changelog, normalize_changelog_links}` once @mcp merges onto main.
pub fn parse_changelog_local(changelog_path: &str) -> Vec<ChangelogEntryLocal> {
    if !pi_js::fs::exists(changelog_path) {
        return Vec::new();
    }
    let Ok(content) = pi_js::fs::read_to_string(changelog_path) else {
        return Vec::new();
    };
    let mut entries = Vec::new();
    let mut current_lines: Vec<String> = Vec::new();
    let mut current_version: Option<(u64, u64, u64)> = None;

    for line in content.split('\n') {
        if line.starts_with("## ") {
            if let Some((major, minor, patch)) = current_version
                && !current_lines.is_empty()
            {
                entries.push(ChangelogEntryLocal {
                    major,
                    minor,
                    patch,
                    content: current_lines.join("\n").trim().to_string(),
                });
            }
            if let Some(ver) = parse_changelog_header_version(line) {
                current_version = Some(ver);
                current_lines = vec![line.to_string()];
            } else {
                current_version = None;
                current_lines.clear();
            }
        } else if current_version.is_some() {
            current_lines.push(line.to_string());
        }
    }

    if let Some((major, minor, patch)) = current_version
        && !current_lines.is_empty()
    {
        entries.push(ChangelogEntryLocal {
            major,
            minor,
            patch,
            content: current_lines.join("\n").trim().to_string(),
        });
    }
    entries
}

fn parse_changelog_header_version(line: &str) -> Option<(u64, u64, u64)> {
    let after = line.strip_prefix("## ")?.trim_start();
    let inner = after.strip_prefix('[').unwrap_or(after);
    let mut parts = inner.split(|c: char| !c.is_ascii_digit());
    let major = parts.next().filter(|s| !s.is_empty())?.parse::<u64>().ok()?;
    let minor = parts.next().filter(|s| !s.is_empty())?.parse::<u64>().ok()?;
    let patch = parts.next().filter(|s| !s.is_empty())?.parse::<u64>().ok()?;
    Some((major, minor, patch))
}

pub fn normalize_changelog_links_local(markdown: &str, version: &str) -> String {
    const GITHUB_REPO: &str = "earendil-works/pi";
    const BASE_PATH: &str = "packages/coding-agent";
    let tag = if version.starts_with('v') {
        version.to_string()
    } else {
        format!("v{version}")
    };
    let repo_url = format!("https://github.com/{GITHUB_REPO}");
    let mut out = String::with_capacity(markdown.len());
    let bytes = markdown.as_bytes();
    let mut i = 0usize;

    while i < bytes.len() {
        if bytes[i] == b'['
            && let Some(close_bracket_rel) = markdown[i + 1..].find(']')
            && !markdown[i + 1..i + 1 + close_bracket_rel].contains('\n')
        {
            let close_bracket = i + 1 + close_bracket_rel;
            if markdown[close_bracket + 1..].starts_with('(')
                && let Some(close_paren_rel) = markdown[close_bracket + 2..].find(')')
            {
                let close_paren = close_bracket + 2 + close_paren_rel;
                let inner = &markdown[close_bracket + 2..close_paren];
                let split_ws = inner.find(char::is_whitespace).unwrap_or(inner.len());
                let raw_target = &inner[..split_ws];
                let rest = &inner[split_ws..];
                if !raw_target.is_empty() {
                    let mut target = normalize_legacy_repo_url(raw_target, &repo_url);
                    for route in ["blob", "tree"] {
                        for branch in ["main", "master"] {
                            let prefix = format!("{repo_url}/{route}/{branch}/");
                            if let Some(tail) = target.strip_prefix(&prefix) {
                                target = format!("{repo_url}/{route}/{tag}/{tail}");
                            }
                        }
                    }
                    if !target.starts_with('#') && !target.starts_with("//") && !has_url_scheme(&target) {
                        target = normalize_local_changelog_target(&target, &repo_url, &tag, BASE_PATH);
                    }
                    out.push_str(&markdown[i..close_bracket + 2]);
                    out.push_str(&target);
                    out.push_str(rest);
                    out.push(')');
                    i = close_paren + 1;
                    continue;
                }
            }
        }
        let Some(ch) = markdown[i..].chars().next() else {
            break;
        };
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn normalize_legacy_repo_url(target: &str, repo_url: &str) -> String {
    for legacy in [
        "https://github.com/badlogic/pi-mono",
        "https://github.com/earendil-works/pi-mono",
    ] {
        if let Some(after) = target.strip_prefix(legacy)
            && (after.is_empty() || after.starts_with('/'))
        {
            return format!("{repo_url}{after}");
        }
    }
    target.to_string()
}

fn has_url_scheme(target: &str) -> bool {
    let Some(colon) = target.find(':') else {
        return false;
    };
    let scheme = &target[..colon];
    !scheme.is_empty()
        && scheme.bytes().enumerate().all(|(i, b)| {
            if i == 0 {
                b.is_ascii_alphabetic()
            } else {
                b.is_ascii_alphanumeric() || matches!(b, b'+' | b'.' | b'-')
            }
        })
}

fn normalize_local_changelog_target(target: &str, repo_url: &str, tag: &str, base_path: &str) -> String {
    let (before_hash, fragment) = match target.find('#') {
        Some(idx) => (&target[..idx], &target[idx..]),
        None => (target, ""),
    };
    let (path_part, query) = match before_hash.find('?') {
        Some(idx) => (&before_hash[..idx], &before_hash[idx..]),
        None => (before_hash, ""),
    };
    if path_part.is_empty() {
        return target.to_string();
    }
    let norm = path_part.replace('\\', "/");
    let joined = if norm.starts_with('/') {
        pi_js::path::posix::normalize(norm.trim_start_matches('/'))
    } else {
        pi_js::path::posix::normalize(&pi_js::path::posix::join(&[base_path, &norm]))
    };
    if joined == "." || joined == ".." || joined.starts_with("../") {
        return target.to_string();
    }
    let basename = pi_js::path::posix::basename(&joined, None);
    let route = if path_part.ends_with('/') || !basename.contains('.') {
        "tree"
    } else {
        "blob"
    };
    let encoded = percent_encoding::utf8_percent_encode(&joined, percent_encoding::CONTROLS).to_string();
    format!("{repo_url}/{route}/{tag}/{encoded}{query}{fragment}")
}

fn has_default_model_provider(provider_id: &str) -> bool {
    default_model_for_provider(provider_id).is_some()
}

fn compare_selector_providers(a: &AuthSelectorProvider, b: &AuthSelectorProvider) -> std::cmp::Ordering {
    compare_providers(&a.id, &a.name, &b.id, &b.name)
}

#[derive(Clone)]
struct LoginProviderMeta {
    option: AuthSelectorProvider,
    oauth_login_label: Option<String>,
    method_name: Option<String>,
    api_key_has_login: bool,
}

struct DialogAuthInteraction {
    mode: InteractiveMode,
    dialog: LoginDialogComponent,
    provider_id: String,
    signal: AbortSignal,
}

#[async_trait::async_trait]
impl AuthInteraction for DialogAuthInteraction {
    fn signal(&self) -> Option<&AbortSignal> {
        Some(&self.signal)
    }

    async fn prompt(&self, prompt: AuthPrompt) -> Result<String> {
        self.mode
            .show_auth_prompt(&self.dialog, prompt, &self.provider_id)
            .await
    }

    fn notify(&self, event: AuthEvent) {
        self.mode.notify_auth_dialog(&self.dialog, event);
    }
}

impl InteractiveMode {
    pub(crate) fn show_user_message_selector(&self) {
        let user_messages = self.session().get_user_messages_for_for_king_or_empty();
        if user_messages.is_empty() {
            self.show_status("No messages to fork from");
            return;
        }
        let initial_selected_id = user_messages.last().map(|m| m.id.clone());
        let m = self.clone();
        self.show_selector(Box::new(move |done| {
            let m_sel = m.clone();
            let done_sel = Arc::clone(&done);
            let m_can = m.clone();
            let selector = UserMessageSelectorComponent::new(
                user_messages,
                Arc::new(move |entry_id| {
                    let id = entry_id.to_string();
                    done_sel();
                    let m2 = m_sel.clone();
                    tokio::spawn(async move {
                        match m2.inner.runtime_host.fork(&id, None).await {
                            Ok(result) => {
                                if result.cancelled {
                                    m2.ui().request_render(None);
                                    return;
                                }
                                m2.active_editor()
                                    .set_text(result.selected_text.as_deref().unwrap_or(""));
                                m2.show_status("Forked to new session");
                            }
                            Err(err) => m2.show_error(&err.to_string()),
                        }
                    });
                }),
                Arc::new(move || {
                    done();
                    m_can.ui().request_render(None);
                }),
                initial_selected_id.as_deref(),
            );
            let focus: Arc<dyn Component> = Arc::new(selector.get_message_list().clone());
            (Arc::new(selector), focus, None)
        }));
    }

    pub(crate) async fn handle_clone_command(&self) {
        let Some(leaf_id) = self.session_manager().get_leaf_id() else {
            self.show_status("Nothing to clone yet");
            return;
        };
        match self
            .inner
            .runtime_host
            .fork(&leaf_id, Some(RuntimeForkOptions { at_leaf: true }))
            .await
        {
            Ok(result) => {
                if result.cancelled {
                    self.ui().request_render(None);
                    return;
                }
                self.active_editor().set_text("");
                self.show_status("Cloned to new session");
            }
            Err(err) => self.show_error(&err.to_string()),
        }
    }

    pub(crate) fn show_tree_selector(&self, initial_selected_id: Option<&str>) {
        let tree = self.session_manager().get_tree();
        if tree.is_empty() {
            self.show_status("No entries in session");
            return;
        }
        let real_leaf_id = self.session_manager().get_leaf_id();
        let initial_filter = to_tree_filter_mode(self.settings_manager().get_tree_filter_mode());
        let rows = self.ui().terminal().rows() as usize;
        let initial_sel = initial_selected_id.map(str::to_string);
        let m = self.clone();

        self.show_selector(Box::new(move |done| {
            let m_sel = m.clone();
            let done_sel = Arc::clone(&done);
            let m_can = m.clone();
            let m_lbl = m.clone();
            let m_copy = m.clone();
            let selector = TreeSelectorComponent::new(
                tree,
                real_leaf_id,
                rows,
                Arc::new(move |entry_id_str| {
                    let entry_id = entry_id_str.to_string();
                    let m2 = m_sel.clone();
                    let d2 = Arc::clone(&done_sel);
                    tokio::spawn(async move {
                        if m2.session_manager().get_leaf_id().as_deref() == Some(entry_id.as_str()) {
                            d2();
                            m2.show_status("Already at this point");
                            return;
                        }
                        d2();

                        let mut wants_summary = false;
                        let mut custom_instructions: Option<String> = None;

                        if !m2.settings_manager().get_branch_summary_skip_prompt() {
                            loop {
                                let choice = m2
                                    .show_extension_selector(
                                        "Summarize branch?",
                                        vec![
                                            "No summary".to_string(),
                                            "Summarize".to_string(),
                                            "Summarize with custom prompt".to_string(),
                                        ],
                                        None,
                                        None,
                                    )
                                    .await;
                                let Some(choice) = choice else {
                                    m2.show_tree_selector(Some(&entry_id));
                                    return;
                                };
                                wants_summary = choice != "No summary";
                                if choice == "Summarize with custom prompt" {
                                    custom_instructions =
                                        m2.show_extension_editor("Custom summarization instructions", None).await;
                                    if custom_instructions.is_none() {
                                        continue;
                                    }
                                }
                                break;
                            }
                        }

                        if m2.session().is_streaming() {
                            m2.restore_queued_messages_to_editor(false);
                            m2.session().abort().await;
                        }
                        if m2.session().is_compacting() {
                            m2.show_error(
                                "Wait for the current compaction or tree navigation to finish before navigating the session tree.",
                            );
                            return;
                        }

                        let mut showing_indicator = false;
                        let original_on_escape = m2
                            .inner
                            .state
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .default_editor_on_escape
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .clone();

                        if wants_summary {
                            let sess_for_abort = m2.session();
                            m2.set_default_editor_escape(Some(Arc::new(move || sess_for_abort.abort_branch_summary())));
                            m2.add_chat_component(Arc::new(Spacer::new(Some(1))), None, None, None);
                            m2.show_status_indicator(StatusIndicator::branch_summary(m2.ui()));
                            showing_indicator = true;
                            m2.ui().request_render(None);
                        }

                        let nav_res = m2
                            .session()
                            .navigate_tree(
                                &entry_id,
                                NavigateTreeOptions {
                                    summarize: wants_summary,
                                    custom_instructions,
                                    replace_instructions: None,
                                    label: None,
                                },
                            )
                            .await;

                        if showing_indicator {
                            m2.clear_status_indicator(Some(StatusIndicatorKind::BranchSummary));
                        }
                        m2.set_default_editor_escape(original_on_escape);

                        match nav_res {
                            Ok(result) => {
                                if result.aborted {
                                    m2.show_status("Branch summarization cancelled");
                                    m2.show_tree_selector(Some(&entry_id));
                                    return;
                                }
                                if result.cancelled {
                                    m2.show_status("Navigation cancelled");
                                    return;
                                }
                                m2.clear_chat_container();
                                m2.render_initial_messages();
                                if let Some(ed_text) = result.editor_text
                                    && m2.active_editor().get_text().trim().is_empty()
                                {
                                    m2.active_editor().set_text(&ed_text);
                                }
                                m2.show_status("Navigated to selected point");
                                m2.flush_compaction_queue(false).await;
                            }
                            Err(err) => {
                                m2.show_error(&err.to_string());
                            }
                        }
                    });
                }),
                Arc::new(move || {
                    done();
                    m_can.ui().request_render(None);
                }),
                Some(Arc::new(move |entry_id, label| {
                    m_lbl.session_manager().append_label_change(entry_id, label);
                    m_lbl.ui().request_render(None);
                })),
                initial_sel,
                Some(initial_filter),
            );
            selector.set_on_copy(Some(Arc::new(move |text_opt| {
                let Some(text) = text_opt.filter(|s| !s.is_empty()).map(str::to_string) else {
                    m_copy.show_error("Selected entry has no text to copy");
                    return;
                };
                let m2 = m_copy.clone();
                tokio::spawn(async move {
                    if let Err(err) = m2.copy_text_to_clipboard(&text).await {
                        m2.show_error(&err.to_string());
                    } else {
                        m2.show_status("Copied selected message to clipboard");
                    }
                });
            })));
            let comp: Arc<dyn Component> = Arc::new(selector);
            (Arc::clone(&comp), comp, None)
        }));
    }

    pub(crate) fn show_session_selector(&self) {
        let m = self.clone();
        self.show_selector(Box::new(move |done| {
            let sm_cur = m.session_manager();
            let sm_all = m.session_manager();
            let sm_ren = m.session_manager();
            let m_sel = m.clone();
            let done_sel = Arc::clone(&done);
            let m_can = m.clone();
            let m_exit = m.clone();
            let m_rnd = m.clone();

            let selector = SessionSelectorComponent::new(
                Arc::new(move |on_progress, signal| sm_cur.list_current_sessions(on_progress, signal)),
                Arc::new(move |on_progress, signal| sm_all.list_all_sessions(on_progress, signal)),
                Arc::new(move |session_path| {
                    let path = session_path.to_string();
                    done_sel();
                    let m2 = m_sel.clone();
                    tokio::spawn(async move {
                        let _ = m2.handle_resume_session(&path).await;
                    });
                }),
                Arc::new(move || {
                    done();
                    m_can.ui().request_render(None);
                }),
                Arc::new(move || {
                    let m2 = m_exit.clone();
                    tokio::spawn(async move { m2.shutdown(false).await });
                }),
                Arc::new(move || m_rnd.ui().request_render(None)),
                SessionSelectorOptions {
                    rename_session: Some(Arc::new(move |session_file, next_name| {
                        let sm = Arc::clone(&sm_ren);
                        Box::pin(async move {
                            let trimmed = next_name.unwrap_or_default().trim().to_string();
                            if trimmed.is_empty() {
                                return Ok(());
                            }
                            sm.rename_session(&session_file, Some(&trimmed)).await
                        })
                    })),
                    show_rename_hint: Some(true),
                    keybindings: Some(Arc::clone(&m.inner.keybindings)),
                },
                m.session_manager().get_session_file().as_deref(),
            );
            let comp: Arc<dyn Component> = Arc::new(selector);
            (Arc::clone(&comp), comp, None)
        }));
    }

    pub(crate) async fn handle_resume_session(&self, session_path: &str) -> CancelledResult {
        self.clear_status_indicator(None);
        match self.inner.runtime_host.switch_session(session_path, None).await {
            Ok(res) => {
                if !res.cancelled {
                    self.show_status("Resumed session");
                }
                res
            }
            Err(err) => {
                // WIRE(@tui-w3): Special-case typed MissingSessionCwdError -> prompt_for_missing_session_cwd once @mcp merges onto main.
                self.handle_fatal_runtime_error("Failed to resume session", &err.to_string())
                    .await;
                CancelledResult { cancelled: true }
            }
        }
    }

    fn build_login_provider_entries(&self, auth_type: Option<AuthType>) -> Vec<LoginProviderMeta> {
        let runtime = self.session().model_runtime();
        let mut entries: Vec<LoginProviderMeta> = Vec::new();
        for provider in runtime.get_providers() {
            let auth_status = runtime.get_provider_auth_status(&provider.id);
            let status = if auth_status.configured {
                Some(AuthCheck {
                    auth_type: if runtime.is_using_oauth(&provider.id) {
                        AuthType::OAuth
                    } else {
                        AuthType::ApiKey
                    },
                    source: auth_status.label.or(auth_status.source),
                })
            } else {
                None
            };
            let subscription = Some(provider.oauth_is_subscription);
            if auth_type.is_none_or(|t| t == AuthType::OAuth) && provider.has_oauth {
                entries.push(LoginProviderMeta {
                    option: AuthSelectorProvider {
                        id: provider.id.clone(),
                        name: provider.name.clone(),
                        auth_type: AuthType::OAuth,
                        method: None,
                        status: status.clone(),
                        subscription,
                    },
                    oauth_login_label: provider.oauth_login_label.clone(),
                    method_name: provider.oauth_method_name.clone(),
                    api_key_has_login: false,
                });
            }
            if auth_type.is_none_or(|t| t == AuthType::ApiKey) && provider.has_api_key {
                entries.push(LoginProviderMeta {
                    option: AuthSelectorProvider {
                        id: provider.id.clone(),
                        name: provider.name.clone(),
                        auth_type: AuthType::ApiKey,
                        method: None,
                        status,
                        subscription,
                    },
                    oauth_login_label: None,
                    method_name: provider.api_key_method_name.clone(),
                    api_key_has_login: provider.api_key_has_login,
                });
            }
        }
        entries.sort_by(|a, b| compare_selector_providers(&a.option, &b.option));
        entries
    }

    pub fn get_login_provider_options(&self, auth_type: Option<AuthType>) -> Vec<AuthSelectorProvider> {
        self.build_login_provider_entries(auth_type)
            .into_iter()
            .map(|e| e.option)
            .collect()
    }

    pub(crate) async fn get_logout_provider_options(&self) -> Result<Vec<AuthSelectorProvider>> {
        let runtime = self.session().model_runtime();
        let providers = runtime.get_providers();
        let signal = AbortSignal::timeout(15_000);
        let stored = runtime.list_credentials(signal).await?;
        let mut options: Vec<AuthSelectorProvider> = stored
            .into_iter()
            .map(|cred| {
                let found = providers.iter().find(|p| p.id == cred.provider_id);
                AuthSelectorProvider {
                    id: cred.provider_id.clone(),
                    name: found.map(|p| p.name.clone()).unwrap_or(cred.provider_id),
                    auth_type: cred.auth_type,
                    method: None,
                    status: Some(AuthCheck {
                        auth_type: cred.auth_type,
                        source: Some("stored credential".to_string()),
                    }),
                    subscription: found.map(|p| p.oauth_is_subscription),
                }
            })
            .collect();
        options.sort_by(compare_selector_providers);
        Ok(options)
    }

    pub(crate) async fn handle_login_command(&self, provider_ref: Option<&str>) {
        let Some(ref_str) = provider_ref.filter(|s| !s.trim().is_empty()) else {
            self.show_login_auth_type_selector(None);
            return;
        };
        let all_entries = self.build_login_provider_entries(None);
        let norm = ref_str.trim().to_ascii_lowercase();
        let matching: Vec<LoginProviderMeta> = all_entries
            .into_iter()
            .filter(|e| e.option.id.to_ascii_lowercase() == norm || e.option.name.to_ascii_lowercase() == norm)
            .collect();
        if matching.len() == 1 {
            if let Some(entry) = matching.into_iter().next() {
                self.start_provider_login(&entry, None).await;
            }
            return;
        }
        if matching.len() > 1 {
            let ids: IndexSet<String> = matching.iter().map(|e| e.option.id.clone()).collect();
            if ids.len() == 1 {
                self.show_login_auth_type_selector(Some(matching));
                return;
            }
        }
        self.show_login_provider_selector(None, Some(ref_str));
    }

    fn start_provider_login_async(&self, entry: LoginProviderMeta, on_back: Option<Arc<dyn Fn() + Send + Sync>>) {
        let m = self.clone();
        tokio::spawn(async move {
            m.start_provider_login(&entry, on_back).await;
        });
    }

    async fn start_provider_login(&self, entry: &LoginProviderMeta, on_back: Option<Arc<dyn Fn() + Send + Sync>>) {
        if entry.option.auth_type == AuthType::OAuth {
            self.show_login_dialog(&entry.option.id, &entry.option.name, on_back)
                .await;
        } else if entry.api_key_has_login {
            self.show_api_key_login_dialog(&entry.option.id, &entry.option.name, on_back)
                .await;
        } else {
            self.show_ambient_auth_dialog(entry, on_back);
        }
    }

    fn show_login_auth_type_selector(&self, entries_override: Option<Vec<LoginProviderMeta>>) {
        let radius_entry = if entries_override.is_none() {
            self.build_login_provider_entries(Some(AuthType::OAuth))
                .into_iter()
                .find(|e| e.option.id == RADIUS_PROVIDER_ID)
        } else {
            None
        };
        let radius_text = radius_entry.as_ref().map(|r| format!("Sign in with {}", r.option.name));
        let radius_label = radius_entry
            .as_ref()
            .zip(radius_text.as_ref())
            .map(|(r, text)| format!("{text}{}", format_auth_selector_provider_status(&r.option)));

        let oauth_label = entries_override
            .as_ref()
            .and_then(|list| list.iter().find(|e| e.option.auth_type == AuthType::OAuth))
            .and_then(|e| e.oauth_login_label.clone());
        let subscription_label = oauth_label.unwrap_or_else(|| "Sign in with an account".to_string());
        let api_key_label = "Sign in with an API key".to_string();

        let available_types: IndexSet<AuthType> = match &entries_override {
            Some(list) => list.iter().map(|e| e.option.auth_type).collect(),
            None => [AuthType::OAuth, AuthType::ApiKey].into_iter().collect(),
        };
        let mut options: Vec<String> = Vec::new();
        if available_types.contains(&AuthType::OAuth) {
            options.push(subscription_label.clone());
        }
        if available_types.contains(&AuthType::ApiKey) {
            options.push(api_key_label.clone());
        }
        if let Some(ref rl) = radius_label {
            options.push(rl.clone());
        }
        if options.is_empty() {
            self.show_status("No login methods available.");
            return;
        }
        if let Some(ref list) = entries_override
            && options.len() == 1
            && let Some(first) = list.first().cloned()
        {
            self.start_provider_login_async(first, None);
            return;
        }

        let title = match entries_override.as_ref().and_then(|l| l.first()) {
            Some(first) => format!("Select authentication method for {}:", first.option.name),
            None => "Select authentication method:".to_string(),
        };

        let m = self.clone();
        self.show_selector(Box::new(move |done| {
            let m_sel = m.clone();
            let done_sel = Arc::clone(&done);
            let m_can = m.clone();
            let entries_sel = entries_override.clone();
            let radius_opt_sel = radius_entry.clone();
            let radius_lbl_sel = radius_label.clone();
            let sub_lbl_sel = subscription_label.clone();

            let on_select: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |option| {
                done_sel();
                if let (Some(r_entry), Some(r_lbl)) = (&radius_opt_sel, &radius_lbl_sel)
                    && option == r_lbl
                {
                    let m_back = m_sel.clone();
                    m_sel.start_provider_login_async(
                        r_entry.clone(),
                        Some(Arc::new(move || m_back.show_login_auth_type_selector(None))),
                    );
                    return;
                }
                let chosen_type = if option == sub_lbl_sel {
                    AuthType::OAuth
                } else {
                    AuthType::ApiKey
                };
                if let Some(ref list) = entries_sel {
                    if let Some(found) = list.iter().find(|e| e.option.auth_type == chosen_type).cloned() {
                        let list_back = list.clone();
                        let m_back = m_sel.clone();
                        m_sel.start_provider_login_async(
                            found,
                            Some(Arc::new(move || {
                                m_back.show_login_auth_type_selector(Some(list_back.clone()))
                            })),
                        );
                    }
                    return;
                }
                m_sel.show_login_provider_selector(Some(chosen_type), None);
            });
            let on_cancel: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
                done();
                m_can.ui().request_render(None);
            });

            if let (Some(r_lbl), Some(r_txt)) = (radius_label.clone(), radius_text.clone()) {
                let selector = create_login_menu_selector(
                    m.ui(),
                    &title,
                    options,
                    RadiusOption {
                        label: r_lbl,
                        text: r_txt,
                    },
                    on_select,
                    on_cancel,
                );
                let comp: Arc<dyn Component> = Arc::new(selector.clone());
                let dispose: Arc<dyn Fn() + Send + Sync> = Arc::new(move || selector.dispose());
                (Arc::clone(&comp), comp, Some(dispose))
            } else {
                let selector = ExtensionSelectorComponent::new(&title, options, on_select, on_cancel, None);
                let comp: Arc<dyn Component> = Arc::new(selector.clone());
                let dispose: Arc<dyn Fn() + Send + Sync> = Arc::new(move || selector.dispose());
                (Arc::clone(&comp), comp, Some(dispose))
            }
        }));
    }

    fn show_login_provider_selector(&self, auth_type: Option<AuthType>, initial_search: Option<&str>) {
        let entries = self.build_login_provider_entries(auth_type);
        if entries.is_empty() {
            let msg = match auth_type {
                Some(AuthType::OAuth) => "No account providers available.",
                Some(AuthType::ApiKey) => "No API key providers available.",
                None => "No login providers available.",
            };
            self.show_status(msg);
            return;
        }
        let provider_options: Vec<AuthSelectorProvider> = entries.iter().map(|e| e.option.clone()).collect();
        let initial_owned = initial_search.map(str::to_string);
        let m = self.clone();

        self.show_selector(Box::new(move |done| {
            let m_sel = m.clone();
            let done_sel = Arc::clone(&done);
            let m_can = m.clone();
            let entries_sel = entries.clone();
            let init_back = initial_owned.clone();

            let selector = OAuthSelectorComponent::new(
                OAuthSelectorMode::Login,
                provider_options,
                Arc::new(move |provider_id, selected_auth_type| {
                    done_sel();
                    if let Some(entry) = entries_sel
                        .iter()
                        .find(|e| e.option.id == provider_id && e.option.auth_type == selected_auth_type)
                        .cloned()
                    {
                        let m_back = m_sel.clone();
                        let back_init = init_back.clone();
                        m_sel.start_provider_login_async(
                            entry,
                            Some(Arc::new(move || {
                                m_back.show_login_provider_selector(auth_type, back_init.as_deref());
                            })),
                        );
                    }
                }),
                Arc::new(move || {
                    done();
                    if auth_type.is_some() {
                        m_can.show_login_auth_type_selector(None);
                    } else {
                        m_can.ui().request_render(None);
                    }
                }),
                initial_owned.as_deref(),
                Some(m.ui()),
            );
            let comp: Arc<dyn Component> = Arc::new(selector.clone());
            let dispose: Arc<dyn Fn() + Send + Sync> = Arc::new(move || selector.dispose());
            (Arc::clone(&comp), comp, Some(dispose))
        }));
    }

    pub(crate) async fn show_oauth_selector(&self, mode: OAuthSelectorMode) {
        if mode == OAuthSelectorMode::Login {
            self.show_login_auth_type_selector(None);
            return;
        }
        let provider_options = match self.get_logout_provider_options().await {
            Ok(opts) => opts,
            Err(err) => {
                self.show_error(&format!("Could not read stored credentials: {err}"));
                return;
            }
        };
        if provider_options.is_empty() {
            self.show_status(
                "No stored credentials to remove. /logout only removes credentials saved by /login; environment variables and models.json config are unchanged.",
            );
            return;
        }

        let m = self.clone();
        self.show_selector(Box::new(move |done| {
            let m_sel = m.clone();
            let done_sel = Arc::clone(&done);
            let m_can = m.clone();
            let opts_sel = provider_options.clone();

            let selector = OAuthSelectorComponent::new(
                OAuthSelectorMode::Logout,
                provider_options,
                Arc::new(move |provider_id, _| {
                    let pid = provider_id.to_string();
                    done_sel();
                    let Some(option) = opts_sel.iter().find(|p| p.id == pid).cloned() else {
                        return;
                    };
                    let m2 = m_sel.clone();
                    tokio::spawn(async move {
                        let signal = AbortSignal::timeout(15_000);
                        match m2.session().model_runtime().logout(&option.id, signal).await {
                            Ok(()) => {
                                m2.update_available_provider_count();
                                let msg = if option.auth_type == AuthType::OAuth {
                                    format!("Logged out of {}", option.name)
                                } else {
                                    format!(
                                        "Removed stored API key for {}. Environment variables and models.json config are unchanged.",
                                        option.name
                                    )
                                };
                                m2.show_status(&msg);
                            }
                            Err(err) => {
                                m2.show_error(&format!("Logout failed: {err}"));
                            }
                        }
                    });
                }),
                Arc::new(move || {
                    done();
                    m_can.ui().request_render(None);
                }),
                None,
                Some(m.ui()),
            );
            let comp: Arc<dyn Component> = Arc::new(selector.clone());
            let dispose: Arc<dyn Fn() + Send + Sync> = Arc::new(move || selector.dispose());
            (Arc::clone(&comp), comp, Some(dispose))
        }));
    }

    async fn complete_provider_authentication(
        &self,
        provider_id: &str,
        provider_name: &str,
        auth_type: AuthType,
        previous_model: Option<Model>,
    ) {
        let action_label = if auth_type == AuthType::OAuth {
            format!("Logged in to {provider_name}")
        } else {
            format!("Saved API key for {provider_name}")
        };
        let sess = self.session();
        let default_model_id = default_model_for_provider(provider_id);
        let defer_selection = is_unknown_model(previous_model.as_ref())
            && has_default_model_provider(provider_id)
            && !sess
                .model_runtime()
                .get_available_snapshot()
                .iter()
                .any(|m| m.provider == provider_id && Some(m.id.as_str()) == default_model_id);

        let finish_authentication = {
            let m = self.clone();
            let pid = provider_id.to_string();
            let act = action_label.clone();
            let prev = previous_model.clone();
            move || {
                let m = m.clone();
                let pid = pid.clone();
                let act = act.clone();
                let prev = prev.clone();
                async move {
                    let mut selected_model: Option<Model> = None;
                    let mut selection_error: Option<String> = None;

                    if is_unknown_model(prev.as_ref()) {
                        let available = m.session().model_runtime().get_available_snapshot();
                        let provider_models: Vec<Model> = available.into_iter().filter(|m| m.provider == pid).collect();
                        if pid == "llama.cpp" {
                            selection_error = Some(llama_cpp_post_login_guidance(&act, provider_models.len()));
                        } else if !has_default_model_provider(&pid) {
                            selection_error = Some(format!(
                                "{act}, but no default model is configured for provider \"{pid}\". Use /model to select a model."
                            ));
                        } else if provider_models.is_empty() {
                            selection_error = Some(format!(
                                "{act}, but no models are available for that provider. Use /model to select a model."
                            ));
                        } else {
                            let def_id = default_model_for_provider(&pid);
                            let picked = provider_models
                                .iter()
                                .find(|m| Some(m.id.as_str()) == def_id)
                                .cloned()
                                .or_else(|| {
                                    if pid == "radius" {
                                        provider_models.first().cloned()
                                    } else {
                                        None
                                    }
                                });
                            if let Some(model_choice) = picked {
                                match m.session().set_model(model_choice.clone(), true).await {
                                    Ok(()) => selected_model = Some(model_choice),
                                    Err(err) => {
                                        selection_error = Some(format!(
                                            "{act}, but selecting its default model failed: {err}. Use /model to select a model."
                                        ));
                                    }
                                }
                            } else {
                                selection_error = Some(format!(
                                    "{act}, but its default model \"{}\" is not available. Use /model to select a model.",
                                    def_id.unwrap_or("")
                                ));
                            }
                        }
                    }

                    m.update_available_provider_count();
                    m.inner.footer.invalidate();
                    m.update_editor_border_color();
                    let auth_path = get_auth_path_local();
                    if let Some(ref sel_m) = selected_model {
                        m.show_status(&format!(
                            "{act}. Selected {}. Credentials saved to {auth_path}",
                            sel_m.id
                        ));
                        m.maybe_warn_about_anthropic_subscription_auth(Some(sel_m)).await;
                    } else {
                        m.show_status(&format!("{act}. Credentials saved to {auth_path}"));
                        if let Some(err) = selection_error {
                            m.show_error(&err);
                        } else {
                            m.maybe_warn_about_anthropic_subscription_auth(None).await;
                        }
                    }
                }
            }
        };

        if defer_selection {
            self.show_status(&format!(
                "{action_label}. Credentials saved to {}. Refreshing model catalog…",
                get_auth_path_local()
            ));
        } else {
            finish_authentication().await;
        }

        let controller = Arc::new(AbortController::new());
        let ctrl_to = Arc::clone(&controller);
        let timeout = pi_js::time::set_timeout(15_000, move || {
            ctrl_to.abort(None);
        });
        let signal = controller.signal();
        let res = sess
            .model_runtime()
            .refresh_providers(vec![provider_id.to_string()], signal)
            .await;
        timeout.clear();

        match res {
            Ok(refresh_res) => {
                if refresh_res.aborted {
                    self.show_warning(&format!(
                        "{action_label}, but its model catalog refresh timed out; using cached models."
                    ));
                } else if !refresh_res.errors.is_empty() {
                    self.show_warning(&format!(
                        "{action_label}, but its model catalog could not be refreshed; using cached models."
                    ));
                }
                if defer_selection && Arc::ptr_eq(&self.session(), &sess) && self.session().model() == previous_model {
                    finish_authentication().await;
                }
                self.update_available_provider_count();
                self.inner.footer.invalidate();
                self.ui().request_render(None);
            }
            Err(err) => {
                self.show_warning(&format!(
                    "{action_label}, but its model catalog could not be refreshed: {err}"
                ));
            }
        }
    }

    fn show_ambient_auth_dialog(&self, entry: &LoginProviderMeta, on_back: Option<Arc<dyn Fn() + Send + Sync>>) {
        let m = self.clone();
        let restore_editor = move || {
            m.inner.editor_container.clear();
            let ed = m.editor_component();
            m.inner.editor_container.add_child(Arc::clone(&ed));
            m.ui().set_focus(Some(ed));
            m.ui().request_render(None);
        };
        let r2 = restore_editor.clone();
        let dialog = LoginDialogComponent::new(
            self.ui(),
            &entry.option.id,
            Arc::new(move |_, _| {
                r2();
                if let Some(ref back) = on_back {
                    back();
                }
            }),
            Some(&entry.option.name),
            Some(&format!("{} setup", entry.option.name)),
        );
        let method_name = entry.method_name.as_deref().unwrap_or("Authentication");
        dialog.show_info(
            &format!("{method_name} is configured outside {APP_NAME_LOCAL}."),
            None,
            Some(true),
        );
        let dialog_arc: Arc<dyn Component> = Arc::new(dialog);
        self.inner.editor_container.clear();
        self.inner.editor_container.add_child(Arc::clone(&dialog_arc));
        self.ui().set_focus(Some(dialog_arc));
        self.ui().request_render(None);
    }

    async fn show_api_key_login_dialog(
        &self,
        provider_id: &str,
        provider_name: &str,
        on_back: Option<Arc<dyn Fn() + Send + Sync>>,
    ) {
        let previous_model = self.session().model();
        let dialog = LoginDialogComponent::new(self.ui(), provider_id, Arc::new(|_, _| {}), Some(provider_name), None);
        if provider_id == "amazon-bedrock" {
            let t = theme();
            dialog.show_details(&[
                t.fg(
                    ThemeColor::Text,
                    "You can also use an AWS profile, IAM keys, or role-based credentials.",
                ),
                t.fg(ThemeColor::Muted, "See:"),
                t.fg(
                    ThemeColor::Accent,
                    &format!("  {}", pi_js::path::join(&[&get_docs_path_local(), "providers.md"])),
                ),
            ]);
        }
        let dialog_arc: Arc<dyn Component> = Arc::new(dialog.clone());
        self.inner.editor_container.clear();
        self.inner.editor_container.add_child(Arc::clone(&dialog_arc));
        self.ui().set_focus(Some(dialog_arc));
        self.ui().request_render(None);

        let restore_editor = || {
            self.inner.editor_container.clear();
            let ed = self.editor_component();
            self.inner.editor_container.add_child(Arc::clone(&ed));
            self.ui().set_focus(Some(ed));
            self.ui().request_render(None);
        };

        match self
            .login_provider(&dialog, provider_id, provider_name, AuthType::ApiKey)
            .await
        {
            Ok(()) => {
                restore_editor();
                self.complete_provider_authentication(provider_id, provider_name, AuthType::ApiKey, previous_model)
                    .await;
            }
            Err(err) => {
                restore_editor();
                let msg = err.to_string();
                if msg == "Login cancelled" {
                    if let Some(back) = on_back {
                        back();
                    }
                } else {
                    self.show_error(&format!("Failed to save API key for {provider_name}: {msg}"));
                }
            }
        }
    }

    async fn show_auth_select(
        &self,
        dialog: &LoginDialogComponent,
        message: &str,
        options: Vec<pi_ai::auth::types::AuthSelectOption>,
        provider_id: &str,
    ) -> Result<String> {
        let (tx, rx) = tokio::sync::oneshot::channel::<Result<String>>();
        let tx_slot = Arc::new(std::sync::Mutex::new(Some(tx)));

        let m_restore = self.clone();
        let dlg_restore = dialog.clone();
        let restore_dialog = Arc::new(move || {
            let dlg_arc: Arc<dyn Component> = Arc::new(dlg_restore.clone());
            m_restore.inner.editor_container.clear();
            m_restore.inner.editor_container.add_child(Arc::clone(&dlg_arc));
            m_restore.ui().set_focus(Some(dlg_arc));
            m_restore.ui().request_render(None);
        });

        let labels: Vec<String> = options.iter().map(|o| o.label.clone()).collect();
        let opts_map = options.clone();
        let r1 = Arc::clone(&restore_dialog);
        let t1 = Arc::clone(&tx_slot);
        let r2 = restore_dialog;
        let t2 = tx_slot;

        let selector = ExtensionSelectorComponent::new(
            message,
            labels,
            Arc::new(move |label| {
                r1();
                let id = opts_map.iter().find(|o| o.label == label).map(|o| o.id.clone());
                if let Some(sender) = t1.lock().unwrap_or_else(|e| e.into_inner()).take() {
                    let _ = sender.send(id.ok_or_else(|| pi_js::Error::msg("Login cancelled")));
                }
            }),
            Arc::new(move || {
                r2();
                if let Some(sender) = t2.lock().unwrap_or_else(|e| e.into_inner()).take() {
                    let _ = sender.send(Err(pi_js::Error::msg("Login cancelled")));
                }
            }),
            Some(ExtensionSelectorOptions {
                description: if provider_id == RADIUS_PROVIDER_ID {
                    Some(RADIUS_LOGIN_INTRO.to_string())
                } else {
                    None
                },
                ..Default::default()
            }),
        );
        let sel_arc: Arc<dyn Component> = Arc::new(selector);
        self.inner.editor_container.clear();
        self.inner.editor_container.add_child(Arc::clone(&sel_arc));
        self.ui().set_focus(Some(sel_arc));
        self.ui().request_render(None);

        rx.await.unwrap_or_else(|_| Err(pi_js::Error::msg("Login cancelled")))
    }

    async fn show_auth_prompt(
        &self,
        dialog: &LoginDialogComponent,
        prompt: AuthPrompt,
        provider_id: &str,
    ) -> Result<String> {
        if let Some(ref sig) = prompt.signal
            && sig.aborted()
        {
            return Err(pi_js::Error::msg("Login cancelled"));
        }

        let fut: BoxFuture<Result<String>> = match prompt.kind {
            AuthPromptKind::Select { message, options } => {
                let this = self.clone();
                let dlg = dialog.clone();
                let pid = provider_id.to_string();
                Box::pin(async move { this.show_auth_select(&dlg, &message, options, &pid).await })
            }
            AuthPromptKind::ManualCode { message, .. } => dialog.show_manual_input(&message),
            AuthPromptKind::Text { message, placeholder } | AuthPromptKind::Secret { message, placeholder } => {
                dialog.show_prompt(&message, placeholder.as_deref())
            }
        };

        if let Some(sig) = prompt.signal {
            let tok = sig.token();
            tokio::select! {
                res = fut => res,
                _ = tok.cancelled() => Err(pi_js::Error::msg("Login cancelled")),
            }
        } else {
            fut.await
        }
    }

    fn notify_auth_dialog(&self, dialog: &LoginDialogComponent, event: AuthEvent) {
        match event {
            AuthEvent::AuthUrl { url, instructions } => dialog.show_auth(&url, instructions.as_deref()),
            AuthEvent::DeviceCode {
                user_code,
                verification_uri,
                interval_seconds,
                expires_in_seconds,
            } => {
                dialog.show_device_code(DeviceCodeInfo {
                    user_code,
                    verification_uri,
                    open_browser_on_confirm: None,
                    copy_code_on_confirm: None,
                    interval_seconds,
                    expires_in_seconds,
                });
                dialog.show_waiting("Waiting for authentication...");
            }
            AuthEvent::Info { message, links } => dialog.show_info(&message, links.as_deref(), None),
            AuthEvent::Progress { message } => dialog.show_progress(&message),
        }
    }

    async fn login_provider(
        &self,
        dialog: &LoginDialogComponent,
        provider_id: &str,
        provider_name: &str,
        method: AuthType,
    ) -> Result<()> {
        self.inner.program_status.set_blocked(
            "login",
            Some(BlockedStatus {
                kind: BlockedKind::Auth,
                message: format!("Log in to {provider_name}"),
            }),
        );
        let signal = dialog.signal();
        let interaction: Arc<dyn AuthInteraction> = Arc::new(DialogAuthInteraction {
            mode: self.clone(),
            dialog: dialog.clone(),
            provider_id: provider_id.to_string(),
            signal: signal.clone(),
        });
        let device_id = self.settings_manager().get_or_create_device_id();
        let res = self
            .session()
            .model_runtime()
            .login(provider_id, method, interaction, signal, device_id)
            .await;
        self.inner.program_status.set_blocked("login", None);
        res
    }

    async fn show_login_dialog(
        &self,
        provider_id: &str,
        provider_name: &str,
        on_back: Option<Arc<dyn Fn() + Send + Sync>>,
    ) {
        let previous_model = self.session().model();
        let dialog = LoginDialogComponent::new(self.ui(), provider_id, Arc::new(|_, _| {}), Some(provider_name), None);
        let dialog_arc: Arc<dyn Component> = Arc::new(dialog.clone());
        self.inner.editor_container.clear();
        self.inner.editor_container.add_child(Arc::clone(&dialog_arc));
        self.ui().set_focus(Some(dialog_arc));
        self.ui().request_render(None);

        let restore_editor = || {
            self.inner.editor_container.clear();
            let ed = self.editor_component();
            self.inner.editor_container.add_child(Arc::clone(&ed));
            self.ui().set_focus(Some(ed));
            self.ui().request_render(None);
        };

        match self
            .login_provider(&dialog, provider_id, provider_name, AuthType::OAuth)
            .await
        {
            Ok(()) => {
                restore_editor();
                self.complete_provider_authentication(provider_id, provider_name, AuthType::OAuth, previous_model)
                    .await;
                if provider_id == RADIUS_PROVIDER_ID {
                    self.offer_radius_mcp_server(provider_id, provider_name);
                }
            }
            Err(err) => {
                restore_editor();
                let msg = err.to_string();
                if msg == "Login cancelled" {
                    if let Some(back) = on_back {
                        back();
                    }
                } else {
                    self.show_error(&format!("Failed to login to {provider_name}: {msg}"));
                }
            }
        }
    }

    fn offer_radius_mcp_server(&self, _provider_id: &str, _provider_name: &str) {
        // WIRE(@tui-w3): Wire crate::extensions::mcp::config::{plan_radius_mcp_server, add_mcp_server_config} once @mcp merges onto main.
    }

    // =========================================================================
    // Command Handlers
    // =========================================================================

    pub(crate) async fn handle_reload_command(&self) {
        let sess = self.session();
        if sess.is_streaming() {
            self.show_warning("Wait for the current response to finish before reloading.");
            return;
        }
        if sess.is_compacting() {
            self.show_warning("Wait for compaction to finish before reloading.");
            return;
        }

        self.reset_extension_ui();

        let reload_box = Container::new();
        let border_color: Arc<dyn Fn(&str) -> String + Send + Sync> =
            Arc::new(|s: &str| theme().fg(ThemeColor::Border, s));
        reload_box.add_child(Arc::new(DynamicBorder::new(Some(Arc::clone(&border_color)))));
        reload_box.add_child(Arc::new(Spacer::new(Some(1))));
        reload_box.add_child(Arc::new(ThemedText::new(
            Arc::new(|| {
                theme().fg(
                    ThemeColor::Muted,
                    "Reloading keybindings, extensions, skills, prompts, themes, and context files...",
                )
            }),
            Some(1),
            Some(0),
        )));
        reload_box.add_child(Arc::new(Spacer::new(Some(1))));
        reload_box.add_child(Arc::new(DynamicBorder::new(Some(border_color))));

        let previous_editor = self.editor_component();
        let reload_arc: Arc<dyn Component> = Arc::new(reload_box);
        self.inner.editor_container.clear();
        self.inner.editor_container.add_child(Arc::clone(&reload_arc));
        self.ui().set_focus(Some(reload_arc));
        self.ui().request_render(Some(true));
        tokio::task::yield_now().await;

        let chat_restored = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let m_restore = self.clone();
        let flag_restore = Arc::clone(&chat_restored);
        let restore_chat_before_start: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            if flag_restore.swap(true, std::sync::atomic::Ordering::SeqCst) {
                return;
            }
            let hide_t = m_restore.settings_manager().get_hide_thinking_block();
            let out_pad = m_restore.settings_manager().get_output_pad();
            {
                let mut st = m_restore.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                st.hide_thinking_block = hide_t;
                st.output_pad = out_pad;
            }
            m_restore.rebuild_chat_from_messages();
        });

        let dismiss_reload_box = |editor: Arc<dyn Component>| {
            self.inner.editor_container.clear();
            self.inner.editor_container.add_child(Arc::clone(&editor));
            self.ui().set_focus(Some(editor));
            self.ui().request_render(None);
        };

        match self
            .session()
            .reload(Some(Arc::clone(&restore_chat_before_start)))
            .await
        {
            Ok(()) => {
                restore_chat_before_start();
                (self.inner.hooks.keybindings_reload)(&self.inner.keybindings);

                let (custom_header_is_none, built_in_header_exp, tool_expanded) = {
                    let st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                    (
                        st.custom_header.is_none(),
                        st.built_in_header.as_ref().and_then(|(_, exp)| exp.clone()),
                        st.tool_output_expanded,
                    )
                };
                if custom_header_is_none && let Some(exp) = built_in_header_exp {
                    exp.set_expanded(tool_expanded);
                }

                let (themes, _, _) = self.session().resource_loader().get_themes();
                let _ = set_registered_themes(themes);
                self.apply_runtime_settings();
                self.inner.theme_controller.apply_from_settings();
                self.setup_autocomplete_provider();
                self.setup_extension_shortcuts();
                self.show_loaded_resources(None, false, true);
                let saved_trust = self.maybe_save_implicit_project_trust_after_reload();
                if let Some(models_err) = self.session().model_runtime().get_error() {
                    self.show_error(&format!("models.json error: {models_err}"));
                }
                self.show_status(if saved_trust {
                    "Reloaded keybindings, extensions, skills, prompts, themes, and context files; saved project trust"
                } else {
                    "Reloaded keybindings, extensions, skills, prompts, themes, and context files"
                });
                dismiss_reload_box(self.editor_component());
            }
            Err(err) => {
                dismiss_reload_box(previous_editor);
                self.show_error(&format!("Reload failed: {err}"));
            }
        }
    }

    pub(crate) async fn handle_export_command(&self, text: &str) {
        let output_path = self.get_path_command_argument(text, "/export");
        match output_path.as_deref() {
            Some(p) if p.ends_with(".jsonl") => match self.session().export_to_jsonl(Some(p)) {
                Ok(file_path) => self.show_status(&format!("Session exported to: {file_path}")),
                Err(err) => self.show_error(&format!("Failed to export session: {err}")),
            },
            opt => match self.session().export_to_html(opt, theme().name()).await {
                Ok(file_path) => self.show_status(&format!("Session exported to: {file_path}")),
                Err(err) => self.show_error(&format!("Failed to export session: {err}")),
            },
        }
    }

    fn get_path_command_argument(&self, text: &str, command: &str) -> Option<String> {
        if text == command {
            return None;
        }
        let after = text.strip_prefix(command)?.strip_prefix(' ')?;
        let args = after.trim_start();
        if args.is_empty() {
            return None;
        }
        let first = args.chars().next()?;
        if first == '"' || first == '\'' {
            let rest = &args[1..];
            let close = rest.find(first)?;
            return Some(rest[..close].to_string());
        }
        let end = args.find(char::is_whitespace).unwrap_or(args.len());
        Some(args[..end].to_string())
    }

    pub(crate) async fn handle_import_command(&self, text: &str) {
        let Some(input_path) = self.get_path_command_argument(text, "/import") else {
            self.show_error("Usage: /import <path.jsonl>");
            return;
        };
        let confirmed = self
            .show_extension_confirm(
                "Import session",
                &format!("Replace current session with {input_path}?"),
                None,
            )
            .await;
        if !confirmed {
            self.show_status("Import cancelled");
            return;
        }
        self.clear_status_indicator(None);
        match self.inner.runtime_host.import_from_jsonl(&input_path, None).await {
            Ok(result) => {
                if result.cancelled {
                    self.show_status("Import cancelled");
                } else {
                    self.show_status(&format!("Session imported from: {input_path}"));
                }
            }
            Err(err) => {
                // WIRE(@tui-w3): Special-case typed MissingSessionCwdError / SessionImportFileNotFoundError once @mcp merges onto main.
                self.handle_fatal_runtime_error("Failed to import session", &err.to_string())
                    .await;
            }
        }
    }

    pub(crate) async fn handle_share_command(&self) {
        let Some(share_sess) = self.session().as_share_session() else {
            self.show_error("Session sharing is not available");
            return;
        };
        let m_st = self.clone();
        let m_err = self.clone();
        share_session(&SessionShareContext {
            session: share_sess,
            ui: self.ui(),
            editor_container: self.inner.editor_container.clone(),
            editor: self.editor_component(),
            show_status: Arc::new(move |msg| m_st.show_status(msg)),
            show_error: Arc::new(move |msg| m_err.show_error(msg)),
        })
        .await;
    }

    pub(crate) async fn handle_bug_command(&self, hint: Option<&str>) {
        let Some(bug_session) = self.session().as_bug_report_session() else {
            self.show_error("Bug reporting is not available");
            return;
        };
        let m_st = self.clone();
        let m_err = self.clone();
        report_bug(
            &BugReportContext {
                session: bug_session,
                ui: self.ui(),
                editor_container: self.inner.editor_container.clone(),
                editor: self.editor_component(),
                keybindings: (*self.inner.keybindings).clone(),
                show_status: Arc::new(move |msg| m_st.show_status(msg)),
                show_error: Arc::new(move |msg| m_err.show_error(msg)),
            },
            hint,
        )
        .await;
    }

    async fn copy_text_to_clipboard(&self, text: &str) -> Result<()> {
        // WIRE(@tui-w3): Also invoke `crate::utils::clipboard::copy_to_clipboard(text)` once @mcp merges onto main.
        (self.inner.hooks.clipboard_copy)(text).await
    }

    pub(crate) async fn handle_copy_command(&self, flash_confirmation: bool, prefer_selection: bool) {
        if prefer_selection {
            let alt_opt = self
                .inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .renderer
                .as_alt_screen()
                .cloned();
            if let Some(alt) = alt_opt
                && !alt.get_copy_on_select()
                && alt.has_active_selection()
            {
                let _ = alt.copy_active_selection_to_clipboard().await;
                return;
            }
        }

        let Some(text) = self.session().get_last_assistant_text() else {
            self.show_error("No agent messages to copy yet.");
            return;
        };

        if let Err(err) = self.copy_text_to_clipboard(&text).await {
            self.show_error(&err.to_string());
            return;
        }
        let alt_opt = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .renderer
            .as_alt_screen()
            .cloned();
        if flash_confirmation && let Some(alt) = alt_opt {
            alt.flash("Copied!", None);
        } else {
            self.show_status("Copied last agent message to clipboard");
        }
    }

    pub(crate) fn handle_name_command(&self, text: &str) {
        let trimmed = text.strip_prefix("/name").unwrap_or(text).trim();
        if trimmed.is_empty() {
            if let Some(current_name) = self.session_manager().get_session_name() {
                self.add_chat_component(Arc::new(Spacer::new(Some(1))), None, None, None);
                let msg = format!("Session name: {current_name}");
                self.add_chat_component(
                    Arc::new(ThemedText::new(
                        Arc::new(move || theme().fg(ThemeColor::Dim, &msg)),
                        Some(1),
                        Some(0),
                    )),
                    None,
                    None,
                    None,
                );
            } else {
                self.show_warning("Usage: /name <name>");
            }
            self.ui().request_render(None);
            return;
        }

        self.session().set_session_name(trimmed);
        let session_name = self.session_manager().get_session_name();
        if session_name.as_deref() != Some(trimmed) {
            let from_j = pi_js::json::stringify(&serde_json::Value::String(trimmed.to_string()));
            let to_j = match &session_name {
                Some(n) => pi_js::json::stringify(&serde_json::Value::String(n.clone())),
                None => "undefined".to_string(),
            };
            self.show_warning(&format!("Session name was normalized from {from_j} to {to_j}"));
        }
        let display_name = session_name.unwrap_or_else(|| trimmed.to_string());
        self.add_chat_component(Arc::new(Spacer::new(Some(1))), None, None, None);
        let msg = format!("Session name set: {display_name}");
        self.add_chat_component(
            Arc::new(ThemedText::new(
                Arc::new(move || theme().fg(ThemeColor::Dim, &msg)),
                Some(1),
                Some(0),
            )),
            None,
            None,
            None,
        );
        self.ui().request_render(None);
    }

    pub(crate) fn handle_session_command(&self) {
        let sess = self.session();
        let stats = sess.get_session_stats();
        let session_name = self.session_manager().get_session_name();
        let (missed_tokens, miss_count, missed_cost) = sess.compute_cache_waste();
        let usage_breakdown = sess.get_usage_cost_breakdown();
        let cache_warming_status = sess.cache_warming_status();
        let cache_warming_mode = self.settings_manager().get_cache_warming_mode();
        let model = sess.model();
        let selected_model_key = match &model {
            Some(m) => format!("{}/{}", m.provider, m.id),
            None => "undefined/undefined".to_string(),
        };

        let render_info = Arc::new(move || {
            let t = theme();
            let mut info = format!("{}\n\n", t.bold("Session Info"));
            if let Some(ref name) = session_name {
                info.push_str(&format!("{} {name}\n", t.fg(ThemeColor::Dim, "Name:")));
            }
            info.push_str(&format!(
                "{} {}\n",
                t.fg(ThemeColor::Dim, "File:"),
                stats.session_file.as_deref().unwrap_or("In-memory")
            ));
            info.push_str(&format!("{} {}\n\n", t.fg(ThemeColor::Dim, "ID:"), stats.session_id));
            info.push_str(&format!("{}\n", t.bold("Messages")));
            info.push_str(&format!(
                "{} {}\n",
                t.fg(ThemeColor::Dim, "Total:"),
                stats.total_messages
            ));
            info.push_str(&format!("{} {}\n", t.fg(ThemeColor::Dim, "User:"), stats.user_messages));
            info.push_str(&format!(
                "{} {}\n",
                t.fg(ThemeColor::Dim, "Assistant:"),
                stats.assistant_messages
            ));
            info.push_str(&format!(
                "{} {} calls, {} results\n\n",
                t.fg(ThemeColor::Dim, "Tools:"),
                stats.tool_calls,
                stats.tool_results
            ));
            info.push_str(&format!("{}\n", t.bold("Tokens")));
            let prompt_tokens = stats.input_tokens + stats.cache_read_tokens + stats.cache_write_tokens;
            info.push_str(&format!(
                "{} {}\n",
                t.fg(ThemeColor::Dim, "Input:"),
                format_int_commas(prompt_tokens)
            ));
            if prompt_tokens > 0 && (stats.cache_read_tokens > 0 || stats.cache_write_tokens > 0) {
                let pct = (stats.cache_read_tokens as f64 / prompt_tokens as f64) * 100.0;
                let hit_rate = t.fg(ThemeColor::Dim, &format!("({pct:.1}%)"));
                info.push_str(&format!(
                    "  {} {} {hit_rate}\n",
                    t.fg(ThemeColor::Dim, "Cached:"),
                    format_int_commas(stats.cache_read_tokens)
                ));
                let written = if stats.cache_write_tokens > 0 {
                    format!(
                        " {}",
                        t.fg(
                            ThemeColor::Dim,
                            &format!("({} written to cache)", format_int_commas(stats.cache_write_tokens))
                        )
                    )
                } else {
                    String::new()
                };
                info.push_str(&format!(
                    "  {} {}{written}\n",
                    t.fg(ThemeColor::Dim, "Uncached:"),
                    format_int_commas(stats.input_tokens + stats.cache_write_tokens)
                ));
            }
            info.push_str(&format!(
                "{} {}\n",
                t.fg(ThemeColor::Dim, "Output:"),
                format_int_commas(stats.output_tokens)
            ));
            info.push_str(&format!(
                "{} {}\n",
                t.fg(ThemeColor::Dim, "Total:"),
                format_int_commas(stats.total_tokens)
            ));

            info.push_str(&format!("\n{}\n", t.bold("Cache Warming")));
            info.push_str(&format!(
                "{} {}\n",
                t.fg(ThemeColor::Dim, "Mode:"),
                cache_warming_mode.as_str()
            ));
            let status_str = cache_warming_status
                .as_ref()
                .map(|s| s.status_text.as_str())
                .unwrap_or("Inactive (cache warming unavailable)");
            info.push_str(&format!("{} {status_str}\n", t.fg(ThemeColor::Dim, "Status:")));
            if let Some(ref cw) = cache_warming_status
                && cw.economics_available
            {
                info.push_str(&format!(
                    "{} ${:.3}\n",
                    t.fg(ThemeColor::Dim, "Cache miss penalty:"),
                    cw.miss_cost
                ));
                info.push_str(&format!(
                    "{} ${:.3}\n",
                    t.fg(ThemeColor::Dim, "Refresh cost:"),
                    cw.warm_cost
                ));
            }

            if stats.cost > 0.0 || missed_tokens > 0 {
                info.push_str(&format!("\n{}\n", t.bold("Cost")));
                info.push_str(&format!("{} ${:.3}", t.fg(ThemeColor::Dim, "Total:"), stats.cost));
                if usage_breakdown.len() > 1 || usage_breakdown.first().is_some_and(|e| e.key != selected_model_key) {
                    for entry in &usage_breakdown {
                        info.push_str(&format!(
                            "\n  {} ${:.3} {}",
                            t.fg(ThemeColor::Dim, &format!("{}:", entry.key)),
                            entry.cost,
                            t.fg(
                                ThemeColor::Dim,
                                &format!(
                                    "({} tokens)",
                                    crate::modes::interactive::components::footer::format_tokens(entry.tokens)
                                )
                            )
                        ));
                    }
                }
                if missed_tokens > 0 {
                    let miss_label = if miss_count == 1 {
                        "1 miss".to_string()
                    } else {
                        format!("{miss_count} misses")
                    };
                    let detail = format!("{} tokens, {miss_label}", format_int_commas(missed_tokens));
                    if missed_cost >= 0.0001 {
                        info.push_str(&format!(
                            "\n{} ${missed_cost:.3} {}",
                            t.fg(ThemeColor::Dim, "Cache Re-billed:"),
                            t.fg(ThemeColor::Dim, &format!("({detail})"))
                        ));
                    } else {
                        info.push_str(&format!("\n{} {detail}", t.fg(ThemeColor::Dim, "Cache Re-billed:")));
                    }
                }
            }
            info
        });

        self.add_chat_component(Arc::new(Spacer::new(Some(1))), None, None, None);
        self.add_chat_component(
            Arc::new(ThemedText::new(render_info, Some(1), Some(0))),
            None,
            None,
            None,
        );
        self.ui().request_render(None);
    }

    pub(crate) fn handle_changelog_command(&self) {
        let changelog_path = get_changelog_path_local();
        let all_entries = parse_changelog_local(&changelog_path);
        let changelog_md = if all_entries.is_empty() {
            "No changelog entries found.".to_string()
        } else {
            all_entries
                .into_iter()
                .rev()
                .map(|e| normalize_changelog_links_local(&e.content, &e.version_string()))
                .collect::<Vec<_>>()
                .join("\n\n")
        };

        self.add_chat_component(Arc::new(Spacer::new(Some(1))), None, None, None);
        self.add_chat_component(Arc::new(DynamicBorder::default()), None, None, None);
        self.add_chat_component(
            Arc::new(ThemedText::new(
                Arc::new(|| theme().bold(&theme().fg(ThemeColor::Accent, "What's New"))),
                Some(1),
                Some(0),
            )),
            None,
            None,
            None,
        );
        self.add_chat_component(Arc::new(Spacer::new(Some(1))), None, None, None);
        self.add_chat_component(
            self.create_changelog_markdown(&changelog_md, 1, 1, None),
            None,
            None,
            None,
        );
        self.add_chat_component(Arc::new(DynamicBorder::default()), None, None, None);
        self.ui().request_render(None);
    }

    pub(crate) fn get_app_key_display(&self, action: &str) -> String {
        key_display_text(&action.to_string())
    }

    pub(crate) fn get_editor_key_display(&self, action: &str) -> String {
        key_display_text(&action.to_string())
    }

    pub(crate) fn handle_hotkeys_command(&self) {
        let cursor_up = self.get_editor_key_display("tui.editor.cursorUp");
        let cursor_down = self.get_editor_key_display("tui.editor.cursorDown");
        let cursor_left = self.get_editor_key_display("tui.editor.cursorLeft");
        let cursor_right = self.get_editor_key_display("tui.editor.cursorRight");
        let cursor_word_left = self.get_editor_key_display("tui.editor.cursorWordLeft");
        let cursor_word_right = self.get_editor_key_display("tui.editor.cursorWordRight");
        let cursor_line_start = self.get_editor_key_display("tui.editor.cursorLineStart");
        let cursor_line_end = self.get_editor_key_display("tui.editor.cursorLineEnd");
        let jump_forward = self.get_editor_key_display("tui.editor.jumpForward");
        let jump_backward = self.get_editor_key_display("tui.editor.jumpBackward");
        let page_up = self.get_editor_key_display("tui.editor.pageUp");
        let page_down = self.get_editor_key_display("tui.editor.pageDown");

        let submit = self.get_editor_key_display("tui.input.submit");
        let new_line = self.get_editor_key_display("tui.input.newLine");
        let delete_word_backward = self.get_editor_key_display("tui.editor.deleteWordBackward");
        let delete_word_forward = self.get_editor_key_display("tui.editor.deleteWordForward");
        let delete_to_line_start = self.get_editor_key_display("tui.editor.deleteToLineStart");
        let delete_to_line_end = self.get_editor_key_display("tui.editor.deleteToLineEnd");
        let yank = self.get_editor_key_display("tui.editor.yank");
        let yank_pop = self.get_editor_key_display("tui.editor.yankPop");
        let undo = self.get_editor_key_display("tui.editor.undo");
        let tab = self.get_editor_key_display("tui.input.tab");

        let interrupt = self.get_app_key_display("app.interrupt");
        let clear = self.get_app_key_display("app.clear");
        let exit = self.get_app_key_display("app.exit");
        let suspend = self.get_app_key_display("app.suspend");
        let cycle_thinking = self.get_app_key_display("app.thinking.cycle");
        let cycle_model_fwd = self.get_app_key_display("app.model.cycleForward");
        let select_model = self.get_app_key_display("app.model.select");
        let expand_tools = self.get_app_key_display("app.tools.expand");
        let toggle_thinking = self.get_app_key_display("app.thinking.toggle");
        let external_editor = self.get_app_key_display("app.editor.external");
        let cycle_model_bwd = self.get_app_key_display("app.model.cycleBackward");
        let copy_message = self.get_app_key_display("app.message.copy");
        let follow_up = self.get_app_key_display("app.message.followUp");
        let dequeue = self.get_app_key_display("app.message.dequeue");
        let paste_image = self.get_app_key_display("app.clipboard.pasteImage");

        let win_hint = if cfg!(windows) {
            " (Ctrl+Enter on Windows Terminal)"
        } else {
            ""
        };

        let mut hotkeys = format!(
            "
**Navigation**
| Key | Action |
|-----|--------|
| `{cursor_up}` / `{cursor_down}` / `{cursor_left}` / `{cursor_right}` | Move cursor / browse history |
| `{cursor_word_left}` / `{cursor_word_right}` | Move by word |
| `{cursor_line_start}` | Start of line |
| `{cursor_line_end}` | End of line |
| `{jump_forward}` | Jump forward to character |
| `{jump_backward}` | Jump backward to character |
| `{page_up}` / `{page_down}` | Scroll by page |

**Editing**
| Key | Action |
|-----|--------|
| `{submit}` | Send message |
| `{new_line}` | New line{win_hint} |
| `{delete_word_backward}` | Delete word backwards |
| `{delete_word_forward}` | Delete word forwards |
| `{delete_to_line_start}` | Delete to start of line |
| `{delete_to_line_end}` | Delete to end of line |
| `{yank}` | Paste the most-recently-deleted text |
| `{yank_pop}` | Cycle through the deleted text after pasting |
| `{undo}` | Undo |

**Other**
| Key | Action |
|-----|--------|
| `{tab}` | Path completion / accept autocomplete |
| `{interrupt}` | Cancel autocomplete / abort streaming |
| `{clear}` | Clear editor (first) / exit (second) |
| `{exit}` | Exit (when editor is empty) |
| `{suspend}` | Suspend to background |
| `{cycle_thinking}` | Cycle thinking level |
| `{cycle_model_fwd}` / `{cycle_model_bwd}` | Cycle models |
| `{select_model}` | Open model selector |
| `{expand_tools}` | Toggle tool output expansion |
| `{toggle_thinking}` | Toggle thinking block visibility |
| `{external_editor}` | Edit message in external editor |
| `{copy_message}` | Copy selection or last assistant message |
| `{follow_up}` | Queue follow-up message |
| `{dequeue}` | Restore queued messages |
| `{paste_image}` | Paste files on macOS, images, or text from clipboard |
| `/` | Slash commands |
| `!` | Run bash command |
| `!!` | Run bash command (excluded from context) |
"
        );

        let shortcuts = self.session().extension_runner().get_shortcuts();
        if !shortcuts.is_empty() {
            hotkeys.push_str("\n**Extensions**\n| Key | Action |\n|-----|--------|\n");
            for (key, shortcut) in shortcuts {
                let desc = shortcut.description.unwrap_or(shortcut.extension_path);
                let key_display = format_key_text(&key, KeyTextFormatOptions { capitalize: true });
                hotkeys.push_str(&format!("| `{key_display}` | {desc} |\n"));
            }
        }

        self.add_chat_component(Arc::new(Spacer::new(Some(1))), None, None, None);
        self.add_chat_component(Arc::new(DynamicBorder::default()), None, None, None);
        self.add_chat_component(
            Arc::new(ThemedText::new(
                Arc::new(|| theme().bold(&theme().fg(ThemeColor::Accent, "Keyboard Shortcuts"))),
                Some(1),
                Some(0),
            )),
            None,
            None,
            None,
        );
        self.add_chat_component(Arc::new(Spacer::new(Some(1))), None, None, None);
        self.add_chat_component(
            Arc::new(Markdown::new(
                hotkeys.trim(),
                1,
                1,
                self.get_markdown_theme_with_settings(),
                None,
                None,
            )),
            None,
            None,
            None,
        );
        self.add_chat_component(Arc::new(DynamicBorder::default()), None, None, None);
        self.ui().request_render(None);
    }

    pub(crate) async fn handle_clear_command(&self) {
        self.clear_status_indicator(None);
        match self.inner.runtime_host.new_session().await {
            Ok(res) => {
                if res.cancelled {
                    return;
                }
                self.add_chat_component(Arc::new(Spacer::new(Some(1))), None, None, None);
                self.add_chat_component(
                    Arc::new(ThemedText::new(
                        Arc::new(|| theme().fg(ThemeColor::Accent, "✓ New session started")),
                        Some(1),
                        Some(1),
                    )),
                    None,
                    None,
                    None,
                );
                self.ui().request_render(None);
            }
            Err(err) => {
                self.handle_fatal_runtime_error("Failed to create session", &err.to_string())
                    .await;
            }
        }
    }

    pub(crate) fn handle_debug_command(&self) {
        let term = self.ui().terminal();
        let width = term.columns() as usize;
        let height = term.rows() as usize;
        let all_lines = self.ui().render(width);
        let debug_log_path = get_debug_log_path_local();

        let mut parts: Vec<String> = vec![
            format!("Debug output at {}", chrono::Utc::now().to_rfc3339()),
            format!("Terminal: {width}x{height}"),
            format!("Total lines: {}", all_lines.len()),
            String::new(),
            "=== All rendered lines with visible widths ===".to_string(),
        ];
        for (idx, line) in all_lines.iter().enumerate() {
            let vw = visible_width(line);
            let escaped = pi_js::json::stringify(&serde_json::Value::String(line.clone()));
            parts.push(format!("[{idx}] (w={vw}) {escaped}"));
        }
        parts.push(String::new());
        parts.push("=== Agent messages (JSONL) ===".to_string());
        for msg in self.session().messages() {
            if let Ok(v) = serde_json::to_value(&msg) {
                parts.push(pi_js::json::stringify(&v));
            }
        }
        parts.push(String::new());

        let parent = pi_js::path::dirname(&debug_log_path);
        if !parent.is_empty() {
            let _ = pi_js::fs::mkdir(&parent, true, None);
        }
        let _ = pi_js::fs::write_file(&debug_log_path, &parts.join("\n"), None);

        self.add_chat_component(Arc::new(Spacer::new(Some(1))), None, None, None);
        let path_disp = debug_log_path;
        self.add_chat_component(
            Arc::new(ThemedText::new(
                Arc::new(move || {
                    format!(
                        "{}\n{}",
                        theme().fg(ThemeColor::Accent, "✓ Debug log written"),
                        theme().fg(ThemeColor::Muted, &path_disp)
                    )
                }),
                Some(1),
                Some(1),
            )),
            None,
            None,
            None,
        );
        self.ui().request_render(None);
    }

    pub(crate) fn handle_armin_says_hi(&self) {
        let alt_opt = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .renderer
            .as_alt_screen()
            .cloned();
        if play_armin_3d(alt_opt.as_ref()) {
            return;
        }
        self.add_chat_component(Arc::new(Spacer::new(Some(1))), None, None, None);
        self.add_chat_component(Arc::new(ArminComponent::new(self.ui())), None, None, None);
        self.ui().request_render(None);
    }

    pub(crate) fn handle_demented_elves(&self) {
        self.add_chat_component(Arc::new(Spacer::new(Some(1))), None, None, None);
        self.add_chat_component(Arc::new(EarendilAnnouncementComponent::new()), None, None, None);
        self.ui().request_render(None);
    }

    pub(crate) async fn handle_bash_command(&self, command: &str, exclude_from_context: bool) {
        let runner = self.session().extension_runner();
        let cwd = self.session_manager().get_cwd();
        let ext_res = if runner.has_user_bash_handlers() {
            match runner.emit_user_bash(command, exclude_from_context, &cwd).await {
                Ok(res) => res,
                Err(_) => return,
            }
        } else {
            None
        };

        let (tool_expanded, output_pad) = {
            let st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            (st.tool_output_expanded, st.output_pad)
        };

        if let Some(result) = ext_res {
            let comp = BashExecutionComponent::new(command, self.ui(), Some(exclude_from_context), Some(output_pad));
            comp.set_expanded(tool_expanded);
            let comp_arc: Arc<dyn Component> = Arc::new(comp.clone());
            let pad_comp = comp.clone();
            let pad_fn: Arc<dyn Fn(i64) + Send + Sync> = Arc::new(move |p| pad_comp.set_output_pad(p));
            if self.session().is_streaming() {
                self.inner
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .pending_bash_components
                    .push((comp.clone(), Arc::clone(&comp_arc)));
                self.add_pending_component(comp_arc, None, Some(pad_fn));
            } else {
                self.add_chat_component(comp_arc, None, None, Some(pad_fn));
            }
            if !result.output.is_empty() {
                comp.append_output(&result.output);
            }
            let trunc = if result.truncated {
                Some(TruncationResult {
                    content: result.output.clone(),
                    truncated: true,
                    truncated_by: None,
                    total_lines: 0,
                    total_bytes: 0,
                    output_lines: 0,
                    output_bytes: 0,
                    last_line_partial: false,
                    first_line_exceeds_limit: false,
                    max_lines: 0,
                    max_bytes: 0,
                })
            } else {
                None
            };
            comp.set_complete(
                result.exit_code,
                result.cancelled,
                trunc,
                result.full_output_path.as_deref(),
            );
            self.session()
                .record_bash_result(command, &result, exclude_from_context);
            self.ui().request_render(None);
            return;
        }

        let is_deferred = self.session().is_streaming();
        let comp = BashExecutionComponent::new(command, self.ui(), Some(exclude_from_context), Some(output_pad));
        comp.set_expanded(tool_expanded);
        let comp_arc: Arc<dyn Component> = Arc::new(comp.clone());
        {
            let mut st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            st.bash_component = Some(comp.clone());
            if is_deferred {
                st.pending_bash_components.push((comp.clone(), Arc::clone(&comp_arc)));
            }
        }
        let pad_comp = comp.clone();
        let pad_fn: Arc<dyn Fn(i64) + Send + Sync> = Arc::new(move |p| pad_comp.set_output_pad(p));
        if is_deferred {
            self.add_pending_component(comp_arc, None, Some(pad_fn));
        } else {
            self.add_chat_component(comp_arc, None, None, Some(pad_fn));
        }
        self.ui().request_render(None);

        let m_chunk = self.clone();
        let on_chunk: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |chunk| {
            let active = m_chunk
                .inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .bash_component
                .clone();
            if let Some(c) = active {
                c.append_output(chunk);
                m_chunk.ui().request_render(None);
            }
        });

        match self
            .session()
            .execute_bash(command, on_chunk, exclude_from_context)
            .await
        {
            Ok(result) => {
                let active = self
                    .inner
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .bash_component
                    .take();
                if let Some(c) = active {
                    let trunc = if result.truncated {
                        Some(TruncationResult {
                            content: result.output.clone(),
                            truncated: true,
                            truncated_by: None,
                            total_lines: 0,
                            total_bytes: 0,
                            output_lines: 0,
                            output_bytes: 0,
                            last_line_partial: false,
                            first_line_exceeds_limit: false,
                            max_lines: 0,
                            max_bytes: 0,
                        })
                    } else {
                        None
                    };
                    c.set_complete(
                        result.exit_code,
                        result.cancelled,
                        trunc,
                        result.full_output_path.as_deref(),
                    );
                }
            }
            Err(err) => {
                let active = self
                    .inner
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .bash_component
                    .take();
                if let Some(c) = active {
                    c.set_complete(None, false, None, None);
                }
                self.show_error(&format!("Bash command failed: {err}"));
            }
        }
        self.ui().request_render(None);
    }

    pub(crate) async fn handle_compact_command(&self, custom_instructions: Option<&str>) {
        self.clear_status_indicator(None);
        let _ = self.session().compact(custom_instructions).await;
    }

    pub fn stop(&self, fullscreen_exit_output: Option<FullscreenExitOutput>) {
        let output = fullscreen_exit_output.unwrap_or_else(|| self.settings_manager().get_fullscreen_exit_output());
        self.dispose_active_selector();
        if self.settings_manager().get_show_terminal_progress() {
            self.ui().terminal().set_progress(false);
        }
        self.clear_status_indicator(None);
        self.inner.theme_controller.disable_auto_sync();
        self.clear_extension_terminal_input_listeners();
        self.inner.footer.dispose();
        self.inner.footer_data_provider.dispose();

        let unsub = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .unsubscribe
            .take();
        drop(unsub);

        let was_init = {
            let mut st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            let prev = st.is_initialized;
            st.is_initialized = false;
            prev
        };
        if was_init {
            self.stop_interactive_tui(output);
        }
        self.unregister_signal_handlers();
    }
}

trait ForkSessionExt {
    fn get_user_messages_for_for_king_or_empty(
        &self,
    ) -> Vec<crate::modes::interactive::components::user_message_selector::UserMessageItem>;
}

impl ForkSessionExt for dyn InteractiveSession {
    fn get_user_messages_for_for_king_or_empty(
        &self,
    ) -> Vec<crate::modes::interactive::components::user_message_selector::UserMessageItem> {
        self.get_user_messages_for_forking()
    }
}

impl From<pi_tui::tui::TuiMode> for crate::modes::interactive::components::settings_selector::TuiMode {
    fn from(mode: pi_tui::tui::TuiMode) -> Self {
        match mode {
            pi_tui::tui::TuiMode::Regular => Self::Regular,
            pi_tui::tui::TuiMode::Fullscreen => Self::Fullscreen,
        }
    }
}
