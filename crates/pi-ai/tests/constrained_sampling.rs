//! Port of packages/ai/test/constrained-sampling.test.ts (unit ai-providers-builtin)

use futures::StreamExt;
use indexmap::{IndexMap, IndexSet};
use pi_ai::api::constrained_sampling::{
    GrammarToolInputJsonBuffer, append_grammar_tool_input_json_delta, make_strict_json_schema,
    resolve_json_schema_strict_sampling,
};
use pi_ai::api::openai_responses_shared::{
    ConvertResponsesMessagesOptions, ConvertResponsesToolsOptions, ProcessResponsesStreamOptions,
    convert_responses_messages, convert_responses_tools, process_responses_stream,
};
use pi_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, ConstrainedSampling, ConstrainedSamplingConfig,
    Context, GrammarFormat, GrammarVariants, JsonSchemaStrict, Message, Model, ModelCost, ModelInput, StopReason,
    TextContent, Tool, ToolCall, ToolResultMessage, Usage, UserContentBlock,
};
use pi_ai::utils::event_stream::AssistantMessageEventStream;
use pi_ai::utils::transcript::normalize_context;
use pi_js::vendor::typebox::{Schema, t};
use serde_json::{Value, json};

fn make_model() -> Model {
    Model {
        r#type: None,
        id: "gpt-test".to_string(),
        name: "GPT Test".to_string(),
        api: "openai-responses".to_string(),
        provider: "openai".into(),
        base_url: "https://api.openai.com/v1".to_string(),
        reasoning: false,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text, ModelInput::Image],
        input_limits: None,
        cost: ModelCost::default(),
        context_window: 128000,
        max_tokens: 4096,
        headers: None,
        sampling_params: None,
        compat: None,
    }
}

