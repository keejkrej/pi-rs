//! Port of packages/telemetry/test/propagation-worker.ts (fixture process entry)

use std::io::Write;
use std::sync::Arc;

use pi_telemetry::{InMemoryTelemetryContext, SpanOptions, SpanValue, TelemetryCarrier, TelemetryPropagation};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    pi_js::ensure_crypto_provider();
    rt.block_on(async {
        let telemetry = InMemoryTelemetryContext::new();
        let arg = std::env::args().nth(1).unwrap_or_else(|| "{}".to_string());
        let carrier: TelemetryCarrier = pi_js::json::parse_as(&arg)?;
        let parent = telemetry.extract(Some(&carrier));
        parent
            .start_span(
                SpanOptions::new("remote"),
                Arc::new(|_span| Box::pin(async { Ok(Box::new(()) as SpanValue) })),
            )
            .await
            .map_err(|_| "remote span failed")?;
        let json = pi_js::json::stringify(&telemetry.get_spans());
        std::io::stdout().write_all(json.as_bytes())?;
        Ok(())
    })
}
