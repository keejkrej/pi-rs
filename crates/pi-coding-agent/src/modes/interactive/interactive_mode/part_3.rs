//! Port of packages/coding-agent/src/modes/interactive/interactive-mode.ts#L1356-L2987

use std::sync::Arc;

use indexmap::IndexMap;
use pi_js::{BoxFuture, Result, Unsubscribe};
use pi_tui::{Component, Container, Markdown, MarkdownTheme, Spacer, Text, matches_key, set_capability_overrides};
use serde_json::Value;

use crate::modes::interactive::components::extension_editor::{ExtensionEditorComponent, ExtensionEditorOptions};
use crate::modes::interactive::components::extension_input::{ExtensionInputComponent, ExtensionInputOptions};
use crate::modes::interactive::components::extension_selector::{ExtensionSelectorComponent, ExtensionSelectorOptions};
use crate::modes::interactive::components::keybinding_hints::{KeyTextFormatOptions, format_key_text, key_text};
use crate::modes::interactive::components::markdown_transform::MarkdownTransformer;
use crate::modes::interactive::components::settings_selector::{QuietStartup, TuiMode as SettingsTuiMode};
use crate::modes::interactive::components::status_indicator::StatusIndicator;
use crate::modes::interactive::components::themed_text::ThemedText;
use crate::modes::interactive::components::tool_execution::ToolExecutionComponent;
use crate::modes::interactive::theme::theme::{
    Theme, ThemeColor, get_available_themes_with_paths, get_markdown_theme, get_theme_by_name, set_registered_themes,
    stop_theme_watcher, theme,
};
use crate::modes::interactive::theme::theme_controller::ThemeResult;

use super::*;

pub const MAX_WIDGET_LINES: usize = 10;

pub fn get_cwd_relative_path_local(file_path: &str, cwd: &str) -> Option<String> {
    let resolved_cwd = pi_js::path::resolve(&[cwd]);
    let resolved_path = pi_js::path::resolve(&[&resolved_cwd, file_path]);
    let rel = pi_js::path::relative(&resolved_cwd, &resolved_path);
    let sep = pi_js::path::sep();
    let inside =
        rel.is_empty() || (rel != ".." && !rel.starts_with(&format!("..{sep}")) && !pi_js::path::is_absolute(&rel));
    if inside {
        Some(if rel.is_empty() { ".".to_string() } else { rel })
    } else {
        None
    }
}

pub fn shorten_home_path_local(path: &str) -> String {
    let home = pi_js::env::home_dir();
    if path.is_empty() || home.is_empty() {
        return path.to_string();
    }
    if path == home {
        return "~".to_string();
    }
    if let Some(rest) = path.strip_prefix(&home)
        && (rest.starts_with('/') || rest.starts_with('\\'))
    {
        return format!("~{rest}");
    }
    path.to_string()
}

pub struct ScopeGroup {
    pub scope: &'static str,
    pub paths: Vec<(String, Option<SourceInfo>)>,
    pub packages: IndexMap<String, Vec<(String, Option<SourceInfo>)>>,
}

struct InteractiveExtensionUiContextImpl {
    mode: InteractiveMode,
}

impl ExtensionUiContext for InteractiveExtensionUiContextImpl {
    fn select(
        &self,
        title: String,
        options: Vec<String>,
        opts: Option<ExtensionUIDialogOptions>,
    ) -> BoxFuture<Option<String>> {
        let m = self.mode.clone();
        Box::pin(async move { m.show_extension_selector(&title, options, opts, None).await })
    }

    fn confirm(&self, title: String, message: String, opts: Option<ExtensionUIDialogOptions>) -> BoxFuture<bool> {
        let m = self.mode.clone();
        Box::pin(async move { m.show_extension_confirm(&title, &message, opts).await })
    }

    fn input(
        &self,
        title: String,
        placeholder: Option<String>,
        opts: Option<ExtensionUIDialogOptions>,
    ) -> BoxFuture<Option<String>> {
        let m = self.mode.clone();
        Box::pin(async move { m.show_extension_input(&title, placeholder.as_deref(), opts).await })
    }

    fn notify(&self, message: &str, kind: Option<NotifyKind>) {
        self.mode.show_extension_notify(message, kind);
    }

    fn on_terminal_input(&self, handler: pi_tui::tui::TuiInputListener) -> Unsubscribe {
        self.mode.add_extension_terminal_input_listener(handler)
    }

    fn set_status(&self, key: &str, text: Option<&str>) {
        self.mode.set_extension_status(key, text);
    }

    fn set_working_message(&self, message: Option<&str>) {
        let (default_msg, active) = {
            let mut st = self.mode.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            st.working_message = message.map(str::to_string);
            (
                "Working".to_string(),
                st.active_status_indicator
                    .as_ref()
                    .filter(|s| {
                        s.kind()
                            == crate::modes::interactive::components::status_indicator::StatusIndicatorKind::Working
                    })
                    .cloned(),
            )
        };
        if let Some(ind) = active {
            ind.set_message(message.unwrap_or(&default_msg));
        }
    }

    fn set_working_visible(&self, visible: bool) {
        self.mode.set_working_visible(visible);
    }

    fn set_working_indicator(&self, options: Option<pi_tui::LoaderIndicatorOptions>) {
        self.mode.set_working_indicator(options);
    }

    fn set_hidden_thinking_label(&self, label: Option<&str>) {
        self.mode.set_hidden_thinking_label(label);
    }

    fn set_widget(&self, key: &str, content: Option<WidgetContent>, options: Option<ExtensionWidgetOptions>) {
        self.mode.set_extension_widget(key, content, options);
    }

    fn set_footer(&self, factory: Option<FooterFactory>) {
        self.mode.set_extension_footer(factory);
    }

    fn set_header(&self, factory: Option<HeaderFactory>) {
        self.mode.set_extension_header(factory);
    }

    fn set_title(&self, title: &str) {
        self.mode.ui().terminal().set_title(title);
    }

    fn custom(&self, factory: CustomUiFactory, options: Option<ExtensionCustomOptions>) -> BoxFuture<Result<Value>> {
        let m = self.mode.clone();
        Box::pin(async move { m.show_extension_custom(factory, options).await })
    }

    fn paste_to_editor(&self, text: &str) {
        let editor = self
            .mode
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .editor
            .clone();
        editor.handle_input(&format!("\x1b[200~{text}\x1b[201~"));
    }

    fn set_editor_text(&self, text: &str) {
        let editor = self
            .mode
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .editor
            .clone();
        editor.set_text(text);
    }

    fn get_editor_text(&self) -> String {
        let editor = self
            .mode
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .editor
            .clone();
        editor.get_expanded_text()
    }

    fn editor(&self, title: String, prefill: Option<String>) -> BoxFuture<Option<String>> {
        let m = self.mode.clone();
        Box::pin(async move { m.show_extension_editor(&title, prefill.as_deref()).await })
    }

    fn add_autocomplete_provider(&self, factory: AutocompleteProviderFactory) {
        {
            let mut st = self.mode.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            st.autocomplete_provider_wrappers.push(factory);
        }
        self.mode.setup_autocomplete_provider();
    }

    fn set_editor_component(&self, factory: Option<EditorFactory>) {
        self.mode.set_custom_editor_component(factory);
    }

    fn get_editor_component(&self) -> Option<EditorFactory> {
        self.mode
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .editor_component_factory
            .clone()
    }

    fn theme(&self) -> Theme {
        theme()
    }

    fn get_all_themes(&self) -> Vec<ThemeInfo> {
        get_available_themes_with_paths()
    }

    fn get_theme(&self, name: &str) -> Option<Theme> {
        get_theme_by_name(name)
    }

    fn set_theme_name(&self, name: &str) -> ThemeResult {
        let result = self.mode.inner.theme_controller.set_theme_name(name, false);
        if result.success && self.mode.settings_manager().get_theme().as_deref() != Some(name) {
            self.mode.settings_manager().set_theme(name);
        }
        result
    }

    fn set_theme_instance(&self, theme_instance: Theme) -> ThemeResult {
        self.mode.inner.theme_controller.set_theme_instance(theme_instance)
    }

    fn get_tools_expanded(&self) -> bool {
        self.mode
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .tool_output_expanded
    }

    fn set_tools_expanded(&self, expanded: bool) {
        self.mode.set_tools_expanded(expanded);
    }
}

struct ToolExpandableAdapter(ToolExecutionComponent);

impl Expandable for ToolExpandableAdapter {
    fn set_expanded(&self, expanded: bool) {
        self.0.set_expanded(expanded);
    }
}

