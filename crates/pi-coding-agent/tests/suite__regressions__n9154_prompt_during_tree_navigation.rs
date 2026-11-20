//! Port of packages/coding-agent/test/suite/regressions/9154-prompt-during-tree-navigation.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use pi_agent_core::types::AgentMessage;
use pi_ai::providers::faux::{FauxAssistantMessageContent, FauxResponseStep, faux_assistant_message};
use pi_ai::types::{Message, TextContent, UserContentBlock, UserMessage, UserMessageContent};
use pi_coding_agent::core::agent_session::{
    AgentSessionEvent, ExtensionBindings, NavigateTreeOptions, PromptOptions, UserMessageInput,
};
use pi_coding_agent::core::extensions::types::{
    AgentSettledEvent, BeforeAgentStartEvent, CancelledResult, ExtensionApi, ExtensionCommandContextActions,
    InlineExtension, InputEvent, InputEventResult, InputSource, RegisteredCommandOptions, SessionBeforeTreeEvent,
    SessionBeforeTreeResult, SessionEntry, SessionTreeEvent, TreeSummaryOverride,
};
use support::suite::harness::{Harness, HarnessOptions, create_harness, get_message_text, get_user_texts};
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

fn seed_history(harness: &Harness) -> String {
    harness
        .session_manager
        .append_message(user_msg("first prompt"))
        .unwrap();
    let target_id = harness
        .session_manager
        .append_message(assistant_msg("first response"))
        .unwrap();
    harness
        .session_manager
        .append_message(user_msg("second prompt"))
        .unwrap();
    harness
        .session_manager
        .append_message(assistant_msg("second response"))
        .unwrap();
    harness.session.refresh_context();
    target_id
}

#[tokio::test]
async fn rejects_a_prompt_while_branch_summarization_is_in_progress() {
    let summary_started = Arc::new(Notify::new());
    let summary_released = Arc::new(Notify::new());
    let ss_c = Arc::clone(&summary_started);
    let sr_c = Arc::clone(&summary_released);

    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let ss = Arc::clone(&ss_c);
        let sr = Arc::clone(&sr_c);
        async move {
            pi.on::<SessionBeforeTreeEvent>(move |_ev, _ctx| {
                let ss = Arc::clone(&ss);
                let sr = Arc::clone(&sr);
                Box::pin(async move {
                    let wait = sr.notified();
                    ss.notify_one();
                    wait.await;
                    Ok(Some(SessionBeforeTreeResult {
                        cancel: None,
                        summary: Some(TreeSummaryOverride {
                            summary: "abandoned branch summary".to_string(),
                            details: None,
                            usage: None,
                        }),
                        custom_instructions: None,
                        replace_instructions: None,
                        label: None,
                    }))
                })
            });
            Ok(())
        }
    }));

    let harness = create_harness(HarnessOptions {
        extension_factories: Some(vec![ext]),
        ..Default::default()
    })
    .await;

    let target_id = seed_history(&harness);
    harness.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text("unexpected response".to_string()),
        None,
    ))]);

    let started_wait = summary_started.notified();
    let s_nav = harness.session.clone();
    let nav_task = tokio::spawn(async move {
        s_nav
            .navigate_tree(
                &target_id,
                Some(NavigateTreeOptions {
                    summarize: Some(true),
                    ..Default::default()
                }),
            )
            .await
    });
    started_wait.await;

    let preflight_calls = Arc::new(AtomicUsize::new(0));
    let pc_c = Arc::clone(&preflight_calls);
    let prompt_res = harness
        .session
        .prompt(
            "prompt during summary",
            Some(PromptOptions {
                source: Some(InputSource::Rpc),
                preflight_result: Some(Arc::new(move |_| {
                    pc_c.fetch_add(1, Ordering::SeqCst);
                })),
                ..Default::default()
            }),
        )
        .await;

    summary_released.notify_one();
    let _ = nav_task.await;

    let err = prompt_res.expect_err("expected compaction error");
    assert!(err.to_string().contains("compaction is in progress"), "err: {err}");
    assert_eq!(preflight_calls.load(Ordering::SeqCst), 0);

    let persisted_user_texts: Vec<String> = harness
        .session_manager
        .get_entries()
        .into_iter()
        .filter_map(|e| match e {
            SessionEntry::Message(m) if m.message.role() == Some("user") => Some(get_message_text(&m.message)),
            _ => None,
        })
        .collect();
    assert!(!get_user_texts(&harness).contains(&"prompt during summary".to_string()));
    assert!(!persisted_user_texts.contains(&"prompt during summary".to_string()));
    assert_eq!(
        harness
            .events()
            .iter()
            .filter(|e| matches!(e, AgentSessionEvent::AgentStart))
            .count(),
        0
    );
    assert_eq!(
        harness
            .events()
            .iter()
            .filter(|e| matches!(e, AgentSessionEvent::AgentSettled { .. }))
            .count(),
        0
    );

    harness.cleanup();
}

