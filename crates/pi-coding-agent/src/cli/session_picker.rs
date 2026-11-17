//! Port of packages/coding-agent/src/cli/session-picker.ts

//! TUI session selector for --resume flag

use std::sync::{Arc, Mutex};

use pi_js::BoxFuture;
use pi_js::abort::AbortSignal;
use pi_tui::TUI;

use crate::cli::startup_ui::{create_startup_tui, start_startup_tui};
use crate::core::keybindings::KeybindingsManager;
use crate::core::session_manager::SessionInfo;
use crate::core::settings_manager::SettingsManager;
use crate::modes::interactive::components::session_selector::{
    SessionSelectorComponent, SessionSelectorOptions, SessionsLoader as ComponentSessionsLoader,
};
use crate::modes::interactive::interactive_mode::sync_tui_keybindings;

pub type SessionsProgressCallback = Arc<dyn Fn(usize, usize, Option<&[SessionInfo]>) + Send + Sync>;

pub type SessionsLoader = Arc<
    dyn Fn(Option<SessionsProgressCallback>, Option<AbortSignal>) -> BoxFuture<pi_js::Result<Vec<SessionInfo>>>
        + Send
        + Sync,
>;

fn wrap_loader(loader: SessionsLoader) -> ComponentSessionsLoader {
    Arc::new(move |on_progress, signal| loader(on_progress, Some(signal)))
}

/// Show TUI session selector and return selected session path or None if cancelled
pub async fn select_session(
    current_sessions_loader: SessionsLoader,
    all_sessions_loader: SessionsLoader,
    settings_manager: &SettingsManager,
) -> pi_js::Result<Option<String>> {
    let ui = create_startup_tui(settings_manager).await?;
    let keybindings = Arc::new(KeybindingsManager::create(None));
    sync_tui_keybindings(&keybindings);

    let (tx, rx) = tokio::sync::oneshot::channel::<Option<String>>();
    let tx_slot = Arc::new(Mutex::new(Some(tx)));

    let ui_sel = Arc::clone(&ui);
    let tx_sel = Arc::clone(&tx_slot);
    let on_select: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |path: &str| {
        let sender = tx_sel.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(tx) = sender {
            ui_sel.stop(None);
            let _ = tx.send(Some(path.to_owned()));
        }
    });

    let ui_can = Arc::clone(&ui);
    let tx_can = Arc::clone(&tx_slot);
    let on_cancel: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        let sender = tx_can.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(tx) = sender {
            ui_can.stop(None);
            let _ = tx.send(None);
        }
    });

    let ui_exit = Arc::clone(&ui);
    let on_exit: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        ui_exit.stop(None);
        std::process::exit(0);
    });

    let ui_render = Arc::clone(&ui);
    let request_render: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        ui_render.request_render(None);
    });

    let selector = SessionSelectorComponent::new(
        wrap_loader(current_sessions_loader),
        wrap_loader(all_sessions_loader),
        on_select,
        on_cancel,
        on_exit,
        request_render,
        SessionSelectorOptions {
            rename_session: None,
            show_rename_hint: Some(false),
            keybindings: Some(keybindings),
        },
        None,
    );

    ui.add_child(Arc::new(selector.clone()));
    ui.set_focus(Some(Arc::new(selector.get_session_list().clone())));
    start_startup_tui(Arc::clone(&ui) as Arc<dyn TUI + Send + Sync>, settings_manager);

    Ok(rx.await.ok().flatten())
}
