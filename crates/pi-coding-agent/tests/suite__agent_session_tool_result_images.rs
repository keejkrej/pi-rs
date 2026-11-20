//! Port of packages/coding-agent/test/suite/agent-session-tool-result-images.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::Arc;

use pi_agent_core::types::{AgentTool, AgentToolResult};
use pi_ai::providers::faux::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxContentBlock, FauxResponseStep,
    faux_assistant_message, faux_tool_call,
};
use pi_ai::types::{
    ImageContent, Message, ModelImageInputLimits, ModelImageResizeOptions, ModelInputLimits, StopReason, TextContent,
    UserContentBlock,
};
use pi_coding_agent::core::settings_manager::{ImageSettings, Settings};
use pi_js::vendor::typebox::t;
use pretty_assertions::assert_eq;
use serde_json::json;
use support::suite::harness::{HarnessOptions, create_harness};

/// 4x4 valid RGB PNG so `max_width: 1, max_height: 1` forces resizing when auto-resize is enabled.
const PNG_4X4_BASE64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAQAAAAECAIAAAAmkwkpAAAAEElEQVR4nGP4z8AARwzEcQCukw/x0F8jngAAAABJRU5ErkJggg==";

fn screenshot_tool() -> AgentTool {
    AgentTool {
        name: "screenshot".to_string(),
        label: "Screenshot".to_string(),
        description: "Return a screenshot".to_string(),
        parameters: t::object([] as [(&str, pi_js::vendor::typebox::Schema); 0]).into(),
        constrained_sampling: None,
        output_schema: None,
        execution_mode: None,
        prepare_arguments: None,
        execute: Arc::new(|_id, _params, _sig, _upd| {
            Box::pin(async move {
                Ok(AgentToolResult {
                    content: vec![
                        UserContentBlock::Text(TextContent::new("captured")),
                        UserContentBlock::Image(ImageContent {
                            data: PNG_4X4_BASE64.to_string(),
                            mime_type: "image/png".to_string(),
                        }),
                    ],
                    details: Some(json!({})),
                    ..Default::default()
                })
            })
        }),
        replay: None,
    }
}

#[tokio::test]
async fn respects_auto_resize_false_when_model_specifies_resize_profile() {
    let harness = create_harness(HarnessOptions {
        tools: Some(vec![screenshot_tool()]),
        settings: Some(Settings {
            images: Some(ImageSettings {
                auto_resize: Some(false),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    })
    .await;

    let mut model = harness.session.model().unwrap();
    model.input_limits = Some(ModelInputLimits {
        max_request_bytes: None,
        images: Some(ModelImageInputLimits {
            resize: Some(ModelImageResizeOptions {
                max_width: Some(1),
                max_height: Some(1),
                max_bytes: Some(500_000),
                jpeg_quality: Some(70.0),
            }),
            max_per_message: None,
            max_per_request: None,
        }),
    });
    harness.session.agent().set_model(model);
    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::ToolCall(faux_tool_call(
                "screenshot",
                indexmap::IndexMap::new(),
                None,
            ))),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("done".to_string()),
            None,
        )),
    ]);

    harness.session.prompt("take a screenshot", None).await.unwrap();

    let tool_result = harness
        .session
        .messages()
        .into_iter()
        .find_map(|m| match m {
            pi_agent_core::types::AgentMessage::Message(Message::ToolResult(tr)) => Some(tr),
            _ => None,
        })
        .unwrap();
    assert_eq!(tool_result.content.len(), 2);
    assert!(matches!(&tool_result.content[1], UserContentBlock::Image(img) if img.data == PNG_4X4_BASE64));

    harness.cleanup();
}

#[tokio::test]
async fn applies_current_model_resize_profile_to_tool_result_images_by_default() {
    let harness = create_harness(HarnessOptions {
        tools: Some(vec![screenshot_tool()]),
        ..Default::default()
    })
    .await;

    let mut model = harness.session.model().unwrap();
    model.input_limits = Some(ModelInputLimits {
        max_request_bytes: None,
        images: Some(ModelImageInputLimits {
            resize: Some(ModelImageResizeOptions {
                max_width: Some(1),
                max_height: Some(1),
                max_bytes: Some(500_000),
                jpeg_quality: Some(70.0),
            }),
            max_per_message: None,
            max_per_request: None,
        }),
    });
    harness.session.agent().set_model(model);
    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::ToolCall(faux_tool_call(
                "screenshot",
                indexmap::IndexMap::new(),
                None,
            ))),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("done".to_string()),
            None,
        )),
    ]);

    harness.session.prompt("take a screenshot", None).await.unwrap();

    let tool_result = harness
        .session
        .messages()
        .into_iter()
        .find_map(|m| match m {
            pi_agent_core::types::AgentMessage::Message(Message::ToolResult(tr)) => Some(tr),
            _ => None,
        })
        .unwrap();
    let has_dimension_note = tool_result.content.iter().any(|b| match b {
        UserContentBlock::Text(t) => t.text.contains("original 4x4, displayed at 1x1"),
        _ => false,
    });
    assert!(has_dimension_note);

    harness.cleanup();
}
