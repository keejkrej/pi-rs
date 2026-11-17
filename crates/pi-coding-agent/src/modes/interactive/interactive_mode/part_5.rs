//! Port of packages/coding-agent/src/modes/interactive/interactive-mode.ts#L4764-L5691

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use indexmap::{IndexMap, IndexSet};
use pi_agent_core::ThinkingLevel;
use pi_ai::{Model, ModelThinkingLevel, ProviderId};
use pi_js::abort::{AbortController, AbortSignal};
use pi_js::{BoxFuture, Result};
use pi_tui::components::settings_list::SettingsList;
use pi_tui::terminal_image::{get_capabilities, hyperlink};
use pi_tui::{Component, DefaultTextStyle, Spacer, TruncatedText};

use crate::modes::interactive::components::dynamic_border::DynamicBorder;
use crate::modes::interactive::components::model_selector::{
    DefaultModelReference, ModelSelectorComponent, ScopedModelItem,
};
use crate::modes::interactive::components::scoped_models_selector::{
    ModelsCallbacks, ModelsConfig, RefreshStatusKind, ScopedModelsSelectorComponent,
};
use crate::modes::interactive::components::settings_selector::{
    SettingsCallbacks, SettingsConfig, SettingsSelectorComponent,
};
use crate::modes::interactive::components::themed_text::ThemedText;
use crate::modes::interactive::components::thinking_selector::ThinkingSelectorComponent;
use crate::modes::interactive::components::trust_selector::{
    ProjectTrustStoreEntry, ProjectTrustUpdate, TrustSelection, TrustSelectorComponent, TrustSelectorOptions,
};
use crate::modes::interactive::external_editor::{
    ExternalEditorOptions, ExternalEditorResult, edit_in_external_editor,
};
use crate::modes::interactive::model_catalog_refresh::{
    ModelCatalogRefreshResult, ModelCatalogRuntime, refresh_model_catalogs,
};
use crate::modes::interactive::theme::theme::{SYSTEM_THEME_NAME, ThemeColor, get_available_themes, theme};

use super::*;

pub const DEFAULT_THINKING_LEVEL_LOCAL: ModelThinkingLevel = ModelThinkingLevel::Medium;
pub const THINKING_LEVEL_OPTIONS_LOCAL: &[ModelThinkingLevel] = &[
    ModelThinkingLevel::Off,
    ModelThinkingLevel::Minimal,
    ModelThinkingLevel::Low,
    ModelThinkingLevel::Medium,
    ModelThinkingLevel::High,
    ModelThinkingLevel::XHigh,
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LatestPiRelease {
    pub version: String,
    pub note: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelScopeDiagnostic {
    pub code: &'static str,
    pub message: String,
    pub pattern: String,
}

#[derive(Clone, Debug, Default)]
pub struct ResolveModelScopeResult {
    pub scoped_models: Vec<ScopedModelItem>,
    pub diagnostics: Vec<ModelScopeDiagnostic>,
}

fn model_thinking_to_str(level: ModelThinkingLevel) -> &'static str {
    match level {
        ModelThinkingLevel::Off => "off",
        ModelThinkingLevel::Minimal => "minimal",
        ModelThinkingLevel::Low => "low",
        ModelThinkingLevel::Medium => "medium",
        ModelThinkingLevel::High => "high",
        ModelThinkingLevel::XHigh => "xhigh",
        ModelThinkingLevel::Max => "max",
    }
}

fn parse_thinking_level_str(s: &str) -> Option<ModelThinkingLevel> {
    match s.to_ascii_lowercase().as_str() {
        "off" => Some(ModelThinkingLevel::Off),
        "minimal" => Some(ModelThinkingLevel::Minimal),
        "low" => Some(ModelThinkingLevel::Low),
        "medium" => Some(ModelThinkingLevel::Medium),
        "high" => Some(ModelThinkingLevel::High),
        "xhigh" => Some(ModelThinkingLevel::XHigh),
        "max" => Some(ModelThinkingLevel::Max),
        _ => None,
    }
}

fn to_agent_thinking_level(level: ModelThinkingLevel) -> ThinkingLevel {
    match level {
        ModelThinkingLevel::Off => ThinkingLevel::Off,
        ModelThinkingLevel::Minimal => ThinkingLevel::Minimal,
        ModelThinkingLevel::Low => ThinkingLevel::Low,
        ModelThinkingLevel::Medium => ThinkingLevel::Medium,
        ModelThinkingLevel::High => ThinkingLevel::High,
        ModelThinkingLevel::XHigh | ModelThinkingLevel::Max => ThinkingLevel::Xhigh,
    }
}

fn from_agent_thinking_level(level: ThinkingLevel) -> ModelThinkingLevel {
    match level {
        ThinkingLevel::Off => ModelThinkingLevel::Off,
        ThinkingLevel::Minimal => ModelThinkingLevel::Minimal,
        ThinkingLevel::Low => ModelThinkingLevel::Low,
        ThinkingLevel::Medium => ModelThinkingLevel::Medium,
        ThinkingLevel::High => ModelThinkingLevel::High,
        ThinkingLevel::Xhigh => ModelThinkingLevel::XHigh,
    }
}

fn format_http_idle_timeout_ms_display(timeout_ms: u64) -> String {
    if timeout_ms == 0 {
        return "disabled".to_string();
    }
    if timeout_ms % 60_000 == 0 {
        format!("{} min", timeout_ms / 60_000)
    } else if timeout_ms % 1000 == 0 {
        format!("{} sec", timeout_ms / 1000)
    } else {
        format!("{timeout_ms} ms")
    }
}

// WIRE(@tui-w3): Replace local trust store mirror with `crate::core::trust_manager::ProjectTrustStore` once @mcp merges onto main.
struct ProjectTrustStoreLocal {
    trust_path: String,
}

impl ProjectTrustStoreLocal {
    fn new(agent_dir: &str) -> Self {
        let resolved_dir = pi_js::path::resolve(&[agent_dir]);
        Self {
            trust_path: pi_js::path::join(&[&resolved_dir, "trust.json"]),
        }
    }

    fn normalize_cwd(cwd: &str) -> String {
        let resolved = pi_js::path::resolve(&[cwd]);
        std::fs::canonicalize(&resolved)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or(resolved)
    }

    fn read_trust_file(&self) -> IndexMap<String, bool> {
        let Ok(raw) = pi_js::fs::read_to_string(&self.trust_path) else {
            return IndexMap::new();
        };
        let trimmed = raw.strip_prefix('\u{feff}').unwrap_or(&raw);
        let Ok(serde_json::Value::Object(map)) = pi_js::json::parse(trimmed) else {
            return IndexMap::new();
        };
        let mut out = IndexMap::new();
        for (k, v) in map {
            if let Some(b) = v.as_bool() {
                out.insert(k, b);
            }
        }
        out
    }

    fn write_trust_file(&self, map: &IndexMap<String, bool>) -> Result<()> {
        let parent = pi_js::path::dirname(&self.trust_path);
        if !parent.is_empty() {
            pi_js::fs::mkdir(&parent, true, None)?;
        }
        let mut obj = serde_json::Map::new();
        for (k, v) in map {
            obj.insert(k.clone(), serde_json::Value::Bool(*v));
        }
        let json_text = pi_js::json::stringify_pretty(&serde_json::Value::Object(obj), "  ");
        pi_js::fs::write_file(&self.trust_path, &format!("{json_text}\n"), None)?;
        Ok(())
    }

    fn get(&self, cwd: &str) -> Option<bool> {
        self.get_entry(cwd).map(|e| e.decision)
    }

    fn get_entry(&self, cwd: &str) -> Option<ProjectTrustStoreEntry> {
        let data = self.read_trust_file();
        let mut current = Self::normalize_cwd(cwd);
        loop {
            if let Some(&decision) = data.get(&current) {
                return Some(ProjectTrustStoreEntry {
                    path: current,
                    decision,
                });
            }
            let parent = pi_js::path::dirname(&current);
            if parent == current {
                return None;
            }
            current = parent;
        }
    }

    fn set(&self, cwd: &str, decision: Option<bool>) -> Result<()> {
        self.set_many(&[ProjectTrustUpdate {
            path: cwd.to_string(),
            decision,
        }])
    }

    fn set_many(&self, updates: &[ProjectTrustUpdate]) -> Result<()> {
        let mut data = self.read_trust_file();
        for upd in updates {
            let key = Self::normalize_cwd(&upd.path);
            data.shift_remove(&key);
            if let Some(decision) = upd.decision {
                data.insert(key, decision);
            }
        }
        self.write_trust_file(&data)
    }
}

struct ModelCatalogBridge(pub(crate) Arc<dyn InteractiveModelRuntime>);

impl ModelCatalogRuntime for ModelCatalogBridge {
    fn runtime_ptr_key(&self) -> usize {
        self.0.runtime_ptr_key()
    }

    fn refresh(&self, signal: AbortSignal) -> BoxFuture<Result<ModelCatalogRefreshResult>> {
        self.0.refresh(signal)
    }

    fn get_available_snapshot(&self) -> Vec<Model> {
        self.0.get_available_snapshot()
    }

    fn get_model(&self, provider: &str, id: &str) -> Option<Model> {
        self.0.get_model(provider, id)
    }

    fn get_error(&self) -> Option<String> {
        self.0.get_error()
    }
}

fn split_provider_prefix<'a>(text: &'a str, models: &'a [Model]) -> Option<(String, String)> {
    let lower_text = text.to_ascii_lowercase();
    let mut best: Option<(String, String)> = None;
    for model in models {
        let prefix = model.provider.trim().to_ascii_lowercase();
        if prefix.is_empty() {
            continue;
        }
        if let Some((ref best_p, _)) = best
            && best_p.len() >= model.provider.len()
        {
            continue;
        }
        if lower_text.len() <= prefix.len() {
            continue;
        }
        if let Some(rel_slash) = lower_text[prefix.len()..].find('/') {
            let slash_idx = prefix.len() + rel_slash;
            if lower_text[..slash_idx].trim() != prefix {
                continue;
            }
            let model_text = text[slash_idx + 1..].trim();
            if !model_text.is_empty() {
                best = Some((model.provider.clone(), model_text.to_string()));
            }
        }
    }
    best
}

