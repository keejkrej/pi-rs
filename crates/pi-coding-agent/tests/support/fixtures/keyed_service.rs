//! Port of packages/coding-agent/test/fixtures/keyed-service.ts

use std::sync::LazyLock;

use pi_chord::api::DefineServiceOptions;
use pi_chord::services::handle::{ServiceObject, ServiceValue};
use pi_chord::{Context, Service, define_service};
use serde::{Deserialize, Serialize};

use pi_coding_agent::experimental::services::connection::{TypedReplicatedState, invoke_remote_method};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyedProbeState {
    pub value: String,
}

pub static KEYED_PROBE_SERVICE: LazyLock<Service> = LazyLock::new(|| {
    define_service("test.keyed-probe", Some(DefineServiceOptions { local: Some(false) }))
        .unwrap_or_else(|e| panic!("valid service definition: {e}"))
});

#[derive(Clone)]
pub struct KeyedProbe {
    pub state: TypedReplicatedState<KeyedProbeState>,
    object: ServiceObject,
}

impl KeyedProbe {
    pub fn from_service(object: ServiceObject) -> pi_js::Result<Self> {
        let state = TypedReplicatedState::from_service_member(&object, "state")?;
        Ok(Self { state, object })
    }

    pub fn into_service_object(self) -> ServiceObject {
        self.object
    }

    pub async fn replace(&self, value: &str, context: Context) -> pi_js::Result<()> {
        let _ = invoke_remote_method(
            &self.object,
            "replace",
            &[serde_json::Value::String(value.to_string())],
            context,
        )
        .await?;
        Ok(())
    }

    pub async fn wait(&self, context: Context) -> pi_js::Result<()> {
        let _ = invoke_remote_method(&self.object, "wait", &[], context).await?;
        Ok(())
    }

    pub fn has_member(&self, name: &str) -> bool {
        !matches!(self.object.get_key(name), Ok(ServiceValue::Undefined) | Err(_))
    }
}
