//! Port of packages/durable/test/spec-usage.test.ts
//!
//! The usage examples of docs/spec.md, compile-checked, plus an offline smoke test exercising the
//! standalone example modules under `support::examples`.
#![allow(dead_code, unused_variables, clippy::all)]

mod support;

use std::marker::PhantomData;
use std::sync::Arc;

use indexmap::IndexMap;
use pi_ai::types::{AssistantMessage, Message, ToolCall};
use pi_ai::{Models, Type};
use pi_chord::Context;
use pi_durable::documents::{ConversationDocToken, DocDefinitionInput, define_conversation_doc};
use pi_durable::env::ExecutionEnv;
use pi_durable::harness::types::{
    EnvTarget, HarnessSettings, ToolExecutionApi, ToolExecutionResult, ToolResultContentBlock,
};
use pi_durable::tools::{BashToolOptions, coding_tools, create_bash_tool, create_edit_tool, create_read_tool};
use pi_durable::types::{
    ConversationId, ConversationOwnership, ConversationQuery, EntryDraft, EntryId, InputSubmissionState, JsonValue,
    LatestFork, NextTaskState, SubmissionState, TaskId, TaskOptions, TaskOutcome, TaskOwnership,
};
use pi_durable::{
    AgentChange, AgentExtensionsChange, AgentToolsChange, DocDraft, Extension, FieldChange, GENERATION_TASK,
    GenerationHooks, Harness, HarnessOptions, HarnessSettingsSnapshot, HookApi, InputSubmissionDraft, LIVE_DOC,
    LiveState, ModelRef, PartialCompactionPolicy, Registry, RegistryReader, RootConversationOptions, SectionOptions,
    Session, Storage, TOOL_TASK, ToolHooks, ToolRegistration, ToolReplayMode, Tx, UserInput, configure,
    create_registry, define_extension, define_task, define_tool, hook, section, section_with_options, wrap_tool,
};
use serde::{Deserialize, Serialize};

// ─── Names the spec leaves to the application ────────────────────────────────

struct SpecStubs {
    context: Context,
    storage: Arc<dyn Storage>,
    models: Models,
    session: Session,
    conversation_id: ConversationId,
    message: EntryDraft,
    haiku: ModelRef,
    sonnet: ModelRef,
    worktree: String,
    name: String,
    manager: Box<dyn SettingsManager>,
}

fn stubs() -> SpecStubs {
    unreachable!("compile-only spec snippet")
}

fn local_env(_cwd: &str) -> pi_js::Result<Option<Arc<dyn ExecutionEnv>>> {
    unreachable!("compile-only spec snippet")
}

async fn container_env(_image: &str, _cwd: &str, _context: Context) -> pi_js::Result<Option<Arc<dyn ExecutionEnv>>> {
    unreachable!("compile-only spec snippet")
}

fn render_agents_md() -> Option<String> {
    unreachable!("compile-only spec snippet")
}

fn render_skills() -> Option<String> {
    unreachable!("compile-only spec snippet")
}

fn is_dangerous(_call: &ToolCall) -> bool {
    unreachable!("compile-only spec snippet")
}

fn writes(_call: &ToolCall) -> bool {
    unreachable!("compile-only spec snippet")
}

async fn request_second_pass(
    _answer: AssistantMessage,
    _api: HookApi,
    _context: Context,
) -> pi_js::Result<Option<UserInput>> {
    unreachable!("compile-only spec snippet")
}

fn record_metric(_name: &str, _ms: i64) {
    unreachable!("compile-only spec snippet")
}

async fn answer_text(_api: &ToolExecutionApi, _entry: EntryId, _context: Context) -> pi_js::Result<String> {
    unreachable!("compile-only spec snippet")
}

async fn payments_charge(_key: &str) -> pi_js::Result<String> {
    unreachable!("compile-only spec snippet")
}

async fn payments_cancel(_checkpoint: &ChargeState) -> pi_js::Result<()> {
    unreachable!("compile-only spec snippet")
}

fn new_key() -> String {
    unreachable!("compile-only spec snippet")
}

fn receipt_entry(id: String) -> EntryDraft {
    EntryDraft {
        kind: "payment.receipt".to_owned(),
        data: Some(JsonValue::String(id)),
        ..Default::default()
    }
}

