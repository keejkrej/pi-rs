//! Port of packages/coding-agent/test/resource-loader.test.ts

use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use indexmap::IndexMap;
use pi_coding_agent::core::diagnostics::{ResourceDiagnosticType, ResourceType};
use pi_coding_agent::core::event_bus::EventBus;
use pi_coding_agent::core::package_manager::PathMetadata;
use pi_coding_agent::core::resource_loader::{
    BasicExtensionLoaderHost, BuiltinExtension, ContextFile, DefaultResourceLoader, DefaultResourceLoaderOptions,
    Extension, ExtensionFlag, ExtensionFlagDefault, ExtensionFlagType, ExtensionLoaderHost, ExtensionRegistrar,
    ExtensionResourcePath, ExtensionRuntime, InlineExtension, LoadExtensionsResult, LoadProjectContextFilesOptions,
    PromptsResult, RegisteredCommand, RegisteredTool, ResourceExtensionPaths, ResourceLoaderReloadOptions,
    SkillsResult, ThemesResult, load_project_context_files,
};
use pi_coding_agent::core::settings_manager::{SettingsManager, SettingsManagerCreateOptions};
use pi_coding_agent::core::source_info::{
    SourceInfo, SourceOrigin, SourceScope, SyntheticSourceOptions, create_synthetic_source_info,
};
use pi_js::{BoxFuture, Result};
use serde_json::json;
use tempfile::TempDir;

struct ResourceFixture {
    temp_dir: TempDir,
    agent_dir: String,
    cwd: String,
}

impl ResourceFixture {
    fn new() -> Self {
        let temp_dir = tempfile::tempdir().expect("create tempdir");
        let agent_dir = temp_dir.path().join("agent");
        let cwd = temp_dir.path().join("project");
        fs::create_dir_all(&agent_dir).expect("create agent_dir");
        fs::create_dir_all(&cwd).expect("create cwd");
        Self {
            temp_dir,
            agent_dir: agent_dir.to_string_lossy().into_owned(),
            cwd: cwd.to_string_lossy().into_owned(),
        }
    }

    fn write_file_rel(base: &str, rel: &str, content: &str) -> String {
        let full = Path::new(base).join(rel);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).expect("create parent dirs");
        }
        fs::write(&full, content).expect("write test file");
        full.to_string_lossy().into_owned()
    }
}

#[derive(Clone, Default)]
struct TestExtSpec {
    tools: Vec<(String, String)>,
    commands: Vec<(String, Option<String>)>,
    flags: Vec<String>,
    replaceable: bool,
    hidden: bool,
    error: Option<String>,
}

/// Scripted mock `ExtensionLoaderHost` that counts disk loads per path for cache & trust tests.
struct ScriptedExtensionHost {
    fallback: BasicExtensionLoaderHost,
    specs_by_filename: Mutex<IndexMap<String, TestExtSpec>>,
    load_counts: Mutex<IndexMap<String, usize>>,
    cache: Mutex<IndexMap<String, Extension>>,
}

impl ScriptedExtensionHost {
    fn new() -> Self {
        Self {
            fallback: BasicExtensionLoaderHost::new(),
            specs_by_filename: Mutex::new(IndexMap::new()),
            load_counts: Mutex::new(IndexMap::new()),
            cache: Mutex::new(IndexMap::new()),
        }
    }

    fn register_path(&self, path: &str, spec: TestExtSpec) {
        let canonical = fs::canonicalize(path)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| path.to_owned());
        self.specs_by_filename
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(canonical, spec);
    }

    fn load_count(&self, path: &str) -> usize {
        let canonical = fs::canonicalize(path)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| path.to_owned());
        self.load_counts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&canonical)
            .copied()
            .unwrap_or(0)
    }

    fn lookup_spec(&self, path: &str) -> Option<TestExtSpec> {
        let canonical = fs::canonicalize(path)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| path.to_owned());
        self.specs_by_filename
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&canonical)
            .cloned()
    }

    fn canonical_key(path: &str) -> String {
        fs::canonicalize(path)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| path.to_owned())
    }
}

impl ExtensionLoaderHost for ScriptedExtensionHost {
    fn clear_cache(&self) {
        self.cache.lock().unwrap_or_else(PoisonError::into_inner).clear();
        self.fallback.clear_cache();
    }

    fn create_runtime(&self) -> ExtensionRuntime {
        self.fallback.create_runtime()
    }

