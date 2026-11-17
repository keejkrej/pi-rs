//! Port of packages/coding-agent/src/experimental/services/slash-commands.ts

use std::any::Any;
use std::sync::{Arc, LazyLock};

use indexmap::IndexMap;
use pi_chord::api::DefineServiceOptions;
use pi_chord::services::handle::{PropertyKey, ServiceObject, ServiceTarget, ServiceValue};
use pi_chord::{Context, Service, define_service};
use pi_js::{BoxFuture, Unsubscribe};
use serde::{Deserialize, Serialize};

use crate::experimental::services::agent_controller::{AgentOperationResponse, AgentQueueResponse};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SlashCommandCompletion {
    pub value: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SlashCommandRunResult {
    Operation(AgentOperationResponse),
    Queue(AgentQueueResponse),
}

pub type SlashCommandCompletionsFn =
    Arc<dyn Fn(&str) -> BoxFuture<pi_js::Result<Option<Vec<SlashCommandCompletion>>>> + Send + Sync>;

pub type SlashCommandRunFn =
    Arc<dyn Fn(String, Context) -> BoxFuture<pi_js::Result<Option<SlashCommandRunResult>>> + Send + Sync>;

#[derive(Clone)]
pub struct SlashCommandContribution {
    pub name: String,
    pub description: String,
    pub argument_hint: Option<String>,
    pub get_argument_completions: Option<SlashCommandCompletionsFn>,
    pub run: SlashCommandRunFn,
}

pub type SlashCommandsListener = Arc<dyn Fn(&[SlashCommandContribution]) + Send + Sync>;

pub trait SlashCommandsApi: Send + Sync {
    fn register(&self, command: SlashCommandContribution) -> pi_js::Result<Unsubscribe>;
    fn replace(&self, command: SlashCommandContribution) -> pi_js::Result<Unsubscribe>;
    fn list(&self) -> Vec<SlashCommandContribution>;
    fn subscribe(&self, listener: SlashCommandsListener) -> pi_js::Result<Unsubscribe>;
}

pub static SLASH_COMMANDS_SERVICE: LazyLock<Service> = LazyLock::new(|| {
    define_service(
        "pi.local.slash-commands",
        Some(DefineServiceOptions { local: Some(true) }),
    )
    .unwrap_or_else(|e| panic!("valid service definition: {e}"))
});

pub(crate) struct ContributionTarget(pub SlashCommandContribution);

impl ServiceTarget for ContributionTarget {
    fn get(&self, _property: &PropertyKey) -> pi_js::Result<ServiceValue> {
        Ok(ServiceValue::Undefined)
    }

    fn apply(&self, _this_arg: Option<&ServiceObject>, _args: &[ServiceValue]) -> pi_js::Result<ServiceValue> {
        Err(pi_js::Error::js(
            "TypeError",
            "SlashCommandContribution is not callable",
        ))
    }

    fn as_any(&self) -> Option<&(dyn Any + Send + Sync)> {
        Some(self)
    }
}

pub(crate) struct ContributionListTarget(pub Vec<SlashCommandContribution>);

impl ServiceTarget for ContributionListTarget {
    fn get(&self, _property: &PropertyKey) -> pi_js::Result<ServiceValue> {
        Ok(ServiceValue::Undefined)
    }

    fn apply(&self, _this_arg: Option<&ServiceObject>, _args: &[ServiceValue]) -> pi_js::Result<ServiceValue> {
        Err(pi_js::Error::js("TypeError", "SlashCommandList is not callable"))
    }

    fn as_any(&self) -> Option<&(dyn Any + Send + Sync)> {
        Some(self)
    }
}

pub(crate) struct ListenerTarget(pub SlashCommandsListener);

impl ServiceTarget for ListenerTarget {
    fn get(&self, _property: &PropertyKey) -> pi_js::Result<ServiceValue> {
        Ok(ServiceValue::Undefined)
    }

    fn apply(&self, _this_arg: Option<&ServiceObject>, _args: &[ServiceValue]) -> pi_js::Result<ServiceValue> {
        Err(pi_js::Error::js("TypeError", "SlashCommandsListener is not callable"))
    }

    fn as_any(&self) -> Option<&(dyn Any + Send + Sync)> {
        Some(self)
    }
}

pub(crate) struct UnsubscribeTarget(pub std::sync::Mutex<Option<Unsubscribe>>);

impl ServiceTarget for UnsubscribeTarget {
    fn get(&self, _property: &PropertyKey) -> pi_js::Result<ServiceValue> {
        Ok(ServiceValue::Undefined)
    }

    fn apply(&self, _this_arg: Option<&ServiceObject>, _args: &[ServiceValue]) -> pi_js::Result<ServiceValue> {
        if let Some(unsub) = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take() {
            unsub.call();
        }
        Ok(ServiceValue::Undefined)
    }

    fn is_callable(&self) -> bool {
        true
    }

    fn as_any(&self) -> Option<&(dyn Any + Send + Sync)> {
        Some(self)
    }
}

#[derive(Clone)]
pub struct SlashCommands {
    object: ServiceObject,
}

impl SlashCommands {
    pub fn from_service(object: ServiceObject) -> Self {
        Self { object }
    }

    pub fn into_service_object(self) -> ServiceObject {
        self.object
    }

    pub fn register(&self, command: SlashCommandContribution) -> pi_js::Result<Unsubscribe> {
        self.call_registration("register", command)
    }

    pub fn replace(&self, command: SlashCommandContribution) -> pi_js::Result<Unsubscribe> {
        self.call_registration("replace", command)
    }

    fn call_registration(&self, member: &str, command: SlashCommandContribution) -> pi_js::Result<Unsubscribe> {
        let ServiceValue::Object(method) = self.object.get_key(member)? else {
            return Err(pi_js::Error::js("TypeError", format!("Expected {member} method")));
        };
        let arg = ServiceValue::Object(ServiceObject::new(Arc::new(ContributionTarget(command))));
        let ServiceValue::Object(unsub_obj) = method.apply(None, &[arg])? else {
            return Err(pi_js::Error::js(
                "TypeError",
                format!("{member} did not return unsubscribe"),
            ));
        };
        Ok(Unsubscribe::new(move || {
            let _ = unsub_obj.apply(None, &[]);
        }))
    }

    pub fn list(&self) -> pi_js::Result<Vec<SlashCommandContribution>> {
        let ServiceValue::Object(method) = self.object.get_key("list")? else {
            return Err(pi_js::Error::js("TypeError", "Expected list method"));
        };
        let ServiceValue::Object(list_obj) = method.apply(None, &[])? else {
            return Err(pi_js::Error::js("TypeError", "list did not return commands"));
        };
        list_obj
            .downcast_ref::<ContributionListTarget>()
            .map(|w| w.0.clone())
            .ok_or_else(|| pi_js::Error::js("TypeError", "Invalid slash command list payload"))
    }

    pub fn subscribe(&self, listener: SlashCommandsListener) -> pi_js::Result<Unsubscribe> {
        let ServiceValue::Object(method) = self.object.get_key("subscribe")? else {
            return Err(pi_js::Error::js("TypeError", "Expected subscribe method"));
        };
        let arg = ServiceValue::Object(ServiceObject::new(Arc::new(ListenerTarget(listener))));
        let ServiceValue::Object(unsub_obj) = method.apply(None, &[arg])? else {
            return Err(pi_js::Error::js("TypeError", "subscribe did not return unsubscribe"));
        };
        Ok(Unsubscribe::new(move || {
            let _ = unsub_obj.apply(None, &[]);
        }))
    }
}

pub fn slash_commands_to_service_object(api: Arc<dyn SlashCommandsApi>) -> ServiceObject {
    let mut members: IndexMap<String, ServiceValue> = IndexMap::new();

    let api_reg = Arc::clone(&api);
    members.insert(
        "register".to_string(),
        ServiceValue::Object(ServiceObject::from_fn(move |_this, args| {
            let Some(ServiceValue::Object(obj)) = args.first() else {
                return Err(pi_js::Error::js("TypeError", "Expected SlashCommandContribution"));
            };
            let Some(contrib) = obj.downcast_ref::<ContributionTarget>() else {
                return Err(pi_js::Error::js("TypeError", "Invalid SlashCommandContribution"));
            };
            let unsub = api_reg.register(contrib.0.clone())?;
            Ok(ServiceValue::Object(ServiceObject::new(Arc::new(UnsubscribeTarget(
                std::sync::Mutex::new(Some(unsub)),
            )))))
        })),
    );

    let api_rep = Arc::clone(&api);
    members.insert(
        "replace".to_string(),
        ServiceValue::Object(ServiceObject::from_fn(move |_this, args| {
            let Some(ServiceValue::Object(obj)) = args.first() else {
                return Err(pi_js::Error::js("TypeError", "Expected SlashCommandContribution"));
            };
            let Some(contrib) = obj.downcast_ref::<ContributionTarget>() else {
                return Err(pi_js::Error::js("TypeError", "Invalid SlashCommandContribution"));
            };
            let unsub = api_rep.replace(contrib.0.clone())?;
            Ok(ServiceValue::Object(ServiceObject::new(Arc::new(UnsubscribeTarget(
                std::sync::Mutex::new(Some(unsub)),
            )))))
        })),
    );

    let api_list = Arc::clone(&api);
    members.insert(
        "list".to_string(),
        ServiceValue::Object(ServiceObject::from_fn(move |_this, _args| {
            let commands = api_list.list();
            Ok(ServiceValue::Object(ServiceObject::new(Arc::new(
                ContributionListTarget(commands),
            ))))
        })),
    );

    let api_sub = Arc::clone(&api);
    members.insert(
        "subscribe".to_string(),
        ServiceValue::Object(ServiceObject::from_fn(move |_this, args| {
            let Some(ServiceValue::Object(obj)) = args.first() else {
                return Err(pi_js::Error::js("TypeError", "Expected listener"));
            };
            let Some(listener) = obj.downcast_ref::<ListenerTarget>() else {
                return Err(pi_js::Error::js("TypeError", "Invalid listener"));
            };
            let unsub = api_sub.subscribe(Arc::clone(&listener.0))?;
            Ok(ServiceValue::Object(ServiceObject::new(Arc::new(UnsubscribeTarget(
                std::sync::Mutex::new(Some(unsub)),
            )))))
        })),
    );

    ServiceObject::from_members(members)
}
