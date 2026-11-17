//! Port of packages/ai/test/transcript-tool-changes.test.ts (unit ai-providers-builtin)

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use pi_ai::compat::stream_simple;
use pi_ai::types::{
    AnthropicMessagesCompat, Context, Message, Model, ModelCompat, ModelCost, ModelInput, OpenAICompletionsCompat,
    OpenAIResponsesCompat, SimpleStreamOptions, StreamOptions, SystemMessage, SystemMessageContent, Tool,
    ToolReference, UserMessage, UserMessageContent,
};
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

#[tokio::test]
async fn sends_anthropic_updates_and_falls_back_when_unsupported_or_redefined() {
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
    assert!(
        payload["betas"]
            .as_array()
            .unwrap()
            .contains(&json!("mid-conversation-tool-changes-2026-07-01"))
    );
    assert_eq!(
        payload["system"].as_array().unwrap()[0]["text"],
        "base prompt\n\n<rules>\nold rules\n</rules>\n\n<docs>\nread docs\n</docs>"
    );
    let tools = payload["tools"].as_array().unwrap();
    assert_eq!(tools[0]["name"], "base_tool");
    assert_eq!(tools[0]["cache_control"], json!({ "type": "ephemeral" }));
    assert!(tools[0].get("defer_loading").is_none());
    assert_eq!(tools[1]["name"], "__pi_deferred_placeholder__");
    assert_eq!(tools[1]["defer_loading"], true);
    assert!(tools[1].get("cache_control").is_none());
    assert_eq!(tools[2]["name"], "late_tool");
    assert_eq!(tools[2]["defer_loading"], true);
    assert!(tools[2].get("cache_control").is_none());

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
    assert_eq!(ucontent[1]["tool"]["name"], "base_tool");
    assert_eq!(ucontent[2]["type"], "tool_addition");
    assert_eq!(ucontent[2]["tool"]["name"], "late_tool");

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

    // Fallbacks: redefinition & no initial tool
    let mut redefined = tool("base_tool");
    redefined.description = "changed".to_string();
    for fallback_ctx in [
        Context {
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
                Message::System(SystemMessage {
                    content: SystemMessageContent::Text("updated guidance".to_string()),
                    sections: None,
                    tools_removed: Some(vec![ToolReference {
                        name: "base_tool".to_string(),
                    }]),
                    tools_added: Some(vec![redefined.clone()]),
                    timestamp: 2,
                }),
            ],
        },
        Context {
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
                Message::System(SystemMessage {
                    content: SystemMessageContent::Text("updated guidance".to_string()),
                    sections: None,
                    tools_added: Some(vec![redefined]),
                    tools_removed: None,
                    timestamp: 2,
                }),
            ],
        },
    ] {
        let fb = capture_payload(&native_model, &fallback_ctx).await;
        if let Some(betas) = fb.get("betas").and_then(Value::as_array) {
            assert!(!betas.contains(&json!("mid-conversation-tool-changes-2026-07-01")));
        }
        assert_eq!(fb["tools"][0]["name"], "base_tool");
        assert_eq!(fb["tools"][0]["description"], "changed");
        assert_eq!(fb["tools"][0]["cache_control"], json!({ "type": "ephemeral" }));
        assert!(fb["tools"][0].get("defer_loading").is_none());
    }

    // Without native support -> folded into system prompt
    let no_support = base_model(
        "claude-sonnet-4-5",
        "Claude Sonnet 4.5",
        "anthropic-messages",
        "anthropic",
        None,
    );
    let folded = capture_payload(&no_support, &ctx).await;
    assert_eq!(
        folded["system"][0]["text"],
        "base prompt\n\nupdated guidance\n\n<rules>\nnew rules\n</rules>"
    );
    assert_eq!(
        folded["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["late_tool"]
    );
    assert_eq!(
        folded["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["role"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["user"]
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
async fn sends_openai_responses_and_completions_mid_convo_tool_changes() {
    let add_ctx = addition_context();
    let full_ctx = sample_context();

    // OpenAI additional_tools
    let resp_add = base_model(
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
    let p1 = capture_payload(&resp_add, &add_ctx).await;
    assert_eq!(p1["tools"].as_array().unwrap().len(), 1);
    assert_eq!(p1["tools"][0]["name"], "base_tool");
    let add_item = p1["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i.get("type").and_then(Value::as_str) == Some("additional_tools"))
        .unwrap();
    assert_eq!(add_item["tools"][0]["name"], "late_tool");
    let dev_contents: Vec<_> = p1["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i.get("role").and_then(Value::as_str) == Some("developer") && i.get("type").is_none())
        .map(|i| i["content"].as_str().unwrap())
        .collect();
    assert_eq!(dev_contents, vec!["base prompt", "updated guidance"]);

    // OpenAI synthetic tool_search
    let resp_search = base_model(
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
    let p2 = capture_payload(&resp_search, &add_ctx).await;
    assert!(
        p2["input"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i.get("type").and_then(Value::as_str) == Some("tool_search_call"))
    );
    let search_out = p2["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i.get("type").and_then(Value::as_str) == Some("tool_search_output"))
        .unwrap();
    assert_eq!(search_out["tools"][0]["name"], "late_tool");

    // Without mid-convo system support -> folds
    let resp_fold = base_model(
        "gpt-4.1",
        "GPT-4.1",
        "openai-responses",
        "openai",
        Some(ModelCompat::OpenAIResponses(OpenAIResponsesCompat {
            supports_additional_tools: Some(true),
            ..Default::default()
        })),
    );
    let p3 = capture_payload(&resp_fold, &full_ctx).await;
    assert_eq!(p3["tools"][0]["name"], "late_tool");
    assert_eq!(
        p3["input"][0]["content"],
        "base prompt\n\nupdated guidance\n\n<rules>\nnew rules\n</rules>"
    );

    // Removals unsupported -> falls back to full current tool state
    let p4 = capture_payload(&resp_add, &full_ctx).await;
    assert_eq!(p4["tools"][0]["name"], "late_tool");
    assert!(
        !p4["input"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i.get("type").and_then(Value::as_str) == Some("additional_tools"))
    );

    // Kimi K3 tool-bearing system messages
    let kimi_k3 = base_model(
        "kimi-k3",
        "Kimi K3",
        "openai-completions",
        "moonshotai",
        Some(ModelCompat::OpenAICompletions(OpenAICompletionsCompat {
            supports_mid_convo_system_messages: Some(true),
            supports_mid_convo_tool_additions: Some(true),
            ..Default::default()
        })),
    );
    let pk3 = capture_payload(&kimi_k3, &add_ctx).await;
    assert_eq!(pk3["tools"][0]["function"]["name"], "base_tool");
    let tool_msg = pk3["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m.get("tools").is_some())
        .unwrap();
    assert_eq!(tool_msg["tools"][0]["function"]["name"], "late_tool");

    // Kimi K2 without mid-convo tool additions -> inline system text, static tools
    let kimi_k2 = base_model(
        "kimi-k2.7-code",
        "Kimi K2.7 Code",
        "openai-completions",
        "moonshotai",
        Some(ModelCompat::OpenAICompletions(OpenAICompletionsCompat {
            supports_mid_convo_system_messages: Some(true),
            ..Default::default()
        })),
    );
    let pk2 = capture_payload(&kimi_k2, &add_ctx).await;
    assert_eq!(pk2["tools"].as_array().unwrap().len(), 2);
    assert!(
        !pk2["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m.get("tools").is_some())
    );
}