    fn load_extensions_cached(
        &self,
        paths: Vec<String>,
        cwd: String,
        event_bus: Arc<dyn EventBus>,
        runtime: Option<ExtensionRuntime>,
    ) -> BoxFuture<LoadExtensionsResult> {
        let mut counts = self.load_counts.lock().unwrap_or_else(PoisonError::into_inner);
        let mut specs_to_run: Vec<(String, TestExtSpec)> = Vec::new();
        for p in &paths {
            let key = Self::canonical_key(p);
            let is_cached = self
                .cache
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .contains_key(&key);
            if !is_cached && let Some(spec) = self.lookup_spec(p) {
                *counts.entry(key).or_insert(0) += 1;
                specs_to_run.push((p.clone(), spec));
            }
        }
        drop(counts);

        let rt = runtime.unwrap_or_else(|| self.create_runtime());
        let mut fallback_paths = Vec::new();
        for p in &paths {
            let key = Self::canonical_key(p);
            let in_cache = self
                .cache
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .contains_key(&key);
            if !in_cache && self.lookup_spec(p).is_none() {
                fallback_paths.push(p.clone());
            }
        }

        let cache_snapshot = self.cache.lock().unwrap_or_else(PoisonError::into_inner).clone();
        let fallback = self.fallback.clone();

        Box::pin(async move {
            let mut result = fallback
                .load_extensions_cached(fallback_paths, cwd, event_bus, Some(rt))
                .await;

            for (raw_path, spec) in specs_to_run {
                if let Some(err) = spec.error {
                    result
                        .errors
                        .push(pi_coding_agent::core::resource_loader::ExtensionLoadError {
                            path: raw_path,
                            error: err,
                        });
                    continue;
                }
                let source_info = create_synthetic_source_info(
                    &raw_path,
                    SyntheticSourceOptions {
                        source: "local".to_owned(),
                        scope: Some(SourceScope::Temporary),
                        origin: Some(SourceOrigin::TopLevel),
                        base_dir: Path::new(&raw_path).parent().map(|p| p.to_string_lossy().into_owned()),
                        changelog_path: None,
                    },
                );
                let mut tools = IndexMap::new();
                for (name, desc) in spec.tools {
                    tools.insert(
                        name.clone(),
                        RegisteredTool {
                            name,
                            description: desc,
                            source_info: source_info.clone(),
                        },
                    );
                }
                let mut commands = IndexMap::new();
                for (name, desc) in spec.commands {
                    commands.insert(
                        name.clone(),
                        RegisteredCommand {
                            name,
                            description: desc,
                            source_info: source_info.clone(),
                        },
                    );
                }
                let mut flags = IndexMap::new();
                for name in spec.flags {
                    flags.insert(
                        name.clone(),
                        ExtensionFlag {
                            name,
                            description: None,
                            flag_type: ExtensionFlagType::Boolean,
                            default: Some(ExtensionFlagDefault::Boolean(false)),
                            extension_path: raw_path.clone(),
                        },
                    );
                }
                let ext = Extension {
                    path: raw_path.clone(),
                    resolved_path: raw_path,
                    hidden: spec.hidden,
                    replaceable: spec.replaceable,
                    source_info,
                    tools,
                    commands,
                    flags,
                };
                result.extensions.push(ext);
            }

            for p in &paths {
                let key = Self::canonical_key(p);
                if let Some(cached) = cache_snapshot.get(&key) {
                    result.extensions.push(cached.clone());
                }
            }
            result
        })
    }

    fn load_extension_from_factory(
        &self,
        factory: pi_coding_agent::core::resource_loader::ExtensionFactory,
        cwd: String,
        event_bus: Arc<dyn EventBus>,
        runtime: ExtensionRuntime,
        extension_path: String,
    ) -> BoxFuture<Result<Extension>> {
        self.fallback
            .load_extension_from_factory(factory, cwd, event_bus, runtime, extension_path)
    }
}

fn valid_theme_json(name: &str, color: &str) -> String {
    pi_js::json::stringify(&json!({
        "name": name,
        "vars": { "accent": color },
        "colors": { "accent": "$accent", "border": color }
    }))
}

