//! Port of packages/coding-agent/test/experimental-client-tui.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::IndexMap;
use pi_ai::{FauxAssistantMessageContent, FauxResponseStep, faux_assistant_message};
use pi_chord::context::BACKGROUND_CONTEXT;
use pi_chord::node::{
    FACET_BUNDLE_ARTIFACT_FORMAT, FACET_BUNDLE_ARTIFACT_FORMAT_VERSION, FacetBundleArtifact, FacetBundleEntry,
    FacetBundlePlugin,
};
use pi_chord::services::handle::ServiceObject;
use pi_chord::types::{RemoteServiceSourceOpenOptions, ServiceIdRef};
use pi_chord::{JsonValue, ServiceCall, ServiceUpdatePublisher};
use pi_client::unix::{UnixTransportOptions, create_unix_transport_factory};
use pi_client::{Client, ClientOptions};
use pi_coding_agent::cli::experimental::commands::client::ClientCommand;
use pi_coding_agent::experimental::client_tui::{
    ClientTuiServer, ExperimentalClientTui, RunClientTuiOptions, prepare_client_session, run_client_tui,
};
use pi_coding_agent::experimental::client_tui_chat::{ExperimentalChatView, live_of};
use pi_coding_agent::experimental::plugins::bundled::{
    create_presentation_facet_data, create_presentation_facet_loaders,
};
use pi_coding_agent::experimental::services::agent_controller::{
    AGENT_CONTROLLER_SERVICE, AgentController, AgentPromptRequest, AgentPromptResult,
};
use pi_coding_agent::experimental::services::connection::{
    ServiceSourceOptions, SessionAttachmentState, create_server_service_source, create_session_service_source,
};
use pi_coding_agent::experimental::services::models::{MODELS_SERVICE, Models};
use pi_coding_agent::experimental::services::server::{
    PreparedSessionPlugins, ServerServicesBackend, create_experimental_server_services,
};
use pi_coding_agent::experimental::services::sessions::SessionSummary;
use pi_coding_agent::experimental::services::transcript::{TRANSCRIPT_SERVICE, Transcript};
use pi_coding_agent::experimental::services::worker::{
    CreateSessionWorkerServicesOptions, SessionWorkerServices, WorkerServiceScope, create_session_worker_services,
};
use pi_coding_agent::experimental::session_catalog::SessionCatalogMetadata;
use pi_js::path::resolve;
use pi_server::transports::unix::{UnixServerOptions, create_unix_server};
use pi_server::{RoutedServerServiceHost, RoutedSessionAttachment, RoutedSessionHandle, ServerHost};
use tempfile::tempdir;

use support::experimental_durable_support::open_faux_conversation;

const SERVER_ID: &str = "00000000-0000-4000-8000-000000000001";

type PublisherMap = Arc<Mutex<IndexMap<String, ServiceUpdatePublisher>>>;

struct LoopbackSessionAttachment {
    services: Arc<SessionWorkerServices>,
    scope: WorkerServiceScope,
    publishers: PublisherMap,
}

#[async_trait]
impl RoutedSessionAttachment for LoopbackSessionAttachment {
    async fn invoke_service(
        &self,
        call: &ServiceCall,
        publish: ServiceUpdatePublisher,
        context: pi_chord::Context,
    ) -> pi_js::Result<Option<JsonValue>> {
        self.publishers
            .lock()
            .expect("lock")
            .insert(self.scope.attachment_id.clone(), publish);
        self.services.invoke(call, self.scope.clone(), context).await
    }

    async fn release(&self, _context: pi_chord::Context) -> pi_js::Result<()> {
        self.publishers
            .lock()
            .expect("lock")
            .shift_remove(&self.scope.attachment_id);
        Ok(())
    }
}

struct LoopbackSessionHandle {
    services: Arc<SessionWorkerServices>,
    session_id: String,
    publishers: PublisherMap,
}

#[async_trait]
impl RoutedSessionHandle for LoopbackSessionHandle {
    async fn attach_client(&self, _context: pi_chord::Context) -> pi_js::Result<Arc<dyn RoutedSessionAttachment>> {
        Ok(Arc::new(LoopbackSessionAttachment {
            services: Arc::clone(&self.services),
            scope: WorkerServiceScope {
                server_connection_id: SERVER_ID.to_string(),
                attachment_id: format!("att-{}", self.session_id),
            },
            publishers: Arc::clone(&self.publishers),
        }))
    }

