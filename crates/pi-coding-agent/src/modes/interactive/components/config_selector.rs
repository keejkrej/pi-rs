//! Port of packages/coding-agent/src/modes/interactive/components/config-selector.ts

use std::sync::{Arc, Mutex};

use crate::config::CONFIG_DIR_NAME;
pub use crate::core::package_manager::{PathMetadata, ResolvedPaths, ResolvedResource};
pub use crate::core::settings_manager::{PackageFilterSource, PackageSource, SettingsManager};
pub use crate::core::source_info::{BUILTIN_PATH_PREFIX, SourceOrigin, SourceScope, get_resource_path_key};
use crate::utils::paths::{PathInputOptions, is_local_path, resolve_path, shorten_home_path};
use indexmap::{IndexMap, IndexSet};
use pi_tui::{
    Component, Container, Focusable, Input, Spacer, TuiMouseEvent, TuiMouseEventResult, get_keybindings, matches_key,
    truncate_to_width, visible_width,
};

use super::super::theme::theme::{ThemeColor, theme};
use super::dynamic_border::DynamicBorder;
use super::keybinding_hints::{key_hint, raw_key_hint};

fn source_scope_str(scope: SourceScope) -> &'static str {
    match scope {
        SourceScope::User => "user",
        SourceScope::Project => "project",
        SourceScope::Temporary => "temporary",
    }
}

fn source_origin_str(origin: SourceOrigin) -> &'static str {
    match origin {
        SourceOrigin::Package => "package",
        SourceOrigin::TopLevel => "top-level",
    }
}