#[tokio::test]
async fn rejects_navigation_during_an_awaited_input_or_before_agent_start_hook() {
    for hook in ["input", "before_agent_start"] {
        let prompt_started = Arc::new(Notify::new());
        let prompt_released = Arc::new(Notify::new());
        let ps_c = Arc::clone(&prompt_started);
        let pr_c = Arc::clone(&prompt_released);
        let before_tree_hits = Arc::new(AtomicUsize::new(0));
        let bt_c = Arc::clone(&before_tree_hits);

        let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
            let ps = Arc::clone(&ps_c);
            let pr = Arc::clone(&pr_c);
            let bt = Arc::clone(&bt_c);
            async move {
                if hook == "input" {
                    pi.on::<InputEvent>(move |_ev, _ctx| {
                        let ps = Arc::clone(&ps);
                        let pr = Arc::clone(&pr);
                        Box::pin(async move {
                            let wait = pr.notified();
                            ps.notify_one();
                            wait.await;
                            Ok(None)
                        })
                    });
                } else {
                    pi.on::<BeforeAgentStartEvent>(move |_ev, _ctx| {
                        let ps = Arc::clone(&ps);
                        let pr = Arc::clone(&pr);
                        Box::pin(async move {
                            let wait = pr.notified();
                            ps.notify_one();
                            wait.await;
                            Ok(None)
                        })
                    });
                }
                pi.on::<SessionBeforeTreeEvent>(move |_ev, _ctx| {
                    let bt = Arc::clone(&bt);
                    Box::pin(async move {
                        bt.fetch_add(1, Ordering::SeqCst);
                        Ok(None)
                    })
                });
                Ok(())
            }
        }));

        let harness = create_harness(HarnessOptions {
            extension_factories: Some(vec![ext]),
            ..Default::default()
        })
        .await;
        let target_id = seed_history(&harness);
        harness.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("response to racing prompt".to_string()),
            None,
        ))]);

        let started_wait = prompt_started.notified();
        let s_p = harness.session.clone();
        let prompt_task = tokio::spawn(async move {
            s_p.prompt(
                "racing prompt",
                Some(PromptOptions {
                    source: Some(InputSource::Rpc),
                    ..Default::default()
                }),
            )
            .await
        });
        started_wait.await;

        let nav_res = harness.session.navigate_tree(&target_id, None).await;
        prompt_released.notify_one();
        prompt_task.await.expect("join").expect("prompt");

        let err = nav_res.expect_err("expected navigation blocked");
        assert_eq!(
            err.to_string(),
            "Wait for the current response to finish before navigating the session tree."
        );
        assert_eq!(before_tree_hits.load(Ordering::SeqCst), 0);
        assert!(get_user_texts(&harness).contains(&"racing prompt".to_string()));
        assert!(
            harness
                .session
                .messages()
                .iter()
                .any(|m| get_message_text(m) == "response to racing prompt")
        );
        let ok_nav = harness.session.navigate_tree(&target_id, None).await.expect("ok nav");
        assert!(!ok_nav.cancelled);

        harness.cleanup();
    }
}