#[tokio::test]
async fn starts_empty_before_reload_and_discovers_agent_dir_resources() {
    let fx = ResourceFixture::new();
    let ext_path = ResourceFixture::write_file_rel(
        &fx.agent_dir,
        "extensions/global-ext.ts",
        "export default function() {}",
    );
    ResourceFixture::write_file_rel(
        &fx.agent_dir,
        "skills/global-skill/SKILL.md",
        "---\nname: global-skill\ndescription: Global skill\n---\nSkill body\n",
    );
    ResourceFixture::write_file_rel(
        &fx.agent_dir,
        "prompts/global-prompt.md",
        "---\ndescription: Global prompt\n---\nPrompt body\n",
    );
    ResourceFixture::write_file_rel(
        &fx.agent_dir,
        "themes/global-theme.json",
        &valid_theme_json("global-theme", "#ff0000"),
    );

    let loader = DefaultResourceLoader::new(DefaultResourceLoaderOptions {
        cwd: fx.cwd.clone(),
        agent_dir: fx.agent_dir.clone(),
        ..Default::default()
    });

    assert!(loader.get_extensions().extensions.is_empty());
    assert!(loader.get_skills().skills.is_empty());
    assert!(loader.get_prompts().prompts.is_empty());
    assert!(loader.get_themes().themes.is_empty());

    loader.reload(None).await.expect("reload");

    assert_eq!(loader.get_extensions().extensions.len(), 1);
    assert_eq!(loader.get_extensions().extensions[0].path, ext_path);
    assert!(loader.get_skills().skills.iter().any(|s| s.name == "global-skill"));
    assert!(
        loader
            .get_prompts()
            .prompts
            .iter()
            .any(|p| p.name == "global-prompt" && p.description == "Global prompt")
    );
    assert!(
        loader
            .get_themes()
            .themes
            .iter()
            .any(|t| t.name.as_deref() == Some("global-theme"))
    );
}

#[tokio::test]
async fn gates_project_resources_on_project_trust_and_reuses_preloaded_global_extensions() {
    let fx = ResourceFixture::new();
    let host = Arc::new(ScriptedExtensionHost::new());

    let global_ext = ResourceFixture::write_file_rel(&fx.agent_dir, "extensions/global.ts", "export default () => {}");
    let project_ext = ResourceFixture::write_file_rel(&fx.cwd, ".pi/extensions/local.ts", "export default () => {}");
    ResourceFixture::write_file_rel(&fx.agent_dir, "SYSTEM.md", "Global system prompt");
    ResourceFixture::write_file_rel(&fx.cwd, ".pi/SYSTEM.md", "Project system prompt");
    ResourceFixture::write_file_rel(&fx.agent_dir, "APPEND_SYSTEM.md", "Global append");
    ResourceFixture::write_file_rel(&fx.cwd, ".pi/APPEND_SYSTEM.md", "Project append");

    host.register_path(
        &global_ext,
        TestExtSpec {
            tools: vec![("global_tool".to_owned(), "Global tool".to_owned())],
            ..Default::default()
        },
    );
    host.register_path(
        &project_ext,
        TestExtSpec {
            tools: vec![("project_tool".to_owned(), "Project tool".to_owned())],
            ..Default::default()
        },
    );

    let settings_manager = SettingsManager::create(
        &fx.cwd,
        Some(&fx.agent_dir),
        Some(SettingsManagerCreateOptions {
            project_trusted: Some(false),
        }),
    );
    let loader = DefaultResourceLoader::new(DefaultResourceLoaderOptions {
        cwd: fx.cwd.clone(),
        agent_dir: fx.agent_dir.clone(),
        settings_manager: Some(settings_manager),
        extension_host: Some(host.clone()),
        ..Default::default()
    });

    // Reject trust after preloading untrusted set -> only global extension/prompts survive,
    // and global extension was NOT loaded twice.
    let preloaded_paths = Arc::new(Mutex::new(Vec::<String>::new()));
    let captured_paths = preloaded_paths.clone();
    loader
        .reload(Some(ResourceLoaderReloadOptions {
            resolve_project_trust: Some(Arc::new(move |preloaded| {
                let paths = preloaded.extensions.iter().map(|e| e.path.clone()).collect();
                *captured_paths.lock().unwrap() = paths;
                Box::pin(async { false })
            })),
        }))
        .await
        .expect("reload untrusted");

    assert_eq!(*preloaded_paths.lock().unwrap(), vec![global_ext.clone()]);
    assert_eq!(host.load_count(&global_ext), 1);
    assert_eq!(host.load_count(&project_ext), 0);
    assert_eq!(loader.get_extensions().extensions.len(), 1);
    assert_eq!(loader.get_system_prompt().as_deref(), Some("Global system prompt"));
    assert_eq!(loader.get_append_system_prompt(), vec!["Global append"]);

    // Approve trust -> project resources load cleanly and shadow global SYSTEM/APPEND_SYSTEM.
    loader
        .reload(Some(ResourceLoaderReloadOptions {
            resolve_project_trust: Some(Arc::new(|_| Box::pin(async { true }))),
        }))
        .await
        .expect("reload trusted");

    let ext_paths: Vec<String> = loader
        .get_extensions()
        .extensions
        .iter()
        .map(|e| e.path.clone())
        .collect();
    assert!(ext_paths.contains(&global_ext));
    assert!(ext_paths.contains(&project_ext));
    assert_eq!(loader.get_system_prompt().as_deref(), Some("Project system prompt"));
    assert_eq!(loader.get_append_system_prompt(), vec!["Project append"]);
}

