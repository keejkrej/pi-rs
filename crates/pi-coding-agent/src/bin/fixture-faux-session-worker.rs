//! Port of packages/coding-agent/test/fixtures/faux-session-worker.ts

use std::process::ExitCode;
use std::sync::{Arc, LazyLock, Mutex, PoisonError};

use indexmap::IndexMap;
use pi_ai::models::create_models;
use pi_ai::{
    FauxAssistantMessageOptions, FauxResponseStep, RegisterFauxProviderOptions, faux_assistant_message, faux_provider,
};
use pi_chord::api::DefineServiceOptions;
use pi_chord::context::BACKGROUND_CONTEXT;
use pi_chord::services::handle::{ServiceObject, ServiceValue};
use pi_chord::{Facet, JsonValue, Service, create_static_facet_loader, define_facet, define_service, replicated_state};
use pi_durable::storage::sqlite::node::{NodeSqliteStorageOptions, open_node_sqlite_storage};
use pi_durable::{
    AgentChange, FieldChange, Harness, HarnessOptions, ModelRef, RootConversationOptions, create_registry,
};
use pi_js::Unsubscribe;
use pi_js::abort::AbortReason;
use serde_json::{Map, Value};

use pi_coding_agent::experimental::process::{InternalProcessRole, consume_internal_process_role};
use pi_coding_agent::experimental::services::connection::{make_async_fn, wrap_mutable_state};
use pi_coding_agent::experimental::session_worker::{SessionWorkerRuntime, run_session_worker_with_harness};

static KEYED_PROBE_SERVICE: LazyLock<Service> = LazyLock::new(|| {
    define_service("test.keyed-probe", Some(DefineServiceOptions { local: Some(false) }))
        .unwrap_or_else(|e| panic!("valid service definition: {e}"))
});

type SpawnFn = Arc<dyn Fn(String) -> pi_js::Result<()> + Send + Sync>;

fn create_keyed_probe_facet() -> Facet {
    define_facet(Facet::new(
        "@test/keyed-probe",
        Arc::new(|env| {
            let probes = env.provide_many::<ServiceObject>(&KEYED_PROBE_SERVICE)?;
            let spawn_slot: Arc<Mutex<Option<SpawnFn>>> = Arc::new(Mutex::new(None));
            let env_for_spawn = env.clone();
            let spawn_slot_clone = Arc::clone(&spawn_slot);
            let spawn_fn: SpawnFn = Arc::new(move |value: String| {
                let mut initial_map = Map::new();
                initial_map.insert("value".to_string(), Value::String(value));
                let state = env_for_spawn.replicated_state::<JsonValue>(Value::Object(initial_map))?;
                // Keep replicated state hooked via pi_chord::replicated_state_initial helper compatibility if needed
                let _ = &replicated_state;
                let close_slot: Arc<Mutex<Option<Unsubscribe>>> = Arc::new(Mutex::new(None));

                let mut members: IndexMap<String, ServiceValue> = IndexMap::new();
                members.insert("state".to_string(), ServiceValue::Object(wrap_mutable_state(&state)));

                let close_for_replace = Arc::clone(&close_slot);
                let spawn_for_replace = Arc::clone(&spawn_slot_clone);
                members.insert(
                    "replace".to_string(),
                    make_async_fn(move |args, _ctx| {
                        let next_res = args
                            .first()
                            .and_then(|v| match v {
                                ServiceValue::String(s) => Some(s.clone()),
                                _ => None,
                            })
                            .ok_or_else(|| pi_js::Error::js("TypeError", "Expected string value for replace"));
                        let close_for_replace = Arc::clone(&close_for_replace);
                        let spawn_for_replace = Arc::clone(&spawn_for_replace);
                        Box::pin(async move {
                            let next = next_res?;
                            if let Some(unsub) = close_for_replace.lock().unwrap_or_else(PoisonError::into_inner).take()
                            {
                                unsub.call();
                            }
                            let spawn = spawn_for_replace
                                .lock()
                                .unwrap_or_else(PoisonError::into_inner)
                                .clone()
                                .ok_or_else(|| pi_js::Error::msg("Spawner missing"))?;
                            spawn(next)?;
                            Ok(None)
                        })
                    }),
                );

                members.insert(
                    "wait".to_string(),
                    make_async_fn(move |_args, ctx| {
                        Box::pin(async move {
                            let signal = ctx
                                .abort_signal()
                                .ok_or_else(|| pi_js::Error::msg("Probe wait requires cancellation"))?;
                            if signal.aborted() {
                                return Err(match signal.reason() {
                                    Some(reason) => pi_js::Error::Abort(reason),
                                    None => pi_js::Error::Abort(AbortReason::error(
                                        "AbortError",
                                        "The operation was aborted",
                                    )),
                                });
                            }
                            signal.cancelled().await;
                            Err(match signal.reason() {
                                Some(reason) => pi_js::Error::Abort(reason),
                                None => {
                                    pi_js::Error::Abort(AbortReason::error("AbortError", "The operation was aborted"))
                                }
                            })
                        })
                    }),
                );

                let obj = ServiceObject::from_members(members);
                let unsub = probes.try_spawn("probe", obj)?;
                *close_slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(unsub);
                Ok(())
            });

            *spawn_slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::clone(&spawn_fn));

            env.on_activate(Arc::new(move || {
                let spawn = Arc::clone(&spawn_fn);
                Box::pin(async move { spawn("first".to_string()) })
            }))?;

            Ok(())
        }),
    ))
}

