//! Port of packages/coding-agent/test/suite/regressions/3686-session-name-event.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::{Arc, Mutex, PoisonError};

use pi_coding_agent::core::agent_session::AgentSessionEvent;
use pi_coding_agent::core::extensions::types::{ExtensionApi, InlineExtension, SessionInfoChangedEvent};
use pretty_assertions::assert_eq;
use support::suite::harness::{HarnessOptions, create_harness};

#[tokio::test]
async fn emits_session_info_changed_when_agent_session_set_session_name_is_called() {
    let harness = create_harness(HarnessOptions::default()).await;

    harness.session.set_session_name("hello world");

    assert_eq!(
        harness.session_manager.get_session_name().as_deref(),
        Some("hello world")
    );
    let names: Vec<Option<String>> = harness
        .events()
        .into_iter()
        .filter_map(|e| match e {
            AgentSessionEvent::SessionInfoChanged { name } => Some(name),
            _ => None,
        })
        .collect();
    assert_eq!(names, vec![Some("hello world".to_string())]);
    harness.cleanup();
}

#[tokio::test]
async fn emits_session_info_changed_when_an_extension_calls_pi_set_session_name() {
    let api_slot = Arc::new(Mutex::new(Option::<ExtensionApi>::None));
    let slot_c = Arc::clone(&api_slot);
    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let s = Arc::clone(&slot_c);
        async move {
            *s.lock().unwrap_or_else(PoisonError::into_inner) = Some(pi);
            Ok(())
        }
    }));

    let harness = create_harness(HarnessOptions {
        extension_factories: Some(vec![ext]),
        ..Default::default()
    })
    .await;

    let pi = api_slot
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
        .expect("extension api");
    pi.set_session_name("from extension").unwrap();

    assert_eq!(
        harness.session_manager.get_session_name().as_deref(),
        Some("from extension")
    );
    let names: Vec<Option<String>> = harness
        .events()
        .into_iter()
        .filter_map(|e| match e {
            AgentSessionEvent::SessionInfoChanged { name } => Some(name),
            _ => None,
        })
        .collect();
    assert_eq!(names, vec![Some("from extension".to_string())]);
    harness.cleanup();
}

#[tokio::test]
async fn emits_session_info_changed_to_extensions() {
    let api_slot = Arc::new(Mutex::new(Option::<ExtensionApi>::None));
    let events = Arc::new(Mutex::new(Vec::<Option<String>>::new()));

    let slot_c = Arc::clone(&api_slot);
    let ev_c = Arc::clone(&events);
    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let s = Arc::clone(&slot_c);
        let ev = Arc::clone(&ev_c);
        async move {
            *s.lock().unwrap_or_else(PoisonError::into_inner) = Some(pi.clone());
            pi.on::<SessionInfoChangedEvent>(move |event, _ctx| {
                let ev = Arc::clone(&ev);
                Box::pin(async move {
                    ev.lock().unwrap_or_else(PoisonError::into_inner).push(event.name);
                    Ok(None)
                })
            });
            Ok(())
        }
    }));

    let harness = create_harness(HarnessOptions {
        extension_factories: Some(vec![ext]),
        ..Default::default()
    })
    .await;

    let pi = api_slot
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
        .expect("extension api");
    pi.set_session_name("first").unwrap();
    harness.session.set_session_name("second");

    assert_eq!(
        *events.lock().unwrap_or_else(PoisonError::into_inner),
        vec![Some("first".to_string()), Some("second".to_string())]
    );
    harness.cleanup();
}
