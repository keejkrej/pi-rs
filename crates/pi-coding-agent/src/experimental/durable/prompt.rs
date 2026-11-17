//! Port of packages/coding-agent/src/experimental/durable/prompt.ts

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use indexmap::IndexMap;
use pi_durable::harness::define::SectionOptions;
use pi_durable::harness::types::{Extension, PromptInput, PromptSection};
use pi_durable::{define_extension, section_with_options};

use crate::config::get_agent_dir;
use crate::core::resource_loader::{LoadProjectContextFilesOptions, load_project_context_files};
use crate::core::settings_manager::SettingsManager;
use crate::core::skills::{LoadSkillsOptions, Skill, load_skills};
use crate::core::system_prompt::{BuildSystemPromptOptions, ContextFile, build_system_prompt_sections};
use crate::core::tools::bash::bash_tool_system_prompt_contribution;
use crate::core::tools::edit::edit_tool_system_prompt_contribution;
use crate::core::tools::read::read_tool_system_prompt_contribution;
use crate::core::tools::write::write_tool_system_prompt_contribution;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

const KEYS: [&str; 7] = ["preamble", "tools", "rules", "docs", "project_context", "skills", "cwd"];

#[derive(Clone)]
struct CachedResources {
    context_files: Vec<ContextFile>,
    skills: Vec<Skill>,
}

fn tool_contribution(name: &str) -> Option<(String, Vec<String>)> {
    match name {
        "read" => {
            let c = read_tool_system_prompt_contribution();
            Some((c.prompt_snippet, c.prompt_guidelines))
        }
        "bash" => {
            let c = bash_tool_system_prompt_contribution(None);
            Some((c.prompt_snippet, c.prompt_guidelines.unwrap_or_default()))
        }
        "edit" => {
            let c = edit_tool_system_prompt_contribution();
            Some((c.prompt_snippet, c.prompt_guidelines))
        }
        "write" => {
            let c = write_tool_system_prompt_contribution();
            Some((c.prompt_snippet, c.prompt_guidelines))
        }
        _ => None,
    }
}

/// pi's system prompt as one extension: the sections of `buildSystemPromptSections()` for the request's
/// tools and the conversation's directory.
pub fn create_pi_prompt(settings: Arc<SettingsManager>, fallback_cwd: String) -> Extension {
    let resources: Arc<Mutex<IndexMap<String, CachedResources>>> = Arc::new(Mutex::new(IndexMap::new()));
    let built_sections: Arc<Mutex<IndexMap<(String, Vec<String>), IndexMap<String, String>>>> =
        Arc::new(Mutex::new(IndexMap::new()));

    let mut sections: Vec<PromptSection> = Vec::with_capacity(KEYS.len());
    for key in KEYS {
        let settings = Arc::clone(&settings);
        let resources = Arc::clone(&resources);
        let built_sections = Arc::clone(&built_sections);
        let fallback_cwd = fallback_cwd.clone();

        sections.push(section_with_options(
            key,
            Arc::new(move |input: PromptInput, _ctx| {
                let cwd = input
                    .env
                    .as_ref()
                    .map(|e| e.cwd().to_string())
                    .or_else(|| input.agent.cwd.clone())
                    .unwrap_or_else(|| fallback_cwd.clone());
                let selected_tools: Vec<String> = input.agent.tools.iter().map(|t| t.name.clone()).collect();
                let cache_key = (cwd.clone(), selected_tools.clone());

                let sections_map = {
                    let cached = lock(&built_sections).get(&cache_key).cloned();
                    if let Some(map) = cached {
                        map
                    } else {
                        let res = {
                            let existing = lock(&resources).get(&cwd).cloned();
                            if let Some(found) = existing {
                                found
                            } else {
                                let agent_dir = get_agent_dir();
                                let context_files = load_project_context_files(LoadProjectContextFilesOptions {
                                    cwd: cwd.clone(),
                                    agent_dir: agent_dir.clone(),
                                })
                                .into_iter()
                                .map(|cf| ContextFile {
                                    path: cf.path,
                                    content: cf.content,
                                })
                                .collect();
                                let skills = load_skills(LoadSkillsOptions {
                                    cwd: cwd.clone(),
                                    agent_dir: Some(agent_dir),
                                    skill_paths: settings.get_skill_paths(),
                                    include_defaults: true,
                                })
                                .skills;
                                let loaded = CachedResources { context_files, skills };
                                lock(&resources).insert(cwd.clone(), loaded.clone());
                                loaded
                            }
                        };

                        let mut snippets = IndexMap::new();
                        let mut guidelines = IndexMap::new();
                        for name in &selected_tools {
                            if let Some((snippet, guides)) = tool_contribution(name) {
                                snippets.insert(name.clone(), snippet.to_string());
                                guidelines.insert(name.clone(), guides);
                            }
                        }

                        let map = build_system_prompt_sections(&BuildSystemPromptOptions {
                            cwd,
                            selected_tools: Some(selected_tools),
                            tool_snippets: Some(snippets),
                            tool_guidelines: Some(guidelines),
                            context_files: Some(res.context_files),
                            skills: Some(res.skills),
                            ..Default::default()
                        })
                        .unwrap_or_default();
                        lock(&built_sections).insert(cache_key, map.clone());
                        map
                    }
                };

                let rendered = sections_map.get(key).cloned();
                Box::pin(futures::future::ready(Ok(rendered)))
            }),
            Some(SectionOptions { tag: Some(false) }),
        ));
    }

    define_extension(Extension {
        name: "pi-prompt".to_string(),
        sections,
        ..Default::default()
    })
}
