//! Port of packages/coding-agent/src/modes/interactive/interactive-mode.ts#L433-L1355

use std::process::Stdio;
use std::sync::{Arc, Mutex, Weak};

use indexmap::IndexSet;
use pi_tui::autocomplete::CombinedCommand;
use pi_tui::tui::{TUI, TuiMode, TuiStopOptions};
use pi_tui::{
    CombinedAutocompleteProvider, Component, Container, SlashCommand, Spacer, Text, set_capability_overrides,
};

use crate::modes::interactive::components::armin_3d_lazy::play_pi_logo_3d;
use crate::modes::interactive::components::custom_editor::{CustomEditor, CustomEditorOptions};
use crate::modes::interactive::components::dynamic_border::DynamicBorder;
use crate::modes::interactive::components::footer::{FooterComponent, ReadonlyFooterDataProvider};
use crate::modes::interactive::components::keybinding_hints::{key_hint, key_text, raw_key_hint};
use crate::modes::interactive::components::mermaid::MermaidTransformerOptions;
use crate::modes::interactive::components::mermaid::create_mermaid_markdown_transformer;
use crate::modes::interactive::components::pi_logo::{pi_logo_lines, pi_wordmark, supports_pi_logo};
use crate::modes::interactive::components::settings_selector::{FullscreenExitOutput, TuiMode as SettingsTuiMode};
use crate::modes::interactive::components::status_indicator::IdleStatus;
use crate::modes::interactive::components::themed_text::ThemedText;
use crate::modes::interactive::model_catalog_refresh::refresh_model_catalogs;
use crate::modes::interactive::model_search::get_model_search_text;
use crate::modes::interactive::theme::theme::{
    ThemeColor, get_editor_theme, on_theme_change, set_registered_themes, theme,
};
use crate::modes::interactive::theme::theme_controller::{
    InteractiveThemeController, InteractiveThemeControllerOptions, ThemeSettingsSource,
};
use crate::modes::interactive::tui_renderer::{
    InteractiveTui, InteractiveTuiMode, InteractiveTuiOptions, create_interactive_tui, create_interactive_tui_reference,
};

use super::*;

#[derive(Clone, Debug)]
pub struct BuiltinSlashCommand {
    pub name: &'static str,
    pub description: &'static str,
    pub argument_hint: Option<&'static str>,
}

/// Built-in slash commands matching `packages/coding-agent/src/core/slash-commands.ts`.
pub const BUILTIN_SLASH_COMMANDS_LOCAL: &[BuiltinSlashCommand] = &[
    BuiltinSlashCommand {
        name: "settings",
        description: "Open settings menu",
        argument_hint: None,
    },
    BuiltinSlashCommand {
        name: "model",
        description: "Select model (opens selector UI)",
        argument_hint: Some("<provider/model>"),
    },
    BuiltinSlashCommand {
        name: "tree",
        description: "Navigate session tree (switch branches)",
        argument_hint: None,
    },
    BuiltinSlashCommand {
        name: "thinking",
        description: "Set thinking level",
        argument_hint: Some("<level>"),
    },
    BuiltinSlashCommand {
        name: "scoped-models",
        description: "Enable/disable models for Ctrl+P cycling",
        argument_hint: None,
    },
    BuiltinSlashCommand {
        name: "export",
        description: "Export session (HTML default, or specify path: .html/.jsonl)",
        argument_hint: None,
    },
    BuiltinSlashCommand {
        name: "import",
        description: "Import a JSONL session, preserving its ID and working directory",
        argument_hint: None,
    },
    BuiltinSlashCommand {
        name: "share",
        description: "Share session via a Radius artifact or a GitHub gist",
        argument_hint: None,
    },
    BuiltinSlashCommand {
        name: "bug",
        description: "Report a bug to the Pi developers",
        argument_hint: Some("<description>"),
    },
    BuiltinSlashCommand {
        name: "copy",
        description: "Copy last agent message to clipboard",
        argument_hint: None,
    },
    BuiltinSlashCommand {
        name: "name",
        description: "Set session display name",
        argument_hint: None,
    },
    BuiltinSlashCommand {
        name: "session",
        description: "Show session info and stats",
        argument_hint: None,
    },
    BuiltinSlashCommand {
        name: "changelog",
        description: "Show changelog entries",
        argument_hint: None,
    },
    BuiltinSlashCommand {
        name: "hotkeys",
        description: "Show all keyboard shortcuts",
        argument_hint: None,
    },
    BuiltinSlashCommand {
        name: "fork",
        description: "Create a new fork from a previous user message",
        argument_hint: None,
    },
    BuiltinSlashCommand {
        name: "clone",
        description: "Duplicate the current session at the current position",
        argument_hint: None,
    },
    BuiltinSlashCommand {
        name: "trust",
        description: "Save project trust decision for future sessions",
        argument_hint: None,
    },
    BuiltinSlashCommand {
        name: "login",
        description: "Configure provider authentication",
        argument_hint: Some("<provider>"),
    },
    BuiltinSlashCommand {
        name: "logout",
        description: "Remove provider authentication",
        argument_hint: None,
    },
    BuiltinSlashCommand {
        name: "new",
        description: "Start a new session",
        argument_hint: None,
    },
    BuiltinSlashCommand {
        name: "compact",
        description: "Manually compact the session context",
        argument_hint: None,
    },
    BuiltinSlashCommand {
        name: "resume",
        description: "Resume another session",
        argument_hint: None,
    },
    BuiltinSlashCommand {
        name: "reload",
        description: "Reload keybindings, extensions, skills, prompts, themes, and context files",
        argument_hint: None,
    },
    BuiltinSlashCommand {
        name: "quit",
        description: "Quit pi",
        argument_hint: None,
    },
];

struct SettingsThemeBridge(Arc<dyn InteractiveSettingsManager>);

impl ThemeSettingsSource for SettingsThemeBridge {
    fn get_theme_setting(&self) -> Option<String> {
        self.0.get_theme_setting()
    }
}

pub(crate) struct FooterDataBridge(pub(crate) Arc<dyn InteractiveFooterDataProvider>);

impl ReadonlyFooterDataProvider for FooterDataBridge {
    fn get_git_branch(&self) -> Option<String> {
        self.0.get_git_branch()
    }

    fn get_extension_statuses(&self) -> indexmap::IndexMap<String, String> {
        self.0.get_extension_statuses()
    }

    fn get_available_provider_count(&self) -> usize {
        self.0.get_available_provider_count()
    }

    fn on_branch_change(&self, cb: Arc<dyn Fn() + Send + Sync>) -> pi_js::Unsubscribe {
        self.0.on_branch_change(cb)
    }
}

