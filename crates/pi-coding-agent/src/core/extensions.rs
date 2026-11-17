//! Port of packages/coding-agent/src/core/extensions/index.ts

// @generated-mods begin (scaffold-owned, do not edit)
pub mod jiti_loader;
pub mod jiti_static_loader;
pub mod loader;
pub mod runner;
pub mod types;
pub mod virtual_modules;
pub mod wrapper;
// @generated-mods end

pub use loader::{
    clear_extension_cache, create_extension_api, create_extension_runtime, discover_and_load_extensions,
    load_extension_from_factory, load_extensions, load_extensions_cached,
};
pub use runner::{
    BeforeAgentStartCombinedResult, BoundaryBaseEvent, BoundaryDispatchResult, DiscoveredResourcePathEntry,
    DiscoveredResourcesResult, EmitProjectTrustOutput, ExtensionRunner, ForkHandler, NavigateTreeHandler,
    NewSessionHandler, ProviderActions, ReloadHandler, RunnerEmitEvent, SessionBeforeEventResult, ShutdownHandler,
    SwitchSessionHandler, emit_project_trust_event, emit_session_shutdown_event, no_op_ui_context,
};
pub use types::*;
pub use wrapper::{wrap_registered_tool, wrap_registered_tools};