#[tokio::test]
async fn supports_no_flags_additional_paths_and_missing_diagnostics() {
    let fx = ResourceFixture::new();
    ResourceFixture::write_file_rel(&fx.agent_dir, "extensions/default.ts", "");
    ResourceFixture::write_file_rel(
        &fx.agent_dir,
        "skills/default-skill/SKILL.md",
        "---\nname: default-skill\ndescription: Default\n---\nbody",
    );
    ResourceFixture::write_file_rel(&fx.agent_dir, "prompts/default-prompt.md", "Default prompt");
    ResourceFixture::write_file_rel(
        &fx.agent_dir,
        "themes/default-theme.json",
        &valid_theme_json("default-theme", "#111111"),
    );
    ResourceFixture::write_file_rel(&fx.cwd, "AGENTS.md", "Project agents");

    let cli_ext = ResourceFixture::write_file_rel(&fx.cwd, "cli-ext.ts", "export default function() {}");
    let cli_skill = ResourceFixture::write_file_rel(
        &fx.cwd,
        "cli-skill/SKILL.md",
        "---\nname: cli-skill\ndescription: CLI\n---\nbody",
    );
    let cli_prompt = ResourceFixture::write_file_rel(&fx.cwd, "cli-prompt.md", "CLI prompt body");
    let cli_theme =
        ResourceFixture::write_file_rel(&fx.cwd, "cli-theme.json", &valid_theme_json("cli-theme", "#222222"));

    let missing_ext = Path::new(&fx.cwd).join("missing-ext.ts");
    let missing_skill = Path::new(&fx.cwd).join("missing-skill");
    let missing_prompt = Path::new(&fx.cwd).join("missing-prompt.md");
    let missing_theme = Path::new(&fx.cwd).join("missing-theme.json");

    let loader = DefaultResourceLoader::new(DefaultResourceLoaderOptions {
        cwd: fx.cwd.clone(),
        agent_dir: fx.agent_dir.clone(),
        additional_extension_paths: vec![cli_ext.clone(), missing_ext.to_string_lossy().into_owned()],
        additional_skill_paths: vec![cli_skill, missing_skill.to_string_lossy().into_owned()],
        additional_prompt_template_paths: vec![cli_prompt, missing_prompt.to_string_lossy().into_owned()],
        additional_theme_paths: vec![cli_theme, missing_theme.to_string_lossy().into_owned()],
        no_extensions: true,
        no_skills: true,
        no_prompt_templates: true,
        no_themes: true,
        no_context_files: true,
        ..Default::default()
    });
    loader.reload(None).await.expect("reload");

    let exts = loader.get_extensions();
    assert_eq!(exts.extensions.len(), 1);
    assert_eq!(exts.extensions[0].path, cli_ext);
    assert!(!exts.errors.is_empty());

    let skills = loader.get_skills();
    assert_eq!(skills.skills.len(), 1);
    assert_eq!(skills.skills[0].name, "cli-skill");
    assert!(skills.diagnostics.iter().any(|d| {
        d.r#type == ResourceDiagnosticType::Warning && d.message.to_lowercase().contains("skill path does not exist")
    }));

    let prompts = loader.get_prompts();
    assert_eq!(prompts.prompts.len(), 1);
    assert_eq!(prompts.prompts[0].name, "cli-prompt");
    assert!(
        prompts.diagnostics.iter().any(|d| {
            d.r#type == ResourceDiagnosticType::Error && d.message == "Prompt template path does not exist"
        })
    );

    let themes = loader.get_themes();
    assert_eq!(themes.themes.len(), 1);
    assert_eq!(themes.themes[0].name.as_deref(), Some("cli-theme"));
    assert!(themes.diagnostics.iter().any(|d| {
        d.r#type == ResourceDiagnosticType::Warning && d.message.to_lowercase().contains("theme path does not exist")
    }));

    assert!(loader.get_agents_files().is_empty());
}

