//! Port of packages/coding-agent/src/experimental/services/agent-controller.ts

use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use indexmap::IndexMap;
use pi_chord::services::handle::{ServiceObject, ServiceValue};
use pi_chord::{Context, JsonValue, Service, define_service};
use serde::{Deserialize, Serialize};

use crate::experimental::services::connection::{decode_arg, invoke_typed, json_value_from_serde, make_async_fn};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentPromptImage {
    #[serde(rename = "type")]
    pub kind: String,
    pub data: String,
    pub mime_type: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentPromptRequest {
    pub message: String,
    pub images: Option<Vec<AgentPromptImage>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentOperationError {
    pub code: String,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentOperationResponse {
    pub accepted: bool,
    pub operation_id: Option<String>,
    pub error: Option<AgentOperationError>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentQueueResponse {
    pub accepted: bool,
    pub entry_id: Option<String>,
    pub error: Option<AgentOperationError>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancelQueuedOutcome {
    Cancelled,
    AlreadyConsumed,
    NotFound,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelQueuedResult {
    pub outcome: CancelQueuedOutcome,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum AgentPromptResult {
    Done { text: String, reason: Option<String> },
    Unanswered { text: Option<String>, reason: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCompactionRequest {
    pub custom_instructions: Option<String>,
}

#[async_trait]
pub trait AgentControllerApi: Send + Sync {
    async fn prompt(&self, request: AgentPromptRequest, context: Context) -> pi_js::Result<AgentOperationResponse>;
    async fn steer(&self, request: AgentPromptRequest, context: Context) -> pi_js::Result<AgentQueueResponse>;
    async fn follow_up(&self, request: AgentPromptRequest, context: Context) -> pi_js::Result<AgentQueueResponse>;
    async fn cancel_queued(&self, entry_id: String, context: Context) -> pi_js::Result<CancelQueuedResult>;
    async fn abort(&self, context: Context) -> pi_js::Result<()>;
    async fn compact(&self, request: AgentCompactionRequest, context: Context)
    -> pi_js::Result<AgentOperationResponse>;
    async fn wait_for_prompt(&self, operation_id: String, context: Context) -> pi_js::Result<AgentPromptResult>;
}

pub static AGENT_CONTROLLER_SERVICE: LazyLock<Service> = LazyLock::new(|| {
    define_service("pi.agent-controller", None).unwrap_or_else(|e| panic!("valid service definition: {e}"))
});

/// Client/caller facade over a Chord `pi.agent-controller` service object.
#[derive(Clone)]
pub struct AgentController {
    object: ServiceObject,
}

impl AgentController {
    pub fn from_service(object: ServiceObject) -> Self {
        Self { object }
    }

    pub fn into_service_object(self) -> ServiceObject {
        self.object
    }

    pub async fn prompt(&self, request: AgentPromptRequest, context: Context) -> pi_js::Result<AgentOperationResponse> {
        let arg = json_value_from_serde(&request)?;
        invoke_typed(&self.object, "prompt", &[arg], context).await
    }

    pub async fn steer(&self, request: AgentPromptRequest, context: Context) -> pi_js::Result<AgentQueueResponse> {
        let arg = json_value_from_serde(&request)?;
        invoke_typed(&self.object, "steer", &[arg], context).await
    }

    pub async fn follow_up(&self, request: AgentPromptRequest, context: Context) -> pi_js::Result<AgentQueueResponse> {
        let arg = json_value_from_serde(&request)?;
        invoke_typed(&self.object, "followUp", &[arg], context).await
    }

    pub async fn cancel_queued(&self, entry_id: &str, context: Context) -> pi_js::Result<CancelQueuedResult> {
        invoke_typed(
            &self.object,
            "cancelQueued",
            &[JsonValue::String(entry_id.to_string())],
            context,
        )
        .await
    }

    pub async fn abort(&self, context: Context) -> pi_js::Result<()> {
        let _ = crate::experimental::services::connection::invoke_remote_method(&self.object, "abort", &[], context)
            .await?;
        Ok(())
    }

    pub async fn compact(
        &self,
        request: AgentCompactionRequest,
        context: Context,
    ) -> pi_js::Result<AgentOperationResponse> {
        let arg = json_value_from_serde(&request)?;
        invoke_typed(&self.object, "compact", &[arg], context).await
    }

    pub async fn wait_for_prompt(&self, operation_id: &str, context: Context) -> pi_js::Result<AgentPromptResult> {
        invoke_typed(
            &self.object,
            "waitForPrompt",
            &[JsonValue::String(operation_id.to_string())],
            context,
        )
        .await
    }
}

pub fn agent_controller_to_service_object(api: Arc<dyn AgentControllerApi>) -> ServiceObject {
    let mut members: IndexMap<String, ServiceValue> = IndexMap::new();

    let api_p = Arc::clone(&api);
    members.insert(
        "prompt".to_string(),
        make_async_fn(move |args, context| {
            let req_res = decode_arg::<AgentPromptRequest>(args, 0, "request");
            let api = Arc::clone(&api_p);
            Box::pin(async move {
                let res = api.prompt(req_res?, context).await?;
                Ok(Some(json_value_from_serde(&res)?))
            })
        }),
    );

    let api_s = Arc::clone(&api);
    members.insert(
        "steer".to_string(),
        make_async_fn(move |args, context| {
            let req_res = decode_arg::<AgentPromptRequest>(args, 0, "request");
            let api = Arc::clone(&api_s);
            Box::pin(async move {
                let res = api.steer(req_res?, context).await?;
                Ok(Some(json_value_from_serde(&res)?))
            })
        }),
    );

    let api_f = Arc::clone(&api);
    members.insert(
        "followUp".to_string(),
        make_async_fn(move |args, context| {
            let req_res = decode_arg::<AgentPromptRequest>(args, 0, "request");
            let api = Arc::clone(&api_f);
            Box::pin(async move {
                let res = api.follow_up(req_res?, context).await?;
                Ok(Some(json_value_from_serde(&res)?))
            })
        }),
    );

    let api_c = Arc::clone(&api);
    members.insert(
        "cancelQueued".to_string(),
        make_async_fn(move |args, context| {
            let id_res = decode_arg::<String>(args, 0, "entryId");
            let api = Arc::clone(&api_c);
            Box::pin(async move {
                let res = api.cancel_queued(id_res?, context).await?;
                Ok(Some(json_value_from_serde(&res)?))
            })
        }),
    );

    let api_a = Arc::clone(&api);
    members.insert(
        "abort".to_string(),
        make_async_fn(move |_args, context| {
            let api = Arc::clone(&api_a);
            Box::pin(async move {
                api.abort(context).await?;
                Ok(None)
            })
        }),
    );

    let api_cmp = Arc::clone(&api);
    members.insert(
        "compact".to_string(),
        make_async_fn(move |args, context| {
            let req_res = decode_arg::<AgentCompactionRequest>(args, 0, "request");
            let api = Arc::clone(&api_cmp);
            Box::pin(async move {
                let res = api.compact(req_res?, context).await?;
                Ok(Some(json_value_from_serde(&res)?))
            })
        }),
    );

    let api_w = Arc::clone(&api);
    members.insert(
        "waitForPrompt".to_string(),
        make_async_fn(move |args, context| {
            let id_res = decode_arg::<String>(args, 0, "operationId");
            let api = Arc::clone(&api_w);
            Box::pin(async move {
                let res = api.wait_for_prompt(id_res?, context).await?;
                Ok(Some(json_value_from_serde(&res)?))
            })
        }),
    );

    ServiceObject::from_members(members)
}