// WIRE(@tui-w3): Replace local matchers with `crate::core::model_resolver::{find_exact_model_reference_match, resolve_model_scope_from_models}` once @ca-cli merges onto main.
pub fn find_exact_model_reference_match_local(model_reference: &str, models: &[Model]) -> Option<Model> {
    let text = model_reference.trim();
    if text.is_empty() {
        return None;
    }
    let lower_text = text.to_ascii_lowercase();
    let raw: Vec<&Model> = models
        .iter()
        .filter(|m| m.id.to_ascii_lowercase() == lower_text)
        .collect();
    let named: Option<&Model> = split_provider_prefix(text, models).and_then(|(provider, model_text)| {
        let lower_model = model_text.to_ascii_lowercase();
        models
            .iter()
            .find(|m| m.provider == provider && m.id.to_ascii_lowercase() == lower_model)
            .or_else(|| raw.iter().copied().find(|m| m.provider == provider))
    });
    if let Some(m) = named {
        return Some(m.clone());
    }
    if raw.is_empty() {
        return None;
    }
    let first_provider = &raw[0].provider;
    if raw.iter().all(|m| &m.provider == first_provider) {
        return Some(raw[0].clone());
    }
    Some(raw[0].clone())
}

fn split_thinking_suffix(text: &str) -> Option<(&str, ModelThinkingLevel)> {
    let idx = text.rfind(':')?;
    let level = parse_thinking_level_str(&text[idx + 1..])?;
    Some((&text[..idx], level))
}

fn glob_match_ci(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.to_ascii_lowercase().chars().collect();
    let t: Vec<char> = text.to_ascii_lowercase().chars().collect();
    glob_match_chars(&p, &t)
}

