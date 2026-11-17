//! Port of packages/coding-agent/test/experimental-agent-controller.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::Arc;

use pi_ai::{FauxAssistantMessageContent, FauxResponseStep, faux_assistant_message};
use pi_chord::context::BACKGROUND_CONTEXT;
use pi_chord::services::handle::ServiceObject;
use pi_chord::{Facet, ServiceCall, create_facet_host, define_facet};
use pi_coding_agent::experimental::services::agent_controller::{
    AGENT_CONTROLLER_SERVICE, AgentCompactionRequest, AgentOperationError, AgentOperationResponse, AgentPromptRequest,
    AgentPromptResult, AgentQueueResponse, CancelQueuedOutcome, CancelQueuedResult,
};
use pi_coding_agent::experimental::services::agent_controller_provider::{
    create_agent_controller, create_agent_controller_service_object,
};
use pi_durable::InboxState;
use serde_json::json;

use support::experimental_durable_support::{open_faux_conversation, pending_response};

#[tokio::test]
async fn prompts_the_root_conversation_through_the_service_catalogue() {
    let msg = faux_assistant_message(FauxAssistantMessageContent::Text("hello back".to_string()), None);
    let conv = open_faux_conversation(vec![FauxResponseStep::Message(msg)])
        .await
        .expect("open faux");
    let harness = conv.harness.clone();
    let conversation = conv.conversation.clone();

    let host = create_facet_host(pi_chord::FacetOptions {
        facets: vec![define_facet(Facet::new(
            "test-agent-controller",
            Arc::new(move |env| {
                env.provide(
                    &*AGENT_CONTROLLER_SERVICE,
                    create_agent_controller_service_object(harness.clone(), conversation.clone()),
                )
            }),
        ))],
        service_sources: None,
        on_error: None,
    })
    .await
    .expect("create host");

    let services = host.services();
    let catalogue = services.catalogue();
    assert_eq!(catalogue.len(), 1);
    assert_eq!(catalogue[0].service_id, AGENT_CONTROLLER_SERVICE.id);
    assert_eq!(catalogue[0].mode, pi_chord::ServiceMode::Singleton);

    let response_val = host
        .services()
        .invoke(
            &ServiceCall {
                service_id: AGENT_CONTROLLER_SERVICE.id.clone(),
                instance: None,
                member: "prompt".to_string(),
                args: vec![json!({ "message": "hello", "images": null })],
            },
            BACKGROUND_CONTEXT.clone(),
        )
        .await
        .expect("invoke prompt")
        .expect("some response");
    let response: AgentOperationResponse = serde_json::from_value(response_val).expect("parse response");
    assert!(response.accepted);
    assert!(response.error.is_none());
    let op_id = response.operation_id.expect("operation id");

    let wait_val = host
        .services()
        .invoke(
            &ServiceCall {
                service_id: AGENT_CONTROLLER_SERVICE.id.clone(),
                instance: None,
                member: "waitForPrompt".to_string(),
                args: vec![json!(op_id)],
            },
            BACKGROUND_CONTEXT.clone(),
        )
        .await
        .expect("invoke waitForPrompt")
        .expect("some result");
    let result: AgentPromptResult = serde_json::from_value(wait_val).expect("parse wait result");
    assert_eq!(
        result,
        AgentPromptResult::Done {
            text: "hello back".to_string(),
            reason: None,
        }
    );

    host.dispose().await.expect("dispose host");
    conv.close().await.expect("close faux");
}

