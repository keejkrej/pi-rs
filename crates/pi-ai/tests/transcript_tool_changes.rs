//! Port of packages/ai/test/transcript-tool-changes.test.ts (unit ai-providers-builtin)

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use pi_ai::compat::{get_model, stream_simple};
use pi_ai::types::{
    AnthropicMessagesCompat, AssistantContentBlock, AssistantMessage, Context, Message, Model, ModelCompat, ModelCost,
    ModelInput, OpenAIResponsesCompat, SimpleStreamOptions, StopReason, StreamOptions, SystemMessage,
    SystemMessageContent, TextContent, Tool, ToolReference, Usage, UserMessage, UserMessageContent,
};
use pi_ai::utils::transcript::{TranscriptToolSupport, TranscriptTools, resolve_transcript_tools_with_support};
use pi_js::vendor::typebox::t;
use serde_json::{Value, json};

fn tool(name: &str) -> Tool {
    Tool {
        name: name.to_string(),
        description: format!("{name} tool"),
        parameters: t::object(std::iter::empty::<(&str, pi_js::vendor::typebox::Schema)>()),
        constrained_sampling: None,
    }
}

fn sections(pairs: &[(&str, Option<&str>)]) -> IndexMap<String, Option<String>> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), v.map(str::to_string)))
        .collect()
}

fn base_model(id: &str, name: &str, api: &str, provider: &str, compat: Option<ModelCompat>) -> Model {
    Model {
        r#type: None,
        id: id.to_string(),
        name: name.to_string(),
        api: api.to_string(),
        provider: provider.into(),
        base_url: "http://127.0.0.1:9".to_string(),
        reasoning: true,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: ModelCost::default(),
        context_window: 100000,
        max_tokens: 1000,
        headers: None,
        sampling_params: None,
        compat,
        sampling_params_by_thinking_level: None,
    }
}