trait SettingsManager: Send + Sync {
    fn timeout_ms(&self) -> u64;
    fn auto_compact(&self) -> bool;
    fn set_auto_compact(&self, value: bool);
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "camelCase")]
enum ChargeState {
    Prepare,
    Charge { key: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct ChargeResult {
    entry_id: EntryId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct FollowState {
    phase: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct PlanModeDoc {
    enabled: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct ContainerDoc {
    image: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ChildDetails {
    conversation_id: ConversationId,
}

/// Never called: the stub names above panic if executed.
async fn examples() -> pi_js::Result<()> {
    let s = stubs();
    let read_tool = create_read_tool();
    let edit_tool = create_edit_tool();
    let bash_tool = create_bash_tool(None);

    // ─── Section 7.1: extensions and host setup ──────────────────────────────────

    let context_files = define_extension(Extension {
        name: "context-files".to_owned(),
        sections: vec![section(
            "agents-md",
            Arc::new(|_input, _ctx| Box::pin(async { Ok(render_agents_md()) })),
        )],
        ..Default::default()
    });
    let skills = define_extension(Extension {
        name: "skills".to_owned(),
        sections: vec![section(
            "skills",
            Arc::new(|_input, _ctx| Box::pin(async { Ok(render_skills()) })),
        )],
        ..Default::default()
    });
    let skills_v2 = define_extension(Extension {
        name: "skills".to_owned(),
        sections: vec![section(
            "skills",
            Arc::new(|_input, _ctx| Box::pin(async { Ok(render_skills()) })),
        )],
        ..Default::default()
    });
    let coding = define_extension(Extension {
        name: "coding".to_owned(),
        sections: vec![
            section_with_options(
                "preamble",
                Arc::new(|_input, _ctx| Box::pin(async { Ok(Some("You are an expert coding assistant.".to_owned())) })),
                Some(SectionOptions { tag: Some(false) }),
            ),
            // The environment the host built for this conversation, in the conversation's directory.
            section(
                "cwd",
                Arc::new(|input, _ctx| {
                    Box::pin(async move {
                        Ok(input
                            .env
                            .as_ref()
                            .map(|env| format!("Working directory: {}", env.cwd())))
                    })
                }),
            ),
        ],
        ..Default::default()
    });
    let permissions = define_extension(Extension {
        name: "permissions".to_owned(),
        hooks: vec![hook(
            &TOOL_TASK,
            ToolHooks {
                before_tool: Some(Arc::new(|call, _api, _ctx| {
                    Box::pin(async move {
                        Ok(if is_dangerous(&call) {
                            Some(pi_durable::BeforeToolDecision::Block("Needs approval".to_owned()))
                        } else {
                            None
                        })
                    })
                })),
                after_tool: None,
            },
        )],
        ..Default::default()
    });
    // A role and a review loop, for conversations that select it.
    let reviewer = define_extension(Extension {
        name: "reviewer".to_owned(),
        sections: vec![section(
            "role",
            Arc::new(|_input, _ctx| {
                Box::pin(async {
                    Ok(Some(
                        "You review diffs. Report problems as a list. Never edit files.".to_owned(),
                    ))
                })
            }),
        )],
        hooks: vec![hook(
            &*GENERATION_TASK,
            GenerationHooks {
                on_yield: Some(Arc::new(|answer, api, ctx| {
                    Box::pin(request_second_pass(answer, api, ctx))
                })),
                ..Default::default()
            },
        )],
        ..Default::default()
    });

    let timing = define_extension(Extension {
        name: "timing".to_owned(),
        wraps: vec![wrap_tool(&bash_tool, |tool| {
            let orig = Arc::clone(&tool.execute);
            ToolRegistration {
                execute: Arc::new(move |args, api, call_context| {
                    let orig = Arc::clone(&orig);
                    Box::pin(async move {
                        let start = pi_js::time::now_ms();
                        let res = orig(args, api, call_context).await;
                        record_metric("bash", pi_js::time::now_ms() - start);
                        res
                    })
                }),
                ..tool
            }
        })],
        ..Default::default()
    });
    // A bash inside a Python virtualenv for one conversation: it replaces CodingTools' bash in place,
    // and Timing, if selected, wraps it.
    let venv = define_extension(Extension {
        name: "venv".to_owned(),
        tools: vec![create_bash_tool(Some(BashToolOptions {
            command_prefix: Some("source .venv/bin/activate".to_owned()),
            prepare: None,
        }))],
        ..Default::default()
    });

    // ─── Section 7.2: hooks reading extension state ──────────────────────────────

    let plan_mode_doc: ConversationDocToken<PlanModeDoc> = define_conversation_doc(
        LatestFork::Current,
        DocDefinitionInput {
            kind: "app.plan-mode",
            version: 1,
            initial: PlanModeDoc::default,
            migrate: None,
            checkpoint_when: None,
        },
    )?;
    let plan_doc_hook = plan_mode_doc.clone();
    let plan_mode = define_extension(Extension {
        name: "plan-mode".to_owned(),
        hooks: vec![hook(
            &TOOL_TASK,
            ToolHooks {
                // An absent document means plan mode is off.
                before_tool: Some(Arc::new(move |call, api, call_context| {
                    let plan_tok = plan_doc_hook.clone();
                    Box::pin(async move {
                        let enabled = api
                            .read()
                            .snapshot_conversation(&plan_tok, api.conversation_id(), call_context)
                            .await?
                            .is_some_and(|doc| doc.enabled);
                        Ok(if enabled && writes(&call) {
                            Some(pi_durable::BeforeToolDecision::Block("Plan mode: read-only".to_owned()))
                        } else {
                            None
                        })
                    })
                })),
                after_tool: None,
            },
        )],
        ..Default::default()
    });

    // ─── Section 7.3: subagents ──────────────────────────────────────────────────

    let subagent = define_extension(Extension {
        name: "subagent".to_owned(),
        tools: vec![define_tool(ToolRegistration {
            name: "subagent".to_owned(),
            description: "Delegate a self-contained task to a subagent and get its answer back.".to_owned(),
            parameters: Type::object([("task", Type::string())]).to_value(),
            replay: Some(ToolReplayMode::Safe),
            execution_mode: None,
            prepare_arguments: None,
            output_limits: None,
            execute: Arc::new(|args, api, call_context| {
                Box::pin(async move {
                    let task = args.get("task").and_then(|v| v.as_str()).unwrap_or_default().to_owned();
                    let task_id = api.task_id();
                    let child = api
                        .commit(
                            move |tx| async move {
                                let existing = tx
                                    .scan_conversations(
                                        ConversationQuery {
                                            owner_conversation_id: None,
                                            owner_task_id: Some(task_id),
                                        },
                                        1,
                                        None,
                                    )
                                    .await?;
                                if let Some(found) = existing.items.into_iter().next() {
                                    return Ok(found.id);
                                }
                                // Starts as a copy of this conversation's agent: model, thinking level, cwd, extensions, tools.
                                let created = tx.create_conversation(ConversationOwnership::Task { task_id }).await?;
                                // Without this extension, the child is not offered this tool.
                                configure(
                                    &tx,
                                    created.id,
                                    &AgentChange {
                                        extensions: FieldChange::Set(AgentExtensionsChange::Patch {
                                            add: None,
                                            remove: Some(vec!["subagent".to_owned()]),
                                        }),
                                        ..Default::default()
                                    },
                                )
                                .await?;
                                Ok(created.id)
                            },
                            call_context.clone(),
                        )
                        .await?;
                    api.details(&ChildDetails { conversation_id: child }, call_context.clone())
                        .await?;
                    let handle = api
                        .conversation(child, call_context.clone())
                        .await?
                        .expect("child exists");
                    let settled = handle
                        .submit(
                            InputSubmissionDraft {
                                request_id: Some(format!("subagent:{task_id}")),
                                content: UserInput::Text(task),
                                when_busy: None,
                            },
                            call_context.clone(),
                        )
                        .await?
                        .wait(call_context.clone())
                        .await?;
                    let SubmissionState::Input(InputSubmissionState::Done { answer, .. }) = settled.record.state else {
                        return Err(pi_js::Error::msg(format!(
                            "Subagent failed: {:?}",
                            settled.record.status()
                        )));
                    };
                    let text = answer_text(&api, answer, call_context).await?;
                    Ok(ToolExecutionResult {
                        content: Some(vec![ToolResultContentBlock::Text(pi_ai::types::TextContent::new(text))]),
                        ..Default::default()
                    })
                })
            }),
        })],
        ..Default::default()
    });

    // ─── Section 7.4 ─────────────────────────────────────────────────────────────

    let chat = define_extension(Extension {
        name: "chat".to_owned(),
        sections: vec![section_with_options(
            "preamble",
            Arc::new(|_input, _ctx| Box::pin(async { Ok(Some("You are a helpful assistant.".to_owned())) })),
            Some(SectionOptions { tag: Some(false) }),
        )],
        ..Default::default()
    });

    // ─── Section 5.1: a task with intent, effect, outcome ────────────────────────

    let mut payment_phases: IndexMap<String, pi_durable::tasks::TypedPhaseHandler<(), ChargeState, ChargeResult, ()>> =
        IndexMap::new();
    payment_phases.insert(
        "prepare".to_owned(),
        Arc::new(|_task, runtime, task_context| {
            Box::pin(async move {
                runtime
                    .commit(
                        |_tx, _current| {
                            Box::pin(async move {
                                Ok(Some(NextTaskState::Running {
                                    checkpoint: ChargeState::Charge { key: new_key() },
                                }))
                            })
                        },
                        task_context,
                    )
                    .await
            })
        }),
    );
    payment_phases.insert(
        "charge".to_owned(),
        Arc::new(|task, runtime, task_context| {
            Box::pin(async move {
                let key = match &task.checkpoint {
                    ChargeState::Charge { key } => key.clone(),
                    ChargeState::Prepare => String::new(),
                };
                let receipt = payments_charge(&key).await?; // idempotent by key
                runtime
                    .commit(
                        move |tx, current| {
                            Box::pin(async move {
                                let entry = tx.append_entry(current.conversation_id, receipt_entry(receipt)).await?;
                                Ok(Some(NextTaskState::Terminal {
                                    outcome: TaskOutcome::Completed {
                                        result: ChargeResult { entry_id: entry.id },
                                    },
                                }))
                            })
                        },
                        task_context,
                    )
                    .await
            })
        }),
    );
    let _payment = define_task(TaskDefinition {
        name: "app.payment".to_owned(),
        version: 1,
        initial: Arc::new(|_| Ok(ChargeState::Prepare)),
        phases: payment_phases,
        // The abort handler decides the outcome; leaving without committing faults the task.
        abort: Arc::new(|task, runtime, task_context| {
            Box::pin(async move {
                payments_cancel(&task.checkpoint).await?;
                runtime
                    .commit(
                        |_tx, _current| {
                            Box::pin(async move {
                                Ok(Some(NextTaskState::Terminal {
                                    outcome: TaskOutcome::Aborted {
                                        reason: Some("user".to_owned()),
                                        result: None,
                                    },
                                }))
                            })
                        },
                        task_context,
                    )
                    .await
            })
        }),
        migrate: None,
        _hooks: PhantomData,
    });

    let mut follow_phases: IndexMap<String, pi_durable::tasks::TypedPhaseHandler<(), FollowState, (), ()>> =
        IndexMap::new();
    follow_phases.insert(
        "follow".to_owned(),
        Arc::new(|_task, _runtime, _ctx| Box::pin(async { Ok(()) })),
    );
    let follow = define_task(TaskDefinition {
        name: "app.follow".to_owned(),
        version: 1,
        initial: Arc::new(|_| {
            Ok(FollowState {
                phase: "follow".to_owned(),
            })
        }),
        phases: follow_phases,
        abort: Arc::new(|_task, _runtime, _ctx| Box::pin(async { Ok(()) })),
        migrate: None,
        _hooks: PhantomData,
    });

    // ─── Sequences ───────────────────────────────────────────────────────────────

    // Section 2.2: a tool's commit creates a configured child.
    let child_in_tool_commit = |tx: Tx, task_id: TaskId| {
        let haiku = s.haiku.clone();
        let read_tool = read_tool.clone();
        let worktree = s.worktree.clone();
        async move {
            // In a tool's commit. The child starts as a copy of this conversation's agent: model, extensions, tools, cwd.
            let child = tx.create_conversation(ConversationOwnership::Task { task_id }).await?;
            // A cheaper model, only the read tool, and its own worktree; everything else stays as copied.
            configure(
                &tx,
                child.id,
                &AgentChange {
                    model: FieldChange::Set(haiku),
                    tools: FieldChange::Set(AgentToolsChange::replace(&[read_tool])),
                    cwd: FieldChange::Set(worktree),
                    ..Default::default()
                },
            )
            .await
        }
    };
    let _ = child_in_tool_commit;

    // Section 2.2: settings read live through trait getters.
    struct UserSettings {
        manager: Box<dyn SettingsManager>,
    }
    impl HarnessSettings for UserSettings {
        fn get(&self) -> HarnessSettingsSnapshot {
            HarnessSettingsSnapshot {
                stream: Some(pi_durable::ConversationStreamOptions {
                    timeout_ms: Some(self.manager.timeout_ms()),
                    ..Default::default()
                }),
                compaction: Some(PartialCompactionPolicy {
                    enabled: Some(self.manager.auto_compact()),
                    ..Default::default()
                }),
                ..Default::default()
            }
        }
    }
    s.manager.set_auto_compact(false); // no Session write; every conversation follows at its next threshold check
    let _live_settings: Arc<dyn HarnessSettings> = Arc::new(UserSettings { manager: s.manager });

    // Section 2.2: an environment per conversation.
    let container_doc: ConversationDocToken<ContainerDoc> = define_conversation_doc(
        LatestFork::Current,
        DocDefinitionInput {
            kind: "app.container",
            version: 1,
            initial: || ContainerDoc {
                image: "node:22".to_owned(),
            },
            migrate: None,
            checkpoint_when: None,
        },
    )?;
    let _container_harness = Harness::open(
        Arc::clone(&s.storage),
        HarnessOptions {
            models: s.models.clone(),
            registry: create_registry() as Arc<dyn RegistryReader>,
            settings: None,
            env: Some(Arc::new(move |target: EnvTarget, env_context| {
                let container_tok = container_doc.clone();
                Box::pin(async move {
                    let container = target
                        .read
                        .snapshot_conversation(&container_tok, target.conversation_id, env_context.clone())
                        .await?;
                    match container {
                        Some(c) => container_env(&c.image, target.cwd.as_deref().unwrap_or("/work"), env_context).await,
                        None => local_env(target.cwd.as_deref().unwrap_or("")),
                    }
                })
            })),
            conversation_created: None,
            now: None,
            on_report: None,
        },
        s.context.clone(),
    )
    .await?;

    // Sections 2.2 and 7.1: host setup, tool filters, extension selection, plan mode, and reload.
    let all_coding = coding_tools();
    let registry = create_registry();
    for ext in [
        all_coding.clone(),
        coding.clone(),
        context_files.clone(),
        skills.clone(),
        permissions.clone(),
        reviewer,
    ] {
        registry.install(ext)?;
    }
    let host_settings: Arc<dyn HarnessSettings> = Arc::new(HarnessSettingsSnapshot {
        extensions: Some(vec![all_coding, coding, context_files, skills.clone(), permissions]),
        ..Default::default()
    });
    let harness = Harness::open(
        Arc::clone(&s.storage),
        HarnessOptions {
            models: s.models,
            registry: Arc::clone(&registry) as Arc<dyn RegistryReader>,
            settings: Some(host_settings),
            env: Some(Arc::new(|target: EnvTarget, _ctx| {
                Box::pin(async move { local_env(target.cwd.as_deref().unwrap_or("")) })
            })),
            conversation_created: None,
            now: None,
            on_report: None,
        },
        s.context.clone(),
    )
    .await?;
    let root = harness
        .root(
            s.context.clone(),
            Some(RootConversationOptions {
                agent: Some(AgentChange {
                    model: FieldChange::Set(s.sonnet),
                    cwd: FieldChange::Set(pi_js::env::cwd()),
                    ..Default::default()
                }),
                init: None,
            }),
        )
        .await?;

    root.configure(
        AgentChange {
            tools: FieldChange::Set(AgentToolsChange::remove(&[edit_tool])),
            ..Default::default()
        },
        s.context.clone(),
    )
    .await?;
    root.configure(
        AgentChange {
            tools: FieldChange::Set(AgentToolsChange::remove(&[bash_tool])),
            ..Default::default()
        },
        s.context.clone(),
    )
    .await?; // edit is offered again
    root.configure(
        AgentChange {
            tools: FieldChange::Clear,
            ..Default::default()
        },
        s.context.clone(),
    )
    .await?; // every tool of the selected extensions again

    registry.install(timing)?;
    registry.install(venv.clone())?; // installed, but not in the default selection
    root.configure(
        AgentChange {
            extensions: FieldChange::Set(AgentExtensionsChange::Patch {
                add: Some(vec![venv.name]),
                remove: None,
            }),
            ..Default::default()
        },
        s.context.clone(),
    )
    .await?;

    registry.install(plan_mode)?;
    let root_id = root.id();
    let plan_tok = plan_mode_doc.clone();
    root.commit(
        move |tx| async move {
            tx.doc_conversation(&plan_tok, root_id)
                .await?
                .update(|doc| doc.enabled = true)?;
            Ok(())
        },
        s.context.clone(),
    )
    .await?;

    registry.install(subagent)?;
    registry.install(chat)?;
    registry.install(skills_v2)?; // same name: conversations selecting skills render v2 at their next request
    registry.uninstall(&skills); // selecting conversations get a system delta removing its section

    // Section 4: table reads before the first table write; documents stay usable.
    let conv_id = s.conversation_id;
    let entry_draft = s.message;
    let follow_tok = follow;
    s.session
        .commit(
            move |tx| async move {
                let _conversation = tx.conversation(conv_id).await?; // table read
                let live = tx.doc_conversation(&*LIVE_DOC, conv_id).await?;

                tx.append_entry(conv_id, entry_draft).await?; // first table write
                live.update(|state| state.generation = None)?; // document mutation remains valid
                tx.create_task(
                    &follow_tok,
                    &(),
                    TaskOptions {
                        ownership: TaskOwnership::Conversation,
                        conversation_id: Some(conv_id),
                        background: false,
                    },
                )
                .await?; // further table writes are fine
                Ok(())
            },
            s.context.clone(),
        )
        .await?;

    // Section 3.4: DocDraft enforces transaction lifetime, returning an error if used outside commit.
    let mut escaped: Option<DocDraft<LiveState>> = None;
    let escaped_slot = &mut escaped;
    s.session
        .commit(
            |tx| async move {
                let draft = tx.doc_conversation(&*LIVE_DOC, conv_id).await?;
                let _ = draft;
                Ok(())
            },
            s.context,
        )
        .await?;
    let _ = escaped_slot;

    Ok(())
}

#[test]
fn compiles_the_specs_usage_examples() {
    let _fn_ptr = examples;
    let _msg_size = std::mem::size_of::<Message>();
}

#[tokio::test]
async fn examples_smoke() -> pi_js::Result<()> {
    support::examples::n00_conversation::run().await?;
    support::examples::n01_documents::run().await?;
    support::examples::n02_forks::run().await?;
    support::examples::n03_owned_conversations::run().await?;
    support::examples::n04_chord_state::run().await?;
    support::examples::n05_watches::run().await?;
    support::examples::n06_harness::run().await?;
    support::examples::n07_configuration::run().await?;
    support::examples::n08_harness_conversations::run().await?;
    support::examples::n09_context::run().await?;
    support::examples::n10_registry_reload::run().await?;
    support::examples::n11_extension_state::run().await?;
    support::examples::n12_tasks::run().await?;
    support::examples::n13_recovery::run().await?;
    support::examples::n14_chat::run().await?;
    support::examples::n15_system_prompt::run().await?;
    support::examples::n17_coding_tools::run().await?;
    support::examples::n18_print::run().await?;
    support::examples::n19_json::run().await?;
    support::examples::n20_inbox::run().await?;
    support::examples::n21_late_join::run().await?;
    support::examples::n22_subagent_foreground::run().await?;
    support::examples::n23_subagent_background::run().await?;
    support::examples::n24_child_tasks::run().await?;
    support::examples::n25_compaction::run().await?;
    support::examples::n26_coding_agent::run().await?;
    support::examples::n27_plan_mode::run().await?;
    support::examples::n28_reviewer::run().await?;
    support::examples::n29_sandbox_per_conversation::run().await?;
    support::examples::n30_tool_override::run().await?;
    support::examples::n31_reload_and_restart::run().await?;
    Ok(())
}