fn package_source_str(source: &PackageSource) -> &str {
    match source {
        PackageSource::String(s) => s,
        PackageSource::Object(obj) => &obj.source,
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConfigSettings {
    pub packages: Option<Vec<PackageSource>>,
    pub extensions: Option<Vec<String>>,
    pub skills: Option<Vec<String>>,
    pub prompts: Option<Vec<String>>,
    pub themes: Option<Vec<String>>,
}

pub trait ConfigSettingsStore: Send + Sync {
    fn get_global_settings(&self) -> ConfigSettings;
    fn get_project_settings(&self) -> ConfigSettings;
    fn set_packages(&self, packages: Vec<PackageSource>);
    fn set_project_packages(&self, packages: Vec<PackageSource>);
    fn set_extension_paths(&self, paths: Vec<String>);
    fn set_project_extension_paths(&self, paths: Vec<String>);
    fn set_skill_paths(&self, paths: Vec<String>);
    fn set_project_skill_paths(&self, paths: Vec<String>);
    fn set_prompt_template_paths(&self, paths: Vec<String>);
    fn set_project_prompt_template_paths(&self, paths: Vec<String>);
    fn set_theme_paths(&self, paths: Vec<String>);
    fn set_project_theme_paths(&self, paths: Vec<String>);
}

impl ConfigSettingsStore for SettingsManager {
    fn get_global_settings(&self) -> ConfigSettings {
        let s = SettingsManager::get_global_settings(self);
        ConfigSettings {
            packages: s.packages,
            extensions: s.extensions,
            skills: s.skills,
            prompts: s.prompts,
            themes: s.themes,
        }
    }

    fn get_project_settings(&self) -> ConfigSettings {
        let s = SettingsManager::get_project_settings(self);
        ConfigSettings {
            packages: s.packages,
            extensions: s.extensions,
            skills: s.skills,
            prompts: s.prompts,
            themes: s.themes,
        }
    }

    fn set_packages(&self, packages: Vec<PackageSource>) {
        SettingsManager::set_packages(self, packages);
    }

    fn set_project_packages(&self, packages: Vec<PackageSource>) {
        let _ = SettingsManager::set_project_packages(self, packages);
    }

    fn set_extension_paths(&self, paths: Vec<String>) {
        SettingsManager::set_extension_paths(self, paths);
    }

    fn set_project_extension_paths(&self, paths: Vec<String>) {
        let _ = SettingsManager::set_project_extension_paths(self, paths);
    }

    fn set_skill_paths(&self, paths: Vec<String>) {
        SettingsManager::set_skill_paths(self, paths);
    }

    fn set_project_skill_paths(&self, paths: Vec<String>) {
        let _ = SettingsManager::set_project_skill_paths(self, paths);
    }

    fn set_prompt_template_paths(&self, paths: Vec<String>) {
        SettingsManager::set_prompt_template_paths(self, paths);
    }

    fn set_project_prompt_template_paths(&self, paths: Vec<String>) {
        let _ = SettingsManager::set_project_prompt_template_paths(self, paths);
    }

    fn set_theme_paths(&self, paths: Vec<String>) {
        SettingsManager::set_theme_paths(self, paths);
    }

    fn set_project_theme_paths(&self, paths: Vec<String>) {
        let _ = SettingsManager::set_project_theme_paths(self, paths);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResourceType {
    Extensions,
    Skills,
    Prompts,
    Themes,
}

impl ResourceType {
    const ALL: [Self; 4] = [Self::Extensions, Self::Skills, Self::Prompts, Self::Themes];

    fn as_str(self) -> &'static str {
        match self {
            Self::Extensions => "extensions",
            Self::Skills => "skills",
            Self::Prompts => "prompts",
            Self::Themes => "themes",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Extensions => "Extensions",
            Self::Skills => "Skills",
            Self::Prompts => "Prompts",
            Self::Themes => "Themes",
        }
    }

    fn order(self) -> usize {
        match self {
            Self::Extensions => 0,
            Self::Skills => 1,
            Self::Prompts => 2,
            Self::Themes => 3,
        }
    }

    fn paths_from_settings(self, settings: &ConfigSettings) -> Vec<String> {
        match self {
            Self::Extensions => settings.extensions.clone().unwrap_or_default(),
            Self::Skills => settings.skills.clone().unwrap_or_default(),
            Self::Prompts => settings.prompts.clone().unwrap_or_default(),
            Self::Themes => settings.themes.clone().unwrap_or_default(),
        }
    }

    fn filter_entries(self, pkg: &PackageFilterSource) -> Option<&Vec<String>> {
        match self {
            Self::Extensions => pkg.extensions.as_ref(),
            Self::Skills => pkg.skills.as_ref(),
            Self::Prompts => pkg.prompts.as_ref(),
            Self::Themes => pkg.themes.as_ref(),
        }
    }

    fn set_filter_entries(self, pkg: &mut PackageFilterSource, value: Option<Vec<String>>) {
        match self {
            Self::Extensions => pkg.extensions = value,
            Self::Skills => pkg.skills = value,
            Self::Prompts => pkg.prompts = value,
            Self::Themes => pkg.themes = value,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ConfigWriteScope {
    #[default]
    Global,
    Project,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SettingsScope {
    User,
    Project,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProjectOverrideState {
    Inherit,
    Load,
    Unload,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScopedResolvedPaths {
    pub global: ResolvedPaths,
    pub project: ResolvedPaths,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceItem {
    pub path: String,
    pub enabled: bool,
    pub metadata: PathMetadata,
    pub resource_type: ResourceType,
    pub display_name: String,
    pub group_key: String,
    pub subgroup_key: String,
}

#[derive(Clone, Debug)]
struct ResourceSubgroup {
    resource_type: ResourceType,
    label: &'static str,
    items: Vec<ResourceItem>,
}

#[derive(Clone, Debug)]
struct ResourceGroup {
    key: String,
    label: String,
    scope: SourceScope,
    origin: SourceOrigin,
    source: String,
    subgroups: Vec<ResourceSubgroup>,
}

#[derive(Clone, Debug)]
enum FlatEntry {
    Group(ResourceGroup),
    Subgroup {
        subgroup: ResourceSubgroup,
        group_scope: SourceScope,
    },
    Item(ResourceItem),
}

fn relative_path(from: &str, to: &str) -> String {
    pi_js::path::relative(from, to)
}

fn format_base_dir(base_dir: &str) -> String {
    let display_path = shorten_home_path(base_dir, None).replace('\\', "/");
    if display_path.ends_with('/') {
        display_path
    } else {
        format!("{display_path}/")
    }
}

fn get_group_label(metadata: &PathMetadata, agent_dir: &str) -> String {
    if metadata.origin == SourceOrigin::Package {
        return format!("{} ({})", metadata.source, source_scope_str(metadata.scope));
    }
    if metadata.source == "builtin" {
        return if metadata.scope == SourceScope::User {
            "Built-in".to_string()
        } else {
            "Built-in (project override)".to_string()
        };
    }
    if metadata.source == "auto" {
        if let Some(ref base_dir) = metadata.base_dir {
            return if metadata.scope == SourceScope::User {
                format!("User ({})", format_base_dir(base_dir))
            } else {
                format!("Project ({})", format_base_dir(base_dir))
            };
        }
        return if metadata.scope == SourceScope::User {
            format!("User ({})", format_base_dir(agent_dir))
        } else {
            format!("Project ({}/)", *CONFIG_DIR_NAME)
        };
    }
    if metadata.scope == SourceScope::User {
        "User settings".to_string()
    } else {
        "Project settings".to_string()
    }
}

fn build_groups(resolved: &ResolvedPaths, agent_dir: &str) -> Vec<ResourceGroup> {
    let mut group_map: IndexMap<String, ResourceGroup> = IndexMap::new();

    let mut add_to_group = |resources: &[ResolvedResource], resource_type: ResourceType| {
        for res in resources {
            let group_key = format!(
                "{}:{}:{}:{}",
                source_origin_str(res.metadata.origin),
                source_scope_str(res.metadata.scope),
                res.metadata.source,
                res.metadata.base_dir.as_deref().unwrap_or("")
            );

            let group = group_map.entry(group_key.clone()).or_insert_with(|| ResourceGroup {
                key: group_key.clone(),
                label: get_group_label(&res.metadata, agent_dir),
                scope: res.metadata.scope,
                origin: res.metadata.origin,
                source: res.metadata.source.clone(),
                subgroups: Vec::new(),
            });

            let subgroup_key = format!("{group_key}:{}", resource_type.as_str());
            let subgroup_idx = match group.subgroups.iter().position(|sg| sg.resource_type == resource_type) {
                Some(idx) => idx,
                None => {
                    group.subgroups.push(ResourceSubgroup {
                        resource_type,
                        label: resource_type.label(),
                        items: Vec::new(),
                    });
                    group.subgroups.len() - 1
                }
            };
            let subgroup = &mut group.subgroups[subgroup_idx];

            let file_name = pi_js::path::basename(&res.path, None);
            let parent_dir = pi_js::path::dirname(&res.path);
            let parent_folder = pi_js::path::basename(&parent_dir, None);
            let display_name = if res.metadata.source == "builtin" {
                res.path
                    .strip_prefix(BUILTIN_PATH_PREFIX)
                    .unwrap_or(&res.path)
                    .to_string()
            } else if resource_type == ResourceType::Extensions && parent_folder != "extensions" {
                format!("{parent_folder}/{file_name}")
            } else if resource_type == ResourceType::Skills && file_name == "SKILL.md" {
                parent_folder
            } else {
                file_name
            };

            subgroup.items.push(ResourceItem {
                path: res.path.clone(),
                enabled: res.enabled,
                metadata: res.metadata.clone(),
                resource_type,
                display_name,
                group_key,
                subgroup_key,
            });
        }
    };

    add_to_group(&resolved.extensions, ResourceType::Extensions);
    add_to_group(&resolved.skills, ResourceType::Skills);
    add_to_group(&resolved.prompts, ResourceType::Prompts);
    add_to_group(&resolved.themes, ResourceType::Themes);

    let mut groups: Vec<ResourceGroup> = group_map.into_values().collect();
    groups.sort_by(|a, b| {
        let a_pkg = a.origin != SourceOrigin::Package;
        let b_pkg = b.origin != SourceOrigin::Package;
        a_pkg
            .cmp(&b_pkg)
            .then_with(|| {
                let a_user = a.scope != SourceScope::User;
                let b_user = b.scope != SourceScope::User;
                a_user.cmp(&b_user)
            })
            .then_with(|| a.source.cmp(&b.source))
    });

    for group in &mut groups {
        group.subgroups.sort_by_key(|a| a.resource_type.order());
        for subgroup in &mut group.subgroups {
            subgroup.items.sort_by(|a, b| a.display_name.cmp(&b.display_name));
        }
    }

    groups
}

#[derive(Clone)]
struct ConfigSelectorHeader {
    write_scope: Arc<Mutex<ConfigWriteScope>>,
    project_mode_available: bool,
}

impl ConfigSelectorHeader {
    fn new(write_scope: ConfigWriteScope, project_mode_available: bool) -> Self {
        Self {
            write_scope: Arc::new(Mutex::new(write_scope)),
            project_mode_available,
        }
    }

    fn set_write_scope(&self, write_scope: ConfigWriteScope) {
        *self.write_scope.lock().unwrap_or_else(|e| e.into_inner()) = write_scope;
    }
}

impl Component for ConfigSelectorHeader {
    fn render(&self, width: usize) -> Vec<String> {
        let write_scope = *self.write_scope.lock().unwrap_or_else(|e| e.into_inner());
        let t = theme();
        let title = t.bold(if write_scope == ConfigWriteScope::Project {
            "Project Local Resources"
        } else {
            "Global Resources"
        });
        let sep = t.fg(ThemeColor::Muted, " · ");
        let switch_hint = if self.project_mode_available {
            format!("{}{sep}", key_hint(&"tui.input.tab".to_string(), "switch mode"))
        } else {
            String::new()
        };
        let action_hint = if write_scope == ConfigWriteScope::Project {
            raw_key_hint("space", "cycle inherit/+/-")
        } else {
            raw_key_hint("space", "toggle")
        };
        let hint = format!("{switch_hint}{action_hint}{sep}{}", raw_key_hint("esc", "close"));
        let spacing = width
            .saturating_sub(visible_width(&title).max(0) as usize)
            .saturating_sub(visible_width(&hint).max(0) as usize)
            .max(1);
        let scope_hint = if write_scope == ConfigWriteScope::Project {
            t.fg(
                ThemeColor::Muted,
                &format!(
                    "{}/settings.json · inherited global resources are dimmed",
                    *CONFIG_DIR_NAME
                ),
            )
        } else {
            t.fg(
                ThemeColor::Muted,
                &format!("~/{}/agent/settings.json", *CONFIG_DIR_NAME),
            )
        };

        vec![
            truncate_to_width(
                &format!("{title}{}{hint}", " ".repeat(spacing)),
                width as i64,
                Some(""),
                None,
            ),
            truncate_to_width(&scope_hint, width as i64, Some(""), None),
        ]
    }

    fn handle_input(&self, _data: &str) {}

    fn invalidate(&self) {}
}

struct ResourceListState {
    groups_by_scope: (Vec<ResourceGroup>, Vec<ResourceGroup>),
    flat_items: Vec<FlatEntry>,
    filtered_items: Vec<FlatEntry>,
    selected_index: usize,
    write_scope: ConfigWriteScope,
    inherited_enabled_by_key: IndexMap<String, bool>,
    focused: bool,
    on_toggle: Option<Arc<dyn Fn(&ResourceItem, bool) + Send + Sync>>,
    on_cancel: Option<Arc<dyn Fn() + Send + Sync>>,
    on_exit: Option<Arc<dyn Fn() + Send + Sync>>,
    on_switch_mode: Option<Arc<dyn Fn() + Send + Sync>>,
}

struct ResourceListInner {
    search_input: Input,
    max_visible: usize,
    settings_manager: Arc<dyn ConfigSettingsStore>,
    cwd: String,
    agent_dir: String,
    state: Mutex<ResourceListState>,
}

#[derive(Clone)]
pub struct ResourceList {
    inner: Arc<ResourceListInner>,
}

impl ResourceList {
    fn new(
        groups_by_scope: (Vec<ResourceGroup>, Vec<ResourceGroup>),
        settings_manager: Arc<dyn ConfigSettingsStore>,
        cwd: String,
        agent_dir: String,
        terminal_height: Option<usize>,
        write_scope: ConfigWriteScope,
    ) -> Self {
        let mut inherited_enabled_by_key = IndexMap::new();
        for group in &groups_by_scope.0 {
            if group.scope != SourceScope::User {
                continue;
            }
            for subgroup in &group.subgroups {
                for item in &subgroup.items {
                    inherited_enabled_by_key.insert(Self::resource_item_key(item), item.enabled);
                }
            }
        }

        let max_visible = terminal_height.map_or(15, |h| (h.saturating_sub(14)).max(5));
        let mut state = ResourceListState {
            groups_by_scope,
            flat_items: Vec::new(),
            filtered_items: Vec::new(),
            selected_index: 0,
            write_scope,
            inherited_enabled_by_key,
            focused: false,
            on_toggle: None,
            on_cancel: None,
            on_exit: None,
            on_switch_mode: None,
        };
        Self::rebuild_flat_items_locked(&mut state);
        state.filtered_items = state.flat_items.clone();

        Self {
            inner: Arc::new(ResourceListInner {
                search_input: Input::new(None),
                max_visible,
                settings_manager,
                cwd,
                agent_dir,
                state: Mutex::new(state),
            }),
        }
    }

    pub fn set_on_toggle(&self, cb: Option<Arc<dyn Fn(&ResourceItem, bool) + Send + Sync>>) {
        self.inner.state.lock().unwrap_or_else(|e| e.into_inner()).on_toggle = cb;
    }

    pub fn set_on_cancel(&self, cb: Option<Arc<dyn Fn() + Send + Sync>>) {
        self.inner.state.lock().unwrap_or_else(|e| e.into_inner()).on_cancel = cb;
    }

    pub fn set_on_exit(&self, cb: Option<Arc<dyn Fn() + Send + Sync>>) {
        self.inner.state.lock().unwrap_or_else(|e| e.into_inner()).on_exit = cb;
    }

    pub fn set_on_switch_mode(&self, cb: Option<Arc<dyn Fn() + Send + Sync>>) {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .on_switch_mode = cb;
    }

    fn resource_item_key(item: &ResourceItem) -> String {
        format!("{}:{}", item.resource_type.as_str(), get_resource_path_key(&item.path))
    }

    fn item_scope(item: &ResourceItem) -> SettingsScope {
        if item.metadata.scope == SourceScope::Project {
            SettingsScope::Project
        } else {
            SettingsScope::User
        }
    }

    fn top_level_base_dir(&self, scope: SettingsScope) -> String {
        match scope {
            SettingsScope::Project => pi_js::path::join(&[&self.inner.cwd, *CONFIG_DIR_NAME]),
            SettingsScope::User => self.inner.agent_dir.clone(),
        }
    }

    fn active_groups<'a>(state: &'a ResourceListState) -> &'a [ResourceGroup] {
        match state.write_scope {
            ConfigWriteScope::Global => &state.groups_by_scope.0,
            ConfigWriteScope::Project => &state.groups_by_scope.1,
        }
    }

    fn active_groups_mut<'a>(state: &'a mut ResourceListState) -> &'a mut Vec<ResourceGroup> {
        match state.write_scope {
            ConfigWriteScope::Global => &mut state.groups_by_scope.0,
            ConfigWriteScope::Project => &mut state.groups_by_scope.1,
        }
    }

    fn rebuild_flat_items_locked(state: &mut ResourceListState) {
        let mut flat = Vec::new();
        for group in Self::active_groups(state) {
            flat.push(FlatEntry::Group(group.clone()));
            for subgroup in &group.subgroups {
                flat.push(FlatEntry::Subgroup {
                    subgroup: subgroup.clone(),
                    group_scope: group.scope,
                });
                for item in &subgroup.items {
                    flat.push(FlatEntry::Item(item.clone()));
                }
            }
        }
        state.selected_index = flat.iter().position(|e| matches!(e, FlatEntry::Item(_))).unwrap_or(0);
        state.flat_items = flat;
    }

    fn set_write_scope(&self, write_scope: ConfigWriteScope) {
        let query = self.inner.search_input.get_value();
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        state.write_scope = write_scope;
        Self::rebuild_flat_items_locked(&mut state);
        Self::filter_items_locked(&mut state, &query);
    }

    fn select_first_item_locked(state: &mut ResourceListState) {
        state.selected_index = state
            .filtered_items
            .iter()
            .position(|e| matches!(e, FlatEntry::Item(_)))
            .unwrap_or(0);
    }

    fn filter_items_locked(state: &mut ResourceListState, query: &str) {
        if query.trim().is_empty() {
            state.filtered_items = state.flat_items.clone();
            Self::select_first_item_locked(state);
            return;
        }

        let lower_query = query.to_lowercase();
        let mut matching_keys: IndexSet<(String, String, String)> = IndexSet::new();
        for entry in &state.flat_items {
            if let FlatEntry::Item(item) = entry
                && (item.display_name.to_lowercase().contains(&lower_query)
                    || item.resource_type.as_str().contains(&lower_query)
                    || item.path.to_lowercase().contains(&lower_query))
            {
                matching_keys.insert((item.group_key.clone(), item.subgroup_key.clone(), item.path.clone()));
            }
        }

        let mut matching_groups: IndexSet<String> = IndexSet::new();
        let mut matching_subgroups: IndexSet<String> = IndexSet::new();
        for group in Self::active_groups(state) {
            for subgroup in &group.subgroups {
                for item in &subgroup.items {
                    if matching_keys.contains(&(item.group_key.clone(), item.subgroup_key.clone(), item.path.clone())) {
                        matching_groups.insert(group.key.clone());
                        matching_subgroups.insert(item.subgroup_key.clone());
                    }
                }
            }
        }

        let mut next = Vec::new();
        for entry in &state.flat_items {
            match entry {
                FlatEntry::Group(group) if matching_groups.contains(&group.key) => next.push(entry.clone()),
                FlatEntry::Subgroup { subgroup, .. } => {
                    if subgroup
                        .items
                        .iter()
                        .any(|i| matching_subgroups.contains(&i.subgroup_key))
                    {
                        next.push(entry.clone());
                    }
                }
                FlatEntry::Item(item)
                    if matching_keys.contains(&(
                        item.group_key.clone(),
                        item.subgroup_key.clone(),
                        item.path.clone(),
                    )) =>
                {
                    next.push(entry.clone());
                }
                _ => {}
            }
        }
        state.filtered_items = next;
        Self::select_first_item_locked(state);
    }

    fn find_next_item_locked(state: &ResourceListState, from_index: usize, direction: isize) -> usize {
        let mut idx = from_index as isize + direction;
        while idx >= 0 && (idx as usize) < state.filtered_items.len() {
            if matches!(state.filtered_items[idx as usize], FlatEntry::Item(_)) {
                return idx as usize;
            }
            idx += direction;
        }
        from_index
    }

    pub fn update_item(&self, item: &ResourceItem, enabled: bool) {
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        for entry in &mut state.filtered_items {
            if let FlatEntry::Item(existing) = entry
                && existing.path == item.path
                && existing.resource_type == item.resource_type
            {
                existing.enabled = enabled;
            }
        }
        for entry in &mut state.flat_items {
            if let FlatEntry::Item(existing) = entry
                && existing.path == item.path
                && existing.resource_type == item.resource_type
            {
                existing.enabled = enabled;
            }
        }
        for group in Self::active_groups_mut(&mut state) {
            for subgroup in &mut group.subgroups {
                if let Some(found) = subgroup
                    .items
                    .iter_mut()
                    .find(|i| i.path == item.path && i.resource_type == item.resource_type)
                {
                    found.enabled = enabled;
                    return;
                }
            }
        }
    }

    fn pattern_entry_target(entry: &str) -> &str {
        entry
            .strip_prefix('!')
            .or_else(|| entry.strip_prefix('+'))
            .or_else(|| entry.strip_prefix('-'))
            .unwrap_or(entry)
    }

    fn resource_pattern(&self, item: &ResourceItem) -> String {
        if item.metadata.source == "builtin" {
            return item.path.clone();
        }
        let scope = Self::item_scope(item);
        let base_dir = item
            .metadata
            .base_dir
            .clone()
            .unwrap_or_else(|| self.top_level_base_dir(scope));
        relative_path(&base_dir, &item.path)
    }

    fn resource_pattern_for_scope(&self, item: &ResourceItem, scope: SettingsScope) -> String {
        let source_scope = Self::item_scope(item);
        if scope != source_scope || item.metadata.source == "builtin" {
            return item.path.clone();
        }
        let base_dir = item
            .metadata
            .base_dir
            .clone()
            .unwrap_or_else(|| self.top_level_base_dir(source_scope));
        relative_path(&base_dir, &item.path)
    }

    fn package_resource_pattern(item: &ResourceItem) -> String {
        let base_dir = item
            .metadata
            .base_dir
            .clone()
            .unwrap_or_else(|| pi_js::path::dirname(&item.path));
        relative_path(&base_dir, &item.path)
    }

    fn top_level_override_patterns(&self, item: &ResourceItem, scope: SettingsScope) -> IndexSet<String> {
        let base_dir = self.top_level_base_dir(scope);
        let mut patterns = IndexSet::new();
        patterns.insert(self.resource_pattern_for_scope(item, scope));
        patterns.insert(item.path.clone());
        patterns.insert(relative_path(&base_dir, &item.path));
        if let Some(ref item_base_dir) = item.metadata.base_dir {
            patterns.insert(relative_path(item_base_dir, &item.path));
        }
        patterns
    }

    fn package_source_string_matches(
        &self,
        left_source: &str,
        left_scope: SettingsScope,
        right_source: &str,
        right_scope: SettingsScope,
    ) -> bool {
        if left_source == right_source {
            return true;
        }
        if !is_local_path(left_source) || !is_local_path(right_source) {
            return false;
        }
        let left = resolve_path(
            left_source,
            Some(&self.top_level_base_dir(left_scope)),
            PathInputOptions::default(),
        );
        let right = resolve_path(
            right_source,
            Some(&self.top_level_base_dir(right_scope)),
            PathInputOptions::default(),
        );
        left == right
    }

    fn find_matching_package_source(&self, item: &ResourceItem, target_scope: SettingsScope) -> Option<PackageSource> {
        let settings = match target_scope {
            SettingsScope::Project => self.inner.settings_manager.get_project_settings(),
            SettingsScope::User => self.inner.settings_manager.get_global_settings(),
        };
        let item_scope = Self::item_scope(item);
        settings.packages.unwrap_or_default().into_iter().find(|pkg| {
            self.package_source_string_matches(
                item.metadata.source.as_str(),
                item_scope,
                package_source_str(pkg),
                target_scope,
            )
        })
    }

    fn create_package_override_source(&self, item: &ResourceItem) -> PackageSource {
        let source = &item.metadata.source;
        if !is_local_path(source) {
            return PackageSource::Object(PackageFilterSource {
                source: source.clone(),
                autoload: Some(false),
                extensions: None,
                skills: None,
                prompts: None,
                themes: None,
            });
        }
        let source_path = resolve_path(
            source,
            Some(&self.top_level_base_dir(Self::item_scope(item))),
            PathInputOptions::default(),
        );
        let rel = relative_path(&self.top_level_base_dir(SettingsScope::Project), &source_path);
        PackageSource::Object(PackageFilterSource {
            source: if rel.is_empty() { ".".to_string() } else { rel },
            autoload: Some(false),
            extensions: None,
            skills: None,
            prompts: None,
            themes: None,
        })
    }

    fn override_state_from_entries(
        entries: &[String],
        patterns: &IndexSet<String>,
        empty_array_is_unload: bool,
    ) -> ProjectOverrideState {
        if entries.is_empty() && empty_array_is_unload {
            return ProjectOverrideState::Unload;
        }
        let mut state = ProjectOverrideState::Inherit;
        for entry in entries {
            if !patterns.contains(Self::pattern_entry_target(entry)) {
                continue;
            }
            if entry.starts_with('!') || entry.starts_with('-') {
                state = ProjectOverrideState::Unload;
            } else {
                state = ProjectOverrideState::Load;
            }
        }
        state
    }

    fn project_override_state(&self, state: &ResourceListState, item: &ResourceItem) -> ProjectOverrideState {
        if state.write_scope != ConfigWriteScope::Project {
            return ProjectOverrideState::Inherit;
        }
        if item.metadata.origin == SourceOrigin::TopLevel {
            let settings = self.inner.settings_manager.get_project_settings();
            let entries = item.resource_type.paths_from_settings(&settings);
            let patterns = self.top_level_override_patterns(item, SettingsScope::Project);
            return Self::override_state_from_entries(&entries, &patterns, false);
        }
        let Some(PackageSource::Object(pkg)) = self.find_matching_package_source(item, SettingsScope::Project) else {
            return ProjectOverrideState::Inherit;
        };
        let Some(entries) = item.resource_type.filter_entries(&pkg) else {
            return ProjectOverrideState::Inherit;
        };
        let mut patterns = IndexSet::new();
        patterns.insert(Self::package_resource_pattern(item));
        Self::override_state_from_entries(entries, &patterns, pkg.autoload != Some(false))
    }

    fn inherited_enabled(state: &ResourceListState, item: &ResourceItem) -> bool {
        state
            .inherited_enabled_by_key
            .get(&Self::resource_item_key(item))
            .copied()
            .unwrap_or_else(|| {
                if Self::item_scope(item) == SettingsScope::User {
                    item.enabled
                } else {
                    true
                }
            })
    }

    fn is_inherited_global_item(state: &ResourceListState, item: &ResourceItem) -> bool {
        Self::item_scope(item) == SettingsScope::User
            || state
                .inherited_enabled_by_key
                .contains_key(&Self::resource_item_key(item))
    }

    fn next_override_state(&self, state: &ResourceListState, item: &ResourceItem) -> ProjectOverrideState {
        let current = self.project_override_state(state, item);
        let inherited_enabled = Self::inherited_enabled(state, item);
        match current {
            ProjectOverrideState::Inherit => {
                if inherited_enabled {
                    ProjectOverrideState::Unload
                } else {
                    ProjectOverrideState::Load
                }
            }
            ProjectOverrideState::Unload => {
                if inherited_enabled {
                    ProjectOverrideState::Load
                } else {
                    ProjectOverrideState::Inherit
                }
            }
            ProjectOverrideState::Load => {
                if inherited_enabled {
                    ProjectOverrideState::Inherit
                } else {
                    ProjectOverrideState::Unload
                }
            }
        }
    }

    fn set_top_level_paths(&self, scope: SettingsScope, key: ResourceType, paths: Vec<String>) {
        match (scope, key) {
            (SettingsScope::Project, ResourceType::Extensions) => {
                self.inner.settings_manager.set_project_extension_paths(paths);
            }
            (SettingsScope::Project, ResourceType::Skills) => {
                self.inner.settings_manager.set_project_skill_paths(paths);
            }
            (SettingsScope::Project, ResourceType::Prompts) => {
                self.inner.settings_manager.set_project_prompt_template_paths(paths);
            }
            (SettingsScope::Project, ResourceType::Themes) => {
                self.inner.settings_manager.set_project_theme_paths(paths);
            }
            (SettingsScope::User, ResourceType::Extensions) => {
                self.inner.settings_manager.set_extension_paths(paths);
            }
            (SettingsScope::User, ResourceType::Skills) => {
                self.inner.settings_manager.set_skill_paths(paths);
            }
            (SettingsScope::User, ResourceType::Prompts) => {
                self.inner.settings_manager.set_prompt_template_paths(paths);
            }
            (SettingsScope::User, ResourceType::Themes) => {
                self.inner.settings_manager.set_theme_paths(paths);
            }
        }
    }

    fn set_project_top_level_override(
        &self,
        state: &ResourceListState,
        item: &ResourceItem,
        override_state: ProjectOverrideState,
    ) -> bool {
        let settings = self.inner.settings_manager.get_project_settings();
        let current = item.resource_type.paths_from_settings(&settings);
        let inherited = Self::is_inherited_global_item(state, item);
        let pattern = if inherited {
            item.path.clone()
        } else {
            self.resource_pattern_for_scope(item, SettingsScope::Project)
        };
        let patterns = self.top_level_override_patterns(item, SettingsScope::Project);
        let mut updated: Vec<String> = current
            .into_iter()
            .filter(|entry| {
                let target = Self::pattern_entry_target(entry);
                if (entry.starts_with('!') || entry.starts_with('+') || entry.starts_with('-'))
                    && patterns.contains(target)
                {
                    return false;
                }
                !(override_state == ProjectOverrideState::Inherit && inherited && target == pattern)
            })
            .collect();

        if override_state != ProjectOverrideState::Inherit {
            if inherited && item.metadata.source != "builtin" && !updated.contains(&pattern) {
                updated.push(pattern.clone());
            }
            let prefix = if override_state == ProjectOverrideState::Load {
                "+"
            } else {
                "-"
            };
            updated.push(format!("{prefix}{pattern}"));
        }
        self.set_top_level_paths(SettingsScope::Project, item.resource_type, updated);
        true
    }

    fn set_project_package_override(&self, item: &ResourceItem, override_state: ProjectOverrideState) -> bool {
        let settings = self.inner.settings_manager.get_project_settings();
        let mut packages = settings.packages.unwrap_or_default();
        let item_scope = Self::item_scope(item);
        let mut pkg_index = packages.iter().position(|pkg| {
            self.package_source_string_matches(
                &item.metadata.source,
                item_scope,
                package_source_str(pkg),
                SettingsScope::Project,
            )
        });
        if pkg_index.is_none() {
            if override_state == ProjectOverrideState::Inherit {
                return false;
            }
            packages.push(self.create_package_override_source(item));
            pkg_index = Some(packages.len() - 1);
        }
        let Some(idx) = pkg_index else {
            return false;
        };
        let mut filter = match packages.get(idx) {
            Some(PackageSource::Object(obj)) => obj.clone(),
            Some(PackageSource::String(s)) => PackageFilterSource {
                source: s.clone(),
                autoload: None,
                extensions: None,
                skills: None,
                prompts: None,
                themes: None,
            },
            None => return false,
        };

        let pattern = Self::package_resource_pattern(item);
        let mut updated: Vec<String> = item
            .resource_type
            .filter_entries(&filter)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|entry| Self::pattern_entry_target(entry) != pattern)
            .collect();
        if override_state != ProjectOverrideState::Inherit {
            let prefix = if override_state == ProjectOverrideState::Load {
                "+"
            } else {
                "-"
            };
            updated.push(format!("{prefix}{pattern}"));
        }
        item.resource_type
            .set_filter_entries(&mut filter, if updated.is_empty() { None } else { Some(updated) });

        let has_filters = ResourceType::ALL.iter().any(|rt| rt.filter_entries(&filter).is_some());
        if !has_filters {
            if filter.autoload == Some(false) {
                packages.remove(idx);
            } else {
                packages[idx] = PackageSource::String(filter.source);
            }
        } else {
            packages[idx] = PackageSource::Object(filter);
        }
        self.inner.settings_manager.set_project_packages(packages);
        true
    }

    fn toggle_top_level_resource(&self, item: &ResourceItem, enabled: bool) {
        let scope = Self::item_scope(item);
        let settings = match scope {
            SettingsScope::Project => self.inner.settings_manager.get_project_settings(),
            SettingsScope::User => self.inner.settings_manager.get_global_settings(),
        };
        let current = item.resource_type.paths_from_settings(&settings);
        let pattern = self.resource_pattern(item);
        let mut updated: Vec<String> = current
            .into_iter()
            .filter(|p| Self::pattern_entry_target(p) != pattern)
            .collect();
        if enabled {
            updated.push(format!("+{pattern}"));
        } else {
            updated.push(format!("-{pattern}"));
        }
        self.set_top_level_paths(scope, item.resource_type, updated);
    }

    fn toggle_package_resource(&self, item: &ResourceItem, enabled: bool) {
        let scope = Self::item_scope(item);
        let settings = match scope {
            SettingsScope::Project => self.inner.settings_manager.get_project_settings(),
            SettingsScope::User => self.inner.settings_manager.get_global_settings(),
        };
        let mut packages = settings.packages.unwrap_or_default();
        let Some(idx) = packages
            .iter()
            .position(|pkg| package_source_str(pkg) == item.metadata.source)
        else {
            return;
        };
        let mut filter = match &packages[idx] {
            PackageSource::String(s) => PackageFilterSource {
                source: s.clone(),
                autoload: None,
                extensions: None,
                skills: None,
                prompts: None,
                themes: None,
            },
            PackageSource::Object(obj) => obj.clone(),
        };

        let pattern = Self::package_resource_pattern(item);
        let mut updated: Vec<String> = item
            .resource_type
            .filter_entries(&filter)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|p| Self::pattern_entry_target(p) != pattern)
            .collect();
        if enabled {
            updated.push(format!("+{pattern}"));
        } else {
            updated.push(format!("-{pattern}"));
        }
        item.resource_type
            .set_filter_entries(&mut filter, if updated.is_empty() { None } else { Some(updated) });

        let has_filters = ResourceType::ALL.iter().any(|rt| rt.filter_entries(&filter).is_some());
        if !has_filters {
            packages[idx] = PackageSource::String(filter.source);
        } else {
            packages[idx] = PackageSource::Object(filter);
        }
        match scope {
            SettingsScope::Project => self.inner.settings_manager.set_project_packages(packages),
            SettingsScope::User => self.inner.settings_manager.set_packages(packages),
        }
    }

    fn toggle_resource(&self, state: &ResourceListState, item: &ResourceItem) -> Option<bool> {
        if state.write_scope == ConfigWriteScope::Project {
            let next_state = self.next_override_state(state, item);
            let ok = if item.metadata.origin == SourceOrigin::TopLevel {
                self.set_project_top_level_override(state, item, next_state)
            } else {
                self.set_project_package_override(item, next_state)
            };
            if !ok {
                return None;
            }
            return Some(match next_state {
                ProjectOverrideState::Inherit => Self::inherited_enabled(state, item),
                ProjectOverrideState::Load => true,
                ProjectOverrideState::Unload => false,
            });
        }

        let enabled = !item.enabled;
        if item.metadata.origin == SourceOrigin::TopLevel {
            self.toggle_top_level_resource(item, enabled);
        } else {
            self.toggle_package_resource(item, enabled);
        }
        Some(enabled)
    }

    fn render_checkbox(&self, state: &ResourceListState, item: &ResourceItem) -> String {
        let t = theme();
        if state.write_scope == ConfigWriteScope::Project {
            return match self.project_override_state(state, item) {
                ProjectOverrideState::Load => t.fg(ThemeColor::Success, "[+]"),
                ProjectOverrideState::Unload => t.fg(ThemeColor::Warning, "[-]"),
                ProjectOverrideState::Inherit => t.fg(ThemeColor::Dim, if item.enabled { "[x]" } else { "[ ]" }),
            };
        }
        if item.enabled {
            t.fg(ThemeColor::Success, "[x]")
        } else {
            t.fg(ThemeColor::Dim, "[ ]")
        }
    }

    fn get_item_suffix(&self, state: &ResourceListState, item: &ResourceItem) -> String {
        if state.write_scope != ConfigWriteScope::Project {
            return String::new();
        }
        let t = theme();
        let override_state = self.project_override_state(state, item);
        if override_state == ProjectOverrideState::Inherit && Self::is_inherited_global_item(state, item) {
            return t.fg(ThemeColor::Dim, " · global");
        }
        if override_state == ProjectOverrideState::Load {
            return t.fg(ThemeColor::Success, " · project +");
        }
        if override_state == ProjectOverrideState::Unload {
            return t.fg(ThemeColor::Warning, " · project -");
        }
        String::new()
    }

    fn is_dimmed_item(&self, state: &ResourceListState, item: &ResourceItem) -> bool {
        state.write_scope == ConfigWriteScope::Project
            && Self::is_inherited_global_item(state, item)
            && self.project_override_state(state, item) == ProjectOverrideState::Inherit
    }
}

impl Focusable for ResourceList {
    fn focused(&self) -> bool {
        self.inner.state.lock().unwrap_or_else(|e| e.into_inner()).focused
    }

    fn set_focused(&self, value: bool) {
        self.inner.state.lock().unwrap_or_else(|e| e.into_inner()).focused = value;
        self.inner.search_input.set_focused(value);
    }
}

impl Component for ResourceList {
    fn render(&self, width: usize) -> Vec<String> {
        let t = theme();
        let mut lines = self.inner.search_input.render(width);
        lines.push(String::new());

        let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.filtered_items.is_empty() {
            lines.push(t.fg(ThemeColor::Muted, "  No resources found"));
            return lines;
        }

        let len = state.filtered_items.len();
        let max_visible = self.inner.max_visible;
        let max_start = len.saturating_sub(max_visible);
        let start_index = state.selected_index.saturating_sub(max_visible / 2).min(max_start);
        let end_index = (start_index + max_visible).min(len);

        for (i, entry) in state
            .filtered_items
            .iter()
            .enumerate()
            .take(end_index)
            .skip(start_index)
        {
            let is_selected = i == state.selected_index;
            match entry {
                FlatEntry::Group(group) => {
                    let inherited = state.write_scope == ConfigWriteScope::Project && group.scope == SourceScope::User;
                    let label = t.bold(&format!(
                        "{}{}",
                        group.label,
                        if inherited { " · inherited global" } else { "" }
                    ));
                    let color = if inherited { ThemeColor::Dim } else { ThemeColor::Accent };
                    let group_line = t.fg(color, &label);
                    lines.push(truncate_to_width(
                        &format!("  {group_line}"),
                        width as i64,
                        Some(""),
                        None,
                    ));
                }
                FlatEntry::Subgroup { subgroup, group_scope } => {
                    let color = if state.write_scope == ConfigWriteScope::Project && *group_scope == SourceScope::User {
                        ThemeColor::Dim
                    } else {
                        ThemeColor::Muted
                    };
                    let subgroup_line = t.fg(color, subgroup.label);
                    lines.push(truncate_to_width(
                        &format!("    {subgroup_line}"),
                        width as i64,
                        Some(""),
                        None,
                    ));
                }
                FlatEntry::Item(item) => {
                    let cursor = if is_selected { "> " } else { "  " };
                    let dimmed = self.is_dimmed_item(&state, item);
                    let name_text = if is_selected && !dimmed {
                        t.bold(&item.display_name)
                    } else {
                        item.display_name.clone()
                    };
                    let name = if dimmed {
                        t.fg(ThemeColor::Dim, &name_text)
                    } else {
                        name_text
                    };
                    let checkbox = self.render_checkbox(&state, item);
                    let suffix = self.get_item_suffix(&state, item);
                    lines.push(truncate_to_width(
                        &format!("{cursor}    {checkbox} {name}{suffix}"),
                        width as i64,
                        Some("..."),
                        None,
                    ));
                }
            }
        }

        if start_index > 0 || end_index < len {
            let item_count = state
                .filtered_items
                .iter()
                .filter(|e| matches!(e, FlatEntry::Item(_)))
                .count();
            let current_item_index = state
                .filtered_items
                .iter()
                .take(state.selected_index)
                .filter(|e| matches!(e, FlatEntry::Item(_)))
                .count()
                + 1;
            lines.push(t.fg(ThemeColor::Dim, &format!("  ({current_item_index}/{item_count})")));
        }

        lines
    }

    fn handle_input(&self, data: &str) {
        let kb = get_keybindings();

        if kb.matches(data, "tui.select.up") {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            state.selected_index = Self::find_next_item_locked(&state, state.selected_index, -1);
            return;
        }
        if kb.matches(data, "tui.select.down") {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            state.selected_index = Self::find_next_item_locked(&state, state.selected_index, 1);
            return;
        }
        if kb.matches(data, "tui.select.pageUp") {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            let mut target = state.selected_index.saturating_sub(self.inner.max_visible);
            while target < state.filtered_items.len() && !matches!(state.filtered_items[target], FlatEntry::Item(_)) {
                target += 1;
            }
            if target < state.filtered_items.len() {
                state.selected_index = target;
            }
            return;
        }
        if kb.matches(data, "tui.select.pageDown") {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if !state.filtered_items.is_empty() {
                let mut target = (state.selected_index + self.inner.max_visible).min(state.filtered_items.len() - 1);
                while !matches!(state.filtered_items[target], FlatEntry::Item(_)) {
                    if target == 0 {
                        return;
                    }
                    target -= 1;
                }
                state.selected_index = target;
            }
            return;
        }
        if kb.matches(data, "tui.select.cancel") {
            let cb = self
                .inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .on_cancel
                .clone();
            if let Some(cb) = cb {
                cb();
            }
            return;
        }
        if matches_key(data, "ctrl+c") {
            let cb = self
                .inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .on_exit
                .clone();
            if let Some(cb) = cb {
                cb();
            }
            return;
        }
        if kb.matches(data, "tui.input.tab") {
            let cb = self
                .inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .on_switch_mode
                .clone();
            if let Some(cb) = cb {
                cb();
            }
            return;
        }
        if data == " " || kb.matches(data, "tui.select.confirm") {
            let toggled: Option<(
                ResourceItem,
                bool,
                Option<Arc<dyn Fn(&ResourceItem, bool) + Send + Sync>>,
            )> = {
                let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                let entry = state.filtered_items.get(state.selected_index).cloned();
                if let Some(FlatEntry::Item(item)) = entry {
                    if state.write_scope == ConfigWriteScope::Project || Self::item_scope(&item) == SettingsScope::User
                    {
                        self.toggle_resource(&state, &item)
                            .map(|new_enabled| (item, new_enabled, state.on_toggle.clone()))
                    } else {
                        None
                    }
                } else {
                    None
                }
            };
            if let Some((item, new_enabled, cb)) = toggled {
                self.update_item(&item, new_enabled);
                if let Some(cb) = cb {
                    cb(&item, new_enabled);
                }
            }
            return;
        }

        self.inner.search_input.handle_input(data);
        let query = self.inner.search_input.get_value();
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        Self::filter_items_locked(&mut state, &query);
    }

    fn invalidate(&self) {}
}

struct ConfigSelectorInner {
    container: Container,
    header: ConfigSelectorHeader,
    resource_list: ResourceList,
    state: Mutex<(ConfigWriteScope, bool)>,
}

#[derive(Clone)]
pub struct ConfigSelectorComponent {
    inner: Arc<ConfigSelectorInner>,
}

impl ConfigSelectorComponent {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        resolved_paths: &ScopedResolvedPaths,
        settings_manager: Arc<dyn ConfigSettingsStore>,
        cwd: &str,
        agent_dir: &str,
        on_close: Arc<dyn Fn() + Send + Sync>,
        on_exit: Arc<dyn Fn() + Send + Sync>,
        request_render: Arc<dyn Fn() + Send + Sync>,
        terminal_height: Option<usize>,
        write_scope: ConfigWriteScope,
        project_mode_available: bool,
    ) -> Self {
        let groups_by_scope = (
            build_groups(&resolved_paths.global, agent_dir),
            build_groups(&resolved_paths.project, agent_dir),
        );

        let container = Container::new();
        container.add_child(Arc::new(Spacer::new(Some(1))));
        container.add_child(Arc::new(DynamicBorder::default()));
        container.add_child(Arc::new(Spacer::new(Some(1))));

        let header = ConfigSelectorHeader::new(write_scope, project_mode_available);
        container.add_child(Arc::new(header.clone()));
        container.add_child(Arc::new(Spacer::new(Some(1))));

        let resource_list = ResourceList::new(
            groups_by_scope,
            settings_manager,
            cwd.to_string(),
            agent_dir.to_string(),
            terminal_height,
            write_scope,
        );
        resource_list.set_on_cancel(Some(on_close));
        resource_list.set_on_exit(Some(on_exit));
        let rr_toggle = Arc::clone(&request_render);
        resource_list.set_on_toggle(Some(Arc::new(move |_, _| {
            rr_toggle();
        })));
        container.add_child(Arc::new(resource_list.clone()));
        container.add_child(Arc::new(Spacer::new(Some(1))));
        container.add_child(Arc::new(DynamicBorder::default()));

        let comp = Self {
            inner: Arc::new(ConfigSelectorInner {
                container,
                header,
                resource_list: resource_list.clone(),
                state: Mutex::new((write_scope, false)),
            }),
        };

        if project_mode_available {
            let comp_clone = comp.clone();
            let rr_switch = request_render;
            resource_list.set_on_switch_mode(Some(Arc::new(move || {
                comp_clone.switch_write_scope();
                rr_switch();
            })));
        }

        comp
    }

    fn switch_write_scope(&self) {
        let next = {
            let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            guard.0 = if guard.0 == ConfigWriteScope::Global {
                ConfigWriteScope::Project
            } else {
                ConfigWriteScope::Global
            };
            guard.0
        };
        self.inner.header.set_write_scope(next);
        self.inner.resource_list.set_write_scope(next);
    }

    pub fn get_resource_list(&self) -> &ResourceList {
        &self.inner.resource_list
    }
}

impl Focusable for ConfigSelectorComponent {
    fn focused(&self) -> bool {
        self.inner.state.lock().unwrap_or_else(|e| e.into_inner()).1
    }

    fn set_focused(&self, value: bool) {
        self.inner.state.lock().unwrap_or_else(|e| e.into_inner()).1 = value;
        self.inner.resource_list.set_focused(value);
    }
}

impl Component for ConfigSelectorComponent {
    fn render(&self, width: usize) -> Vec<String> {
        self.inner.container.render(width)
    }

    fn handle_input(&self, data: &str) {
        self.inner.resource_list.handle_input(data);
    }

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.inner.container.handle_mouse(event)
    }

    fn invalidate(&self) {
        self.inner.container.invalidate();
    }
}
