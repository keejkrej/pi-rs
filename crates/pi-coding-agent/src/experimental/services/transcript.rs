//! Port of packages/coding-agent/src/experimental/services/transcript.ts

use std::sync::LazyLock;

use indexmap::IndexMap;
use pi_chord::services::handle::{ServiceObject, ServiceValue};
use pi_chord::{Service, define_service};
use pi_durable::ConversationView;

use crate::experimental::services::connection::TypedReplicatedState;

pub static TRANSCRIPT_SERVICE: LazyLock<Service> =
    LazyLock::new(|| define_service("pi.transcript", None).unwrap_or_else(|e| panic!("valid service definition: {e}")));

#[derive(Clone)]
pub struct Transcript {
    pub state: TypedReplicatedState<ConversationView>,
    object: ServiceObject,
}

impl Transcript {
    pub fn from_service(object: ServiceObject) -> pi_js::Result<Self> {
        let state = TypedReplicatedState::from_service_member(&object, "state")?;
        Ok(Self { state, object })
    }

    pub fn from_state(state: TypedReplicatedState<ConversationView>) -> Self {
        let mut members = IndexMap::new();
        members.insert("state".to_string(), ServiceValue::Object(state.to_service_object()));
        let object = ServiceObject::from_members(members);
        Self { state, object }
    }

    pub fn into_service_object(self) -> ServiceObject {
        self.object
    }
}
