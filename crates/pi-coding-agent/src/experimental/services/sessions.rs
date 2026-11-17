//! Port of packages/coding-agent/src/experimental/services/sessions.ts

use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use indexmap::IndexMap;
use pi_chord::services::handle::{ServiceObject, ServiceValue};
use pi_chord::{Context, JsonValue, Service, define_service};
use pi_protocol::ServerId;
use serde::{Deserialize, Serialize};

use crate::experimental::services::connection::{
    TypedReplicatedState, decode_arg, invoke_remote_method, invoke_typed, json_value_from_serde, make_async_fn,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionAddress {
    pub server_id: ServerId,
    pub session_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    pub server_id: ServerId,
    pub session_id: String,
    pub created_at: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCreateOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionDirectoryState {
    pub revision: i64,
    pub sessions: Vec<SessionSummary>,
}

pub static SESSION_DIRECTORY_SERVICE: LazyLock<Service> = LazyLock::new(|| {
    define_service("pi.session-directory", None).unwrap_or_else(|e| panic!("valid service definition: {e}"))
});

#[derive(Clone)]
pub struct SessionDirectory {
    pub state: TypedReplicatedState<SessionDirectoryState>,
    object: ServiceObject,
}

impl SessionDirectory {
    pub fn from_service(object: ServiceObject) -> pi_js::Result<Self> {
        let state = TypedReplicatedState::from_service_member(&object, "state")?;
        Ok(Self { state, object })
    }

    pub fn from_state(state: TypedReplicatedState<SessionDirectoryState>) -> Self {
        let mut members = IndexMap::new();
        members.insert("state".to_string(), ServiceValue::Object(state.to_service_object()));
        let object = ServiceObject::from_members(members);
        Self { state, object }
    }

    pub fn into_service_object(self) -> ServiceObject {
        self.object
    }
}

#[async_trait]
pub trait SessionManagementApi: Send + Sync {
    async fn create(&self, options: SessionCreateOptions, context: Context) -> pi_js::Result<SessionSummary>;
    async fn remove(&self, session_id: String, context: Context) -> pi_js::Result<()>;
    async fn attach(&self, session_id: String, context: Context) -> pi_js::Result<()>;
    async fn detach(&self, context: Context) -> pi_js::Result<()>;
}

pub static SESSION_MANAGEMENT_SERVICE: LazyLock<Service> = LazyLock::new(|| {
    define_service("pi.session-management", None).unwrap_or_else(|e| panic!("valid service definition: {e}"))
});

#[derive(Clone)]
pub struct SessionManagement {
    object: ServiceObject,
}

impl SessionManagement {
    pub fn from_service(object: ServiceObject) -> Self {
        Self { object }
    }

    pub fn into_service_object(self) -> ServiceObject {
        self.object
    }

    pub async fn create(&self, options: SessionCreateOptions, context: Context) -> pi_js::Result<SessionSummary> {
        let arg = json_value_from_serde(&options)?;
        invoke_typed(&self.object, "create", &[arg], context).await
    }

    pub async fn remove(&self, session_id: &str, context: Context) -> pi_js::Result<()> {
        let _ = invoke_remote_method(
            &self.object,
            "remove",
            &[JsonValue::String(session_id.to_string())],
            context,
        )
        .await?;
        Ok(())
    }

    pub async fn attach(&self, session_id: &str, context: Context) -> pi_js::Result<()> {
        let _ = invoke_remote_method(
            &self.object,
            "attach",
            &[JsonValue::String(session_id.to_string())],
            context,
        )
        .await?;
        Ok(())
    }

    pub async fn detach(&self, context: Context) -> pi_js::Result<()> {
        let _ = invoke_remote_method(&self.object, "detach", &[], context).await?;
        Ok(())
    }
}

pub fn session_management_to_service_object(api: Arc<dyn SessionManagementApi>) -> ServiceObject {
    let mut members: IndexMap<String, ServiceValue> = IndexMap::new();

    let api_c = Arc::clone(&api);
    members.insert(
        "create".to_string(),
        make_async_fn(move |args, context| {
            let opts_res = if args.is_empty() || matches!(args.first(), Some(ServiceValue::Undefined)) {
                Ok(SessionCreateOptions::default())
            } else {
                decode_arg::<SessionCreateOptions>(args, 0, "options")
            };
            let api = Arc::clone(&api_c);
            Box::pin(async move {
                let created = api.create(opts_res?, context).await?;
                Ok(Some(json_value_from_serde(&created)?))
            })
        }),
    );

    let api_r = Arc::clone(&api);
    members.insert(
        "remove".to_string(),
        make_async_fn(move |args, context| {
            let id_res = decode_arg::<String>(args, 0, "sessionId");
            let api = Arc::clone(&api_r);
            Box::pin(async move {
                api.remove(id_res?, context).await?;
                Ok(None)
            })
        }),
    );

    let api_a = Arc::clone(&api);
    members.insert(
        "attach".to_string(),
        make_async_fn(move |args, context| {
            let id_res = decode_arg::<String>(args, 0, "sessionId");
            let api = Arc::clone(&api_a);
            Box::pin(async move {
                api.attach(id_res?, context).await?;
                Ok(None)
            })
        }),
    );

    let api_d = Arc::clone(&api);
    members.insert(
        "detach".to_string(),
        make_async_fn(move |_args, context| {
            let api = Arc::clone(&api_d);
            Box::pin(async move {
                api.detach(context).await?;
                Ok(None)
            })
        }),
    );

    ServiceObject::from_members(members)
}