async fn capture_payload(model: &Model, context: &Context) -> Value {
    let slot: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
    let cap = Arc::clone(&slot);
    stream_simple(
        model,
        context,
        Some(SimpleStreamOptions {
            base: StreamOptions {
                api_key: Some("test-key".to_string()),
                on_payload: Some(Arc::new(move |payload, _m| {
                    let cap = Arc::clone(&cap);
                    Box::pin(async move {
                        *cap.lock().unwrap() = Some(payload);
                        Err(pi_js::Error::msg("payload captured"))
                    })
                })),
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .result()
    .await;
    slot.lock().unwrap().clone().expect("Expected payload capture")
}

async fn capture_tool_history(model: &Model) -> Vec<Value> {
    let base_tool = tool("base_tool");
    let drop_tool = tool("drop_tool");
    let late_tool = tool("late_tool");
    let steps = vec![
        vec![Message::User(UserMessage {
            content: UserMessageContent::Text("first".to_string()),
            timestamp: 1,
        })],
        vec![
            Message::System(SystemMessage {
                content: SystemMessageContent::Text(String::new()),
                sections: None,
                tools_removed: Some(vec![ToolReference {
                    name: "drop_tool".to_string(),
                }]),
                tools_added: None,
                timestamp: 2,
            }),
            Message::User(UserMessage {
                content: UserMessageContent::Text("after removal".to_string()),
                timestamp: 3,
            }),
        ],
        vec![
            Message::System(SystemMessage {
                content: SystemMessageContent::Text(String::new()),
                sections: None,
                tools_added: Some(vec![late_tool]),
                tools_removed: None,
                timestamp: 4,
            }),
            Message::User(UserMessage {
                content: UserMessageContent::Text("after first load".to_string()),
                timestamp: 5,
            }),
        ],
        vec![
            Message::System(SystemMessage {
                content: SystemMessageContent::Text(String::new()),
                sections: None,
                tools_added: Some(vec![tool("later_tool")]),
                tools_removed: None,
                timestamp: 6,
            }),
            Message::User(UserMessage {
                content: UserMessageContent::Text("after second load".to_string()),
                timestamp: 7,
            }),
        ],
    ];
    let mut messages = vec![Message::System(SystemMessage {
        content: SystemMessageContent::Text("base prompt".to_string()),
        sections: None,
        tools_added: Some(vec![base_tool, drop_tool]),
        tools_removed: None,
        timestamp: 0,
    })];
    let mut payloads = Vec::new();
    for step in steps {
        messages.extend(step);
        payloads.push(
            capture_payload(
                model,
                &Context {
                    system_prompt: None,
                    tools: None,
                    messages: messages.clone(),
                },
            )
            .await,
        );
    }
    payloads
}

fn expect_growing_prefix(inputs: &[&Vec<Value>]) {
    for idx in 1..inputs.len() {
        let prev = inputs[idx - 1];
        let curr = inputs[idx];
        assert!(curr.len() >= prev.len());
        assert_eq!(&curr[..prev.len()], prev.as_slice());
    }
}

fn sample_context() -> Context {
    let base_tool = tool("base_tool");
    let late_tool = tool("late_tool");
    Context {
        system_prompt: None,
        tools: None,
        messages: vec![
            Message::System(SystemMessage {
                content: SystemMessageContent::Text("base prompt".to_string()),
                sections: Some(sections(&[
                    ("rules", Some("<rules>\nold rules\n</rules>")),
                    ("docs", Some("<docs>\nread docs\n</docs>")),
                ])),
                tools_added: Some(vec![base_tool]),
                tools_removed: None,
                timestamp: 0,
            }),
            Message::User(UserMessage {
                content: UserMessageContent::Text("before".to_string()),
                timestamp: 1,
            }),
            Message::System(SystemMessage {
                content: SystemMessageContent::Text("updated guidance".to_string()),
                sections: Some(sections(&[
                    ("rules", Some("<rules>\nnew rules\n</rules>")),
                    ("docs", None),
                ])),
                tools_removed: Some(vec![ToolReference {
                    name: "base_tool".to_string(),
                }]),
                tools_added: Some(vec![late_tool]),
                timestamp: 2,
            }),
        ],
    }
}

fn addition_context() -> Context {
    let base_tool = tool("base_tool");
    let late_tool = tool("late_tool");
    Context {
        system_prompt: None,
        tools: None,
        messages: vec![
            Message::System(SystemMessage {
                content: SystemMessageContent::Text("base prompt".to_string()),
                sections: None,
                tools_added: Some(vec![base_tool]),
                tools_removed: None,
                timestamp: 0,
            }),
            Message::User(UserMessage {
                content: UserMessageContent::Text("before".to_string()),
                timestamp: 1,
            }),
            Message::System(SystemMessage {
                content: SystemMessageContent::Text("updated guidance".to_string()),
                sections: None,
                tools_added: Some(vec![late_tool]),
                tools_removed: None,
                timestamp: 2,
            }),
        ],
    }
}

fn make_reply(text: &str, provider: &str, api: &str, model: &str) -> AssistantMessage {
    AssistantMessage {
        content: vec![AssistantContentBlock::Text(TextContent::new(text))],
        api: api.to_string(),
        provider: provider.into(),
        model: model.to_string(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason: StopReason::Stop,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 3,
        duration_ms: None,
    }
}

#[tokio::test]
async fn sends_anthropic_updates_and_inline_tool_changes_with_fallbacks() {
    let ctx = sample_context();
    let native_model = base_model(
        "claude-opus-5",
        "Claude Opus 5",
        "anthropic-messages",
        "anthropic",
        Some(ModelCompat::AnthropicMessages(AnthropicMessagesCompat {
            supports_mid_convo_system_messages: Some(true),
            supports_mid_convo_tool_changes: Some(true),
            ..Default::default()
        })),
    );

    let payload = capture_payload(&native_model, &ctx).await;
    let betas = payload["betas"].as_array().unwrap();
    assert!(betas.contains(&json!("inline-tools-2026-09-15")));
    assert!(!betas.contains(&json!("mid-conversation-tool-changes-2026-07-01")));
    assert_eq!(
        payload["system"].as_array().unwrap()[0]["text"],
        "base prompt\n\n<rules>\nold rules\n</rules>\n\n<docs>\nread docs\n</docs>"
    );
    let tools = payload["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 2);
    assert_eq!(tools[0]["name"], "base_tool");
    assert_eq!(tools[0]["cache_control"], json!({ "type": "ephemeral" }));
    assert!(tools[0].get("defer_loading").is_none());
    assert_eq!(tools[1]["name"], "__pi_deferred_placeholder__");
    assert_eq!(tools[1]["defer_loading"], true);
    assert!(tools[1].get("cache_control").is_none());

    let update = payload["messages"].as_array().unwrap().last().unwrap();
    assert_eq!(update["role"], "system");
    let ucontent = update["content"].as_array().unwrap();
    assert!(ucontent[0]["text"].as_str().unwrap().contains("updated guidance"));
    assert!(
        ucontent[0]["text"]
            .as_str()
            .unwrap()
            .contains("<rules>\nnew rules\n</rules>")
    );
    assert!(
        ucontent[0]["text"]
            .as_str()
            .unwrap()
            .contains("Removed system prompt section \"docs\"")
    );
    assert_eq!(ucontent[1]["type"], "tool_removal");
    assert_eq!(
        ucontent[1]["tool"],
        json!({ "type": "tool_reference", "name": "base_tool" })
    );
    assert_eq!(ucontent[2]["type"], "tool_addition");
    assert_eq!(ucontent[2]["tool"]["type"], "tool_definition");
    assert_eq!(ucontent[2]["tool"]["definition"]["name"], "late_tool");
    assert_eq!(ucontent[2]["tool"]["definition"]["description"], "late_tool tool");
    assert_eq!(ucontent[2]["cache_control"], json!({ "type": "ephemeral" }));
    let def = &ucontent[2]["tool"]["definition"];
    assert!(def.get("cache_control").is_none());
    assert!(def.get("defer_loading").is_none());

    let initial = capture_payload(
        &native_model,
        &Context {
            system_prompt: None,
            tools: None,
            messages: ctx.messages[..2].to_vec(),
        },
    )
    .await;
    let init_names: Vec<_> = initial["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(init_names, vec!["base_tool", "__pi_deferred_placeholder__"]);

    // Redefines an Anthropic tool by value under the same name
    let mut redefined = tool("base_tool");
    redefined.description = "changed".to_string();
    let redef_payload = capture_payload(
        &native_model,
        &Context {
            system_prompt: None,
            tools: None,
            messages: vec![
                Message::System(SystemMessage {
                    content: SystemMessageContent::Text("base prompt".to_string()),
                    sections: None,
                    tools_added: Some(vec![tool("base_tool")]),
                    tools_removed: None,
                    timestamp: 0,
                }),
                Message::User(UserMessage {
                    content: UserMessageContent::Text("before".to_string()),
                    timestamp: 1,
                }),
                Message::System(SystemMessage {
                    content: SystemMessageContent::Text(String::new()),
                    sections: None,
                    tools_removed: Some(vec![ToolReference {
                        name: "base_tool".to_string(),
                    }]),
                    tools_added: Some(vec![redefined]),
                    timestamp: 2,
                }),
            ],
        },
    )
    .await;
    assert!(
        redef_payload["betas"]
            .as_array()
            .unwrap()
            .contains(&json!("inline-tools-2026-09-15"))
    );
    assert_eq!(redef_payload["tools"][0]["description"], "base_tool tool");
    assert_eq!(redef_payload["tools"][1]["name"], "__pi_deferred_placeholder__");
    let redef_last = redef_payload["messages"].as_array().unwrap().last().unwrap();
    assert_eq!(redef_last["content"].as_array().unwrap().len(), 1);
    assert_eq!(redef_last["content"][0]["type"], "tool_addition");
    assert_eq!(redef_last["content"][0]["tool"]["definition"]["name"], "base_tool");
    assert_eq!(redef_last["content"][0]["tool"]["definition"]["description"], "changed");

    // Sends current tool list without an initial tool
    let no_init_payload = capture_payload(
        &native_model,
        &Context {
            system_prompt: None,
            tools: None,
            messages: vec![
                Message::System(SystemMessage {
                    content: SystemMessageContent::Text("base prompt".to_string()),
                    sections: None,
                    tools_added: None,
                    tools_removed: None,
                    timestamp: 0,
                }),
                Message::User(UserMessage {
                    content: UserMessageContent::Text("before".to_string()),
                    timestamp: 1,
                }),
                Message::System(SystemMessage {
                    content: SystemMessageContent::Text("updated guidance".to_string()),
                    sections: None,
                    tools_added: Some(vec![tool("late_tool")]),
                    tools_removed: None,
                    timestamp: 2,
                }),
            ],
        },
    )
    .await;
    if let Some(b) = no_init_payload.get("betas").and_then(Value::as_array) {
        assert!(!b.contains(&json!("inline-tools-2026-09-15")));
    }
    assert_eq!(no_init_payload["tools"][0]["name"], "late_tool");
    assert_eq!(
        no_init_payload["tools"][0]["cache_control"],
        json!({ "type": "ephemeral" })
    );
    assert!(no_init_payload["tools"][0].get("defer_loading").is_none());

    // #11091: holds update past an assistant message that converts to no blocks
    let hold_msgs = vec![
        Message::System(SystemMessage {
            content: SystemMessageContent::Text("base prompt".to_string()),
            sections: None,
            tools_added: None,
            tools_removed: None,
            timestamp: 0,
        }),
        Message::User(UserMessage {
            content: UserMessageContent::Text("hi".to_string()),
            timestamp: 1,
        }),
        Message::System(SystemMessage {
            content: SystemMessageContent::Text("updated guidance".to_string()),
            sections: None,
            tools_added: None,
            tools_removed: None,
            timestamp: 2,
        }),
        Message::Assistant(make_reply("", "openai-codex", "openai-codex-responses", "gpt-x")),
        Message::User(UserMessage {
            content: UserMessageContent::Text("again".to_string()),
            timestamp: 4,
        }),
    ];
    let at_end = capture_payload(
        &native_model,
        &Context {
            system_prompt: None,
            tools: None,
            messages: hold_msgs.clone(),
        },
    )
    .await;
    let at_end_roles: Vec<_> = at_end["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["role"].as_str().unwrap())
        .collect();
    assert_eq!(at_end_roles, vec!["user", "user", "system"]);

    let mut before_asst_msgs = hold_msgs;
    before_asst_msgs.push(Message::Assistant(make_reply(
        "answer",
        &native_model.provider,
        &native_model.api,
        &native_model.id,
    )));
    before_asst_msgs.push(Message::User(UserMessage {
        content: UserMessageContent::Text("third".to_string()),
        timestamp: 5,
    }));
    let before_asst = capture_payload(
        &native_model,
        &Context {
            system_prompt: None,
            tools: None,
            messages: before_asst_msgs,
        },
    )
    .await;
    let before_asst_roles: Vec<_> = before_asst["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["role"].as_str().unwrap())
        .collect();
    assert_eq!(before_asst_roles, vec!["user", "user", "system", "assistant", "user"]);

    // Without native support -> folded into system prompt
    let no_support = base_model(
        "claude-sonnet-4-5",
        "Claude Sonnet 4.5",
        "anthropic-messages",
        "anthropic",
        None,
    );
    let folded = capture_payload(&no_support, &ctx).await;
    if let Some(b) = folded.get("betas").and_then(Value::as_array) {
        assert!(!b.contains(&json!("inline-tools-2026-09-15")));
    }
    assert_eq!(
        folded["system"][0]["text"],
        "base prompt\n\nupdated guidance\n\n<rules>\nnew rules\n</rules>"
    );

    // Requires BOTH capabilities
    let partial_compat = base_model(
        "claude-opus-5",
        "Claude Opus 5",
        "anthropic-messages",
        "anthropic",
        Some(ModelCompat::AnthropicMessages(AnthropicMessagesCompat {
            supports_mid_convo_tool_changes: Some(true),
            ..Default::default()
        })),
    );
    let pc = capture_payload(&partial_compat, &ctx).await;
    assert_eq!(
        pc["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["late_tool"]
    );
}

#[tokio::test]
async fn sends_openai_and_kimi_mid_convo_tool_changes_and_preserves_prefix() {
    let add_ctx = addition_context();
    let full_ctx = sample_context();

    let additional_tools_model = base_model(
        "gpt-5.4",
        "GPT-5.4",
        "openai-responses",
        "openai",
        Some(ModelCompat::OpenAIResponses(OpenAIResponsesCompat {
            supports_mid_convo_system_messages: Some(true),
            supports_additional_tools: Some(true),
            ..Default::default()
        })),
    );

    // Declares every OpenAI tool in additional_tools at its developer message
    let p_add = capture_payload(&additional_tools_model, &add_ctx).await;
    assert!(p_add.get("tools").is_none());
    let add_names: Vec<_> = p_add["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i.get("type").and_then(Value::as_str) == Some("additional_tools"))
        .map(|i| i["tools"][0]["name"].as_str().unwrap())
        .collect();
    assert_eq!(add_names, vec!["base_tool", "late_tool"]);
    let input_arr = p_add["input"].as_array().unwrap();
    assert_eq!(input_arr[0]["type"], "additional_tools");
    assert_eq!(input_arr[1]["content"], "base prompt");
    assert!(
        input_arr[3]["content"]
            .as_str()
            .unwrap()
            .contains("incremental tools update")
    );
    assert_eq!(input_arr[4]["type"], "additional_tools");
    assert_eq!(input_arr[5]["content"], "updated guidance");

    // Announces OpenAI removals as text
    let p_rem = capture_payload(&additional_tools_model, &full_ctx).await;
    assert!(p_rem.get("tools").is_none());
    let rem_last = p_rem["input"].as_array().unwrap().last().unwrap()["content"]
        .as_str()
        .unwrap();
    assert!(rem_last.contains("updated guidance"));
    assert!(rem_last.contains("The following tools are no longer available. Do not call them:\n- base_tool"));

    // Redefines an OpenAI tool with additional_tools instead of announcing removal
    let p_redef = capture_payload(
        &additional_tools_model,
        &Context {
            system_prompt: None,
            tools: None,
            messages: vec![
                Message::System(SystemMessage {
                    content: SystemMessageContent::Text("base prompt".to_string()),
                    sections: None,
                    tools_added: Some(vec![tool("base_tool")]),
                    tools_removed: None,
                    timestamp: 0,
                }),
                Message::User(UserMessage {
                    content: UserMessageContent::Text("before".to_string()),
                    timestamp: 1,
                }),
                Message::System(SystemMessage {
                    content: SystemMessageContent::Text(String::new()),
                    sections: None,
                    tools_removed: Some(vec![ToolReference {
                        name: "base_tool".to_string(),
                    }]),
                    tools_added: Some(vec![Tool {
                        description: "changed".to_string(),
                        ..tool("base_tool")
                    }]),
                    timestamp: 2,
                }),
            ],
        },
    )
    .await;
    let redef_inp = p_redef["input"].as_array().unwrap();
    assert_eq!(redef_inp.len(), 5);
    assert!(
        redef_inp[3]["content"]
            .as_str()
            .unwrap()
            .contains("latest definition replaces the earlier one")
    );
    assert_eq!(redef_inp[4]["tools"][0]["description"], "changed");

    // #10940: Keeps OpenAI request prefix across removal and later loads
    let hist_payloads = capture_tool_history(&additional_tools_model).await;
    for p in &hist_payloads {
        assert!(p.get("tools").is_none());
    }
    let hist_inputs: Vec<&Vec<Value>> = hist_payloads.iter().map(|p| p["input"].as_array().unwrap()).collect();
    expect_growing_prefix(&hist_inputs);
    let last_hist = hist_payloads.last().unwrap()["input"].as_array().unwrap();
    let hist_add_groups: Vec<Vec<&str>> = last_hist
        .iter()
        .filter(|i| i.get("type").and_then(Value::as_str) == Some("additional_tools"))
        .map(|i| {
            i["tools"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t["name"].as_str().unwrap())
                .collect()
        })
        .collect();
    assert_eq!(
        hist_add_groups,
        vec![vec!["base_tool", "drop_tool"], vec!["late_tool"], vec!["later_tool"]]
    );
    let notices: Vec<&str> = last_hist
        .iter()
        .filter_map(|i| i.get("content").and_then(Value::as_str))
        .filter(|s| s.contains("incremental tools update"))
        .collect();
    assert_eq!(notices.len(), 2);
    assert!(notices.iter().all(|s| s.contains("\nStill unavailable: drop_tool.")));

    // Tool search prefix across loads after removal
    let search_model = base_model(
        "gpt-5.4",
        "GPT-5.4",
        "openai-responses",
        "openai",
        Some(ModelCompat::OpenAIResponses(OpenAIResponsesCompat {
            supports_mid_convo_system_messages: Some(true),
            supports_tool_search: Some(true),
            ..Default::default()
        })),
    );
    let search_hist = capture_tool_history(&search_model).await;
    let before_rem = &search_hist[0];
    let after_rem = &search_hist[1..];
    let before_names: Vec<_> = before_rem["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(before_names, vec!["base_tool", "drop_tool"]);
    for p in after_rem {
        let names: Vec<_> = p["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["base_tool"]);
    }
    let after_inputs: Vec<&Vec<Value>> = after_rem.iter().map(|p| p["input"].as_array().unwrap()).collect();
    expect_growing_prefix(&after_inputs);

    // #10940: resolve_transcript_tools_with_support unit check
    let sys_msgs = vec![
        Message::System(SystemMessage {
            content: SystemMessageContent::Text("base prompt".to_string()),
            sections: None,
            tools_added: Some(vec![tool("base_tool"), tool("drop_tool")]),
            tools_removed: None,
            timestamp: 0,
        }),
        Message::System(SystemMessage {
            content: SystemMessageContent::Text(String::new()),
            sections: None,
            tools_removed: Some(vec![ToolReference {
                name: "drop_tool".to_string(),
            }]),
            tools_added: None,
            timestamp: 1,
        }),
        Message::System(SystemMessage {
            content: SystemMessageContent::Text(String::new()),
            sections: None,
            tools_added: Some(vec![tool("drop_tool")]),
            tools_removed: None,
            timestamp: 2,
        }),
        Message::System(SystemMessage {
            content: SystemMessageContent::Text(String::new()),
            sections: None,
            tools_added: Some(vec![Tool {
                description: "x".to_string(),
                ..tool("late_tool")
            }]),
            tools_removed: None,
            timestamp: 3,
        }),
        Message::System(SystemMessage {
            content: SystemMessageContent::Text(String::new()),
            sections: None,
            tools_added: Some(vec![Tool {
                description: "y".to_string(),
                ..tool("late_tool")
            }]),
            tools_removed: None,
            timestamp: 4,
        }),
    ];
    let r1: TranscriptTools = resolve_transcript_tools_with_support(&sys_msgs[..2], TranscriptToolSupport::Additions);
    assert_eq!(r1.request_tools, vec![tool("base_tool")]);
    assert_eq!(r1.folded_system_messages, 2);
    let r2 = resolve_transcript_tools_with_support(&sys_msgs[..4], TranscriptToolSupport::Additions);
    assert_eq!(r2.request_tools, vec![tool("base_tool"), tool("drop_tool")]);
    assert_eq!(r2.folded_system_messages, 3);
    let r3 = resolve_transcript_tools_with_support(&sys_msgs, TranscriptToolSupport::Additions);
    assert_eq!(r3.folded_system_messages, 5);
    let r_chg = resolve_transcript_tools_with_support(&sys_msgs, TranscriptToolSupport::Changes);
    assert!(r_chg.request_tools.is_empty());
    assert_eq!(r_chg.folded_system_messages, 0);

    // Kimi prefix across loads + Kimi Coding highspeed vs regular (#10720, #10940)
    let k3_model = get_model("kimi-coding", "k3").unwrap();
    let kimi_hist = capture_tool_history(&k3_model).await;
    assert_eq!(
        kimi_hist[0]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["base_tool", "drop_tool"]
    );
    for p in &kimi_hist[1..] {
        assert_eq!(
            p["tools"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t["function"]["name"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["base_tool"]
        );
    }
    let kimi_after_msgs: Vec<&Vec<Value>> = kimi_hist[1..]
        .iter()
        .map(|p| p["messages"].as_array().unwrap())
        .collect();
    expect_growing_prefix(&kimi_after_msgs);

    for id in ["k3", "k3-256k", "kimi-for-coding"] {
        let p = capture_payload(&get_model("kimi-coding", id).unwrap(), &add_ctx).await;
        assert_eq!(
            p["tools"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t["function"]["name"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["base_tool"]
        );
        let msg_tools = p["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m.get("tools").is_some())
            .unwrap();
        assert_eq!(msg_tools["tools"][0]["function"]["name"], "late_tool");
    }

    let hs_p = capture_payload(
        &get_model("kimi-coding", "kimi-for-coding-highspeed").unwrap(),
        &add_ctx,
    )
    .await;
    assert_eq!(
        hs_p["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["base_tool", "late_tool"]
    );
    assert!(
        !hs_p["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m.get("tools").is_some())
    );
}