impl InteractiveMode {
    pub(crate) fn editor_component(&self) -> Arc<dyn Component> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .editor
            .as_component()
    }

    pub(crate) fn active_editor(&self) -> ActiveEditor {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .editor
            .clone()
    }

    pub(crate) fn add_chat_component(
        &self,
        component: Arc<dyn Component>,
        expandable: Option<Arc<dyn Expandable>>,
        assistant: Option<crate::modes::interactive::components::assistant_message::AssistantMessageComponent>,
        set_output_pad: Option<Arc<dyn Fn(i64) + Send + Sync>>,
    ) {
        self.add_chat_component_full(component, expandable, assistant, None, set_output_pad);
    }

    pub(crate) fn add_tool_chat_component(&self, tool_comp: ToolExecutionComponent) {
        let comp_arc: Arc<dyn Component> = Arc::new(tool_comp.clone());
        let tool_for_pad = tool_comp.clone();
        let pad_fn: Arc<dyn Fn(i64) + Send + Sync> = Arc::new(move |pad| tool_for_pad.set_output_pad(pad));
        let tool_for_exp = tool_comp.clone();
        let exp_fn: Arc<dyn Expandable> = Arc::new(ToolExpandableAdapter(tool_for_exp));
        self.add_chat_component_full(comp_arc, Some(exp_fn), None, Some(tool_comp), Some(pad_fn));
    }

    fn add_chat_component_full(
        &self,
        component: Arc<dyn Component>,
        expandable: Option<Arc<dyn Expandable>>,
        assistant: Option<crate::modes::interactive::components::assistant_message::AssistantMessageComponent>,
        tool_exec: Option<ToolExecutionComponent>,
        set_output_pad: Option<Arc<dyn Fn(i64) + Send + Sync>>,
    ) {
        self.inner.chat_container.add_child(Arc::clone(&component));
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .chat_trackers
            .push(ChildTrackers {
                component,
                expandable,
                assistant,
                tool_exec,
                set_output_pad,
            });
    }

    pub(crate) fn clear_chat_container(&self) {
        self.inner.chat_container.clear();
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .chat_trackers
            .clear();
    }

    pub(crate) fn remove_chat_component(&self, component: &Arc<dyn Component>) {
        self.inner.chat_container.remove_child(component);
        let mut st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        st.chat_trackers.retain(|t| !Arc::ptr_eq(&t.component, component));
    }

    pub(crate) fn insert_chat_component_before(
        &self,
        component: Arc<dyn Component>,
        before: &Arc<dyn Component>,
        expandable: Option<Arc<dyn Expandable>>,
        set_output_pad: Option<Arc<dyn Fn(i64) + Send + Sync>>,
    ) {
        let mut children = self.inner.chat_container.children();
        if let Some(idx) = children.iter().position(|c| Arc::ptr_eq(c, before)) {
            children.insert(idx, Arc::clone(&component));
            self.inner.chat_container.set_children(children);
            let mut st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            let tracker = ChildTrackers {
                component,
                expandable,
                assistant: None,
                tool_exec: None,
                set_output_pad,
            };
            if let Some(t_idx) = st.chat_trackers.iter().position(|t| Arc::ptr_eq(&t.component, before)) {
                st.chat_trackers.insert(t_idx, tracker);
            } else {
                st.chat_trackers.push(tracker);
            }
        } else {
            self.add_chat_component(component, expandable, None, set_output_pad);
        }
    }

    pub(crate) fn add_resource_component(
        &self,
        component: Arc<dyn Component>,
        expandable: Option<Arc<dyn Expandable>>,
    ) {
        self.inner.loaded_resources_container.add_child(Arc::clone(&component));
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .resource_trackers
            .push(ChildTrackers {
                component,
                expandable,
                assistant: None,
                tool_exec: None,
                set_output_pad: None,
            });
    }

    pub(crate) fn clear_resource_container(&self) {
        self.inner.loaded_resources_container.clear();
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .resource_trackers
            .clear();
    }

    pub(crate) fn add_pending_component(
        &self,
        component: Arc<dyn Component>,
        expandable: Option<Arc<dyn Expandable>>,
        set_output_pad: Option<Arc<dyn Fn(i64) + Send + Sync>>,
    ) {
        self.inner.pending_messages_container.add_child(Arc::clone(&component));
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pending_trackers
            .push(ChildTrackers {
                component,
                expandable,
                assistant: None,
                tool_exec: None,
                set_output_pad,
            });
    }

    pub(crate) fn clear_pending_container(&self) {
        self.inner.pending_messages_container.clear();
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pending_trackers
            .clear();
    }

    pub(crate) fn get_changelog_for_display(&self) -> Option<String> {
        if !self.session().messages().is_empty() {
            return None;
        }
        let settings = self.settings_manager();
        if settings.get_last_changelog_version().is_none() {
            settings.set_last_changelog_version(VERSION_LOCAL);
            self.report_install_telemetry(VERSION_LOCAL);
            return None;
        }
        // WIRE(@tui-w3): wire crate::utils::changelog::{get_changelog_path, parse_changelog, get_new_entries, normalize_changelog_links} once @mcp lands.
        None
    }

    pub(crate) fn get_extension_changelog_updates(&self) -> Vec<ExtensionChangelogUpdate> {
        if !self.session().messages().is_empty() {
            return Vec::new();
        }
        let mut updates = Vec::new();
        let mut seen_keys = indexmap::IndexSet::new();
        let settings = self.settings_manager();
        for ext in self.session().resource_loader().get_extensions().extensions {
            let Some(ref info) = ext.source_info else {
                continue;
            };
            if info.origin != "package" {
                continue;
            }
            let Some(ref base_dir) = info.base_dir else {
                continue;
            };
            if info.changelog_path.is_none() {
                continue;
            }
            let key = format!("{}:{}", info.scope, info.source);
            if !seen_keys.insert(key.clone()) {
                continue;
            }
            let Some((_pkg_name, Some(version))) = self.read_package_json(base_dir) else {
                continue;
            };
            if settings.get_extension_changelog_version(&key).is_none() {
                settings.set_extension_changelog_version(&key, &version);
                continue;
            }
            // WIRE(@tui-w3): wire get_new_entries(parse_changelog(changelog_path), last_version) once @mcp lands.
            let _ = &mut updates;
        }
        updates
    }

    fn read_package_json(&self, package_root: &str) -> Option<(Option<String>, Option<String>)> {
        let pkg_path = pi_js::path::join(&[package_root, "package.json"]);
        let raw = pi_js::fs::read_to_string(&pkg_path).ok()?;
        let cleaned = raw.strip_prefix('\u{FEFF}').unwrap_or(&raw);
        let val = pi_js::json::parse(cleaned).ok()?;
        let name = val.get("name").and_then(Value::as_str).map(str::to_string);
        let version = val.get("version").and_then(Value::as_str).map(str::to_string);
        Some((name, version))
    }

    fn report_install_telemetry(&self, version: &str) {
        if pi_js::env::var("PI_OFFLINE").as_deref() == Some("1") {
            return;
        }
        if !self.settings_manager().get_enable_install_telemetry() {
            return;
        }
        // WIRE(@tui-w3): wire install telemetry HTTP ping if enabled once @mcp lands.
        let _ = version;
    }

    pub(crate) fn get_markdown_theme_with_settings(&self) -> MarkdownTheme {
        let mut md_theme = get_markdown_theme();
        md_theme.code_block_indent = Some(" ".repeat(self.settings_manager().get_code_block_indent()));
        md_theme
    }

    fn format_display_path(&self, p: &str) -> String {
        shorten_home_path_local(p)
    }

    fn format_extension_display_path(&self, p: &str) -> String {
        let s = self.format_display_path(p);
        s.strip_suffix("/index.ts")
            .or_else(|| s.strip_suffix("/index.js"))
            .unwrap_or(&s)
            .to_string()
    }

    fn format_context_path(&self, p: &str) -> String {
        let cwd = pi_js::path::resolve(&[&self.session_manager().get_cwd()]);
        let abs = if pi_js::path::is_absolute(p) {
            pi_js::path::resolve(&[p])
        } else {
            pi_js::path::resolve(&[&cwd, p])
        };
        get_cwd_relative_path_local(&abs, &cwd).unwrap_or_else(|| self.format_display_path(&abs))
    }

    pub(crate) fn get_startup_expansion_state(&self) -> bool {
        let st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        st.options.verbose.unwrap_or(false) || st.tool_output_expanded
    }

    pub(crate) fn should_show_startup_header(&self) -> bool {
        let verbose = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .options
            .verbose
            == Some(true);
        verbose || self.settings_manager().get_quiet_startup() != QuietStartup::True
    }

    pub fn get_model_scope_notice(&self) -> Option<String> {
        let scoped = self.session().scoped_models();
        if scoped.is_empty() || !self.should_show_startup_details() {
            return None;
        }
        let model_list: Vec<String> = scoped
            .into_iter()
            .map(|sm| match sm.thinking_level {
                Some(ref lvl) if !lvl.is_empty() => format!("{}:{lvl}", sm.model.id),
                _ => sm.model.id,
            })
            .collect();
        let cycle_keys = self.inner.keybindings.get_keys(&"app.model.cycleForward".to_string());
        let cycle_hint = if !cycle_keys.is_empty() {
            let formatted = format_key_text(&cycle_keys.join("/"), KeyTextFormatOptions { capitalize: true });
            theme().fg(ThemeColor::Muted, &format!(" ({formatted} to cycle)"))
        } else {
            String::new()
        };
        Some(theme().fg(
            ThemeColor::Dim,
            &format!("Model scope: {}{cycle_hint}", model_list.join(", ")),
        ))
    }

    pub(crate) fn show_fullscreen_model_scope_notice(&self) {
        if self.get_model_scope_notice().is_none() {
            return;
        }
        let this = self.clone();
        self.inner.header_container.add_child(Arc::new(ThemedText::new(
            Arc::new(move || this.get_model_scope_notice().unwrap_or_default()),
            Some(1),
            Some(0),
        )));
        self.inner.header_container.add_child(Arc::new(Spacer::new(Some(1))));
    }

    pub(crate) fn should_show_startup_details(&self) -> bool {
        let verbose = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .options
            .verbose
            == Some(true);
        verbose || self.settings_manager().get_quiet_startup() == QuietStartup::False
    }

    fn get_short_path(&self, full_path: &str, source_info: Option<&SourceInfo>) -> String {
        let norm_full = full_path.replace('\\', "/");
        if let Some(info) = source_info
            && let Some(ref base_dir) = info.base_dir
            && Self::is_package_source(Some(info))
        {
            let norm_base = base_dir.replace('\\', "/");
            if let Some(nm_idx) = norm_base.rfind("/node_modules/") {
                let prefix_with_sep = format!("{}/node_modules/", &norm_base[..nm_idx]);
                if norm_full.starts_with(&prefix_with_sep) {
                    return pi_js::path::posix::relative(&norm_base, &norm_full);
                }
            }
            let rel = pi_js::path::relative(&pi_js::path::resolve(&[base_dir]), &pi_js::path::resolve(&[full_path]));
            if !rel.is_empty() && rel != "." && !rel.starts_with("..") && !pi_js::path::is_absolute(&rel) {
                return rel.replace('\\', "/");
            }
        }

        let source = source_info.map(|s| s.source.as_str()).unwrap_or("");
        if source.starts_with("npm:")
            && let Some(idx) = norm_full.find("node_modules/")
        {
            let after = &norm_full[idx + "node_modules/".len()..];
            let mut parts = after.splitn(if after.starts_with('@') { 3 } else { 2 }, '/');
            if after.starts_with('@') {
                if let (Some(_scope), Some(_pkg), Some(rest)) = (parts.next(), parts.next(), parts.next()) {
                    return rest.to_string();
                }
            } else if let (Some(_pkg), Some(rest)) = (parts.next(), parts.next()) {
                return rest.to_string();
            }
        }

        if source.starts_with("git:")
            && let Some(idx) = norm_full.find("git/")
        {
            let after = &norm_full[idx + 4..];
            let mut parts = after.splitn(3, '/');
            if let (Some(_host), Some(_repo), Some(rest)) = (parts.next(), parts.next(), parts.next()) {
                return rest.to_string();
            }
        }

        self.format_display_path(full_path)
    }

    fn get_compact_path_label(&self, resource_path: &str, source_info: Option<&SourceInfo>) -> String {
        let short_path = self.get_short_path(resource_path, source_info);
        let norm = short_path.replace('\\', "/");
        if let Some(last) = norm.split('/').rfind(|s| !s.is_empty() && *s != "~") {
            last.to_string()
        } else {
            short_path
        }
    }

    fn get_compact_package_source_label(&self, source_info: Option<&SourceInfo>) -> String {
        let source = source_info.map(|s| s.source.as_str()).unwrap_or("");
        if let Some(rest) = source.strip_prefix("npm:")
            && !rest.is_empty()
        {
            return rest.to_string();
        }
        if let Some((_host, path, _ref)) = parse_git_url_local(source)
            && !path.is_empty()
        {
            return path;
        }
        source.to_string()
    }

    fn get_compact_extension_label(&self, resource_path: &str, source_info: Option<&SourceInfo>) -> String {
        if !Self::is_package_source(source_info) {
            return self.get_compact_path_label(resource_path, source_info);
        }
        let source_label = self.get_compact_package_source_label(source_info);
        if source_label.is_empty() {
            return self.get_compact_path_label(resource_path, source_info);
        }
        let short_path = self.get_short_path(resource_path, source_info).replace('\\', "/");
        let pkg_path = short_path.strip_prefix("extensions/").unwrap_or(&short_path);
        let dir = pi_js::path::posix::dirname(pkg_path);
        let file = pi_js::path::posix::basename(pkg_path, None);
        let stem = file.rsplit_once('.').map(|(s, _)| s).unwrap_or(&file);
        if stem == "index" {
            if dir.is_empty() || dir == "." {
                source_label
            } else {
                format!("{source_label}:{dir}")
            }
        } else {
            format!("{source_label}:{pkg_path}")
        }
    }

    fn get_compact_display_path_segments(&self, resource_path: &str) -> Vec<String> {
        self.format_display_path(resource_path)
            .replace('\\', "/")
            .split('/')
            .filter(|s| !s.is_empty() && *s != "~")
            .map(str::to_string)
            .collect()
    }

    fn get_compact_non_package_extension_label(
        &self,
        resource_path: &str,
        index: usize,
        all: &[(String, Vec<String>)],
    ) -> String {
        let Some((_, segments)) = all.get(index) else {
            return self.get_compact_path_label(resource_path, None);
        };
        if segments.is_empty() {
            return self.get_compact_path_label(resource_path, None);
        }
        for count in 1..=segments.len() {
            let candidate = segments[segments.len() - count..].join("/");
            let unique = all.iter().enumerate().all(|(idx, (_, other))| {
                if idx == index {
                    return true;
                }
                let start = other.len().saturating_sub(count);
                other[start..].join("/") != candidate
            });
            if unique {
                return candidate;
            }
        }
        segments.join("/")
    }

    fn get_compact_extension_labels(&self, extensions: &[(String, Option<SourceInfo>)]) -> Vec<String> {
        let non_package: Vec<(String, Vec<String>)> = extensions
            .iter()
            .filter(|(_, info)| !Self::is_package_source(info.as_ref()))
            .map(|(p, _)| {
                let mut segs = self.get_compact_display_path_segments(p);
                if segs.len() > 1 && matches!(segs.last().map(String::as_str), Some("index.ts" | "index.js")) {
                    segs.pop();
                }
                (p.clone(), segs)
            })
            .collect();

        extensions
            .iter()
            .map(|(p, info)| {
                if Self::is_package_source(info.as_ref()) {
                    return self.get_compact_extension_label(p, info.as_ref());
                }
                if let Some(idx) = non_package.iter().position(|(np_path, _)| np_path == p) {
                    self.get_compact_non_package_extension_label(p, idx, &non_package)
                } else {
                    self.get_compact_path_label(p, info.as_ref())
                }
            })
            .collect()
    }

    fn get_display_source_info(&self, source_info: Option<&SourceInfo>) -> (String, Option<&'static str>) {
        let source = source_info.map(|s| s.source.as_str()).unwrap_or("local");
        let scope = source_info.map(|s| s.scope.as_str()).unwrap_or("project");
        if source == "local" {
            return match scope {
                "user" => ("user".to_string(), None),
                "project" => ("project".to_string(), None),
                "temporary" => ("path".to_string(), Some("temp")),
                _ => ("path".to_string(), None),
            };
        }
        if source == "cli" {
            return (
                "path".to_string(),
                if scope == "temporary" { Some("temp") } else { None },
            );
        }
        let scope_label = match scope {
            "user" => Some("user"),
            "project" => Some("project"),
            "temporary" => Some("temp"),
            _ => None,
        };
        (source.to_string(), scope_label)
    }

    fn get_scope_group(source_info: Option<&SourceInfo>) -> &'static str {
        let source = source_info.map(|s| s.source.as_str()).unwrap_or("local");
        let scope = source_info.map(|s| s.scope.as_str()).unwrap_or("project");
        if source == "cli" || scope == "temporary" {
            return "path";
        }
        match scope {
            "user" => "user",
            "project" => "project",
            _ => "path",
        }
    }

    fn is_package_source(source_info: Option<&SourceInfo>) -> bool {
        let source = source_info.map(|s| s.source.as_str()).unwrap_or("");
        source.starts_with("npm:") || source.starts_with("git:")
    }

    fn build_scope_groups(&self, items: Vec<(String, Option<SourceInfo>)>) -> Vec<ScopeGroup> {
        let mut user = ScopeGroup {
            scope: "user",
            paths: Vec::new(),
            packages: IndexMap::new(),
        };
        let mut project = ScopeGroup {
            scope: "project",
            paths: Vec::new(),
            packages: IndexMap::new(),
        };
        let mut path = ScopeGroup {
            scope: "path",
            paths: Vec::new(),
            packages: IndexMap::new(),
        };

        for (p, info) in items {
            let group_key = Self::get_scope_group(info.as_ref());
            let target = match group_key {
                "user" => &mut user,
                "project" => &mut project,
                _ => &mut path,
            };
            let source = info
                .as_ref()
                .map(|i| i.source.clone())
                .unwrap_or_else(|| "local".to_string());
            if Self::is_package_source(info.as_ref()) {
                target.packages.entry(source).or_default().push((p, info));
            } else {
                target.paths.push((p, info));
            }
        }

        [project, user, path]
            .into_iter()
            .filter(|g| !g.paths.is_empty() || !g.packages.is_empty())
            .collect()
    }

    fn format_scope_groups<FPath, FPkg>(
        &self,
        groups: &[ScopeGroup],
        format_path: FPath,
        format_package_path: FPkg,
    ) -> String
    where
        FPath: Fn(&str, Option<&SourceInfo>) -> String,
        FPkg: Fn(&str, Option<&SourceInfo>, &str) -> String,
    {
        let t = theme();
        let mut lines = Vec::new();
        for group in groups {
            lines.push(format!("  {}", t.fg(ThemeColor::Accent, group.scope)));
            let mut sorted_paths = group.paths.clone();
            sorted_paths.sort_by(|(a, _), (b, _)| a.cmp(b));
            for (p, info) in sorted_paths {
                lines.push(t.fg(ThemeColor::Dim, &format!("    {}", format_path(&p, info.as_ref()))));
            }
            let mut sorted_pkgs: Vec<(&String, &Vec<(String, Option<SourceInfo>)>)> = group.packages.iter().collect();
            sorted_pkgs.sort_by(|(a, _), (b, _)| a.cmp(b));
            for (source, items) in sorted_pkgs {
                lines.push(format!("    {}", t.fg(ThemeColor::MdLink, source)));
                let mut sorted_pkg_paths = items.clone();
                sorted_pkg_paths.sort_by(|(a, _), (b, _)| a.cmp(b));
                for (p, info) in sorted_pkg_paths {
                    lines.push(t.fg(
                        ThemeColor::Dim,
                        &format!("      {}", format_package_path(&p, info.as_ref(), source)),
                    ));
                }
            }
        }
        lines.join("\n")
    }

    fn find_source_info_for_path<'a>(
        p: &str,
        source_infos: &'a IndexMap<String, SourceInfo>,
    ) -> Option<&'a SourceInfo> {
        if let Some(exact) = source_infos.get(p) {
            return Some(exact);
        }
        let mut current = p;
        while let Some(idx) = current.rfind('/') {
            current = &current[..idx];
            if let Some(parent) = source_infos.get(current) {
                return Some(parent);
            }
        }
        None
    }

    fn format_path_with_source(&self, p: &str, source_info: Option<&SourceInfo>) -> String {
        if let Some(info) = source_info {
            let short_path = self.get_short_path(p, Some(info));
            let (label, scope_label) = self.get_display_source_info(Some(info));
            let label_text = match scope_label {
                Some(sl) => format!("{label} ({sl})"),
                None => label,
            };
            return format!("{label_text} {short_path}");
        }
        self.format_display_path(p)
    }

    fn format_diagnostics(
        &self,
        diagnostics: &[ResourceDiagnostic],
        source_infos: &IndexMap<String, SourceInfo>,
    ) -> String {
        let t = theme();
        let mut lines = Vec::new();
        let mut collisions: IndexMap<String, Vec<&ResourceDiagnostic>> = IndexMap::new();
        let mut other = Vec::new();

        for d in diagnostics {
            if d.diagnostic_type == DiagnosticLevel::Collision
                && let Some(ref c) = d.collision
            {
                collisions.entry(c.name.clone()).or_default().push(d);
            } else {
                other.push(d);
            }
        }

        for (name, list) in collisions {
            let Some(first) = list.first().and_then(|d| d.collision.as_ref()) else {
                continue;
            };
            lines.push(t.fg(ThemeColor::Warning, &format!("  \"{name}\" collision:")));
            lines.push(t.fg(
                ThemeColor::Dim,
                &format!(
                    "    {} {}",
                    t.fg(ThemeColor::Success, "✓"),
                    self.format_path_with_source(
                        &first.winner_path,
                        Self::find_source_info_for_path(&first.winner_path, source_infos)
                    )
                ),
            ));
            for d in list {
                if let Some(ref c) = d.collision {
                    lines.push(t.fg(
                        ThemeColor::Dim,
                        &format!(
                            "    {} {} (skipped)",
                            t.fg(ThemeColor::Warning, "✗"),
                            self.format_path_with_source(
                                &c.loser_path,
                                Self::find_source_info_for_path(&c.loser_path, source_infos)
                            )
                        ),
                    ));
                }
            }
        }

        for d in other {
            let color = if d.diagnostic_type == DiagnosticLevel::Error {
                ThemeColor::Error
            } else {
                ThemeColor::Warning
            };
            if let Some(ref path) = d.path {
                let formatted = self.format_path_with_source(path, Self::find_source_info_for_path(path, source_infos));
                lines.push(t.fg(color, &format!("  {formatted}")));
                lines.push(t.fg(color, &format!("    {}", d.message)));
            } else {
                lines.push(t.fg(color, &format!("  {}", d.message)));
            }
        }

        lines.join("\n")
    }

    pub fn show_loaded_resources(
        &self,
        extensions_override: Option<Vec<(String, Option<SourceInfo>)>>,
        force: bool,
        show_diagnostics_when_quiet: bool,
    ) {
        self.clear_resource_container();

        let show_listing = force || self.should_show_startup_details();
        let show_diagnostics = show_listing || show_diagnostics_when_quiet;
        if !show_listing && !show_diagnostics {
            return;
        }

        let section_header = |name: &str, color: ThemeColor| -> String { theme().fg(color, &format!("[{name}]")) };
        let format_compact_list = |items: Vec<String>, sort: bool| -> String {
            let mut labels: Vec<String> = items
                .into_iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if sort {
                labels.sort();
            }
            theme().fg(ThemeColor::Dim, &format!("  {}", labels.join(", ")))
        };

        let add_loaded_section = |name: &'static str,
                                  collapsed_body: Arc<dyn Fn() -> String + Send + Sync>,
                                  expanded_body: Arc<dyn Fn() -> String + Send + Sync>,
                                  color: ThemeColor| {
            let section = ExpandableText::new(
                Arc::new(move || format!("{}\n{}", section_header(name, color), collapsed_body())),
                Arc::new(move || format!("{}\n{}", section_header(name, color), expanded_body())),
                self.get_startup_expansion_state(),
                0,
                0,
            );
            let arc_comp: Arc<dyn Component> = Arc::new(section.clone());
            let arc_exp: Arc<dyn Expandable> = Arc::new(section);
            self.add_resource_component(arc_comp, Some(arc_exp));
            self.add_resource_component(Arc::new(Spacer::new(Some(1))), None);
        };

        let sess = self.session();
        let loader = sess.resource_loader();
        let (skills, skill_diagnostics) = loader.get_skills();
        let (prompts, prompt_diagnostics) = loader.get_prompts();
        let (_, loaded_themes, theme_diagnostics) = loader.get_themes();
        let ext_res = loader.get_extensions();

        let extensions: Vec<(String, Option<SourceInfo>)> = extensions_override.unwrap_or_else(|| {
            ext_res
                .extensions
                .iter()
                .filter(|e| !e.hidden)
                .map(|e| (e.path.clone(), e.source_info.clone()))
                .collect()
        });

        let mut source_infos = IndexMap::new();
        for (p, info) in &extensions {
            if let Some(info) = info {
                source_infos.insert(p.clone(), info.clone());
            }
        }
        for s in &skills {
            if let Some(ref info) = s.source_info {
                source_infos.insert(s.file_path.clone(), info.clone());
            }
        }
        for p in &prompts {
            if let Some(ref info) = p.source_info {
                source_infos.insert(p.file_path.clone(), info.clone());
            }
        }
        for t_res in &loaded_themes {
            if let (Some(sp), Some(info)) = (&t_res.source_path, &t_res.source_info) {
                source_infos.insert(sp.clone(), info.clone());
            }
        }

        if show_listing {
            let mut context_files = Vec::new();
            if let Some(sys) = loader.get_system_prompt_source() {
                context_files.push(sys);
            }
            context_files.extend(loader.get_append_system_prompt_sources());
            context_files.extend(loader.get_agents_files());
            if !context_files.is_empty() {
                self.add_resource_component(Arc::new(Spacer::new(Some(1))), None);
                let paths: Vec<String> = context_files.into_iter().map(|c| c.path).collect();
                let paths_exp = paths.clone();
                let paths_col = paths;
                let m_exp = self.clone();
                let m_col = self.clone();
                add_loaded_section(
                    "Context",
                    Arc::new(move || {
                        format_compact_list(paths_col.iter().map(|p| m_col.format_context_path(p)).collect(), false)
                    }),
                    Arc::new(move || {
                        let t = theme();
                        paths_exp
                            .iter()
                            .map(|p| t.fg(ThemeColor::Dim, &format!("  {}", m_exp.format_display_path(p))))
                            .collect::<Vec<_>>()
                            .join("\n")
                    }),
                    ThemeColor::MdHeading,
                );
            }

            if !skills.is_empty() {
                let groups = Arc::new(
                    self.build_scope_groups(
                        skills
                            .iter()
                            .map(|s| (s.file_path.clone(), s.source_info.clone()))
                            .collect(),
                    ),
                );
                let names: Vec<String> = skills.iter().map(|s| s.name.clone()).collect();
                let m_exp = self.clone();
                let groups_c = Arc::clone(&groups);
                add_loaded_section(
                    "Skills",
                    Arc::new(move || format_compact_list(names.clone(), true)),
                    Arc::new(move || {
                        m_exp.format_scope_groups(
                            &groups_c,
                            |p, _| m_exp.format_display_path(p),
                            |p, info, _| m_exp.get_short_path(p, info),
                        )
                    }),
                    ThemeColor::MdHeading,
                );
            }

            let templates = sess.prompt_templates();
            if !templates.is_empty() {
                let groups = Arc::new(
                    self.build_scope_groups(
                        templates
                            .iter()
                            .map(|t| (t.file_path.clone(), t.source_info.clone()))
                            .collect(),
                    ),
                );
                let mut by_path = IndexMap::new();
                for t in &templates {
                    by_path.insert(t.file_path.clone(), t.name.clone());
                }
                let by_path = Arc::new(by_path);
                let compact_names: Vec<String> = templates.iter().map(|t| format!("/{}", t.name)).collect();
                let m_exp = self.clone();
                add_loaded_section(
                    "Prompts",
                    Arc::new(move || format_compact_list(compact_names.clone(), true)),
                    Arc::new(move || {
                        let map_a = Arc::clone(&by_path);
                        let map_b = Arc::clone(&by_path);
                        let m_a = m_exp.clone();
                        let m_b = m_exp.clone();
                        m_exp.format_scope_groups(
                            &groups,
                            move |p, _| {
                                map_a
                                    .get(p)
                                    .map(|n| format!("/{n}"))
                                    .unwrap_or_else(|| m_a.format_display_path(p))
                            },
                            move |p, _, _| {
                                map_b
                                    .get(p)
                                    .map(|n| format!("/{n}"))
                                    .unwrap_or_else(|| m_b.format_display_path(p))
                            },
                        )
                    }),
                    ThemeColor::MdHeading,
                );
            }

            if !extensions.is_empty() {
                let groups = Arc::new(self.build_scope_groups(extensions.clone()));
                let ext_labels = self.get_compact_extension_labels(&extensions);
                let m_exp = self.clone();
                add_loaded_section(
                    "Extensions",
                    Arc::new(move || format_compact_list(ext_labels.clone(), true)),
                    Arc::new(move || {
                        m_exp.format_scope_groups(
                            &groups,
                            |p, _| m_exp.format_extension_display_path(p),
                            |p, info, _| m_exp.format_extension_display_path(&m_exp.get_short_path(p, info)),
                        )
                    }),
                    ThemeColor::MdHeading,
                );
            }
        }

        if show_diagnostics {
            let source_infos = Arc::new(source_infos);
            let add_diag = |header: &'static str, diags: Vec<ResourceDiagnostic>| {
                if diags.is_empty() {
                    return;
                }
                let m = self.clone();
                let si = Arc::clone(&source_infos);
                let diags = Arc::new(diags);
                self.add_resource_component(
                    Arc::new(ThemedText::new(
                        Arc::new(move || {
                            format!(
                                "{}\n{}",
                                theme().fg(ThemeColor::Warning, header),
                                m.format_diagnostics(&diags, &si)
                            )
                        }),
                        Some(0),
                        Some(0),
                    )),
                    None,
                );
                self.add_resource_component(Arc::new(Spacer::new(Some(1))), None);
            };

            add_diag("[Skill warnings]", skill_diagnostics);
            add_diag("[Prompt conflicts]", prompt_diagnostics);

            let mut ext_diags = Vec::new();
            for e in ext_res.errors {
                ext_diags.push(ResourceDiagnostic {
                    diagnostic_type: DiagnosticLevel::Error,
                    message: e.message,
                    path: Some(e.path),
                    collision: None,
                });
            }
            for w in ext_res.warnings {
                ext_diags.push(ResourceDiagnostic {
                    diagnostic_type: DiagnosticLevel::Warning,
                    message: w.message,
                    path: Some(w.path),
                    collision: None,
                });
            }
            let runner = sess.extension_runner();
            ext_diags.extend(runner.get_command_diagnostics());
            ext_diags.extend(self.get_built_in_command_conflict_diagnostics(&*runner));
            ext_diags.extend(runner.get_shortcut_diagnostics());
            add_diag("[Extension issues]", ext_diags);
            add_diag("[Theme conflicts]", theme_diagnostics);
        }
    }

    async fn bind_current_session_extensions(&self) {
        let ui_context = self.create_extension_ui_context();
        let m_abort = self.clone();
        let m_new = self.clone();
        let m_fork = self.clone();
        let m_nav = self.clone();
        let m_switch = self.clone();
        let m_reload = self.clone();
        let m_shut = self.clone();
        let m_err = self.clone();
        let m_idle = self.clone();

        let bindings = InteractiveExtensionBindings {
            ui_context,
            abort_handler: Arc::new(move || {
                m_abort.restore_queued_messages_to_editor(true);
            }),
            shutdown_handler: Arc::new(move || {
                m_shut
                    .inner
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .shutdown_requested = true;
                if m_shut.session().is_idle() {
                    let ms = m_shut.clone();
                    tokio::spawn(async move {
                        ms.shutdown(false).await;
                    });
                }
            }),
            on_error: Arc::new(move |ext_path, err, stack| {
                m_err.show_extension_error(ext_path, err, stack);
            }),
            wait_for_idle: Arc::new(move || {
                let s = m_idle.session();
                Box::pin(async move { s.wait_for_idle().await })
            }),
            new_session: Arc::new(move || {
                let m = m_new.clone();
                Box::pin(async move {
                    m.clear_status_indicator(None);
                    match m.inner.runtime_host.new_session().await {
                        Ok(res) => Ok(res),
                        Err(e) => {
                            m.handle_fatal_runtime_error("Failed to create session", &e.to_string())
                                .await;
                            Err(e)
                        }
                    }
                })
            }),
            fork: Arc::new(move |entry_id, opts| {
                let m = m_fork.clone();
                Box::pin(async move {
                    match m.inner.runtime_host.fork(&entry_id, opts).await {
                        Ok(res) => {
                            if !res.cancelled {
                                m.active_editor().set_text(res.selected_text.as_deref().unwrap_or(""));
                                m.show_status("Forked to new session");
                            }
                            Ok(CancelledResult {
                                cancelled: res.cancelled,
                            })
                        }
                        Err(e) => {
                            m.handle_fatal_runtime_error("Failed to fork session", &e.to_string())
                                .await;
                            Err(e)
                        }
                    }
                })
            }),
            navigate_tree: Arc::new(move |target_id, opts| {
                let m = m_nav.clone();
                Box::pin(async move {
                    let result = m.session().navigate_tree(&target_id, opts).await?;
                    if result.cancelled {
                        return Ok(CancelledResult { cancelled: true });
                    }
                    m.clear_chat_container();
                    m.render_initial_messages();
                    if let Some(ref editor_text) = result.editor_text
                        && m.active_editor().get_text().trim().is_empty()
                    {
                        m.active_editor().set_text(editor_text);
                    }
                    m.show_status("Navigated to selected point");
                    let mf = m.clone();
                    tokio::spawn(async move {
                        mf.flush_compaction_queue(false).await;
                    });
                    Ok(CancelledResult { cancelled: false })
                })
            }),
            switch_session: Arc::new(move |session_path| {
                let m = m_switch.clone();
                Box::pin(async move { Ok(m.handle_resume_session(&session_path).await) })
            }),
            reload: Arc::new(move || {
                let m = m_reload.clone();
                Box::pin(async move {
                    m.handle_reload_command().await;
                    Ok(())
                })
            }),
        };

        let _ = self.session().bind_extensions(bindings).await;
        let (themes, _, _) = self.session().resource_loader().get_themes();
        let _ = set_registered_themes(themes);
        self.setup_autocomplete_provider();
        self.setup_extension_shortcuts();
        self.show_loaded_resources(None, false, true);
        self.show_startup_notices_if_needed();
    }

    pub(crate) fn apply_fullscreen_scrollbar_setting(&self) {
        let mode = self.settings_manager().get_fullscreen_scrollbar();
        if let Some(ref sv) = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .transcript_scroll_view
        {
            sv.set_scrollbar(mode);
        }
    }

    pub(crate) fn ensure_png_transcoder(&self) {
        // WIRE(@tui-w3): wire crate::utils::image_convert::ensure_png_transcoder once @mcp lands on main.
    }

    pub(crate) fn apply_runtime_settings(&self) {
        let settings = self.settings_manager();
        set_capability_overrides(settings.get_terminal_capability_overrides());
        self.ensure_png_transcoder();
        self.apply_fullscreen_scrollbar_setting();

        let (renderer, editor, has_active_indicator) = {
            let st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            (
                st.renderer.clone(),
                st.editor.clone(),
                st.active_status_indicator.is_some(),
            )
        };
        if let Some(alt) = renderer.as_alt_screen() {
            alt.set_copy_on_select(settings.get_fullscreen_copy_on_select());
            alt.set_wheel_scroll_lines(settings.get_fullscreen_wheel_scroll_lines());
        }
        let sess = self.session();
        self.inner.footer.set_session(sess.as_footer_session());
        self.inner
            .footer
            .set_auto_compact_enabled(sess.auto_compaction_enabled());
        self.inner
            .footer_data_provider
            .set_cwd(&self.session_manager().get_cwd());

        let hide_thinking = settings.get_hide_thinking_block();
        let output_pad = settings.get_output_pad();
        {
            let mut st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            st.hide_thinking_block = hide_thinking;
            st.output_pad = output_pad;
        }

        self.ui().set_show_hardware_cursor(settings.get_show_hardware_cursor());
        let clear_on_shrink = settings.get_clear_on_shrink();
        self.ui().set_clear_on_shrink(clear_on_shrink);
        if !clear_on_shrink && !has_active_indicator {
            self.inner.status_container.clear();
        }

        let padding_x = settings.get_editor_padding_x();
        let max_vis = settings.get_autocomplete_max_visible();
        self.inner.default_editor.set_padding_x(padding_x as f64);
        self.inner.default_editor.set_autocomplete_max_visible(max_vis as f64);
        if !editor.is_default(&self.inner.default_editor) {
            editor.set_padding_x(padding_x as f64);
            editor.set_autocomplete_max_visible(max_vis as f64);
        }
    }

    pub(crate) async fn rebind_current_session(&self, render_before_bind: bool) {
        let previous_session = self.session();
        let unsub = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .unsubscribe
            .take();
        if let Some(u) = unsub {
            u.call();
        }
        self.inner.program_status.reset();
        self.apply_runtime_settings();

        if render_before_bind {
            self.render_current_session_state();
            self.subscribe_to_agent();
        }

        self.bind_current_session_extensions().await;

        let current = self.session();
        if !Arc::ptr_eq(&previous_session, &current) {
            return;
        }

        if !render_before_bind {
            self.subscribe_to_agent();
        }

        self.update_available_provider_count();
        self.update_editor_border_color();
        self.update_terminal_title();
    }

    pub(crate) async fn handle_fatal_runtime_error(&self, prefix: &str, error_msg: &str) {
        self.show_error(&format!("{prefix}: {error_msg}"));
        let pad = self.inner.state.lock().unwrap_or_else(|e| e.into_inner()).output_pad;
        if let Some(hint) = self.get_crash_extension_hint(None) {
            self.add_chat_component(
                Arc::new(ThemedText::new(
                    Arc::new(move || theme().fg(ThemeColor::Warning, &hint)),
                    Some(pad),
                    Some(0),
                )),
                None,
                None,
                None,
            );
        }
        stop_theme_watcher();
        self.stop(Some(FullscreenExitOutput::Transcript));
        (self.inner.hooks.exit_process)(1);
    }

    fn get_crash_extension_hint(&self, stack: Option<&str>) -> Option<String> {
        let _ = stack;
        None
    }

    fn suggest_bug_report(&self) {
        let (already, pad) = {
            let mut st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if st.bug_report_hint_shown {
                return;
            }
            st.bug_report_hint_shown = true;
            (false, st.output_pad)
        };
        let _ = already;
        self.add_chat_component(
            Arc::new(ThemedText::new(
                Arc::new(|| {
                    theme().fg(
                        ThemeColor::Muted,
                        &format!("If this looks like a {APP_NAME_LOCAL} bug, /bug sends a report to the developers."),
                    )
                }),
                Some(pad),
                Some(0),
            )),
            None,
            None,
            None,
        );
        self.ui().request_render(None);
    }

    pub(crate) fn maybe_suggest_bug_report(&self, message: &pi_ai::AssistantMessage) {
        if message.stop_reason != pi_ai::StopReason::Error {
            return;
        }
        let err = message.error_message.as_deref().unwrap_or("");
        let lower = err.to_ascii_lowercase();
        if lower.contains("abort") || lower.contains("cancel") {
            return;
        }
        if self.maybe_show_install_change_warning() {
            return;
        }
        self.suggest_bug_report();
    }

    pub(crate) fn maybe_show_install_change_warning(&self) -> bool {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .install_change_warning_shown
    }

    fn render_current_session_state(&self) {
        self.clear_resource_container();
        self.clear_chat_container();
        self.clear_pending_container();
        {
            let mut st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            st.pending_bash_components.clear();
            st.compaction_queued_messages.clear();
            st.streaming_component = None;
            st.streaming_component_arc = None;
            st.streaming_message = None;
            st.pending_tool_components.clear();
        }
        self.render_initial_messages();
    }

    pub(crate) fn get_markdown_transformers(&self) -> Vec<MarkdownTransformer> {
        let mut transformers = vec![self.inner.mermaid_markdown_transformer.clone()];
        transformers.extend(self.session().extension_runner().get_markdown_transformers());
        transformers
    }

    pub(crate) fn create_changelog_markdown(
        &self,
        markdown: &str,
        padding_x: i64,
        padding_y: i64,
        default_text_style: Option<pi_tui::DefaultTextStyle>,
    ) -> Arc<dyn Component> {
        Arc::new(Markdown::new(
            markdown,
            padding_x,
            padding_y,
            self.get_markdown_theme_with_settings(),
            default_text_style,
            None,
        ))
    }

    pub(crate) fn setup_extension_shortcuts(&self) {
        let shortcuts = self.session().extension_runner().get_shortcuts();
        if shortcuts.is_empty() {
            self.set_default_editor_extension_shortcut(None);
            return;
        }
        let m = self.clone();
        self.set_default_editor_extension_shortcut(Some(Arc::new(move |data: &str| {
            for (shortcut_str, shortcut) in &shortcuts {
                if matches_key(data, shortcut_str) {
                    let handler = Arc::clone(&shortcut.handler);
                    let m_err = m.clone();
                    tokio::spawn(async move {
                        handler().await;
                        let _ = &m_err;
                    });
                    return true;
                }
            }
            false
        })));
    }

    fn set_extension_status(&self, key: &str, text: Option<&str>) {
        self.inner.footer_data_provider.set_extension_status(key, text);
        self.ui().request_render(None);
    }

    pub(crate) fn set_editor_working_status_indicator(&self, indicator: Option<StatusIndicator>) -> bool {
        self.inner.default_editor.set_working_status_indicator(None);
        let editor = self.active_editor();
        if !editor.embed_working_status() {
            return false;
        }
        editor.set_working_status_indicator(indicator);
        true
    }

    pub(crate) fn show_status_indicator(&self, indicator: StatusIndicator) {
        let prev = {
            let mut st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            let prev = st.active_status_indicator.replace(indicator.clone());
            st.active_working_indicator_embedded = false;
            prev
        };
        if let Some(p) = prev {
            p.dispose();
        }
        self.inner.status_container.clear();
        self.set_editor_working_status_indicator(None);
        if self.set_editor_working_status_indicator(Some(indicator.clone())) {
            self.inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .active_working_indicator_embedded = true;
            return;
        }
        self.inner.status_container.add_child(Arc::new(indicator));
    }

    pub(crate) fn clear_status_indicator(
        &self,
        kind: Option<crate::modes::interactive::components::status_indicator::StatusIndicatorKind>,
    ) {
        let (cleared, was_embedded, is_regular) = {
            let mut st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(target_kind) = kind
                && st.active_status_indicator.as_ref().map(|i| i.kind()) != Some(target_kind)
            {
                return;
            }
            let cleared = st.active_status_indicator.take();
            let was_embedded = st.active_working_indicator_embedded;
            st.active_working_indicator_embedded = false;
            let is_regular = st.options.tui_mode == Some(SettingsTuiMode::Regular);
            (cleared, was_embedded, is_regular)
        };
        if let Some(ref c) = cleared {
            c.dispose();
        }
        self.inner.status_container.clear();
        self.set_editor_working_status_indicator(None);
        if cleared.is_some() && !was_embedded && is_regular && self.ui().get_clear_on_shrink() {
            self.inner
                .status_container
                .add_child(Arc::new(self.inner.idle_status.clone()));
        }
    }

    pub(crate) fn show_working_status_indicator(&self) {
        let (msg, opts, border_fn) = {
            let st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            let msg = st.working_message.clone().unwrap_or_else(|| "Working".to_string());
            let opts = st.working_indicator_options.clone();
            let border = if st.editor.embed_working_status() {
                let fallback =
                    theme().get_thinking_border_color_by_name(&self.session().model_thinking_level().to_string());
                Some(st.editor.border_color().unwrap_or(fallback))
            } else {
                None
            };
            (msg, opts, border)
        };
        self.show_status_indicator(StatusIndicator::working(self.ui(), &msg, opts, border_fn));
    }

    fn set_working_visible(&self, visible: bool) {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .working_visible = visible;
        if !visible {
            self.clear_status_indicator(Some(
                crate::modes::interactive::components::status_indicator::StatusIndicatorKind::Working,
            ));
            self.ui().request_render(None);
            return;
        }
        let is_working = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .active_status_indicator
            .as_ref()
            .map(|i| i.kind())
            == Some(crate::modes::interactive::components::status_indicator::StatusIndicatorKind::Working);
        if self.session().is_streaming() && !is_working {
            self.show_working_status_indicator();
        }
        self.ui().request_render(None);
    }

    fn set_working_indicator(&self, options: Option<pi_tui::LoaderIndicatorOptions>) {
        let active = {
            let mut st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            st.working_indicator_options = options;
            st.active_status_indicator
                .as_ref()
                .filter(|i| {
                    i.kind() == crate::modes::interactive::components::status_indicator::StatusIndicatorKind::Working
                })
                .cloned()
        };
        let _ = active;
        self.ui().request_render(None);
    }

    fn set_hidden_thinking_label(&self, label: Option<&str>) {
        let next_label = label.unwrap_or("Thinking...").to_string();
        let (assistants, streaming) = {
            let mut st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            st.hidden_thinking_label = next_label.clone();
            let assistants: Vec<_> = st.chat_trackers.iter().filter_map(|t| t.assistant.clone()).collect();
            (assistants, st.streaming_component.clone())
        };
        for child in assistants {
            child.set_hidden_thinking_label(&next_label);
        }
        if let Some(stream) = streaming {
            stream.set_hidden_thinking_label(&next_label);
        }
        self.ui().request_render(None);
    }

    fn set_extension_widget(&self, key: &str, content: Option<WidgetContent>, options: Option<ExtensionWidgetOptions>) {
        let placement = options.map(|o| o.placement).unwrap_or_default();
        let old_above = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extension_widgets_above
            .shift_remove(key);
        if let Some(entry) = old_above
            && let Some(dispose) = entry.dispose
        {
            dispose();
        }
        let old_below = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extension_widgets_below
            .shift_remove(key);
        if let Some(entry) = old_below
            && let Some(dispose) = entry.dispose
        {
            dispose();
        }

        let Some(content) = content else {
            self.render_widgets();
            return;
        };

        let component: Arc<dyn Component> = match content {
            WidgetContent::Lines(lines) => {
                let container = Container::new();
                for line in lines.iter().take(MAX_WIDGET_LINES) {
                    container.add_child(Arc::new(Text::new(Some(line), Some(1), Some(0), None)));
                }
                if lines.len() > MAX_WIDGET_LINES {
                    container.add_child(Arc::new(ThemedText::new(
                        Arc::new(|| theme().fg(ThemeColor::Muted, "... (widget truncated)")),
                        Some(1),
                        Some(0),
                    )));
                }
                Arc::new(container)
            }
            WidgetContent::Factory(factory) => factory(self.ui(), theme()),
        };

        {
            let mut st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            let map = match placement {
                WidgetPlacement::BelowEditor => &mut st.extension_widgets_below,
                WidgetPlacement::AboveEditor => &mut st.extension_widgets_above,
            };
            map.insert(
                key.to_string(),
                ExtensionWidgetEntry {
                    component,
                    dispose: None,
                },
            );
        }
        self.render_widgets();
    }

    fn clear_extension_widgets(&self) {
        let (above, below) = {
            let mut st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            (
                std::mem::take(&mut st.extension_widgets_above),
                std::mem::take(&mut st.extension_widgets_below),
            )
        };
        for (_, entry) in above.into_iter().chain(below.into_iter()) {
            if let Some(dispose) = entry.dispose {
                dispose();
            }
        }
        self.render_widgets();
    }

    pub fn reset_extension_ui(&self) {
        let (dialogs, overlays, default_msg) = {
            let mut st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            let dialogs = std::mem::take(&mut st.extension_dialogs);
            let overlays = st.extension_overlays.drain(..).collect::<Vec<_>>();
            (dialogs, overlays, "Working".to_string())
        };
        for dialog in dialogs {
            if let Some(dispose) = dialog.dispose {
                dispose();
            }
        }
        self.show_top_extension_dialog();
        for (_, remove) in overlays.into_iter().rev() {
            remove();
        }
        self.clear_extension_terminal_input_listeners();
        self.set_extension_footer(None);
        self.set_extension_header(None);
        self.clear_extension_widgets();
        self.inner.footer_data_provider.clear_extension_statuses();
        self.inner.footer.invalidate();
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .autocomplete_provider_wrappers
            .clear();
        self.set_custom_editor_component(None);
        self.setup_autocomplete_provider();
        self.set_default_editor_extension_shortcut(None);
        self.update_terminal_title();
        let active_working = {
            let mut st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            st.working_message = None;
            st.working_visible = true;
            st.working_indicator_options = None;
            st.active_status_indicator
                .as_ref()
                .filter(|i| {
                    i.kind() == crate::modes::interactive::components::status_indicator::StatusIndicatorKind::Working
                })
                .cloned()
        };
        if let Some(ind) = active_working {
            ind.set_message(&format!(
                "{default_msg} ({} to interrupt)",
                key_text(&"app.interrupt".to_string())
            ));
        }
        self.set_hidden_thinking_label(None);
    }

    pub(crate) fn render_widgets(&self) {
        let (above, below): (Vec<Arc<dyn Component>>, Vec<Arc<dyn Component>>) = {
            let st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            (
                st.extension_widgets_above
                    .values()
                    .map(|e| Arc::clone(&e.component))
                    .collect(),
                st.extension_widgets_below
                    .values()
                    .map(|e| Arc::clone(&e.component))
                    .collect(),
            )
        };
        Self::render_widget_container(&self.inner.widget_container_above, &above, true, true);
        Self::render_widget_container(&self.inner.widget_container_below, &below, false, false);
        self.ui().request_render(None);
    }

    fn render_widget_container(
        container: &Container,
        widgets: &[Arc<dyn Component>],
        spacer_when_empty: bool,
        leading_spacer: bool,
    ) {
        container.clear();
        if widgets.is_empty() {
            if spacer_when_empty {
                container.add_child(Arc::new(Spacer::new(Some(1))));
            }
            return;
        }
        if leading_spacer {
            container.add_child(Arc::new(Spacer::new(Some(1))));
        }
        for comp in widgets {
            container.add_child(Arc::clone(comp));
        }
    }

    fn set_extension_footer(&self, factory: Option<FooterFactory>) {
        let old = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .custom_footer
            .take();
        if let Some((_, Some(dispose))) = old {
            dispose();
        }
        self.inner.footer_container.clear();
        if let Some(f) = factory {
            let custom = f(
                self.ui(),
                theme(),
                Arc::new(FooterDataBridge(Arc::clone(&self.inner.footer_data_provider))),
            );
            self.inner.footer_container.add_child(Arc::clone(&custom));
            self.inner.state.lock().unwrap_or_else(|e| e.into_inner()).custom_footer = Some((custom, None));
        } else {
            self.inner
                .footer_container
                .add_child(Arc::new(self.inner.footer.clone()));
        }
        self.ui().request_render(None);
    }

    fn set_extension_header(&self, factory: Option<HeaderFactory>) {
        let (built_in_opt, old_custom, expanded) = {
            let mut st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if st.built_in_header.is_none() {
                return;
            }
            (
                st.built_in_header.clone(),
                st.custom_header.take(),
                st.tool_output_expanded,
            )
        };
        let old_comp = if let Some((comp, disp_opt)) = old_custom {
            if let Some(dispose) = disp_opt {
                dispose();
            }
            Some(comp)
        } else {
            None
        };
        let Some((built_in_comp, built_in_exp)) = built_in_opt else {
            return;
        };
        let mut children = self.inner.header_container.children();
        let current_arc = old_comp.unwrap_or_else(|| Arc::clone(&built_in_comp));
        let idx_opt = children.iter().position(|c| Arc::ptr_eq(c, &current_arc));

        if let Some(f) = factory {
            let custom = f(self.ui(), theme());
            if let Some(idx) = idx_opt {
                children[idx] = Arc::clone(&custom);
            } else {
                children.insert(0, Arc::clone(&custom));
            }
            self.inner.state.lock().unwrap_or_else(|e| e.into_inner()).custom_header = Some((custom, None));
        } else {
            if let Some(ref exp) = built_in_exp {
                exp.set_expanded(expanded);
            }
            if let Some(idx) = idx_opt {
                children[idx] = built_in_comp;
            }
        }
        self.inner.header_container.set_children(children);
        self.ui().request_render(None);
    }

    fn add_extension_terminal_input_listener(&self, handler: pi_tui::tui::TuiInputListener) -> Unsubscribe {
        let unsub = self.ui().add_input_listener(Arc::clone(&handler));
        let id = {
            let mut st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            let id = st.next_input_sub_id;
            st.next_input_sub_id += 1;
            st.extension_terminal_input_subscriptions
                .push(ExtensionInputSubscriptionEntry {
                    id,
                    handler,
                    unsubscribe: unsub,
                });
            id
        };
        let m = self.clone();
        Unsubscribe::new(move || {
            let removed = {
                let mut st = m.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(pos) = st
                    .extension_terminal_input_subscriptions
                    .iter()
                    .position(|e| e.id == id)
                {
                    Some(st.extension_terminal_input_subscriptions.remove(pos))
                } else {
                    None
                }
            };
            if let Some(entry) = removed {
                entry.unsubscribe.call();
            }
        })
    }

    pub(crate) fn rebind_extension_terminal_input_listeners(&self) {
        let subs = {
            let mut st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            std::mem::take(&mut st.extension_terminal_input_subscriptions)
        };
        let mut rebuilt = Vec::with_capacity(subs.len());
        for sub in subs {
            sub.unsubscribe.call();
            let new_unsub = self.ui().add_input_listener(Arc::clone(&sub.handler));
            rebuilt.push(ExtensionInputSubscriptionEntry {
                id: sub.id,
                handler: sub.handler,
                unsubscribe: new_unsub,
            });
        }
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extension_terminal_input_subscriptions = rebuilt;
    }

    pub(crate) fn clear_extension_terminal_input_listeners(&self) {
        let subs = {
            let mut st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            std::mem::take(&mut st.extension_terminal_input_subscriptions)
        };
        for sub in subs {
            sub.unsubscribe.call();
        }
    }

    pub fn create_extension_ui_context(&self) -> Arc<dyn ExtensionUiContext> {
        Arc::new(InteractiveExtensionUiContextImpl { mode: self.clone() })
    }

    pub(crate) fn push_extension_dialog(
        &self,
        component: Arc<dyn Component>,
        dispose: Option<Arc<dyn Fn() + Send + Sync>>,
        blocked: Option<BlockedStatus>,
    ) {
        self.dispose_active_selector();
        {
            let mut st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            st.extension_dialogs.push(ExtensionDialogEntry {
                component,
                dispose,
                blocked,
            });
        }
        self.show_top_extension_dialog();
    }

    pub(crate) fn remove_extension_dialog(&self, component: &Arc<dyn Component>) {
        let is_top = {
            let mut st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            let Some(idx) = st
                .extension_dialogs
                .iter()
                .position(|d| Arc::ptr_eq(&d.component, component))
            else {
                return;
            };
            st.extension_dialogs.remove(idx);
            idx == st.extension_dialogs.len()
        };
        if is_top {
            self.show_top_extension_dialog();
        }
    }

    fn show_top_extension_dialog(&self) {
        let (comp, blocked) = {
            let st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(top) = st.extension_dialogs.last() {
                (Arc::clone(&top.component), top.blocked.clone())
            } else {
                (st.editor.as_component(), None)
            }
        };
        self.inner.editor_container.clear();
        self.inner.editor_container.add_child(Arc::clone(&comp));
        self.ui().set_focus(Some(comp));
        self.inner.program_status.set_blocked("extension-dialog", blocked);
        self.ui().request_render(None);
    }

    pub async fn show_extension_selector(
        &self,
        title: &str,
        options: Vec<String>,
        opts: Option<ExtensionUIDialogOptions>,
        blocked_override: Option<BlockedStatus>,
    ) -> Option<String> {
        let blocked = blocked_override.unwrap_or_else(|| BlockedStatus {
            kind: BlockedKind::Question,
            message: title.to_string(),
        });
        if let Some(ref o) = opts
            && let Some(ref sig) = o.signal
            && sig.aborted()
        {
            return None;
        }
        let (tx, rx) = tokio::sync::oneshot::channel();
        let tx_slot = Arc::new(std::sync::Mutex::new(Some(tx)));
        let comp_slot: Arc<std::sync::Mutex<Option<Arc<dyn Component>>>> = Arc::new(std::sync::Mutex::new(None));
        let sel_slot: Arc<std::sync::Mutex<Option<ExtensionSelectorComponent>>> = Arc::new(std::sync::Mutex::new(None));
        let signal_opt = opts.as_ref().and_then(|o| o.signal.clone());
        let timeout_ms = opts.as_ref().and_then(|o| o.timeout_ms);

        let finish: Arc<dyn Fn(Option<String>) + Send + Sync> = {
            let tx_slot = Arc::clone(&tx_slot);
            let comp_slot = Arc::clone(&comp_slot);
            let sel_slot = Arc::clone(&sel_slot);
            let m = self.clone();
            Arc::new(move |value: Option<String>| {
                if let Some(sel) = sel_slot.lock().unwrap_or_else(|e| e.into_inner()).take() {
                    sel.dispose();
                }
                if let Some(comp) = comp_slot.lock().unwrap_or_else(|e| e.into_inner()).take() {
                    m.remove_extension_dialog(&comp);
                }
                if let Some(sender) = tx_slot.lock().unwrap_or_else(|e| e.into_inner()).take() {
                    let _ = sender.send(value);
                }
            })
        };

        let f_sel = Arc::clone(&finish);
        let f_can = Arc::clone(&finish);
        let f_abort = Arc::clone(&finish);
        let m_toggle = self.clone();
        let selector = ExtensionSelectorComponent::new(
            title,
            options,
            Arc::new(move |opt| f_sel(Some(opt.to_string()))),
            Arc::new(move || f_can(None)),
            Some(ExtensionSelectorOptions {
                tui: Some(self.ui()),
                timeout: timeout_ms,
                on_toggle_tools_expanded: Some(Arc::new(move || m_toggle.toggle_tool_output_expansion())),
                description: None,
            }),
        );
        let comp_arc: Arc<dyn Component> = Arc::new(selector.clone());
        *comp_slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::clone(&comp_arc));
        *sel_slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(selector.clone());
        let sel_for_disp = selector.clone();
        self.push_extension_dialog(comp_arc, Some(Arc::new(move || sel_for_disp.dispose())), Some(blocked));

        if let Some(sig) = signal_opt {
            sig.on_abort(move |_| {
                f_abort(None);
            })
            .forget();
        }
        rx.await.unwrap_or(None)
    }

    pub async fn show_extension_confirm(
        &self,
        title: &str,
        message: &str,
        opts: Option<ExtensionUIDialogOptions>,
    ) -> bool {
        let result = self
            .show_extension_selector(
                &format!("{title}\n{message}"),
                vec!["Yes".to_string(), "No".to_string()],
                opts,
                Some(BlockedStatus {
                    kind: BlockedKind::Permission,
                    message: title.to_string(),
                }),
            )
            .await;
        result.as_deref() == Some("Yes")
    }

    #[allow(dead_code)] // WIRE(@tui-w3): used when MissingSessionCwdError lands on main.
    pub(crate) async fn prompt_for_missing_session_cwd(
        &self,
        prompt: &str,
        options: Vec<(String, MissingSessionCwdChoice)>,
    ) -> Option<MissingSessionCwdChoice> {
        let labels: Vec<String> = options.iter().map(|(l, _)| l.clone()).collect();
        let selected = self.show_extension_selector(prompt, labels, None, None).await?;
        options.into_iter().find(|(l, _)| *l == selected).map(|(_, v)| v)
    }

    pub async fn show_extension_input(
        &self,
        title: &str,
        placeholder: Option<&str>,
        opts: Option<ExtensionUIDialogOptions>,
    ) -> Option<String> {
        if let Some(ref o) = opts
            && let Some(ref sig) = o.signal
            && sig.aborted()
        {
            return None;
        }
        let (tx, rx) = tokio::sync::oneshot::channel();
        let tx_slot = Arc::new(std::sync::Mutex::new(Some(tx)));
        let comp_slot: Arc<std::sync::Mutex<Option<Arc<dyn Component>>>> = Arc::new(std::sync::Mutex::new(None));
        let input_slot: Arc<std::sync::Mutex<Option<ExtensionInputComponent>>> = Arc::new(std::sync::Mutex::new(None));
        let signal_opt = opts.as_ref().and_then(|o| o.signal.clone());
        let timeout_ms = opts.as_ref().and_then(|o| o.timeout_ms);

        let finish: Arc<dyn Fn(Option<String>) + Send + Sync> = {
            let tx_slot = Arc::clone(&tx_slot);
            let comp_slot = Arc::clone(&comp_slot);
            let input_slot = Arc::clone(&input_slot);
            let m = self.clone();
            Arc::new(move |value: Option<String>| {
                if let Some(inp) = input_slot.lock().unwrap_or_else(|e| e.into_inner()).take() {
                    inp.dispose();
                }
                if let Some(comp) = comp_slot.lock().unwrap_or_else(|e| e.into_inner()).take() {
                    m.remove_extension_dialog(&comp);
                }
                if let Some(sender) = tx_slot.lock().unwrap_or_else(|e| e.into_inner()).take() {
                    let _ = sender.send(value);
                }
            })
        };

        let f_sub = Arc::clone(&finish);
        let f_can = Arc::clone(&finish);
        let f_abort = Arc::clone(&finish);
        let input = ExtensionInputComponent::new(
            title,
            placeholder,
            Arc::new(move |v| f_sub(Some(v.to_string()))),
            Arc::new(move || f_can(None)),
            Some(ExtensionInputOptions {
                tui: Some(self.ui()),
                timeout: timeout_ms,
                initial_value: None,
                description: None,
            }),
        );
        let comp_arc: Arc<dyn Component> = Arc::new(input.clone());
        *comp_slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::clone(&comp_arc));
        *input_slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(input.clone());
        let input_for_disp = input.clone();
        self.push_extension_dialog(
            comp_arc,
            Some(Arc::new(move || input_for_disp.dispose())),
            Some(BlockedStatus {
                kind: BlockedKind::Question,
                message: title.to_string(),
            }),
        );

        if let Some(sig) = signal_opt {
            sig.on_abort(move |_| {
                f_abort(None);
            })
            .forget();
        }
        rx.await.unwrap_or(None)
    }

    pub async fn show_extension_editor(&self, title: &str, prefill: Option<&str>) -> Option<String> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let tx_slot = Arc::new(std::sync::Mutex::new(Some(tx)));
        let comp_slot: Arc<std::sync::Mutex<Option<Arc<dyn Component>>>> = Arc::new(std::sync::Mutex::new(None));
        let finish: Arc<dyn Fn(Option<String>) + Send + Sync> = {
            let tx_slot = Arc::clone(&tx_slot);
            let comp_slot = Arc::clone(&comp_slot);
            let m = self.clone();
            Arc::new(move |value: Option<String>| {
                if let Some(comp) = comp_slot.lock().unwrap_or_else(|e| e.into_inner()).take() {
                    m.remove_extension_dialog(&comp);
                }
                if let Some(sender) = tx_slot.lock().unwrap_or_else(|e| e.into_inner()).take() {
                    let _ = sender.send(value);
                }
            })
        };
        let f_sub = Arc::clone(&finish);
        let f_can = Arc::clone(&finish);
        let ext_cmd = self.settings_manager().get_external_editor_command();
        let editor = ExtensionEditorComponent::new(
            self.ui(),
            (*self.inner.keybindings).clone(),
            title,
            prefill,
            Arc::new(move |v| f_sub(Some(v.to_string()))),
            Arc::new(move || f_can(None)),
            Some(ExtensionEditorOptions::default()),
            ext_cmd.as_deref(),
        );
        let comp_arc: Arc<dyn Component> = Arc::new(editor);
        *comp_slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::clone(&comp_arc));
        self.push_extension_dialog(
            comp_arc,
            None,
            Some(BlockedStatus {
                kind: BlockedKind::Question,
                message: title.to_string(),
            }),
        );
        rx.await.unwrap_or(None)
    }
}