#[tokio::test]
async fn applies_overrides_and_inline_or_file_system_prompts() {
    let fx = ResourceFixture::new();
    let custom_prompt_file = ResourceFixture::write_file_rel(&fx.cwd, "custom-sys.md", "System from file");
    let custom_append_file = ResourceFixture::write_file_rel(&fx.cwd, "custom-append.md", "Append from file");

    let loader = DefaultResourceLoader::new(DefaultResourceLoaderOptions {
        cwd: fx.cwd.clone(),
        agent_dir: fx.agent_dir.clone(),
        system_prompt: Some(custom_prompt_file.clone()),
        append_system_prompt: Some(vec![custom_append_file.clone(), "Inline append".to_owned()]),
        skills_override: Some(Arc::new(|mut res| {
            res.skills.clear();
            SkillsResult {
                skills: res.skills,
                diagnostics: res.diagnostics,
            }
        })),
        prompts_override: Some(Arc::new(|mut res| {
            res.prompts.clear();
            PromptsResult {
                prompts: res.prompts,
                diagnostics: res.diagnostics,
            }
        })),
        themes_override: Some(Arc::new(|mut res| {
            res.themes.clear();
            ThemesResult {
                themes: res.themes,
                diagnostics: res.diagnostics,
            }
        })),
        agents_files_override: Some(Arc::new(|_| {
            vec![ContextFile {
                path: "/virtual/AGENTS.md".to_owned(),
                content: "Overridden agents".to_owned(),
            }]
        })),
        system_prompt_override: Some(Arc::new(|base| base.map(|s| format!("{s} + override")))),
        append_system_prompt_override: Some(Arc::new(|mut base| {
            base.push("Extra append".to_owned());
            base
        })),
        ..Default::default()
    });
    loader.reload(None).await.expect("reload");

    assert_eq!(
        loader.get_system_prompt().as_deref(),
        Some("System from file + override")
    );
    assert_eq!(
        loader.get_system_prompt_source().map(|s| s.path),
        Some(custom_prompt_file)
    );
    assert_eq!(
        loader.get_append_system_prompt(),
        vec!["Append from file", "Inline append", "Extra append"]
    );
    assert_eq!(loader.get_append_system_prompt_sources().len(), 1);
    assert_eq!(loader.get_agents_files().len(), 1);
    assert_eq!(loader.get_agents_files()[0].content, "Overridden agents");
}