pub fn to_interactive_tui_mode(mode: SettingsTuiMode) -> InteractiveTuiMode {
    match mode {
        SettingsTuiMode::Regular => InteractiveTuiMode::Regular,
        SettingsTuiMode::Fullscreen => InteractiveTuiMode::Fullscreen,
    }
}

pub fn to_settings_tui_mode(mode: TuiMode) -> SettingsTuiMode {
    match mode {
        TuiMode::Regular => SettingsTuiMode::Regular,
        TuiMode::Fullscreen => SettingsTuiMode::Fullscreen,
    }
}

pub fn to_pi_tui_mode(mode: InteractiveTuiMode) -> TuiMode {
    match mode {
        InteractiveTuiMode::Regular => TuiMode::Regular,
        InteractiveTuiMode::Fullscreen => TuiMode::Fullscreen,
    }
}

pub(crate) fn parse_git_url_local(source: &str) -> Option<(String, String, Option<String>)> {
    let trimmed = source.strip_prefix("git:").unwrap_or(source).trim();
    if trimmed.is_empty() || (!source.starts_with("git:") && !trimmed.contains("://") && !trimmed.starts_with("git@")) {
        return None;
    }
    let (without_ref, git_ref) = if let Some(idx) = trimmed.rfind('@')
        && !trimmed[idx + 1..].is_empty()
        && !trimmed[idx + 1..].contains('/')
        && idx > trimmed.find("://").map(|i| i + 3).unwrap_or(0)
    {
        (&trimmed[..idx], Some(trimmed[idx + 1..].to_string()))
    } else {
        (trimmed, None)
    };
    if let Some(rest) = without_ref.strip_prefix("git@")
        && let Some((host, path)) = rest.split_once(':')
    {
        return Some((host.to_string(), path.trim_end_matches(".git").to_string(), git_ref));
    }
    if let Ok(parsed) = url::Url::parse(without_ref)
        && let Some(host) = parsed.host_str()
    {
        let p = parsed.path().trim_start_matches('/').trim_end_matches(".git");
        return Some((host.to_string(), p.to_string(), git_ref));
    }
    let mut parts = without_ref.splitn(2, '/');
    let host = parts.next()?;
    let path = parts.next()?;
    if host.contains('.') && !path.is_empty() {
        Some((host.to_string(), path.trim_end_matches(".git").to_string(), git_ref))
    } else {
        None
    }
}

fn upgrade_weak_slot(slot: &Mutex<Option<Weak<InteractiveModeInner>>>) -> Option<Arc<InteractiveModeInner>> {
    slot.lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(Weak::upgrade)
}

