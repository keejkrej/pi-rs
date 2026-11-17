//! Port of packages/coding-agent/src/experimental/services/models.ts

use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use indexmap::IndexMap;
use pi_ai::types::ModelThinkingLevel;
use pi_chord::services::handle::{ServiceObject, ServiceValue};
use pi_chord::{Context, JsonValue, Service, define_service};
use serde::{Deserialize, Serialize};

use crate::experimental::services::connection::{
    TypedReplicatedState, decode_arg, invoke_remote_method, invoke_typed, json_value_from_serde, make_async_fn,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRef {
    pub provider: String,
    pub model_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelSummary {
    pub provider: String,
    pub model_id: String,
    pub name: String,
    pub reasoning: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsCatalog {
    pub revision: i64,
    pub available_models: Vec<ModelSummary>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsConfiguration {
    pub model: Option<ModelRef>,
    pub thinking_level: ModelThinkingLevel,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum ModelsRefreshState {
    Idle,
    Refreshing,
    Done,
    Warning { errors: IndexMap<String, String> },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsState {
    pub catalog: ModelsCatalog,
    pub configuration: ModelsConfiguration,
    pub refresh: ModelsRefreshState,
}

#[async_trait]
pub trait ModelsApi: Send + Sync {
    fn state(&self) -> &TypedReplicatedState<ModelsState>;
    async fn cycle_thinking(&self, context: Context) -> pi_js::Result<()>;
    async fn get_thinking_levels(&self, context: Context) -> pi_js::Result<Vec<ModelThinkingLevel>>;
    async fn refresh(&self, context: Context) -> pi_js::Result<()>;
    async fn select(&self, model: ModelRef, context: Context) -> pi_js::Result<()>;
    async fn select_thinking(&self, level: ModelThinkingLevel, context: Context) -> pi_js::Result<()>;
}

pub static MODELS_SERVICE: LazyLock<Service> =
    LazyLock::new(|| define_service("pi.models", None).unwrap_or_else(|e| panic!("valid service definition: {e}")));

#[derive(Clone)]
pub struct Models {
    pub state: TypedReplicatedState<ModelsState>,
    object: ServiceObject,
}

impl Models {
    pub fn from_service(object: ServiceObject) -> pi_js::Result<Self> {
        let state = TypedReplicatedState::from_service_member(&object, "state")?;
        Ok(Self { state, object })
    }

    pub fn into_service_object(self) -> ServiceObject {
        self.object
    }

    pub async fn cycle_thinking(&self, context: Context) -> pi_js::Result<()> {
        let _ = invoke_remote_method(&self.object, "cycleThinking", &[], context).await?;
        Ok(())
    }

    pub async fn get_thinking_levels(&self, context: Context) -> pi_js::Result<Vec<ModelThinkingLevel>> {
        invoke_typed(&self.object, "getThinkingLevels", &[], context).await
    }

    pub async fn refresh(&self, context: Context) -> pi_js::Result<()> {
        let _ = invoke_remote_method(&self.object, "refresh", &[], context).await?;
        Ok(())
    }

    pub async fn select(&self, model: ModelRef, context: Context) -> pi_js::Result<()> {
        let arg = json_value_from_serde(&model)?;
        let _ = invoke_remote_method(&self.object, "select", &[arg], context).await?;
        Ok(())
    }

    pub async fn select_thinking(&self, level: ModelThinkingLevel, context: Context) -> pi_js::Result<()> {
        let arg = json_value_from_serde(&level)?;
        let _ = invoke_remote_method(&self.object, "selectThinking", &[arg], context).await?;
        Ok(())
    }
}

pub fn models_to_service_object(api: Arc<dyn ModelsApi>) -> ServiceObject {
    let mut members: IndexMap<String, ServiceValue> = IndexMap::new();
    members.insert(
        "state".to_string(),
        ServiceValue::Object(api.state().to_service_object()),
    );

    let api_c = Arc::clone(&api);
    members.insert(
        "cycleThinking".to_string(),
        make_async_fn(move |_args, context| {
            let api = Arc::clone(&api_c);
            Box::pin(async move {
                api.cycle_thinking(context).await?;
                Ok(None)
            })
        }),
    );

    let api_g = Arc::clone(&api);
    members.insert(
        "getThinkingLevels".to_string(),
        make_async_fn(move |_args, context| {
            let api = Arc::clone(&api_g);
            Box::pin(async move {
                let levels = api.get_thinking_levels(context).await?;
                Ok(Some(json_value_from_serde(&levels)?))
            })
        }),
    );

    let api_r = Arc::clone(&api);
    members.insert(
        "refresh".to_string(),
        make_async_fn(move |_args, context| {
            let api = Arc::clone(&api_r);
            Box::pin(async move {
                api.refresh(context).await?;
                Ok(None)
            })
        }),
    );

    let api_s = Arc::clone(&api);
    members.insert(
        "select".to_string(),
        make_async_fn(move |args, context| {
            let model_res = decode_arg::<ModelRef>(args, 0, "model");
            let api = Arc::clone(&api_s);
            Box::pin(async move {
                api.select(model_res?, context).await?;
                Ok(None)
            })
        }),
    );

    let api_st = Arc::clone(&api);
    members.insert(
        "selectThinking".to_string(),
        make_async_fn(move |args, context| {
            let level_res = decode_arg::<ModelThinkingLevel>(args, 0, "level");
            let api = Arc::clone(&api_st);
            Box::pin(async move {
                api.select_thinking(level_res?, context).await?;
                Ok(None)
            })
        }),
    );

    ServiceObject::from_members(members)
}

pub fn _unused_json(_: &JsonValue) {}