#[tokio::test]
async fn handles_collisions_prompt_truncation_extend_resources_and_extension_replacement() {
    let fx = ResourceFixture::new();
    let host = Arc::new(ScriptedExtensionHost::new());

    // Prompt collisions & body fallback truncation (>60 chars)
    let project_shared_prompt = ResourceFixture::write_file_rel(
        &fx.cwd,
        ".pi/prompts/shared.md",
        "This first line is intentionally longer than sixty characters to verify truncation fallback",
    );
    ResourceFixture::write_file_rel(
        &fx.agent_dir,
        "prompts/shared.md",
        "---\ndescription: Global shared\nargument-hint: <arg>\n---\nGlobal body",
    );
    let project_dup_theme = ResourceFixture::write_file_rel(
        &fx.cwd,
        ".pi/themes/dup-theme.json",
        &valid_theme_json("dup-theme", "#0000ff"),
    );
    ResourceFixture::write_file_rel(
        &fx.agent_dir,
        "themes/dup-theme.json",
        &valid_theme_json("dup-theme", "#00ff00"),
    );

    // Replaceable builtin extension vs external override
    let replaceable_builtin = InlineExtension::builtin(
        "builtin-tool",
        Arc::new(|reg: &mut ExtensionRegistrar| {
            reg.register_tool("shared_tool", "Builtin tool");
            Box::pin(async { Ok(()) })
        }),
        true,
    );
    let override_ext = ResourceFixture::write_file_rel(&fx.cwd, "override-ext.ts", "");
    host.register_path(
        &override_ext,
        TestExtSpec {
            tools: vec![("shared_tool".to_owned(), "External override".to_owned())],
            ..Default::default()
        },
    );

    // Dynamic skill & host-provided package dependency warning
    let pkg_dir = Path::new(&fx.agent_dir).join("ext-pkg");
    let pkg_entry = ResourceFixture::write_file_rel(&fx.agent_dir, "ext-pkg/index.ts", "");
    ResourceFixture::write_file_rel(
        &fx.agent_dir,
        "ext-pkg/package.json",
        &pi_js::json::stringify(&json!({
            "name": "bad-ext-pkg",
            "dependencies": { "@earendil-works/pi-ai": "^1.0.0" },
            "pi": { "extensions": ["./index.ts"] }
        })),
    );
    ResourceFixture::write_file_rel(
        &fx.agent_dir,
        "settings.json",
        &pi_js::json::stringify(&json!({ "packages": [pkg_dir.to_string_lossy()] })),
    );

    let loader = DefaultResourceLoader::new(DefaultResourceLoaderOptions {
        cwd: fx.cwd.clone(),
        agent_dir: fx.agent_dir.clone(),
        additional_extension_paths: vec![override_ext.clone()],
        extension_factories: vec![replaceable_builtin],
        extension_host: Some(host),
        ..Default::default()
    });
    loader.reload(None).await.expect("reload");

    let exts = loader.get_extensions();
    // Replaceable builtin omitted when external extension registers the same tool.
    assert!(!exts.extensions.iter().any(|e| e.path == "builtin:builtin-tool"));
    assert!(exts.extensions.iter().any(|e| e.path == override_ext));
    assert!(exts.extensions.iter().any(|e| e.path == pkg_entry));
    assert!(exts.warnings.iter().any(|w| {
        w.warning
            .contains("Host-provided extension packages must be declared in peerDependencies")
    }));

    let prompts = loader.get_prompts();
    assert_eq!(prompts.prompts.len(), 1);
    assert_eq!(prompts.prompts[0].file_path, project_shared_prompt);
    assert!(prompts.prompts[0].description.ends_with("..."));
    assert!(prompts.diagnostics.iter().any(|d| {
        d.r#type == ResourceDiagnosticType::Collision
            && d.collision
                .as_ref()
                .is_some_and(|c| c.resource_type == ResourceType::Prompt)
    }));

    let themes = loader.get_themes();
    assert_eq!(themes.themes.len(), 1);
    assert_eq!(
        themes.themes[0].source_path.as_deref(),
        Some(project_dup_theme.as_str())
    );
    assert!(themes.diagnostics.iter().any(|d| {
        d.r#type == ResourceDiagnosticType::Collision
            && d.collision
                .as_ref()
                .is_some_and(|c| c.resource_type == ResourceType::Theme)
    }));

    // extendResources adds dynamic skill path with package source metadata
    let ext_skill = ResourceFixture::write_file_rel(
        &fx.cwd,
        "dyn-skills/extra/SKILL.md",
        "---\nname: dyn-skill\ndescription: Dynamic\n---\nbody",
    );
    let dyn_meta = PathMetadata {
        source: "npm:dyn-pkg".to_owned(),
        scope: SourceScope::User,
        origin: SourceOrigin::Package,
        base_dir: Some(fx.cwd.clone()),
        package_root: Some(fx.cwd.clone()),
        changelog_path: None,
    };
    loader.extend_resources(ResourceExtensionPaths {
        skill_paths: Some(vec![ExtensionResourcePath {
            path: ext_skill,
            metadata: dyn_meta,
        }]),
        ..Default::default()
    });
    let dyn_skill = loader
        .get_skills()
        .skills
        .into_iter()
        .find(|s| s.name == "dyn-skill")
        .expect("dynamic skill loaded");
    assert_eq!(dyn_skill.source_info.source, "npm:dyn-pkg");

    // Disabled built-in via InlineExtension replaceable=true also hides without conflict error.
    let _ = BuiltinExtension {
        name: "unused".to_owned(),
        factory: Arc::new(|_| Box::pin(async { Ok(()) })),
        hidden: true,
        replaceable: true,
        builtin: true,
    };
}

fn link_worktree(main_dir: &Path, worktree_dir: &Path, name: &str) {
    let git_dir = main_dir.join(".git").join("worktrees").join(name);
    fs::create_dir_all(&git_dir).expect("mkdir worktree gitdir");
    fs::create_dir_all(worktree_dir).expect("mkdir worktree_dir");
    fs::write(main_dir.join(".git").join("HEAD"), "ref: refs/heads/main\n").expect("main HEAD");
    fs::write(git_dir.join("HEAD"), "ref: refs/heads/feat\n").expect("wt HEAD");
    fs::write(git_dir.join("commondir"), "../..").expect("commondir");
    fs::write(worktree_dir.join(".git"), format!("gitdir: {}\n", git_dir.display())).expect("wt .git");
}