impl InteractiveMode {
    pub fn new(runtime_host: Arc<dyn InteractiveRuntimeHost>, options: InteractiveModeOptions) -> Self {
        let session = runtime_host.session();
        let settings_manager = session.settings_manager();
        let session_manager = session.session_manager();
        let hooks = options.hooks.clone().unwrap_or_default();

        set_capability_overrides(settings_manager.get_terminal_capability_overrides());
        let tui_mode = options.tui_mode.unwrap_or_else(|| settings_manager.get_tui_mode());
        let mut stored_options = options.clone();
        stored_options.tui_mode = Some(tui_mode);
        let auto_trust_on_reload_cwd = options.auto_trust_on_reload_cwd.clone();

        let weak_slot: Arc<Mutex<Option<Weak<InteractiveModeInner>>>> = Arc::new(Mutex::new(None));
        let weak_for_paste = Arc::clone(&weak_slot);
        let on_right_click_paste: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            if let Some(inner) = upgrade_weak_slot(&weak_for_paste) {
                let mode = InteractiveMode { inner };
                if let Ok(handle) = tokio::runtime::Handle::try_current() {
                    handle.spawn(async move {
                        mode.handle_right_click_paste().await;
                    });
                }
            }
        });

        let copy_cb = Arc::clone(&hooks.clipboard_copy);
        let copy_selection: pi_tui::tui_alt_screen::CopySelectionFn = Arc::new(move |text: String| {
            let cb = Arc::clone(&copy_cb);
            Box::pin(async move {
                match cb(&text).await {
                    Ok(()) => pi_tui::tui_alt_screen::CopySelectionResult::Bool(true),
                    Err(err) => pi_tui::tui_alt_screen::CopySelectionResult::Message(err.to_string()),
                }
            })
        });
        let renderer = create_interactive_tui(InteractiveTuiOptions {
            tui_mode: Some(to_interactive_tui_mode(tui_mode)),
            show_hardware_cursor: settings_manager.get_show_hardware_cursor(),
            log_directory: get_agent_dir(),
            terminal: options.terminal.clone(),
            on_right_click_paste: Some(Arc::clone(&on_right_click_paste)),
            fullscreen_copy_on_select: Some(settings_manager.get_fullscreen_copy_on_select()),
            fullscreen_wheel_scroll_lines: Some(settings_manager.get_fullscreen_wheel_scroll_lines()),
            copy_selection: Some(copy_selection),
        });

        let renderer_slot: Arc<Mutex<Option<Weak<InteractiveModeInner>>>> = Arc::clone(&weak_slot);
        let ui = Arc::new(create_interactive_tui_reference(Arc::new(move || {
            if let Some(inner) = upgrade_weak_slot(&renderer_slot) {
                return inner
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .renderer
                    .as_tui_arc();
            }
            // During early construction before weak_slot is populated, fall back to a dummy or unreachable is avoided
            unreachable!("InteractiveTuiReference accessed before InteractiveModeInner construction finished")
        })));

        let header_container = Container::new();
        let loaded_resources_container = Container::new();
        let chat_container = Container::new();
        let document_container = Container::new();
        document_container.add_child(Arc::new(header_container.clone()));
        document_container.add_child(Arc::new(loaded_resources_container.clone()));
        document_container.add_child(Arc::new(chat_container.clone()));

        let pending_messages_container = Container::new();
        let status_container = Container::new();
        let widget_container_above = Container::new();
        let widget_container_below = Container::new();

        let keybindings = hooks
            .keybindings
            .clone()
            .unwrap_or_else(|| Arc::new(KeybindingsManager::create(Some(&runtime_host.agent_dir()))));
        sync_tui_keybindings(&keybindings);

        let editor_padding_x = settings_manager.get_editor_padding_x();
        let autocomplete_max_visible = settings_manager.get_autocomplete_max_visible();
        let default_editor = CustomEditor::new(
            Arc::clone(&ui) as Arc<dyn TUI + Send + Sync>,
            get_editor_theme(),
            (*keybindings).clone(),
            Some(CustomEditorOptions {
                padding_x: Some(editor_padding_x as f64),
                autocomplete_max_visible: Some(autocomplete_max_visible as f64),
                embed_working_status: Some(true),
            }),
        );
        let editor = ActiveEditor::Default(default_editor.clone());
        let editor_container = Container::new();
        editor_container.add_child(editor.as_component());

        let footer_data_provider = (hooks.create_footer_data_provider)(&session_manager.get_cwd());
        let footer = FooterComponent::new(
            session.as_footer_session(),
            Arc::new(FooterDataBridge(Arc::clone(&footer_data_provider))),
        );
        footer.set_auto_compact_enabled(session.auto_compaction_enabled());
        let footer_container = Container::new();
        footer_container.add_child(Arc::new(footer.clone()));

        let hide_thinking_block = settings_manager.get_hide_thinking_block();
        let output_pad = settings_manager.get_output_pad();

        let (themes, _, _) = session.resource_loader().get_themes();
        let _ = set_registered_themes(themes);

        let weak_for_theme_mgr = Arc::clone(&weak_slot);
        let weak_for_theme_err = Arc::clone(&weak_slot);
        let weak_for_theme_chg = Arc::clone(&weak_slot);
        let host_for_theme_mgr = Arc::clone(&runtime_host);

        let theme_controller = InteractiveThemeController::new(
            Arc::new(renderer.clone()) as Arc<dyn TUI + Send + Sync>,
            InteractiveThemeControllerOptions {
                get_settings_manager: Arc::new(move || {
                    let mgr = if let Some(inner) = upgrade_weak_slot(&weak_for_theme_mgr) {
                        inner.runtime_host.session().settings_manager()
                    } else {
                        host_for_theme_mgr.session().settings_manager()
                    };
                    Arc::new(SettingsThemeBridge(mgr))
                }),
                show_error: Arc::new(move |msg| {
                    if let Some(inner) = upgrade_weak_slot(&weak_for_theme_err) {
                        InteractiveMode { inner }.show_error(msg);
                    }
                }),
                on_changed: Arc::new(move || {
                    if let Some(inner) = upgrade_weak_slot(&weak_for_theme_chg) {
                        InteractiveMode { inner }.update_editor_border_color();
                    }
                }),
                initial_theme_setting: options.initial_theme_setting.clone(),
            },
        );

        let weak_for_prog_term = Arc::clone(&weak_slot);
        let weak_for_prog_name = Arc::clone(&weak_slot);
        let fallback_terminal = renderer.terminal();
        let program_status = ProgramStatusReporter::new(
            Arc::new(move || {
                if let Some(inner) = upgrade_weak_slot(&weak_for_prog_term) {
                    return inner
                        .state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .renderer
                        .terminal();
                }
                Arc::clone(&fallback_terminal)
            }),
            Arc::new(move || {
                upgrade_weak_slot(&weak_for_prog_name)
                    .and_then(|inner| inner.runtime_host.session().session_manager().get_session_name())
            }),
        );

        let weak_for_mermaid = Arc::clone(&weak_slot);
        let host_for_mermaid = Arc::clone(&runtime_host);
        let mermaid_markdown_transformer = create_mermaid_markdown_transformer(MermaidTransformerOptions {
            get_mode: Arc::new(move || {
                if let Some(inner) = upgrade_weak_slot(&weak_for_mermaid) {
                    return inner
                        .runtime_host
                        .session()
                        .settings_manager()
                        .get_mermaid_rendering_mode();
                }
                host_for_mermaid
                    .session()
                    .settings_manager()
                    .get_mermaid_rendering_mode()
            }),
            theme: Some(theme()),
        });

        let inner = Arc::new(InteractiveModeInner {
            runtime_host: Arc::clone(&runtime_host),
            ui: Arc::clone(&ui),
            hooks,
            header_container,
            loaded_resources_container,
            chat_container,
            document_container,
            pending_messages_container,
            status_container,
            widget_container_above,
            widget_container_below,
            editor_container,
            footer_container,
            default_editor,
            footer,
            footer_data_provider,
            keybindings,
            version: (*VERSION).to_string(),
            idle_status: IdleStatus::new(),
            program_status,
            theme_controller,
            mermaid_markdown_transformer,
            shutdown_notify: Arc::new(tokio::sync::Notify::new()),
            state: Mutex::new(InteractiveModeState {
                renderer,
                main_screen_render_state: None,
                transcript_scroll_view: None,
                fullscreen_layout_root: None,
                editor,
                editor_component_factory: None,
                autocomplete_provider: None,
                autocomplete_provider_wrappers: Vec::new(),
                fd_path: None,
                active_selector_token: 0,
                next_selector_token: 1,
                active_selector_dispose: None,
                is_initialized: false,
                is_shutting_down: false,
                on_input_callback: None,
                pending_user_inputs: std::collections::VecDeque::new(),
                signal_handles: Vec::new(),
                active_status_indicator: None,
                active_working_indicator_embedded: false,
                working_message: None,
                working_visible: true,
                working_indicator_options: None,
                hidden_thinking_label: "Thinking...".to_string(),
                last_sigint_time_ms: 0.0,
                last_escape_time_ms: 0.0,
                changelog_markdown: None,
                extension_changelog_updates: Vec::new(),
                startup_notices_shown: false,
                cache_warming_notice: None,
                anthropic_subscription_warning_shown: false,
                last_status_spacer: None,
                last_status_text: None,
                last_status_text_component: None,
                last_status_message: Arc::new(Mutex::new(String::new())),
                managed_tool_status_started: false,
                streaming_component: None,
                streaming_component_arc: None,
                entries_rendered_by_boundary_compaction: IndexSet::new(),
                streaming_message: None,
                tool_output_expanded: false,
                hide_thinking_block,
                output_pad,
                skill_commands: IndexMap::new(),
                unsubscribe: None,
                is_bash_mode: false,
                bash_component: None,
                pending_bash_components: Vec::new(),
                auto_compaction_escape_handler: None,
                retry_escape_handler: None,
                compaction_queued_messages: Vec::new(),
                shutdown_requested: false,
                bug_report_hint_shown: false,
                install_change_warning_shown: false,
                extension_dialogs: Vec::new(),
                next_blocking_overlay_id: 0,
                extension_overlays: Vec::new(),
                next_overlay_entry_id: 1,
                extension_terminal_input_subscriptions: Vec::new(),
                next_input_sub_id: 1,
                extension_widgets_above: IndexMap::new(),
                extension_widgets_below: IndexMap::new(),
                custom_footer: None,
                built_in_header: None,
                custom_header: None,
                options: stored_options,
                auto_trust_on_reload_cwd,
                default_editor_on_escape: Arc::new(Mutex::new(None)),
                default_editor_on_ctrl_d: Arc::new(Mutex::new(None)),
                default_editor_on_paste_image: Arc::new(Mutex::new(None)),
                default_editor_on_extension_shortcut: Arc::new(Mutex::new(None)),
                default_editor_actions: IndexMap::new(),
                resource_trackers: Vec::new(),
                chat_trackers: Vec::new(),
                pending_trackers: Vec::new(),
                pending_tool_components: IndexMap::new(),
            }),
        });

        *weak_slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::downgrade(&inner));

        // Now that `weak_slot` is populated, `InteractiveTuiReference` can safely resolve `self.renderer`.
        let mode = Self { inner };
        mode.ui().set_clear_on_shrink(settings_manager.get_clear_on_shrink());

        let mode_invalidate = mode.clone();
        mode.inner.runtime_host.set_before_session_invalidate(Arc::new(move || {
            mode_invalidate.reset_extension_ui();
        }));
        let mode_rebind = mode.clone();
        mode.inner.runtime_host.set_rebind_session(Arc::new(move || {
            let m = mode_rebind.clone();
            Box::pin(async move {
                m.rebind_current_session(true).await;
                m.inner.theme_controller.apply_from_settings();
            })
        }));

        mode
    }

    pub fn get_autocomplete_source_tag(&self, source_info: Option<&SourceInfo>) -> Option<String> {
        let info = source_info?;
        if info.source == "builtin" {
            return None;
        }
        let scope_prefix = match info.scope.as_str() {
            "user" => "u",
            "project" => "p",
            _ => "t",
        };
        let source = info.source.trim();
        if source == "auto" || source == "local" || source == "cli" {
            return Some(scope_prefix.to_string());
        }
        if source.starts_with("npm:") {
            return Some(format!("{scope_prefix}:{source}"));
        }
        if let Some((host, path, git_ref)) = parse_git_url_local(source) {
            let ref_suffix = git_ref.map(|r| format!("@{r}")).unwrap_or_default();
            return Some(format!("{scope_prefix}:git:{host}/{path}{ref_suffix}"));
        }
        Some(scope_prefix.to_string())
    }

    fn prefix_autocomplete_description(
        &self,
        description: Option<&str>,
        source_info: Option<&SourceInfo>,
    ) -> Option<String> {
        let Some(tag) = self.get_autocomplete_source_tag(source_info) else {
            return description.map(str::to_string);
        };
        Some(match description {
            Some(desc) if !desc.is_empty() => format!("[{tag}] {desc}"),
            _ => format!("[{tag}]"),
        })
    }

    pub(crate) fn get_built_in_command_conflict_diagnostics(
        &self,
        extension_runner: &dyn InteractiveExtensionRunner,
    ) -> Vec<ResourceDiagnostic> {
        let builtin_names: IndexSet<&str> = BUILTIN_SLASH_COMMANDS_LOCAL.iter().map(|c| c.name).collect();
        extension_runner
            .get_registered_commands()
            .into_iter()
            .filter(|cmd| builtin_names.contains(cmd.name.as_str()))
            .map(|cmd| ResourceDiagnostic {
                diagnostic_type: DiagnosticLevel::Warning,
                message: if cmd.invocation_name == cmd.name {
                    format!(
                        "Extension command '/{}' conflicts with built-in interactive command. Skipping in autocomplete.",
                        cmd.name
                    )
                } else {
                    format!(
                        "Extension command '/{}' conflicts with built-in interactive command. Available as '/{}'.",
                        cmd.name, cmd.invocation_name
                    )
                },
                path: (!cmd.source_info.path.is_empty()).then_some(cmd.source_info.path),
                collision: None,
            })
            .collect()
    }

    fn create_base_autocomplete_provider(&self) -> Arc<dyn AutocompleteProvider + Send + Sync> {
        let mut slash_commands: Vec<SlashCommand> = BUILTIN_SLASH_COMMANDS_LOCAL
            .iter()
            .map(|cmd| {
                let this = self.clone();
                let completions: Option<pi_tui::autocomplete::GetArgumentCompletions> = match cmd.name {
                    "model" => Some(Arc::new(move |prefix: &str| {
                        let m = this.clone();
                        let p = prefix.to_string();
                        Box::pin(async move {
                            let sess = m.session();
                            let scoped = sess.scoped_models();
                            let models: Vec<pi_ai::Model> = if !scoped.is_empty() {
                                scoped.into_iter().map(|s| s.model).collect()
                            } else {
                                sess.model_runtime().get_available_snapshot()
                            };
                            if models.is_empty() {
                                return None;
                            }
                            create_fuzzy_autocomplete_items(
                                &models,
                                &p,
                                |model| get_model_search_text(&model.id, &model.provider, Some(&model.name)),
                                |model| pi_tui::AutocompleteItem {
                                    value: format!("{}/{}", model.provider, model.id),
                                    label: model.id.clone(),
                                    description: Some(model.provider.clone()),
                                },
                            )
                        })
                    })),
                    "thinking" => Some(Arc::new(move |prefix: &str| {
                        let m = this.clone();
                        let p = prefix.to_string();
                        Box::pin(async move {
                            let levels: Vec<String> = m
                                .session()
                                .get_available_thinking_levels()
                                .into_iter()
                                .map(|lvl| lvl.to_string())
                                .collect();
                            create_fuzzy_autocomplete_items(
                                &levels,
                                &p,
                                |lvl| lvl.clone(),
                                |lvl| pi_tui::AutocompleteItem {
                                    value: lvl.clone(),
                                    label: lvl.clone(),
                                    description: None,
                                },
                            )
                        })
                    })),
                    "login" => Some(Arc::new(move |prefix: &str| {
                        let m = this.clone();
                        let p = prefix.to_string();
                        Box::pin(async move {
                            let providers = get_login_provider_completion_options(&m.get_login_provider_options(None));
                            create_fuzzy_autocomplete_items(
                                &providers,
                                &p,
                                get_login_provider_search_text,
                                |provider| pi_tui::AutocompleteItem {
                                    value: provider.id.clone(),
                                    label: provider.id.clone(),
                                    description: Some(format_login_provider_completion_description(provider)),
                                },
                            )
                        })
                    })),
                    _ => None,
                };
                SlashCommand {
                    name: cmd.name.to_string(),
                    description: Some(cmd.description.to_string()),
                    argument_hint: cmd.argument_hint.map(str::to_string),
                    get_argument_completions: completions,
                }
            })
            .collect();

        let session = self.session();
        let builtin_names: IndexSet<String> = slash_commands.iter().map(|c| c.name.clone()).collect();

        for tpl in session.prompt_templates() {
            let desc = self.prefix_autocomplete_description(tpl.description.as_deref(), tpl.source_info.as_ref());
            slash_commands.push(SlashCommand {
                name: tpl.name,
                description: desc,
                argument_hint: tpl.argument_hint,
                get_argument_completions: None,
            });
        }

        for cmd in session.extension_runner().get_registered_commands() {
            if builtin_names.contains(&cmd.name) {
                continue;
            }
            let desc = self.prefix_autocomplete_description(cmd.description.as_deref(), Some(&cmd.source_info));
            slash_commands.push(SlashCommand {
                name: cmd.invocation_name,
                description: desc,
                argument_hint: None,
                get_argument_completions: cmd.get_argument_completions,
            });
        }

        let enable_skill_commands = session.settings_manager().get_enable_skill_commands();
        let mut new_skill_commands = IndexMap::new();
        if enable_skill_commands {
            let (skills, _) = session.resource_loader().get_skills();
            for skill in skills {
                let command_name = format!("skill:{}", skill.name);
                new_skill_commands.insert(command_name.clone(), skill.file_path);
                let desc = self.prefix_autocomplete_description(Some(&skill.description), skill.source_info.as_ref());
                slash_commands.push(SlashCommand {
                    name: command_name,
                    description: desc,
                    argument_hint: None,
                    get_argument_completions: None,
                });
            }
        }

        let (cwd, fd_path) = {
            let mut st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            st.skill_commands = new_skill_commands;
            (session.session_manager().get_cwd(), st.fd_path.clone())
        };

        let commands: Vec<CombinedCommand> = slash_commands.into_iter().map(CombinedCommand::SlashCommand).collect();
        Arc::new(CombinedAutocompleteProvider::new(
            Some(commands),
            &cwd,
            fd_path.as_deref(),
        ))
    }

    pub fn setup_autocomplete_provider(&self) {
        let mut provider = self.create_base_autocomplete_provider();
        let wrappers = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .autocomplete_provider_wrappers
            .clone();
        let mut triggers = IndexSet::new();
        for wrapper in wrappers {
            provider = wrapper(provider);
            if let Some(chars) = provider.trigger_characters() {
                for ch in chars {
                    triggers.insert(ch);
                }
            }
        }
        if !triggers.is_empty() {
            provider.set_trigger_characters(Some(triggers.into_iter().collect()));
        }

        let editor = {
            let mut st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            st.autocomplete_provider = Some(Arc::clone(&provider));
            st.editor.clone()
        };
        self.inner
            .default_editor
            .set_autocomplete_provider(Arc::clone(&provider));
        if !editor.is_default(&self.inner.default_editor) {
            editor.set_autocomplete_provider(provider);
        }
    }

    pub(crate) fn show_startup_notices_if_needed(&self) {
        let (changelog_md, ext_updates) = {
            let mut st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if st.startup_notices_shown {
                return;
            }
            st.startup_notices_shown = true;
            (st.changelog_markdown.clone(), st.extension_changelog_updates.clone())
        };
        if changelog_md.is_none() && ext_updates.is_empty() {
            return;
        }
        if !self.inner.chat_container.children().is_empty() {
            self.add_chat_component(Arc::new(Spacer::new(Some(1))), None, None, None);
        }
        self.add_chat_component(Arc::new(DynamicBorder::default()), None, None, None);

        let collapse = self.settings_manager().get_collapse_changelog();
        if let Some(ref md) = changelog_md {
            if collapse {
                let version = extract_changelog_version(md).unwrap_or_else(|| self.inner.version.clone());
                let condensed = format!(
                    "Updated to v{version}. Use {} to view full changelog.",
                    theme().bold("/changelog")
                );
                self.add_chat_component(
                    Arc::new(Text::new(Some(&condensed), Some(1), Some(0), None)),
                    None,
                    None,
                    None,
                );
            } else {
                self.add_chat_component(
                    Arc::new(ThemedText::new(
                        Arc::new(|| {
                            let t = theme();
                            t.bold(&t.fg(ThemeColor::Accent, "What's New"))
                        }),
                        Some(1),
                        Some(0),
                    )),
                    None,
                    None,
                    None,
                );
                self.add_chat_component(Arc::new(Spacer::new(Some(1))), None, None, None);
                self.add_chat_component(self.create_changelog_markdown(md.trim(), 1, 0, None), None, None, None);
                self.add_chat_component(Arc::new(Spacer::new(Some(1))), None, None, None);
            }
        }
        if !ext_updates.is_empty() {
            if changelog_md.is_some() {
                self.add_chat_component(Arc::new(Spacer::new(Some(1))), None, None, None);
            }
            if collapse {
                let condensed: Vec<String> = ext_updates
                    .iter()
                    .map(|u| format!("Updated {} to v{}.", u.name, u.version))
                    .collect();
                self.add_chat_component(
                    Arc::new(Text::new(Some(&condensed.join("\n")), Some(1), Some(0), None)),
                    None,
                    None,
                    None,
                );
            } else {
                self.add_chat_component(
                    Arc::new(ThemedText::new(
                        Arc::new(|| {
                            let t = theme();
                            t.bold(&t.fg(ThemeColor::Accent, "Extension Updates"))
                        }),
                        Some(1),
                        Some(0),
                    )),
                    None,
                    None,
                    None,
                );
                self.add_chat_component(Arc::new(Spacer::new(Some(1))), None, None, None);
                let combined: Vec<&str> = ext_updates.iter().map(|u| u.markdown.as_str()).collect();
                self.add_chat_component(
                    self.create_changelog_markdown(&combined.join("\n\n"), 1, 0, None),
                    None,
                    None,
                    None,
                );
                self.add_chat_component(Arc::new(Spacer::new(Some(1))), None, None, None);
            }
        }
        self.add_chat_component(Arc::new(DynamicBorder::default()), None, None, None);
    }

    fn mount_interactive_tui(&self, tui: &InteractiveTui, components: &[Arc<dyn Component>]) {
        for component in components {
            tui.add_child(Arc::clone(component));
        }
        if let Some(alt) = tui.as_alt_screen() {
            let root = self
                .inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .fullscreen_layout_root
                .clone();
            if let Some(root) = root {
                alt.set_layout_root(Some(root));
            }
        }
    }

    pub fn stop_interactive_tui(&self, fullscreen_exit_output: FullscreenExitOutput) {
        let renderer = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .renderer
            .clone();
        let is_fullscreen = renderer.mode() == TuiMode::Fullscreen;
        if is_fullscreen && fullscreen_exit_output == FullscreenExitOutput::Transcript {
            if let Some(alt) = renderer.as_alt_screen() {
                while alt.has_overlay_entries() {
                    alt.hide_overlay();
                }
            }
            self.switch_tui_mode(SettingsTuiMode::Regular, false, false);
            let updated_renderer = self
                .inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .renderer
                .clone();
            updated_renderer.render_now(None);
        }
        let preserve = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .renderer
            .mode()
            == TuiMode::Fullscreen;
        self.ui().stop(Some(TuiStopOptions {
            preserve_screen: Some(preserve),
        }));
    }

    pub fn switch_tui_mode(&self, mode: SettingsTuiMode, restore_progress: bool, start_renderer: bool) -> bool {
        let previous_ui = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .renderer
            .clone();
        let target_mode = to_pi_tui_mode(to_interactive_tui_mode(mode));
        if target_mode == previous_ui.mode() {
            return true;
        }
        let has_overlays = match &previous_ui {
            InteractiveTui::Main(m) => m.has_overlay_entries(),
            InteractiveTui::Alt(a) => a.has_overlay_entries(),
        };
        if has_overlays {
            return false;
        }

        let components = previous_ui.children();
        let focus = match &previous_ui {
            InteractiveTui::Main(m) => m.get_focused_component(),
            InteractiveTui::Alt(a) => a.get_focused_component(),
        };
        let terminal = previous_ui.terminal();
        let show_hardware_cursor = previous_ui.get_show_hardware_cursor();
        let clear_on_shrink = previous_ui.get_clear_on_shrink();
        let on_debug = previous_ui.on_debug();
        if let InteractiveTui::Main(main) = &previous_ui {
            self.inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .main_screen_render_state = Some(main.capture_render_state());
        }

        previous_ui.stop(Some(TuiStopOptions {
            preserve_screen: Some(true),
        }));
        previous_ui.set_focus(None);
        previous_ui.clear();
        if let Some(alt) = previous_ui.as_alt_screen() {
            alt.set_layout_root(None);
        }

        let settings = self.settings_manager();
        let weak = Arc::downgrade(&self.inner);
        let on_right_click_paste: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            if let Some(inner) = weak.upgrade() {
                let m = InteractiveMode { inner };
                if let Ok(handle) = tokio::runtime::Handle::try_current() {
                    handle.spawn(async move {
                        m.handle_right_click_paste().await;
                    });
                }
            }
        });

        let copy_cb = Arc::clone(&self.inner.hooks.clipboard_copy);
        let copy_selection: pi_tui::tui_alt_screen::CopySelectionFn = Arc::new(move |text: String| {
            let cb = Arc::clone(&copy_cb);
            Box::pin(async move {
                match cb(&text).await {
                    Ok(()) => pi_tui::tui_alt_screen::CopySelectionResult::Bool(true),
                    Err(err) => pi_tui::tui_alt_screen::CopySelectionResult::Message(err.to_string()),
                }
            })
        });
        let next_ui = create_interactive_tui(InteractiveTuiOptions {
            tui_mode: Some(to_interactive_tui_mode(mode)),
            show_hardware_cursor,
            log_directory: get_agent_dir(),
            terminal: Some(Arc::clone(&terminal)),
            on_right_click_paste: Some(on_right_click_paste),
            fullscreen_copy_on_select: Some(settings.get_fullscreen_copy_on_select()),
            fullscreen_wheel_scroll_lines: Some(settings.get_fullscreen_wheel_scroll_lines()),
            copy_selection: Some(copy_selection),
        });
        next_ui.set_clear_on_shrink(clear_on_shrink);
        next_ui.set_on_debug(on_debug);
        if let InteractiveTui::Main(main) = &next_ui {
            let render_state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .main_screen_render_state
                .take();
            if let Some(rs) = render_state {
                main.restore_render_state(rs);
            }
        }

        {
            let mut st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            st.renderer = next_ui.clone();
            st.options.tui_mode = Some(mode);
        }

        self.mount_interactive_tui(&next_ui, &components);
        next_ui.invalidate();
        next_ui.set_focus(focus);
        if !start_renderer {
            return true;
        }
        next_ui.start();
        self.inner.theme_controller.rebind_tui();
        self.rebind_extension_terminal_input_listeners();
        let sess = self.session();
        if restore_progress && settings.get_show_terminal_progress() && (sess.is_streaming() || sess.is_compacting()) {
            terminal.set_progress(true);
        }
        true
    }

    pub async fn init(&self) {
        if self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_initialized
        {
            return;
        }

        self.register_signal_handlers();

        let changelog = self.get_changelog_for_display();
        let ext_updates = self.get_extension_changelog_updates();
        {
            let mut st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            st.changelog_markdown = changelog;
            st.extension_changelog_updates = ext_updates;
        }

        let is_fullscreen = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .renderer
            .mode()
            == TuiMode::Fullscreen;
        if !is_fullscreen && let Some(notice) = self.get_model_scope_notice() {
            pi_js::console::log(&notice);
        }

        self.render_widgets();
        let settings = self.settings_manager();
        let scrollbar = settings.get_fullscreen_scrollbar();
        let viewport = crate::modes::interactive::chat_viewport::create_chat_viewport(
            crate::modes::interactive::chat_viewport::ChatViewportOptions {
                document: Arc::new(self.inner.document_container.clone()),
                pending_messages: Arc::new(self.inner.pending_messages_container.clone()),
                status: Arc::new(self.inner.status_container.clone()),
                editor: Arc::new(self.inner.editor_container.clone()),
                footer: Arc::new(self.inner.footer_container.clone()),
                widgets_above: Some(Arc::new(self.inner.widget_container_above.clone())),
                widgets_below: Some(Arc::new(self.inner.widget_container_below.clone())),
                scrollbar: Some(scrollbar),
                scrollbar_track_style: Some(Arc::new(|s| theme().fg(ThemeColor::ScrollbarTrack, s))),
                scrollbar_thumb_style: Some(Arc::new(|s| theme().fg(ThemeColor::ScrollbarThumb, s))),
            },
        );
        let renderer = {
            let mut st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            st.transcript_scroll_view = Some(viewport.transcript);
            st.fullscreen_layout_root = Some(viewport.root);
            st.renderer.clone()
        };

        let root_children: Vec<Arc<dyn Component>> = vec![
            Arc::new(self.inner.document_container.clone()),
            Arc::new(self.inner.pending_messages_container.clone()),
            Arc::new(self.inner.status_container.clone()),
            Arc::new(self.inner.widget_container_above.clone()),
            Arc::new(self.inner.editor_container.clone()),
            Arc::new(self.inner.widget_container_below.clone()),
            Arc::new(self.inner.footer_container.clone()),
        ];
        self.mount_interactive_tui(&renderer, &root_children);

        // Startup-only input handlers until managed tools are ready.
        let m1 = self.clone();
        self.set_default_editor_action("app.clear", Arc::new(move || m1.handle_ctrl_c()));
        let m2 = self.clone();
        self.set_default_editor_ctrl_d(Some(Arc::new(move || m2.handle_ctrl_d())));
        let m3 = self.clone();
        self.inner.default_editor.set_on_submit(Some(Arc::new(move |text| {
            m3.handle_startup_submit(text);
        })));
        self.ui().set_focus(Some(self.editor_component()));

        self.ui().start();
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_initialized = true;
        self.inner.program_status.report();
        self.ensure_png_transcoder();

        self.inner.theme_controller.apply_from_settings();
        self.inner.theme_controller.wait_for_terminal_colors().await;

        if self.should_show_startup_header() {
            let show_details = self.should_show_startup_details();
            let show_logo = supports_pi_logo();
            let version = self.inner.version.clone();
            let with_logo = move |hints: &str| -> String {
                if !show_logo {
                    return format!(
                        "{} {}\n{hints}",
                        pi_wordmark(),
                        theme().fg(ThemeColor::Dim, &format!("v{version}"))
                    );
                }
                let [top, bottom] = pi_logo_lines();
                format!(
                    "{top} {}\n{bottom} {hints}",
                    theme().fg(ThemeColor::Dim, &format!("v{version}"))
                )
            };
            let with_logo_collapsed = with_logo.clone();
            let with_logo_expanded = with_logo;

            let expanded_fn = Arc::new(move || {
                let expanded_instructions = [
                    key_hint(&"app.interrupt".to_string(), "to interrupt"),
                    key_hint(&"app.clear".to_string(), "to clear"),
                    raw_key_hint(&format!("{} twice", key_text(&"app.clear".to_string())), "to exit"),
                    key_hint(&"app.exit".to_string(), "to exit (empty)"),
                    key_hint(&"app.suspend".to_string(), "to suspend"),
                    key_hint(&"tui.editor.deleteToLineEnd".to_string(), "to delete to end"),
                    key_hint(&"app.thinking.cycle".to_string(), "to cycle thinking level"),
                    raw_key_hint(
                        &format!(
                            "{}/{}",
                            key_text(&"app.model.cycleForward".to_string()),
                            key_text(&"app.model.cycleBackward".to_string())
                        ),
                        "to cycle models",
                    ),
                    key_hint(&"app.model.select".to_string(), "to select model"),
                    key_hint(&"app.tools.expand".to_string(), "to expand tools"),
                    key_hint(&"app.thinking.toggle".to_string(), "to expand thinking"),
                    key_hint(&"app.editor.external".to_string(), "for external editor"),
                    raw_key_hint("/", "for commands"),
                    raw_key_hint("!", "to run bash"),
                    raw_key_hint("!!", "to run bash (no context)"),
                    key_hint(&"app.message.followUp".to_string(), "to queue follow-up"),
                    key_hint(&"app.message.dequeue".to_string(), "to edit all queued messages"),
                    key_hint(
                        &"app.clipboard.pasteImage".to_string(),
                        "to paste files on macOS, images, or text",
                    ),
                    raw_key_hint("drop files", "to attach"),
                ]
                .join("\n");
                let onboarding = theme().fg(
                    ThemeColor::Dim,
                    "Pi can explain its own features and look up its docs. Ask it how to use or extend Pi.",
                );
                format!("{}\n\n{onboarding}", with_logo_expanded(&expanded_instructions))
            });

            let collapsed_fn = Arc::new(move || {
                let compact_sep = theme().fg(ThemeColor::Muted, " · ");
                let compact_instructions = [
                    key_hint(&"app.interrupt".to_string(), "interrupt"),
                    raw_key_hint(
                        &format!(
                            "{}/{}",
                            key_text(&"app.clear".to_string()),
                            key_text(&"app.exit".to_string())
                        ),
                        "clear/exit",
                    ),
                    raw_key_hint("/", "commands"),
                    raw_key_hint("!", "bash"),
                    key_hint(&"app.tools.expand".to_string(), "more"),
                ]
                .join(&compact_sep);
                let suffix = if show_details { " and loaded resources" } else { "" };
                let compact_onboarding = theme().fg(
                    ThemeColor::Dim,
                    &format!(
                        "Press {} to show full startup help{suffix}.",
                        key_text(&"app.tools.expand".to_string())
                    ),
                );
                let onboarding = theme().fg(
                    ThemeColor::Dim,
                    "Pi can explain its own features and look up its docs. Ask it how to use or extend Pi.",
                );
                format!(
                    "{}\n{compact_onboarding}\n\n{onboarding}",
                    with_logo_collapsed(&compact_instructions)
                )
            });

            let header = BuiltInHeader::new(collapsed_fn, expanded_fn, self.get_startup_expansion_state(), 1, 0);
            if show_logo {
                let m = self.clone();
                header.set_on_logo_click(Some(Arc::new(move |col, row| {
                    let alt = m
                        .inner
                        .state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .renderer
                        .as_alt_screen()
                        .cloned();
                    play_pi_logo_3d(alt.as_ref(), col as usize, row as usize);
                })));
            }
            let header_arc: Arc<dyn Component> = Arc::new(header.clone());
            let header_exp: Arc<dyn Expandable> = Arc::new(header);
            self.inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .built_in_header = Some((Arc::clone(&header_arc), Some(header_exp)));
            self.inner.header_container.add_child(Arc::new(Spacer::new(Some(1))));
            self.inner.header_container.add_child(header_arc);
            self.inner.header_container.add_child(Arc::new(Spacer::new(Some(1))));
        } else {
            let empty_header: Arc<dyn Component> = Arc::new(Text::new(Some(""), Some(0), Some(0), None));
            self.inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .built_in_header = Some((Arc::clone(&empty_header), None));
            self.inner.header_container.add_child(empty_header);
        }

        if is_fullscreen {
            self.show_fullscreen_model_scope_notice();
        }
        self.ui().request_render(None);

        let this_fd = self.clone();
        let this_rg = self.clone();
        let (fd_path, _) = tokio::join!(
            crate::utils::tools_manager::ensure_tool(
                crate::utils::tools_manager::ManagedTool::Fd,
                Some(move |status: crate::utils::tools_manager::ToolStatus| {
                    this_fd.show_managed_tool_status(&status.into());
                }),
            ),
            crate::utils::tools_manager::ensure_tool(
                crate::utils::tools_manager::ManagedTool::Rg,
                Some(move |status: crate::utils::tools_manager::ToolStatus| {
                    this_rg.show_managed_tool_status(&status.into());
                }),
            ),
        );
        self.inner.state.lock().unwrap_or_else(|e| e.into_inner()).fd_path = fd_path;

        self.setup_key_handlers();
        self.setup_editor_submit_handler();
        self.ui().request_render(None);

        self.rebind_current_session(false).await;
        self.render_initial_messages();

        let m_theme = self.clone();
        on_theme_change(Some(Arc::new(move || {
            m_theme.ui().invalidate();
            m_theme.update_editor_border_color();
            m_theme.ui().request_render(None);
        })));

        let m_branch = self.clone();
        let _ = self.inner.footer_data_provider.on_branch_change(Arc::new(move || {
            m_branch.ui().request_render(None);
        }));

        self.update_available_provider_count();
        self.ui().render_now(None);
    }

    pub(crate) fn update_terminal_title(&self) {
        let cwd = self.session_manager().get_cwd();
        let cwd_basename = std::path::Path::new(&cwd)
            .file_name()
            .and_then(|s| s.to_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(&cwd);
        let session_name = self.session_manager().get_session_name();
        let title = if let Some(name) = session_name {
            format!("{} - {name} - {cwd_basename}", *APP_TITLE)
        } else {
            format!("{} - {cwd_basename}", *APP_TITLE)
        };
        self.ui().terminal().set_title(&title);
    }

    pub async fn run(&self) {
        self.init().await;

        if pi_js::env::var("PI_OFFLINE").as_deref() != Some("1") {
            let model_runtime = self.session().model_runtime();
            let this = self.clone();
            tokio::spawn(async move {
                let controller = Arc::new(pi_js::abort::AbortController::new());
                let signal = controller.signal();
                let ctrl_for_timeout = Arc::clone(&controller);
                let timeout = pi_js::time::set_timeout(15_000, move || {
                    ctrl_for_timeout.abort(None);
                });
                let _ = refresh_model_catalogs(model_runtime, signal).await;
                timeout.clear();
                this.update_available_provider_count();
            });
        }

        let this_ver = self.clone();
        let version_for_check = self.inner.version.clone();
        tokio::spawn(async move {
            if let Some(new_release) = crate::utils::version_check::check_for_new_pi_version(&version_for_check).await {
                this_ver.show_new_version_notification(&new_release);
            }
        });
        if !crate::core::package_manager::is_offline_mode_enabled()
            && let Some(core_sm) = self.session().settings_manager().as_core_settings_manager().cloned()
        {
            let this_pkg = self.clone();
            let cwd = self.session_manager().get_cwd();
            let agent_dir = self.inner.runtime_host.agent_dir();
            tokio::spawn(async move {
                let pkg_mgr = crate::core::package_manager::DefaultPackageManager::new(
                    crate::core::package_manager::PackageManagerOptions {
                        cwd,
                        agent_dir,
                        settings_manager: core_sm,
                        builtin_extensions: Vec::new(),
                    },
                );
                if let Ok(updates) = pkg_mgr.check_for_available_updates().await
                    && !updates.is_empty()
                {
                    let names: Vec<String> = updates.into_iter().map(|u| u.display_name).collect();
                    this_pkg.show_package_update_notification(&names);
                }
            });
        }
        let this_tmux = self.clone();
        tokio::spawn(async move {
            if let Some(warning) = this_tmux.check_tmux_keyboard_setup().await {
                this_tmux.show_warning(&warning);
            }
        });

        let (
            migrated_providers,
            startup_diagnostics,
            model_fallback_message,
            initial_message,
            initial_images,
            initial_messages,
        ) = {
            let st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            (
                st.options.migrated_providers.clone(),
                st.options.startup_diagnostics.clone(),
                st.options.model_fallback_message.clone(),
                st.options.initial_message.clone(),
                st.options.initial_images.clone(),
                st.options.initial_messages.clone(),
            )
        };

        for diagnostic in startup_diagnostics.unwrap_or_default() {
            match diagnostic.diagnostic_type {
                DiagnosticLevel::Error => self.show_error(&diagnostic.message),
                DiagnosticLevel::Warning => self.show_warning(&diagnostic.message),
                _ => self.show_status(&diagnostic.message),
            }
        }
        if let Some(providers) = migrated_providers
            && !providers.is_empty()
        {
            self.show_warning(&format!("Migrated credentials to auth.json: {}", providers.join(", ")));
        }
        if let Some(err) = self.session().model_runtime().get_error() {
            self.show_error(&format!("models.json error: {err}"));
        }
        if let Some(fallback_msg) = model_fallback_message {
            self.show_warning(&fallback_msg);
        }

        let model = self.session().model();
        self.maybe_warn_about_anthropic_subscription_auth(model.as_ref()).await;

        if let Some(msg) = initial_message {
            let opts = InteractivePromptOptions {
                images: initial_images,
                streaming_behavior: None,
            };
            if let Err(err) = self.session().prompt(&msg, Some(opts)).await {
                self.show_error(&err.to_string());
            }
        }

        if let Some(messages) = initial_messages {
            for msg in messages {
                if let Err(err) = self.session().prompt(&msg, None).await {
                    self.show_error(&err.to_string());
                }
            }
        }

        loop {
            if self
                .inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_shutting_down
            {
                break;
            }
            let Some(user_input) = self.get_user_input_or_shutdown().await else {
                break;
            };
            if let Err(err) = self.session().prompt(&user_input, None).await {
                self.show_error(&err.to_string());
            }
        }
    }

    pub async fn check_tmux_keyboard_setup(&self) -> Option<String> {
        if pi_js::env::var("TMUX").as_deref().unwrap_or("").is_empty() {
            return None;
        }

        async fn run_tmux_show(option: &str) -> Option<String> {
            let mut cmd = tokio::process::Command::new("tmux");
            cmd.args(["show", "-gv", option])
                .env_clear()
                .envs(pi_js::env::vars())
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            let fut = async move {
                let out = cmd.output().await.ok()?;
                if out.status.success() {
                    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
                } else {
                    None
                }
            };
            tokio::time::timeout(std::time::Duration::from_millis(2000), fut)
                .await
                .ok()
                .flatten()
        }

        let (extended_keys, extended_keys_format) =
            tokio::join!(run_tmux_show("extended-keys"), run_tmux_show("extended-keys-format"));
        let ext_keys = extended_keys?;
        if ext_keys != "on" && ext_keys != "always" {
            return Some(
                "tmux extended-keys is off. Modified Enter keys may not work. Add `set -g extended-keys on` to ~/.tmux.conf and restart tmux.".to_string(),
            );
        }
        if extended_keys_format.as_deref() == Some("xterm") {
            return Some(
                "tmux extended-keys-format is xterm. Pi works best with csi-u. Add `set -g extended-keys-format csi-u` to ~/.tmux.conf and restart tmux.".to_string(),
            );
        }
        None
    }
}

fn extract_changelog_version(markdown: &str) -> Option<String> {
    for line in markdown.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("##") {
            let clean = rest.trim().trim_start_matches('[');
            let ver: String = clean.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
            if ver.split('.').count() == 3 && !ver.ends_with('.') {
                return Some(ver);
            }
        }
    }
    None
}
