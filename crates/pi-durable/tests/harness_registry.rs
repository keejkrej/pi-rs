//! Port of packages/durable/test/harness-registry.test.ts

mod support;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::IndexMap;
use pi_ai::Type;
use pi_ai::types::ModelThinkingLevel;
use pi_chord::Context;
use pi_durable::documents::ErasedDocDefinition;
use pi_durable::harness::agent::{agent_hooks, resolve_agent, resolve_settings};
use pi_durable::harness::compaction::COMPACTION_TASK;
use pi_durable::harness::define::{
    SectionOptions, define_extension, define_tool, hook, section, section_with_options, wrap_section, wrap_tool,
};
use pi_durable::harness::generation::GENERATION_TASK;
use pi_durable::harness::registry::{Registry, create_registry};
use pi_durable::harness::tool::TOOL_TASK;
use pi_durable::harness::types::{
    Agent, AgentExtensionsFilter, AgentState, AgentToolsFilter, DocumentReader, DocumentReaderBackend, Extension,
    GenerationHooks, HarnessSettingsSnapshot, HookResult, ModelRef, PromptInput, PromptSection, RegistryReader,
    RegistrySnapshot, ToolExecutionResult, ToolHooks, ToolRegistration,
};
use pi_durable::tasks::{Task, TaskDefinition, define_task};
use pi_durable::types::{ConversationId, EntryId, JsonObject, TaskId};
use pretty_assertions::assert_eq;
use support::session_support::context;

fn tool(name: &str, description: Option<&str>) -> ToolRegistration {
    define_tool(ToolRegistration {
        name: name.to_owned(),
        description: description
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| format!("{name} tool")),
        parameters: Type::object([]).to_value(),
        replay: None,
        execution_mode: None,
        prepare_arguments: None,
        output_limits: None,
        execute: Arc::new(|_, _, _| Box::pin(async { Ok(ToolExecutionResult::default()) })),
    })
}

fn task(name: &str, version: i64) -> Task<(), String, (), ()> {
    let mut phases = IndexMap::new();
    phases.insert("run".to_owned(), Arc::new(|_, _, _| Box::pin(async { Ok(()) })) as _);
    define_task(TaskDefinition {
        name: name.to_owned(),
        version,
        initial: Arc::new(|_| Ok("run".to_owned())),
        phases,
        abort: Arc::new(|_, _, _| Box::pin(async { Ok(()) })),
        migrate: None,
        _hooks: std::marker::PhantomData,
    })
}

fn ext_names(items: &[Extension]) -> Vec<String> {
    items.iter().map(|item| item.name.clone()).collect()
}

fn tool_names(items: &[ToolRegistration]) -> Vec<String> {
    items.iter().map(|item| item.name.clone()).collect()
}

fn resolve_with(
    state: Option<&AgentState>,
    snapshot: &dyn RegistrySnapshot,
    settings: Option<&HarnessSettingsSnapshot>,
    reports: &Arc<Mutex<Vec<String>>>,
) -> Agent {
    let resolved_settings = resolve_settings(settings.map(|s| s as &dyn pi_durable::harness::types::HarnessSettings));
    let reports_cb = Arc::clone(reports);
    resolve_agent(state, snapshot, &resolved_settings, &move |err| {
        reports_cb
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(err.to_string());
    })
}

struct NoopReaderBackend;

#[async_trait]
impl DocumentReaderBackend for NoopReaderBackend {
    async fn snapshot_erased(
        &self,
        _def: &Arc<ErasedDocDefinition>,
        _conversation_id: Option<ConversationId>,
        _task_id: Option<TaskId>,
        _key: Option<&str>,
        _context: Context,
    ) -> pi_js::Result<Option<JsonObject>> {
        Ok(None)
    }

    async fn snapshot_as_of_erased(
        &self,
        _def: &Arc<ErasedDocDefinition>,
        _conversation_id: ConversationId,
        _key: Option<&str>,
        _at: EntryId,
        _context: Context,
    ) -> pi_js::Result<Option<JsonObject>> {
        Ok(None)
    }
}