#[tokio::test]
async fn allows_navigation_after_a_prompt_is_handled_or_rejected_during_preflight() {
    for outcome in ["handled", "rejected"] {
        let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| async move {
            if outcome == "handled" {
                pi.on::<InputEvent>(|_ev, _ctx| Box::pin(async { Ok(Some(InputEventResult::Handled)) }));
            }
            Ok(())
        }));
        let harness = create_harness(HarnessOptions {
            with_configured_auth: Some(outcome != "rejected"),
            extension_factories: Some(vec![ext]),
            ..Default::default()
        })
        .await;
        let target_id = seed_history(&harness);
        let prompt_res = harness.session.prompt("not sent to the model", None).await;
        if outcome == "handled" {
            prompt_res.expect("handled prompt");
        } else {
            let err = prompt_res.expect_err("expected missing auth error");
            assert!(err.to_string().contains("No API key"), "err: {err}");
        }
        let nav = harness.session.navigate_tree(&target_id, None).await.expect("nav");
        assert!(!nav.cancelled);
        harness.cleanup();
    }
}

#[tokio::test]
async fn keeps_navigation_blocked_when_another_prompt_finishes_preflight_first() {
    let started = Arc::new(Notify::new());
    let released = Arc::new(Notify::new());
    let s_c = Arc::clone(&started);
    let r_c = Arc::clone(&released);

    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let s_c = Arc::clone(&s_c);
        let r_c = Arc::clone(&r_c);
        async move {
            pi.on::<InputEvent>(move |ev, _ctx| {
                let s_c = Arc::clone(&s_c);
                let r_c = Arc::clone(&r_c);
                Box::pin(async move {
                    if ev.text == "waiting prompt" {
                        let wait = r_c.notified();
                        s_c.notify_one();
                        wait.await;
                    }
                    Ok(Some(InputEventResult::Handled))
                })
            });
            Ok(())
        }
    }));
    let harness = create_harness(HarnessOptions {
        extension_factories: Some(vec![ext]),
        ..Default::default()
    })
    .await;
    let target_id = seed_history(&harness);

    let started_wait = started.notified();
    let s = harness.session.clone();
    let waiting = tokio::spawn(async move { s.prompt("waiting prompt", None).await });
    started_wait.await;

    harness
        .session
        .prompt("handled immediately", None)
        .await
        .expect("immediate handled");
    let err = harness
        .session
        .navigate_tree(&target_id, None)
        .await
        .expect_err("expected nav blocked");
    assert_eq!(
        err.to_string(),
        "Wait for the current response to finish before navigating the session tree."
    );

    released.notify_one();
    waiting.await.expect("join").expect("waiting ok");
    let nav = harness.session.navigate_tree(&target_id, None).await.expect("nav");
    assert!(!nav.cancelled);

    harness.cleanup();
}

