//! Port of packages/coding-agent/test/experimental-remote-runtime.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use indexmap::IndexSet;
use pi_chord::api::DefineServiceOptions;
use pi_chord::context::BACKGROUND_CONTEXT;
use pi_chord::node::{
    FACET_BUNDLE_MANIFEST_FILE, FacetBundleEntry, FacetBundleManifest, FacetBundlePluginInfo,
    write_facet_bundle_manifest,
};
use pi_chord::services::handle::ServiceObject;
use pi_chord::types::ServiceIdRef;
use pi_chord::{
    Facet, FacetOptions, Service, ServiceCall, ServiceMode, create_facet_host, create_static_facet_loader,
    define_facet, define_service,
};
use pi_client::unix::{UnixTransportOptions, create_unix_transport_factory};
use pi_client::{Client, ClientOptions};
use pi_coding_agent::cli::experimental::command_options::TransportAddress;
use pi_coding_agent::cli::experimental::commands::client::ClientCommand;
use pi_coding_agent::experimental::client::{ClientResult, RunClientOptions, run_client};
use pi_coding_agent::experimental::client_runtime::{
    OpenClientRuntimeOptions, activate_builtin_client_services, open_client_runtime,
};
use pi_coding_agent::experimental::plugins::bundled::{
    create_presentation_facet_data, create_presentation_facet_loaders, create_session_plugin_facet_loader,
};
use pi_coding_agent::experimental::plugins::package::{
    create_server_plugin_package, normalize_plugin_package_paths, read_session_plugin_package_profile,
    write_session_plugin_package_profile,
};
use pi_coding_agent::experimental::process::{
    INTERNAL_PROCESS_EXE_ENV, InternalProcessRole, InternalProcessSpawnHook, set_internal_process_spawn_hook,
    spawn_internal_process_direct,
};
use pi_coding_agent::experimental::server::{
    ENV_SERVER_DIR, ENV_SERVER_ID, RunningServer, StartServerOptions, start_server,
};
use pi_coding_agent::experimental::services::agent_controller::{
    AGENT_CONTROLLER_SERVICE, AgentController, AgentPromptRequest, AgentPromptResult,
};
use pi_coding_agent::experimental::services::connection::{
    ServiceSourceOptions, SessionAttachmentState, create_session_service_source, make_async_fn,
};
use pi_coding_agent::experimental::services::models::{MODELS_SERVICE, ModelRef, Models};
use pi_coding_agent::experimental::services::plugins::{
    PRESENTATION_PLUGINS_SERVICE, PrepareSessionPluginsRequest, SESSION_PLUGINS_SERVICE, SessionPlugins,
};
use pi_coding_agent::experimental::services::sessions::{
    SESSION_DIRECTORY_SERVICE, SESSION_MANAGEMENT_SERVICE, SessionAddress, SessionCreateOptions,
};
use pi_coding_agent::experimental::services::transcript::{TRANSCRIPT_SERVICE, Transcript};
use pi_coding_agent::experimental::services::worker::{
    CreateSessionWorkerServicesOptions, WorkerServiceScope, create_session_worker_services,
};
use pi_coding_agent::experimental::session_worker::SESSION_WORKER_ORPHAN_DEMAND_GRACE_ENV;
use pi_durable::harness::types::ModelRef as DurableModelRef;
use pi_js::fs::promises::{mkdir, readdir, write_file};
use pi_js::path::join;
use pi_protocol::{ProtocolError, ServerHelloError, ServerMessage, encode_server_message};
use serde_json::{Value, json};
use serial_test::serial;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use support::experimental_durable_support::open_faux_conversation;
use support::experimental_service_binding::{
    ServiceBindingOptions, create_server_service_binding, create_session_service_binding,
};
use support::experimental_session_support::{
    configure_experimental_worker_model, create_experimental_sessions, read_experimental_session_state,
};
use support::fixtures::keyed_service::{KEYED_PROBE_SERVICE, KeyedProbe};

const PROVIDER: &str = "anthropic";
const MODEL: &str = "claude-sonnet-4-5";

struct EnvRestore {
    saved: Vec<(&'static str, Option<String>)>,
}

impl EnvRestore {
    fn new() -> Self {
        Self { saved: Vec::new() }
    }

    fn set(&mut self, key: &'static str, value: &str) {
        if !self.saved.iter().any(|(k, _)| *k == key) {
            self.saved.push((key, pi_js::env::var(key)));
        }
        pi_js::env::set_var(key, value);
    }