async fn rendered(agent: &Agent) -> Vec<(String, Option<String>)> {
    let input = PromptInput {
        conversation_id: ConversationId::new_unchecked(1),
        agent: agent.clone(),
        env: None,
        shown: IndexMap::new(),
        read: DocumentReader::from_backend(Arc::new(NoopReaderBackend)),
    };
    let mut out = Vec::with_capacity(agent.sections.len());
    for item in &agent.sections {
        let text = (item.render)(input.clone(), context()).await.expect("section render");
        out.push((item.key.clone(), text));
    }
    out
}

#[test]
fn installs_replaces_in_place_and_uninstalls_extensions_by_name() {
    let registry = create_registry();
    let listener: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let listener_cb = Arc::clone(&listener);
    let registry_cb = Arc::clone(&registry);
    let _unsub = registry.subscribe(Arc::new(move || {
        listener_cb
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(ext_names(registry_cb.snapshot().installed()).join(","));
    }));

    let a = define_extension(Extension {
        name: "a".to_owned(),
        tools: vec![tool("read", Some("Read files"))],
        ..Default::default()
    });
    let b = define_extension(Extension {
        name: "b".to_owned(),
        tools: vec![tool("read", None), tool("bash", None)],
        ..Default::default()
    });
    registry.install(a.clone()).expect("install a");
    registry.install(b).expect("install b");
    let before = registry.snapshot();

    // A new object with an installed name replaces it at its position.
    let a2 = define_extension(Extension {
        name: "a".to_owned(),
        tools: vec![tool("grep", None)],
        ..Default::default()
    });
    registry.install(a2).expect("install a2");
    assert_eq!(ext_names(registry.snapshot().installed()), vec!["a", "b"]);
    assert_eq!(
        registry
            .snapshot()
            .extension("a")
            .unwrap()
            .tools
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>(),
        vec!["grep"]
    );
    let mapped_tools: Vec<String> = registry
        .snapshot()
        .tools()
        .into_iter()
        .map(|entry| format!("{}@{}", entry.tool.name, entry.extension.name))
        .collect();
    assert_eq!(mapped_tools, vec!["grep@a", "read@b", "bash@b"]);

    // Old snapshots stay as they were.
    assert_eq!(before.extension("a").unwrap().tools[0].description, "Read files");

    // Uninstall matches the name, whichever object; a later install appends.
    registry.uninstall(&a);
    registry.uninstall(&a);
    assert_eq!(ext_names(registry.snapshot().installed()), vec!["b"]);
    registry.install(a).expect("reinstall a");
    assert_eq!(ext_names(registry.snapshot().installed()), vec!["b", "a"]);
    assert_eq!(
        *listener.lock().unwrap_or_else(|e| e.into_inner()),
        vec!["a", "a,b", "a,b", "b", "b,a"]
    );
}

#[test]
fn validates_the_registry_as_it_would_be_after_an_install_and_publishes_nothing_when_invalid() {
    let registry = create_registry();
    let tasks_ext = define_extension(Extension {
        name: "tasks".to_owned(),
        tasks: vec![task("app.index", 1).erased.clone()],
        ..Default::default()
    });
    registry.install(tasks_ext).expect("install tasks");
    let published: Arc<Mutex<Vec<usize>>> = Arc::new(Mutex::new(Vec::new()));
    let published_cb = Arc::clone(&published);
    let _unsub = registry.subscribe(Arc::new(move || {
        published_cb.lock().unwrap_or_else(|e| e.into_inner()).push(1);
    }));
    let before = registry.snapshot();

    let invalid = vec![
        (
            define_extension(Extension {
                name: "x".to_owned(),
                tools: vec![tool("read", None), tool("read", None)],
                ..Default::default()
            }),
            "two tools named read",
        ),
        (
            define_extension(Extension {
                name: "x".to_owned(),
                sections: vec![
                    section("a", Arc::new(|_, _| Box::pin(async { Ok(Some("1".to_owned())) }))),
                    section("a", Arc::new(|_, _| Box::pin(async { Ok(Some("2".to_owned())) }))),
                ],
                ..Default::default()
            }),
            "two sections",
        ),
        (
            define_extension(Extension {
                name: "x".to_owned(),
                sections: vec![section(
                    "Bad Key",
                    Arc::new(|_, _| Box::pin(async { Ok(Some(String::new())) })),
                )],
                ..Default::default()
            }),
            "must match",
        ),
        (
            define_extension(Extension {
                name: "x".to_owned(),
                sections: vec![section(
                    "instructions",
                    Arc::new(|_, _| Box::pin(async { Ok(Some(String::new())) })),
                )],
                ..Default::default()
            }),
            "reserved",
        ),
        (
            define_extension(Extension {
                name: "x".to_owned(),
                tasks: vec![task("pi.generation", 1).erased.clone()],
                ..Default::default()
            }),
            "already installed",
        ),
        (
            define_extension(Extension {
                name: "x".to_owned(),
                tasks: vec![task("app.index", 1).erased.clone()],
                ..Default::default()
            }),
            "already installed",
        ),
    ];

    for (extension, message) in invalid {
        let err = registry.install(extension).expect_err("should fail validation");
        assert!(err.to_string().contains(message), "expected '{message}' in '{err}'");
    }
    assert!(Arc::ptr_eq(&registry.snapshot(), &before));
    assert!(published.lock().unwrap_or_else(|e| e.into_inner()).is_empty());

    // Replacing the extension that holds a task name is valid: the check runs on the state after replacement.
    registry
        .install(define_extension(Extension {
            name: "tasks".to_owned(),
            tasks: vec![task("app.index", 2).erased.clone()],
            ..Default::default()
        }))
        .expect("replace task version");
    assert_eq!(registry.snapshot().task("app.index").unwrap().version(), 2);
}

