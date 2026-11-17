//! Port of packages/coding-agent/examples/extensions/jev-router.ts

use std::process::ExitCode;
use std::sync::Arc;

use async_trait::async_trait;
use indexmap::IndexMap;
use pi_ai::compat::builtin_models;
use pi_ai::models::ModelsClassifierOptions;
use pi_ai::types::{
    AnyModel, ClassifierAnswer, ClassifierContext, ClassifierOptions, ClassifierQuestion, ClassifierStopReason,
    Message, ModelThinkingLevel, ModelType, ProviderRequestOptions, UserContentBlock, UserMessageContent,
};
use pi_coding_agent::core::extensions::types::{
    Extension, ExtensionApi, ExtensionContext, ExtensionFactory, ExtensionVirtualModel, InlineExtension, mcp_wire,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

const PROVIDER: &str = "openai-codex";
const SOL: &str = "gpt-5.6-sol";
const TERRA: &str = "gpt-5.6-terra";
const LUNA: &str = "gpt-5.6-luna";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Phase {
    Planning,
    Implementation,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct JevState {
    phase: Phase,
    model: String,
}

fn route_to(
    request: &mcp_wire::ModelRouteRequest,
    ctx: &ExtensionContext,
    id: &str,
    state: Option<&JevState>,
) -> pi_js::Result<mcp_wire::ModelRoute> {
    let model = ctx
        .model_registry()?
        .get_available_models()
        .into_iter()
        .find(|m| m.provider == PROVIDER && m.id == id)
        .ok_or_else(|| pi_js::Error::msg(format!("Model {PROVIDER}/{id} is not in the catalog")))?;
    let serialized_state = state.and_then(|s| serde_json::to_value(s).ok());
    Ok(mcp_wire::ModelRoute {
        model,
        thinking_level: request.thinking_level,
        state: serialized_state,
    })
}

fn last_user_text(messages: &[Message]) -> String {
    let Some(msg) = messages.iter().rfind(|m| matches!(m, Message::User(_))) else {
        return String::new();
    };
    let Message::User(user) = msg else {
        return String::new();
    };
    match &user.content {
        UserMessageContent::Text(s) => s.clone(),
        UserMessageContent::Blocks(blocks) => blocks
            .iter()
            .filter_map(|b| match b {
                UserContentBlock::Text(tc) => Some(tc.text.as_str()),
                UserContentBlock::Image(_) => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn edited_this_turn(messages: &[Message]) -> bool {
    let last_user_idx = messages.iter().rposition(|m| matches!(m, Message::User(_)));
    let slice = match last_user_idx {
        Some(idx) => &messages[idx + 1..],
        None => messages,
    };
    slice.iter().any(|m| match m {
        Message::ToolResult(tr) => (tr.tool_name == "edit" || tr.tool_name == "write") && !tr.is_error,
        _ => false,
    })
}

async fn choose_planning_model(request: &mcp_wire::ModelRouteRequest) -> &'static str {
    // WIRE(@mcp-w3): ModelRouteRequest omits `previous`; keep planning continuity when added.
    let Some(AnyModel::Classifier(jev)) =
        builtin_models().get_model_of_type(ModelType::Classifier, "typesafe", "jev-latest")
    else {
        return TERRA;
    };

    let prompt_excerpt: String = last_user_text(&request.messages).chars().take(16_000).collect();
    let mut state_map = Map::new();
    state_map.insert("prompt".to_string(), Value::String(prompt_excerpt));

    let mut criteria = IndexMap::new();
    criteria.insert(
        "standard".to_string(),
        "Ordinary features, fixes, reviews, or questions".to_string(),
    );
    criteria.insert(
        "complex".to_string(),
        "Subtle design, cross-cutting changes, or hard debugging".to_string(),
    );

    let mut questions = IndexMap::new();
    questions.insert(
        "complexity".to_string(),
        ClassifierQuestion::Choice {
            instructions: "How demanding is the software engineering work requested in `prompt`?".to_string(),
            criteria,
        },
    );

    let result = builtin_models()
        .classify(
            &jev,
            &ClassifierContext {
                state: state_map,
                questions,
            },
            Some(ModelsClassifierOptions {
                base: ClassifierOptions {
                    base: ProviderRequestOptions {
                        signal: request.signal.clone(),
                        ..Default::default()
                    },
                    temperature: None,
                },
                transform_headers: None,
            }),
        )
        .await;

    if result.stop_reason == ClassifierStopReason::Stop
        && let Some(ClassifierAnswer::Choice { probabilities, .. }) = result.answers.get("complexity")
        && probabilities.get("complex").copied().unwrap_or(0.0) >= 0.5
    {
        return SOL;
    }

    TERRA
}

pub struct JevRouterExtension;

#[async_trait]
impl Extension for JevRouterExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        pi.register_virtual_model(ExtensionVirtualModel {
            provider: "jev".to_string(),
            id: "auto".to_string(),
            name: "Auto (Jev)".to_string(),
            thinking_levels: Some(vec![
                ModelThinkingLevel::Low,
                ModelThinkingLevel::Medium,
                ModelThinkingLevel::High,
                ModelThinkingLevel::XHigh,
            ]),
            route: Arc::new(|request, ctx| {
                Box::pin(async move {
                    if request.reason == mcp_wire::ModelRouteReason::Direct {
                        return route_to(&request, &ctx, LUNA, None);
                    }

                    let parsed_state = request
                        .state
                        .as_ref()
                        .and_then(|v| serde_json::from_value::<JevState>(v.clone()).ok());

                    let Some(state) = parsed_state else {
                        let model = choose_planning_model(&request).await;
                        let next_state = JevState {
                            phase: Phase::Planning,
                            model: model.to_string(),
                        };
                        return route_to(&request, &ctx, model, Some(&next_state));
                    };

                    if state.phase == Phase::Planning && edited_this_turn(&request.messages) {
                        let next_state = JevState {
                            phase: Phase::Implementation,
                            model: LUNA.to_string(),
                        };
                        return route_to(&request, &ctx, LUNA, Some(&next_state));
                    }

                    route_to(&request, &ctx, &state.model, None)
                })
            }),
        })?;

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(JevRouterExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