#[tokio::test]
async fn allows_a_prompt_from_session_tree_after_the_selected_context_is_installed() {
    let errors = Arc::new(Mutex::new(Vec::<String>::new()));
    let errs_c = Arc::clone(&errors);
    let session_slot: Arc<std::sync::Mutex<Option<pi_coding_agent::core::agent_session::AgentSession>>> =
        Arc::new(std::sync::Mutex::new(None));
    let slot_c = Arc::clone(&session_slot);

    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let slot_c = Arc::clone(&slot_c);
        async move {
            pi.on::<SessionTreeEvent>(move |_ev, _ctx| {
                let sess = slot_c.lock().unwrap().clone();
                Box::pin(async move {
                    if let Some(s) = sess {
                        s.send_user_message(
                            UserMessageInput::Text("continue on the selected branch".to_string()),
                            None,
                        )
                        .await?;
                    }
                    Ok(None)
                })
            });
            Ok(())
        }
    }));

    let harness = create_harness(HarnessOptions {
        extension_factories: Some(vec![ext]),
        ..Default::default()
    })
    .await;
    *session_slot.lock().unwrap() = Some(harness.session.clone());

    harness
        .session
        .bind_extensions(ExtensionBindings {
            on_error: Some(Arc::new(move |err| {
                let errs_c = Arc::clone(&errs_c);
                tokio::spawn(async move {
                    errs_c.lock().await.push(err.error);
                });
            })),
            ..Default::default()
        })
        .await
        .expect("bind");

    let target_id = seed_history(&harness);
    harness.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text("continued".to_string()),
        None,
    ))]);
    harness.session.navigate_tree(&target_id, None).await.expect("nav");

    assert!(errors.lock().await.is_empty());
    assert_eq!(
        get_user_texts(&harness),
        vec![
            "first prompt".to_string(),
            "continue on the selected branch".to_string()
        ]
    );
    assert_eq!(harness.session.get_last_assistant_text().as_deref(), Some("continued"));

    harness.cleanup();
}

#[tokio::test]
async fn does_not_clear_a_new_navigation_started_from_session_tree() {
    let started = Arc::new(Notify::new());
    let released = Arc::new(Notify::new());
    let next_target_id = Arc::new(std::sync::Mutex::new(String::new()));
    let continuation = Arc::new(std::sync::Mutex::new(
        None::<tokio::task::JoinHandle<pi_js::Result<()>>>,
    ));
    let session_slot: Arc<std::sync::Mutex<Option<pi_coding_agent::core::agent_session::AgentSession>>> =
        Arc::new(std::sync::Mutex::new(None));

    let s_c = Arc::clone(&started);
    let r_c = Arc::clone(&released);
    let nt_c = Arc::clone(&next_target_id);
    let cont_c = Arc::clone(&continuation);
    let slot_c = Arc::clone(&session_slot);

    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let s_c = Arc::clone(&s_c);
        let r_c = Arc::clone(&r_c);
        let nt_c = Arc::clone(&nt_c);
        let cont_c = Arc::clone(&cont_c);
        let slot_c = Arc::clone(&slot_c);
        async move {
            let nt_before = Arc::clone(&nt_c);
            let nt_tree = Arc::clone(&nt_c);
            pi.on::<SessionBeforeTreeEvent>(move |ev, _ctx| {
                let s_c = Arc::clone(&s_c);
                let r_c = Arc::clone(&r_c);
                let nt_c = Arc::clone(&nt_before);
                Box::pin(async move {
                    let target = nt_c.lock().unwrap().clone();
                    if ev.preparation.target_id == target {
                        let wait = r_c.notified();
                        s_c.notify_one();
                        wait.await;
                    }
                    Ok(None)
                })
            });
            pi.on::<SessionTreeEvent>(move |_ev, _ctx| {
                let nt_c = Arc::clone(&nt_tree);
                let cont_c = Arc::clone(&cont_c);
                let slot_c = Arc::clone(&slot_c);
                Box::pin(async move {
                    let mut guard = cont_c.lock().unwrap();
                    if guard.is_none()
                        && let Some(sess) = slot_c.lock().unwrap().clone()
                    {
                        let target = nt_c.lock().unwrap().clone();
                        *guard = Some(tokio::spawn(async move {
                            sess.navigate_tree(&target, None).await.map(|_| ())
                        }));
                    }
                    Ok(None)
                })
            });
            Ok(())
        }
    }));

    let harness = create_harness(HarnessOptions {
        extension_factories: Some(vec![ext]),
        ..Default::default()
    })
    .await;
    *session_slot.lock().unwrap() = Some(harness.session.clone());
    let target_id = seed_history(&harness);
    let leaf = harness.session_manager.get_leaf_id().expect("leaf");
    *next_target_id.lock().unwrap() = leaf.clone();

    harness.session.navigate_tree(&target_id, None).await.expect("nav1");
    let handle = continuation.lock().unwrap().take().expect("continuation handle");
    tokio::select! {
        _ = started.notified() => {}
        _ = tokio::time::sleep(std::time::Duration::from_millis(200)) => {}
    }
    assert!(harness.session.is_compacting());
    let err = harness
        .session
        .prompt("must wait for the second navigation", None)
        .await
        .expect_err("expected prompt rejected");
    assert!(err.to_string().contains("compaction is in progress"), "err: {err}");

    released.notify_one();
    handle.await.expect("join").expect("second nav ok");
    assert_eq!(harness.session_manager.get_leaf_id().as_deref(), Some(leaf.as_str()));
    assert!(!harness.session.is_compacting());

    harness.cleanup();
}