#[test]
fn always_holds_the_built_in_tasks_which_are_not_an_extension() {
    let registry = create_registry();
    assert!(registry.snapshot().installed().is_empty());
    let builtin_names: Vec<&str> = registry.snapshot().tasks().iter().map(|t| t.name()).collect();
    assert_eq!(
        builtin_names,
        vec![GENERATION_TASK.name(), TOOL_TASK.name(), COMPACTION_TASK.name()]
    );

    let custom = task("app.custom", 1);
    registry
        .install(define_extension(Extension {
            name: "custom".to_owned(),
            tasks: vec![custom.erased.clone()],
            ..Default::default()
        }))
        .expect("install custom");
    let with_custom: Vec<&str> = registry.snapshot().tasks().iter().map(|t| t.name()).collect();
    assert_eq!(
        with_custom,
        vec![
            GENERATION_TASK.name(),
            TOOL_TASK.name(),
            COMPACTION_TASK.name(),
            "app.custom",
        ]
    );
    assert_eq!(registry.snapshot().task("app.custom").unwrap().name(), "app.custom");
    registry.uninstall(&define_extension(Extension {
        name: "custom".to_owned(),
        ..Default::default()
    }));
    assert!(registry.snapshot().task("app.custom").is_none());
}

fn make_agent_fixtures() -> (
    Extension,
    Extension,
    Extension,
    Arc<pi_durable::harness::registry::RegistryImpl>,
) {
    let read = tool("read", None);
    let bash = tool("bash", None);
    let edit = tool("edit", None);
    let coding = define_extension(Extension {
        name: "coding".to_owned(),
        tools: vec![read, bash, edit],
        sections: vec![
            section_with_options(
                "preamble",
                Arc::new(|_, _| Box::pin(async { Ok(Some("You code.".to_owned())) })),
                Some(SectionOptions { tag: Some(false) }),
            ),
            section("cwd", Arc::new(|_, _| Box::pin(async { Ok(Some("/repo".to_owned())) }))),
        ],
        ..Default::default()
    });
    let skills = define_extension(Extension {
        name: "skills".to_owned(),
        sections: vec![section(
            "skills",
            Arc::new(|_, _| Box::pin(async { Ok(Some("S".to_owned())) })),
        )],
        ..Default::default()
    });
    let reviewer = define_extension(Extension {
        name: "reviewer".to_owned(),
        sections: vec![section(
            "role",
            Arc::new(|_, _| Box::pin(async { Ok(Some("Review.".to_owned())) })),
        )],
        ..Default::default()
    });
    let registry = create_registry();
    for extension in [&coding, &skills, &reviewer] {
        registry.install(extension.clone()).expect("install fixture");
    }
    (coding, skills, reviewer, registry)
}

