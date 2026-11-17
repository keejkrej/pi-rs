//! Port of packages/coding-agent/src/core/package-manager.ts

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::Duration;

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use indexmap::{IndexMap, IndexSet};
use node_semver::{Range, Version};
use pi_js::fs::{Dirent, exists, mkdir, read_to_string, readdir, readdir_with_file_types, rm, stat, write_file};
use pi_js::path::{basename, dirname, join, relative, resolve, sep};
use pi_js::{BoxFuture, Error, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::config::CONFIG_DIR_NAME;
use crate::core::output_guard::is_stdout_taken_over;
use crate::core::pi_manifest::{PiManifest, read_pi_manifest};
use crate::core::settings_manager::{PackageFilterSource, PackageSource, Settings, SettingsManager};
use crate::core::source_info::{BUILTIN_PATH_PREFIX, PathMetadata as SourcePathMetadata, SourceOrigin, SourceScope};
use crate::utils::child_process::{SpawnProcessOptions, spawn_process, spawn_process_sync, wait_for_child_process};
use crate::utils::git::{GitSource, parse_git_url};
use crate::utils::paths::{
    PathInputOptions, canonicalize_path, is_local_path, mark_path_ignored_by_cloud_sync, resolve_path,
};
use crate::utils::text::strip_bom;
use crate::vendor::minimatch::minimatch;

const NETWORK_TIMEOUT_MS: u64 = 10_000;
const UPDATE_CHECK_CONCURRENCY: usize = 4;
const GIT_UPDATE_CONCURRENCY: usize = 4;
const IGNORE_FILE_NAMES: &[&str] = &[".gitignore", ".ignore", ".fdignore"];

static NPM_SPEC_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(@?[^@]+(?:/[^@]+)?)(?:@(.+))?$").expect("valid regex"));
static EXT_FILE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\.(ts|js)$").expect("valid regex"));
static MD_FILE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\.md$").expect("valid regex"));
static JSON_FILE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\.json$").expect("valid regex"));
static REMOTE_COMMIT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^([0-9a-f]{40})\s+").expect("valid regex"));
static REMOTE_HEAD_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^([0-9a-f]{40})\s+HEAD$").expect("valid regex"));

pub(crate) fn is_offline_mode_enabled() -> bool {
    let Some(value) = pi_js::env::var("PI_OFFLINE") else {
        return false;
    };
    if value.is_empty() {
        return false;
    }
    value == "1" || value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("yes")
}

fn is_exact_npm_version(version: Option<&str>) -> bool {
    let Some(v) = version else {
        return false;
    };
    let trimmed = v.trim();
    let cleaned = trimmed
        .strip_prefix('v')
        .or_else(|| trimmed.strip_prefix('='))
        .unwrap_or(trimmed);
    Version::parse(cleaned).is_ok()
}

fn get_npm_version_range(version: Option<&str>) -> Option<Range> {
    let v = version?.trim();
    if v.is_empty() {
        return None;
    }
    Range::parse(v).ok()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PathMetadata {
    pub source: String,
    pub scope: SourceScope,
    pub origin: SourceOrigin,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_root: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changelog_path: Option<String>,
}

impl PathMetadata {
    pub fn to_source_metadata(&self) -> SourcePathMetadata {
        SourcePathMetadata {
            source: self.source.clone(),
            scope: self.scope,
            origin: self.origin,
            base_dir: self.base_dir.clone(),
            changelog_path: self.changelog_path.clone(),
        }
    }
}

impl From<&PathMetadata> for SourcePathMetadata {
    fn from(value: &PathMetadata) -> Self {
        value.to_source_metadata()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedResource {
    pub path: String,
    pub enabled: bool,
    pub metadata: PathMetadata,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedPaths {
    pub extensions: Vec<ResolvedResource>,
    pub skills: Vec<ResolvedResource>,
    pub prompts: Vec<ResolvedResource>,
    pub themes: Vec<ResolvedResource>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MissingSourceAction {
    Install,
    Skip,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProgressEventType {
    Start,
    Progress,
    Complete,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProgressAction {
    Install,
    Remove,
    Update,
    Clone,
    Pull,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressEvent {
    #[serde(rename = "type")]
    pub event_type: ProgressEventType,
    pub action: ProgressAction,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

pub type ProgressCallback = Arc<dyn Fn(ProgressEvent) + Send + Sync>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PackageUpdateType {
    Npm,
    Git,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InstalledSourceScope {
    User,
    Project,
}

impl From<InstalledSourceScope> for SourceScope {
    fn from(scope: InstalledSourceScope) -> Self {
        match scope {
            InstalledSourceScope::User => Self::User,
            InstalledSourceScope::Project => Self::Project,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageUpdate {
    pub source: String,
    pub display_name: String,
    #[serde(rename = "type")]
    pub package_type: PackageUpdateType,
    pub scope: InstalledSourceScope,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfiguredPackage {
    pub source: String,
    pub scope: InstalledSourceScope,
    pub filtered: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_path: Option<String>,
}

pub type MissingSourceCallback = Arc<dyn Fn(String) -> BoxFuture<Result<MissingSourceAction>> + Send + Sync>;

#[async_trait::async_trait]
pub trait PackageManager: Send + Sync {
    async fn resolve(&self, on_missing: Option<MissingSourceCallback>) -> Result<ResolvedPaths>;
    async fn install(&self, source: &str, local: bool) -> Result<()>;
    async fn install_and_persist(&self, source: &str, local: bool) -> Result<()>;
    async fn remove(&self, source: &str, local: bool) -> Result<()>;
    async fn remove_and_persist(&self, source: &str, local: bool) -> Result<bool>;
    async fn update(&self, source: Option<&str>) -> Result<()>;
    fn list_configured_packages(&self) -> Result<Vec<ConfiguredPackage>>;
    async fn resolve_extension_sources(
        &self,
        sources: &[String],
        local: bool,
        temporary: bool,
    ) -> Result<ResolvedPaths>;
    fn add_source_to_settings(&self, source: &str, local: bool) -> Result<bool>;
    fn remove_source_from_settings(&self, source: &str, local: bool) -> Result<bool>;
    fn set_progress_callback(&self, callback: Option<ProgressCallback>);
    fn get_installed_path(&self, source: &str, scope: InstalledSourceScope) -> Result<Option<String>>;
}

#[async_trait::async_trait]
pub trait PackageCommandRunner: Send + Sync {
    async fn run_command(&self, command: &str, args: &[&str], cwd: Option<&str>) -> Result<()>;
    async fn run_command_capture(
        &self,
        command: &str,
        args: &[&str],
        cwd: Option<&str>,
        timeout_ms: Option<u64>,
        extra_env: Option<&IndexMap<String, String>>,
    ) -> Result<String>;
    fn run_command_sync(&self, command: &str, args: &[&str]) -> Result<String>;
}

#[derive(Clone)]
pub struct PackageManagerOptions {
    pub cwd: String,
    pub agent_dir: String,
    pub settings_manager: SettingsManager,
    pub builtin_extensions: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct NpmSource {
    pub spec: String,
    pub name: String,
    pub version: Option<String>,
    pub range: Option<Range>,
    pub pinned: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalSource {
    pub path: String,
}

#[derive(Clone, Debug)]
pub enum ParsedSource {
    Npm(NpmSource),
    Git(GitSource),
    Local(LocalSource),
}

#[derive(Clone)]
struct ConfiguredUpdateSource {
    source: String,
    scope: InstalledSourceScope,
}

#[derive(Clone)]
struct NpmUpdateTarget {
    source: String,
    scope: InstalledSourceScope,
    parsed: NpmSource,
}

#[derive(Clone)]
struct GitUpdateTarget {
    source: String,
    scope: InstalledSourceScope,
    parsed: GitSource,
}

#[derive(Clone)]
struct ResourceEntry {
    metadata: PathMetadata,
    enabled: bool,
}

#[derive(Default)]
struct ResourceAccumulator {
    extensions: IndexMap<String, ResourceEntry>,
    skills: IndexMap<String, ResourceEntry>,
    prompts: IndexMap<String, ResourceEntry>,
    themes: IndexMap<String, ResourceEntry>,
}

fn resource_precedence_rank(m: &PathMetadata) -> u8 {
    if m.source == "builtin" {
        return 5;
    }
    if m.origin == SourceOrigin::Package {
        return 4;
    }
    let scope_base = if m.scope == SourceScope::Project { 0 } else { 2 };
    scope_base + if m.source == "local" { 0 } else { 1 }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResourceKind {
    Extensions,
    Skills,
    Prompts,
    Themes,
}

const RESOURCE_KINDS: &[ResourceKind] = &[
    ResourceKind::Extensions,
    ResourceKind::Skills,
    ResourceKind::Prompts,
    ResourceKind::Themes,
];

impl ResourceKind {
    const fn dir_name(self) -> &'static str {
        match self {
            Self::Extensions => "extensions",
            Self::Skills => "skills",
            Self::Prompts => "prompts",
            Self::Themes => "themes",
        }
    }

    fn matches_file(self, filename: &str) -> bool {
        match self {
            Self::Extensions => EXT_FILE_RE.is_match(filename),
            Self::Skills | Self::Prompts => MD_FILE_RE.is_match(filename),
            Self::Themes => JSON_FILE_RE.is_match(filename),
        }
    }
}

fn to_posix_path(p: &str) -> String {
    let s = sep();
    if s == "/" {
        p.to_owned()
    } else {
        p.split(s).collect::<Vec<_>>().join("/")
    }
}

fn get_home_dir() -> String {
    let home = pi_js::env::var("HOME").unwrap_or_default();
    if home.is_empty() { pi_js::env::home_dir() } else { home }
}

pub fn get_extension_temp_folder(agent_dir: &str) -> Result<String> {
    let temp_folder = join(&[agent_dir, "tmp", "extensions"]);
    mkdir(&temp_folder, true, Some(0o700))?;
    let _ = pi_js::fs::chmod(&temp_folder, 0o700);
    Ok(temp_folder)
}

fn prefix_ignore_pattern(line: &str, prefix: &str) -> Option<String> {
    let trimmed = pi_js::str16::trim(line);
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with('#') && !trimmed.starts_with("\\#") {
        return None;
    }

    let mut pattern = line;
    let mut negated = false;
    if let Some(rest) = pattern.strip_prefix('!') {
        negated = true;
        pattern = rest;
    } else if let Some(rest) = pattern.strip_prefix("\\!") {
        pattern = rest;
    }
    if let Some(rest) = pattern.strip_prefix('/') {
        pattern = rest;
    }

    let prefixed = if prefix.is_empty() {
        pattern.to_owned()
    } else {
        format!("{prefix}{pattern}")
    };
    Some(if negated { format!("!{prefixed}") } else { prefixed })
}

fn add_ignore_rules(lines: &mut Vec<String>, dir: &str, root_dir: &str) {
    let relative_dir = relative(root_dir, dir);
    let prefix = if relative_dir.is_empty() {
        String::new()
    } else {
        format!("{}/", to_posix_path(&relative_dir))
    };

    for &filename in IGNORE_FILE_NAMES {
        let ignore_path = join(&[dir, filename]);
        if !exists(&ignore_path) {
            continue;
        }
        if let Ok(content) = read_to_string(&ignore_path) {
            for raw_line in content.split('\n') {
                let clean = raw_line.strip_suffix('\r').unwrap_or(raw_line);
                if let Some(pattern) = prefix_ignore_pattern(clean, &prefix) {
                    lines.push(pattern);
                }
            }
        }
    }
}

fn build_gitignore_matcher(root: &str, lines: &[String]) -> Gitignore {
    if lines.is_empty() {
        return Gitignore::empty();
    }
    let mut builder = GitignoreBuilder::new(Path::new(root));
    for line in lines {
        let _ = builder.add_line(None, line);
    }
    builder.build().unwrap_or_else(|_| Gitignore::empty())
}

fn is_ignored(matcher: &Gitignore, rel_path: &str, is_dir: bool) -> bool {
    if matcher.is_empty() {
        return false;
    }
    matcher.matched(Path::new(rel_path), is_dir).is_ignore()
}

fn is_pattern(s: &str) -> bool {
    s.starts_with('!') || s.starts_with('+') || s.starts_with('-') || s.contains('*') || s.contains('?')
}

fn is_override_pattern(s: &str) -> bool {
    s.starts_with('!') || s.starts_with('+') || s.starts_with('-')
}

fn has_glob_pattern(s: &str) -> bool {
    s.contains('*') || s.contains('?')
}

fn has_no_hidden_segments(root: &str, path: &str) -> bool {
    let rel = relative(root, path);
    if rel.is_empty() {
        return true;
    }
    let s = sep();
    rel.split(s).all(|segment| segment == ".." || !segment.starts_with('.'))
}

fn walk_glob_candidates(dir: &str, root: &str, out: &mut Vec<String>) {
    let Ok(entries) = readdir_with_file_types(dir) else {
        return;
    };
    for entry in entries {
        if entry.name.starts_with('.') {
            continue;
        }
        let full_path = join(&[dir, &entry.name]);
        let (is_dir, _) = resolve_entry_type(&full_path, &entry);
        let rel_posix = to_posix_path(&relative(root, &full_path));
        out.push(rel_posix);
        if is_dir {
            walk_glob_candidates(&full_path, root, out);
        }
    }
}

/// Glob entries discover visible paths; exact entries can target dot paths or symlinked trees.
fn expand_package_glob(pattern: &str, root: &str) -> Vec<String> {
    if !exists(root) {
        return Vec::new();
    }
    let norm_pattern = to_posix_path(
        pattern
            .strip_prefix("./")
            .or_else(|| pattern.strip_prefix(".\\"))
            .unwrap_or(pattern),
    );
    let mut candidates = Vec::new();
    walk_glob_candidates(root, root, &mut candidates);
    let mut matches: Vec<String> = candidates
        .into_iter()
        .filter(|rel_posix| minimatch(rel_posix, &norm_pattern, None))
        .map(|rel_posix| resolve(&[root, &rel_posix]))
        .filter(|path| has_no_hidden_segments(root, path))
        .collect();
    matches.sort();
    matches
}

fn split_patterns(entries: &[String]) -> (Vec<String>, Vec<String>) {
    let mut plain = Vec::new();
    let mut patterns = Vec::new();
    for entry in entries {
        if is_pattern(entry) {
            patterns.push(entry.clone());
        } else {
            plain.push(entry.clone());
        }
    }
    (plain, patterns)
}

fn resolve_entry_type(full_path: &str, entry: &Dirent) -> (bool, bool) {
    if entry.is_symbolic_link() {
        match stat(full_path) {
            Ok(stats) => (stats.is_directory(), stats.is_file()),
            Err(_) => (false, false),
        }
    } else {
        (entry.is_directory(), entry.is_file())
    }
}

fn collect_files(
    dir: &str,
    kind: ResourceKind,
    skip_node_modules: bool,
    ignore_lines: Option<&Vec<String>>,
    root_dir: Option<&str>,
) -> Vec<String> {
    let mut files = Vec::new();
    if !exists(dir) {
        return files;
    }

    let root = root_dir.unwrap_or(dir);
    let mut local_lines = ignore_lines.cloned().unwrap_or_default();
    add_ignore_rules(&mut local_lines, dir, root);
    let matcher = build_gitignore_matcher(root, &local_lines);

    let Ok(entries) = readdir_with_file_types(dir) else {
        return files;
    };
    for entry in entries {
        if entry.name.starts_with('.') {
            continue;
        }
        if skip_node_modules && entry.name == "node_modules" {
            continue;
        }
        let full_path = join(&[dir, &entry.name]);
        let (is_dir, is_file) = resolve_entry_type(&full_path, &entry);
        let rel_path = to_posix_path(&relative(root, &full_path));
        if is_ignored(&matcher, &rel_path, is_dir) {
            continue;
        }
        if is_dir {
            files.extend(collect_files(
                &full_path,
                kind,
                skip_node_modules,
                Some(&local_lines),
                Some(root),
            ));
        } else if is_file && kind.matches_file(&entry.name) {
            files.push(full_path);
        }
    }
    files
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SkillDiscoveryMode {
    Pi,
    Agents,
}

fn collect_skill_entries(
    dir: &str,
    mode: SkillDiscoveryMode,
    ignore_lines: Option<&Vec<String>>,
    root_dir: Option<&str>,
) -> Vec<String> {
    let mut entries_out = Vec::new();
    if !exists(dir) {
        return entries_out;
    }
    let root = root_dir.unwrap_or(dir);
    let mut local_lines = ignore_lines.cloned().unwrap_or_default();
    add_ignore_rules(&mut local_lines, dir, root);
    let matcher = build_gitignore_matcher(root, &local_lines);

    let Ok(dir_entries) = readdir_with_file_types(dir) else {
        return entries_out;
    };

    for entry in &dir_entries {
        if entry.name != "SKILL.md" {
            continue;
        }
        let full_path = join(&[dir, &entry.name]);
        let (_, is_file) = resolve_entry_type(&full_path, entry);
        let rel_path = to_posix_path(&relative(root, &full_path));
        if is_file && !is_ignored(&matcher, &rel_path, false) {
            entries_out.push(full_path);
            return entries_out;
        }
    }

    for entry in dir_entries {
        if entry.name.starts_with('.') || entry.name == "node_modules" {
            continue;
        }
        let full_path = join(&[dir, &entry.name]);
        let (is_dir, is_file) = resolve_entry_type(&full_path, &entry);
        let rel_path = to_posix_path(&relative(root, &full_path));
        let should_include_md = is_file
            && entry.name.ends_with(".md")
            && !is_ignored(&matcher, &rel_path, false)
            && ((mode == SkillDiscoveryMode::Pi && dir == root) || (mode == SkillDiscoveryMode::Agents && dir != root));
        if should_include_md {
            entries_out.push(full_path);
            continue;
        }
        if !is_dir || is_ignored(&matcher, &rel_path, true) {
            continue;
        }
        entries_out.extend(collect_skill_entries(&full_path, mode, Some(&local_lines), Some(root)));
    }
    entries_out
}

fn find_git_repo_root(start_dir: &str) -> Option<String> {
    let mut dir = resolve(&[start_dir]);
    loop {
        if exists(&join(&[&dir, ".git"])) {
            return Some(dir);
        }
        let parent = dirname(&dir);
        if parent == dir {
            return None;
        }
        dir = parent;
    }
}

fn collect_ancestor_agents_skill_dirs(start_dir: &str) -> Vec<String> {
    let mut skill_dirs = Vec::new();
    let resolved_start = resolve(&[start_dir]);
    let git_repo_root = find_git_repo_root(&resolved_start);

    let mut dir = resolved_start;
    loop {
        skill_dirs.push(join(&[&dir, ".agents", "skills"]));
        if git_repo_root.as_deref() == Some(dir.as_str()) {
            break;
        }
        let parent = dirname(&dir);
        if parent == dir {
            break;
        }
        dir = parent;
    }
    skill_dirs
}

fn collect_auto_flat_entries(dir: &str, suffix: &str) -> Vec<String> {
    let mut out = Vec::new();
    if !exists(dir) {
        return out;
    }
    let mut lines = Vec::new();
    add_ignore_rules(&mut lines, dir, dir);
    let matcher = build_gitignore_matcher(dir, &lines);

    let Ok(entries) = readdir_with_file_types(dir) else {
        return out;
    };
    for entry in entries {
        if entry.name.starts_with('.') || entry.name == "node_modules" {
            continue;
        }
        let full_path = join(&[dir, &entry.name]);
        let (_, is_file) = resolve_entry_type(&full_path, &entry);
        let rel_path = to_posix_path(&relative(dir, &full_path));
        if is_ignored(&matcher, &rel_path, false) {
            continue;
        }
        if is_file && entry.name.ends_with(suffix) {
            out.push(full_path);
        }
    }
    out
}

fn resolve_extension_entries(dir: &str) -> Option<Vec<String>> {
    let package_json_path = join(&[dir, "package.json"]);
    if exists(&package_json_path)
        && let Some(manifest) = read_pi_manifest(&package_json_path)
        && let Some(exts) = manifest.extensions
        && !exts.is_empty()
    {
        let entries: Vec<String> = exts
            .into_iter()
            .map(|p| resolve(&[dir, &p]))
            .filter(|p| exists(p))
            .collect();
        if !entries.is_empty() {
            return Some(entries);
        }
    }

    let index_ts = join(&[dir, "index.ts"]);
    if exists(&index_ts) {
        return Some(vec![index_ts]);
    }
    let index_js = join(&[dir, "index.js"]);
    if exists(&index_js) {
        return Some(vec![index_js]);
    }
    None
}

fn collect_auto_extension_entries(dir: &str) -> Vec<String> {
    let mut entries = Vec::new();
    if !exists(dir) {
        return entries;
    }
    if let Some(root_entries) = resolve_extension_entries(dir) {
        return root_entries;
    }

    let mut lines = Vec::new();
    add_ignore_rules(&mut lines, dir, dir);
    let matcher = build_gitignore_matcher(dir, &lines);

    let Ok(dir_entries) = readdir_with_file_types(dir) else {
        return entries;
    };
    for entry in dir_entries {
        if entry.name.starts_with('.') || entry.name == "node_modules" {
            continue;
        }
        let full_path = join(&[dir, &entry.name]);
        let (is_dir, is_file) = resolve_entry_type(&full_path, &entry);
        let rel_path = to_posix_path(&relative(dir, &full_path));
        if is_ignored(&matcher, &rel_path, is_dir) {
            continue;
        }
        if is_file && (entry.name.ends_with(".ts") || entry.name.ends_with(".js")) {
            entries.push(full_path);
        } else if is_dir && let Some(sub_entries) = resolve_extension_entries(&full_path) {
            entries.extend(sub_entries);
        }
    }
    entries
}

fn collect_resource_files(dir: &str, kind: ResourceKind) -> Vec<String> {
    match kind {
        ResourceKind::Skills => collect_skill_entries(dir, SkillDiscoveryMode::Pi, None, None),
        ResourceKind::Extensions => collect_auto_extension_entries(dir),
        ResourceKind::Prompts | ResourceKind::Themes => collect_files(dir, kind, true, None, None),
    }
}

fn matches_any_pattern(file_path: &str, patterns: &[String], base_dir: &str) -> bool {
    let rel = to_posix_path(&relative(base_dir, file_path));
    let name = basename(file_path, None);
    let file_path_posix = to_posix_path(file_path);
    let is_skill_file = name == "SKILL.md";
    let parent_dir = if is_skill_file { Some(dirname(file_path)) } else { None };
    let parent_rel = parent_dir.as_ref().map(|pd| to_posix_path(&relative(base_dir, pd)));
    let parent_name = parent_dir.as_ref().map(|pd| basename(pd, None));
    let parent_dir_posix = parent_dir.as_ref().map(|pd| to_posix_path(pd));

    patterns.iter().any(|pattern| {
        let norm = to_posix_path(pattern);
        if minimatch(&rel, &norm, None) || minimatch(&name, &norm, None) || minimatch(&file_path_posix, &norm, None) {
            return true;
        }
        if !is_skill_file {
            return false;
        }
        parent_rel.as_deref().is_some_and(|pr| minimatch(pr, &norm, None))
            || parent_name.as_deref().is_some_and(|pn| minimatch(pn, &norm, None))
            || parent_dir_posix
                .as_deref()
                .is_some_and(|pdp| minimatch(pdp, &norm, None))
    })
}

fn normalize_exact_pattern(pattern: &str) -> String {
    let stripped = pattern
        .strip_prefix("./")
        .or_else(|| pattern.strip_prefix(".\\"))
        .unwrap_or(pattern);
    to_posix_path(stripped)
}

fn matches_any_exact_pattern(file_path: &str, patterns: &[String], base_dir: &str) -> bool {
    if patterns.is_empty() {
        return false;
    }
    let rel = to_posix_path(&relative(base_dir, file_path));
    let name = basename(file_path, None);
    let file_path_posix = to_posix_path(file_path);
    let is_skill_file = name == "SKILL.md";
    let parent_dir = if is_skill_file { Some(dirname(file_path)) } else { None };
    let parent_rel = parent_dir.as_ref().map(|pd| to_posix_path(&relative(base_dir, pd)));
    let parent_dir_posix = parent_dir.as_ref().map(|pd| to_posix_path(pd));

    patterns.iter().any(|pattern| {
        let normalized = normalize_exact_pattern(pattern);
        if normalized == rel || normalized == file_path_posix {
            return true;
        }
        if !is_skill_file {
            return false;
        }
        parent_rel.as_deref() == Some(normalized.as_str()) || parent_dir_posix.as_deref() == Some(normalized.as_str())
    })
}

fn get_override_patterns(entries: &[String]) -> Vec<String> {
    entries
        .iter()
        .filter(|p| p.starts_with('!') || p.starts_with('+') || p.starts_with('-'))
        .cloned()
        .collect()
}

fn is_enabled_by_overrides(file_path: &str, patterns: &[String], base_dir: &str) -> bool {
    let overrides = get_override_patterns(patterns);
    let excludes: Vec<String> = overrides
        .iter()
        .filter_map(|p| p.strip_prefix('!').map(str::to_owned))
        .collect();
    let force_includes: Vec<String> = overrides
        .iter()
        .filter_map(|p| p.strip_prefix('+').map(str::to_owned))
        .collect();
    let force_excludes: Vec<String> = overrides
        .iter()
        .filter_map(|p| p.strip_prefix('-').map(str::to_owned))
        .collect();

    let mut enabled = true;
    if !excludes.is_empty() && matches_any_pattern(file_path, &excludes, base_dir) {
        enabled = false;
    }
    if !force_includes.is_empty() && matches_any_exact_pattern(file_path, &force_includes, base_dir) {
        enabled = true;
    }
    if !force_excludes.is_empty() && matches_any_exact_pattern(file_path, &force_excludes, base_dir) {
        enabled = false;
    }
    enabled
}

fn apply_patterns(all_paths: &[String], patterns: &[String], base_dir: &str) -> IndexSet<String> {
    let mut includes = Vec::new();
    let mut excludes = Vec::new();
    let mut force_includes = Vec::new();
    let mut force_excludes = Vec::new();

    for p in patterns {
        if let Some(rest) = p.strip_prefix('+') {
            force_includes.push(rest.to_owned());
        } else if let Some(rest) = p.strip_prefix('-') {
            force_excludes.push(rest.to_owned());
        } else if let Some(rest) = p.strip_prefix('!') {
            excludes.push(rest.to_owned());
        } else {
            includes.push(p.clone());
        }
    }

    let mut result: Vec<String> = if includes.is_empty() {
        all_paths.to_vec()
    } else {
        all_paths
            .iter()
            .filter(|fp| matches_any_pattern(fp, &includes, base_dir))
            .cloned()
            .collect()
    };

    if !excludes.is_empty() {
        result.retain(|fp| !matches_any_pattern(fp, &excludes, base_dir));
    }

    if !force_includes.is_empty() {
        for fp in all_paths {
            if !result.contains(fp) && matches_any_exact_pattern(fp, &force_includes, base_dir) {
                result.push(fp.clone());
            }
        }
    }

    if !force_excludes.is_empty() {
        result.retain(|fp| !matches_any_exact_pattern(fp, &force_excludes, base_dir));
    }

    result.into_iter().collect()
}

fn apply_autoload_disabled_patterns(
    all_paths: &[String],
    patterns: &[String],
    base_dir: &str,
) -> IndexMap<String, bool> {
    let mut result = IndexMap::new();
    for pattern in patterns {
        let (target, enabled, exact) = if let Some(rest) = pattern.strip_prefix('+') {
            (rest, true, true)
        } else if let Some(rest) = pattern.strip_prefix('-') {
            (rest, false, true)
        } else if let Some(rest) = pattern.strip_prefix('!') {
            (rest, false, false)
        } else {
            (pattern.as_str(), true, false)
        };
        let target_vec = [target.to_owned()];
        for fp in all_paths {
            let matched = if exact {
                matches_any_exact_pattern(fp, &target_vec, base_dir)
            } else {
                matches_any_pattern(fp, &target_vec, base_dir)
            };
            if matched {
                result.insert(fp.clone(), enabled);
            }
        }
    }
    result
}

struct PackageManagerState {
    global_npm_root: Option<String>,
    global_npm_root_command_key: Option<String>,
    progress_callback: Option<ProgressCallback>,
    command_runner: Option<Arc<dyn PackageCommandRunner>>,
}

struct PackageManagerInner {
    cwd: String,
    agent_dir: String,
    settings_manager: SettingsManager,
    builtin_extensions: Vec<String>,
    state: Mutex<PackageManagerState>,
}

#[derive(Clone)]
pub struct DefaultPackageManager {
    inner: Arc<PackageManagerInner>,
}

impl DefaultPackageManager {
    pub fn new(options: PackageManagerOptions) -> Self {
        Self {
            inner: Arc::new(PackageManagerInner {
                cwd: resolve_path(&options.cwd, None, PathInputOptions::default()),
                agent_dir: resolve_path(&options.agent_dir, None, PathInputOptions::default()),
                settings_manager: options.settings_manager,
                builtin_extensions: options.builtin_extensions,
                state: Mutex::new(PackageManagerState {
                    global_npm_root: None,
                    global_npm_root_command_key: None,
                    progress_callback: None,
                    command_runner: None,
                }),
            }),
        }
    }

    pub fn set_command_runner(&self, runner: Option<Arc<dyn PackageCommandRunner>>) {
        self.inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .command_runner = runner;
    }

    fn command_runner(&self) -> Option<Arc<dyn PackageCommandRunner>> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .command_runner
            .clone()
    }

    pub fn set_progress_callback(&self, callback: Option<ProgressCallback>) {
        self.inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .progress_callback = callback;
    }

    fn emit_progress(&self, event: ProgressEvent) {
        let cb = self
            .inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .progress_callback
            .clone();
        if let Some(cb) = cb {
            cb(event);
        }
    }

    async fn with_progress<F, Fut>(&self, action: ProgressAction, source: &str, message: &str, op: F) -> Result<()>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<()>>,
    {
        self.emit_progress(ProgressEvent {
            event_type: ProgressEventType::Start,
            action,
            source: source.to_owned(),
            message: Some(message.to_owned()),
        });
        match op().await {
            Ok(()) => {
                self.emit_progress(ProgressEvent {
                    event_type: ProgressEventType::Complete,
                    action,
                    source: source.to_owned(),
                    message: None,
                });
                Ok(())
            }
            Err(err) => {
                self.emit_progress(ProgressEvent {
                    event_type: ProgressEventType::Error,
                    action,
                    source: source.to_owned(),
                    message: Some(err.message()),
                });
                Err(err)
            }
        }
    }

    pub fn add_source_to_settings(&self, source: &str, local: bool) -> Result<bool> {
        let scope = if local { SourceScope::Project } else { SourceScope::User };
        let current_settings = match scope {
            SourceScope::Project => self.inner.settings_manager.get_project_settings(),
            _ => self.inner.settings_manager.get_global_settings(),
        };
        let current_packages = current_settings.packages.unwrap_or_default();
        let normalized_source = self.normalize_package_source_for_settings(source, scope)?;
        let match_index = current_packages
            .iter()
            .position(|existing| self.package_sources_match(existing, source, scope).unwrap_or(false));
        if let Some(idx) = match_index {
            let existing = &current_packages[idx];
            if Self::get_package_source_string(existing) == normalized_source {
                return Ok(false);
            }
            let mut next_packages = current_packages.clone();
            next_packages[idx] = match existing {
                PackageSource::String(_) => PackageSource::String(normalized_source),
                PackageSource::Object(obj) => {
                    let mut updated = obj.clone();
                    updated.source = normalized_source;
                    PackageSource::Object(updated)
                }
            };
            if scope == SourceScope::Project {
                self.inner.settings_manager.set_project_packages(next_packages)?;
            } else {
                self.inner.settings_manager.set_packages(next_packages);
            }
            return Ok(true);
        }
        let mut next_packages = current_packages;
        next_packages.push(PackageSource::String(normalized_source));
        if scope == SourceScope::Project {
            self.inner.settings_manager.set_project_packages(next_packages)?;
        } else {
            self.inner.settings_manager.set_packages(next_packages);
        }
        Ok(true)
    }

    pub fn remove_source_from_settings(&self, source: &str, local: bool) -> Result<bool> {
        let scope = if local { SourceScope::Project } else { SourceScope::User };
        let current_settings = match scope {
            SourceScope::Project => self.inner.settings_manager.get_project_settings(),
            _ => self.inner.settings_manager.get_global_settings(),
        };
        let current_packages = current_settings.packages.unwrap_or_default();
        let orig_len = current_packages.len();
        let next_packages: Vec<PackageSource> = current_packages
            .into_iter()
            .filter(|existing| !self.package_sources_match(existing, source, scope).unwrap_or(false))
            .collect();
        if next_packages.len() == orig_len {
            return Ok(false);
        }
        if scope == SourceScope::Project {
            self.inner.settings_manager.set_project_packages(next_packages)?;
        } else {
            self.inner.settings_manager.set_packages(next_packages);
        }
        Ok(true)
    }

    pub fn get_installed_path(&self, source: &str, scope: InstalledSourceScope) -> Result<Option<String>> {
        let src_scope = SourceScope::from(scope);
        match self.parse_source(source) {
            ParsedSource::Npm(npm) => {
                let p = self.get_npm_install_path(&npm, src_scope)?;
                Ok(if exists(&p) { Some(p) } else { None })
            }
            ParsedSource::Git(git) => {
                let p = self.get_git_install_path(&git, src_scope)?;
                Ok(if exists(&p) { Some(p) } else { None })
            }
            ParsedSource::Local(local) => {
                let base_dir = self.get_base_dir_for_scope(src_scope)?;
                let p = self.resolve_path_from_base(&local.path, &base_dir);
                Ok(if exists(&p) { Some(p) } else { None })
            }
        }
    }

    pub async fn resolve(&self, on_missing: Option<MissingSourceCallback>) -> Result<ResolvedPaths> {
        let mut accumulator = ResourceAccumulator::default();
        let global_settings = self.inner.settings_manager.get_global_settings();
        let project_settings = self.inner.settings_manager.get_project_settings();

        let mut all_packages: Vec<(PackageSource, SourceScope)> = Vec::new();
        for pkg in project_settings.packages.clone().unwrap_or_default() {
            all_packages.push((pkg, SourceScope::Project));
        }
        for pkg in global_settings.packages.clone().unwrap_or_default() {
            all_packages.push((pkg, SourceScope::User));
        }

        let package_sources = self.dedupe_packages(all_packages)?;
        self.resolve_package_sources(&package_sources, &mut accumulator, on_missing.as_ref())
            .await?;

        let global_base_dir = self.inner.agent_dir.clone();
        let project_base_dir = join(&[&self.inner.cwd, *CONFIG_DIR_NAME]);

        for &kind in RESOURCE_KINDS {
            let project_entries = Self::settings_entries_for_kind(&project_settings, kind);
            let global_entries = Self::settings_entries_for_kind(&global_settings, kind);

            let project_meta = PathMetadata {
                source: "local".to_owned(),
                scope: SourceScope::Project,
                origin: SourceOrigin::TopLevel,
                base_dir: None,
                package_root: None,
                changelog_path: None,
            };
            self.resolve_local_entries(
                project_entries,
                kind,
                Self::get_target_map(&mut accumulator, kind),
                &project_meta,
                &project_base_dir,
            );

            let global_meta = PathMetadata {
                source: "local".to_owned(),
                scope: SourceScope::User,
                origin: SourceOrigin::TopLevel,
                base_dir: None,
                package_root: None,
                changelog_path: None,
            };
            self.resolve_local_entries(
                global_entries,
                kind,
                Self::get_target_map(&mut accumulator, kind),
                &global_meta,
                &global_base_dir,
            );
        }

        self.add_auto_discovered_resources(
            &mut accumulator,
            &global_settings,
            &project_settings,
            &global_base_dir,
            &project_base_dir,
        );

        let project_ext_overrides = get_override_patterns(project_settings.extensions.as_deref().unwrap_or_default());
        let global_ext_entries = global_settings.extensions.as_deref().unwrap_or_default();
        for name in &self.inner.builtin_extensions {
            let path = format!("{BUILTIN_PATH_PREFIX}{name}");
            let single = [path.clone()];
            let project_enabled = apply_autoload_disabled_patterns(&single, &project_ext_overrides, &project_base_dir)
                .get(&path)
                .copied();
            let scope = if project_enabled.is_none() {
                SourceScope::User
            } else {
                SourceScope::Project
            };
            let enabled =
                project_enabled.unwrap_or_else(|| is_enabled_by_overrides(&path, global_ext_entries, &global_base_dir));
            Self::add_resource(
                &mut accumulator.extensions,
                &path,
                &PathMetadata {
                    source: "builtin".to_owned(),
                    scope,
                    origin: SourceOrigin::TopLevel,
                    base_dir: None,
                    package_root: None,
                    changelog_path: None,
                },
                enabled,
            );
        }

        Ok(Self::to_resolved_paths(accumulator))
    }

    pub async fn resolve_extension_sources(
        &self,
        sources: &[String],
        local: bool,
        temporary: bool,
    ) -> Result<ResolvedPaths> {
        let mut accumulator = ResourceAccumulator::default();
        let scope = if temporary {
            SourceScope::Temporary
        } else if local {
            SourceScope::Project
        } else {
            SourceScope::User
        };

        for source in sources {
            if source.starts_with(BUILTIN_PATH_PREFIX) {
                Self::add_resource(
                    &mut accumulator.extensions,
                    source,
                    &PathMetadata {
                        source: "builtin".to_owned(),
                        scope,
                        origin: SourceOrigin::TopLevel,
                        base_dir: None,
                        package_root: None,
                        changelog_path: None,
                    },
                    true,
                );
            }
        }

        let pkg_sources: Vec<(PackageSource, SourceScope)> = sources
            .iter()
            .filter(|s| !s.starts_with(BUILTIN_PATH_PREFIX))
            .map(|s| (PackageSource::String(s.clone()), scope))
            .collect();
        self.resolve_package_sources(&pkg_sources, &mut accumulator, None)
            .await?;
        Ok(Self::to_resolved_paths(accumulator))
    }

    pub fn list_configured_packages(&self) -> Result<Vec<ConfiguredPackage>> {
        let global_settings = self.inner.settings_manager.get_global_settings();
        let project_settings = self.inner.settings_manager.get_project_settings();
        let mut out = Vec::new();

        for pkg in global_settings.packages.unwrap_or_default() {
            let filtered = matches!(pkg, PackageSource::Object(_));
            let source = Self::get_package_source_string(&pkg).to_owned();
            let installed_path = self.get_installed_path(&source, InstalledSourceScope::User)?;
            out.push(ConfiguredPackage {
                source,
                scope: InstalledSourceScope::User,
                filtered,
                installed_path,
            });
        }
        for pkg in project_settings.packages.unwrap_or_default() {
            let filtered = matches!(pkg, PackageSource::Object(_));
            let source = Self::get_package_source_string(&pkg).to_owned();
            let installed_path = self.get_installed_path(&source, InstalledSourceScope::Project)?;
            out.push(ConfiguredPackage {
                source,
                scope: InstalledSourceScope::Project,
                filtered,
                installed_path,
            });
        }
        Ok(out)
    }

    pub async fn install(&self, source: &str, local: bool) -> Result<()> {
        let parsed = self.parse_source(source);
        let scope = if local { SourceScope::Project } else { SourceScope::User };
        self.assert_project_trusted_for_scope(scope)?;
        self.with_progress(
            ProgressAction::Install,
            source,
            &format!("Installing {source}..."),
            || async {
                match parsed {
                    ParsedSource::Npm(npm) => self.install_npm(&npm, scope, false).await,
                    ParsedSource::Git(git) => self.install_git(&git, scope).await,
                    ParsedSource::Local(local_src) => {
                        let resolved = self.resolve_cwd_path(&local_src.path);
                        if !exists(&resolved) {
                            return Err(Error::msg(format!("Path does not exist: {resolved}")));
                        }
                        Ok(())
                    }
                }
            },
        )
        .await
    }

    pub async fn install_and_persist(&self, source: &str, local: bool) -> Result<()> {
        self.install(source, local).await?;
        let _ = self.add_source_to_settings(source, local)?;
        Ok(())
    }

    pub async fn remove(&self, source: &str, local: bool) -> Result<()> {
        let parsed = self.parse_source(source);
        let scope = if local { SourceScope::Project } else { SourceScope::User };
        self.assert_project_trusted_for_scope(scope)?;
        self.with_progress(
            ProgressAction::Remove,
            source,
            &format!("Removing {source}..."),
            || async {
                match parsed {
                    ParsedSource::Npm(npm) => self.uninstall_npm(&npm, scope).await,
                    ParsedSource::Git(git) => self.remove_git(&git, scope),
                    ParsedSource::Local(_) => Ok(()),
                }
            },
        )
        .await
    }

    pub async fn remove_and_persist(&self, source: &str, local: bool) -> Result<bool> {
        self.remove(source, local).await?;
        self.remove_source_from_settings(source, local)
    }

    pub async fn update(&self, source: Option<&str>) -> Result<()> {
        let global_settings = self.inner.settings_manager.get_global_settings();
        let project_settings = self.inner.settings_manager.get_project_settings();
        let identity = source.map(|s| self.get_package_identity(s, None)).transpose()?;
        let mut matched = false;
        let mut update_sources = Vec::new();

        for pkg in global_settings.packages.as_deref().unwrap_or_default() {
            let source_str = Self::get_package_source_string(pkg);
            if let Some(ref target_id) = identity
                && &self.get_package_identity(source_str, Some(SourceScope::User))? != target_id
            {
                continue;
            }
            matched = true;
            update_sources.push(ConfiguredUpdateSource {
                source: source_str.to_owned(),
                scope: InstalledSourceScope::User,
            });
        }
        for pkg in project_settings.packages.as_deref().unwrap_or_default() {
            let source_str = Self::get_package_source_string(pkg);
            if let Some(ref target_id) = identity
                && &self.get_package_identity(source_str, Some(SourceScope::Project))? != target_id
            {
                continue;
            }
            matched = true;
            update_sources.push(ConfiguredUpdateSource {
                source: source_str.to_owned(),
                scope: InstalledSourceScope::Project,
            });
        }

        if let Some(src) = source
            && !matched
        {
            let mut all_pkgs = global_settings.packages.unwrap_or_default();
            all_pkgs.extend(project_settings.packages.unwrap_or_default());
            return Err(Error::msg(self.build_no_matching_package_message(src, &all_pkgs)));
        }

        self.update_configured_sources(&update_sources).await
    }

    async fn update_configured_sources(&self, sources: &[ConfiguredUpdateSource]) -> Result<()> {
        if is_offline_mode_enabled() || sources.is_empty() {
            return Ok(());
        }

        let mut npm_candidates = Vec::new();
        let mut git_candidates = Vec::new();
        for entry in sources {
            match self.parse_source(&entry.source) {
                ParsedSource::Npm(npm) if !npm.pinned => npm_candidates.push(NpmUpdateTarget {
                    source: entry.source.clone(),
                    scope: entry.scope,
                    parsed: npm,
                }),
                ParsedSource::Git(git) => git_candidates.push(GitUpdateTarget {
                    source: entry.source.clone(),
                    scope: entry.scope,
                    parsed: git,
                }),
                _ => {}
            }
        }

        let check_results = run_with_concurrency(
            npm_candidates
                .into_iter()
                .map(|entry| async move {
                    let should = self.should_update_npm_source(&entry.parsed, entry.scope).await;
                    Ok((entry, should))
                })
                .collect(),
            UPDATE_CHECK_CONCURRENCY,
        )
        .await?;

        let mut user_npm_updates = Vec::new();
        let mut project_npm_updates = Vec::new();
        for (entry, should_update) in check_results {
            if !should_update {
                continue;
            }
            match entry.scope {
                InstalledSourceScope::User => user_npm_updates.push(entry),
                InstalledSourceScope::Project => project_npm_updates.push(entry),
            }
        }

        let user_fut = async {
            if !user_npm_updates.is_empty() {
                self.update_npm_batch(&user_npm_updates, InstalledSourceScope::User)
                    .await
            } else {
                Ok(())
            }
        };
        let proj_fut = async {
            if !project_npm_updates.is_empty() {
                self.update_npm_batch(&project_npm_updates, InstalledSourceScope::Project)
                    .await
            } else {
                Ok(())
            }
        };
        let git_fut = async {
            if git_candidates.is_empty() {
                return Ok(());
            }
            let tasks: Vec<_> = git_candidates
                .into_iter()
                .map(|entry| async move {
                    self.with_progress(
                        ProgressAction::Update,
                        &entry.source,
                        &format!("Updating {}...", entry.source),
                        || async { self.update_git(&entry.parsed, SourceScope::from(entry.scope)).await },
                    )
                    .await
                })
                .collect();
            let _ = run_with_concurrency(tasks, GIT_UPDATE_CONCURRENCY).await?;
            Ok(())
        };

        futures::future::try_join3(user_fut, proj_fut, git_fut).await?;
        Ok(())
    }

    async fn should_update_npm_source(&self, source: &NpmSource, scope: InstalledSourceScope) -> bool {
        let Ok(installed_path) = self.get_managed_npm_install_path(source, SourceScope::from(scope)) else {
            return true;
        };
        let installed_version = if exists(&installed_path) {
            self.get_installed_npm_version(&installed_path)
        } else {
            None
        };
        let Some(installed_ver) = installed_version else {
            return true;
        };
        let spec = if source.version.is_some() {
            source.spec.as_str()
        } else {
            source.name.as_str()
        };
        let Ok(target_version) = self.get_latest_npm_version(spec, source.range.as_ref()).await else {
            return true;
        };
        match (Version::parse(&target_version), Version::parse(&installed_ver)) {
            (Ok(t), Ok(i)) => t > i,
            _ => true,
        }
    }

    async fn update_npm_batch(&self, sources: &[NpmUpdateTarget], scope: InstalledSourceScope) -> Result<()> {
        if sources.is_empty() {
            return Ok(());
        }
        let scope_label = match scope {
            InstalledSourceScope::User => "user",
            InstalledSourceScope::Project => "project",
        };
        let source_label = if sources.len() == 1 {
            sources[0].source.clone()
        } else {
            format!("{scope_label} npm packages")
        };
        let message = if sources.len() == 1 {
            format!("Updating {}...", sources[0].source)
        } else {
            format!("Updating {scope_label} npm packages...")
        };
        let specs: Vec<String> = sources
            .iter()
            .map(|e| {
                if e.parsed.version.is_some() {
                    e.parsed.spec.clone()
                } else {
                    format!("{}@latest", e.parsed.name)
                }
            })
            .collect();

        self.with_progress(ProgressAction::Update, &source_label, &message, || async {
            self.install_npm_batch(&specs, SourceScope::from(scope)).await
        })
        .await
    }

    async fn install_npm_batch(&self, specs: &[String], scope: SourceScope) -> Result<()> {
        let install_root = self.get_npm_install_root(scope, false)?;
        self.ensure_npm_project(&install_root)?;
        let args = self.get_npm_install_args(specs, &install_root)?;
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        self.run_npm_command(&arg_refs, None).await
    }

    pub async fn check_for_available_updates(&self) -> Result<Vec<PackageUpdate>> {
        if is_offline_mode_enabled() {
            return Ok(Vec::new());
        }
        let global_settings = self.inner.settings_manager.get_global_settings();
        let project_settings = self.inner.settings_manager.get_project_settings();
        let mut all_packages: Vec<(PackageSource, SourceScope)> = Vec::new();
        for pkg in project_settings.packages.unwrap_or_default() {
            all_packages.push((pkg, SourceScope::Project));
        }
        for pkg in global_settings.packages.unwrap_or_default() {
            all_packages.push((pkg, SourceScope::User));
        }

        let package_sources = self.dedupe_packages(all_packages)?;
        let tasks: Vec<_> = package_sources
            .into_iter()
            .filter_map(|(pkg, scope)| {
                let installed_scope = match scope {
                    SourceScope::User => InstalledSourceScope::User,
                    SourceScope::Project => InstalledSourceScope::Project,
                    SourceScope::Temporary => return None,
                };
                Some(async move {
                    let source = Self::get_package_source_string(&pkg).to_owned();
                    match self.parse_source(&source) {
                        ParsedSource::Local(_) => Ok(None),
                        ParsedSource::Npm(npm) => {
                            if npm.pinned {
                                return Ok(None);
                            }
                            let installed_path = self.get_npm_install_path(&npm, scope)?;
                            if !exists(&installed_path) {
                                return Ok(None);
                            }
                            if !self.npm_has_available_update(&npm, &installed_path).await {
                                return Ok(None);
                            }
                            Ok(Some(PackageUpdate {
                                source,
                                display_name: npm.name,
                                package_type: PackageUpdateType::Npm,
                                scope: installed_scope,
                            }))
                        }
                        ParsedSource::Git(git) => {
                            if git.pinned {
                                return Ok(None);
                            }
                            let installed_path = self.get_git_install_path(&git, scope)?;
                            if !exists(&installed_path) {
                                return Ok(None);
                            }
                            if !self.git_has_available_update(&installed_path).await {
                                return Ok(None);
                            }
                            Ok(Some(PackageUpdate {
                                source,
                                display_name: format!("{}/{}", git.host, git.path),
                                package_type: PackageUpdateType::Git,
                                scope: installed_scope,
                            }))
                        }
                    }
                })
            })
            .collect();

        let results = run_with_concurrency(tasks, UPDATE_CHECK_CONCURRENCY).await?;
        Ok(results.into_iter().flatten().collect())
    }

    async fn resolve_package_sources(
        &self,
        sources: &[(PackageSource, SourceScope)],
        accumulator: &mut ResourceAccumulator,
        on_missing: Option<&MissingSourceCallback>,
    ) -> Result<()> {
        for (pkg, scope) in sources {
            let source_str = Self::get_package_source_string(pkg);
            let filter = match pkg {
                PackageSource::Object(obj) => Some(obj),
                PackageSource::String(_) => None,
            };
            let delta_base = self.find_autoload_delta_base(pkg, *scope, sources)?;
            let (resolved_source, resolved_scope) = match delta_base {
                Some((s, sc)) => (s, sc),
                None => (source_str.to_owned(), *scope),
            };
            let parsed = self.parse_source(&resolved_source);
            let mut metadata = PathMetadata {
                source: source_str.to_owned(),
                scope: *scope,
                origin: SourceOrigin::Package,
                base_dir: None,
                package_root: None,
                changelog_path: None,
            };

            match parsed {
                ParsedSource::Local(local) => {
                    let base_dir = self.get_base_dir_for_scope(resolved_scope)?;
                    self.resolve_local_extension_source(&local, accumulator, filter, &mut metadata, &base_dir);
                    continue;
                }
                ParsedSource::Npm(npm) => {
                    let mut installed_path = self.get_npm_install_path(&npm, resolved_scope)?;
                    let needs_install = !exists(&installed_path)
                        || !self.installed_npm_matches_configured_version(&npm, &installed_path);
                    if needs_install {
                        let installed = self
                            .install_missing(
                                &resolved_source,
                                &ParsedSource::Npm(npm.clone()),
                                resolved_scope,
                                on_missing,
                            )
                            .await?;
                        if !installed {
                            continue;
                        }
                        installed_path = self.get_npm_install_path(&npm, resolved_scope)?;
                    }
                    metadata.base_dir = Some(installed_path.clone());
                    metadata.package_root = Some(installed_path.clone());
                    self.collect_package_resources(&installed_path, accumulator, filter, &metadata);
                }
                ParsedSource::Git(git) => {
                    let installed_path = self.get_git_install_path(&git, resolved_scope)?;
                    if !exists(&installed_path) {
                        let installed = self
                            .install_missing(
                                &resolved_source,
                                &ParsedSource::Git(git.clone()),
                                resolved_scope,
                                on_missing,
                            )
                            .await?;
                        if !installed {
                            continue;
                        }
                    } else if resolved_scope == SourceScope::Temporary && !git.pinned && !is_offline_mode_enabled() {
                        self.refresh_temporary_git_source(&git, &resolved_source).await;
                    }
                    metadata.base_dir = Some(installed_path.clone());
                    metadata.package_root = Some(installed_path.clone());
                    self.collect_package_resources(&installed_path, accumulator, filter, &metadata);
                }
            }
        }
        Ok(())
    }

    async fn install_missing(
        &self,
        resolved_source: &str,
        parsed: &ParsedSource,
        resolved_scope: SourceScope,
        on_missing: Option<&MissingSourceCallback>,
    ) -> Result<bool> {
        if is_offline_mode_enabled() {
            return Ok(false);
        }
        if let Some(cb) = on_missing {
            let action = cb(resolved_source.to_owned()).await?;
            match action {
                MissingSourceAction::Skip => return Ok(false),
                MissingSourceAction::Error => {
                    return Err(Error::msg(format!("Missing source: {resolved_source}")));
                }
                MissingSourceAction::Install => {}
            }
        }
        match parsed {
            ParsedSource::Npm(npm) => {
                self.install_npm(npm, resolved_scope, resolved_scope == SourceScope::Temporary)
                    .await?
            }
            ParsedSource::Git(git) => self.install_git(git, resolved_scope).await?,
            ParsedSource::Local(_) => {}
        }
        Ok(true)
    }

    fn find_autoload_delta_base(
        &self,
        pkg: &PackageSource,
        scope: SourceScope,
        sources: &[(PackageSource, SourceScope)],
    ) -> Result<Option<(String, SourceScope)>> {
        let PackageSource::Object(obj) = pkg else {
            return Ok(None);
        };
        if scope != SourceScope::Project || obj.autoload != Some(false) {
            return Ok(None);
        }
        let identity = self.get_package_identity(&obj.source, Some(scope))?;
        for (entry_pkg, entry_scope) in sources {
            if *entry_scope == SourceScope::User {
                let entry_src = Self::get_package_source_string(entry_pkg);
                if self.get_package_identity(entry_src, Some(SourceScope::User))? == identity {
                    return Ok(Some((entry_src.to_owned(), SourceScope::User)));
                }
            }
        }
        Ok(None)
    }

    fn resolve_local_extension_source(
        &self,
        source: &LocalSource,
        accumulator: &mut ResourceAccumulator,
        filter: Option<&PackageFilterSource>,
        metadata: &mut PathMetadata,
        base_dir: &str,
    ) {
        let resolved = self.resolve_path_from_base(&source.path, base_dir);
        if !exists(&resolved) {
            return;
        }
        let Ok(stats) = stat(&resolved) else {
            return;
        };
        if stats.is_file() {
            metadata.base_dir = Some(dirname(&resolved));
            Self::add_resource(&mut accumulator.extensions, &resolved, metadata, true);
            return;
        }
        if stats.is_directory() {
            metadata.base_dir = Some(resolved.clone());
            metadata.package_root = Some(resolved.clone());
            let had_resources = self.collect_package_resources(&resolved, accumulator, filter, metadata);
            if !had_resources {
                Self::add_resource(&mut accumulator.extensions, &resolved, metadata, true);
            }
        }
    }

    fn get_package_source_string(pkg: &PackageSource) -> &str {
        match pkg {
            PackageSource::String(s) => s,
            PackageSource::Object(o) => &o.source,
        }
    }

    fn get_source_match_key_for_input(&self, source: &str) -> String {
        match self.parse_source(source) {
            ParsedSource::Npm(npm) => format!("npm:{}", npm.name),
            ParsedSource::Git(git) => format!("git:{}/{}", git.host, git.path),
            ParsedSource::Local(local) => format!("local:{}", self.resolve_cwd_path(&local.path)),
        }
    }

    fn get_source_match_key_for_settings(&self, source: &str, scope: SourceScope) -> Result<String> {
        Ok(match self.parse_source(source) {
            ParsedSource::Npm(npm) => format!("npm:{}", npm.name),
            ParsedSource::Git(git) => format!("git:{}/{}", git.host, git.path),
            ParsedSource::Local(local) => {
                let base_dir = self.get_base_dir_for_scope(scope)?;
                format!("local:{}", self.resolve_path_from_base(&local.path, &base_dir))
            }
        })
    }

    fn build_no_matching_package_message(&self, source: &str, configured: &[PackageSource]) -> String {
        match self.find_suggested_configured_source(source, configured) {
            Some(suggestion) => format!("No matching package found for {source}. Did you mean {suggestion}?"),
            None => format!("No matching package found for {source}"),
        }
    }

    fn find_suggested_configured_source(&self, source: &str, configured: &[PackageSource]) -> Option<String> {
        let trimmed = source.trim();
        for pkg in configured {
            let source_str = Self::get_package_source_string(pkg);
            match self.parse_source(source_str) {
                ParsedSource::Npm(npm) => {
                    if trimmed == npm.name || trimmed == npm.spec {
                        return Some(source_str.to_owned());
                    }
                }
                ParsedSource::Git(git) => {
                    let shorthand = format!("{}/{}", git.host, git.path);
                    let shorthand_with_ref = git.ref_name.as_ref().map(|r| format!("{shorthand}@{r}"));
                    if trimmed == shorthand || shorthand_with_ref.as_deref() == Some(trimmed) {
                        return Some(source_str.to_owned());
                    }
                }
                ParsedSource::Local(_) => {}
            }
        }
        None
    }

    fn package_sources_match(&self, existing: &PackageSource, input_source: &str, scope: SourceScope) -> Result<bool> {
        let left = self.get_source_match_key_for_settings(Self::get_package_source_string(existing), scope)?;
        let right = self.get_source_match_key_for_input(input_source);
        Ok(left == right)
    }

    fn normalize_package_source_for_settings(&self, source: &str, scope: SourceScope) -> Result<String> {
        let ParsedSource::Local(local) = self.parse_source(source) else {
            return Ok(source.to_owned());
        };
        let base_dir = self.get_base_dir_for_scope(scope)?;
        let resolved = self.resolve_cwd_path(&local.path);
        let rel = relative(&base_dir, &resolved);
        Ok(if rel.is_empty() { ".".to_owned() } else { rel })
    }

    pub fn parse_source(&self, source: &str) -> ParsedSource {
        if let Some(rest) = source.strip_prefix("npm:") {
            let spec = rest.trim().to_owned();
            let (name, version) = Self::parse_npm_spec(&spec);
            let range = get_npm_version_range(version.as_deref());
            let pinned = is_exact_npm_version(version.as_deref());
            return ParsedSource::Npm(NpmSource {
                spec,
                name,
                version,
                range,
                pinned,
            });
        }
        if is_local_path(source) {
            return ParsedSource::Local(LocalSource {
                path: source.to_owned(),
            });
        }
        if let Some(git) = parse_git_url(source) {
            return ParsedSource::Git(git);
        }
        ParsedSource::Local(LocalSource {
            path: source.to_owned(),
        })
    }

    fn parse_npm_spec(spec: &str) -> (String, Option<String>) {
        if let Some(caps) = NPM_SPEC_RE.captures(spec) {
            let name = caps
                .get(1)
                .map(|m| m.as_str().to_owned())
                .unwrap_or_else(|| spec.to_owned());
            let version = caps.get(2).map(|m| m.as_str().to_owned());
            return (name, version);
        }
        (spec.to_owned(), None)
    }

    pub fn installed_npm_matches_configured_version(&self, source: &NpmSource, installed_path: &str) -> bool {
        let Some(installed_ver) = self.get_installed_npm_version(installed_path) else {
            return false;
        };
        let Some(ref range) = source.range else {
            return true;
        };
        Version::parse(&installed_ver).is_ok_and(|v| range.satisfies(&v))
    }

    async fn npm_has_available_update(&self, source: &NpmSource, installed_path: &str) -> bool {
        if is_offline_mode_enabled() {
            return false;
        }
        let Some(installed_ver) = self.get_installed_npm_version(installed_path) else {
            return false;
        };
        let spec = if source.version.is_some() {
            source.spec.as_str()
        } else {
            source.name.as_str()
        };
        let Ok(target_ver) = self.get_latest_npm_version(spec, source.range.as_ref()).await else {
            return false;
        };
        match (Version::parse(&target_ver), Version::parse(&installed_ver)) {
            (Ok(t), Ok(i)) => t > i,
            _ => false,
        }
    }

    pub fn get_installed_npm_version(&self, installed_path: &str) -> Option<String> {
        let package_json_path = join(&[installed_path, "package.json"]);
        if !exists(&package_json_path) {
            return None;
        }
        let content = read_to_string(&package_json_path).ok()?;
        let parsed = pi_js::json::parse(strip_bom(&content)).ok()?;
        parsed.get("version").and_then(Value::as_str).map(str::to_owned)
    }

    pub async fn get_latest_npm_version(&self, package_spec: &str, range: Option<&Range>) -> Result<String> {
        let (cmd, base_args) = self.get_npm_command()?;
        let mut args: Vec<&str> = base_args.iter().map(String::as_str).collect();
        args.extend(["view", package_spec, "version", "--json"]);
        let stdout = self
            .run_command_capture(&cmd, &args, Some(&self.inner.cwd), Some(NETWORK_TIMEOUT_MS), None)
            .await?;
        let raw = stdout.trim();
        if raw.is_empty() {
            return Err(Error::msg("Empty response from npm view"));
        }
        let parsed = pi_js::json::parse(raw)?;
        if let Some(s) = parsed.as_str() {
            return Ok(s.to_owned());
        }
        if let Some(arr) = parsed.as_array() {
            let mut versions: Vec<(Version, String)> = arr
                .iter()
                .filter_map(Value::as_str)
                .filter(|s| !s.is_empty())
                .filter_map(|s| Version::parse(s).ok().map(|v| (v, s.to_owned())))
                .collect();
            versions.sort_by(|a, b| b.0.cmp(&a.0));
            let matched = if let Some(r) = range {
                versions.into_iter().find(|(v, _)| r.satisfies(v)).map(|(_, s)| s)
            } else {
                versions.into_iter().next().map(|(_, s)| s)
            };
            if let Some(latest) = matched {
                return Ok(latest);
            }
        }
        Err(Error::msg("Unexpected response from npm view"))
    }

    async fn git_has_available_update(&self, installed_path: &str) -> bool {
        if is_offline_mode_enabled() {
            return false;
        }
        let Ok(local_head) = self
            .run_command_capture(
                "git",
                &["rev-parse", "HEAD"],
                Some(installed_path),
                Some(NETWORK_TIMEOUT_MS),
                None,
            )
            .await
        else {
            return false;
        };
        let Ok(remote_head) = self.get_remote_git_head(installed_path).await else {
            return false;
        };
        local_head.trim() != remote_head.trim()
    }

    async fn get_remote_git_head(&self, installed_path: &str) -> Result<String> {
        if let Some(upstream_ref) = self.get_git_upstream_ref(installed_path).await {
            let output = self
                .run_git_remote_command(installed_path, &["ls-remote", "origin", &upstream_ref])
                .await?;
            if let Some(caps) = REMOTE_COMMIT_RE.captures(&output)
                && let Some(m) = caps.get(1)
            {
                return Ok(m.as_str().to_owned());
            }
        }
        let output = self
            .run_git_remote_command(installed_path, &["ls-remote", "origin", "HEAD"])
            .await?;
        if let Some(caps) = REMOTE_HEAD_RE.captures(&output)
            && let Some(m) = caps.get(1)
        {
            return Ok(m.as_str().to_owned());
        }
        Err(Error::msg("Failed to determine remote HEAD"))
    }

    async fn get_local_git_update_target(&self, installed_path: &str) -> Result<(String, String, Vec<String>)> {
        let upstream_res = self
            .run_command_capture(
                "git",
                &["rev-parse", "--abbrev-ref", "@{upstream}"],
                Some(installed_path),
                Some(NETWORK_TIMEOUT_MS),
                None,
            )
            .await;
        if let Ok(upstream) = upstream_res {
            let trimmed = upstream.trim();
            if let Some(branch) = trimmed.strip_prefix("origin/")
                && !branch.is_empty()
            {
                let head = self
                    .run_command_capture(
                        "git",
                        &["rev-parse", "@{upstream}"],
                        Some(installed_path),
                        Some(NETWORK_TIMEOUT_MS),
                        None,
                    )
                    .await?;
                return Ok((
                    "@{upstream}".to_owned(),
                    head,
                    vec![
                        "fetch".to_owned(),
                        "--prune".to_owned(),
                        "--no-tags".to_owned(),
                        "origin".to_owned(),
                        format!("+refs/heads/{branch}:refs/remotes/origin/{branch}"),
                    ],
                ));
            }
        }

        let _ = self
            .run_command("git", &["remote", "set-head", "origin", "-a"], Some(installed_path))
            .await;
        let head = self
            .run_command_capture(
                "git",
                &["rev-parse", "origin/HEAD"],
                Some(installed_path),
                Some(NETWORK_TIMEOUT_MS),
                None,
            )
            .await?;
        let origin_head_ref = self
            .run_command_capture(
                "git",
                &["symbolic-ref", "refs/remotes/origin/HEAD"],
                Some(installed_path),
                Some(NETWORK_TIMEOUT_MS),
                None,
            )
            .await
            .unwrap_or_default();
        let trimmed_ref = origin_head_ref.trim();
        let branch = trimmed_ref.strip_prefix("refs/remotes/origin/").unwrap_or(trimmed_ref);
        if !branch.is_empty() && trimmed_ref.starts_with("refs/remotes/origin/") {
            Ok((
                "origin/HEAD".to_owned(),
                head,
                vec![
                    "fetch".to_owned(),
                    "--prune".to_owned(),
                    "--no-tags".to_owned(),
                    "origin".to_owned(),
                    format!("+refs/heads/{branch}:refs/remotes/origin/{branch}"),
                ],
            ))
        } else {
            Ok((
                "origin/HEAD".to_owned(),
                head,
                vec![
                    "fetch".to_owned(),
                    "--prune".to_owned(),
                    "--no-tags".to_owned(),
                    "origin".to_owned(),
                    "+HEAD:refs/remotes/origin/HEAD".to_owned(),
                ],
            ))
        }
    }

    async fn get_git_upstream_ref(&self, installed_path: &str) -> Option<String> {
        let upstream = self
            .run_command_capture(
                "git",
                &["rev-parse", "--abbrev-ref", "@{upstream}"],
                Some(installed_path),
                Some(NETWORK_TIMEOUT_MS),
                None,
            )
            .await
            .ok()?;
        let branch = upstream.trim().strip_prefix("origin/")?;
        if branch.is_empty() {
            None
        } else {
            Some(format!("refs/heads/{branch}"))
        }
    }

    async fn run_git_remote_command(&self, installed_path: &str, args: &[&str]) -> Result<String> {
        let mut extra_env = IndexMap::new();
        extra_env.insert("GIT_TERMINAL_PROMPT".to_owned(), "0".to_owned());
        self.run_command_capture(
            "git",
            args,
            Some(installed_path),
            Some(NETWORK_TIMEOUT_MS),
            Some(&extra_env),
        )
        .await
    }

    pub fn get_package_identity(&self, source: &str, scope: Option<SourceScope>) -> Result<String> {
        Ok(match self.parse_source(source) {
            ParsedSource::Npm(npm) => format!("npm:{}", npm.name),
            ParsedSource::Git(git) => format!("git:{}/{}", git.host, git.path),
            ParsedSource::Local(local) => match scope {
                Some(sc) => {
                    let base_dir = self.get_base_dir_for_scope(sc)?;
                    format!("local:{}", self.resolve_path_from_base(&local.path, &base_dir))
                }
                None => format!("local:{}", self.resolve_cwd_path(&local.path)),
            },
        })
    }

    fn dedupe_packages(
        &self,
        packages: Vec<(PackageSource, SourceScope)>,
    ) -> Result<Vec<(PackageSource, SourceScope)>> {
        let mut result: Vec<(PackageSource, SourceScope)> = Vec::new();
        let mut seen: IndexMap<String, usize> = IndexMap::new();
        for (pkg, scope) in packages {
            let identity = self.get_package_identity(Self::get_package_source_string(&pkg), Some(scope))?;
            if let Some(&idx) = seen.get(&identity) {
                let (ref existing_pkg, existing_scope) = result[idx];
                if existing_scope == SourceScope::Project && scope == SourceScope::User {
                    if let PackageSource::Object(obj) = existing_pkg
                        && obj.autoload == Some(false)
                    {
                        result.push((pkg, scope));
                    }
                } else if scope == SourceScope::Project {
                    result[idx] = (pkg, scope);
                }
            } else {
                seen.insert(identity, result.len());
                result.push((pkg, scope));
            }
        }
        Ok(result)
    }

    fn assert_project_trusted_for_scope(&self, scope: SourceScope) -> Result<()> {
        if scope == SourceScope::Project && !self.inner.settings_manager.is_project_trusted() {
            return Err(Error::msg(
                "Project is not trusted; refusing to access project package storage",
            ));
        }
        Ok(())
    }

    fn get_npm_command(&self) -> Result<(String, Vec<String>)> {
        let configured = self.inner.settings_manager.get_npm_command();
        let Some(cmd_list) = configured.filter(|c| !c.is_empty()) else {
            return Ok(("npm".to_owned(), Vec::new()));
        };
        let command = cmd_list[0].clone();
        if command.is_empty() {
            return Err(Error::msg(
                "Invalid npmCommand: first array entry must be a non-empty command",
            ));
        }
        Ok((command, cmd_list[1..].to_vec()))
    }

    fn normalize_command_name(command: &str) -> String {
        let base = basename(command, None);
        if let Some(s) = base.strip_suffix(".cmd").or_else(|| base.strip_suffix(".CMD")) {
            s.to_owned()
        } else if let Some(s) = base.strip_suffix(".exe").or_else(|| base.strip_suffix(".EXE")) {
            s.to_owned()
        } else {
            base
        }
    }

    pub fn get_package_manager_name(&self) -> Result<String> {
        let (command, args) = self.get_npm_command()?;
        let direct = Self::normalize_command_name(&command);
        let is_supported = |name: &str| matches!(name, "npm" | "pnpm" | "bun");
        if let Some(sep_idx) = args.iter().rposition(|a| a == "--") {
            return Ok(args
                .get(sep_idx + 1)
                .map(|w| Self::normalize_command_name(w))
                .unwrap_or(direct));
        }
        if is_supported(&direct) {
            return Ok(direct);
        }
        let mut wrapped = IndexSet::new();
        for arg in &args {
            let norm = Self::normalize_command_name(arg);
            if is_supported(&norm) {
                wrapped.insert(norm);
            }
        }
        if wrapped.len() > 1 {
            let list: Vec<String> = wrapped.into_iter().collect();
            return Err(Error::msg(format!(
                "Ambiguous npmCommand package managers: {}",
                list.join(", ")
            )));
        }
        Ok(wrapped.into_iter().next().unwrap_or(direct))
    }

    async fn run_npm_command(&self, args: &[&str], cwd: Option<&str>) -> Result<()> {
        let (command, prefix_args) = self.get_npm_command()?;
        let mut combined: Vec<String> = prefix_args;
        combined.extend(args.iter().map(|s| (*s).to_owned()));
        let combined_refs: Vec<&str> = combined.iter().map(String::as_str).collect();
        self.run_command(&command, &combined_refs, cwd).await
    }

    fn run_npm_command_sync(&self, args: &[&str]) -> Result<String> {
        let (command, prefix_args) = self.get_npm_command()?;
        let mut combined: Vec<String> = prefix_args;
        combined.extend(args.iter().map(|s| (*s).to_owned()));
        let combined_refs: Vec<&str> = combined.iter().map(String::as_str).collect();
        self.run_command_sync(&command, &combined_refs)
    }

    pub fn get_git_dependency_install_args(&self) -> Result<Vec<&'static str>> {
        match self.get_package_manager_name()?.as_str() {
            "bun" => Ok(vec!["install", "--omit=dev", "--omit=peer"]),
            "pnpm" => Ok(vec![
                "install",
                "--prod",
                "--config.auto-install-peers=false",
                "--config.strict-peer-dependencies=false",
                "--config.strict-dep-builds=false",
            ]),
            "npm" => Ok(vec!["install", "--omit=dev", "--legacy-peer-deps"]),
            _ => Ok(vec!["install"]),
        }
    }

    fn get_npm_install_args(&self, specs: &[String], install_root: &str) -> Result<Vec<String>> {
        let pm = self.get_package_manager_name()?;
        let mut args = vec!["install".to_owned()];
        args.extend(specs.iter().cloned());
        match pm.as_str() {
            "bun" => {
                args.extend(["--cwd".to_owned(), install_root.to_owned(), "--omit=peer".to_owned()]);
            }
            "pnpm" => {
                args.extend([
                    "--prefix".to_owned(),
                    install_root.to_owned(),
                    "--config.auto-install-peers=false".to_owned(),
                    "--config.strict-peer-dependencies=false".to_owned(),
                    "--config.strict-dep-builds=false".to_owned(),
                ]);
            }
            _ => {
                args.extend([
                    "--prefix".to_owned(),
                    install_root.to_owned(),
                    "--legacy-peer-deps".to_owned(),
                ]);
            }
        }
        Ok(args)
    }

    async fn install_npm(&self, source: &NpmSource, scope: SourceScope, temporary: bool) -> Result<()> {
        let install_root = self.get_npm_install_root(scope, temporary)?;
        self.ensure_npm_project(&install_root)?;
        let args = self.get_npm_install_args(std::slice::from_ref(&source.spec), &install_root)?;
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        self.run_npm_command(&arg_refs, None).await
    }

    async fn uninstall_npm(&self, source: &NpmSource, scope: SourceScope) -> Result<()> {
        let install_root = self.get_npm_install_root(scope, false)?;
        if !exists(&install_root) {
            return Ok(());
        }
        let pm = self.get_package_manager_name()?;
        if pm == "bun" {
            return self
                .run_npm_command(&["uninstall", &source.name, "--cwd", &install_root], None)
                .await;
        }
        let mut args = vec!["uninstall", &source.name, "--prefix", &install_root];
        if pm != "pnpm" {
            args.push("--legacy-peer-deps");
        }
        self.run_npm_command(&args, None).await
    }

    async fn install_git(&self, source: &GitSource, scope: SourceScope) -> Result<()> {
        let target_dir = self.get_git_install_path(source, scope)?;
        if exists(&target_dir) {
            if let Some(ref git_ref) = source.ref_name {
                return self
                    .ensure_git_ref(&target_dir, &["fetch", "origin", git_ref], "FETCH_HEAD")
                    .await;
            }
            let (target_ref, _head, fetch_args) = self.get_local_git_update_target(&target_dir).await?;
            let fetch_refs: Vec<&str> = fetch_args.iter().map(String::as_str).collect();
            return self.ensure_git_ref(&target_dir, &fetch_refs, &target_ref).await;
        }

        let git_root = self.get_git_install_root(scope)?;
        if let Some(ref gr) = git_root {
            self.ensure_git_ignore(gr)?;
        }
        mkdir(&dirname(&target_dir), true, None)?;
        let _ = rm(&self.get_git_update_marker_path(&target_dir), false, true);

        let res = async {
            self.run_command("git", &["clone", &source.repo, &target_dir], None)
                .await?;
            if let Some(ref git_ref) = source.ref_name {
                self.run_command("git", &["checkout", git_ref], Some(&target_dir))
                    .await?;
            }
            let package_json_path = join(&[&target_dir, "package.json"]);
            if exists(&package_json_path) {
                let dep_args = self.get_git_dependency_install_args()?;
                self.run_npm_command(&dep_args, Some(&target_dir)).await?;
            }
            Ok::<(), Error>(())
        }
        .await;

        if let Err(err) = res {
            let _ = rm(&target_dir, true, true);
            self.prune_empty_git_parents(&target_dir, git_root.as_deref());
            return Err(err);
        }
        Ok(())
    }

    async fn update_git(&self, source: &GitSource, scope: SourceScope) -> Result<()> {
        let target_dir = self.get_git_install_path(source, scope)?;
        if !exists(&target_dir) {
            return self.install_git(source, scope).await;
        }
        if let Some(ref git_ref) = source.ref_name {
            return self
                .ensure_git_ref(&target_dir, &["fetch", "origin", git_ref], "FETCH_HEAD")
                .await;
        }
        let (target_ref, _head, fetch_args) = self.get_local_git_update_target(&target_dir).await?;
        let fetch_refs: Vec<&str> = fetch_args.iter().map(String::as_str).collect();
        self.ensure_git_ref(&target_dir, &fetch_refs, &target_ref).await
    }

    fn has_missing_git_dependencies(&self, target_dir: &str) -> bool {
        let package_json_path = join(&[target_dir, "package.json"]);
        if !exists(&package_json_path) {
            return false;
        }
        let Ok(content) = read_to_string(&package_json_path) else {
            return false;
        };
        let Ok(parsed) = pi_js::json::parse(strip_bom(&content)) else {
            return false;
        };
        let Some(deps) = parsed.get("dependencies").and_then(Value::as_object) else {
            return false;
        };
        let node_modules_dir = resolve(&[target_dir, "node_modules"]);
        let s = sep();
        let prefix = format!("{node_modules_dir}{s}");
        deps.keys().any(|name| {
            let dep_path = resolve(&[&node_modules_dir, name]);
            if !dep_path.starts_with(&prefix) {
                return false;
            }
            !exists(&dep_path)
        })
    }

    async fn repair_missing_git_dependencies(&self, target_dir: &str) -> Result<()> {
        if !self.has_missing_git_dependencies(target_dir) {
            return Ok(());
        }
        let dep_args = self.get_git_dependency_install_args()?;
        self.run_npm_command(&dep_args, Some(target_dir)).await
    }

    fn get_git_update_marker_path(&self, target_dir: &str) -> String {
        join(&[
            &dirname(target_dir),
            &format!(".{}.pi-update-incomplete", basename(target_dir, None)),
        ])
    }

    async fn clean_and_install_git_dependencies(&self, target_dir: &str, marker_path: &str) -> Result<()> {
        if let Err(err) = self.run_command("git", &["clean", "-fdx"], Some(target_dir)).await {
            let _ = self.repair_missing_git_dependencies(target_dir).await;
            return Err(err);
        }
        let package_json_path = join(&[target_dir, "package.json"]);
        if exists(&package_json_path) {
            let dep_args = self.get_git_dependency_install_args()?;
            self.run_npm_command(&dep_args, Some(target_dir)).await?;
        }
        let _ = rm(marker_path, false, true);
        Ok(())
    }

    async fn ensure_git_ref(&self, target_dir: &str, fetch_args: &[&str], ref_name: &str) -> Result<()> {
        self.run_command("git", fetch_args, Some(target_dir)).await?;
        let local_head = self
            .run_command_capture(
                "git",
                &["rev-parse", "HEAD"],
                Some(target_dir),
                Some(NETWORK_TIMEOUT_MS),
                None,
            )
            .await?;
        let commit_ref = format!("{ref_name}^{{commit}}");
        let target_head = self
            .run_command_capture(
                "git",
                &["rev-parse", &commit_ref],
                Some(target_dir),
                Some(NETWORK_TIMEOUT_MS),
                None,
            )
            .await?;
        let marker_path = self.get_git_update_marker_path(target_dir);
        if local_head.trim() == target_head.trim() {
            if exists(&marker_path) {
                self.clean_and_install_git_dependencies(target_dir, &marker_path)
                    .await?;
            } else {
                self.repair_missing_git_dependencies(target_dir).await?;
            }
            return Ok(());
        }
        write_file(&marker_path, "", None)?;
        self.run_command("git", &["reset", "--hard", &commit_ref], Some(target_dir))
            .await?;
        self.clean_and_install_git_dependencies(target_dir, &marker_path).await
    }

    async fn refresh_temporary_git_source(&self, source: &GitSource, source_str: &str) {
        if is_offline_mode_enabled() {
            return;
        }
        let _ = self
            .with_progress(
                ProgressAction::Pull,
                source_str,
                &format!("Refreshing {source_str}..."),
                || async { self.update_git(source, SourceScope::Temporary).await },
            )
            .await;
    }

    fn remove_git(&self, source: &GitSource, scope: SourceScope) -> Result<()> {
        let target_dir = self.get_git_install_path(source, scope)?;
        let _ = rm(&target_dir, true, true);
        let _ = rm(&self.get_git_update_marker_path(&target_dir), false, true);
        let git_root = self.get_git_install_root(scope)?;
        self.prune_empty_git_parents(&target_dir, git_root.as_deref());
        Ok(())
    }

    fn prune_empty_git_parents(&self, target_dir: &str, install_root: Option<&str>) {
        let Some(root) = install_root else {
            return;
        };
        let resolved_root = resolve(&[root]);
        let mut current = dirname(target_dir);
        while current.starts_with(&resolved_root) && current != resolved_root {
            if !exists(&current) {
                current = dirname(&current);
                continue;
            }
            let Ok(entries) = readdir(&current) else {
                break;
            };
            if !entries.is_empty() {
                break;
            }
            if rm(&current, true, true).is_err() {
                break;
            }
            current = dirname(&current);
        }
    }

    fn ensure_npm_project(&self, install_root: &str) -> Result<()> {
        if !exists(install_root) {
            mkdir(install_root, true, None)?;
        }
        mark_path_ignored_by_cloud_sync(install_root);
        self.ensure_git_ignore(install_root)?;
        let package_json_path = join(&[install_root, "package.json"]);
        if !exists(&package_json_path) {
            let mut obj = serde_json::Map::new();
            obj.insert("name".to_owned(), Value::String("pi-extensions".to_owned()));
            obj.insert("private".to_owned(), Value::Bool(true));
            let text = pi_js::json::try_stringify_pretty(&Value::Object(obj), "  ")?;
            write_file(&package_json_path, text, None)?;
        }
        Ok(())
    }

    fn ensure_git_ignore(&self, dir: &str) -> Result<()> {
        if !exists(dir) {
            mkdir(dir, true, None)?;
        }
        let ignore_path = join(&[dir, ".gitignore"]);
        if !exists(&ignore_path) {
            write_file(&ignore_path, "*\n!.gitignore\n", None)?;
        }
        Ok(())
    }

    fn get_npm_install_root(&self, scope: SourceScope, temporary: bool) -> Result<String> {
        if temporary {
            return self.get_temporary_dir("npm", None, None);
        }
        if scope == SourceScope::Project {
            self.assert_project_trusted_for_scope(scope)?;
            return Ok(join(&[&self.inner.cwd, *CONFIG_DIR_NAME, "npm"]));
        }
        Ok(join(&[&self.inner.agent_dir, "npm"]))
    }

    fn get_global_npm_root(&self) -> Result<String> {
        let (command, args) = self.get_npm_command()?;
        let mut parts = vec![command];
        parts.extend(args);
        let command_key = parts.join("\0");

        {
            let st = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
            if st.global_npm_root_command_key.as_deref() == Some(command_key.as_str())
                && let Some(ref root) = st.global_npm_root
            {
                return Ok(root.clone());
            }
        }

        let computed = if self.get_package_manager_name()? == "bun" {
            let bin_dir = self.run_npm_command_sync(&["pm", "bin", "-g"])?.trim().to_owned();
            join(&[&dirname(&bin_dir), "install", "global", "node_modules"])
        } else {
            self.run_npm_command_sync(&["root", "-g"])?.trim().to_owned()
        };

        let mut st = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.global_npm_root = Some(computed.clone());
        st.global_npm_root_command_key = Some(command_key);
        Ok(computed)
    }

    fn get_pnpm_global_package_path(&self, package_name: &str) -> Result<Option<String>> {
        if self.get_package_manager_name()? != "pnpm" {
            return Ok(None);
        }
        let output = self.run_npm_command_sync(&["list", "-g", "--depth", "0", "--json"])?;
        let parsed = pi_js::json::parse(&output)?;
        let Some(entries) = parsed.as_array() else {
            return Ok(None);
        };
        for entry in entries {
            if let Some(p) = entry
                .get("dependencies")
                .and_then(|d| d.get(package_name))
                .and_then(|p| p.get("path"))
                .and_then(Value::as_str)
            {
                return Ok(Some(p.to_owned()));
            }
        }
        Ok(None)
    }

    fn get_managed_npm_install_path(&self, source: &NpmSource, scope: SourceScope) -> Result<String> {
        if scope == SourceScope::Temporary {
            return Ok(join(&[
                &self.get_temporary_dir("npm", None, None)?,
                "node_modules",
                &source.name,
            ]));
        }
        if scope == SourceScope::Project {
            self.assert_project_trusted_for_scope(scope)?;
            return Ok(join(&[
                &self.inner.cwd,
                *CONFIG_DIR_NAME,
                "npm",
                "node_modules",
                &source.name,
            ]));
        }
        Ok(join(&[&self.inner.agent_dir, "npm", "node_modules", &source.name]))
    }

    fn get_legacy_global_npm_install_path(&self, source: &NpmSource) -> Option<String> {
        if let Ok(Some(p)) = self.get_pnpm_global_package_path(&source.name) {
            return Some(p);
        }
        self.get_global_npm_root().ok().map(|root| join(&[&root, &source.name]))
    }

    pub fn get_npm_install_path(&self, source: &NpmSource, scope: SourceScope) -> Result<String> {
        let managed = self.get_managed_npm_install_path(source, scope)?;
        if scope != SourceScope::User || exists(&managed) {
            return Ok(managed);
        }
        if let Some(legacy) = self.get_legacy_global_npm_install_path(source)
            && exists(&legacy)
        {
            return Ok(legacy);
        }
        Ok(managed)
    }

    pub fn get_git_install_path(&self, source: &GitSource, scope: SourceScope) -> Result<String> {
        if scope == SourceScope::Temporary {
            return self.get_temporary_dir(
                &format!("git-{}", source.host),
                Some(&source.path),
                source.ref_name.as_deref(),
            );
        }
        let install_root = self
            .get_git_install_root(scope)?
            .ok_or_else(|| Error::msg("Missing git install root"))?;
        self.resolve_managed_path(&install_root, &[&source.host, &source.path])
    }

    fn get_git_install_root(&self, scope: SourceScope) -> Result<Option<String>> {
        if scope == SourceScope::Temporary {
            return Ok(None);
        }
        if scope == SourceScope::Project {
            self.assert_project_trusted_for_scope(scope)?;
            return Ok(Some(join(&[&self.inner.cwd, *CONFIG_DIR_NAME, "git"])));
        }
        Ok(Some(join(&[&self.inner.agent_dir, "git"])))
    }

    fn get_temporary_dir(&self, prefix: &str, suffix: Option<&str>, ref_name: Option<&str>) -> Result<String> {
        let temp_base = get_extension_temp_folder(&self.inner.agent_dir)?;
        let root = self.resolve_managed_path(&temp_base, &[prefix])?;
        let ref_part = ref_name.map(|r| format!("@{r}")).unwrap_or_default();
        let input = format!("{prefix}-{}{ref_part}", suffix.unwrap_or(""));
        let digest = Sha256::digest(input.as_bytes());
        let hash_full = hex::encode(digest);
        let hash = &hash_full[..8];
        self.resolve_managed_path(&root, &[hash, suffix.unwrap_or("")])
    }

    fn resolve_managed_path(&self, root: &str, parts: &[&str]) -> Result<String> {
        let resolved_root = resolve(&[root]);
        let mut all_parts = vec![resolved_root.as_str()];
        all_parts.extend_from_slice(parts);
        let resolved_path = resolve(&all_parts);
        let s = sep();
        let prefix = if resolved_root.ends_with(s) {
            resolved_root.clone()
        } else {
            format!("{resolved_root}{s}")
        };
        if resolved_path != resolved_root && !resolved_path.starts_with(&prefix) {
            return Err(Error::msg(format!(
                "Refusing to use path outside package install root: {resolved_path}"
            )));
        }
        Ok(resolved_path)
    }

    fn get_base_dir_for_scope(&self, scope: SourceScope) -> Result<String> {
        match scope {
            SourceScope::Project => {
                self.assert_project_trusted_for_scope(scope)?;
                Ok(join(&[&self.inner.cwd, *CONFIG_DIR_NAME]))
            }
            SourceScope::User => Ok(self.inner.agent_dir.clone()),
            SourceScope::Temporary => Ok(self.inner.cwd.clone()),
        }
    }

    fn resolve_cwd_path(&self, input: &str) -> String {
        resolve_path(
            input,
            Some(&self.inner.cwd),
            PathInputOptions {
                home_dir: Some(get_home_dir()),
                trim: Some(true),
                ..Default::default()
            },
        )
    }

    fn resolve_path_from_base(&self, input: &str, base_dir: &str) -> String {
        resolve_path(
            input,
            Some(base_dir),
            PathInputOptions {
                home_dir: Some(get_home_dir()),
                trim: Some(true),
                ..Default::default()
            },
        )
    }

    fn settings_entries_for_kind(settings: &Settings, kind: ResourceKind) -> &[String] {
        match kind {
            ResourceKind::Extensions => settings.extensions.as_deref().unwrap_or_default(),
            ResourceKind::Skills => settings.skills.as_deref().unwrap_or_default(),
            ResourceKind::Prompts => settings.prompts.as_deref().unwrap_or_default(),
            ResourceKind::Themes => settings.themes.as_deref().unwrap_or_default(),
        }
    }

    fn filter_entries_for_kind(filter: &PackageFilterSource, kind: ResourceKind) -> Option<&[String]> {
        match kind {
            ResourceKind::Extensions => filter.extensions.as_deref(),
            ResourceKind::Skills => filter.skills.as_deref(),
            ResourceKind::Prompts => filter.prompts.as_deref(),
            ResourceKind::Themes => filter.themes.as_deref(),
        }
    }

    fn manifest_entries_for_kind(manifest: &PiManifest, kind: ResourceKind) -> Option<&[String]> {
        match kind {
            ResourceKind::Extensions => manifest.extensions.as_deref(),
            ResourceKind::Skills => manifest.skills.as_deref(),
            ResourceKind::Prompts => manifest.prompts.as_deref(),
            ResourceKind::Themes => manifest.themes.as_deref(),
        }
    }

    fn collect_package_resources(
        &self,
        package_root: &str,
        accumulator: &mut ResourceAccumulator,
        filter: Option<&PackageFilterSource>,
        metadata: &PathMetadata,
    ) -> bool {
        if let Some(filter) = filter {
            for &kind in RESOURCE_KINDS {
                let target = Self::get_target_map(accumulator, kind);
                let patterns = Self::filter_entries_for_kind(filter, kind);
                if filter.autoload == Some(false) {
                    self.apply_package_delta_filter(package_root, patterns.unwrap_or_default(), kind, target, metadata);
                } else if let Some(pats) = patterns {
                    self.apply_package_filter(package_root, pats, kind, target, metadata);
                } else {
                    self.collect_default_resources(package_root, kind, target, metadata);
                }
            }
            return true;
        }

        if let Some(manifest) = read_pi_manifest(&join(&[package_root, "package.json"])) {
            for &kind in RESOURCE_KINDS {
                let entries = Self::manifest_entries_for_kind(&manifest, kind);
                self.add_manifest_entries(
                    entries,
                    package_root,
                    kind,
                    Self::get_target_map(accumulator, kind),
                    metadata,
                );
            }
            return true;
        }

        let mut has_any_dir = false;
        for &kind in RESOURCE_KINDS {
            let dir = join(&[package_root, kind.dir_name()]);
            if exists(&dir) {
                let files = collect_resource_files(&dir, kind);
                let target = Self::get_target_map(accumulator, kind);
                for f in files {
                    Self::add_resource(target, &f, metadata, true);
                }
                has_any_dir = true;
            }
        }
        has_any_dir
    }

    fn collect_default_resources(
        &self,
        package_root: &str,
        kind: ResourceKind,
        target: &mut IndexMap<String, ResourceEntry>,
        metadata: &PathMetadata,
    ) {
        if let Some(manifest) = read_pi_manifest(&join(&[package_root, "package.json"]))
            && let Some(entries) = Self::manifest_entries_for_kind(&manifest, kind)
        {
            self.add_manifest_entries(Some(entries), package_root, kind, target, metadata);
            return;
        }
        let dir = join(&[package_root, kind.dir_name()]);
        if exists(&dir) {
            for f in collect_resource_files(&dir, kind) {
                Self::add_resource(target, &f, metadata, true);
            }
        }
    }

    fn apply_package_filter(
        &self,
        package_root: &str,
        user_patterns: &[String],
        kind: ResourceKind,
        target: &mut IndexMap<String, ResourceEntry>,
        metadata: &PathMetadata,
    ) {
        let all_files = self.collect_manifest_files(package_root, kind);
        if user_patterns.is_empty() {
            for f in &all_files {
                Self::add_resource(target, f, metadata, false);
            }
            return;
        }
        let enabled_by_user = apply_patterns(&all_files, user_patterns, package_root);
        for f in &all_files {
            Self::add_resource(target, f, metadata, enabled_by_user.contains(f));
        }
    }

    fn apply_package_delta_filter(
        &self,
        package_root: &str,
        user_patterns: &[String],
        kind: ResourceKind,
        target: &mut IndexMap<String, ResourceEntry>,
        metadata: &PathMetadata,
    ) {
        if user_patterns.is_empty() {
            return;
        }
        let all_files = self.collect_manifest_files(package_root, kind);
        let enabled_by_user = apply_autoload_disabled_patterns(&all_files, user_patterns, package_root);
        for (fp, enabled) in enabled_by_user {
            Self::add_resource(target, &fp, metadata, enabled);
        }
    }

    fn collect_manifest_files(&self, package_root: &str, kind: ResourceKind) -> Vec<String> {
        if let Some(manifest) = read_pi_manifest(&join(&[package_root, "package.json"]))
            && let Some(entries) = Self::manifest_entries_for_kind(&manifest, kind)
            && !entries.is_empty()
        {
            let all_files = self.collect_files_from_manifest_entries(entries, package_root, kind);
            let manifest_patterns: Vec<String> = entries.iter().filter(|e| is_override_pattern(e)).cloned().collect();
            if manifest_patterns.is_empty() {
                return all_files;
            }
            return apply_patterns(&all_files, &manifest_patterns, package_root)
                .into_iter()
                .collect();
        }
        let convention_dir = join(&[package_root, kind.dir_name()]);
        if !exists(&convention_dir) {
            return Vec::new();
        }
        collect_resource_files(&convention_dir, kind)
    }

    fn add_manifest_entries(
        &self,
        entries: Option<&[String]>,
        root: &str,
        kind: ResourceKind,
        target: &mut IndexMap<String, ResourceEntry>,
        metadata: &PathMetadata,
    ) {
        let Some(entries) = entries else {
            return;
        };
        let all_files = self.collect_files_from_manifest_entries(entries, root, kind);
        let patterns: Vec<String> = entries.iter().filter(|e| is_override_pattern(e)).cloned().collect();
        let enabled_paths = apply_patterns(&all_files, &patterns, root);
        for f in all_files {
            if enabled_paths.contains(&f) {
                Self::add_resource(target, &f, metadata, true);
            }
        }
    }

    fn collect_files_from_manifest_entries(&self, entries: &[String], root: &str, kind: ResourceKind) -> Vec<String> {
        let mut resolved = Vec::new();
        for entry in entries {
            if is_override_pattern(entry) {
                continue;
            }
            if !has_glob_pattern(entry) {
                resolved.push(resolve(&[root, entry]));
            } else {
                resolved.extend(expand_package_glob(entry, root));
            }
        }
        self.collect_files_from_paths(&resolved, kind)
    }

    fn resolve_local_entries(
        &self,
        entries: &[String],
        kind: ResourceKind,
        target: &mut IndexMap<String, ResourceEntry>,
        metadata: &PathMetadata,
        base_dir: &str,
    ) {
        if entries.is_empty() {
            return;
        }
        let (plain, patterns) = split_patterns(entries);
        let resolved_plain: Vec<String> = plain.iter().map(|p| self.resolve_path_from_base(p, base_dir)).collect();
        let all_files = self.collect_files_from_paths(&resolved_plain, kind);
        let enabled_paths = apply_patterns(&all_files, &patterns, base_dir);
        for f in all_files {
            let enabled = enabled_paths.contains(&f);
            Self::add_resource(target, &f, metadata, enabled);
        }
    }

    fn add_auto_discovered_resources(
        &self,
        accumulator: &mut ResourceAccumulator,
        global_settings: &Settings,
        project_settings: &Settings,
        global_base_dir: &str,
        project_base_dir: &str,
    ) {
        let user_metadata = PathMetadata {
            source: "auto".to_owned(),
            scope: SourceScope::User,
            origin: SourceOrigin::TopLevel,
            base_dir: Some(global_base_dir.to_owned()),
            package_root: None,
            changelog_path: None,
        };
        let project_metadata = PathMetadata {
            source: "auto".to_owned(),
            scope: SourceScope::Project,
            origin: SourceOrigin::TopLevel,
            base_dir: Some(project_base_dir.to_owned()),
            package_root: None,
            changelog_path: None,
        };

        let user_agents_skills_dir = join(&[&get_home_dir(), ".agents", "skills"]);
        let project_trusted = self.inner.settings_manager.is_project_trusted();
        let project_agents_skill_dirs: Vec<String> = if project_trusted {
            let resolved_user_agents = resolve(&[&user_agents_skills_dir]);
            collect_ancestor_agents_skill_dirs(&self.inner.cwd)
                .into_iter()
                .filter(|dir| resolve(&[dir]) != resolved_user_agents)
                .collect()
        } else {
            Vec::new()
        };

        let add_res = |target: &mut IndexMap<String, ResourceEntry>,
                       paths: Vec<String>,
                       meta: &PathMetadata,
                       overrides: &[String],
                       base_dir: &str| {
            for p in paths {
                let enabled = is_enabled_by_overrides(&p, overrides, base_dir);
                Self::add_resource(target, &p, meta, enabled);
            }
        };

        if project_trusted {
            add_res(
                &mut accumulator.extensions,
                collect_auto_extension_entries(&join(&[project_base_dir, "extensions"])),
                &project_metadata,
                project_settings.extensions.as_deref().unwrap_or_default(),
                project_base_dir,
            );
            add_res(
                &mut accumulator.skills,
                collect_skill_entries(&join(&[project_base_dir, "skills"]), SkillDiscoveryMode::Pi, None, None),
                &project_metadata,
                project_settings.skills.as_deref().unwrap_or_default(),
                project_base_dir,
            );
        }

        for agents_skills_dir in project_agents_skill_dirs {
            let agents_base_dir = dirname(&agents_skills_dir);
            let mut agents_meta = project_metadata.clone();
            agents_meta.base_dir = Some(agents_base_dir.clone());
            add_res(
                &mut accumulator.skills,
                collect_skill_entries(&agents_skills_dir, SkillDiscoveryMode::Agents, None, None),
                &agents_meta,
                project_settings.skills.as_deref().unwrap_or_default(),
                &agents_base_dir,
            );
        }

        if project_trusted {
            add_res(
                &mut accumulator.prompts,
                collect_auto_flat_entries(&join(&[project_base_dir, "prompts"]), ".md"),
                &project_metadata,
                project_settings.prompts.as_deref().unwrap_or_default(),
                project_base_dir,
            );
            add_res(
                &mut accumulator.themes,
                collect_auto_flat_entries(&join(&[project_base_dir, "themes"]), ".json"),
                &project_metadata,
                project_settings.themes.as_deref().unwrap_or_default(),
                project_base_dir,
            );
        }

        add_res(
            &mut accumulator.extensions,
            collect_auto_extension_entries(&join(&[global_base_dir, "extensions"])),
            &user_metadata,
            global_settings.extensions.as_deref().unwrap_or_default(),
            global_base_dir,
        );
        add_res(
            &mut accumulator.skills,
            collect_skill_entries(&join(&[global_base_dir, "skills"]), SkillDiscoveryMode::Pi, None, None),
            &user_metadata,
            global_settings.skills.as_deref().unwrap_or_default(),
            global_base_dir,
        );

        let user_agents_base_dir = dirname(&user_agents_skills_dir);
        let mut user_agents_meta = user_metadata.clone();
        user_agents_meta.base_dir = Some(user_agents_base_dir.clone());
        add_res(
            &mut accumulator.skills,
            collect_skill_entries(&user_agents_skills_dir, SkillDiscoveryMode::Agents, None, None),
            &user_agents_meta,
            global_settings.skills.as_deref().unwrap_or_default(),
            &user_agents_base_dir,
        );

        add_res(
            &mut accumulator.prompts,
            collect_auto_flat_entries(&join(&[global_base_dir, "prompts"]), ".md"),
            &user_metadata,
            global_settings.prompts.as_deref().unwrap_or_default(),
            global_base_dir,
        );
        add_res(
            &mut accumulator.themes,
            collect_auto_flat_entries(&join(&[global_base_dir, "themes"]), ".json"),
            &user_metadata,
            global_settings.themes.as_deref().unwrap_or_default(),
            global_base_dir,
        );
    }

    fn collect_files_from_paths(&self, paths: &[String], kind: ResourceKind) -> Vec<String> {
        let mut files = Vec::new();
        for p in paths {
            if !exists(p) {
                continue;
            }
            let Ok(stats) = stat(p) else {
                continue;
            };
            if stats.is_file() {
                files.push(p.clone());
            } else if stats.is_directory() {
                files.extend(collect_resource_files(p, kind));
            }
        }
        files
    }

    fn get_target_map(acc: &mut ResourceAccumulator, kind: ResourceKind) -> &mut IndexMap<String, ResourceEntry> {
        match kind {
            ResourceKind::Extensions => &mut acc.extensions,
            ResourceKind::Skills => &mut acc.skills,
            ResourceKind::Prompts => &mut acc.prompts,
            ResourceKind::Themes => &mut acc.themes,
        }
    }

    fn add_resource(map: &mut IndexMap<String, ResourceEntry>, path: &str, metadata: &PathMetadata, enabled: bool) {
        if path.is_empty() || map.contains_key(path) {
            return;
        }
        map.insert(
            path.to_owned(),
            ResourceEntry {
                metadata: metadata.clone(),
                enabled,
            },
        );
    }

    fn to_resolved_paths(acc: ResourceAccumulator) -> ResolvedPaths {
        let map_to_resolved = |entries: IndexMap<String, ResourceEntry>| -> Vec<ResolvedResource> {
            let mut resolved: Vec<ResolvedResource> = entries
                .into_iter()
                .map(|(path, entry)| ResolvedResource {
                    path,
                    enabled: entry.enabled,
                    metadata: entry.metadata,
                })
                .collect();
            resolved.sort_by_key(|r| resource_precedence_rank(&r.metadata));
            let mut seen = IndexSet::new();
            resolved
                .into_iter()
                .filter(|entry| seen.insert(canonicalize_path(&entry.path)))
                .collect()
        };

        ResolvedPaths {
            extensions: map_to_resolved(acc.extensions),
            skills: map_to_resolved(acc.skills),
            prompts: map_to_resolved(acc.prompts),
            themes: map_to_resolved(acc.themes),
        }
    }

    async fn run_command_capture(
        &self,
        command: &str,
        args: &[&str],
        cwd: Option<&str>,
        timeout_ms: Option<u64>,
        extra_env: Option<&IndexMap<String, String>>,
    ) -> Result<String> {
        if let Some(runner) = self.command_runner() {
            return runner
                .run_command_capture(command, args, cwd, timeout_ms, extra_env)
                .await;
        }
        Self::default_run_command_capture(command, args, cwd, timeout_ms, extra_env).await
    }

    pub async fn default_run_command_capture(
        command: &str,
        args: &[&str],
        cwd: Option<&str>,
        timeout_ms: Option<u64>,
        extra_env: Option<&IndexMap<String, String>>,
    ) -> Result<String> {
        let mut env = pi_js::env::vars();
        if let Some(extra) = extra_env {
            for (k, v) in extra {
                env.insert(k.clone(), v.clone());
            }
        }
        let mut child = spawn_process(
            command,
            args,
            SpawnProcessOptions {
                cwd: cwd.map(str::to_owned),
                env: Some(env),
                stdout_piped: true,
                stderr_piped: true,
                ..Default::default()
            },
        )?;

        let stdout_buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let stderr_buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let out_cb_buf = stdout_buf.clone();
        let err_cb_buf = stderr_buf.clone();

        let status = if let Some(ms) = timeout_ms {
            match tokio::time::timeout(
                Duration::from_millis(ms),
                wait_for_child_process(
                    child,
                    Some(Box::new(move |chunk| {
                        out_cb_buf
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .extend_from_slice(chunk);
                    })),
                    Some(Box::new(move |chunk| {
                        err_cb_buf
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .extend_from_slice(chunk);
                    })),
                ),
            )
            .await
            {
                Ok(res) => res?,
                Err(_) => {
                    return Err(Error::msg(format!(
                        "{command} {} timed out after {ms}ms",
                        args.join(" ")
                    )));
                }
            }
        } else {
            let _ = &mut child;
            wait_for_child_process(
                child,
                Some(Box::new(move |chunk| {
                    out_cb_buf
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .extend_from_slice(chunk);
                })),
                Some(Box::new(move |chunk| {
                    err_cb_buf
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .extend_from_slice(chunk);
                })),
            )
            .await?
        };

        let out_bytes = stdout_buf.lock().unwrap_or_else(PoisonError::into_inner);
        let err_bytes = stderr_buf.lock().unwrap_or_else(PoisonError::into_inner);
        let stdout_str = String::from_utf8_lossy(&out_bytes).into_owned();
        let stderr_str = String::from_utf8_lossy(&err_bytes).into_owned();
        if status == Some(0) {
            return Ok(stdout_str.trim().to_owned());
        }
        let exit_status = match status {
            Some(code) => format!("code {code}"),
            None => "signal unknown".to_owned(),
        };
        let details = if !stderr_str.is_empty() { stderr_str } else { stdout_str };
        Err(Error::msg(format!(
            "{command} {} failed with {exit_status}: {details}",
            args.join(" ")
        )))
    }

    async fn run_command(&self, command: &str, args: &[&str], cwd: Option<&str>) -> Result<()> {
        if let Some(runner) = self.command_runner() {
            return runner.run_command(command, args, cwd).await;
        }
        Self::default_run_command(command, args, cwd).await
    }

    pub async fn default_run_command(command: &str, args: &[&str], cwd: Option<&str>) -> Result<()> {
        let env = pi_js::env::vars();
        let redirect_stderr = is_stdout_taken_over();
        let child = spawn_process(
            command,
            args,
            SpawnProcessOptions {
                cwd: cwd.map(str::to_owned),
                env: Some(env),
                stdout_piped: redirect_stderr,
                stderr_piped: redirect_stderr,
                ..Default::default()
            },
        )?;
        let status = wait_for_child_process(
            child,
            if redirect_stderr {
                Some(Box::new(|chunk| {
                    let s = String::from_utf8_lossy(chunk);
                    pi_js::console::stderr_write(&s);
                }))
            } else {
                None
            },
            if redirect_stderr {
                Some(Box::new(|chunk| {
                    let s = String::from_utf8_lossy(chunk);
                    pi_js::console::stderr_write(&s);
                }))
            } else {
                None
            },
        )
        .await?;
        if status == Some(0) {
            Ok(())
        } else {
            let code_str = status.map_or_else(|| "unknown".to_owned(), |c| c.to_string());
            Err(Error::msg(format!(
                "{command} {} failed with code {code_str}",
                args.join(" ")
            )))
        }
    }

    fn run_command_sync(&self, command: &str, args: &[&str]) -> Result<String> {
        if let Some(runner) = self.command_runner() {
            return runner.run_command_sync(command, args);
        }
        Self::default_run_command_sync(command, args)
    }

    pub fn default_run_command_sync(command: &str, args: &[&str]) -> Result<String> {
        let env = pi_js::env::vars();
        let result = spawn_process_sync(
            command,
            args,
            SpawnProcessOptions {
                env: Some(env),
                stdout_piped: true,
                stderr_piped: true,
                ..Default::default()
            },
        );
        if result.error.is_some() || result.status != Some(0) {
            let detail = result.error.unwrap_or_else(|| {
                if !result.stderr.is_empty() {
                    result.stderr
                } else {
                    result.stdout
                }
            });
            return Err(Error::msg(format!(
                "Failed to run {command} {}: {detail}",
                args.join(" ")
            )));
        }
        let text = if !result.stdout.is_empty() {
            result.stdout
        } else {
            result.stderr
        };
        Ok(text.trim().to_owned())
    }
}

async fn run_with_concurrency<T, F>(tasks: Vec<F>, limit: usize) -> Result<Vec<T>>
where
    F: std::future::Future<Output = Result<T>>,
{
    if tasks.is_empty() {
        return Ok(Vec::new());
    }
    let limit = limit.max(1).min(tasks.len());
    let mut results: BTreeMap<usize, T> = BTreeMap::new();
    let mut iter = tasks.into_iter().enumerate();
    loop {
        let chunk: Vec<(usize, F)> = (&mut iter).take(limit).collect();
        if chunk.is_empty() {
            break;
        }
        let futs = chunk
            .into_iter()
            .map(|(idx, fut)| async move { Ok::<(usize, T), Error>((idx, fut.await?)) });
        for (idx, val) in futures::future::try_join_all(futs).await? {
            results.insert(idx, val);
        }
    }
    Ok(results.into_values().collect())
}

#[async_trait::async_trait]
impl PackageManager for DefaultPackageManager {
    async fn resolve(&self, on_missing: Option<MissingSourceCallback>) -> Result<ResolvedPaths> {
        DefaultPackageManager::resolve(self, on_missing).await
    }

    async fn install(&self, source: &str, local: bool) -> Result<()> {
        DefaultPackageManager::install(self, source, local).await
    }

    async fn install_and_persist(&self, source: &str, local: bool) -> Result<()> {
        DefaultPackageManager::install_and_persist(self, source, local).await
    }

    async fn remove(&self, source: &str, local: bool) -> Result<()> {
        DefaultPackageManager::remove(self, source, local).await
    }

    async fn remove_and_persist(&self, source: &str, local: bool) -> Result<bool> {
        DefaultPackageManager::remove_and_persist(self, source, local).await
    }

    async fn update(&self, source: Option<&str>) -> Result<()> {
        DefaultPackageManager::update(self, source).await
    }

    fn list_configured_packages(&self) -> Result<Vec<ConfiguredPackage>> {
        DefaultPackageManager::list_configured_packages(self)
    }

    async fn resolve_extension_sources(
        &self,
        sources: &[String],
        local: bool,
        temporary: bool,
    ) -> Result<ResolvedPaths> {
        DefaultPackageManager::resolve_extension_sources(self, sources, local, temporary).await
    }

    fn add_source_to_settings(&self, source: &str, local: bool) -> Result<bool> {
        DefaultPackageManager::add_source_to_settings(self, source, local)
    }

    fn remove_source_from_settings(&self, source: &str, local: bool) -> Result<bool> {
        DefaultPackageManager::remove_source_from_settings(self, source, local)
    }

    fn set_progress_callback(&self, callback: Option<ProgressCallback>) {
        DefaultPackageManager::set_progress_callback(self, callback);
    }

    fn get_installed_path(&self, source: &str, scope: InstalledSourceScope) -> Result<Option<String>> {
        DefaultPackageManager::get_installed_path(self, source, scope)
    }
}
