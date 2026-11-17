//! Port of packages/coding-agent/src/cli/config-selector.ts

//! TUI config selector for `pi config` command

use std::sync::{Arc, Mutex};

use pi_tui::{Terminal, TuiMainScreen};

use crate::core::keybindings::KeybindingsManager;
use crate::core::package_manager::ResolvedPaths;
use crate::core::settings_manager::{SettingsManager, SettingsScope};
use crate::modes::interactive::components::config_selector::{
    ConfigSelectorComponent, ConfigWriteScope, ScopedResolvedPaths,
};
use crate::modes::interactive::interactive_mode::sync_tui_keybindings;
use crate::modes::interactive::theme::theme::{init_theme, stop_theme_watcher};

#[derive(Clone)]
pub struct ConfigSelectorOptions {
    pub resolved_paths_global: ResolvedPaths,
    pub resolved_paths_project: ResolvedPaths,
    pub settings_manager: SettingsManager,
    pub cwd: String,
    pub agent_dir: String,
    pub write_scope: SettingsScope,
    pub project_mode_available: bool,
}

/// Show TUI config selector and return when closed
pub async fn select_config(options: ConfigSelectorOptions) -> pi_js::Result<()> {
    init_theme(options.settings_manager.get_theme().as_deref(), Some(true));
    let kb = KeybindingsManager::create(Some(&options.agent_dir));
    sync_tui_keybindings(&kb);

    let terminal = Arc::new(pi_tui::terminal::ProcessTerminal::new());
    let rows = usize::try_from(terminal.rows()).ok().filter(|&r| r > 0);
    let ui = Arc::new(TuiMainScreen::new(
        terminal,
        Some(options.settings_manager.get_show_hardware_cursor()),
        Some(&options.agent_dir),
    ));
    ui.set_clear_on_shrink(options.settings_manager.get_clear_on_shrink());

    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let tx_slot = Arc::new(Mutex::new(Some(tx)));

    let write_scope = match options.write_scope {
        SettingsScope::Global => ConfigWriteScope::Global,
        SettingsScope::Project => ConfigWriteScope::Project,
    };

    let ui_close = Arc::clone(&ui);
    let tx_close = Arc::clone(&tx_slot);
    let on_close: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        let sender = tx_close.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(tx) = sender {
            ui_close.stop(None);
            stop_theme_watcher();
            let _ = tx.send(());
        }
    });

    let ui_exit = Arc::clone(&ui);
    let on_exit: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        ui_exit.stop(None);
        stop_theme_watcher();
        std::process::exit(0);
    });

    let ui_render = Arc::clone(&ui);
    let request_render: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        ui_render.request_render(None);
    });

    let resolved_paths = ScopedResolvedPaths {
        global: options.resolved_paths_global,
        project: options.resolved_paths_project,
    };
    let selector = ConfigSelectorComponent::new(
        &resolved_paths,
        Arc::new(options.settings_manager),
        &options.cwd,
        &options.agent_dir,
        on_close,
        on_exit,
        request_render,
        rows,
        write_scope,
        options.project_mode_available,
    );

    ui.add_child(Arc::new(selector.clone()));
    ui.set_focus(Some(Arc::new(selector.get_resource_list().clone())));
    ui.start();

    let _ = rx.await;
    Ok(())
}
