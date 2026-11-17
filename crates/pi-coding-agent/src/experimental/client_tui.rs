//! Port of packages/coding-agent/src/experimental/client-tui.ts

use std::sync::Arc;

use pi_chord::context::BACKGROUND_CONTEXT;
use pi_chord::services::handle::ServiceObject;
use pi_chord::types::{RemoteServiceSourceOpenOptions, ServiceIdRef};
use pi_chord::{FacetLoader, JsonValue};
use pi_js::path::resolve;

use crate::cli::experimental::command_options::TransportAddress;
use crate::cli::experimental::commands::client::ClientCommand;
use crate::experimental::services::connection::{ServerServiceSource, SessionServiceSource};
use crate::experimental::services::plugins::{
    PRESENTATION_PLUGINS_SERVICE, PrepareSessionPluginsRequest, PresentationPlugins,
};
use crate::experimental::services::sessions::{
    SESSION_DIRECTORY_SERVICE, SESSION_MANAGEMENT_SERVICE, SessionCreateOptions, SessionDirectory, SessionManagement,
    SessionSummary,
};

#[derive(Clone, Default)]
pub struct RunClientTuiOptions {
    pub directory: Option<String>,
    pub facet_loader: Option<FacetLoader>,
}

#[derive(Clone)]
pub struct ClientTuiServer {
    pub server_id: String,
    pub radius: bool,
    pub server: ServerServiceSource,
    pub session: SessionServiceSource,
}

#[derive(Clone)]
pub struct PreparedClientSession {
    pub server: ClientTuiServer,
    pub summary: SessionSummary,
    pub presentation_plugins: JsonValue,
}

