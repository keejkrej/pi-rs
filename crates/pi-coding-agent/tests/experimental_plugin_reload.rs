//! Port of packages/coding-agent/test/experimental-plugin-reload.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use pi_chord::context::BACKGROUND_CONTEXT;
use pi_chord::{Facet, FacetLoader, LoadedFacets, ServiceCall, define_facet};
use pi_coding_agent::experimental::services::plugins::SESSION_PLUGINS_SERVICE;
use pi_coding_agent::experimental::services::worker::{
    CreateSessionWorkerServicesOptions, WorkerServiceScope, create_session_worker_services,
};

use support::experimental_durable_support::open_faux_conversation;

#[tokio::test]
async fn loads_and_cuts_over_a_fresh_session_facet_generation() {
    // PORT: §2.6 pure-Rust FacetLoader mirrors TS multi-generation plugin reload without JS bundling.
    let activations: Arc<Mutex<Vec<usize>>> = Arc::new(Mutex::new(Vec::new()));
    let disposals: Arc<Mutex<Vec<usize>>> = Arc::new(Mutex::new(Vec::new()));
    let generation = Arc::new(AtomicUsize::new(0));

    let facet_loader = FacetLoader::new(Arc::new({
        let activations = Arc::clone(&activations);
        let disposals = Arc::clone(&disposals);
        let generation = Arc::clone(&generation);
        move || {
            let current = generation.fetch_add(1, Ordering::AcqRel) + 1;
            let activations = Arc::clone(&activations);
            let disposals = Arc::clone(&disposals);
            Box::pin(async move {
                let act_for_facet = Arc::clone(&activations);
                let facet = define_facet(Facet::new(
                    "reloadable-session-plugin",
                    Arc::new(move |env| {
                        let act = Arc::clone(&act_for_facet);
                        env.on_activate(Arc::new(move || {
                            act.lock().expect("lock").push(current);
                            Box::pin(futures::future::ready(Ok(())))
                        }))
                    }),
                ));
                let disp = Arc::clone(&disposals);
                Ok(LoadedFacets::new(
                    vec![facet],
                    Arc::new(move || {
                        disp.lock().expect("lock").push(current);
                        Box::pin(futures::future::ready(Ok(())))
                    }),
                ))
            })
        }
    }));

    let conv = open_faux_conversation(Vec::new()).await.expect("open faux");
    let services = create_session_worker_services(CreateSessionWorkerServicesOptions {
        harness: conv.harness.clone(),
        conversation: conv.conversation.clone(),
        model_runtime: None,
        settings_manager: None,
        facet_loader: Some(facet_loader),
        publish: Arc::new(|_, _, _| Box::pin(futures::future::ready(Ok(())))),
    })
    .await
    .expect("create worker services");

    assert_eq!(*activations.lock().expect("lock"), vec![1]);

    services
        .invoke(
            &ServiceCall {
                service_id: SESSION_PLUGINS_SERVICE.id.clone(),
                instance: None,
                member: "reload".to_string(),
                args: Vec::new(),
            },
            WorkerServiceScope {
                server_connection_id: "server-1".to_string(),
                attachment_id: "attachment-1".to_string(),
            },
            BACKGROUND_CONTEXT.clone(),
        )
        .await
        .expect("reload");

    assert_eq!(*activations.lock().expect("lock"), vec![1, 2]);
    assert_eq!(*disposals.lock().expect("lock"), vec![1]);

    services.dispose().await.expect("dispose services");
    conv.close().await.expect("close");
    assert_eq!(*disposals.lock().expect("lock"), vec![1, 2]);
}