    fn set_pi_bin(&mut self) {
        self.set(INTERNAL_PROCESS_EXE_ENV, env!("CARGO_BIN_EXE_pi"));
    }
}

impl Drop for EnvRestore {
    fn drop(&mut self) {
        for (key, original) in self.saved.iter().rev() {
            match original {
                Some(v) => pi_js::env::set_var(key, v),
                None => pi_js::env::remove_var(key),
            }
        }
    }
}

struct SpawnHookGuard(Option<InternalProcessSpawnHook>);

impl SpawnHookGuard {
    fn with_faux_worker() -> Self {
        let prev = set_internal_process_spawn_hook(Some(Arc::new(|role, args, options| {
            let mut adjusted = options.clone();
            if role == InternalProcessRole::SessionWorker {
                adjusted.entry_path = Some(env!("CARGO_BIN_EXE_fixture-faux-session-worker").to_string());
            }
            spawn_internal_process_direct(role, args, &adjusted)
        })));
        Self(prev)
    }
}

impl Drop for SpawnHookGuard {
    fn drop(&mut self) {
        set_internal_process_spawn_hook(self.0.take());
    }
}

struct TestEnv {
    _agent_temp: TempDir,
    agent_dir: String,
    _env: EnvRestore,
}

async fn setup_test_env() -> TestEnv {
    let agent_temp = tempfile::Builder::new()
        .prefix("pi-experimental-agent-")
        .tempdir()
        .expect("create agent tempdir");
    let agent_dir = agent_temp.path().to_string_lossy().into_owned();
    configure_experimental_worker_model(&agent_dir)
        .await
        .expect("configure worker auth");
    let mut env = EnvRestore::new();
    env.set_pi_bin();
    env.set("PI_CODING_AGENT_DIR", &agent_dir);
    let sessions_root = join(&[&agent_dir, "experimental", "sessions"]);
    create_experimental_sessions(&sessions_root, &["demo-1", "demo-2"], None)
        .await
        .expect("create demo sessions");
    TestEnv {
        _agent_temp: agent_temp,
        agent_dir,
        _env: env,
    }
}

async fn make_server(directory: &str) -> RunningServer {
    start_server(StartServerOptions {
        directory: Some(directory.to_string()),
        provider: Some(PROVIDER.to_string()),
        model: Some(MODEL.to_string()),
        ..Default::default()
    })
    .await
    .expect("start server")
}

async fn connect_unix_client(server_id: &str, socket_path: &str) -> Client {
    Client::connect_options(ClientOptions {
        server_id: server_id.to_string(),
        transport_factory: create_unix_transport_factory(UnixTransportOptions {
            path: socket_path.to_string(),
            max_pending_bytes: None,
        })
        .expect("transport factory"),
        max_frame_length: None,
        on_listener_error: None,
    })
    .await
    .expect("connect unix client")
}

async fn attach_session(client: &Client, session_id: &str) {
    let binding = create_server_service_binding(
        client.clone(),
        ServiceBindingOptions {
            services: vec![pi_chord::types::ServiceIdRef {
                id: SESSION_MANAGEMENT_SERVICE.id.clone(),
            }],
            on_error: None,
        },
    )
    .expect("create server binding");
    binding.ready(BACKGROUND_CONTEXT.clone()).await.expect("binding ready");
    let mgmt = pi_coding_agent::experimental::services::sessions::SessionManagement::from_service(
        binding
            .r#use::<ServiceObject>(&*SESSION_MANAGEMENT_SERVICE)
            .expect("session management"),
    );
    mgmt.attach(session_id, BACKGROUND_CONTEXT.clone())
        .await
        .expect("attach session");
    binding
        .dispose(BACKGROUND_CONTEXT.clone())
        .await
        .expect("dispose binding");
}

async fn attach_client(runtime: &RunningServer, session_id: &str) -> Client {
    let client = connect_unix_client(&runtime.server_id, &runtime.socket_path).await;
    attach_session(&client, session_id).await;
    client
}

async fn poll_until<F>(timeout_ms: u64, mut pred: F)
where
    F: FnMut() -> bool,
{
    let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        if pred() {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out after {timeout_ms}ms waiting for condition"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn path_exists(path: &str) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

#[cfg(unix)]
fn process_exists(pid: u32) -> bool {
    let Ok(raw) = i32::try_from(pid) else {
        return false;
    };
    matches!(
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(raw), None),
        Ok(()) | Err(nix::errno::Errno::EPERM)
    )
}

#[cfg(not(unix))]
fn process_exists(_pid: u32) -> bool {
    false
}

fn base_client_command() -> ClientCommand {
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
    }
}

#[tokio::test]
#[serial]
async fn uses_pi_server_dir_and_pi_server_id() {
    let mut env = setup_test_env().await;
    let dir = tempfile::Builder::new()
        .prefix("pi-server-dir-")
        .tempdir()
        .expect("server tempdir");
    let directory = dir.path().to_string_lossy().into_owned();
    let server_id = "00000000-0000-4000-8000-000000000001";
    env._env.set(ENV_SERVER_DIR, &directory);
    env._env.set(ENV_SERVER_ID, server_id);

    let runtime = start_server(StartServerOptions::default()).await.expect("start server");
    assert_eq!(runtime.server_id, server_id);
    assert_eq!(runtime.socket_path, join(&[&directory, &format!("{server_id}.sock")]));

    #[cfg(unix)]
    {
        use std::os::unix::fs::{FileTypeExt, PermissionsExt};
        let dir_meta = std::fs::symlink_metadata(&directory).expect("stat server dir");
        assert_eq!(dir_meta.permissions().mode() & 0o777, 0o700);
        let public_meta = std::fs::symlink_metadata(&runtime.socket_path).expect("stat public socket");
        let control_meta = std::fs::symlink_metadata(join(&[&directory, &format!("control-{server_id}.sock")]))
            .expect("stat control socket");
        assert!(public_meta.file_type().is_socket());
        assert_eq!(public_meta.permissions().mode() & 0o777, 0o600);
        assert!(control_meta.file_type().is_socket());
        assert_eq!(control_meta.permissions().mode() & 0o777, 0o600);
    }

    let entries = readdir(&directory).await.expect("readdir");
    assert_eq!(entries.len(), 3);
    assert!(entries.iter().all(|e| !e.starts_with('.')));
    assert!(entries.contains(&format!("{server_id}.sock")));
    assert!(entries.contains(&format!("control-{server_id}.sock")));
    let server_re = regex::Regex::new(&format!(r"^server-{server_id}-[0-9a-f]{{12}}\.sock$")).expect("regex");
    assert!(entries.iter().any(|e| server_re.is_match(e)));

    let res = run_client(&base_client_command(), RunClientOptions::default())
        .await
        .expect("run_client list");
    assert_eq!(
        res,
        ClientResult::List {
            sessions: vec![
                SessionAddress {
                    server_id: server_id.to_string(),
                    session_id: "demo-1".to_string(),
                },
                SessionAddress {
                    server_id: server_id.to_string(),
                    session_id: "demo-2".to_string(),
                },
            ],
        }
    );

    runtime.close().await.expect("close runtime");
    drop(env);
}

#[tokio::test]
#[serial]
async fn rejects_a_provider_without_a_model() {
    let _env = setup_test_env().await;
    let dir = tempfile::Builder::new().prefix("pes-").tempdir().expect("tempdir");
    let err = start_server(StartServerOptions {
        directory: Some(dir.path().to_string_lossy().into_owned()),
        provider: Some(PROVIDER.to_string()),
        model: None,
        ..Default::default()
    })
    .await
    .err()
    .expect("must reject provider without model");
    assert!(
        err.message().contains("provider requires a model"),
        "unexpected error: {}",
        err.message()
    );
}

#[tokio::test]
#[serial]
async fn preserves_an_existing_session_model_when_the_server_default_changes() {
    let env = setup_test_env().await;
    let settings_json = pi_js::json::stringify(&json!({
        "defaultProvider": "anthropic",
        "defaultModel": "claude-opus-4-6",
    }));
    write_file(
        &join(&[&env.agent_dir, "settings.json"]),
        settings_json.as_bytes(),
        None,
    )
    .await
    .expect("write settings");

    let dir = tempfile::Builder::new().prefix("pes-").tempdir().expect("tempdir");
    let directory = dir.path().to_string_lossy().into_owned();
    let first = start_server(StartServerOptions {
        directory: Some(directory.clone()),
        ..Default::default()
    })
    .await
    .expect("start first");
    let first_client = attach_client(&first, "demo-1").await;
    first_client.dispose().await.expect("dispose first client");
    poll_until(5_000, || !first.worker_pids().contains_key("demo-1")).await;
    first.close().await.expect("close first");

    let second = make_server(&directory).await;
    let second_client = attach_client(&second, "demo-1").await;
    second_client.dispose().await.expect("dispose second client");
    poll_until(5_000, || !second.worker_pids().contains_key("demo-1")).await;

    let state = read_experimental_session_state(&second.session_dir, "demo-1")
        .await
        .expect("read session state");
    assert_eq!(
        state.model,
        Some(DurableModelRef {
            provider: "anthropic".to_string(),
            model_id: "claude-opus-4-6".to_string(),
        })
    );
    second.close().await.expect("close second");
}

#[tokio::test]
#[serial]
async fn rejects_model_options_when_discovery_selects_an_existing_server() {
    let _env = setup_test_env().await;
    let dir = tempfile::Builder::new().prefix("pes-").tempdir().expect("tempdir");
    let directory = dir.path().to_string_lossy().into_owned();
    let runtime = make_server(&directory).await;

    let mut cmd = base_client_command();
    cmd.model = Some("anthropic/claude-opus-4-6".to_string());
    let err = run_client(
        &cmd,
        RunClientOptions {
            directory: Some(directory),
        },
    )
    .await
    .expect_err("must reject model on existing server");
    assert!(
        err.message()
            .contains("Model selection is only valid when automatically activating a new server"),
        "unexpected error: {}",
        err.message()
    );
    runtime.close().await.expect("close runtime");
}

#[cfg(unix)]
#[tokio::test]
#[serial]
async fn rechecks_an_auto_discovered_server_after_a_version_mismatch() {
    let _env = setup_test_env().await;
    let dir = tempfile::Builder::new().prefix("pe-ver-").tempdir().expect("tempdir");
    let directory = dir.path().to_string_lossy().into_owned();
    let server_id = "00000000-0000-4000-8000-000000000001";

    let sock_path = join(&[&directory, &format!("{server_id}.sock")]);
    let listener = tokio::net::UnixListener::bind(&sock_path).expect("bind stale version socket");
    let sock_cleanup = sock_path.clone();
    let stale_task = tokio::spawn(async move {
        if let Ok((mut stream, _)) = listener.accept().await {
            let mut buf = [0u8; 512];
            let _ = stream.read(&mut buf).await;
            let hello_err = encode_server_message(
                &ServerMessage::HelloError(ServerHelloError {
                    error: ProtocolError {
                        code: "version".to_string(),
                        message: "stale server".to_string(),
                    },
                }),
                None,
            )
            .expect("encode version error");
            let _ = stream.write_all(&hello_err).await;
            let _ = stream.shutdown().await;
        }
        drop(listener);
        let _ = std::fs::remove_file(&sock_cleanup);
    });

    let client_runtime = open_client_runtime(
        &base_client_command(),
        OpenClientRuntimeOptions {
            directory: Some(directory.clone()),
        },
    )
    .await
    .expect("open client runtime after version mismatch");
    let route_ids: Vec<String> = client_runtime
        .servers
        .iter()
        .map(|s| s.route.server_id().clone())
        .collect();
    assert_eq!(route_ids, vec![server_id.to_string()]);
    client_runtime.dispose().await.expect("dispose client runtime");
    let _ = stale_task.await;
    poll_until(5_000, || !path_exists(&sock_path)).await;
}

#[tokio::test]
#[serial]
async fn serializes_concurrent_cold_activation_and_retires_after_both_clients_leave() {
    let mut env = setup_test_env().await;
    let dir = tempfile::Builder::new()
        .prefix("pi-auto-server-")
        .tempdir()
        .expect("tempdir");
    let directory = dir.path().to_string_lossy().into_owned();
    let server_id = "00000000-0000-4000-8000-000000000001";
    env._env.set(ENV_SERVER_DIR, &directory);
    env._env.set(ENV_SERVER_ID, server_id);

    let cmd = base_client_command();
    let (res1, res2) = tokio::try_join!(
        run_client(&cmd, RunClientOptions::default()),
        run_client(&cmd, RunClientOptions::default()),
    )
    .expect("concurrent cold activation");

    let expected = ClientResult::List {
        sessions: vec![
            SessionAddress {
                server_id: server_id.to_string(),
                session_id: "demo-1".to_string(),
            },
            SessionAddress {
                server_id: server_id.to_string(),
                session_id: "demo-2".to_string(),
            },
        ],
    };
    assert_eq!(res1, expected);
    assert_eq!(res2, expected);

    let public_sock = join(&[&directory, &format!("{server_id}.sock")]);
    let control_sock = join(&[&directory, &format!("control-{server_id}.sock")]);
    assert!(path_exists(&public_sock));
    poll_until(15_000, || !path_exists(&public_sock)).await;
    poll_until(5_000, || !path_exists(&control_sock)).await;
    drop(env);
}

#[tokio::test]
#[serial]
async fn passes_client_plugin_packages_to_a_cold_server_and_restores_them_for_its_next_generation() {
    let mut env = setup_test_env().await;
    let dir = tempfile::Builder::new()
        .prefix("pi-auto-plugin-")
        .tempdir()
        .expect("tempdir");
    let directory = dir.path().to_string_lossy().into_owned();
    let server_id = "00000000-0000-4000-8000-000000000001";
    env._env.set(ENV_SERVER_DIR, &directory);
    env._env.set(ENV_SERVER_ID, server_id);

    // PORT: §2.6 JS/TS plugin bundling via esbuild is unsupported in pi-rs; verify canonical error
    // over cold activation AND pure-Rust profile persistence across server generations.
    let pkg_dir = join(&[&directory, "example-plugin"]);
    mkdir(&join(&[&pkg_dir, "src"]), true, None).await.expect("mkdir pkg");
    write_file(
        &join(&[&pkg_dir, "package.json"]),
        b"{\"name\":\"example-plugin\"}",
        None,
    )
    .await
    .expect("write package.json");
    write_file(&join(&[&pkg_dir, "src", "session.ts"]), b"export default {};\n", None)
        .await
        .expect("write session.ts");

    let mut cmd = base_client_command();
    cmd.provider = Some(PROVIDER.to_string());
    cmd.model = Some(MODEL.to_string());
    let first = open_client_runtime(&cmd, OpenClientRuntimeOptions::default())
        .await
        .expect("open first runtime");
    let activated = activate_builtin_client_services(&first.servers[0])
        .await
        .expect("activate builtins");
    let err = activated
        .plugins
        .prepare_session(
            PrepareSessionPluginsRequest {
                session_id: "demo-1".to_string(),
                package_paths: Some(vec![pkg_dir.clone()]),
            },
            BACKGROUND_CONTEXT.clone(),
        )
        .await
        .expect_err("JS bundle prepare_session must fail cleanly");
    assert!(
        err.message().contains("unsupported") || err.message().contains("not supported"),
        "unexpected error: {}",
        err.message()
    );
    first.dispose().await.expect("dispose first");

    let public_sock = join(&[&directory, &format!("{server_id}.sock")]);
    poll_until(15_000, || !path_exists(&public_sock)).await;

    let session_path = join(&[&env.agent_dir, "experimental", "sessions", "demo-1"]);
    write_session_plugin_package_profile(&directory, server_id, &session_path, &[pkg_dir.clone()])
        .await
        .expect("write session plugin profile");
    assert_eq!(
        read_session_plugin_package_profile(&directory, server_id, &session_path)
            .await
            .expect("read restored profile"),
        Some(vec![pkg_dir])
    );

    let artifact = pi_chord::node::FacetBundleArtifact {
        format: pi_chord::node::FACET_BUNDLE_ARTIFACT_FORMAT.to_string(),
        format_version: pi_chord::node::FACET_BUNDLE_ARTIFACT_FORMAT_VERSION,
        plugin: FacetBundlePluginInfo {
            id: "@earendil-works/pi-example-plugin".to_string(),
            version: None,
        },
        entry_name: "tui".to_string(),
        entry: FacetBundleEntry {
            file: "tui.cjs".to_string(),
            integrity: "sha256-deadbeef".to_string(),
            external_imports: Vec::new(),
        },
        source: "// placeholder".to_string(),
    };
    let presentation_data = create_presentation_facet_data(&[artifact]);
    assert_eq!(create_presentation_facet_loaders(&presentation_data).len(), 1);
}

#[tokio::test]
#[serial]
async fn retires_a_cold_server_after_its_only_session_attachment_disconnects() {
    let mut env = setup_test_env().await;
    let dir = tempfile::Builder::new()
        .prefix("pi-auto-session-")
        .tempdir()
        .expect("tempdir");
    let directory = dir.path().to_string_lossy().into_owned();
    let server_id = "00000000-0000-4000-8000-000000000001";
    env._env.set(ENV_SERVER_DIR, &directory);
    env._env.set(ENV_SERVER_ID, server_id);

    let mut cmd = base_client_command();
    cmd.session_id = Some("demo-1".to_string());
    cmd.provider = Some(PROVIDER.to_string());
    cmd.model = Some(MODEL.to_string());
    let res = run_client(&cmd, RunClientOptions::default())
        .await
        .expect("run_client attach");
    assert_eq!(
        res,
        ClientResult::Attached {
            server_id: server_id.to_string(),
            session_id: "demo-1".to_string(),
        }
    );

    let public_sock = join(&[&directory, &format!("{server_id}.sock")]);
    let control_sock = join(&[&directory, &format!("control-{server_id}.sock")]);
    poll_until(15_000, || !path_exists(&public_sock)).await;
    poll_until(5_000, || !path_exists(&control_sock)).await;
    drop(env);
}

#[tokio::test]
#[serial]
async fn runs_and_discovers_multiple_logical_servers_from_one_directory() {
    let _env = setup_test_env().await;
    let dir = tempfile::Builder::new()
        .prefix("pi-multi-server-")
        .tempdir()
        .expect("tempdir");
    let directory = dir.path().to_string_lossy().into_owned();
    let first_id = "00000000-0000-4000-8000-000000000001";
    let second_id = "00000000-0000-4000-8000-000000000002";

    let (first, second) = tokio::try_join!(
        start_server(StartServerOptions {
            directory: Some(directory.clone()),
            server_id: Some(first_id.to_string()),
            ..Default::default()
        }),
        start_server(StartServerOptions {
            directory: Some(directory.clone()),
            server_id: Some(second_id.to_string()),
            ..Default::default()
        }),
    )
    .expect("start two logical servers");

    assert!(path_exists(&join(&[&directory, &format!("control-{first_id}.sock")])));
    assert!(path_exists(&join(&[&directory, &format!("control-{second_id}.sock")])));

    let list1 = run_client(
        &base_client_command(),
        RunClientOptions {
            directory: Some(directory.clone()),
        },
    )
    .await
    .expect("list both");
    assert_eq!(
        list1,
        ClientResult::List {
            sessions: vec![
                SessionAddress {
                    server_id: first_id.to_string(),
                    session_id: "demo-1".to_string(),
                },
                SessionAddress {
                    server_id: first_id.to_string(),
                    session_id: "demo-2".to_string(),
                },
                SessionAddress {
                    server_id: second_id.to_string(),
                    session_id: "demo-1".to_string(),
                },
                SessionAddress {
                    server_id: second_id.to_string(),
                    session_id: "demo-2".to_string(),
                },
            ],
        }
    );

    first.close().await.expect("close first");
    poll_until(5_000, || !path_exists(&first.socket_path)).await;

    let list2 = run_client(
        &base_client_command(),
        RunClientOptions {
            directory: Some(directory),
        },
    )
    .await
    .expect("list second only");
    assert_eq!(
        list2,
        ClientResult::List {
            sessions: vec![
                SessionAddress {
                    server_id: second_id.to_string(),
                    session_id: "demo-1".to_string(),
                },
                SessionAddress {
                    server_id: second_id.to_string(),
                    session_id: "demo-2".to_string(),
                },
            ],
        }
    );

    second.close().await.expect("close second");
}

#[tokio::test]
#[serial]
async fn hydrates_and_mutates_server_session_services_across_framed_clients() {
    let _env = setup_test_env().await;
    let dir = tempfile::Builder::new().prefix("pes-").tempdir().expect("tempdir");
    let directory = dir.path().to_string_lossy().into_owned();
    let runtime = make_server(&directory).await;

    let first_client = connect_unix_client(&runtime.server_id, &runtime.socket_path).await;
    let second_client = connect_unix_client(&runtime.server_id, &runtime.socket_path).await;

    let errors = Arc::new(Mutex::new(Vec::<String>::new()));
    let mk_opts = |errs: Arc<Mutex<Vec<String>>>| ServiceBindingOptions {
        services: vec![
            pi_chord::types::ServiceIdRef {
                id: SESSION_DIRECTORY_SERVICE.id.clone(),
            },
            pi_chord::types::ServiceIdRef {
                id: SESSION_MANAGEMENT_SERVICE.id.clone(),
            },
        ],
        on_error: Some(Arc::new(move |err| {
            errs.lock().expect("lock").push(err.message().to_string());
        })),
    };

    let first_services = create_server_service_binding(first_client.clone(), mk_opts(Arc::clone(&errors)))
        .expect("first server binding");
    let second_services = create_server_service_binding(second_client.clone(), mk_opts(Arc::clone(&errors)))
        .expect("second server binding");

    assert!(matches!(
        first_services.connection.value(),
        Some(pi_coding_agent::experimental::services::connection::ServerConnectionState::Connected { .. })
    ));
    let catalogue = first_services
        .catalogue(BACKGROUND_CONTEXT.clone())
        .await
        .expect("catalogue");
    assert_eq!(
        catalogue,
        vec![
            pi_chord::ServiceCatalogueEntry {
                service_id: SESSION_DIRECTORY_SERVICE.id.clone(),
                mode: ServiceMode::Singleton,
            },
            pi_chord::ServiceCatalogueEntry {
                service_id: SESSION_MANAGEMENT_SERVICE.id.clone(),
                mode: ServiceMode::Singleton,
            },
            pi_chord::ServiceCatalogueEntry {
                service_id: PRESENTATION_PLUGINS_SERVICE.id.clone(),
                mode: ServiceMode::Singleton,
            },
        ]
    );

    let first_dir = pi_coding_agent::experimental::services::sessions::SessionDirectory::from_service(
        first_services
            .r#use::<ServiceObject>(&*SESSION_DIRECTORY_SERVICE)
            .expect("dir1"),
    )
    .expect("wrap dir1");
    let second_dir = pi_coding_agent::experimental::services::sessions::SessionDirectory::from_service(
        second_services
            .r#use::<ServiceObject>(&*SESSION_DIRECTORY_SERVICE)
            .expect("dir2"),
    )
    .expect("wrap dir2");
    let first_mgmt = pi_coding_agent::experimental::services::sessions::SessionManagement::from_service(
        first_services
            .r#use::<ServiceObject>(&*SESSION_MANAGEMENT_SERVICE)
            .expect("mgmt1"),
    );
    let second_mgmt = pi_coding_agent::experimental::services::sessions::SessionManagement::from_service(
        second_services
            .r#use::<ServiceObject>(&*SESSION_MANAGEMENT_SERVICE)
            .expect("mgmt2"),
    );

    tokio::try_join!(
        first_services.ready(BACKGROUND_CONTEXT.clone()),
        second_services.ready(BACKGROUND_CONTEXT.clone()),
    )
    .expect("both server bindings ready");

    let ids1: Vec<String> = first_dir
        .state
        .value()
        .expect("state1")
        .sessions
        .into_iter()
        .map(|s| s.session_id)
        .collect();
    assert_eq!(ids1, vec!["demo-1".to_string(), "demo-2".to_string()]);
    assert_eq!(second_dir.state.value(), first_dir.state.value());

    first_mgmt
        .create(
            SessionCreateOptions {
                id: Some("demo-3".to_string()),
            },
            BACKGROUND_CONTEXT.clone(),
        )
        .await
        .expect("create demo-3");
    poll_until(5_000, || {
        let s1 = first_dir.state.value();
        let s2 = second_dir.state.value();
        s1.as_ref()
            .is_some_and(|st| st.sessions.iter().any(|s| s.session_id == "demo-3"))
            && s1 == s2
    })
    .await;

    tokio::try_join!(
        first_mgmt.attach("demo-1", BACKGROUND_CONTEXT.clone()),
        second_mgmt.attach("demo-1", BACKGROUND_CONTEXT.clone()),
    )
    .expect("attach both");
    assert_eq!(
        first_client.attachment().map(|a| a.session_id),
        Some("demo-1".to_string())
    );
    assert_eq!(
        second_client.attachment().map(|a| a.session_id),
        Some("demo-1".to_string())
    );

    first_mgmt
        .remove("demo-1", BACKGROUND_CONTEXT.clone())
        .await
        .expect("remove demo-1");
    poll_until(5_000, || {
        first_client.attachment().is_none()
            && second_client.attachment().is_none()
            && first_dir
                .state
                .value()
                .as_ref()
                .is_some_and(|st| !st.sessions.iter().any(|s| s.session_id == "demo-1"))
            && second_dir.state.value() == first_dir.state.value()
    })
    .await;

    second_mgmt.detach(BACKGROUND_CONTEXT.clone()).await.expect("detach");
    assert!(errors.lock().expect("lock").is_empty());

    let _ = tokio::try_join!(
        first_services.dispose(BACKGROUND_CONTEXT.clone()),
        second_services.dispose(BACKGROUND_CONTEXT.clone()),
    );
    let _ = first_client.dispose().await;
    let _ = second_client.dispose().await;
    runtime.close().await.expect("close runtime");
}

#[tokio::test]
#[serial]
async fn hydrates_and_updates_the_models_service_across_concurrent_framed_clients() {
    let _env = setup_test_env().await;
    let dir = tempfile::Builder::new().prefix("pes-").tempdir().expect("tempdir");
    let directory = dir.path().to_string_lossy().into_owned();
    let runtime = make_server(&directory).await;

    let first_client = attach_client(&runtime, "demo-1").await;
    let worker_pid = runtime.worker_pids().get("demo-1").copied();
    assert!(worker_pid.is_some());
    let second_client = attach_client(&runtime, "demo-1").await;
    assert_eq!(runtime.worker_pids().get("demo-1").copied(), worker_pid);

    let errors = Arc::new(Mutex::new(Vec::<String>::new()));
    let mk_opts = |errs: Arc<Mutex<Vec<String>>>| ServiceBindingOptions {
        services: vec![pi_chord::types::ServiceIdRef {
            id: MODELS_SERVICE.id.clone(),
        }],
        on_error: Some(Arc::new(move |err| {
            errs.lock().expect("lock").push(err.message().to_string());
        })),
    };
    let first_services = create_session_service_binding(first_client.clone(), mk_opts(Arc::clone(&errors)))
        .expect("first session binding");
    let second_services = create_session_service_binding(second_client.clone(), mk_opts(Arc::clone(&errors)))
        .expect("second session binding");
    let first_models =
        Models::from_service(first_services.r#use::<ServiceObject>(&*MODELS_SERVICE).expect("m1")).expect("wrap m1");
    let second_models =
        Models::from_service(second_services.r#use::<ServiceObject>(&*MODELS_SERVICE).expect("m2")).expect("wrap m2");

    tokio::try_join!(
        first_services.ready(BACKGROUND_CONTEXT.clone()),
        second_services.ready(BACKGROUND_CONTEXT.clone()),
    )
    .expect("models ready");
    assert_eq!(
        first_services.attachment.value(),
        Some(SessionAttachmentState::Attached {
            session_id: "demo-1".to_string(),
        })
    );
    assert_eq!(
        second_services.attachment.value(),
        Some(SessionAttachmentState::Attached {
            session_id: "demo-1".to_string(),
        })
    );
    assert_eq!(
        first_models.state.value().expect("state1").configuration.model,
        Some(ModelRef {
            provider: PROVIDER.to_string(),
            model_id: MODEL.to_string(),
        })
    );
    assert_eq!(second_models.state.value(), first_models.state.value());

    let previous_thinking = first_models.state.value().expect("state1").configuration.thinking_level;
    first_models
        .cycle_thinking(BACKGROUND_CONTEXT.clone())
        .await
        .expect("cycle thinking");
    poll_until(5_000, || {
        let s1 = first_models.state.value();
        let s2 = second_models.state.value();
        s1.as_ref()
            .is_some_and(|st| st.configuration.thinking_level != previous_thinking)
            && s1 == s2
    })
    .await;
    assert!(errors.lock().expect("lock").is_empty());

    let _ = tokio::try_join!(
        first_services.dispose(BACKGROUND_CONTEXT.clone()),
        second_services.dispose(BACKGROUND_CONTEXT.clone()),
    );
    let _ = first_client.dispose().await;
    let _ = second_client.dispose().await;
    runtime.close().await.expect("close runtime");
}

#[tokio::test]
#[serial]
async fn loads_conventional_session_facets_from_multiple_configured_plugin_packages() {
    let _env = setup_test_env().await;
    let dir = tempfile::Builder::new()
        .prefix("pes-plugin-")
        .tempdir()
        .expect("tempdir");
    let directory = dir.path().to_string_lossy().into_owned();
    let first_pkg = join(&[&directory, "first-plugin"]);
    let second_pkg = join(&[&directory, "second-plugin"]);
    for pkg in [&first_pkg, &second_pkg] {
        mkdir(&join(&[pkg, "src"]), true, None).await.expect("mkdir pkg");
        write_file(&join(&[pkg, "package.json"]), b"{\"name\":\"test-plugin\"}", None)
            .await
            .expect("write pkg");
        write_file(&join(&[pkg, "src", "session.ts"]), b"export default {};\n", None)
            .await
            .expect("write session.ts");
    }

    // PORT: §2.6 JS/TS plugin package build rejects esbuild bundling cleanly, while pure-Rust
    // static facets compose multi-plugin services cleanly inside `SessionWorkerServices`.
    let normalized = normalize_plugin_package_paths(&[first_pkg.clone(), second_pkg.clone()]).expect("normalize");
    assert_eq!(normalized.len(), 2);
    let server_pkg = create_server_plugin_package(&directory, "00000000-0000-4000-8000-000000000001", &second_pkg);
    let err = server_pkg.build().await.expect_err("JS bundle build must reject");
    assert!(
        err.message().contains("unsupported") || err.message().contains("not supported"),
        "unexpected error: {}",
        err.message()
    );

    let mut entries = indexmap::IndexMap::new();
    entries.insert(
        "session".to_string(),
        FacetBundleEntry {
            file: "session.cjs".to_string(),
            integrity: "sha256-deadbeef".to_string(),
            external_imports: Vec::new(),
        },
    );
    let manifest_dir = join(&[&directory, "manifest-check"]);
    mkdir(&manifest_dir, true, None).await.expect("mkdir manifest");
    write_facet_bundle_manifest(
        &join(&[&manifest_dir, FACET_BUNDLE_MANIFEST_FILE]),
        &FacetBundleManifest {
            format: pi_chord::node::FACET_BUNDLE_MANIFEST_FORMAT.to_string(),
            format_version: pi_chord::node::FACET_BUNDLE_MANIFEST_FORMAT_VERSION,
            plugin: FacetBundlePluginInfo {
                id: "multi-plugin".to_string(),
                version: None,
            },
            entries,
        },
    )
    .await
    .expect("write manifest");
    let loader = create_session_plugin_facet_loader(&[join(&[&manifest_dir, FACET_BUNDLE_MANIFEST_FILE])])
        .expect("session loader");
    let manifest_err = loader.load().await.err().expect("JS session bundle load must reject");
    assert!(
        manifest_err.message().contains("unsupported") || manifest_err.message().contains("not supported"),
        "unexpected manifest error: {}",
        manifest_err.message()
    );

    let first_service = define_service("test.first-plugin", None).expect("define first");
    let second_service = define_service("test.second-plugin", None).expect("define second");
    let first_service_for_facet = first_service.clone();
    let second_service_for_facet = second_service.clone();
    let facet_a = define_facet(Facet::new(
        "first-session-plugin",
        Arc::new(move |env| {
            let mut members = indexmap::IndexMap::new();
            members.insert(
                "greet".to_string(),
                pi_chord::services::handle::ServiceValue::Object(make_async_fn(|_args, _ctx| {
                    Box::pin(async { Ok(Some(json!({ "workerActivations": 1 }))) })
                })),
            );
            env.provide(&first_service_for_facet, ServiceObject::from_members(members))
        }),
    ));
    let facet_b = define_facet(Facet::new(
        "second-session-plugin",
        Arc::new(move |env| {
            let mut members = indexmap::IndexMap::new();
            members.insert(
                "read".to_string(),
                pi_chord::services::handle::ServiceValue::Object(make_async_fn(|_args, _ctx| {
                    Box::pin(async { Ok(Some(Value::String("second".to_string()))) })
                })),
            );
            env.provide(&second_service_for_facet, ServiceObject::from_members(members))
        }),
    ));

    let conv = open_faux_conversation(Vec::new()).await.expect("open faux");
    let services = create_session_worker_services(CreateSessionWorkerServicesOptions {
        harness: conv.harness.clone(),
        conversation: conv.conversation.clone(),
        model_runtime: None,
        settings_manager: None,
        facet_loader: Some(create_static_facet_loader(vec![facet_a, facet_b])),
        publish: Arc::new(|_, _, _| Box::pin(futures::future::ready(Ok(())))),
    })
    .await
    .expect("worker services");
    let scope = WorkerServiceScope {
        server_connection_id: "conn-1".to_string(),
        attachment_id: "att-1".to_string(),
    };
    let greet_res = services
        .invoke(
            &ServiceCall {
                service_id: first_service.id,
                instance: None,
                member: "greet".to_string(),
                args: vec![json!({ "name": "Armin" })],
            },
            scope.clone(),
            BACKGROUND_CONTEXT.clone(),
        )
        .await
        .expect("invoke greet");
    assert_eq!(greet_res, Some(json!({ "workerActivations": 1 })));
    let read_res = services
        .invoke(
            &ServiceCall {
                service_id: second_service.id,
                instance: None,
                member: "read".to_string(),
                args: Vec::new(),
            },
            scope,
            BACKGROUND_CONTEXT.clone(),
        )
        .await
        .expect("invoke read");
    assert_eq!(read_res, Some(Value::String("second".to_string())));

    services.dispose().await.expect("dispose services");
    conv.close().await.expect("close conv");
}

#[tokio::test]
#[serial]
async fn uses_the_most_recently_selected_model_for_a_new_session() {
    let _env = setup_test_env().await;
    let dir = tempfile::Builder::new()
        .prefix("pes-model-default-")
        .tempdir()
        .expect("tempdir");
    let runtime = start_server(StartServerOptions {
        directory: Some(dir.path().to_string_lossy().into_owned()),
        ..Default::default()
    })
    .await
    .expect("start server");

    let first_client = attach_client(&runtime, "demo-1").await;
    let first_services = create_session_service_binding(
        first_client.clone(),
        ServiceBindingOptions {
            services: vec![pi_chord::types::ServiceIdRef {
                id: MODELS_SERVICE.id.clone(),
            }],
            on_error: None,
        },
    )
    .expect("first binding");
    let first_models =
        Models::from_service(first_services.r#use::<ServiceObject>(&*MODELS_SERVICE).expect("m1")).expect("wrap m1");
    first_services.ready(BACKGROUND_CONTEXT.clone()).await.expect("ready1");
    first_models
        .select(
            ModelRef {
                provider: PROVIDER.to_string(),
                model_id: "claude-opus-4-6".to_string(),
            },
            BACKGROUND_CONTEXT.clone(),
        )
        .await
        .expect("select opus");
    first_services
        .dispose(BACKGROUND_CONTEXT.clone())
        .await
        .expect("dispose s1");
    first_client.dispose().await.expect("dispose c1");
    poll_until(5_000, || !runtime.worker_pids().contains_key("demo-1")).await;

    let second_client = attach_client(&runtime, "demo-2").await;
    let second_services = create_session_service_binding(
        second_client.clone(),
        ServiceBindingOptions {
            services: vec![pi_chord::types::ServiceIdRef {
                id: MODELS_SERVICE.id.clone(),
            }],
            on_error: None,
        },
    )
    .expect("second binding");
    let second_models =
        Models::from_service(second_services.r#use::<ServiceObject>(&*MODELS_SERVICE).expect("m2")).expect("wrap m2");
    second_services.ready(BACKGROUND_CONTEXT.clone()).await.expect("ready2");
    assert_eq!(
        second_models.state.value().expect("state2").configuration.model,
        Some(ModelRef {
            provider: PROVIDER.to_string(),
            model_id: "claude-opus-4-6".to_string(),
        })
    );

    second_services
        .dispose(BACKGROUND_CONTEXT.clone())
        .await
        .expect("dispose s2");
    second_client.dispose().await.expect("dispose c2");
    runtime.close().await.expect("close runtime");
}

#[tokio::test]
#[serial]
async fn composes_management_attachment_with_session_service_hydration() {
    let _env = setup_test_env().await;
    let dir = tempfile::Builder::new().prefix("pes-").tempdir().expect("tempdir");
    let runtime = make_server(&dir.path().to_string_lossy()).await;

    let mut cmd = base_client_command();
    cmd.connect = Some(TransportAddress::Unix {
        path: runtime.socket_path.clone(),
    });
    let client_runtime = open_client_runtime(&cmd, OpenClientRuntimeOptions::default())
        .await
        .expect("open client runtime");
    let server = activate_builtin_client_services(&client_runtime.servers[0])
        .await
        .expect("activate builtins");
    server
        .plugins
        .prepare_session(
            PrepareSessionPluginsRequest {
                session_id: "demo-1".to_string(),
                package_paths: None,
            },
            BACKGROUND_CONTEXT.clone(),
        )
        .await
        .expect("prepare session");
    server
        .management
        .attach("demo-1", BACKGROUND_CONTEXT.clone())
        .await
        .expect("attach demo-1");

    assert_eq!(
        server.session.attachment.value(),
        Some(SessionAttachmentState::Attached {
            session_id: "demo-1".to_string(),
        })
    );
    assert_eq!(
        server.models.state.value().expect("models state").configuration.model,
        Some(ModelRef {
            provider: PROVIDER.to_string(),
            model_id: MODEL.to_string(),
        })
    );

    client_runtime.dispose().await.expect("dispose client runtime");
    runtime.close().await.expect("close runtime");
}

#[tokio::test]
#[serial]
async fn fences_superseded_attachment_hydration_by_attachment_generation() {
    let _env = setup_test_env().await;
    let dir = tempfile::Builder::new().prefix("pes-").tempdir().expect("tempdir");
    let runtime = make_server(&dir.path().to_string_lossy()).await;
    let client = connect_unix_client(&runtime.server_id, &runtime.socket_path).await;

    let errors = Arc::new(Mutex::new(Vec::<String>::new()));
    let services = create_session_service_binding(
        client.clone(),
        ServiceBindingOptions {
            services: vec![pi_chord::types::ServiceIdRef {
                id: MODELS_SERVICE.id.clone(),
            }],
            on_error: Some(Arc::new({
                let errors = Arc::clone(&errors);
                move |err| errors.lock().expect("lock").push(err.message().to_string())
            })),
        },
    )
    .expect("session binding");
    let models =
        Models::from_service(services.r#use::<ServiceObject>(&*MODELS_SERVICE).expect("models")).expect("wrap models");
    services.ready(BACKGROUND_CONTEXT.clone()).await.expect("ready");

    let states = Arc::new(Mutex::new(Vec::<SessionAttachmentState>::new()));
    let states_ref = Arc::clone(&states);
    let unsub = services
        .attachment
        .subscribe(move |state, _ctx, _del| {
            states_ref.lock().expect("lock").push(state);
            Box::pin(futures::future::ready(Ok(())))
        })
        .expect("subscribe attachment");

    // PORT: rapidly attach demo-2 then supersede with demo-1 before acting on demo-2's hydration.
    attach_session(&client, "demo-2").await;
    attach_session(&client, "demo-1").await;
    services
        .when_attached("demo-1", BACKGROUND_CONTEXT.clone())
        .await
        .expect("when attached demo-1");
    assert_eq!(
        services.attachment.value(),
        Some(SessionAttachmentState::Attached {
            session_id: "demo-1".to_string(),
        })
    );
    assert_eq!(
        models.state.value().expect("models state").configuration.model,
        Some(ModelRef {
            provider: PROVIDER.to_string(),
            model_id: MODEL.to_string(),
        })
    );

    unsub.call();
    let recorded = states.lock().expect("lock").clone();
    let latest_demo1 = recorded
        .iter()
        .rposition(|st| matches!(st, SessionAttachmentState::Attached { session_id } | SessionAttachmentState::Attaching { session_id } if session_id == "demo-1"))
        .expect("demo-1 transition observed");
    assert!(!recorded[latest_demo1..].iter().any(|st| {
        matches!(
            st,
            SessionAttachmentState::Attached { session_id } | SessionAttachmentState::Degraded { session_id }
                if session_id == "demo-2"
        )
    }));
    assert!(errors.lock().expect("lock").is_empty());

    services.dispose(BACKGROUND_CONTEXT.clone()).await.expect("dispose");
    client.dispose().await.expect("dispose client");
    runtime.close().await.expect("close runtime");
}

#[tokio::test]
#[serial]
async fn observes_keyed_service_instances_and_fences_replacement_generations_over_framed_transport() {
    let _env = setup_test_env().await;
    let _hook = SpawnHookGuard::with_faux_worker();
    let dir = tempfile::Builder::new().prefix("pes-").tempdir().expect("tempdir");
    let runtime = make_server(&dir.path().to_string_lossy()).await;
    let client = attach_client(&runtime, "demo-1").await;

    let errors = Arc::new(Mutex::new(Vec::<String>::new()));
    let services = create_session_service_source(
        client.clone(),
        ServiceSourceOptions {
            on_error: Some(Arc::new({
                let errors = Arc::clone(&errors);
                move |err| errors.lock().expect("lock").push(err.message().to_string())
            })),
        },
    )
    .expect("session service source");

    let catalogue = services.catalogue(BACKGROUND_CONTEXT.clone()).await.expect("catalogue");
    assert!(catalogue.contains(&pi_chord::ServiceCatalogueEntry {
        service_id: KEYED_PROBE_SERVICE.id.clone(),
        mode: ServiceMode::Keyed,
    }));

    let observed = Arc::new(Mutex::new(Vec::<(KeyedProbe, Option<String>)>::new()));
    let observed_ref = Arc::clone(&observed);
    let consumer = define_facet(Facet::new(
        "@test/keyed-probe-consumer",
        Arc::new(move |env| {
            let observed = Arc::clone(&observed_ref);
            env.observe::<ServiceObject>(
                &*KEYED_PROBE_SERVICE,
                Arc::new(move |obj, _ctx| {
                    let probe = KeyedProbe::from_service(obj)?;
                    let value = probe.state.value().map(|s| s.value);
                    observed.lock().expect("lock").push((probe, value));
                    Box::pin(futures::future::ready(Ok(())))
                }),
            )
        }),
    ));
    let facet_host = create_facet_host(FacetOptions {
        facets: vec![consumer],
        service_sources: Some(vec![services.as_remote_source()]),
        on_error: None,
    })
    .await
    .expect("create facet host");

    assert_eq!(
        services.attachment.value(),
        Some(SessionAttachmentState::Attached {
            session_id: "demo-1".to_string(),
        })
    );
    poll_until(5_000, || observed.lock().expect("lock").len() >= 1).await;
    let first_probe = {
        let guard = observed.lock().expect("lock");
        assert_eq!(guard[0].1.as_deref(), Some("first"));
        guard[0].0.clone()
    };

    first_probe
        .replace("second", BACKGROUND_CONTEXT.clone())
        .await
        .expect("replace first -> second");
    poll_until(5_000, || observed.lock().expect("lock").len() >= 2).await;
    let second_probe = {
        let guard = observed.lock().expect("lock");
        assert_eq!(guard[1].1.as_deref(), Some("second"));
        guard[1].0.clone()
    };
    let stale_err = first_probe
        .replace("late", BACKGROUND_CONTEXT.clone())
        .await
        .expect_err("stale replace must fail");
    assert!(
        stale_err.message().contains("observation is closed"),
        "unexpected stale error: {}",
        stale_err.message()
    );

    attach_session(&client, "demo-2").await;
    services
        .when_attached("demo-2", BACKGROUND_CONTEXT.clone())
        .await
        .expect("when attached demo-2");
    poll_until(5_000, || observed.lock().expect("lock").len() >= 3).await;
    assert_eq!(
        services.attachment.value(),
        Some(SessionAttachmentState::Attached {
            session_id: "demo-2".to_string(),
        })
    );
    assert_eq!(observed.lock().expect("lock")[2].1.as_deref(), Some("first"));
    let replaced_err = second_probe
        .replace("late", BACKGROUND_CONTEXT.clone())
        .await
        .expect_err("replaced observation must fail");
    assert!(
        replaced_err.message().contains("observation is closed"),
        "unexpected replaced error: {}",
        replaced_err.message()
    );
    assert!(errors.lock().expect("lock").is_empty());

    facet_host.dispose().await.expect("dispose facet host");
    services
        .dispose(BACKGROUND_CONTEXT.clone())
        .await
        .expect("dispose source");
    client.dispose().await.expect("dispose client");
    runtime.close().await.expect("close runtime");
}

#[tokio::test]
#[serial]
async fn answers_a_client_prompt_through_the_worker_owned_service_provider() {
    let _env = setup_test_env().await;
    let _hook = SpawnHookGuard::with_faux_worker();
    let dir = tempfile::Builder::new().prefix("pes-").tempdir().expect("tempdir");
    let directory = dir.path().to_string_lossy().into_owned();
    let runtime = make_server(&directory).await;

    let mut cmd = base_client_command();
    cmd.session_id = Some("demo-1".to_string());
    cmd.prompt = Some("question".to_string());
    let res = run_client(
        &cmd,
        RunClientOptions {
            directory: Some(directory),
        },
    )
    .await
    .expect("run_client prompt");

    assert_eq!(
        res,
        ClientResult::Prompted {
            server_id: runtime.server_id.clone(),
            session_id: "demo-1".to_string(),
            text: "deterministic remote answer".to_string(),
        }
    );
    runtime.close().await.expect("close runtime");
}

#[tokio::test]
#[serial]
async fn replicates_the_transcript_after_consecutive_prompts() {
    let _env = setup_test_env().await;
    let _hook = SpawnHookGuard::with_faux_worker();
    let dir = tempfile::Builder::new().prefix("pes-").tempdir().expect("tempdir");
    let runtime = make_server(&dir.path().to_string_lossy()).await;
    let client = attach_client(&runtime, "demo-1").await;

    let services = create_session_service_binding(
        client.clone(),
        ServiceBindingOptions {
            services: vec![
                pi_chord::types::ServiceIdRef {
                    id: AGENT_CONTROLLER_SERVICE.id.clone(),
                },
                pi_chord::types::ServiceIdRef {
                    id: SESSION_PLUGINS_SERVICE.id.clone(),
                },
                pi_chord::types::ServiceIdRef {
                    id: TRANSCRIPT_SERVICE.id.clone(),
                },
            ],
            on_error: None,
        },
    )
    .expect("session binding");
    let controller = AgentController::from_service(
        services
            .r#use::<ServiceObject>(&*AGENT_CONTROLLER_SERVICE)
            .expect("controller"),
    );
    let session_plugins = SessionPlugins::from_service(
        services
            .r#use::<ServiceObject>(&*SESSION_PLUGINS_SERVICE)
            .expect("session plugins"),
    );
    let transcript = Transcript::from_service(
        services
            .r#use::<ServiceObject>(&*TRANSCRIPT_SERVICE)
            .expect("transcript"),
    )
    .expect("wrap transcript");

    services.ready(BACKGROUND_CONTEXT.clone()).await.expect("ready");
    session_plugins
        .reload(BACKGROUND_CONTEXT.clone())
        .await
        .expect("reload session plugins");

    for (idx, message) in ["first question", "second question"].iter().enumerate() {
        let resp = controller
            .prompt(
                AgentPromptRequest {
                    message: (*message).to_string(),
                    images: None,
                },
                BACKGROUND_CONTEXT.clone(),
            )
            .await
            .expect("prompt");
        assert!(resp.accepted);
        let op_id = resp.operation_id.expect("operation id");
        let outcome = controller
            .wait_for_prompt(&op_id, BACKGROUND_CONTEXT.clone())
            .await
            .expect("wait_for_prompt");
        assert_eq!(
            outcome,
            AgentPromptResult::Done {
                text: "deterministic remote answer".to_string(),
                reason: None,
            }
        );
        let expected_assistant_count = idx + 1;
        poll_until(5_000, || {
            transcript.state.value().is_some_and(|view| {
                view.entries.iter().filter(|e| e.kind == "pi.assistant").count() == expected_assistant_count
            })
        })
        .await;
    }

    services.dispose(BACKGROUND_CONTEXT.clone()).await.expect("dispose");
    client.dispose().await.expect("dispose client");
    runtime.close().await.expect("close runtime");
}

#[tokio::test]
#[serial]
async fn stops_an_idle_session_worker_after_its_client_disconnects() {
    let _env = setup_test_env().await;
    let dir = tempfile::Builder::new().prefix("pes-").tempdir().expect("tempdir");
    let runtime = make_server(&dir.path().to_string_lossy()).await;
    let client = attach_client(&runtime, "demo-1").await;
    let pid = runtime.worker_pids().get("demo-1").copied().expect("worker pid");

    client.dispose().await.expect("dispose client");
    poll_until(5_000, || !runtime.worker_pids().contains_key("demo-1")).await;
    assert!(!process_exists(pid));
    runtime.close().await.expect("close runtime");
}

#[tokio::test]
#[serial]
async fn starts_one_process_per_attached_session_and_stops_them_during_shutdown() {
    let _env = setup_test_env().await;
    let dir = tempfile::Builder::new().prefix("pes-").tempdir().expect("tempdir");
    let runtime = make_server(&dir.path().to_string_lossy()).await;
    let (c1, c2) = tokio::join!(attach_client(&runtime, "demo-1"), attach_client(&runtime, "demo-2"));

    let pids: Vec<u32> = runtime.worker_pids().values().copied().collect();
    assert_eq!(pids.len(), 2);
    let unique: IndexSet<u32> = pids.iter().copied().collect();
    assert_eq!(unique.len(), 2);
    for pid in &pids {
        assert!(process_exists(*pid));
    }

    runtime.close().await.expect("close runtime");
    assert!(runtime.worker_pids().is_empty());
    for pid in pids {
        poll_until(5_000, || !process_exists(pid)).await;
    }
    let _ = c1.dispose().await;
    let _ = c2.dispose().await;
}

#[cfg(unix)]
#[tokio::test]
#[serial]
async fn server_runtime_replaces_an_exited_worker_on_the_next_attach() {
    let _env = setup_test_env().await;
    let dir = tempfile::Builder::new().prefix("pew-").tempdir().expect("tempdir");
    let runtime = make_server(&dir.path().to_string_lossy()).await;
    let client = attach_client(&runtime, "demo-1").await;
    let first_pid = runtime.worker_pids().get("demo-1").copied().expect("first pid");

    let raw_pid = i32::try_from(first_pid).expect("valid pid");
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(raw_pid), nix::sys::signal::Signal::SIGKILL)
        .expect("kill worker");
    poll_until(5_000, || !runtime.worker_pids().contains_key("demo-1")).await;

    attach_session(&client, "demo-1").await;
    let replacement_pid = runtime.worker_pids().get("demo-1").copied().expect("replacement pid");
    assert_ne!(replacement_pid, first_pid);

    let _ = client.dispose().await;
    runtime.close().await.expect("close runtime");
}

#[tokio::test]
#[serial]
async fn discovers_workers_after_replacing_the_server() {
    let _env = setup_test_env().await;
    let dir = tempfile::Builder::new().prefix("per-").tempdir().expect("tempdir");
    let directory = dir.path().to_string_lossy().into_owned();
    let first = make_server(&directory).await;
    let c1 = attach_client(&first, "demo-1").await;
    let first_worker_pid = first.worker_pids().get("demo-1").copied().expect("first worker pid");

    let replacement = make_server(&directory).await;
    first.closed().await.expect("first server closed");

    assert_eq!(replacement.server_id, first.server_id);
    assert_eq!(replacement.worker_pids().get("demo-1").copied(), Some(first_worker_pid));
    poll_until(5_000, || first.worker_pids().is_empty()).await;
    assert!(process_exists(first_worker_pid));

    let list = run_client(
        &base_client_command(),
        RunClientOptions {
            directory: Some(directory),
        },
    )
    .await
    .expect("list via replacement");
    assert_eq!(
        list,
        ClientResult::List {
            sessions: vec![
                SessionAddress {
                    server_id: first.server_id.clone(),
                    session_id: "demo-1".to_string(),
                },
                SessionAddress {
                    server_id: first.server_id.clone(),
                    session_id: "demo-2".to_string(),
                },
            ],
        }
    );

    let c2 = attach_client(&replacement, "demo-1").await;
    assert_eq!(replacement.worker_pids().get("demo-1").copied(), Some(first_worker_pid));
    let c3 = attach_client(&replacement, "demo-2").await;
    let second_worker_pid = replacement.worker_pids().get("demo-2").copied().expect("second pid");
    assert_ne!(second_worker_pid, first_worker_pid);

    let _ = c1.dispose().await;
    let _ = c2.dispose().await;
    let _ = c3.dispose().await;
    replacement.close().await.expect("close replacement");
}

#[tokio::test]
#[serial]
async fn retires_an_unclaimed_idle_worker_after_replacement_demand_expires() {
    let mut env = setup_test_env().await;
    env._env.set(SESSION_WORKER_ORPHAN_DEMAND_GRACE_ENV, "50");
    let dir = tempfile::Builder::new()
        .prefix("pi-orphan-worker-")
        .tempdir()
        .expect("tempdir");
    let directory = dir.path().to_string_lossy().into_owned();
    let first = make_server(&directory).await;
    let client = attach_client(&first, "demo-1").await;
    let worker_pid = first.worker_pids().get("demo-1").copied().expect("worker pid");

    let replacement = make_server(&directory).await;
    first.closed().await.expect("first server closed");
    assert_eq!(replacement.worker_pids().get("demo-1").copied(), Some(worker_pid));

    poll_until(5_000, || !replacement.worker_pids().contains_key("demo-1")).await;
    assert!(!process_exists(worker_pid));

    let _ = client.dispose().await;
    replacement.close().await.expect("close replacement");
    drop(env);
}

#[tokio::test]
#[serial]
async fn restores_tracked_sessions_that_are_outside_the_replacement_catalog() {
    let _env = setup_test_env().await;
    let dir = tempfile::Builder::new().prefix("pet-").tempdir().expect("tempdir");
    let empty_dir = tempfile::Builder::new()
        .prefix("pet-sessions-")
        .tempdir()
        .expect("tempdir");
    let directory = dir.path().to_string_lossy().into_owned();
    let empty_session_dir = empty_dir.path().to_string_lossy().into_owned();

    let first = make_server(&directory).await;
    let c1 = attach_client(&first, "demo-1").await;
    let worker_pid = first.worker_pids().get("demo-1").copied().expect("worker pid");

    let replacement = start_server(StartServerOptions {
        directory: Some(directory.clone()),
        session_dir: Some(empty_session_dir),
        provider: Some(PROVIDER.to_string()),
        model: Some(MODEL.to_string()),
        ..Default::default()
    })
    .await
    .expect("start replacement");
    first.closed().await.expect("first server closed");

    let list = run_client(
        &base_client_command(),
        RunClientOptions {
            directory: Some(directory),
        },
    )
    .await
    .expect("list tracked sessions");
    assert_eq!(
        list,
        ClientResult::List {
            sessions: vec![SessionAddress {
                server_id: first.server_id.clone(),
                session_id: "demo-1".to_string(),
            }],
        }
    );

    let c2 = attach_client(&replacement, "demo-1").await;
    assert_eq!(replacement.worker_pids().get("demo-1").copied(), Some(worker_pid));

    let _ = c1.dispose().await;
    let _ = c2.dispose().await;
    replacement.close().await.expect("close replacement");
}

#[tokio::test]
#[serial]
async fn reports_missing_and_ambiguous_session_selections() {
    let _env = setup_test_env().await;
    let dir = tempfile::Builder::new().prefix("ped-").tempdir().expect("tempdir");
    let shared_directory = dir.path().to_string_lossy().into_owned();

    let (first_shared, second_shared) = tokio::try_join!(
        start_server(StartServerOptions {
            directory: Some(shared_directory.clone()),
            server_id: Some("00000000-0000-4000-8000-000000000001".to_string()),
            provider: Some(PROVIDER.to_string()),
            model: Some(MODEL.to_string()),
            ..Default::default()
        }),
        start_server(StartServerOptions {
            directory: Some(shared_directory.clone()),
            server_id: Some("00000000-0000-4000-8000-000000000002".to_string()),
            provider: Some(PROVIDER.to_string()),
            model: Some(MODEL.to_string()),
            ..Default::default()
        }),
    )
    .expect("start shared servers");

    let mut missing_cmd = base_client_command();
    missing_cmd.session_id = Some("missing".to_string());
    let missing_err = run_client(
        &missing_cmd,
        RunClientOptions {
            directory: Some(shared_directory.clone()),
        },
    )
    .await
    .expect_err("missing session must fail");
    assert!(
        missing_err
            .message()
            .contains("No discovered server contains session missing"),
        "unexpected error: {}",
        missing_err.message()
    );

    let mut dup_cmd = base_client_command();
    dup_cmd.session_id = Some("demo-1".to_string());
    let dup_err = run_client(
        &dup_cmd,
        RunClientOptions {
            directory: Some(shared_directory),
        },
    )
    .await
    .expect_err("ambiguous session must fail");
    assert!(
        dup_err
            .message()
            .contains("Session demo-1 is available from more than one server"),
        "unexpected error: {}",
        dup_err.message()
    );

    let _ = first_shared.close().await;
    let _ = second_shared.close().await;
}

#[tokio::test]
#[serial]
async fn rejects_a_duplicate_session_id_within_one_session_directory() {
    let env = setup_test_env().await;
    let sessions_root = join(&[&env.agent_dir, "experimental", "sessions"]);
    let other_cwd = join(&[&env.agent_dir, "other-cwd"]);
    let err = create_experimental_sessions(&sessions_root, &["demo-1"], Some(&other_cwd))
        .await
        .err()
        .expect("duplicate session ID must fail");
    assert!(
        err.message().contains("Session demo-1 already exists"),
        "unexpected error: {}",
        err.message()
    );
}
