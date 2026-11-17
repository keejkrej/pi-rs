//! Port of packages/coding-agent/src/experimental/services/presentation-ui.ts

use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use indexmap::IndexMap;
use pi_chord::api::DefineServiceOptions;
use pi_chord::context::BACKGROUND_CONTEXT;
use pi_chord::services::handle::{ServiceObject, ServiceValue};
use pi_chord::{Context, Service, define_service};
use serde::{Deserialize, Serialize};

use crate::experimental::services::connection::{
    decode_arg, invoke_remote_method, json_value_from_serde, make_async_fn,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PresentationSelectItem {
    pub value: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[async_trait]
pub trait PresentationUiApi: Send + Sync {
    async fn select(
        &self,
        title: String,
        items: Vec<PresentationSelectItem>,
        selected_value: Option<String>,
        context: Context,
    ) -> pi_js::Result<Option<String>>;
    fn show_status(&self, message: String, context: Context);
}

pub static PRESENTATION_UI_SERVICE: LazyLock<Service> = LazyLock::new(|| {
    define_service(
        "pi.local.presentation-ui",
        Some(DefineServiceOptions { local: Some(true) }),
    )
    .unwrap_or_else(|e| panic!("valid service definition: {e}"))
});

#[derive(Clone)]
pub struct PresentationUI {
    object: ServiceObject,
}

impl PresentationUI {
    pub fn from_service(object: ServiceObject) -> Self {
        Self { object }
    }

    pub fn into_service_object(self) -> ServiceObject {
        self.object
    }

    pub async fn select(
        &self,
        title: &str,
        items: &[PresentationSelectItem],
        selected_value: Option<&str>,
        context: Context,
    ) -> pi_js::Result<Option<String>> {
        let items_json = json_value_from_serde(&items)?;
        let selected_json = match selected_value {
            Some(s) => serde_json::Value::String(s.to_string()),
            None => serde_json::Value::Null,
        };
        let result = invoke_remote_method(
            &self.object,
            "select",
            &[serde_json::Value::String(title.to_string()), items_json, selected_json],
            context,
        )
        .await?;
        match result {
            None | Some(serde_json::Value::Null) => Ok(None),
            Some(serde_json::Value::String(s)) => Ok(Some(s)),
            Some(other) => serde_json::from_value(other).map_err(|e| pi_js::Error::msg(e.to_string())),
        }
    }

    pub fn show_status(&self, message: &str, context: Context) -> pi_js::Result<()> {
        let ServiceValue::Object(method) = self.object.get_key("showStatus")? else {
            return Err(pi_js::Error::js("TypeError", "Expected showStatus method"));
        };
        let _ = method.apply(
            None,
            &[
                ServiceValue::String(message.to_string()),
                ServiceValue::Object(ServiceObject::from_context(context)),
            ],
        )?;
        Ok(())
    }
}

pub fn presentation_ui_to_service_object(api: Arc<dyn PresentationUiApi>) -> ServiceObject {
    let mut members: IndexMap<String, ServiceValue> = IndexMap::new();

    let api_s = Arc::clone(&api);
    members.insert(
        "select".to_string(),
        make_async_fn(move |args, context| {
            let title_res = decode_arg::<String>(args, 0, "title");
            let items_res = decode_arg::<Vec<PresentationSelectItem>>(args, 1, "items");
            let sel_res = match args.get(2) {
                None | Some(ServiceValue::Undefined) | Some(ServiceValue::Null) => Ok(None),
                Some(ServiceValue::String(s)) => Ok(Some(s.clone())),
                Some(_) => Err(pi_js::Error::js("TypeError", "Invalid selectedValue")),
            };
            let api = Arc::clone(&api_s);
            Box::pin(async move {
                let out = api.select(title_res?, items_res?, sel_res?, context).await?;
                Ok(out.map(serde_json::Value::String))
            })
        }),
    );

    let api_st = Arc::clone(&api);
    members.insert(
        "showStatus".to_string(),
        ServiceValue::Object(ServiceObject::from_fn(move |_this, args| {
            let message = match args.first() {
                Some(ServiceValue::String(s)) => s.clone(),
                _ => return Err(pi_js::Error::js("TypeError", "Invalid message")),
            };
            let ctx = args
                .get(1)
                .and_then(|v| match v {
                    ServiceValue::Object(o) => o.as_context(),
                    _ => None,
                })
                .unwrap_or_else(|| BACKGROUND_CONTEXT.clone());
            api_st.show_status(message, ctx);
            Ok(ServiceValue::Undefined)
        })),
    );

    ServiceObject::from_members(members)
}
