//! Port of packages/coding-agent/src/experimental/vacation/runtime.ts

use pi_durable::harness::types::AgentExtensionsChange;

pub use crate::experimental::durable::runtime::{
    ConversationSummary, DurableController, DurableSessionMeta, DurableView, DurableViewSource, ModelSummary, Notice,
    NoticeLevel, OpenDurableOptions, OpenDurableResult, agent_of,
};
use crate::experimental::durable::runtime::{DurableProfileConfig, open_durable_with};
use crate::experimental::vacation::harness_setup::create_vacation_registry;
use crate::experimental::vacation::sessions::select_session;
use crate::experimental::vacation::vacation::VACATION;

pub async fn open_durable(options: OpenDurableOptions) -> pi_js::Result<OpenDurableResult> {
    open_durable_with(
        options,
        DurableProfileConfig {
            select_location: |cwd, cont| Box::pin(async move { select_session(&cwd, cont).await }),
            build_registry: |_settings, _cwd| create_vacation_registry(),
            build_env: None,
            root_extensions: Some(AgentExtensionsChange::Replace(vec![VACATION.name.clone()])),
            root_cwd: false,
        },
    )
    .await
}