#[test]
fn selects_the_default_an_array_or_the_default_edited_by_add_and_remove() {
    let (coding, skills, _, registry) = make_agent_fixtures();
    let snapshot = registry.snapshot();
    let reports = Arc::new(Mutex::new(Vec::new()));

    assert_eq!(
        ext_names(&resolve_with(None, snapshot.as_ref(), None, &reports).extensions),
        vec!["coding", "skills", "reviewer"]
    );
    let settings = HarnessSettingsSnapshot {
        extensions: Some(vec![coding, skills]),
        ..Default::default()
    };
    assert_eq!(
        ext_names(
            &resolve_with(
                Some(&AgentState::default()),
                snapshot.as_ref(),
                Some(&settings),
                &reports
            )
            .extensions
        ),
        vec!["coding", "skills"]
    );
    let reordered = AgentState {
        extensions: Some(AgentExtensionsFilter::List(vec![
            "reviewer".to_owned(),
            "coding".to_owned(),
        ])),
        ..Default::default()
    };
    assert_eq!(
        ext_names(&resolve_with(Some(&reordered), snapshot.as_ref(), Some(&settings), &reports).extensions),
        vec!["reviewer", "coding"]
    );

    // Add appends, remove drops, duplicates keep their first position, uninstalled names are skipped.
    let edited = AgentState {
        extensions: Some(AgentExtensionsFilter::Patch {
            add: Some(vec!["reviewer".to_owned(), "coding".to_owned(), "gone".to_owned()]),
            remove: Some(vec!["skills".to_owned()]),
        }),
        ..Default::default()
    };
    assert_eq!(
        ext_names(&resolve_with(Some(&edited), snapshot.as_ref(), Some(&settings), &reports).extensions),
        vec!["coding", "reviewer"]
    );

    // An old object stands for its name: the installed extension is selected.
    let stale = HarnessSettingsSnapshot {
        extensions: Some(vec![
            define_extension(Extension {
                name: "skills".to_owned(),
                ..Default::default()
            }),
            define_extension(Extension {
                name: "skills".to_owned(),
                ..Default::default()
            }),
        ]),
        ..Default::default()
    };
    let resolved_stale = resolve_with(Some(&AgentState::default()), snapshot.as_ref(), Some(&stale), &reports);
    assert_eq!(resolved_stale.extensions.len(), 1);
    assert_eq!(resolved_stale.extensions[0].name, "skills");
    assert_eq!(resolved_stale.extensions[0].sections.len(), 1);
}

#[test]
fn skips_uninstalled_names_and_resolves_them_again_once_they_are_installed() {
    let (coding, skills, _, _) = make_agent_fixtures();
    let local = create_registry();
    local.install(coding).expect("install coding");
    let reports = Arc::new(Mutex::new(Vec::new()));
    let state = AgentState {
        extensions: Some(AgentExtensionsFilter::List(vec![
            "coding".to_owned(),
            "skills".to_owned(),
        ])),
        ..Default::default()
    };
    assert_eq!(
        ext_names(&resolve_with(Some(&state), local.snapshot().as_ref(), None, &reports).extensions),
        vec!["coding"]
    );
    local.install(skills).expect("install skills");
    assert_eq!(
        ext_names(&resolve_with(Some(&state), local.snapshot().as_ref(), None, &reports).extensions),
        vec!["coding", "skills"]
    );
}

