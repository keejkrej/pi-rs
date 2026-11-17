//! Port of packages/coding-agent/src/experimental/services/plugins.ts

use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use indexmap::IndexMap;
use pi_chord::services::handle::{ServiceObject, ServiceValue};
use pi_chord::{Context, JsonValue, Service, define_service};
use serde::{Deserialize, Serialize};

use crate::experimental::services::connection::{
    decode_arg, invoke_remote_method, json_value_from_serde, make_async_fn,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrepareSessionPluginsRequest {
    pub session_id: String,
    pub package_paths: Option<Vec<String>>,
}

#[async_trait]
pub trait PresentationPluginsApi: Send + Sync {
    async fn prepare_session(
        &self,
        request: PrepareSessionPluginsRequest,
        context: Context,
    ) -> pi_js::Result<JsonValue>;
    async fn reload(&self, context: Context) -> pi_js::Result<JsonValue>;
}

pub static PRESENTATION_PLUGINS_SERVICE: LazyLock<Service> = LazyLock::new(|| {
    define_service("pi.presentation-plugins", None).unwrap_or_else(|e| panic!("valid service definition: {e}"))
});

#[derive(Clone)]
pub struct PresentationPlugins {
    object: ServiceObject,
}

impl PresentationPlugins {
    pub fn from_service(object: ServiceObject) -> Self {
        Self { object }
    }

    pub fn into_service_object(self) -> ServiceObject {
        self.object
    }

    pub async fn prepare_session(
        &self,
        request: PrepareSessionPluginsRequest,
        context: Context,
    ) -> pi_js::Result<JsonValue> {
        let arg = json_value_from_serde(&request)?;
        Ok(invoke_remote_method(&self.object, "prepareSession", &[arg], context)
            .await?
            .unwrap_or(JsonValue::Null))
    }

    pub async fn reload(&self, context: Context) -> pi_js::Result<JsonValue> {
        Ok(invoke_remote_method(&self.object, "reload", &[], context)
            .await?
            .unwrap_or(JsonValue::Null))
    }
}

pub fn presentation_plugins_to_service_object(api: Arc<dyn PresentationPluginsApi>) -> ServiceObject {
    let mut members: IndexMap<String, ServiceValue> = IndexMap::new();

    let api_p = Arc::clone(&api);
    members.insert(
        "prepareSession".to_string(),
        make_async_fn(move |args, context| {
            let req_res = decode_arg::<PrepareSessionPluginsRequest>(args, 0, "request");
            let api = Arc::clone(&api_p);
            Box::pin(async move { Ok(Some(api.prepare_session(req_res?, context).await?)) })
        }),
    );

    let api_r = Arc::clone(&api);
    members.insert(
        "reload".to_string(),
        make_async_fn(move |_args, context| {
            let api = Arc::clone(&api_r);
            Box::pin(async move { Ok(Some(api.reload(context).await?)) })
        }),
    );

    ServiceObject::from_members(members)
}

#[async_trait]
pub trait SessionPluginsApi: Send + Sync {
    async fn reload(&self, context: Context) -> pi_js::Result<()>;
}

pub static SESSION_PLUGINS_SERVICE: LazyLock<Service> = LazyLock::new(|| {
    define_service("pi.session-plugins", None).unwrap_or_else(|e| panic!("valid service definition: {e}"))
});

#[derive(Clone)]
pub struct SessionPlugins {
    object: ServiceObject,
}

impl SessionPlugins {
    pub fn from_service(object: ServiceObject) -> Self {
        Self { object }
    }

    pub fn into_service_object(self) -> ServiceObject {
        self.object
    }

    pub async fn reload(&self, context: Context) -> pi_js::Result<()> {
        let _ = invoke_remote_method(&self.object, "reload", &[], context).await?;
        Ok(())
    }
}

pub fn session_plugins_to_service_object(api: Arc<dyn SessionPluginsApi>) -> ServiceObject {
    let mut members: IndexMap<String, ServiceValue> = IndexMap::new();
    let api_r = Arc::clone(&api);
    members.insert(
        "reload".to_string(),
        make_async_fn(move |_args, context| {
            let api = Arc::clone(&api_r);
            Box::pin(async move {
                api.reload(context).await?;
                Ok(None)
            })
        }),
    );
    ServiceObject::from_members(members)
}
