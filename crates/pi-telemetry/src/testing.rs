//! Port of packages/telemetry/src/testing/index.ts
#![doc(hidden)]

// @generated-mods begin (scaffold-owned, do not edit)
pub mod conformance;
pub mod types;
// @generated-mods end

pub use conformance::create_telemetry_adapter_conformance;
pub use types::{TelemetryAdapterConformanceCase, TelemetryAdapterFixture, TelemetryAdapterFixtureFactory};