#[test]
fn replaces_same_name_tools_in_place_wraps_the_winner_then_applies_the_filter() {
    let (coding, _, _, _) = make_agent_fixtures();
    let local = create_registry();
    let venv_bash = tool("bash", Some("venv bash"));
    let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let calls_cb = Arc::clone(&calls);
    let venv = define_extension(Extension {
        name: "venv".to_owned(),
        tools: vec![venv_bash],
        ..Default::default()
    });
    let timing = define_extension(Extension {
        name: "timing".to_owned(),
        wraps: vec![
            wrap_tool(&tool("bash", None), |mut inner| {
                inner.description = format!("{} (timed)", inner.description);
                inner
            }),
            wrap_tool(&tool("bash", None), |mut inner| {
                inner.description = format!("{} [2]", inner.description);
                inner
            }),
            wrap_tool(&tool("grep", None), move |_| {
                calls_cb
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push("grep".to_owned());
                tool("grep", None)
            }),
        ],
        ..Default::default()
    });
    for extension in [coding, venv, timing] {
        local.install(extension).expect("install");
    }
    let reports = Arc::new(Mutex::new(Vec::new()));
    let agent = resolve_with(None, local.snapshot().as_ref(), None, &reports);
    let pairs: Vec<(String, String)> = agent
        .tools
        .iter()
        .map(|each| (each.name.clone(), each.description.clone()))
        .collect();
    assert_eq!(
        pairs,
        vec![
            ("read".to_owned(), "read tool".to_owned()),
            ("bash".to_owned(), "venv bash (timed) [2]".to_owned()),
            ("edit".to_owned(), "edit tool".to_owned()),
        ]
    );
    assert!(reports.lock().unwrap_or_else(|e| e.into_inner()).is_empty());
    assert!(calls.lock().unwrap_or_else(|e| e.into_inner()).is_empty());

    // An array keeps exactly these names in its order, a repeated name at its first position.
    let filtered = resolve_with(
        Some(&AgentState {
            tools: Some(AgentToolsFilter::List(vec![
                "edit".to_owned(),
                "missing".to_owned(),
                "read".to_owned(),
                "edit".to_owned(),
            ])),
            ..Default::default()
        }),
        local.snapshot().as_ref(),
        None,
        &reports,
    );
    assert_eq!(tool_names(&filtered.tools), vec!["edit", "read"]);
    let removed = resolve_with(
        Some(&AgentState {
            tools: Some(AgentToolsFilter::Remove {
                remove: vec!["bash".to_owned()],
            }),
            ..Default::default()
        }),
        local.snapshot().as_ref(),
        None,
        &reports,
    );
    assert_eq!(tool_names(&removed.tools), vec!["read", "edit"]);
}

#[test]
fn drops_a_tool_or_section_whose_wrapper_throws_or_renames_it_and_reports_the_failure() {
    let (coding, _, _, _) = make_agent_fixtures();
    let local = create_registry();
    let broken = define_extension(Extension {
        name: "broken".to_owned(),
        wraps: vec![
            // Rust wrappers are infallible (`Fn(T) -> T`), so dropping via wrapper rename exercises both tool & section failure paths.
            wrap_tool(&tool("read", None), |mut inner| {
                inner.name = "read-renamed".to_owned();
                inner
            }),
            wrap_tool(&tool("edit", None), |mut inner| {
                inner.name = "renamed".to_owned();
                inner
            }),
            wrap_section("cwd", |mut inner| {
                inner.key = "cwd-renamed".to_owned();
                inner
            }),
        ],
        ..Default::default()
    });
    local.install(coding).expect("install coding");
    local.install(broken).expect("install broken");
    let reports = Arc::new(Mutex::new(Vec::new()));
    let agent = resolve_with(None, local.snapshot().as_ref(), None, &reports);
    assert_eq!(tool_names(&agent.tools), vec!["bash"]);
    let section_keys: Vec<&str> = agent.sections.iter().map(|each| each.key.as_str()).collect();
    assert_eq!(section_keys, vec!["preamble"]);
    let recorded = reports.lock().unwrap_or_else(|e| e.into_inner()).clone();
    assert_eq!(
        recorded,
        vec![
            "Wrapper renamed read to read-renamed".to_owned(),
            "Wrapper renamed edit to renamed".to_owned(),
            "Wrapper renamed cwd to cwd-renamed".to_owned(),
        ]
    );
}