pub async fn prepare_client_session(
    command: &ClientCommand,
    servers: &[ClientTuiServer],
) -> pi_js::Result<PreparedClientSession> {
    let opened: Vec<(&ClientTuiServer, pi_chord::RemoteServices)> = servers
        .iter()
        .map(|server| {
            let services = server.server.open(RemoteServiceSourceOpenOptions {
                services: vec![
                    ServiceIdRef {
                        id: SESSION_DIRECTORY_SERVICE.id.clone(),
                    },
                    ServiceIdRef {
                        id: SESSION_MANAGEMENT_SERVICE.id.clone(),
                    },
                    ServiceIdRef {
                        id: PRESENTATION_PLUGINS_SERVICE.id.clone(),
                    },
                ],
                assert_access: Arc::new(|| Ok(())),
                on_error: Arc::new(|_| {}),
            });
            (server, services)
        })
        .collect();

    struct ServerFeature<'a> {
        server: &'a ClientTuiServer,
        directory: SessionDirectory,
        management: SessionManagement,
        plugins: PresentationPlugins,
    }

    let outcome = async {
        let mut features = Vec::with_capacity(opened.len());
        for (server, services) in &opened {
            let directory =
                SessionDirectory::from_service(services.r#use::<ServiceObject>(&*SESSION_DIRECTORY_SERVICE)?)?;
            let management =
                SessionManagement::from_service(services.r#use::<ServiceObject>(&*SESSION_MANAGEMENT_SERVICE)?);
            let plugins =
                PresentationPlugins::from_service(services.r#use::<ServiceObject>(&*PRESENTATION_PLUGINS_SERVICE)?);
            features.push(ServerFeature {
                server,
                directory,
                management,
                plugins,
            });
        }
        futures::future::try_join_all(
            opened
                .iter()
                .map(|(_, services)| services.ready(BACKGROUND_CONTEXT.clone())),
        )
        .await?;

        struct Selected<'a> {
            server: &'a ClientTuiServer,
            management: &'a SessionManagement,
            plugins: &'a PresentationPlugins,
            summary: SessionSummary,
        }

        let mut selected: Option<Selected<'_>> = None;
        if let Some(ref requested_id) = command.session_id {
            let mut matches: Vec<Selected<'_>> = Vec::new();
            for feature in &features {
                if let Some(state) = feature.directory.state.value() {
                    for summary in state.sessions {
                        if &summary.session_id == requested_id {
                            matches.push(Selected {
                                server: feature.server,
                                management: &feature.management,
                                plugins: &feature.plugins,
                                summary,
                            });
                        }
                    }
                }
            }
            if matches.len() > 1 {
                return Err(pi_js::Error::msg(format!(
                    "Session {requested_id} is available from more than one server"
                )));
            }
            if let Some(found) = matches.into_iter().next() {
                selected = Some(found);
            } else {
                if matches!(command.connect, Some(TransportAddress::Radius { .. })) {
                    return Err(pi_js::Error::msg(format!(
                        "Remote server does not contain Session {requested_id}"
                    )));
                }
                let feature = require_single_server(&features)?;
                let summary = feature
                    .management
                    .create(
                        SessionCreateOptions {
                            id: Some(requested_id.clone()),
                        },
                        BACKGROUND_CONTEXT.clone(),
                    )
                    .await?;
                selected = Some(Selected {
                    server: feature.server,
                    management: &feature.management,
                    plugins: &feature.plugins,
                    summary,
                });
            }
        } else if command.continue_ || command.resume {
            let mut candidates: Vec<Selected<'_>> = Vec::new();
            for feature in &features {
                if let Some(state) = feature.directory.state.value() {
                    for summary in state.sessions {
                        candidates.push(Selected {
                            server: feature.server,
                            management: &feature.management,
                            plugins: &feature.plugins,
                            summary,
                        });
                    }
                }
            }
            candidates.sort_by(|l, r| {
                r.summary
                    .created_at
                    .cmp(&l.summary.created_at)
                    .then_with(|| l.summary.server_id.cmp(&r.summary.server_id))
                    .then_with(|| l.summary.session_id.cmp(&r.summary.session_id))
            });
            selected = candidates.into_iter().next();
        }

        let selected = match selected {
            Some(sel) => sel,
            None => {
                let feature = require_single_server(&features)?;
                let summary = feature
                    .management
                    .create(SessionCreateOptions::default(), BACKGROUND_CONTEXT.clone())
                    .await?;
                Selected {
                    server: feature.server,
                    management: &feature.management,
                    plugins: &feature.plugins,
                    summary,
                }
            }
        };

        let package_paths = if command.plugin_packages.is_empty() {
            None
        } else {
            Some(command.plugin_packages.iter().map(|p| resolve(&[p])).collect())
        };
        let presentation_plugins = selected
            .plugins
            .prepare_session(
                PrepareSessionPluginsRequest {
                    session_id: selected.summary.session_id.clone(),
                    package_paths,
                },
                BACKGROUND_CONTEXT.clone(),
            )
            .await?;

        selected
            .management
            .attach(&selected.summary.session_id, BACKGROUND_CONTEXT.clone())
            .await?;
        selected
            .server
            .session
            .when_attached(&selected.summary.session_id, BACKGROUND_CONTEXT.clone())
            .await?;

        Ok(PreparedClientSession {
            server: selected.server.clone(),
            summary: selected.summary,
            presentation_plugins,
        })
    }
    .await;

    for (_, services) in opened {
        let _ = services.dispose(BACKGROUND_CONTEXT.clone()).await;
    }

    outcome
}

pub struct ExperimentalClientTui {
    ui: crate::modes::interactive::tui_renderer::InteractiveTui,
    theme_controller: crate::modes::interactive::theme::theme_controller::InteractiveThemeController,
    chat: Arc<std::sync::Mutex<crate::experimental::client_tui_chat::ExperimentalChatView>>,
    agent: crate::experimental::services::agent_controller::AgentController,
    models: crate::experimental::services::models::Models,
    footer: Arc<pi_tui::Text>,
    session_services: pi_chord::RemoteServices,
    unsubscribes: std::sync::Mutex<Vec<pi_js::Unsubscribe>>,
    exit_notify: Arc<tokio::sync::Notify>,
    exited: Arc<std::sync::atomic::AtomicBool>,
    editor: crate::modes::interactive::components::custom_editor::CustomEditor,
    prepared: PreparedClientSession,
}

impl ExperimentalClientTui {
    pub async fn create(command: &ClientCommand, servers: &[ClientTuiServer]) -> pi_js::Result<Self> {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{Arc, Mutex, PoisonError};

        use pi_tui::{Component, TUI, Text, TuiMode, set_capability_overrides};

        use crate::config::get_agent_dir;
        use crate::core::keybindings::KeybindingsManager;
        use crate::core::settings_manager::SettingsManager;
        use crate::experimental::client_tui_chat::ExperimentalChatView;
        use crate::experimental::services::agent_controller::{
            AGENT_CONTROLLER_SERVICE, AgentCompactionRequest, AgentController,
        };
        use crate::experimental::services::models::{MODELS_SERVICE, Models};
        use crate::experimental::services::transcript::{TRANSCRIPT_SERVICE, Transcript};
        use crate::modes::interactive::chat_viewport::{ChatViewportOptions, create_chat_viewport};
        use crate::modes::interactive::components::custom_editor::{CustomEditor, CustomEditorOptions};
        use crate::modes::interactive::components::dynamic_border::DynamicBorder;
        use crate::modes::interactive::interactive_mode::sync_tui_keybindings;
        use crate::modes::interactive::theme::theme::{ThemeColor, get_editor_theme, theme};
        use crate::modes::interactive::theme::theme_controller::{
            InteractiveThemeController, InteractiveThemeControllerOptions,
        };
        use crate::modes::interactive::tui_renderer::{
            InteractiveTuiMode, InteractiveTuiOptions, create_interactive_tui,
        };

        let prepared = prepare_client_session(command, servers).await?;
        let cwd = pi_js::env::cwd();
        let settings = Arc::new(SettingsManager::create(&cwd, None, None));
        let kb = KeybindingsManager::create(None);
        sync_tui_keybindings(&kb);
        set_capability_overrides(settings.get_terminal_capability_overrides());

        let tui_mode = match settings.get_tui_mode() {
            crate::modes::interactive::components::settings_selector::TuiMode::Regular => InteractiveTuiMode::Regular,
            crate::modes::interactive::components::settings_selector::TuiMode::Fullscreen => {
                InteractiveTuiMode::Fullscreen
            }
        };
        let ui = create_interactive_tui(InteractiveTuiOptions {
            tui_mode: Some(tui_mode),
            show_hardware_cursor: settings.get_show_hardware_cursor(),
            log_directory: get_agent_dir(),
            fullscreen_copy_on_select: Some(settings.get_fullscreen_copy_on_select()),
            fullscreen_wheel_scroll_lines: Some(settings.get_fullscreen_wheel_scroll_lines()),
            ..Default::default()
        });
        ui.set_clear_on_shrink(settings.get_clear_on_shrink());

        let session_services = prepared.server.session.open(RemoteServiceSourceOpenOptions {
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

        let agent_obj =
            session_services.r#use::<pi_chord::services::handle::ServiceObject>(&*AGENT_CONTROLLER_SERVICE)?;
        let models_obj = session_services.r#use::<pi_chord::services::handle::ServiceObject>(&*MODELS_SERVICE)?;
        let transcript_obj =
            session_services.r#use::<pi_chord::services::handle::ServiceObject>(&*TRANSCRIPT_SERVICE)?;

        let agent = AgentController::from_service(agent_obj);
        let models = Models::from_service(models_obj)?;
        let transcript = Transcript::from_service(transcript_obj)?;
        session_services.ready(BACKGROUND_CONTEXT.clone()).await?;

        let chat = Arc::new(Mutex::new(ExperimentalChatView::new_with_ui(
            &cwd,
            Some(ui.as_tui_arc()),
        )?));
        if let Some(initial_view) = transcript.state.value() {
            chat.lock().unwrap_or_else(PoisonError::into_inner).apply(&initial_view);
        }

        let footer = Arc::new(Text::new(Some(""), Some(1), Some(0), None));
        let update_footer = {
            let footer = Arc::clone(&footer);
            let models_state = models.state.clone();
            let server_id = prepared.server.server_id.clone();
            let session_id = prepared.summary.session_id.clone();
            Arc::new(move || {
                let model_info = models_state
                    .value()
                    .map(|st| {
                        let m = st
                            .configuration
                            .model
                            .map(|r| format!("{}/{}", r.provider, r.model_id))
                            .unwrap_or_else(|| "no-model".to_string());
                        let t = format!("{:?}", st.configuration.thinking_level).to_lowercase();
                        format!("{m} · {t}")
                    })
                    .unwrap_or_else(|| "no-model".to_string());
                let text = theme().fg(
                    ThemeColor::Dim,
                    &format!("{server_id}/{session_id} · {model_info} · /compact /thinking /exit"),
                );
                footer.set_text(&text);
            })
        };
        update_footer();

        let editor = CustomEditor::new(
            ui.as_tui_arc(),
            get_editor_theme(),
            kb,
            Some(CustomEditorOptions {
                padding_x: Some(settings.get_editor_padding_x() as f64),
                autocomplete_max_visible: Some(settings.get_autocomplete_max_visible() as f64),
                embed_working_status: Some(true),
            }),
        );

        let theme_controller = InteractiveThemeController::new(ui.as_tui_arc(), {
            let settings_for_theme = Arc::clone(&settings);
            let chat_for_theme = Arc::clone(&chat);
            let transcript_for_theme = transcript.state.clone();
            let ui_for_theme = ui.clone();
            InteractiveThemeControllerOptions {
                get_settings_manager: Arc::new(move || {
                    Arc::clone(&settings_for_theme)
                        as Arc<dyn crate::modes::interactive::theme::theme_controller::ThemeSettingsSource>
                }),
                show_error: Arc::new(|_| {}),
                on_changed: Arc::new(move || {
                    if let Some(view) = transcript_for_theme.value() {
                        chat_for_theme
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .refresh_theme(&view);
                    }
                    ui_for_theme.request_render(Some(true));
                }),
                initial_theme_setting: settings.get_theme(),
            }
        });

        let (transcript_comp, pending_comp, status_comp) = {
            let guard = chat.lock().unwrap_or_else(PoisonError::into_inner);
            (
                Arc::new(guard.transcript.clone()) as Arc<dyn Component>,
                Arc::new(guard.pending_messages.clone()) as Arc<dyn Component>,
                Arc::new(guard.status.clone()) as Arc<dyn Component>,
            )
        };

        if ui.mode() == TuiMode::Fullscreen {
            let viewport = create_chat_viewport(ChatViewportOptions {
                document: transcript_comp,
                pending_messages: pending_comp,
                status: status_comp,
                editor: Arc::new(editor.clone()),
                footer: footer.clone(),
                widgets_above: None,
                widgets_below: None,
                scrollbar: Some(settings.get_fullscreen_scrollbar()),
                scrollbar_track_style: None,
                scrollbar_thumb_style: None,
            });
            ui.set_children(vec![viewport.root]);
        } else {
            ui.set_children(vec![
                Arc::new(DynamicBorder::new(None)) as Arc<dyn Component>,
                transcript_comp,
                pending_comp,
                status_comp,
                Arc::new(editor.clone()) as Arc<dyn Component>,
                footer.clone(),
            ]);
        }
        ui.set_focus(Some(Arc::new(editor.clone())));

        let exit_notify = Arc::new(tokio::sync::Notify::new());
        let exited = Arc::new(AtomicBool::new(false));

        let mut unsubscribes = Vec::new();
        {
            let chat_sub = Arc::clone(&chat);
            let ui_sub = ui.clone();
            let unsub_t = transcript.state.subscribe(move |view, _ctx, _delivery| {
                let chat_sub = Arc::clone(&chat_sub);
                let ui_sub = ui_sub.clone();
                Box::pin(async move {
                    chat_sub.lock().unwrap_or_else(PoisonError::into_inner).apply(&view);
                    ui_sub.request_render(None);
                    Ok(())
                })
            })?;
            unsubscribes.push(unsub_t);
        }
        {
            let update_footer = Arc::clone(&update_footer);
            let ui_sub = ui.clone();
            let unsub_m = models.state.subscribe(move |_st, _ctx, _delivery| {
                let update_footer = Arc::clone(&update_footer);
                let ui_sub = ui_sub.clone();
                Box::pin(async move {
                    update_footer();
                    ui_sub.request_render(None);
                    Ok(())
                })
            })?;
            unsubscribes.push(unsub_m);
        }

        let agent_submit = agent.clone();
        let models_submit = models.clone();
        let editor_submit = editor.clone();
        let exit_submit = Arc::clone(&exit_notify);
        let exited_submit = Arc::clone(&exited);
        editor.set_on_submit(Some(Arc::new(move |raw: &str| {
            let trimmed = raw.trim().to_string();
            if trimmed.is_empty() {
                return;
            }
            editor_submit.add_to_history(&trimmed);
            editor_submit.set_text("");

            if trimmed == "/exit" || trimmed == "/quit" {
                exited_submit.store(true, Ordering::Release);
                exit_submit.notify_waiters();
                return;
            }
            if trimmed == "/thinking" {
                let m = models_submit.clone();
                tokio::spawn(async move {
                    let _ = m.cycle_thinking(BACKGROUND_CONTEXT.clone()).await;
                });
                return;
            }
            if trimmed == "/compact" || trimmed.starts_with("/compact ") {
                let custom = trimmed
                    .strip_prefix("/compact")
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty());
                let a = agent_submit.clone();
                tokio::spawn(async move {
                    let _ = a
                        .compact(
                            AgentCompactionRequest {
                                custom_instructions: custom,
                            },
                            BACKGROUND_CONTEXT.clone(),
                        )
                        .await;
                });
                return;
            }

            let a = agent_submit.clone();
            tokio::spawn(async move {
                let _ = a
                    .prompt(
                        crate::experimental::services::agent_controller::AgentPromptRequest {
                            message: trimmed,
                            images: None,
                        },
                        BACKGROUND_CONTEXT.clone(),
                    )
                    .await;
            });
        })));

        let agent_esc = agent.clone();
        editor.set_on_escape(Some(Arc::new(move || {
            let a = agent_esc.clone();
            tokio::spawn(async move {
                let _ = a.abort(BACKGROUND_CONTEXT.clone()).await;
            });
        })));

        let exit_ctrl_d = Arc::clone(&exit_notify);
        let exited_ctrl_d = Arc::clone(&exited);
        editor.set_on_ctrl_d(Some(Arc::new(move || {
            exited_ctrl_d.store(true, Ordering::Release);
            exit_ctrl_d.notify_waiters();
        })));

        Ok(Self {
            ui,
            theme_controller,
            chat,
            agent,
            models,
            footer,
            session_services,
            unsubscribes: std::sync::Mutex::new(unsubscribes),
            exit_notify,
            exited,
            editor,
            prepared,
        })
    }

    pub fn tui(&self) -> &crate::modes::interactive::tui_renderer::InteractiveTui {
        &self.ui
    }

    pub async fn run(&self) -> pi_js::Result<()> {
        use std::sync::atomic::Ordering;

        use pi_tui::{TUI, TuiStopOptions};

        self.ui.start();
        self.ui.request_render(Some(true));
        while !self.exited.load(Ordering::Acquire) {
            self.exit_notify.notified().await;
        }
        self.ui.stop(Some(TuiStopOptions::default()));
        Ok(())
    }

    pub async fn dispose(&self) -> pi_js::Result<()> {
        let _ = (&self.agent, &self.models, &self.footer, &self.editor, &self.prepared);
        let unsubs = {
            let mut guard = self.unsubscribes.lock().unwrap_or_else(|e| e.into_inner());
            std::mem::take(&mut *guard)
        };
        for u in unsubs {
            u.call();
        }
        self.theme_controller.dispose();
        self.chat.lock().unwrap_or_else(|e| e.into_inner()).dispose();
        self.session_services.dispose(BACKGROUND_CONTEXT.clone()).await
    }
}

pub async fn run_client_tui(command: &ClientCommand, options: RunClientTuiOptions) -> pi_js::Result<()> {
    use crate::experimental::client_runtime::{OpenClientRuntimeOptions, open_client_runtime};

    let runtime = open_client_runtime(
        command,
        OpenClientRuntimeOptions {
            directory: options.directory,
        },
    )
    .await?;

    let servers: Vec<ClientTuiServer> = runtime
        .servers
        .iter()
        .map(|s| ClientTuiServer {
            server_id: s.route.server_id().clone(),
            radius: s.route.is_radius(),
            server: s.server.clone(),
            session: s.session.clone(),
        })
        .collect();

    let run_res = match ExperimentalClientTui::create(command, &servers).await {
        Ok(app) => {
            let r = app.run().await;
            let d = app.dispose().await;
            r.and(d)
        }
        Err(err) => Err(err),
    };
    let dispose_res = runtime.dispose().await;
    run_res?;
    dispose_res
}

fn require_single_server<T>(features: &[T]) -> pi_js::Result<&T> {
    if features.len() != 1 {
        return Err(pi_js::Error::msg("Starting a Session requires exactly one server"));
    }
    Ok(&features[0])
}