#[test]
fn load_project_context_files_handles_nested_worktree_dedup_matrix() {
    let fx = ResourceFixture::new();
    let root = fx.temp_dir.path();

    // 1. Nested worktree with its own AGENTS.md shadows main repo's duplicate.
    {
        let outer = root.join("case1/outer");
        let main = outer.join("main");
        let worktree = main.join("worktrees/feat");
        let worktree_src = worktree.join("src");
        fs::create_dir_all(&worktree_src).unwrap();
        link_worktree(&main, &worktree, "feat");
        fs::write(outer.join("AGENTS.md"), "outer instructions").unwrap();
        fs::write(main.join("AGENTS.md"), "main repo instructions").unwrap();
        fs::write(worktree.join("AGENTS.md"), "worktree instructions").unwrap();

        let files = load_project_context_files(LoadProjectContextFilesOptions {
            cwd: worktree_src.to_string_lossy().into_owned(),
            agent_dir: fx.agent_dir.clone(),
        });
        let contents: Vec<&str> = files.iter().map(|f| f.content.as_str()).collect();
        assert_eq!(contents, vec!["outer instructions", "worktree instructions"]);
    }

    // 2. Nested worktree without its own context inherits main repo's context.
    {
        let main = root.join("case2/main");
        let worktree = main.join("worktrees/feat");
        let worktree_src = worktree.join("src");
        fs::create_dir_all(&worktree_src).unwrap();
        link_worktree(&main, &worktree, "feat");
        fs::write(main.join("AGENTS.md"), "main repo instructions").unwrap();

        let files = load_project_context_files(LoadProjectContextFilesOptions {
            cwd: worktree_src.to_string_lossy().into_owned(),
            agent_dir: fx.agent_dir.clone(),
        });
        let contents: Vec<&str> = files.iter().map(|f| f.content.as_str()).collect();
        assert_eq!(contents, vec!["main repo instructions"]);
    }

    // 3. Different filename (CLAUDE.md in main vs AGENTS.md in worktree) is NOT skipped.
    {
        let main = root.join("case3/main");
        let worktree = main.join("worktrees/feat");
        let worktree_src = worktree.join("src");
        fs::create_dir_all(&worktree_src).unwrap();
        link_worktree(&main, &worktree, "feat");
        fs::write(main.join("CLAUDE.md"), "main claude").unwrap();
        fs::write(worktree.join("AGENTS.md"), "worktree agents").unwrap();

        let files = load_project_context_files(LoadProjectContextFilesOptions {
            cwd: worktree_src.to_string_lossy().into_owned(),
            agent_dir: fx.agent_dir.clone(),
        });
        let contents: Vec<&str> = files.iter().map(|f| f.content.as_str()).collect();
        assert_eq!(contents, vec!["main claude", "worktree agents"]);
    }

    // 4. Bare layout (proj/.bare + proj/main) does NOT skip container AGENTS.md.
    {
        let proj = root.join("case4/proj");
        let bare = proj.join(".bare");
        let worktree = proj.join("main");
        let wt_git_dir = bare.join("worktrees/main");
        fs::create_dir_all(&wt_git_dir).unwrap();
        fs::create_dir_all(&worktree).unwrap();
        fs::write(bare.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::write(wt_git_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::write(wt_git_dir.join("commondir"), "../..").unwrap();
        fs::write(worktree.join(".git"), format!("gitdir: {}\n", wt_git_dir.display())).unwrap();
        fs::write(proj.join("AGENTS.md"), "container instructions").unwrap();
        fs::write(worktree.join("AGENTS.md"), "worktree instructions").unwrap();

        let files = load_project_context_files(LoadProjectContextFilesOptions {
            cwd: worktree.to_string_lossy().into_owned(),
            agent_dir: fx.agent_dir.clone(),
        });
        let contents: Vec<&str> = files.iter().map(|f| f.content.as_str()).collect();
        assert_eq!(contents, vec!["container instructions", "worktree instructions"]);
    }

    // 5. Sibling worktree leaves sibling/main unaffected, keeps outer ancestors.
    {
        let outer = root.join("case5/outer");
        let main = outer.join("main");
        let sib = outer.join("sib-feat");
        let sib_src = sib.join("src");
        fs::create_dir_all(&sib_src).unwrap();
        fs::create_dir_all(&main).unwrap();
        fs::write(outer.join("AGENTS.md"), "outer instructions").unwrap();
        fs::write(main.join("AGENTS.md"), "main instructions").unwrap();
        fs::write(sib.join("AGENTS.md"), "sibling worktree instructions").unwrap();
        link_worktree(&main, &sib, "sib");

        let files = load_project_context_files(LoadProjectContextFilesOptions {
            cwd: sib_src.to_string_lossy().into_owned(),
            agent_dir: fx.agent_dir.clone(),
        });
        let contents: Vec<&str> = files.iter().map(|f| f.content.as_str()).collect();
        assert_eq!(contents, vec!["outer instructions", "sibling worktree instructions"]);
    }

    // 6. Submodule (.git/modules without commondir) keeps superproject context.
    {
        let sup = root.join("case6/super");
        let sub = sup.join("vendor/lib");
        let sub_src = sub.join("src");
        fs::create_dir_all(&sub_src).unwrap();
        fs::write(sup.join("AGENTS.md"), "superproject instructions").unwrap();
        fs::write(sub.join("AGENTS.md"), "submodule instructions").unwrap();
        let sub_git_dir = sup.join(".git/modules/vendor/lib");
        fs::create_dir_all(&sub_git_dir).unwrap();
        fs::write(sub_git_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::write(sub.join(".git"), format!("gitdir: {}\n", sub_git_dir.display())).unwrap();

        let files = load_project_context_files(LoadProjectContextFilesOptions {
            cwd: sub_src.to_string_lossy().into_owned(),
            agent_dir: fx.agent_dir.clone(),
        });
        let contents: Vec<&str> = files.iter().map(|f| f.content.as_str()).collect();
        assert_eq!(contents, vec!["superproject instructions", "submodule instructions"]);
    }

    // 7. Ordinary repo + corrupt gitdir: still climb normally.
    {
        let outer = root.join("case7/outer");
        let repo = outer.join("repo");
        let leaf = repo.join("src");
        fs::create_dir_all(&leaf).unwrap();
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::write(repo.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::write(outer.join("AGENTS.md"), "outer").unwrap();
        fs::write(repo.join("AGENTS.md"), "repo").unwrap();
        fs::write(leaf.join("AGENTS.md"), "leaf").unwrap();

        let files = load_project_context_files(LoadProjectContextFilesOptions {
            cwd: leaf.to_string_lossy().into_owned(),
            agent_dir: fx.agent_dir.clone(),
        });
        let contents: Vec<&str> = files.iter().map(|f| f.content.as_str()).collect();
        assert_eq!(contents, vec!["outer", "repo", "leaf"]);

        let corrupt = root.join("case7/corrupt");
        let c_src = corrupt.join("src");
        fs::create_dir_all(&c_src).unwrap();
        fs::write(corrupt.join(".git"), "gitdir: /nonexistent/path/worktrees/feat\n").unwrap();
        fs::write(corrupt.join("AGENTS.md"), "corrupt repo").unwrap();
        fs::write(c_src.join("AGENTS.md"), "corrupt src").unwrap();
        let corrupt_files = load_project_context_files(LoadProjectContextFilesOptions {
            cwd: c_src.to_string_lossy().into_owned(),
            agent_dir: fx.agent_dir.clone(),
        });
        let c_contents: Vec<&str> = corrupt_files.iter().map(|f| f.content.as_str()).collect();
        assert_eq!(c_contents, vec!["corrupt repo", "corrupt src"]);
    }
}

#[test]
fn load_project_context_files_reads_source_info_defaults() {
    let fx = ResourceFixture::new();
    let _: SourceInfo = create_synthetic_source_info(
        "/tmp/x",
        SyntheticSourceOptions {
            source: "local".to_owned(),
            scope: Some(SourceScope::Project),
            origin: Some(SourceOrigin::TopLevel),
            base_dir: None,
            changelog_path: None,
        },
    );
    let global_md = ResourceFixture::write_file_rel(&fx.agent_dir, "CLAUDE.md", "Global claude");
    let proj_md = ResourceFixture::write_file_rel(&fx.cwd, "AGENTS.md", "Project agents");
    let files = load_project_context_files(LoadProjectContextFilesOptions {
        cwd: fx.cwd.clone(),
        agent_dir: fx.agent_dir.clone(),
    });
    assert_eq!(files.len(), 2);
    assert_eq!(files[0].path, global_md);
    assert_eq!(files[1].path, proj_md);
}