fn glob_match_chars(p: &[char], t: &[char]) -> bool {
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star_pi: Option<usize> = None;
    let mut star_ti = 0usize;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star_pi = Some(pi);
            pi += 1;
            star_ti = ti;
        } else if let Some(sp) = star_pi {
            pi = sp + 1;
            star_ti += 1;
            ti = star_ti;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

pub fn resolve_model_scope_from_models_local(patterns: &[String], models: &[Model]) -> ResolveModelScopeResult {
    let mut out = ResolveModelScopeResult::default();
    let add = |out: &mut ResolveModelScopeResult, model: &Model, thinking_level: Option<ModelThinkingLevel>| {
        if !out
            .scoped_models
            .iter()
            .any(|s| s.model.provider == model.provider && s.model.id == model.id)
        {
            out.scoped_models.push(ScopedModelItem {
                model: model.clone(),
                thinking_level: thinking_level.map(|lvl| model_thinking_to_str(lvl).to_string()),
            });
        }
    };

    for pattern in patterns {
        if pattern.contains('*') || pattern.contains('?') || pattern.contains('[') {
            let (glob_pattern, thinking_level) = match split_thinking_suffix(pattern) {
                Some((base, lvl)) => (base, Some(lvl)),
                None => (pattern.as_str(), None),
            };
            let matches: Vec<&Model> = if let Some(exact) = find_exact_model_reference_match_local(glob_pattern, models)
            {
                models
                    .iter()
                    .filter(|m| m.provider == exact.provider && m.id == exact.id)
                    .collect()
            } else {
                models
                    .iter()
                    .filter(|m| {
                        glob_match_ci(glob_pattern, &format!("{}/{}", m.provider, m.id))
                            || glob_match_ci(glob_pattern, &m.id)
                    })
                    .collect()
            };
            if matches.is_empty() {
                out.diagnostics.push(ModelScopeDiagnostic {
                    code: "no-match",
                    message: format!("No models match pattern \"{pattern}\""),
                    pattern: pattern.clone(),
                });
            }
            for m in matches {
                add(&mut out, m, thinking_level);
            }
            continue;
        }

        let (lookup_text, thinking_level) = match split_thinking_suffix(pattern) {
            Some((base, lvl)) => (base, Some(lvl)),
            None => (pattern.as_str(), None),
        };
        if let Some(exact) = find_exact_model_reference_match_local(lookup_text, models) {
            add(&mut out, &exact, thinking_level);
            continue;
        }
        let lower = lookup_text.to_ascii_lowercase();
        let fuzzy: Vec<&Model> = models
            .iter()
            .filter(|m| m.id.to_ascii_lowercase().contains(&lower) || m.name.to_ascii_lowercase().contains(&lower))
            .collect();
        if let Some(best) = fuzzy.first() {
            add(&mut out, best, thinking_level);
        } else {
            out.diagnostics.push(ModelScopeDiagnostic {
                code: "no-match",
                message: format!("No models match pattern \"{pattern}\""),
                pattern: pattern.clone(),
            });
        }
    }

    out
}

pub type SelectorFactory = Box<
    dyn FnOnce(
        Arc<dyn Fn() + Send + Sync>,
    ) -> (
        Arc<dyn Component>,
        Arc<dyn Component>,
        Option<Arc<dyn Fn() + Send + Sync>>,
    ),
>;

impl InteractiveMode {
    fn update_thinking_block_visibility(&self) {
        let (hide_thinking, assistants) = {
            let st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            let list: Vec<crate::modes::interactive::components::assistant_message::AssistantMessageComponent> =
                st.chat_trackers.iter().filter_map(|t| t.assistant.clone()).collect();
            (st.hide_thinking_block, list)
        };
        for comp in assistants {
            comp.set_hide_thinking_block(hide_thinking);
        }
        self.ui().request_render(None);
    }

    pub(crate) fn toggle_thinking_block_visibility(&self) {
        let next = {
            let mut st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            st.hide_thinking_block = !st.hide_thinking_block;
            st.hide_thinking_block
        };
        self.settings_manager().set_hide_thinking_block(next);
        self.update_thinking_block_visibility();
        let state = if next { "hidden" } else { "visible" };
        self.show_status(&format!("Thinking blocks: {state}"));
    }

    pub(crate) async fn handle_open_external_editor(&self) {
        let editor_cmd = self
            .settings_manager()
            .get_external_editor_command()
            .or_else(|| pi_js::env::var("VISUAL").filter(|s| !s.is_empty()))
            .or_else(|| pi_js::env::var("EDITOR").filter(|s| !s.is_empty()))
            .unwrap_or_default();
        let content = self.active_editor().get_expanded_text();
        self.ui().stop(None);
        let outcome = if editor_cmd.trim().is_empty() {
            Ok(ExternalEditorResult::Failed {
                message: "No external editor configured ($VISUAL or $EDITOR)".to_string(),
            })
        } else {
            edit_in_external_editor(ExternalEditorOptions {
                command: editor_cmd,
                content,
            })
            .await
        };
        self.ui().start();
        self.ui().request_render(Some(true));

        match outcome {
            Ok(ExternalEditorResult::Complete { content }) => {
                self.active_editor().set_text(&content);
            }
            Ok(ExternalEditorResult::Failed { message }) => {
                self.show_status(&message);
            }
            Err(err) => {
                self.show_status(&err.to_string());
            }
        }
    }

    // =========================================================================
    // UI Helpers
    // =========================================================================

    pub fn clear_editor(&self) {
        self.active_editor().set_text("");
        self.ui().request_render(None);
    }

    pub fn show_error(&self, error_message: &str) {
        let output_pad = self.inner.state.lock().unwrap_or_else(|e| e.into_inner()).output_pad;
        let msg = format!("Error: {error_message}");
        self.add_chat_component(Arc::new(Spacer::new(Some(1))), None, None, None);
        self.add_chat_component(
            Arc::new(ThemedText::new(
                Arc::new(move || theme().fg(ThemeColor::Error, &msg)),
                Some(output_pad),
                Some(0),
            )),
            None,
            None,
            None,
        );
        self.ui().request_render(None);
    }

    pub fn show_warning(&self, warning_message: &str) {
        let msg = format!("Warning: {warning_message}");
        self.add_chat_component(Arc::new(Spacer::new(Some(1))), None, None, None);
        self.add_chat_component(
            Arc::new(ThemedText::new(
                Arc::new(move || theme().fg(ThemeColor::Warning, &msg)),
                Some(1),
                Some(0),
            )),
            None,
            None,
            None,
        );
        self.ui().request_render(None);
    }

    pub fn show_new_version_notification(&self, release: &LatestPiRelease) {
        let version = release.version.clone();
        let update_instruction = move || {
            format!(
                "{}{}",
                theme().fg(ThemeColor::Muted, &format!("New version {version} is available. Run ")),
                theme().fg(ThemeColor::Accent, &format!("{APP_NAME_LOCAL} update")),
            )
        };
        let changelog_url = "https://pi.dev/changelog";
        let changelog_line = move || {
            let link_text = theme().fg(ThemeColor::Accent, changelog_url);
            let formatted_link = if get_capabilities().hyperlinks {
                hyperlink(&link_text, changelog_url)
            } else {
                link_text
            };
            format!("{}{formatted_link}", theme().fg(ThemeColor::Muted, "Changelog: "))
        };
        let note = release.note.as_deref().map(str::trim).filter(|s| !s.is_empty());

        let border_color_fn: Arc<dyn Fn(&str) -> String + Send + Sync> =
            Arc::new(|t: &str| theme().fg(ThemeColor::Warning, t));
        self.add_chat_component(Arc::new(Spacer::new(Some(1))), None, None, None);
        self.add_chat_component(
            Arc::new(DynamicBorder::new(Some(Arc::clone(&border_color_fn)))),
            None,
            None,
            None,
        );
        self.add_chat_component(
            Arc::new(ThemedText::new(
                Arc::new(move || {
                    format!(
                        "{}\n{}",
                        theme().bold(&theme().fg(ThemeColor::Warning, "Update Available")),
                        update_instruction()
                    )
                }),
                Some(1),
                Some(0),
            )),
            None,
            None,
            None,
        );
        if let Some(note_md) = note {
            self.add_chat_component(Arc::new(Spacer::new(Some(1))), None, None, None);
            let md = self.create_changelog_markdown(
                note_md,
                1,
                0,
                Some(DefaultTextStyle {
                    color: Some(Arc::new(|t: &str| theme().fg(ThemeColor::Muted, t))),
                    ..Default::default()
                }),
            );
            self.add_chat_component(md, None, None, None);
            self.add_chat_component(Arc::new(Spacer::new(Some(1))), None, None, None);
        }
        self.add_chat_component(
            Arc::new(ThemedText::new(Arc::new(changelog_line), Some(1), Some(0))),
            None,
            None,
            None,
        );
        self.add_chat_component(Arc::new(DynamicBorder::new(Some(border_color_fn))), None, None, None);
        self.ui().request_render(None);
    }

    pub fn show_package_update_notification(&self, packages: &[String]) {
        let package_lines = packages
            .iter()
            .map(|pkg| format!("- {pkg}"))
            .collect::<Vec<_>>()
            .join("\n");
        let border_color_fn: Arc<dyn Fn(&str) -> String + Send + Sync> =
            Arc::new(|t: &str| theme().fg(ThemeColor::Warning, t));
        self.add_chat_component(Arc::new(Spacer::new(Some(1))), None, None, None);
        self.add_chat_component(
            Arc::new(DynamicBorder::new(Some(Arc::clone(&border_color_fn)))),
            None,
            None,
            None,
        );
        self.add_chat_component(
            Arc::new(ThemedText::new(
                Arc::new(move || {
                    let update_instruction = format!(
                        "{}{}",
                        theme().fg(ThemeColor::Muted, "Package updates are available. Run "),
                        theme().fg(ThemeColor::Accent, &format!("{APP_NAME_LOCAL} update --extensions")),
                    );
                    format!(
                        "{}\n{update_instruction}\n{}\n{package_lines}",
                        theme().bold(&theme().fg(ThemeColor::Warning, "Package Updates Available")),
                        theme().fg(ThemeColor::Muted, "Packages:")
                    )
                }),
                Some(1),
                Some(0),
            )),
            None,
            None,
            None,
        );
        self.add_chat_component(Arc::new(DynamicBorder::new(Some(border_color_fn))), None, None, None);
        self.ui().request_render(None);
    }

    fn get_all_queued_messages(&self) -> (Vec<String>, Vec<String>) {
        let sess = self.session();
        let mut steering = sess.get_steering_messages();
        let mut follow_up = sess.get_follow_up_messages();
        let st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        for msg in &st.compaction_queued_messages {
            match msg.mode {
                CompactionQueueMode::Steer => steering.push(msg.text.clone()),
                CompactionQueueMode::FollowUp => follow_up.push(msg.text.clone()),
            }
        }
        (steering, follow_up)
    }

    pub(crate) fn clear_all_queues(&self) -> (Vec<String>, Vec<String>) {
        let cleared = self.session().clear_queue();
        let mut steering = cleared.steering;
        let mut follow_up = cleared.follow_up;
        let drained = {
            let mut st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            std::mem::take(&mut st.compaction_queued_messages)
        };
        for msg in drained {
            match msg.mode {
                CompactionQueueMode::Steer => steering.push(msg.text),
                CompactionQueueMode::FollowUp => follow_up.push(msg.text),
            }
        }
        (steering, follow_up)
    }

    pub(crate) fn update_pending_messages_display(&self) {
        self.clear_pending_container();
        let pending_bash = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pending_bash_components
            .clone();
        for (bash_comp, comp_arc) in pending_bash {
            let pad_fn: Arc<dyn Fn(i64) + Send + Sync> = Arc::new(move |p| bash_comp.set_output_pad(p));
            self.add_pending_component(comp_arc, None, Some(pad_fn));
        }

        let (steering_msgs, follow_up_msgs) = self.get_all_queued_messages();
        if !steering_msgs.is_empty() || !follow_up_msgs.is_empty() {
            self.add_pending_component(Arc::new(Spacer::new(Some(1))), None, None);
            for msg in steering_msgs {
                let text = theme().fg(ThemeColor::Dim, &format!("Steering: {msg}"));
                self.add_pending_component(Arc::new(TruncatedText::new(&text, Some(1), Some(0))), None, None);
            }
            for msg in follow_up_msgs {
                let text = theme().fg(ThemeColor::Dim, &format!("Follow-up: {msg}"));
                self.add_pending_component(Arc::new(TruncatedText::new(&text, Some(1), Some(0))), None, None);
            }
            let dequeue_hint = self.get_app_key_display("app.message.dequeue");
            let hint_text = theme().fg(
                ThemeColor::Dim,
                &format!("↳ {dequeue_hint} to edit all queued messages"),
            );
            self.add_pending_component(Arc::new(TruncatedText::new(&hint_text, Some(1), Some(0))), None, None);
        }
    }

    pub(crate) fn restore_queued_messages_to_editor(&self, abort: bool) -> usize {
        let (steering, follow_up) = self.clear_all_queues();
        let mut all_queued = steering;
        all_queued.extend(follow_up);
        if all_queued.is_empty() {
            self.update_pending_messages_display();
            if abort {
                let sess = self.session();
                tokio::spawn(async move { sess.abort().await });
            }
            return 0;
        }
        let queued_text = all_queued.join("\n\n");
        let current_text = self.active_editor().get_expanded_text();
        let combined = if current_text.trim().is_empty() {
            queued_text
        } else {
            format!("{queued_text}\n\n{current_text}")
        };
        self.active_editor().set_text(&combined);
        self.update_pending_messages_display();
        if abort {
            let sess = self.session();
            tokio::spawn(async move { sess.abort().await });
        }
        all_queued.len()
    }

    pub(crate) fn queue_compaction_message(&self, text: &str, mode: CompactionQueueMode) {
        {
            let mut st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            st.compaction_queued_messages.push(CompactionQueuedMessage {
                text: text.to_string(),
                mode,
            });
        }
        self.active_editor().add_to_history(text);
        self.active_editor().set_text("");
        self.update_pending_messages_display();
        self.show_status("Queued message for after compaction");
    }

    pub(crate) fn is_extension_command(&self, text: &str) -> bool {
        let Some(rest) = text.strip_prefix('/') else {
            return false;
        };
        let command_name = rest.split(' ').next().unwrap_or("");
        self.session().extension_runner().has_command(command_name)
    }

    pub(crate) async fn flush_compaction_queue(&self, will_retry: bool) {
        let queued_messages = {
            let mut st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if st.compaction_queued_messages.is_empty() {
                return;
            }
            std::mem::take(&mut st.compaction_queued_messages)
        };
        self.update_pending_messages_display();

        let restore_queue = |err_msg: &str| {
            let _ = self.session().clear_queue();
            self.inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .compaction_queued_messages = queued_messages.clone();
            self.update_pending_messages_display();
            let plural = if queued_messages.len() > 1 { "s" } else { "" };
            self.show_error(&format!("Failed to send queued message{plural}: {err_msg}"));
        };

        let sess = self.session();
        if will_retry {
            for msg in &queued_messages {
                let res = if self.is_extension_command(&msg.text) {
                    sess.prompt(&msg.text, None).await
                } else if msg.mode == CompactionQueueMode::FollowUp {
                    sess.follow_up(&msg.text).await
                } else {
                    sess.steer(&msg.text).await
                };
                if let Err(err) = res {
                    restore_queue(&err.to_string());
                    return;
                }
            }
            self.update_pending_messages_display();
            return;
        }

        let Some(first_prompt_idx) = queued_messages.iter().position(|m| !self.is_extension_command(&m.text)) else {
            for msg in &queued_messages {
                if let Err(err) = sess.prompt(&msg.text, None).await {
                    restore_queue(&err.to_string());
                    return;
                }
            }
            return;
        };

        for msg in &queued_messages[..first_prompt_idx] {
            if let Err(err) = sess.prompt(&msg.text, None).await {
                restore_queue(&err.to_string());
                return;
            }
        }

        let first_prompt = queued_messages[first_prompt_idx].clone();
        let rest = queued_messages[first_prompt_idx + 1..].to_vec();

        let m_err = self.clone();
        let queued_for_err = queued_messages.clone();
        let sess_prompt = Arc::clone(&sess);
        tokio::spawn(async move {
            if let Err(err) = sess_prompt
                .prompt(
                    &first_prompt.text,
                    Some(InteractivePromptOptions {
                        images: None,
                        streaming_behavior: Some(first_prompt.mode),
                    }),
                )
                .await
            {
                let _ = m_err.session().clear_queue();
                m_err
                    .inner
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .compaction_queued_messages = queued_for_err.clone();
                m_err.update_pending_messages_display();
                let plural = if queued_for_err.len() > 1 { "s" } else { "" };
                m_err.show_error(&format!("Failed to send queued message{plural}: {err}"));
            }
        });

        for msg in &rest {
            let res = if self.is_extension_command(&msg.text) {
                sess.prompt(&msg.text, None).await
            } else if msg.mode == CompactionQueueMode::FollowUp {
                sess.follow_up(&msg.text).await
            } else {
                sess.steer(&msg.text).await
            };
            if let Err(err) = res {
                restore_queue(&err.to_string());
                return;
            }
        }
        self.update_pending_messages_display();
    }

    pub(crate) fn flush_pending_bash_components(&self) {
        let pending = {
            let mut st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            std::mem::take(&mut st.pending_bash_components)
        };
        for (bash_comp, comp_arc) in pending {
            self.inner.pending_messages_container.remove_child(&comp_arc);
            {
                let mut st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                st.pending_trackers.retain(|t| !Arc::ptr_eq(&t.component, &comp_arc));
            }
            let pad_fn: Arc<dyn Fn(i64) + Send + Sync> = Arc::new(move |p| bash_comp.set_output_pad(p));
            self.add_chat_component(comp_arc, None, None, Some(pad_fn));
        }
    }

    // =========================================================================
    // Selectors
    // =========================================================================

    pub(crate) fn dispose_active_selector(&self) {
        let dispose = {
            let mut st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            st.active_selector_token = 0;
            st.active_selector_dispose.take()
        };
        if let Some(d) = dispose {
            d();
        }
    }

    pub(crate) fn show_selector(&self, create: SelectorFactory) {
        let token = {
            let mut st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            st.next_selector_token += 1;
            st.next_selector_token
        };
        let dispose_slot: Arc<std::sync::Mutex<Option<Arc<dyn Fn() + Send + Sync>>>> =
            Arc::new(std::sync::Mutex::new(None));
        let done: Arc<dyn Fn() + Send + Sync> = {
            let m = self.clone();
            let d_slot = Arc::clone(&dispose_slot);
            Arc::new(move || {
                if let Some(d) = d_slot.lock().unwrap_or_else(|e| e.into_inner()).take() {
                    d();
                }
                {
                    let mut st = m.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                    if st.active_selector_token != token {
                        return;
                    }
                    st.active_selector_token = 0;
                    st.active_selector_dispose = None;
                }
                m.inner.editor_container.clear();
                let ed_comp = m.editor_component();
                m.inner.editor_container.add_child(Arc::clone(&ed_comp));
                m.ui().set_focus(Some(ed_comp));
            })
        };

        let (component, focus, dispose) = create(done);
        *dispose_slot.lock().unwrap_or_else(|e| e.into_inner()) = dispose.clone();
        self.dispose_active_selector();
        {
            let mut st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            st.active_selector_token = token;
            st.active_selector_dispose = dispose;
        }
        self.inner.editor_container.clear();
        self.inner.editor_container.add_child(component);
        self.ui().set_focus(Some(focus));
        self.ui().request_render(None);
    }

    pub(crate) fn show_settings_selector(&self) {
        let m = self.clone();
        self.show_selector(Box::new(move |done| {
            let settings = m.settings_manager();
            let sess = m.session();
            let default_provider = settings.get_default_provider();
            let default_model_id = settings.get_default_model();
            let default_model = match (default_provider, default_model_id) {
                (Some(p), Some(id)) if !p.is_empty() && !id.is_empty() => format!("{p}/{id}"),
                _ => "not set".to_string(),
            };
            let hide_thinking = m
                .inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .hide_thinking_block;
            let scrollbar = settings.get_fullscreen_scrollbar();
            let config = SettingsConfig {
                auto_compact: sess.auto_compaction_enabled(),
                default_model,
                current_model: sess.model(),
                available_default_models: sess.model_runtime().get_available_snapshot(),
                show_images: settings.get_show_images(),
                image_width_cells: settings.get_image_width_cells().unwrap_or(80),
                auto_resize_images: settings.get_image_auto_resize(),
                block_images: settings.get_block_images(),
                enable_skill_commands: settings.get_enable_skill_commands(),
                steering_mode: sess.steering_mode(),
                follow_up_mode: sess.follow_up_mode(),
                transport: settings.get_transport(),
                http_idle_timeout_ms: settings.get_http_idle_timeout_ms(),
                cache_warming_mode: settings.get_cache_warming_mode(),
                thinking_level: settings
                    .get_default_thinking_level()
                    .unwrap_or(DEFAULT_THINKING_LEVEL_LOCAL),
                available_thinking_levels: THINKING_LEVEL_OPTIONS_LOCAL.to_vec(),
                model_thinking_levels: settings.get_all_model_thinking_levels(),
                current_theme: m
                    .inner
                    .theme_controller
                    .get_theme_selection()
                    .unwrap_or_else(|| SYSTEM_THEME_NAME.to_string()),
                terminal_theme: m.inner.theme_controller.get_terminal_theme(),
                available_themes: get_available_themes(),
                hide_thinking_block: hide_thinking,
                mermaid_rendering_mode: settings.get_mermaid_rendering_mode(),
                show_cache_miss_notices: settings.get_show_cache_miss_notices(),
                collapse_changelog: settings.get_collapse_changelog(),
                enable_install_telemetry: settings.get_enable_install_telemetry(),
                double_escape_action: settings.get_double_escape_action(),
                tree_filter_mode: settings.get_tree_filter_mode(),
                show_hardware_cursor: settings.get_show_hardware_cursor(),
                editor_padding_x: settings.get_editor_padding_x(),
                output_pad: settings.get_output_pad(),
                autocomplete_max_visible: settings.get_autocomplete_max_visible(),
                quiet_startup: settings.get_quiet_startup(),
                default_project_trust: settings.get_default_project_trust(),
                clear_on_shrink: settings.get_clear_on_shrink(),
                show_terminal_progress: settings.get_show_terminal_progress(),
                tui_mode: m.ui().mode().into(),
                fullscreen_exit_output: settings.get_fullscreen_exit_output(),
                fullscreen_scrollbar: scrollbar,
                fullscreen_copy_on_select: settings.get_fullscreen_copy_on_select(),
                fullscreen_wheel_scroll_lines: settings.get_fullscreen_wheel_scroll_lines(),
                warnings: settings.get_warnings(),
            };

            let settings_list_slot: Arc<std::sync::Mutex<Option<SettingsList>>> = Arc::new(std::sync::Mutex::new(None));

            let m_ac = m.clone();
            let m_img = m.clone();
            let m_iw = m.clone();
            let m_ar = m.clone();
            let m_bi = m.clone();
            let m_sc = m.clone();
            let m_st = m.clone();
            let m_fu = m.clone();
            let m_tr = m.clone();
            let m_ht = m.clone();
            let m_cw = m.clone();
            let m_mtc = m.clone();
            let m_mtr = m.clone();
            let m_th = m.clone();
            let m_tp = m.clone();
            let m_htb = m.clone();
            let m_mm = m.clone();
            let m_cmn = m.clone();
            let m_cc = m.clone();
            let m_it = m.clone();
            let m_dea = m.clone();
            let m_tfm = m.clone();
            let m_hc = m.clone();
            let m_epx = m.clone();
            let m_op = m.clone();
            let m_amv = m.clone();
            let m_qs = m.clone();
            let m_dpt = m.clone();
            let m_cos = m.clone();
            let m_stp = m.clone();
            let m_tm = m.clone();
            let sl_tm = Arc::clone(&settings_list_slot);
            let m_feo = m.clone();
            let m_fsb = m.clone();
            let m_fcs = m.clone();
            let m_fws = m.clone();
            let m_wn = m.clone();
            let m_can = m.clone();

            let callbacks = SettingsCallbacks {
                on_auto_compact_change: Some(Arc::new(move |enabled| {
                    m_ac.session().set_auto_compaction_enabled(enabled);
                    m_ac.inner.footer.set_auto_compact_enabled(enabled);
                })),
                on_show_images_change: Some(Arc::new(move |enabled| {
                    m_img.settings_manager().set_show_images(enabled);
                    let tools: Vec<_> = m_img
                        .inner
                        .state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .chat_trackers
                        .iter()
                        .filter_map(|t| t.tool_exec.clone())
                        .collect();
                    for tool in tools {
                        tool.set_show_images(enabled);
                    }
                })),
                on_image_width_cells_change: Some(Arc::new(move |width| {
                    m_iw.settings_manager().set_image_width_cells(Some(width));
                    let tools: Vec<_> = m_iw
                        .inner
                        .state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .chat_trackers
                        .iter()
                        .filter_map(|t| t.tool_exec.clone())
                        .collect();
                    for tool in tools {
                        tool.set_image_width_cells(width);
                    }
                })),
                on_auto_resize_images_change: Some(Arc::new(move |enabled| {
                    m_ar.settings_manager().set_image_auto_resize(enabled);
                })),
                on_block_images_change: Some(Arc::new(move |blocked| {
                    m_bi.settings_manager().set_block_images(blocked);
                })),
                on_enable_skill_commands_change: Some(Arc::new(move |enabled| {
                    m_sc.settings_manager().set_enable_skill_commands(enabled);
                    m_sc.setup_autocomplete_provider();
                })),
                on_steering_mode_change: Some(Arc::new(move |mode| {
                    m_st.session().set_steering_mode(mode);
                })),
                on_follow_up_mode_change: Some(Arc::new(move |mode| {
                    m_fu.session().set_follow_up_mode(mode);
                })),
                on_transport_change: Some(Arc::new(move |transport| {
                    m_tr.settings_manager().set_transport(transport);
                    m_tr.session().set_transport(transport);
                })),
                on_http_idle_timeout_ms_change: Some(Arc::new(move |timeout_ms| {
                    m_ht.settings_manager().set_http_idle_timeout_ms(timeout_ms);
                    // WIRE(@tui-w3): Call crate::core::http_dispatcher::configure_http_dispatcher(timeout_ms) once @mcp merges onto main.
                    m_ht.show_status(&format!(
                        "HTTP idle timeout: {}",
                        format_http_idle_timeout_ms_display(timeout_ms)
                    ));
                })),
                on_cache_warming_mode_change: Some(Arc::new(move |mode| {
                    m_cw.session().set_cache_warming_mode(mode);
                    m_cw.show_status(&format!("Cache warming: {}", mode.as_str()));
                })),
                on_model_thinking_level_change: Some(Arc::new(move |provider, model_id, level| {
                    m_mtc
                        .settings_manager()
                        .set_model_thinking_level(provider, model_id, level);
                    if let Some(current) = m_mtc.session().model()
                        && current.provider == provider
                        && current.id == model_id
                    {
                        let _ = m_mtc.session().set_thinking_level(level, false);
                        m_mtc.inner.footer.invalidate();
                        m_mtc.update_editor_border_color();
                    }
                })),
                on_model_thinking_level_remove: Some(Arc::new(move |provider, model_id| {
                    m_mtr.settings_manager().remove_model_thinking_level(provider, model_id);
                    if let Some(current) = m_mtr.session().model()
                        && current.provider == provider
                        && current.id == model_id
                    {
                        let global_default = m_mtr
                            .settings_manager()
                            .get_default_thinking_level()
                            .unwrap_or(DEFAULT_THINKING_LEVEL_LOCAL);
                        let _ = m_mtr.session().set_thinking_level(global_default, false);
                        m_mtr.inner.footer.invalidate();
                        m_mtr.update_editor_border_color();
                    }
                })),
                on_theme_change: Some(Arc::new(move |theme_setting| {
                    m_th.settings_manager().set_theme(theme_setting);
                    m_th.inner.theme_controller.set_theme_setting(theme_setting);
                })),
                on_theme_preview: Some(Arc::new(move |theme_name| {
                    m_tp.inner.theme_controller.preview(theme_name);
                })),
                on_hide_thinking_block_change: Some(Arc::new(move |hidden| {
                    m_htb
                        .inner
                        .state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .hide_thinking_block = hidden;
                    m_htb.settings_manager().set_hide_thinking_block(hidden);
                    m_htb.update_thinking_block_visibility();
                })),
                on_mermaid_rendering_mode_change: Some(Arc::new(move |mode| {
                    m_mm.settings_manager().set_mermaid_rendering_mode(mode);
                    m_mm.inner.chat_container.invalidate();
                    m_mm.ui().request_render(None);
                })),
                on_show_cache_miss_notices_change: Some(Arc::new(move |shown| {
                    m_cmn.settings_manager().set_show_cache_miss_notices(shown);
                    m_cmn.rebuild_chat_from_messages();
                })),
                on_collapse_changelog_change: Some(Arc::new(move |collapsed| {
                    m_cc.settings_manager().set_collapse_changelog(collapsed);
                })),
                on_enable_install_telemetry_change: Some(Arc::new(move |enabled| {
                    m_it.settings_manager().set_enable_install_telemetry(enabled);
                })),
                on_double_escape_action_change: Some(Arc::new(move |action| {
                    m_dea.settings_manager().set_double_escape_action(action);
                })),
                on_tree_filter_mode_change: Some(Arc::new(move |mode| {
                    m_tfm.settings_manager().set_tree_filter_mode(mode);
                })),
                on_show_hardware_cursor_change: Some(Arc::new(move |enabled| {
                    m_hc.settings_manager().set_show_hardware_cursor(enabled);
                    m_hc.ui().set_show_hardware_cursor(enabled);
                })),
                on_editor_padding_x_change: Some(Arc::new(move |padding| {
                    m_epx.settings_manager().set_editor_padding_x(padding);
                    m_epx.inner.default_editor.set_padding_x(padding as f64);
                    let active = m_epx.active_editor();
                    if !active.is_default(&m_epx.inner.default_editor) {
                        active.set_padding_x(padding as f64);
                    }
                })),
                on_output_pad_change: Some(Arc::new(move |padding| {
                    m_op.settings_manager().set_output_pad(padding);
                    let pad_fns: Vec<Arc<dyn Fn(i64) + Send + Sync>> = {
                        let mut st = m_op.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                        st.output_pad = padding;
                        st.chat_trackers
                            .iter()
                            .chain(st.pending_trackers.iter())
                            .filter_map(|t| t.set_output_pad.clone())
                            .collect()
                    };
                    for f in pad_fns {
                        f(padding);
                    }
                    m_op.ui().request_render(None);
                })),
                on_autocomplete_max_visible_change: Some(Arc::new(move |max_visible| {
                    m_amv.settings_manager().set_autocomplete_max_visible(max_visible);
                    m_amv
                        .inner
                        .default_editor
                        .set_autocomplete_max_visible(max_visible as f64);
                    let active = m_amv.active_editor();
                    if !active.is_default(&m_amv.inner.default_editor) {
                        active.set_autocomplete_max_visible(max_visible as f64);
                    }
                })),
                on_quiet_startup_change: Some(Arc::new(move |quiet| {
                    m_qs.settings_manager().set_quiet_startup(quiet);
                })),
                on_default_project_trust_change: Some(Arc::new(move |trust| {
                    m_dpt.settings_manager().set_default_project_trust(trust);
                })),
                on_clear_on_shrink_change: Some(Arc::new(move |enabled| {
                    m_cos.settings_manager().set_clear_on_shrink(enabled);
                    m_cos.ui().set_clear_on_shrink(enabled);
                    let has_active = m_cos
                        .inner
                        .state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .active_status_indicator
                        .is_some();
                    if !enabled && !has_active {
                        m_cos.inner.status_container.clear();
                    }
                })),
                on_show_terminal_progress_change: Some(Arc::new(move |enabled| {
                    m_stp.settings_manager().set_show_terminal_progress(enabled);
                })),
                on_tui_mode_change: Some(Arc::new(move |mode| {
                    if !m_tm.switch_tui_mode(mode, true, true) {
                        let current_mode = to_settings_tui_mode(m_tm.ui().mode());
                        if let Some(sl) = sl_tm.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
                            sl.update_value("tui-mode", current_mode.as_str());
                        }
                        m_tm.show_status("Close active overlays before changing TUI mode");
                        return;
                    }
                    m_tm.settings_manager().set_tui_mode(mode);
                    let has_active = m_tm
                        .inner
                        .state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .active_status_indicator
                        .is_some();
                    if !has_active {
                        m_tm.inner.status_container.clear();
                    }
                    m_tm.show_status(&format!("TUI mode: {}", mode.as_str()));
                })),
                on_fullscreen_exit_output_change: Some(Arc::new(move |output| {
                    m_feo.settings_manager().set_fullscreen_exit_output(output);
                })),
                on_fullscreen_scrollbar_change: Some(Arc::new(move |sb_mode| {
                    m_fsb.settings_manager().set_fullscreen_scrollbar(sb_mode);
                    m_fsb.apply_fullscreen_scrollbar_setting();
                })),
                on_fullscreen_copy_on_select_change: Some(Arc::new(move |enabled| {
                    m_fcs.settings_manager().set_fullscreen_copy_on_select(enabled);
                    if let Some(alt) = m_fcs
                        .inner
                        .state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .renderer
                        .as_alt_screen()
                    {
                        alt.set_copy_on_select(enabled);
                    }
                })),
                on_fullscreen_wheel_scroll_lines_change: Some(Arc::new(move |lines| {
                    m_fws.settings_manager().set_fullscreen_wheel_scroll_lines(lines);
                    if let Some(alt) = m_fws
                        .inner
                        .state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .renderer
                        .as_alt_screen()
                    {
                        alt.set_wheel_scroll_lines(lines);
                    }
                })),
                on_warnings_change: Some(Arc::new(move |warnings| {
                    m_wn.settings_manager().set_warnings(warnings);
                })),
                on_cancel: Some(Arc::new(move || {
                    done();
                    m_can.ui().request_render(None);
                })),
            };

            let selector = SettingsSelectorComponent::new(config, callbacks);
            let sl = selector.get_settings_list().clone();
            *settings_list_slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(sl.clone());
            (Arc::new(selector), Arc::new(sl), None)
        }));
    }

    pub(crate) fn handle_thinking_command(&self, search_term: Option<&str>) {
        let available_levels = self.session().get_available_thinking_levels();
        let Some(term) = search_term.filter(|s| !s.trim().is_empty()) else {
            self.show_thinking_selector();
            return;
        };
        let normalized = term.trim().to_ascii_lowercase();
        let matched = available_levels
            .iter()
            .copied()
            .find(|lvl| model_thinking_to_str(*lvl) == normalized);
        let Some(level) = matched else {
            let names: Vec<&str> = available_levels.iter().map(|l| model_thinking_to_str(*l)).collect();
            self.show_error(&format!(
                "Unknown thinking level \"{term}\". Available levels: {}.",
                names.join(", ")
            ));
            return;
        };
        self.select_thinking_level(level, false);
    }

    fn select_thinking_level(&self, level: ModelThinkingLevel, persist: bool) {
        match self.session().set_thinking_level(level, persist) {
            Ok(()) => {
                self.inner.footer.invalidate();
                self.update_editor_border_color();
                let name = model_thinking_to_str(level);
                self.show_status(&if persist {
                    format!("Default thinking level: {name}")
                } else {
                    format!("Thinking level: {name}")
                });
            }
            Err(err) => {
                self.show_error(&err.to_string());
            }
        }
    }

    pub(crate) fn show_thinking_selector(&self) {
        let m = self.clone();
        self.show_selector(Box::new(move |done| {
            let current = to_agent_thinking_level(m.session().model_thinking_level());
            let available: Vec<ThinkingLevel> = m
                .session()
                .get_available_thinking_levels()
                .into_iter()
                .map(to_agent_thinking_level)
                .collect();
            let default_lvl = m
                .settings_manager()
                .get_default_thinking_level()
                .unwrap_or(DEFAULT_THINKING_LEVEL_LOCAL);
            let m_sel = m.clone();
            let done_sel = Arc::clone(&done);
            let m_def = m.clone();
            let done_def = Arc::clone(&done);
            let m_can = m.clone();
            let selector = ThinkingSelectorComponent::new(
                current,
                available,
                Arc::new(move |lvl| {
                    m_sel.select_thinking_level(from_agent_thinking_level(lvl), false);
                    done_sel();
                }),
                Arc::new(move || {
                    done();
                    m_can.ui().request_render(None);
                }),
                Some(Arc::new(move |lvl| {
                    m_def.select_thinking_level(from_agent_thinking_level(lvl), true);
                    done_def();
                })),
                Some(to_agent_thinking_level(default_lvl)),
            );
            let comp: Arc<dyn Component> = Arc::new(selector);
            (Arc::clone(&comp), comp, None)
        }));
    }

    pub(crate) async fn handle_model_command(&self, search_term: Option<&str>) {
        let Some(term) = search_term.filter(|s| !s.trim().is_empty()) else {
            self.show_model_selector(None);
            return;
        };
        if let Some(model) = self.find_exact_model_match(term).await {
            match self.session().set_model(model.clone(), false).await {
                Ok(()) => {
                    self.inner.footer.invalidate();
                    self.update_editor_border_color();
                    self.show_status(&format!("Model: {}", model.id));
                    let m = self.clone();
                    tokio::spawn(async move {
                        m.maybe_warn_about_anthropic_subscription_auth(Some(&model)).await;
                    });
                }
                Err(err) => self.show_error(&err.to_string()),
            }
            return;
        }
        self.show_model_selector(Some(term));
    }

    async fn find_exact_model_match(&self, search_term: &str) -> Option<Model> {
        let scoped = self.session().scoped_models();
        let cached: Vec<Model> = if !scoped.is_empty() {
            scoped.into_iter().map(|s| s.model).collect()
        } else {
            self.session().model_runtime().get_available_snapshot()
        };
        if let Some(found) = find_exact_model_reference_match_local(search_term, &cached) {
            return Some(found);
        }
        if !self.session().scoped_models().is_empty() {
            return None;
        }

        self.show_status("Refreshing model catalogs…");
        let controller = Arc::new(AbortController::new());
        let signal = controller.signal();
        let timed_out = Arc::new(AtomicBool::new(false));
        let ctrl_to = Arc::clone(&controller);
        let to_flag = Arc::clone(&timed_out);
        let timeout = pi_js::time::set_timeout(15_000, move || {
            to_flag.store(true, Ordering::SeqCst);
            ctrl_to.abort(None);
        });
        let res = refresh_model_catalogs(self.session().model_runtime(), signal).await;
        timeout.clear();

        match res {
            Ok(refresh_res) => {
                if refresh_res.aborted && timed_out.load(Ordering::SeqCst) {
                    self.show_warning("Model refresh timed out; searching cached models.");
                } else if !refresh_res.errors.is_empty() {
                    let keys: Vec<String> = refresh_res.errors.keys().cloned().collect();
                    self.show_warning(&format!(
                        "Could not refresh {}; searching cached models.",
                        keys.join(", ")
                    ));
                }
            }
            Err(err) => {
                if timed_out.load(Ordering::SeqCst) {
                    self.show_warning("Model refresh timed out; searching cached models.");
                } else {
                    self.show_warning(&format!("Could not refresh model catalogs: {err}"));
                }
            }
        }
        let refreshed = self.session().model_runtime().get_available_snapshot();
        find_exact_model_reference_match_local(search_term, &refreshed)
    }

    pub(crate) fn update_available_provider_count(&self) {
        let scoped = self.session().scoped_models();
        let models = if !scoped.is_empty() {
            scoped.into_iter().map(|s| s.model).collect()
        } else {
            self.session().model_runtime().get_available_snapshot()
        };
        let unique: IndexSet<ProviderId> = models.into_iter().map(|m| m.provider).collect();
        self.inner
            .footer_data_provider
            .set_available_provider_count(unique.len());
    }

    pub(crate) async fn maybe_warn_about_anthropic_subscription_auth(&self, model: Option<&Model>) {
        if self.settings_manager().get_warnings().anthropic_extra_usage == Some(false) {
            return;
        }
        if self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .anthropic_subscription_warning_shown
        {
            return;
        }
        let session_model = if model.is_none() { self.session().model() } else { None };
        let target_model = model.or(session_model.as_ref());
        let Some(model) = target_model else {
            return;
        };
        if model.provider != "anthropic" {
            return;
        }

        let runtime = self.session().model_runtime();
        if runtime.check_auth_type("anthropic").await == Some(pi_ai::auth::types::AuthType::OAuth) {
            self.inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .anthropic_subscription_warning_shown = true;
            self.show_warning(ANTHROPIC_SUBSCRIPTION_AUTH_WARNING);
            return;
        }
        let api_key = runtime.get_api_key(&model.provider).await;
        if is_anthropic_subscription_auth_key(api_key.as_deref()) {
            self.inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .anthropic_subscription_warning_shown = true;
            self.show_warning(ANTHROPIC_SUBSCRIPTION_AUTH_WARNING);
        }
    }

    pub(crate) fn maybe_save_implicit_project_trust_after_reload(&self) -> bool {
        let cwd = self.session_manager().get_cwd();
        let matches_auto_trust = {
            let st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            st.auto_trust_on_reload_cwd.as_deref() == Some(cwd.as_str())
        };
        if !matches_auto_trust {
            return false;
        }
        if !self.settings_manager().is_project_trusted() || !has_trust_requiring_project_resources_local(&cwd) {
            return false;
        }
        let trust_store = ProjectTrustStoreLocal::new(&self.inner.runtime_host.agent_dir());
        if trust_store.get(&cwd).is_some() {
            self.inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .auto_trust_on_reload_cwd = None;
            return false;
        }
        match trust_store.set(&cwd, Some(true)) {
            Ok(()) => {
                self.inner
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .auto_trust_on_reload_cwd = None;
                true
            }
            Err(err) => {
                self.show_warning(&format!("Could not save project trust after reload: {err}"));
                false
            }
        }
    }

    pub(crate) fn show_trust_selector(&self) {
        let cwd = self.session_manager().get_cwd();
        let agent_dir = self.inner.runtime_host.agent_dir();
        let trust_store = ProjectTrustStoreLocal::new(&agent_dir);
        let saved_decision = trust_store.get_entry(&cwd);
        let project_trusted = self.settings_manager().is_project_trusted();
        let m = self.clone();
        self.show_selector(Box::new(move |done| {
            let m_sel = m.clone();
            let done_sel = Arc::clone(&done);
            let m_can = m.clone();
            let agent_dir_sel = agent_dir.clone();
            let selector = TrustSelectorComponent::new(TrustSelectorOptions {
                cwd,
                saved_decision,
                project_trusted,
                on_select: Arc::new(move |selection: TrustSelection| {
                    let store = ProjectTrustStoreLocal::new(&agent_dir_sel);
                    if let Err(err) = store.set_many(&selection.updates) {
                        m_sel.show_error(&format!("Failed to save trust decision: {err}"));
                    }
                    done_sel();
                    let label = if selection.trusted { "trusted" } else { "untrusted" };
                    m_sel.show_status(&format!(
                        "Saved trust decision: {label}. Restart {APP_NAME_LOCAL} for this to take effect."
                    ));
                }),
                on_cancel: Arc::new(move || {
                    done();
                    m_can.ui().request_render(None);
                }),
            });
            let comp: Arc<dyn Component> = Arc::new(selector);
            (Arc::clone(&comp), comp, None)
        }));
    }

    pub(crate) fn show_model_selector(&self, initial_search_input: Option<&str>) {
        let initial_search = initial_search_input.map(str::to_string);
        let m = self.clone();
        self.show_selector(Box::new(move |done| {
            let select_model = {
                let m_outer = m.clone();
                let done_outer = Arc::clone(&done);
                move |model: Model, persist: bool| {
                    let m2 = m_outer.clone();
                    let d2 = Arc::clone(&done_outer);
                    tokio::spawn(async move {
                        match m2.session().set_model(model.clone(), persist).await {
                            Ok(()) => {
                                m2.update_available_provider_count();
                                m2.inner.footer.invalidate();
                                m2.update_editor_border_color();
                                d2();
                                m2.show_status(&if persist {
                                    format!("Default model: {}/{}", model.provider, model.id)
                                } else {
                                    format!("Model: {}", model.id)
                                });
                                m2.maybe_warn_about_anthropic_subscription_auth(Some(&model)).await;
                            }
                            Err(err) => {
                                d2();
                                m2.show_error(&err.to_string());
                            }
                        }
                    });
                }
            };

            let settings = m.settings_manager();
            let default_ref = match (settings.get_default_provider(), settings.get_default_model()) {
                (Some(provider), Some(id)) if !provider.is_empty() && !id.is_empty() => {
                    Some(DefaultModelReference { provider, id })
                }
                _ => None,
            };
            let runtime_bridge: Arc<dyn ModelCatalogRuntime> =
                Arc::new(ModelCatalogBridge(m.session().model_runtime()));
            let m_can = m.clone();
            let d_can = Arc::clone(&done);
            let s1 = select_model.clone();
            let s2 = select_model;
            let selector = ModelSelectorComponent::new(
                m.ui(),
                m.session().model(),
                runtime_bridge,
                m.session().scoped_models(),
                Arc::new(move |model| s1(model, false)),
                Arc::new(move || {
                    d_can();
                    m_can.ui().request_render(None);
                }),
                initial_search.as_deref(),
                Some(Arc::new(move |model| s2(model, true))),
                default_ref,
            );
            let comp: Arc<dyn Component> = Arc::new(selector.clone());
            let disp_selector = selector;
            let dispose: Arc<dyn Fn() + Send + Sync> = Arc::new(move || disp_selector.dispose());
            (Arc::clone(&comp), comp, Some(dispose))
        }));
    }

    pub(crate) fn show_models_selector(&self) {
        let initial_models = self.session().model_runtime().get_available_snapshot();
        let initial_model_ids: IndexSet<String> = initial_models
            .iter()
            .map(|model| format!("{}/{}", model.provider, model.id))
            .collect();
        let configured_patterns = self.settings_manager().get_enabled_models();
        let session_scoped = self.session().scoped_models();
        let had_session_scoped = !session_scoped.is_empty();

        let compute_configured_ids = {
            let configured_patterns = configured_patterns.clone();
            move |models: &[Model]| -> Option<Vec<String>> {
                let patterns = configured_patterns.as_ref()?.as_slice();
                if patterns.is_empty() {
                    return None;
                }
                let resolved = resolve_model_scope_from_models_local(patterns, models);
                let mut ids: Vec<String> = resolved
                    .scoped_models
                    .iter()
                    .map(|s| format!("{}/{}", s.model.provider, s.model.id))
                    .collect();
                for diag in resolved.diagnostics {
                    if diag.code == "no-match" && !ids.contains(&diag.pattern) {
                        ids.push(diag.pattern);
                    }
                }
                Some(ids)
            }
        };

        let initial_enabled_ids = if !session_scoped.is_empty() {
            Some(
                session_scoped
                    .iter()
                    .map(|s| format!("{}/{}", s.model.provider, s.model.id))
                    .collect(),
            )
        } else {
            compute_configured_ids(&initial_models)
        };

        let shared_models = Arc::new(std::sync::Mutex::new(initial_models.clone()));
        let shared_ids = Arc::new(std::sync::Mutex::new(initial_model_ids));
        let shared_enabled = Arc::new(std::sync::Mutex::new(initial_enabled_ids.clone()));
        let selection_changed = Arc::new(AtomicBool::new(false));

        let m_upd = self.clone();
        let sh_m_upd = Arc::clone(&shared_models);
        let sh_i_upd = Arc::clone(&shared_ids);
        let sh_e_upd = Arc::clone(&shared_enabled);
        let update_session_models = Arc::new(move |enabled_ids: Option<Vec<String>>| {
            *sh_e_upd.lock().unwrap_or_else(|e| e.into_inner()) = enabled_ids.clone();
            let models_snap = sh_m_upd.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let ids_snap = sh_i_upd.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let has_enabled = enabled_ids
                .as_ref()
                .is_some_and(|ids| ids.iter().any(|id| ids_snap.contains(id)));
            let all_enabled = enabled_ids
                .as_ref()
                .is_some_and(|ids| ids_snap.iter().all(|id| ids.contains(id)));
            if let Some(ref ids) = enabled_ids
                && has_enabled
                && !all_enabled
            {
                let resolved = resolve_model_scope_from_models_local(ids, &models_snap);
                m_upd.session().set_scoped_models(resolved.scoped_models);
            } else {
                m_upd.session().set_scoped_models(Vec::new());
            }
            m_upd.update_available_provider_count();
            m_upd.ui().request_render(None);
        });

        let m = self.clone();
        self.show_selector(Box::new(move |done| {
            let disposed = Arc::new(AtomicBool::new(false));
            let timed_out = Arc::new(AtomicBool::new(false));
            let controller = Arc::new(AbortController::new());
            let ctrl_to = Arc::clone(&controller);
            let to_flag = Arc::clone(&timed_out);
            let timeout = pi_js::time::set_timeout(15_000, move || {
                to_flag.store(true, Ordering::SeqCst);
                ctrl_to.abort(None);
            });

            let sel_chg_cb = Arc::clone(&selection_changed);
            let upd_cb = Arc::clone(&update_session_models);
            let m_persist = m.clone();
            let sh_m_per = Arc::clone(&shared_models);
            let sh_i_per = Arc::clone(&shared_ids);
            let m_can = m.clone();
            let d_can = Arc::clone(&done);

            let selector = ScopedModelsSelectorComponent::new(
                ModelsConfig {
                    all_models: initial_models,
                    enabled_model_ids: initial_enabled_ids,
                    refresh_status: Some("Refreshing model catalogs…".to_string()),
                },
                ModelsCallbacks {
                    on_change: Arc::new(move |enabled| {
                        sel_chg_cb.store(true, Ordering::SeqCst);
                        upd_cb(enabled);
                    }),
                    on_persist: Arc::new(move |enabled| {
                        let m_len = sh_m_per.lock().unwrap_or_else(|e| e.into_inner()).len();
                        let ids_snap = sh_i_per.lock().unwrap_or_else(|e| e.into_inner()).clone();
                        let all_enabled = enabled
                            .as_ref()
                            .is_some_and(|list| list.len() == m_len && list.iter().all(|id| ids_snap.contains(id)));
                        let new_patterns = match enabled {
                            Some(list) if !all_enabled => Some(list),
                            _ => None,
                        };
                        m_persist.settings_manager().set_enabled_models(new_patterns);
                        m_persist.show_status("Model selection saved to settings");
                    }),
                    on_cancel: Arc::new(move || {
                        d_can();
                        m_can.ui().request_render(None);
                    }),
                },
            );

            let m_bg = m.clone();
            let sel_bg = selector.clone();
            let disp_bg = Arc::clone(&disposed);
            let to_bg = Arc::clone(&timed_out);
            let sh_m_bg = Arc::clone(&shared_models);
            let sh_i_bg = Arc::clone(&shared_ids);
            let sh_e_bg = Arc::clone(&shared_enabled);
            let sel_chg_bg = Arc::clone(&selection_changed);
            let upd_bg = Arc::clone(&update_session_models);
            let signal = controller.signal();
            let runtime = m.session().model_runtime();
            let timeout_for_bg = timeout.clone();

            tokio::spawn(async move {
                let res = refresh_model_catalogs(runtime.clone(), signal).await;
                timeout_for_bg.clear();
                if disp_bg.load(Ordering::SeqCst) {
                    return;
                }
                match res {
                    Ok(refresh_res) => {
                        let refreshed_models = runtime.get_available_snapshot();
                        let refreshed_ids: IndexSet<String> = refreshed_models
                            .iter()
                            .map(|model| format!("{}/{}", model.provider, model.id))
                            .collect();
                        *sh_m_bg.lock().unwrap_or_else(|e| e.into_inner()) = refreshed_models.clone();
                        *sh_i_bg.lock().unwrap_or_else(|e| e.into_inner()) = refreshed_ids;
                        if !sel_chg_bg.load(Ordering::SeqCst) && !had_session_scoped {
                            let new_enabled = compute_configured_ids(&refreshed_models);
                            *sh_e_bg.lock().unwrap_or_else(|e| e.into_inner()) = new_enabled.clone();
                            sel_bg.update_models(&refreshed_models, Some(new_enabled.as_deref()));
                        } else {
                            sel_bg.update_models(&refreshed_models, None);
                        }
                        let curr_enabled = sh_e_bg.lock().unwrap_or_else(|e| e.into_inner()).clone();
                        if curr_enabled.is_some() {
                            upd_bg(curr_enabled);
                        }
                        if refresh_res.aborted && to_bg.load(Ordering::SeqCst) {
                            sel_bg.set_refresh_status(
                                "Model refresh timed out; showing cached models.",
                                RefreshStatusKind::Warning,
                            );
                        } else if !refresh_res.errors.is_empty() {
                            let keys: Vec<String> = refresh_res.errors.keys().cloned().collect();
                            sel_bg.set_refresh_status(
                                &format!("Could not refresh {}; showing cached models.", keys.join(", ")),
                                RefreshStatusKind::Warning,
                            );
                        } else {
                            sel_bg.set_refresh_status("Model catalogs refreshed.", RefreshStatusKind::Success);
                        }
                        m_bg.ui().request_render(None);
                    }
                    Err(err) => {
                        if to_bg.load(Ordering::SeqCst) {
                            sel_bg.set_refresh_status(
                                "Model refresh timed out; showing cached models.",
                                RefreshStatusKind::Warning,
                            );
                        } else {
                            sel_bg.set_refresh_status(
                                &format!("Could not refresh model catalogs: {err}"),
                                RefreshStatusKind::Warning,
                            );
                        }
                        m_bg.ui().request_render(None);
                    }
                }
            });

            let comp: Arc<dyn Component> = Arc::new(selector);
            let timeout_disp = timeout;
            let dispose: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
                disposed.store(true, Ordering::SeqCst);
                timeout_disp.clear();
                controller.abort(None);
            });
            (Arc::clone(&comp), comp, Some(dispose))
        }));
    }
}