#[tokio::test]
async fn allows_navigation_from_an_extension_command_and_agent_settled() {
    for event_kind in ["command", "agent_settled"] {
        let target_slot = Arc::new(std::sync::Mutex::new(String::new()));
        let navigated = Arc::new(AtomicBool::new(false));
        let session_slot: Arc<std::sync::Mutex<Option<pi_coding_agent::core::agent_session::AgentSession>>> =
            Arc::new(std::sync::Mutex::new(None));

        let ts_c = Arc::clone(&target_slot);
        let nav_c = Arc::clone(&navigated);
        let ss_c = Arc::clone(&session_slot);

        let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
            let ts_c = Arc::clone(&ts_c);
            let nav_c = Arc::clone(&nav_c);
            let ss_c = Arc::clone(&ss_c);
            async move {
                if event_kind == "command" {
                    pi.register_command(
                        "rewind",
                        RegisteredCommandOptions {
                            description: None,
                            get_argument_completions: None,
                            handler: Arc::new(move |_args, _ctx| {
                                let t = ts_c.lock().unwrap().clone();
                                let s = ss_c.lock().unwrap().clone();
                                let n = Arc::clone(&nav_c);
                                Box::pin(async move {
                                    if let Some(sess) = s {
                                        sess.navigate_tree(&t, None).await?;
                                        n.store(true, Ordering::SeqCst);
                                    }
                                    Ok(())
                                })
                            }),
                        },
                    )?;
                } else {
                    pi.on::<AgentSettledEvent>(move |_ev, _ctx| {
                        let t = ts_c.lock().unwrap().clone();
                        let s = ss_c.lock().unwrap().clone();
                        let n = Arc::clone(&nav_c);
                        Box::pin(async move {
                            if let Some(sess) = s {
                                sess.navigate_tree(&t, None).await?;
                                n.store(true, Ordering::SeqCst);
                            }
                            Ok(None)
                        })
                    });
                }
                Ok(())
            }
        }));

        let harness = create_harness(HarnessOptions {
            extension_factories: Some(vec![ext]),
            ..Default::default()
        })
        .await;
        *session_slot.lock().unwrap() = Some(harness.session.clone());

        let s_idle = harness.session.clone();
        let s_nav = harness.session.clone();
        harness
            .session
            .bind_extensions(ExtensionBindings {
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
                    reload: Arc::new(|| Box::pin(async { Ok(()) })),
                }),
                ..Default::default()
            })
            .await
            .expect("bind");

        let target_id = seed_history(&harness);
        *target_slot.lock().unwrap() = target_id.clone();
        harness.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("done".to_string()),
            None,
        ))]);

        harness
            .session
            .prompt(
                if event_kind == "command" {
                    "/rewind"
                } else {
                    "finish then rewind"
                },
                None,
            )
            .await
            .expect("prompt");
        assert!(navigated.load(Ordering::SeqCst), "event_kind={event_kind}");
        assert_eq!(
            harness.session_manager.get_leaf_id().as_deref(),
            Some(target_id.as_str())
        );

        harness.cleanup();
    }
}
