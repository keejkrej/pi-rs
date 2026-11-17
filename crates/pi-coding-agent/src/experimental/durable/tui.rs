//! Port of packages/coding-agent/src/experimental/durable/tui.ts

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use pi_durable::ModelRef;
use pi_durable::harness::types::WhenBusy;
use pi_tui::{Component, TUI, Text, TuiMode, TuiStopOptions, set_capability_overrides};

use crate::config::get_agent_dir;
use crate::core::keybindings::KeybindingsManager;
use crate::core::settings_manager::SettingsManager;
use crate::experimental::client_tui_chat::ExperimentalChatView;
use crate::experimental::durable::runtime::{DurableController, DurableView, DurableViewSource, agent_of};
use crate::modes::interactive::chat_viewport::{ChatViewportOptions, create_chat_viewport};
use crate::modes::interactive::components::custom_editor::{CustomEditor, CustomEditorOptions};
use crate::modes::interactive::components::dynamic_border::DynamicBorder;
use crate::modes::interactive::components::footer::format_tokens;
use crate::modes::interactive::components::keybinding_hints::key_text;
use crate::modes::interactive::components::settings_selector::TuiMode as SettingsTuiMode;
use crate::modes::interactive::interactive_mode::sync_tui_keybindings;
use crate::modes::interactive::theme::theme::{ThemeColor, get_editor_theme, theme};
use crate::modes::interactive::theme::theme_controller::{
    InteractiveThemeController, InteractiveThemeControllerOptions, ThemeSettingsSource,
};
use crate::modes::interactive::tui_renderer::{InteractiveTuiMode, InteractiveTuiOptions, create_interactive_tui};

fn total_usage_tokens(view: &DurableView) -> i64 {
    let Some(raw) = view.conversation.docs.get("pi.usage") else {
        return 0;
    };
    let Ok(usage_state) =
        serde_json::from_value::<pi_durable::harness::types::UsageState>(serde_json::Value::Object(raw.clone()))
    else {
        return 0;
    };
    usage_state
        .models
        .values()
        .chain(usage_state.tools.values())
        .map(|u| u.total_tokens)
        .sum()
}

fn format_durable_footer(view: &DurableView) -> String {
    let agent = agent_of(&view.conversation);
    let model = agent
        .model
        .map(|m| format!("{}/{}", m.provider, m.model_id))
        .unwrap_or_else(|| "no-model".to_string());
    let thinking = agent
        .thinking_level
        .map(|t| format!("{t:?}").to_lowercase())
        .unwrap_or_else(|| "off".to_string());
    let tokens = format_tokens(total_usage_tokens(view));
    let conv_label = view
        .conversations
        .iter()
        .find(|c| c.id == view.conversation.conversation.id)
        .map(|c| c.label.as_str())
        .unwrap_or("root");
    let task_count = view.tasks.as_ref().map(|g| g.tasks.len()).unwrap_or(0);
    let expand_key = key_text(&"app.tools.expand".to_string());
    let text = format!(
        "{model} · {thinking} · {tokens} tok · [{conv_label}] · tasks:{task_count} · {expand_key} /compact /thinking /exit"
    );
    theme().fg(ThemeColor::Dim, &text)
}

/// Run the durable harness interactive TUI over one view source and controller.
pub async fn run_durable_tui(
    source: &DurableViewSource,
    controller: &DurableController,
    settings: &SettingsManager,
) -> pi_js::Result<()> {
    let initial = source.current();
    let settings_arc = Arc::new(settings.clone());
    let kb = KeybindingsManager::create(None);
    sync_tui_keybindings(&kb);
    set_capability_overrides(settings.get_terminal_capability_overrides());

    let tui_mode = match settings.get_tui_mode() {
        SettingsTuiMode::Regular => InteractiveTuiMode::Regular,
        SettingsTuiMode::Fullscreen => InteractiveTuiMode::Fullscreen,
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

    let chat = Arc::new(Mutex::new(ExperimentalChatView::new_with_ui(
        &initial.session.cwd,
        Some(ui.as_tui_arc()),
    )?));
    chat.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .apply(&initial.conversation);

    let footer = Arc::new(Text::new(
        Some(&format_durable_footer(&initial)),
        Some(1),
        Some(0),
        None,
    ));

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
        let settings_for_theme = Arc::clone(&settings_arc);
        let source_for_theme = source.clone();
        let chat_for_theme = Arc::clone(&chat);
        let footer_for_theme = Arc::clone(&footer);
        let ui_for_theme = ui.clone();
        InteractiveThemeControllerOptions {
            get_settings_manager: Arc::new(move || Arc::clone(&settings_for_theme) as Arc<dyn ThemeSettingsSource>),
            show_error: Arc::new(|_| {}),
            on_changed: Arc::new(move || {
                let view = source_for_theme.current();
                chat_for_theme
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .refresh_theme(&view.conversation);
                footer_for_theme.set_text(&format_durable_footer(&view));
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

    let unsub_view = {
        let source_sub = source.clone();
        let chat_sub = Arc::clone(&chat);
        let footer_sub = Arc::clone(&footer);
        let ui_sub = ui.clone();
        source.subscribe(Arc::new(move || {
            let view = source_sub.current();
            chat_sub
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .apply(&view.conversation);
            footer_sub.set_text(&format_durable_footer(&view));
            ui_sub.request_render(None);
        }))
    };

    let exit_notify = Arc::new(tokio::sync::Notify::new());
    let exited = Arc::new(AtomicBool::new(false));

    let ctrl_submit = controller.clone();
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
            let c = ctrl_submit.clone();
            tokio::spawn(async move {
                let _ = c.cycle_thinking().await;
            });
            return;
        }
        if trimmed == "/tasks" {
            let c = ctrl_submit.clone();
            tokio::spawn(async move {
                let _ = c.toggle_tasks().await;
            });
            return;
        }
        if trimmed == "/compact" || trimmed.starts_with("/compact ") {
            let instructions = trimmed
                .strip_prefix("/compact")
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            let c = ctrl_submit.clone();
            tokio::spawn(async move {
                let _ = c.compact(instructions).await;
            });
            return;
        }
        if let Some(arg) = trimmed.strip_prefix("/model ").map(str::trim)
            && let Some((provider, model_id)) = arg.split_once('/')
        {
            let c = ctrl_submit.clone();
            let model_ref = ModelRef {
                provider: provider.to_string(),
                model_id: model_id.to_string(),
            };
            tokio::spawn(async move {
                let _ = c.set_model(model_ref).await;
            });
            return;
        }

        let c = ctrl_submit.clone();
        tokio::spawn(async move {
            let _ = c.submit(trimmed, WhenBusy::FollowUp).await;
        });
    })));

    let ctrl_esc = controller.clone();
    editor.set_on_escape(Some(Arc::new(move || {
        let c = ctrl_esc.clone();
        tokio::spawn(async move {
            let _ = c.abort().await;
        });
    })));

    let exit_ctrl_d = Arc::clone(&exit_notify);
    let exited_ctrl_d = Arc::clone(&exited);
    editor.set_on_ctrl_d(Some(Arc::new(move || {
        exited_ctrl_d.store(true, Ordering::Release);
        exit_ctrl_d.notify_waiters();
    })));

    ui.start();
    ui.request_render(Some(true));
    while !exited.load(Ordering::Acquire) {
        exit_notify.notified().await;
    }
    ui.stop(Some(TuiStopOptions::default()));
    unsub_view.call();
    theme_controller.dispose();
    chat.lock().unwrap_or_else(PoisonError::into_inner).dispose();
    Ok(())
}