    async fn close(&self, _context: pi_chord::Context) -> pi_js::Result<()> {
        self.services.dispose().await
    }
}

struct LoopbackServerHost {
    server_services_host: Arc<dyn RoutedServerServiceHost>,
    session_handle: Arc<LoopbackSessionHandle>,
}

#[async_trait]
impl ServerHost<SessionCatalogMetadata> for LoopbackServerHost {
    fn server_services(&self) -> Arc<dyn RoutedServerServiceHost> {
        Arc::clone(&self.server_services_host)
    }

    async fn resolve_session(
        &self,
        session_id: &str,
        _context: pi_chord::Context,
    ) -> pi_js::Result<SessionCatalogMetadata> {
        Ok(SessionCatalogMetadata {
            id: session_id.to_string(),
            created_at: 1,
            cwd: "/tmp".to_string(),
            path: format!("/tmp/{session_id}"),
        })
    }

    async fn open_session(
        &self,
        _metadata: SessionCatalogMetadata,
        _context: pi_chord::Context,
    ) -> pi_js::Result<Arc<dyn RoutedSessionHandle>> {
        Ok(Arc::clone(&self.session_handle) as Arc<dyn RoutedSessionHandle>)
    }
}

#[tokio::test]
async fn prepares_new_continued_and_plugin_selected_sessions_and_binds_session_services() {
    let cases = [
        (
            "new",
            ClientCommand {
                auth: None,
                connect: None,
                session_id: None,
                continue_: false,
                resume: false,
                provider: None,
                model: None,
                plugin_packages: Vec::new(),
                prompt: None,
            },
            "two",
            1usize,
            None,
        ),
        (
            "continued",
            ClientCommand {
                auth: None,
                connect: None,
                session_id: None,
                continue_: true,
                resume: false,
                provider: None,
                model: None,
                plugin_packages: Vec::new(),
                prompt: None,
            },
            "one",
            0usize,
            None,
        ),
        (
            "plugin-selected",
            ClientCommand {
                auth: None,
                connect: None,
                session_id: None,
                continue_: false,
                resume: false,
                provider: None,
                model: None,
                plugin_packages: vec!["./example-plugin".to_string()],
                prompt: None,
            },
            "two",
            1usize,
            Some(vec![resolve(&["./example-plugin"])]),
        ),
    ];

    for (kind, command, expected_session_id, expected_creates, expected_package_paths) in cases {
        let dir = tempdir().expect("tempdir");
        let sock_path = dir.path().join("server.sock").to_string_lossy().into_owned();

        let msg = faux_assistant_message(FauxAssistantMessageContent::Text("remote answer".to_string()), None);
        let conv = open_faux_conversation(vec![FauxResponseStep::Message(msg)])
            .await
            .expect("open faux");

        let sessions = Arc::new(Mutex::new(vec![SessionSummary {
            server_id: SERVER_ID.to_string(),
            session_id: "one".to_string(),
            created_at: 1,
        }]));
        let creates = Arc::new(Mutex::new(0usize));
        let prepare_calls = Arc::new(Mutex::new(Vec::<(String, Option<Vec<String>>)>::new()));

        let presentation_artifact = FacetBundleArtifact {
            format: FACET_BUNDLE_ARTIFACT_FORMAT.to_string(),
            format_version: FACET_BUNDLE_ARTIFACT_FORMAT_VERSION,
            plugin: FacetBundlePlugin {
                id: "test-tui-plugin".to_string(),
                version: None,
            },
            entry_name: "tui".to_string(),
            entry: FacetBundleEntry {
                file: "tui.cjs".to_string(),
                integrity: "sha256-deadbeef".to_string(),
                external_imports: vec!["@earendil-works/chord".to_string()],
                source_map: None,
            },
            source: "// unsupported js bundle placeholder".to_string(),
            source_map_contents: None,
        };
        let presentation_data = create_presentation_facet_data(&[presentation_artifact]);

        let server_services = create_experimental_server_services(ServerServicesBackend {
            list: {
                let sessions = Arc::clone(&sessions);
                Arc::new(move |_ctx| {
                    let list = sessions.lock().expect("lock").clone();
                    Box::pin(futures::future::ready(Ok(list)))
                })
            },
            create: {
                let sessions = Arc::clone(&sessions);
                let creates = Arc::clone(&creates);
                Arc::new(move |_opts, _ctx| {
                    *creates.lock().expect("lock") += 1;
                    let created = SessionSummary {
                        server_id: SERVER_ID.to_string(),
                        session_id: "two".to_string(),
                        created_at: 2,
                    };
                    sessions.lock().expect("lock").push(created.clone());
                    Box::pin(futures::future::ready(Ok(created)))
                })
            },
            remove: Arc::new(|_, _| Box::pin(futures::future::ready(Ok(())))),
            prepare_session_plugins: {
                let prepare_calls = Arc::clone(&prepare_calls);
                let data = presentation_data.clone();
                Arc::new(move |session_id, package_paths, _ctx| {
                    prepare_calls
                        .lock()
                        .expect("lock")
                        .push((session_id, package_paths.clone()));
                    Box::pin(futures::future::ready(Ok(PreparedSessionPlugins {
                        package_paths: package_paths.unwrap_or_default(),
                        presentation_plugins: data.clone(),
                    })))
                })
            },
            reload_presentation_plugins: {
                let data = presentation_data.clone();
                Arc::new(move |_, _| Box::pin(futures::future::ready(Ok(data.clone()))))
            },
        })
        .await
        .expect("create server services");

        let publishers: PublisherMap = Arc::new(Mutex::new(IndexMap::new()));
        let publishers_for_worker = Arc::clone(&publishers);
        let worker_services = Arc::new(
            create_session_worker_services(CreateSessionWorkerServicesOptions {
                harness: conv.harness.clone(),
                conversation: conv.conversation.clone(),
                model_runtime: None,
                settings_manager: None,
                facet_loader: None,
                publish: Arc::new(move |scope, subscription_id, update| {
                    let publisher = publishers_for_worker
                        .lock()
                        .expect("lock")
                        .get(&scope.attachment_id)
                        .cloned();
                    Box::pin(async move {
                        if let Some(pub_fn) = publisher {
                            pub_fn(subscription_id, update, BACKGROUND_CONTEXT.clone()).await?;
                        }
                        Ok(())
                    })
                }),
            })
            .await
            .expect("worker services"),
        );
        let session_handle = Arc::new(LoopbackSessionHandle {
            services: Arc::clone(&worker_services),
            session_id: expected_session_id.to_string(),
            publishers,
        });

        let server = create_unix_server(
            Arc::new(LoopbackServerHost {
                server_services_host: Arc::clone(&server_services.host),
                session_handle: Arc::clone(&session_handle),
            }),
            UnixServerOptions {
                server_id: SERVER_ID.to_string(),
                path: sock_path.clone(),
                mode: Some(0o600),
                max_pending_bytes: None,
                graceful_close_timeout_ms: None,
                max_frame_length: None,
                handshake_timeout_ms: None,
                on_connection_count_changed: None,
                on_error: None,
            },
        )
        .expect("unix server");
        server.start().await.expect("start unix server");

        let client = Client::connect_options(ClientOptions {
            server_id: SERVER_ID.to_string(),
            transport_factory: create_unix_transport_factory(UnixTransportOptions {
                path: sock_path,
                max_pending_bytes: None,
            })
            .expect("transport factory"),
            max_frame_length: None,
            on_listener_error: None,
        })
        .await
        .expect("client connect");

        let tui_server = ClientTuiServer {
            server_id: SERVER_ID.to_string(),
            radius: true,
            server: create_server_service_source(client.clone(), ServiceSourceOptions::default()).expect("server src"),
            session: create_session_service_source(client.clone(), ServiceSourceOptions::default())
                .expect("session src"),
        };

        let prepared = prepare_client_session(&command, &[tui_server.clone()])
            .await
            .expect("prepare client session");
        assert_eq!(prepared.summary.session_id, expected_session_id, "kind={kind}");
        assert_eq!(*creates.lock().expect("lock"), expected_creates, "kind={kind}");
        assert_eq!(
            *prepare_calls.lock().expect("lock"),
            vec![(expected_session_id.to_string(), expected_package_paths)],
            "kind={kind}"
        );
        assert_eq!(
            tui_server.session.attachment.value(),
            Some(SessionAttachmentState::Attached {
                session_id: expected_session_id.to_string(),
            })
        );

        // PORT: §2.6 presentation artifacts deserialize cleanly; invoking the JS bundle loader returns the canonical unsupported error.
        let loaders = create_presentation_facet_loaders(&prepared.presentation_plugins).expect("loaders");
        assert_eq!(loaders.len(), 1);
        let load_err = loaders[0]
            .load()
            .await
            .err()
            .expect("JS bundle load must error cleanly");
        assert!(
            load_err.message().contains("unsupported") || load_err.message().contains("not supported"),
            "unexpected bundle error: {}",
            load_err.message()
        );

        if kind == "new" {
            let view = conv
                .conversation
                .view_state(BACKGROUND_CONTEXT.clone())
                .await
                .expect("view");
            let view_val = view.value();
            assert!(live_of(&view_val).run.is_none());

            let session_remote = tui_server.session.open(RemoteServiceSourceOpenOptions {
                services: vec![
                    ServiceIdRef {
                        id: AGENT_CONTROLLER_SERVICE.id.clone(),
                    },
                    ServiceIdRef {
                        id: MODELS_SERVICE.id.clone(),
                    },
                    ServiceIdRef {
                        id: TRANSCRIPT_SERVICE.id.clone(),
                    },
                ],
                assert_access: Arc::new(|| Ok(())),
                on_error: Arc::new(|_| {}),
            });
            let controller = AgentController::from_service(
                session_remote
                    .r#use::<ServiceObject>(&*AGENT_CONTROLLER_SERVICE)
                    .expect("agent controller"),
            );
            let _models =
                Models::from_service(session_remote.r#use::<ServiceObject>(&*MODELS_SERVICE).expect("models"))
                    .expect("models wrap");
            let _transcript = Transcript::from_service(
                session_remote
                    .r#use::<ServiceObject>(&*TRANSCRIPT_SERVICE)
                    .expect("transcript"),
            )
            .expect("transcript wrap");
            session_remote
                .ready(BACKGROUND_CONTEXT.clone())
                .await
                .expect("session ready");

            let resp = controller
                .prompt(
                    AgentPromptRequest {
                        message: "hello".to_string(),
                        images: None,
                    },
                    BACKGROUND_CONTEXT.clone(),
                )
                .await
                .expect("prompt");
            assert!(resp.accepted);
            let op_id = resp.operation_id.expect("operation id");
            let result = controller
                .wait_for_prompt(&op_id, BACKGROUND_CONTEXT.clone())
                .await
                .expect("wait for prompt");
            assert_eq!(
                result,
                AgentPromptResult::Done {
                    text: "remote answer".to_string(),
                    reason: None,
                }
            );
            session_remote
                .dispose(BACKGROUND_CONTEXT.clone())
                .await
                .expect("dispose session remote");
        }

        let _ = client.dispose().await;
        let _ = server.close().await;
        let _ = session_handle.close(BACKGROUND_CONTEXT.clone()).await;
        let _ = server_services.dispose().await;
        let _ = conv.close().await;
    }
}

#[tokio::test]
async fn defers_interactive_tui_rendering_until_tui_lands() {
    // WIRE(@tui-w3): Once @tui lands `modes::interactive::*` (chat_viewport, components, theme,
    // tui_renderer), remove the early-return gates in `experimental::{client_tui, client_tui_chat}`
    // and port the component render/input/reload/model/thinking interaction tests.
    let err = ExperimentalClientTui::create(
        &ClientCommand {
            auth: None,
            connect: None,
            session_id: None,
            continue_: false,
            resume: false,
            provider: None,
            model: None,
            plugin_packages: Vec::new(),
            prompt: None,
        },
        &[],
    )
    .await
    .err()
    .expect("deferred until @tui-w3");
    assert!(err.message().contains("@tui-w3"));

    let view_err = ExperimentalChatView::new("/tmp").err().expect("deferred until @tui-w3");
    assert!(view_err.message().contains("@tui-w3"));

    let run_err = run_client_tui(
        &ClientCommand {
            auth: None,
            connect: None,
            session_id: None,
            continue_: false,
            resume: false,
            provider: None,
            model: None,
            plugin_packages: Vec::new(),
            prompt: None,
        },
        RunClientTuiOptions::default(),
    )
    .await
    .expect_err("deferred until @tui-w3");
    assert!(run_err.message().contains("@tui-w3"));
}
