//! Port of packages/coding-agent/test/suite/regressions/7193-event-bus-lifecycle.test.ts

mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use pi_coding_agent::core::event_bus::create_event_bus;
use serde_json::Value;

use support::test_harness::{HarnessOptions, create_harness};
use support::utilities::{
    CreateTestResourceLoaderOptions, TestExtensionInput, create_test_extensions_result, create_test_resource_loader,
};

#[tokio::test(flavor = "current_thread")]
async fn removes_extension_owned_event_bus_listeners_on_reload_and_dispose() {
    let event_bus = create_event_bus();
    let extension_calls = Arc::new(AtomicUsize::new(0));
    let host_calls = Arc::new(AtomicUsize::new(0));

    let host_counter = host_calls.clone();
    let _host_unsub = event_bus.on_sync("reload:test", move |_| {
        host_counter.fetch_add(1, Ordering::SeqCst);
    });

    let bus_for_ext = event_bus.clone();
    let ext_counter = extension_calls.clone();
    let unsub_holder: Arc<std::sync::Mutex<Option<pi_js::Unsubscribe>>> = Arc::new(std::sync::Mutex::new(None));
    let unsub_ref = unsub_holder.clone();
    let extensions_res = create_test_extensions_result(
        vec![TestExtensionInput::Factory(Arc::new(move |_api| {
            let bus = bus_for_ext.clone();
            let c = ext_counter.clone();
            let unsub_box = unsub_ref.clone();
            Box::pin(async move {
                let unsub = bus.on_sync("reload:test", move |_| {
                    c.fetch_add(1, Ordering::SeqCst);
                });
                *unsub_box.lock().expect("lock") = Some(unsub);
                Ok(())
            })
        }))],
        None,
    )
    .await
    .expect("create extensions result");

    let resource_loader = create_test_resource_loader(Some(CreateTestResourceLoaderOptions {
        extensions_result: Some(extensions_res),
    }));
    let harness = create_harness(Some(HarnessOptions {
        resource_loader: Some(resource_loader),
        ..Default::default()
    }))
    .await
    .expect("create harness");

    let emit = || async {
        let ext_before = extension_calls.load(Ordering::SeqCst);
        let host_before = host_calls.load(Ordering::SeqCst);
        event_bus.emit("reload:test", Value::Null);
        tokio::task::yield_now().await;
        (
            extension_calls.load(Ordering::SeqCst) - ext_before,
            host_calls.load(Ordering::SeqCst) - host_before,
        )
    };

    assert_eq!(emit().await, (1, 1));

    // WIRE(@ca-cli-w3): once `AgentSession::{bind_extensions, reload, dispose}`
    // lands, test automatic extension-owned event-bus unsubscription on session reload and stale `firstApi.getCommands()` error.
    if let Some(unsub) = unsub_holder.lock().expect("lock").take() {
        unsub.call();
    }
    harness.session.dispose();
    assert_eq!(emit().await, (0, 1));

    harness.cleanup();
}