#[tokio::test]
async fn orders_sections_by_extension_replaces_same_keys_in_place_and_renders_instructions_last_and_unwrapped() {
    let (coding, skills, _, _) = make_agent_fixtures();
    let local = create_registry();
    let override_ext = define_extension(Extension {
        name: "override".to_owned(),
        sections: vec![section_with_options(
            "preamble",
            Arc::new(|_, _| Box::pin(async { Ok(Some("You review.".to_owned())) })),
            Some(SectionOptions { tag: Some(false) }),
        )],
        wraps: vec![
            wrap_section("cwd", |inner| {
                let inner_render = inner.render;
                PromptSection {
                    key: inner.key,
                    tag: inner.tag,
                    render: Arc::new(move |input, ctx| {
                        let inner_render = Arc::clone(&inner_render);
                        Box::pin(async move {
                            let val = inner_render(input, ctx).await?;
                            Ok(Some(format!("{}!", val.unwrap_or_default())))
                        })
                    }),
                }
            }),
            // Instructions are not wrapped.
            wrap_section("instructions", |inner| PromptSection {
                key: inner.key,
                tag: inner.tag,
                render: Arc::new(|_, _| Box::pin(async { Err(pi_js::Error::msg("never")) })),
            }),
        ],
        ..Default::default()
    });
    for extension in [coding, skills, override_ext] {
        local.install(extension).expect("install");
    }
    let reports = Arc::new(Mutex::new(Vec::new()));
    let agent = resolve_with(
        Some(&AgentState {
            instructions: Some("Be terse.".to_owned()),
            ..Default::default()
        }),
        local.snapshot().as_ref(),
        None,
        &reports,
    );
    assert_eq!(
        rendered(&agent).await,
        vec![
            ("preamble".to_owned(), Some("You review.".to_owned())),
            ("cwd".to_owned(), Some("/repo!".to_owned())),
            ("skills".to_owned(), Some("S".to_owned())),
            ("instructions".to_owned(), Some("Be terse.".to_owned())),
        ]
    );
    assert!(!agent.sections.last().unwrap().tag);
    assert!(reports.lock().unwrap_or_else(|e| e.into_inner()).is_empty());
}

#[test]
fn collects_hooks_of_the_selected_extensions_in_extension_order_and_applies_field_defaults() {
    let local = create_registry();
    let first = ToolHooks {
        before_tool: Some(Arc::new(|_, _, _| Box::pin(async { Ok(HookResult::Continue) }))),
        after_tool: None,
    };
    let second = ToolHooks {
        before_tool: Some(Arc::new(|_, _, _| Box::pin(async { Ok(HookResult::Continue) }))),
        after_tool: None,
    };
    let on_yield = GenerationHooks {
        before_request: None,
        after_response: None,
        on_yield: Some(Arc::new(|_, _, _| Box::pin(async { Ok(HookResult::Continue) }))),
        after_tools: None,
    };
    local
        .install(define_extension(Extension {
            name: "a".to_owned(),
            hooks: vec![hook(&TOOL_TASK, first), hook(&GENERATION_TASK, on_yield)],
            ..Default::default()
        }))
        .expect("install a");
    local
        .install(define_extension(Extension {
            name: "b".to_owned(),
            hooks: vec![hook(&TOOL_TASK, second)],
            ..Default::default()
        }))
        .expect("install b");
    let reports = Arc::new(Mutex::new(Vec::new()));

    let all_tool_hooks = agent_hooks(
        &resolve_with(None, local.snapshot().as_ref(), None, &reports),
        "pi.tool",
    );
    assert_eq!(all_tool_hooks.len(), 2);
    let reordered_hooks = agent_hooks(
        &resolve_with(
            Some(&AgentState {
                extensions: Some(AgentExtensionsFilter::List(vec!["b".to_owned(), "a".to_owned()])),
                ..Default::default()
            }),
            local.snapshot().as_ref(),
            None,
            &reports,
        ),
        "pi.tool",
    );
    assert_eq!(reordered_hooks.len(), 2);
    assert!(
        agent_hooks(
            &resolve_with(
                Some(&AgentState {
                    extensions: Some(AgentExtensionsFilter::List(vec!["b".to_owned()])),
                    ..Default::default()
                }),
                local.snapshot().as_ref(),
                None,
                &reports,
            ),
            "pi.generation",
        )
        .is_empty()
    );

    let defaults = resolve_with(None, local.snapshot().as_ref(), None, &reports);
    assert!(defaults.model.is_none());
    assert_eq!(defaults.thinking_level, ModelThinkingLevel::Off);
    assert!(defaults.cwd.is_none());
    let custom = resolve_with(
        Some(&AgentState {
            model: Some(ModelRef {
                provider: "p".to_owned(),
                model_id: "m".to_owned(),
            }),
            thinking_level: Some(ModelThinkingLevel::High),
            cwd: Some("/w".to_owned()),
            ..Default::default()
        }),
        local.snapshot().as_ref(),
        None,
        &reports,
    );
    assert_eq!(
        custom.model,
        Some(ModelRef {
            provider: "p".to_owned(),
            model_id: "m".to_owned()
        })
    );
    assert_eq!(custom.thinking_level, ModelThinkingLevel::High);
    assert_eq!(custom.cwd.as_deref(), Some("/w"));
}
