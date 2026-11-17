//! Port of packages/coding-agent/test/suite/regressions/8423-extension-factory-failure.test.ts

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use async_trait::async_trait;
use futures::future::BoxFuture;
use pi_coding_agent::core::extensions::mcp_wire::{EventBus, create_event_bus}; // WIRE(@mcp-w3)
use pi_coding_agent::core::extensions::{
    Extension, ExtensionApi, ExtensionFactory, FlagOptions, FlagType, FlagValue, ProviderConfig,
    create_extension_runtime, load_extension_from_factory,
};
use serde_json::json;
use tokio::sync::Notify;

struct SyncFactory<F>(F);

#[async_trait]
impl<F> Extension for SyncFactory<F>
where
    F: Fn(&ExtensionApi) -> pi_js::Result<()> + Send + Sync + 'static,
{
    async fn register(&self, api: ExtensionApi) -> pi_js::Result<()> {
        (self.0)(&api)
    }
}

struct AsyncFactory<F>(F);

#[async_trait]
impl<F> Extension for AsyncFactory<F>
where
    F: Fn(ExtensionApi) -> BoxFuture<'static, pi_js::Result<()>> + Send + Sync + 'static,
{
    async fn register(&self, api: ExtensionApi) -> pi_js::Result<()> {
        (self.0)(api).await
    }
}

fn provider_config() -> ProviderConfig {
    ProviderConfig {
        base_url: Some("https://provider.test/v1".to_string()),
        api_key: Some("provider-test-key".to_string()),
        ..Default::default()
    }
}

#[tokio::test(flavor = "current_thread")]
async fn discards_runtime_changes_and_disables_the_failed_api() {
    let runtime = create_extension_runtime();
    let event_bus_ctrl = create_event_bus();
    let event_bus: Arc<dyn EventBus> = event_bus_ctrl.clone();
    let captured_api: Arc<std::sync::Mutex<Option<ExtensionApi>>> = Arc::new(std::sync::Mutex::new(None));
    let event_calls = Arc::new(AtomicUsize::new(0));
    let flag_during_load: Arc<std::sync::Mutex<Option<FlagValue>>> = Arc::new(std::sync::Mutex::new(None));

    let working: ExtensionFactory = Arc::new(SyncFactory(|pi: &ExtensionApi| {
        pi.register_provider("working-provider", provider_config())?;
        Ok(())
    }));
    load_extension_from_factory(working, ".", Arc::clone(&event_bus), runtime.clone(), Some("<working>"))
        .await
        .ok()
        .expect("working factory succeeds");

    let captured_api_clone = Arc::clone(&captured_api);
    let event_calls_clone = Arc::clone(&event_calls);
    let flag_clone = Arc::clone(&flag_during_load);
    let failing: ExtensionFactory = Arc::new(SyncFactory(move |pi: &ExtensionApi| {
        *captured_api_clone.lock().unwrap_or_else(|e| e.into_inner()) = Some(pi.clone());
        let calls = Arc::clone(&event_calls_clone);
        let _ = pi.events().on(
            "factory-failure",
            Arc::new(move |_| {
                calls.fetch_add(1, Ordering::SeqCst);
            }),
        );
        pi.register_flag(
            "failed-flag",
            FlagOptions {
                r#type: Some(FlagType::Boolean),
                description: None,
                default: Some(FlagValue::Bool(true)),
            },
        )?;
        *flag_clone.lock().unwrap_or_else(|e| e.into_inner()) = pi.get_flag("failed-flag")?;
        pi.unregister_provider("working-provider")?;
        pi.register_provider("failed-provider", provider_config())?;
        Err(pi_js::Error::msg("factory failed"))
    }));
    let err = load_extension_from_factory(failing, ".", Arc::clone(&event_bus), runtime.clone(), Some("<failing>"))
        .await
        .err()
        .expect("failing factory errors");
    assert!(err.to_string().contains("factory failed"), "unexpected error: {err}");

    event_bus_ctrl.emit("factory-failure", json!(null));
    assert_eq!(
        *flag_during_load.lock().unwrap_or_else(|e| e.into_inner()),
        Some(FlagValue::Bool(true))
    );
    assert!(
        !runtime
            .inner
            .flag_values
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key("failed-flag")
    );
    let pending: Vec<String> = runtime
        .inner
        .pending_provider_registrations
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .map(|r| r.name.clone())
        .collect();
    assert_eq!(pending, vec!["working-provider".to_string()]);
    assert_eq!(event_calls.load(Ordering::SeqCst), 0);

    let api = captured_api
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .expect("captured api");
    let late_err = api
        .register_flag(
            "late-flag",
            FlagOptions {
                r#type: Some(FlagType::Boolean),
                description: None,
                default: Some(FlagValue::Bool(true)),
            },
        )
        .expect_err("failed API should reject later calls");
    assert!(
        late_err
            .to_string()
            .contains("Extension \"<failing>\" failed to load and its API is no longer active."),
        "unexpected late error: {late_err}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn does_not_discard_concurrently_loaded_factory_provider() {
    let runtime = create_extension_runtime();
    let event_bus_ctrl = create_event_bus();
    let event_bus: Arc<dyn EventBus> = event_bus_ctrl.clone();
    let gate = Arc::new(Notify::new());
    let released = Arc::new(AtomicBool::new(false));
    let gate_wait = Arc::clone(&gate);
    let released_wait = Arc::clone(&released);

    let failing: ExtensionFactory = Arc::new(AsyncFactory(
        move |pi: ExtensionApi| -> BoxFuture<'static, pi_js::Result<()>> {
            let gate = Arc::clone(&gate_wait);
            let released = Arc::clone(&released_wait);
            Box::pin(async move {
                pi.register_provider("failed-provider", provider_config())?;
                while !released.load(Ordering::SeqCst) {
                    gate.notified().await;
                }
                Err(pi_js::Error::msg("factory failed"))
            })
        },
    ));
    let failing_fut =
        load_extension_from_factory(failing, ".", Arc::clone(&event_bus), runtime.clone(), Some("<failing>"));

    let working: ExtensionFactory = Arc::new(SyncFactory(|pi: &ExtensionApi| {
        pi.register_provider("working-provider", provider_config())?;
        Ok(())
    }));
    let working_fut =
        load_extension_from_factory(working, ".", Arc::clone(&event_bus), runtime.clone(), Some("<working>"));

    let (fail_res, work_res) = tokio::join!(failing_fut, async {
        let res = working_fut.await;
        released.store(true, Ordering::SeqCst);
        gate.notify_waiters();
        res
    });

    assert!(work_res.is_ok(), "working factory succeeds");
    let err = fail_res.err().expect("failing factory errors");
    assert!(err.to_string().contains("factory failed"), "unexpected error: {err}");

    let pending: Vec<String> = runtime
        .inner
        .pending_provider_registrations
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .map(|r| r.name.clone())
        .collect();
    assert_eq!(pending, vec!["working-provider".to_string()]);
}