#[tokio::test]
async fn rejects_a_prompt_while_busy_and_queues_steering_and_follow_up_input() {
    let pending = pending_response();
    let reached = pending.reached.clone();
    let conv = open_faux_conversation(vec![pending.step]).await.expect("open faux");
    let controller = create_agent_controller(conv.harness.clone(), conv.conversation.clone());

    let first = controller
        .prompt(
            AgentPromptRequest {
                message: "first".to_string(),
                images: None,
            },
            BACKGROUND_CONTEXT.clone(),
        )
        .await
        .expect("first prompt");
    assert!(first.accepted);
    reached.await;

    let second = controller
        .prompt(
            AgentPromptRequest {
                message: "second".to_string(),
                images: None,
            },
            BACKGROUND_CONTEXT.clone(),
        )
        .await
        .expect("second prompt");
    assert!(!second.accepted);
    assert!(second.operation_id.is_none());
    assert!(matches!(
        second.error,
        Some(AgentOperationError { ref code, ref message }) if code == "busy" && message.contains("busy")
    ));

    let steer = controller
        .steer(
            AgentPromptRequest {
                message: "steer".to_string(),
                images: None,
            },
            BACKGROUND_CONTEXT.clone(),
        )
        .await
        .expect("steer");
    let follow_up = controller
        .follow_up(
            AgentPromptRequest {
                message: "later".to_string(),
                images: None,
            },
            BACKGROUND_CONTEXT.clone(),
        )
        .await
        .expect("follow_up");
    assert!(matches!(
        steer,
        AgentQueueResponse {
            accepted: true,
            entry_id: Some(_),
            error: None
        }
    ));
    assert!(matches!(
        follow_up,
        AgentQueueResponse {
            accepted: true,
            entry_id: Some(_),
            error: None
        }
    ));

    let inbox_state = conv
        .conversation
        .view_state(BACKGROUND_CONTEXT.clone())
        .await
        .expect("inbox view state");
    let inbox_val = inbox_state.value().docs.get("pi.inbox").cloned().expect("pi.inbox doc");
    let inbox: InboxState = serde_json::from_value(serde_json::Value::Object(inbox_val)).expect("parse InboxState");
    let modes: Vec<&str> = inbox
        .items
        .iter()
        .map(|item| match item {
            pi_durable::InboxItem::Steer { .. } => "steer",
            pi_durable::InboxItem::FollowUp { .. } => "followUp",
            pi_durable::InboxItem::Write { .. } => "write",
        })
        .collect();
    assert_eq!(modes, vec!["steer", "followUp"]);
    inbox_state.dispose();

    let follow_up_id = follow_up.entry_id.expect("follow-up entry id");
    assert_eq!(
        controller
            .cancel_queued(&follow_up_id, BACKGROUND_CONTEXT.clone())
            .await
            .expect("cancel"),
        CancelQueuedResult {
            outcome: CancelQueuedOutcome::Cancelled,
        }
    );
    assert_eq!(
        controller
            .cancel_queued(&follow_up_id, BACKGROUND_CONTEXT.clone())
            .await
            .expect("cancel twice"),
        CancelQueuedResult {
            outcome: CancelQueuedOutcome::AlreadyConsumed,
        }
    );
    assert_eq!(
        controller
            .cancel_queued("999", BACKGROUND_CONTEXT.clone())
            .await
            .expect("cancel 999"),
        CancelQueuedResult {
            outcome: CancelQueuedOutcome::NotFound,
        }
    );
    assert_eq!(
        controller
            .cancel_queued("not-an-id", BACKGROUND_CONTEXT.clone())
            .await
            .expect("cancel invalid"),
        CancelQueuedResult {
            outcome: CancelQueuedOutcome::NotFound,
        }
    );

    controller.abort(BACKGROUND_CONTEXT.clone()).await.expect("abort");
    let view = conv
        .conversation
        .view_state(BACKGROUND_CONTEXT.clone())
        .await
        .expect("view after abort");
    let live_has_run = view
        .value()
        .docs
        .get("pi.live")
        .and_then(|v| v.get("run"))
        .is_some_and(|v| !v.is_null());
    assert!(!live_has_run);
    view.dispose();

    let first_op = first.operation_id.expect("first op id");
    let wait_first = controller
        .wait_for_prompt(&first_op, BACKGROUND_CONTEXT.clone())
        .await
        .expect("wait first");
    assert!(matches!(wait_first, AgentPromptResult::Unanswered { text: None, .. }));

    conv.close().await.expect("close");
    let _: Option<ServiceObject> = None;
}

#[tokio::test]
async fn starts_a_compaction_task() {
    let conv = open_faux_conversation(Vec::new()).await.expect("open faux");
    let controller = create_agent_controller(conv.harness.clone(), conv.conversation.clone());
    let res = controller
        .compact(
            AgentCompactionRequest {
                custom_instructions: Some("short".to_string()),
            },
            BACKGROUND_CONTEXT.clone(),
        )
        .await
        .expect("compact");
    assert!(res.accepted);
    assert!(res.operation_id.is_some());
    assert!(res.error.is_none());
    conv.close().await.expect("close");
}
