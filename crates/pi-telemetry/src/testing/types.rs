//! Port of packages/telemetry/src/testing/types.ts

use std::sync::Arc;

use async_trait::async_trait;
use pi_js::BoxFuture;

use crate::memory::{RecordedTelemetryMeasurement, RecordedTelemetrySpan};
use crate::metrics::TelemetryMetrics;
use crate::{Error, Result, TelemetryContext, TelemetryPropagation};

/// A fresh adapter instance and normalized snapshot reader owned by one conformance case.
#[async_trait]
pub trait TelemetryAdapterFixture: Send + Sync {
    fn context(&self) -> Arc<dyn TelemetryContext + Send + Sync>;

    fn propagation(&self) -> Option<Arc<dyn TelemetryPropagation + Send + Sync>> {
        None
    }

    fn metrics(&self) -> Option<Arc<dyn TelemetryMetrics + Send + Sync>> {
        None
    }

    async fn get_metrics(&self) -> Result<Vec<RecordedTelemetryMeasurement>> {
        Err(Error::msg("Metrics reader not implemented by fixture"))
    }

    async fn get_spans(&self) -> Result<Vec<RecordedTelemetrySpan>>;

    // PORT: `Symbol.asyncDispose`.
    async fn async_dispose(&self) -> Result<()>;
}

/// Creates an isolated adapter fixture for one conformance case.
pub type TelemetryAdapterFixtureFactory =
    Arc<dyn Fn() -> BoxFuture<Result<Box<dyn TelemetryAdapterFixture + Send + Sync>>> + Send + Sync>;

/// A runner-independent conformance case that can be registered with any test framework.
#[derive(Clone)]
pub struct TelemetryAdapterConformanceCase {
    pub group: String,
    pub name: String,
    run: Arc<dyn Fn() -> BoxFuture<Result<()>> + Send + Sync>,
}

impl TelemetryAdapterConformanceCase {
    pub fn new(
        group: impl Into<String>,
        name: impl Into<String>,
        run: Arc<dyn Fn() -> BoxFuture<Result<()>> + Send + Sync>,
    ) -> Self {
        Self {
            group: group.into(),
            name: name.into(),
            run,
        }
    }

    pub async fn run(&self) -> Result<()> {
        (self.run)().await
    }
}