fn make_output() -> AssistantMessage {
    AssistantMessage {
        content: Vec::new(),
        api: "openai-responses".to_string(),
        provider: "openai".into(),
        model: "gpt-test".to_string(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason: StopReason::Pending,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 1,
    }
}

fn make_tool(cs: Option<ConstrainedSampling>) -> Tool {
    Tool {
        name: "sample_tool".to_string(),
        description: "Sample tool".to_string(),
        parameters: Schema::from(json!({
            "type": "object",
            "properties": { "payload": { "type": "string" } },
            "required": ["payload"],
            "additionalProperties": false
        })),
        constrained_sampling: cs,
    }
}

#[test]
fn converts_supported_constraints_and_falls_back_when_unsupported() {
    let prefer = convert_responses_tools(
        &[make_tool(Some(
            ConstrainedSamplingConfig::JsonSchema {
                strict: JsonSchemaStrict::Prefer,
            }
            .into(),
        ))],
        None,
    )
    .unwrap();
    assert_eq!(prefer[0]["type"], "function");
    assert_eq!(prefer[0]["name"], "sample_tool");
    assert_eq!(prefer[0]["strict"], true);

    let err = convert_responses_tools(
        &[make_tool(Some(
            ConstrainedSamplingConfig::JsonSchema {
                strict: JsonSchemaStrict::Require,
            }
            .into(),
        ))],
        Some(&ConvertResponsesToolsOptions {
            strict: Some(false),
            ..Default::default()
        }),
    )
    .unwrap_err();
    assert!(
        err.message()
            .contains("Tool \"sample_tool\" requires JSON-schema constrained sampling"),
        "msg={}",
        err.message()
    );

    let mut variants = GrammarVariants::new();
    variants.insert(GrammarFormat::OpenAILark, "start: /[a-z]+/".to_string());
    let grammar_tool = make_tool(Some(ConstrainedSamplingConfig::Grammar { variants }.into()));

    let custom = convert_responses_tools(
        &[grammar_tool.clone()],
        Some(&ConvertResponsesToolsOptions {
            supports_openai_grammar_tools: Some(true),
            ..Default::default()
        }),
    )
    .unwrap();
    assert_eq!(custom[0]["type"], "custom");
    assert_eq!(custom[0]["name"], "sample_tool");
    assert_eq!(
        custom[0]["format"],
        json!({ "type": "grammar", "syntax": "lark", "definition": "start: /[a-z]+/" })
    );

    let empty_err = convert_responses_tools(
        &[make_tool(Some(
            ConstrainedSamplingConfig::Grammar {
                variants: GrammarVariants::new(),
            }
            .into(),
        ))],
        Some(&ConvertResponsesToolsOptions {
            supports_openai_grammar_tools: Some(true),
            ..Default::default()
        }),
    )
    .unwrap_err();
    assert!(
        empty_err.message().contains(
            "Tool \"sample_tool\" cannot use grammar constrained sampling: no supported grammar variant was provided"
        ),
        "msg={}",
        empty_err.message()
    );

    let fallback = convert_responses_tools(
        &[grammar_tool],
        Some(&ConvertResponsesToolsOptions {
            strict: Some(false),
            supports_openai_grammar_tools: Some(false),
            ..Default::default()
        }),
    )
    .unwrap();
    assert_eq!(fallback[0]["type"], "function");
    assert_eq!(fallback[0]["name"], "sample_tool");
    assert!(fallback[0].get("strict").is_none());

    assert_eq!(
        convert_responses_tools(&[make_tool(Some(ConstrainedSampling::Disabled))], None).unwrap(),
        convert_responses_tools(&[make_tool(None)], None).unwrap()
    );
}

#[test]
fn derives_strict_provider_schemas_without_mutating_definitions() {
    let parameters = t::object([
        ("path", t::string()),
        ("offset", t::number().optional()),
        ("metadata", t::object([("enabled", t::boolean().optional())])),
        ("nullable", t::union([t::string(), t::null()]).optional()),
    ]);
    let raw_before = parameters.to_value();

    let strict = make_strict_json_schema(&parameters, None).unwrap();
    let strict_val = Value::Object(strict.into_iter().collect());

    assert!(raw_before.get("additionalProperties").is_none());
    assert_eq!(raw_before["required"], json!(["path", "metadata"]));
    assert_eq!(strict_val["additionalProperties"], false);
    assert_eq!(
        strict_val["required"],
        json!(["path", "offset", "metadata", "nullable"])
    );
    assert_eq!(
        strict_val["properties"]["offset"],
        json!({ "anyOf": [{ "type": "number" }, { "type": "null" }] })
    );
    assert_eq!(strict_val["properties"]["metadata"]["additionalProperties"], false);
    assert_eq!(
        strict_val["properties"]["metadata"]["properties"]["enabled"],
        json!({ "anyOf": [{ "type": "boolean" }, { "type": "null" }] })
    );
    assert_eq!(
        strict_val["properties"]["nullable"],
        json!({ "anyOf": [{ "type": "string" }, { "type": "null" }] })
    );
}

#[test]
fn falls_back_or_rejects_unsupported_strict_schemas() {
    let cases = [
        (
            Schema::from(json!({
                "type": "object",
                "properties": {
                    "metadata": { "type": "object", "properties": {}, "additionalProperties": { "type": "string" } }
                },
                "required": ["metadata"]
            })),
            "additionalProperties is unsupported",
        ),
        (
            t::intersect([t::object([("a", t::string())]), t::object([("b", t::number())])]),
            "allOf schemas are unsupported",
        ),
        (
            t::object([("value", t::union([t::object([("nested", t::string())]), t::null()]))]),
            "object and array unions are unsupported",
        ),
        (
            Schema::from(json!({
                "type": "object",
                "properties": { "child": { "$ref": "https://example.com/child.json" } },
                "required": ["child"]
            })),
            "$ref schemas are unsupported",
        ),
    ];

    for (parameters, expected_err) in cases {
        let mut tool = make_tool(Some(
            ConstrainedSamplingConfig::JsonSchema {
                strict: JsonSchemaStrict::Prefer,
            }
            .into(),
        ));
        tool.parameters = parameters.clone();

        let err = make_strict_json_schema(&parameters, None).unwrap_err();
        assert!(err.message().contains(expected_err), "msg={}", err.message());

        assert!(
            resolve_json_schema_strict_sampling(&tool, true, None)
                .unwrap()
                .is_none()
        );
        let converted = convert_responses_tools(
            &[tool.clone()],
            Some(&ConvertResponsesToolsOptions {
                strict: Some(true),
                ..Default::default()
            }),
        )
        .unwrap();
        assert_eq!(converted[0]["strict"], false);

        tool.constrained_sampling = Some(
            ConstrainedSamplingConfig::JsonSchema {
                strict: JsonSchemaStrict::Require,
            }
            .into(),
        );
        let req_err = resolve_json_schema_strict_sampling(&tool, true, None).unwrap_err();
        assert!(req_err.message().contains(expected_err), "msg={}", req_err.message());
    }
}

#[test]
fn replays_grammar_calls_and_drops_foreign_item_ids() {
    let mut allowed = IndexSet::new();
    allowed.insert("openai".to_string());
    let mut grammar_props = IndexMap::new();
    grammar_props.insert("sample_tool".to_string(), "payload".to_string());
    let conv_opts = ConvertResponsesMessagesOptions {
        include_system_prompt: None,
        grammar_tool_input_properties: Some(grammar_props.clone()),
        ..Default::default()
    };

    for bad_args in [
        IndexMap::new(),
        [("payload".to_string(), json!(42))].into_iter().collect(),
    ] {
        let tc = ToolCall {
            id: "call_1|ctc_1".to_string(),
            name: "sample_tool".to_string(),
            namespace: None,
            arguments: bad_args,
            thought_signature: None,
        };
        let ctx = normalize_context(&Context {
            system_prompt: None,
            tools: None,
            messages: vec![
                Message::Assistant(AssistantMessage {
                    content: vec![AssistantContentBlock::ToolCall(tc)],
                    api: "openai-responses".to_string(),
                    provider: "openai".into(),
                    model: "gpt-test".to_string(),
                    response_model: None,
                    response_id: None,
                    provider_thinking_level: None,
                    thinking_level: None,
                    diagnostics: None,
                    usage: Usage::default(),
                    stop_reason: StopReason::ToolUse,
                    deferred: None,
                    error_message: None,
                    raw_stop_reason: None,
                    end_turn: None,
                    timestamp: 1,
                }),
                Message::ToolResult(ToolResultMessage {
                    tool_call_id: "call_1|ctc_1".to_string(),
                    tool_name: "sample_tool".to_string(),
                    content: vec![UserContentBlock::Text(TextContent {
                        text: "done".to_string(),
                        text_signature: None,
                    })],
                    details: None,
                    usage: None,
                    nested_calls: None,
                    is_error: false,
                    timestamp: 2,
                }),
            ],
        });
        assert!(convert_responses_messages(&make_model(), &ctx, &allowed, Some(&conv_opts)).is_empty() == false);
    }

    let ok_tc = ToolCall {
        id: "call_1|ctc_1".to_string(),
        name: "sample_tool".to_string(),
        namespace: None,
        arguments: [("payload".to_string(), json!("abc"))].into_iter().collect(),
        thought_signature: None,
    };
    let ok_ctx = normalize_context(&Context {
        system_prompt: None,
        tools: None,
        messages: vec![
            Message::Assistant(AssistantMessage {
                content: vec![AssistantContentBlock::ToolCall(ok_tc)],
                api: "openai-responses".to_string(),
                provider: "openai".into(),
                model: "gpt-test".to_string(),
                response_model: None,
                response_id: None,
                provider_thinking_level: None,
                thinking_level: None,
                diagnostics: None,
                usage: Usage::default(),
                stop_reason: StopReason::ToolUse,
                deferred: None,
                error_message: None,
                raw_stop_reason: None,
                end_turn: None,
                timestamp: 1,
            }),
            Message::ToolResult(ToolResultMessage {
                tool_call_id: "call_1|ctc_1".to_string(),
                tool_name: "sample_tool".to_string(),
                content: vec![UserContentBlock::Text(TextContent {
                    text: "done".to_string(),
                    text_signature: None,
                })],
                details: None,
                usage: None,
                nested_calls: None,
                is_error: false,
                timestamp: 2,
            }),
        ],
    });
    let msgs = convert_responses_messages(&make_model(), &ok_ctx, &allowed, Some(&conv_opts));
    assert!(msgs.contains(&json!({
        "type": "custom_tool_call",
        "id": "ctc_1",
        "call_id": "call_1",
        "name": "sample_tool",
        "input": "abc"
    })));
    assert!(msgs.contains(&json!({
        "type": "custom_tool_call_output",
        "call_id": "call_1",
        "output": "done"
    })));

    // Foreign provider item ids dropped
    let foreign_ctx = normalize_context(&Context {
        system_prompt: None,
        tools: None,
        messages: vec![
            Message::Assistant(AssistantMessage {
                content: vec![AssistantContentBlock::ToolCall(ToolCall {
                    id: "call_1|ctc_1".to_string(),
                    name: "sample_tool".to_string(),
                    namespace: None,
                    arguments: [("payload".to_string(), json!("abc"))].into_iter().collect(),
                    thought_signature: None,
                })],
                api: "pi-messages".to_string(),
                provider: "radius".into(),
                model: "gpt-other".to_string(),
                response_model: None,
                response_id: None,
                provider_thinking_level: None,
                thinking_level: None,
                diagnostics: None,
                usage: Usage::default(),
                stop_reason: StopReason::ToolUse,
                deferred: None,
                error_message: None,
                raw_stop_reason: None,
                end_turn: None,
                timestamp: 1,
            }),
            Message::ToolResult(ToolResultMessage {
                tool_call_id: "call_1|ctc_1".to_string(),
                tool_name: "sample_tool".to_string(),
                content: vec![UserContentBlock::Text(TextContent {
                    text: "done".to_string(),
                    text_signature: None,
                })],
                details: None,
                usage: None,
                nested_calls: None,
                is_error: false,
                timestamp: 2,
            }),
        ],
    });
    let fmsgs = convert_responses_messages(&make_model(), &foreign_ctx, &allowed, Some(&conv_opts));
    let call = fmsgs
        .iter()
        .find(|item| item.get("type").and_then(Value::as_str) == Some("custom_tool_call"))
        .unwrap();
    assert_eq!(call["call_id"], "call_1");
    assert_eq!(call["input"], "abc");
    assert!(call.get("id").is_none());
}

#[test]
fn keeps_grammar_input_json_deltas_append_only() {
    let mut buffer = GrammarToolInputJsonBuffer::default();
    let first = append_grammar_tool_input_json_delta(&mut buffer, "payload", "a\"", false)
        .unwrap()
        .unwrap();
    let second = append_grammar_tool_input_json_delta(&mut buffer, "payload", "a\"\nb", true)
        .unwrap()
        .unwrap();
    let parsed: Value = serde_json::from_str(&format!("{first}{second}")).unwrap();
    assert_eq!(parsed, json!({ "payload": "a\"\nb" }));

    assert_eq!(
        append_grammar_tool_input_json_delta(&mut buffer, "payload", "a\"\nb", true).unwrap(),
        None
    );
    let err = append_grammar_tool_input_json_delta(&mut buffer, "payload", "changed", true).unwrap_err();
    assert!(
        err.message()
            .contains("grammar tool input for property \"payload\" changed after it was closed"),
        "msg={}",
        err.message()
    );
}

#[tokio::test]
async fn starts_custom_responses_tool_calls_with_initial_input() {
    let mut output = make_output();
    let event_stream = AssistantMessageEventStream::new();
    let mut grammar_props = IndexMap::new();
    grammar_props.insert("sample_tool".to_string(), "payload".to_string());

    let raw_events = vec![
        Ok(json!({
            "type": "response.output_item.added",
            "output_index": 0,
            "item": { "type": "custom_tool_call", "call_id": "call_1", "id": "ctc_1", "name": "sample_tool", "input": "a" }
        })),
        Ok(json!({
            "type": "response.custom_tool_call_input.delta",
            "output_index": 0,
            "item_id": "ctc_1",
            "delta": "b"
        })),
        Ok(json!({
            "type": "response.custom_tool_call_input.done",
            "output_index": 0,
            "item_id": "ctc_1",
            "input": "abc"
        })),
        Ok(json!({
            "type": "response.output_item.done",
            "output_index": 0,
            "item": { "type": "custom_tool_call", "call_id": "call_1", "id": "ctc_1", "name": "sample_tool", "input": "abc" }
        })),
        Ok(json!({
            "type": "response.completed",
            "response": { "status": "completed", "usage": { "input_tokens": 1, "output_tokens": 1, "total_tokens": 2 } }
        })),
    ];

    let events_stream = Box::pin(futures::stream::iter(raw_events));
    let reader_stream = event_stream.clone();
    process_responses_stream(
        events_stream,
        &mut output,
        &event_stream,
        &make_model(),
        Some(&ProcessResponsesStreamOptions {
            grammar_tool_input_properties: Some(grammar_props),
            ..Default::default()
        }),
    )
    .await
    .unwrap();
    event_stream.end(Some(output.clone()));

    let mut deltas = String::new();
    let mut starts = Vec::new();
    let collected: Vec<_> = reader_stream.collect().await;
    for ev in collected {
        match ev {
            AssistantMessageEvent::ToolCallStart { content_index, partial } => {
                if let Some(AssistantContentBlock::ToolCall(tc)) = partial.content.get(content_index) {
                    starts.push(tc.arguments.clone());
                }
            }
            AssistantMessageEvent::ToolCallDelta { delta, .. } => deltas.push_str(&delta),
            _ => {}
        }
    }

    assert_eq!(output.stop_reason, StopReason::ToolUse);
    assert_eq!(
        starts,
        vec![
            [("payload".to_string(), json!("a"))]
                .into_iter()
                .collect::<IndexMap<_, _>>()
        ]
    );
    assert_eq!(
        output.content,
        vec![AssistantContentBlock::ToolCall(ToolCall {
            id: "call_1|ctc_1".to_string(),
            name: "sample_tool".to_string(),
            namespace: None,
            arguments: [("payload".to_string(), json!("abc"))].into_iter().collect(),
            thought_signature: None,
        })]
    );
    assert_eq!(
        serde_json::from_str::<Value>(&deltas).unwrap(),
        json!({ "payload": "abc" })
    );
}
