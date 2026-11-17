//! Port of packages/coding-agent/src/experimental/vacation/harness-setup.ts

use std::sync::Arc;

use pi_durable::Registry;
use pi_durable::create_registry;
use pi_durable::harness::registry::RegistryImpl;

pub use crate::experimental::durable::harness_setup::{
    CliModelSelection, InitialModel, configure_harness_http, create_harness_settings, find_initial_agent_model,
};
use crate::experimental::vacation::vacation::{SEARCH, VACATION};

/// A registry with the vacation planner and its research subagent's search. No coding tools, no pi prompt.
pub fn create_vacation_registry() -> Arc<RegistryImpl> {
    let registry = create_registry();
    let _ = registry.install(VACATION.clone());
    let _ = registry.install(SEARCH.clone());
    registry
}