fn main() -> ExitCode {
    pi_js::ensure_crypto_provider();
    match consume_internal_process_role() {
        Ok(Some(InternalProcessRole::SessionWorker)) => {}
        Ok(_) | Err(_) => {
            eprintln!("Faux Session worker requires a session-worker invocation");
            return ExitCode::from(1);
        }
    }

    let args: Vec<String> = std::env::args().skip(1).collect();
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(err) => {
            eprintln!("Failed to start faux session worker runtime: {err}");
            return ExitCode::from(1);
        }
    };

    let res = rt.block_on(run_session_worker_with_harness(
        &args,
        Arc::new(|database_path, options| {
            Box::pin(async move {
                if options.provider.as_deref() != Some("anthropic")
                    || options.model.as_deref() != Some("claude-sonnet-4-5")
                {
                    return Err(pi_js::Error::msg(format!(
                        "Unexpected faux worker model: {:?}/{:?}",
                        options.provider, options.model
                    )));
                }
                let faux = faux_provider(Some(RegisterFauxProviderOptions::default()));
                let answer = faux_assistant_message(
                    pi_ai::FauxAssistantMessageContent::Text("deterministic remote answer".to_string()),
                    Some(FauxAssistantMessageOptions {
                        timestamp: Some(20),
                        ..Default::default()
                    }),
                );
                faux.set_responses(vec![
                    FauxResponseStep::Message(answer.clone()),
                    FauxResponseStep::Message(answer),
                ]);
                let models = create_models(None);
                models.set_provider(faux.provider());
                let storage =
                    Arc::new(open_node_sqlite_storage(&database_path, NodeSqliteStorageOptions::default()).await?);
                let harness = Harness::open(
                    storage,
                    HarnessOptions {
                        models,
                        registry: create_registry(),
                        settings: None,
                        env: None,
                        conversation_created: None,
                        now: None,
                        on_report: None,
                    },
                    BACKGROUND_CONTEXT.clone(),
                )
                .await?;
                let model = faux.get_model();
                let conversation = harness
                    .root(
                        BACKGROUND_CONTEXT.clone(),
                        Some(RootConversationOptions {
                            agent: Some(AgentChange {
                                cwd: FieldChange::set(options.metadata.cwd),
                                model: FieldChange::set(ModelRef {
                                    provider: model.provider,
                                    model_id: model.id,
                                }),
                                ..Default::default()
                            }),
                            init: None,
                        }),
                    )
                    .await?;
                Ok(SessionWorkerRuntime {
                    harness,
                    conversation,
                    model_runtime: None,
                    settings_manager: None,
                    facet_loader: Some(create_static_facet_loader(vec![create_keyed_probe_facet()])),
                    cleanup: None,
                })
            })
        }),
    ));

    match res {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{err}");
            ExitCode::from(1)
        }
    }
}
