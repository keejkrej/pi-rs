//! Port of packages/coding-agent/test/suite/regressions/9221-reload-during-tool.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use pi_agent_core::types::{AgentMessage, AgentToolResult};
use pi_ai::providers::faux::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxContentBlock, FauxResponseStep,
    faux_assistant_message, faux_tool_call,
};
use pi_ai::types::{
    Message, StopReason, TextContent, ToolResultMessage, UserContentBlock, UserMessage, UserMessageContent,
};
use pi_coding_agent::core::agent_session::{ExtensionBindings, NavigateTreeOptions};
use pi_coding_agent::core::compaction::CompactionResult;
use pi_coding_agent::core::diagnostics::ResourceDiagnostic;
use pi_coding_agent::core::extensions::types::{
    CancelledResult, ExtensionApi, ExtensionCommandContextActions, InlineExtension, InputSource,
    RegisteredCommandOptions, SessionBeforeCompactEvent, SessionBeforeCompactResult, SessionBeforeTreeEvent,
    SessionEntry, SessionShutdownEvent, SessionStartEvent, SessionStartReason, ToolDefinition,
};
use pi_coding_agent::core::prompt_templates::PromptTemplate;
use pi_coding_agent::core::resource_loader::{
    ContextFile, LoadExtensionsResult as LoaderLoadExtensionsResult, PromptSource, ResourceExtensionPaths,
    ResourceLoader, ResourceLoaderReloadOptions, Theme as LoaderTheme,
};
use pi_coding_agent::core::settings_manager::{CompactionSettings, Settings};
use pi_coding_agent::core::skills::Skill;
use pi_js::BoxFuture;
use pi_js::vendor::typebox::t;
use support::suite::harness::{HarnessOptions, create_harness, create_suite_extensions_result, get_message_text};
use tokio::sync::{Mutex, Notify};

fn user_msg(text: &str) -> AgentMessage {
    AgentMessage::from(Message::User(UserMessage {
        content: UserMessageContent::Blocks(vec![UserContentBlock::Text(TextContent::new(text))]),
        timestamp: pi_js::time::now_ms(),
    }))
}

fn assistant_msg(text: &str) -> AgentMessage {
    AgentMessage::from(Message::Assistant(faux_assistant_message(
        FauxAssistantMessageContent::Text(text.to_string()),
        None,
    )))
}

#[tokio::test]
async fn rejects_command_and_direct_reload_without_invalidating_an_active_tool_context() {
    for source in ["command", "direct"] {
        let started = Arc::new(Notify::new());
        let released = Arc::new(Notify::new());
        let shutdown_calls = Arc::new(AtomicUsize::new(0));
        let errors = Arc::new(Mutex::new(Vec::<String>::new()));
        let provider_results = Arc::new(Mutex::new(Vec::<ToolResultMessage>::new()));

        let s_c = Arc::clone(&started);
        let r_c = Arc::clone(&released);
        let sh_c = Arc::clone(&shutdown_calls);

        let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
            let s_c = Arc::clone(&s_c);
            let r_c = Arc::clone(&r_c);
            let sh_c = Arc::clone(&sh_c);
            async move {
                pi.on::<SessionShutdownEvent>(move |_ev, _ctx| {
                    let sh_c = Arc::clone(&sh_c);
                    Box::pin(async move {
                        sh_c.fetch_add(1, Ordering::SeqCst);
                        Ok(None)
                    })
                });
                pi.register_command(
                    "reload-runtime",
                    RegisteredCommandOptions {
                        description: Some("Reload extensions".to_string()),
                        get_argument_completions: None,
                        handler: Arc::new(|_args, ctx| Box::pin(async move { ctx.reload().await })),
                    },
                )?;
                let _ = pi.register_tool(ToolDefinition {
                    name: "wait_then_succeed".to_string(),
                    label: "Wait then succeed".to_string(),
                    description: "Use the tool context after waiting".to_string(),
                    prompt_snippet: None,
                    prompt_guidelines: None,
                    parameters: t::object([] as [(&str, pi_js::vendor::typebox::Schema); 0]),
                    constrained_sampling: None,
                    render_shell: None,
                    prepare_arguments: None,
                    output_schema: None,
                    exposure: None,
                    namespace: None,
                    annotations: None,
                    default_active: None,
                    prepare_loadout: None,
                    execution_mode: None,
                    execute: Arc::new(move |_id, _params, _sig, _upd, ctx| {
                        let s_c = Arc::clone(&s_c);
                        let r_c = Arc::clone(&r_c);
                        Box::pin(async move {
                            let wait = r_c.notified();
                            s_c.notify_one();
                            wait.await;
                            let _ = ctx.as_context().get_system_prompt()?;
                            Ok(AgentToolResult::text("tool succeeded"))
                        })
                    }),
                    render_call: None,
                    render_result: None,
                });
                Ok(())
            }
        }));

        let harness = create_harness(HarnessOptions {
            extension_factories: Some(vec![ext]),
            ..Default::default()
        })
        .await;

        let errs_c = Arc::clone(&errors);
        let s_idle = harness.session.clone();
        let s_nav = harness.session.clone();
        let s_reload = harness.session.clone();
        harness
            .session
            .bind_extensions(ExtensionBindings {
                mode: Some(pi_coding_agent::core::extensions::types::ExtensionMode::Rpc),
                on_error: Some(Arc::new(move |err| {
                    let errs_c = Arc::clone(&errs_c);
                    tokio::spawn(async move {
                        errs_c.lock().await.push(err.error);
                    });
                })),
                command_context_actions: Some(ExtensionCommandContextActions {
                    wait_for_idle: Arc::new(move || {
                        let s = s_idle.clone();
                        Box::pin(async move {
                            s.wait_for_idle().await;
                            Ok(())
                        })
                    }),
                    new_session: Arc::new(|_| Box::pin(async { Ok(CancelledResult { cancelled: false }) })),
                    fork: Arc::new(|_, _| Box::pin(async { Ok(CancelledResult { cancelled: false }) })),
                    navigate_tree: Arc::new(move |id, opts| {
                        let s = s_nav.clone();
                        let id = id.to_string();
                        let mapped = opts.map(|o| NavigateTreeOptions {
                            summarize: o.summarize,
                            custom_instructions: o.custom_instructions,
                            replace_instructions: o.replace_instructions,
                            label: o.label,
                        });
                        Box::pin(async move {
                            let r = s.navigate_tree(&id, mapped).await?;
                            Ok(CancelledResult { cancelled: r.cancelled })
                        })
                    }),
                    switch_session: Arc::new(|_, _| Box::pin(async { Ok(CancelledResult { cancelled: false }) })),
                    reload: Arc::new(move || {
                        let s = s_reload.clone();
                        Box::pin(async move { s.reload(None).await })
                    }),
                }),
                ..Default::default()
            })
            .await
            .expect("bind");

        let runner = harness.session.extension_runner().expect("runner");
        let pr_c = Arc::clone(&provider_results);
        harness.set_responses(vec![
            FauxResponseStep::Message(faux_assistant_message(
                FauxAssistantMessageContent::Blocks(vec![FauxContentBlock::ToolCall(faux_tool_call(
                    "wait_then_succeed",
                    Default::default(),
                    None,
                ))]),
                Some(FauxAssistantMessageOptions {
                    stop_reason: Some(StopReason::ToolUse),
                    ..Default::default()
                }),
            )),
            FauxResponseStep::Factory(Arc::new(move |context, _opts, _st, _mdl| {
                let pr_c = Arc::clone(&pr_c);
                Box::pin(async move {
                    let trs: Vec<ToolResultMessage> = context
                        .messages
                        .into_iter()
                        .filter_map(|m| match m {
                            Message::ToolResult(tr) => Some(tr),
                            _ => None,
                        })
                        .collect();
                    *pr_c.lock().await = trs;
                    Ok(faux_assistant_message(
                        FauxAssistantMessageContent::Text("done".to_string()),
                        None,
                    ))
                })
            })),
        ]);

        let started_wait = started.notified();
        let s_p = harness.session.clone();
        let prompt_task = tokio::spawn(async move { s_p.prompt("run the tool", None).await });
        started_wait.await;

        if source == "command" {
            harness
                .session
                .prompt(
                    "/reload-runtime",
                    Some(pi_coding_agent::core::agent_session::PromptOptions {
                        source: Some(InputSource::Rpc),
                        ..Default::default()
                    }),
                )
                .await
                .expect("command returns without panicking");
        } else {
            let err = harness
                .session
                .reload(None)
                .await
                .expect_err("expected direct reload error");
            assert!(
                err.to_string()
                    .contains("Wait for the current response to finish before reloading."),
                "err: {err}"
            );
        }

        released.notify_one();
        prompt_task.await.expect("join").expect("prompt ok");

        let persisted: Vec<ToolResultMessage> = harness
            .session_manager
            .get_entries()
            .into_iter()
            .filter_map(|e| match e {
                SessionEntry::Message(m) => match m.message {
                    AgentMessage::Message(Message::ToolResult(tr)) => Some(tr),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        assert_eq!(persisted.len(), 1);
        assert!(!persisted[0].is_error);
        let text = get_message_text(&AgentMessage::from(Message::ToolResult(persisted[0].clone())));
        assert_eq!(text, "tool succeeded");
        let seen_trs = provider_results.lock().await.clone();
        assert_eq!(seen_trs, persisted);

        // Give async on_error task a turn if any
        tokio::task::yield_now().await;
        let errs = errors.lock().await.clone();
        if source == "command" {
            assert_eq!(
                errs,
                vec!["Wait for the current response to finish before reloading.".to_string()]
            );
        } else {
            assert!(errs.is_empty());
        }
        assert_eq!(shutdown_calls.load(Ordering::SeqCst), 0);
        assert!(
            runner
                .get_active_tools()
                .expect("still active")
                .contains(&"wait_then_succeed".to_string())
        );

        harness.cleanup();
    }
}

#[tokio::test]
async fn rejects_reload_during_compaction_and_tree_navigation() {
    for operation in ["compaction", "tree navigation"] {
        let started = Arc::new(Notify::new());
        let released = Arc::new(Notify::new());
        let shutdown_calls = Arc::new(AtomicUsize::new(0));

        let s_c = Arc::clone(&started);
        let r_c = Arc::clone(&released);
        let sh_c = Arc::clone(&shutdown_calls);

        let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
            let s_c = Arc::clone(&s_c);
            let r_c = Arc::clone(&r_c);
            let sh_c = Arc::clone(&sh_c);
            async move {
                pi.on::<SessionShutdownEvent>(move |_ev, _ctx| {
                    let sh_c = Arc::clone(&sh_c);
                    Box::pin(async move {
                        sh_c.fetch_add(1, Ordering::SeqCst);
                        Ok(None)
                    })
                });
                let s1 = Arc::clone(&s_c);
                let r1 = Arc::clone(&r_c);
                pi.on::<SessionBeforeCompactEvent>(move |ev, _ctx| {
                    let s1 = Arc::clone(&s1);
                    let r1 = Arc::clone(&r1);
                    Box::pin(async move {
                        let wait = r1.notified();
                        s1.notify_one();
                        wait.await;
                        Ok(Some(SessionBeforeCompactResult {
                            cancel: None,
                            compaction: Some(CompactionResult {
                                summary: "summary".to_string(),
                                first_kept_entry_id: ev.preparation.first_kept_entry_id,
                                tokens_before: ev.preparation.tokens_before,
                                estimated_tokens_after: None,
                                details: None,
                                usage: None,
                            }),
                        }))
                    })
                });
                let s2 = Arc::clone(&s_c);
                let r2 = Arc::clone(&r_c);
                pi.on::<SessionBeforeTreeEvent>(move |_ev, _ctx| {
                    let s2 = Arc::clone(&s2);
                    let r2 = Arc::clone(&r2);
                    Box::pin(async move {
                        let wait = r2.notified();
                        s2.notify_one();
                        wait.await;
                        Ok(None)
                    })
                });
                Ok(())
            }
        }));

        let harness = create_harness(HarnessOptions {
            settings: Some(Settings {
                compaction: Some(CompactionSettings {
                    keep_recent_tokens: Some(1),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            extension_factories: Some(vec![ext]),
            ..Default::default()
        })
        .await;

        harness.session_manager.append_message(user_msg("first user")).unwrap();
        let target_id = harness
            .session_manager
            .append_message(assistant_msg("first assistant"))
            .unwrap();
        harness.session_manager.append_message(user_msg("second user")).unwrap();
        harness
            .session_manager
            .append_message(assistant_msg("second assistant"))
            .unwrap();
        let msgs = harness.session_manager.build_session_context().messages;
        harness.session.agent().replace_messages(msgs);

        let runner = harness.session.extension_runner().expect("runner");
        let started_wait = started.notified();
        let s_op = harness.session.clone();
        let pending = tokio::spawn(async move {
            if operation == "compaction" {
                s_op.compact(None).await.map(|_| ())
            } else {
                s_op.navigate_tree(&target_id, None).await.map(|_| ())
            }
        });
        started_wait.await;

        assert!(!harness.session.is_streaming());
        let err = harness.session.reload(None).await.expect_err("expected reload error");
        assert!(
            err.to_string()
                .contains("Wait for compaction to finish before reloading."),
            "operation={operation}, err={err}"
        );
        assert_eq!(shutdown_calls.load(Ordering::SeqCst), 0);
        assert!(runner.get_active_tools().is_ok());

        released.notify_one();
        pending.await.expect("join").expect("pending op");
        assert!(harness.session.is_idle());

        harness.cleanup();
    }
}

struct SpyResourceLoader {
    reload_calls: AtomicUsize,
    core_extensions: pi_coding_agent::core::extensions::types::LoadExtensionsResult,
}

impl ResourceLoader for SpyResourceLoader {
    fn get_extensions(&self) -> LoaderLoadExtensionsResult {
        LoaderLoadExtensionsResult::default()
    }
    fn get_core_extensions(&self) -> pi_coding_agent::core::extensions::types::LoadExtensionsResult {
        self.core_extensions.clone()
    }
    fn get_skills(&self) -> (Vec<Skill>, Vec<ResourceDiagnostic>) {
        (Vec::new(), Vec::new())
    }
    fn get_prompts(&self) -> (Vec<PromptTemplate>, Vec<ResourceDiagnostic>) {
        (Vec::new(), Vec::new())
    }
    fn get_themes(&self) -> (Vec<LoaderTheme>, Vec<ResourceDiagnostic>) {
        (Vec::new(), Vec::new())
    }
    fn get_agents_files(&self) -> Vec<ContextFile> {
        Vec::new()
    }
    fn get_system_prompt(&self) -> Option<String> {
        None
    }
    fn get_system_prompt_source(&self) -> Option<PromptSource> {
        None
    }
    fn get_append_system_prompt(&self) -> Vec<String> {
        Vec::new()
    }
    fn get_append_system_prompt_sources(&self) -> Vec<PromptSource> {
        Vec::new()
    }
    fn extend_resources(&self, _paths: ResourceExtensionPaths) {}
    fn reload(&self, _options: Option<ResourceLoaderReloadOptions>) -> BoxFuture<pi_coding_agent::Result<()>> {
        self.reload_calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }
}

#[tokio::test]
async fn still_reloads_and_replaces_the_runner_when_idle() {
    let shutdown_calls = Arc::new(AtomicUsize::new(0));
    let start_reasons = Arc::new(Mutex::new(Vec::<SessionStartReason>::new()));
    let sh_c = Arc::clone(&shutdown_calls);
    let sr_c = Arc::clone(&start_reasons);

    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let sh_c = Arc::clone(&sh_c);
        let sr_c = Arc::clone(&sr_c);
        async move {
            pi.on::<SessionShutdownEvent>(move |_ev, _ctx| {
                let sh_c = Arc::clone(&sh_c);
                Box::pin(async move {
                    sh_c.fetch_add(1, Ordering::SeqCst);
                    Ok(None)
                })
            });
            pi.on::<SessionStartEvent>(move |ev, _ctx| {
                let sr_c = Arc::clone(&sr_c);
                Box::pin(async move {
                    sr_c.lock().await.push(ev.reason);
                    Ok(None)
                })
            });
            Ok(())
        }
    }));

    let core_exts = create_suite_extensions_result(&[ext], ".").await;
    let loader = Arc::new(SpyResourceLoader {
        reload_calls: AtomicUsize::new(0),
        core_extensions: core_exts,
    });
    let harness = create_harness(HarnessOptions {
        resource_loader: Some(Arc::clone(&loader) as Arc<dyn ResourceLoader>),
        ..Default::default()
    })
    .await;

    harness
        .session
        .bind_extensions(ExtensionBindings {
            on_error: Some(Arc::new(|_| {})),
            ..Default::default()
        })
        .await
        .expect("bind");
    start_reasons.lock().await.clear();
    let old_runner = harness.session.extension_runner().expect("old runner");

    harness.session.reload(None).await.expect("reload");

    assert_eq!(shutdown_calls.load(Ordering::SeqCst), 1);
    assert_eq!(loader.reload_calls.load(Ordering::SeqCst), 1);
    assert_eq!(*start_reasons.lock().await, vec![SessionStartReason::Reload]);
    let err = old_runner.get_active_tools().expect_err("expected stale runner error");
    assert!(err.to_string().contains("stale"), "err: {err}");
    assert!(
        harness
            .session
            .extension_runner()
            .expect("new runner")
            .get_active_tools()
            .is_ok()
    );

    harness.cleanup();
}
