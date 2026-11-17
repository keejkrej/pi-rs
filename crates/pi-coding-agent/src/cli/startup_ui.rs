//! Port of packages/coding-agent/src/cli/startup-ui.ts

use std::sync::{Arc, Mutex};

use indexmap::IndexSet;
use pi_tui::{TUI, TuiMainScreen, set_capability_overrides};

use crate::config::{APP_NAME, CONFIG_DIR_NAME, ENV_AGENT_DIR, PACKAGE_NAME, get_agent_dir, get_settings_path};
use crate::core::experimental::are_experimental_features_enabled;
use crate::core::keybindings::KeybindingsManager;
use crate::core::package_manager::{
    DefaultPackageManager, MissingSourceAction, PackageManagerOptions, ResolvedResource,
};
use crate::core::settings_manager::{SettingsManager, SettingsManagerCreateOptions};
use crate::modes::interactive::components::extension_input::{ExtensionInputComponent, ExtensionInputOptions};
use crate::modes::interactive::components::extension_selector::{ExtensionSelectorComponent, ExtensionSelectorOptions};
use crate::modes::interactive::components::first_time_setup::{
    FirstTimeSetupComponent, FirstTimeSetupOptions, FirstTimeSetupResult,
};
use crate::modes::interactive::interactive_mode::sync_tui_keybindings;
use crate::modes::interactive::theme::theme::{
    SYSTEM_THEME_NAME, Theme, get_terminal_theme, init_theme, load_theme_from_path, mark_terminal_colors_pending,
    resolve_theme_setting, set_registered_themes, set_terminal_colors, set_theme,
};
use crate::modes::interactive::theme::theme_controller::request_terminal_colors;

const OFFICIAL_PACKAGE_NAME: &str = "@earendil-works/pi-coding-agent";
const OFFICIAL_APP_NAME: &str = "pi";
const OFFICIAL_CONFIG_DIR_NAME: &str = ".pi";

#[derive(Clone, Debug)]
pub struct DistributionMetadata {
    pub package_name: String,
    pub app_name: String,
    pub config_dir_name: String,
}

thread_local! {
    static DISTRIBUTION_METADATA_OVERRIDE: pi_js::seam::Slot<dyn Fn() -> DistributionMetadata + Send + Sync> =
        pi_js::seam::Slot::new();
}

fn current_distribution_metadata() -> DistributionMetadata {
    if let Some(f) = DISTRIBUTION_METADATA_OVERRIDE.with(|s| s.get()) {
        return f();
    }
    DistributionMetadata {
        package_name: (*PACKAGE_NAME).to_string(),
        app_name: (*APP_NAME).to_string(),
        config_dir_name: (*CONFIG_DIR_NAME).to_string(),
    }
}

pub fn is_official_distribution(meta: &DistributionMetadata) -> bool {
    meta.package_name == OFFICIAL_PACKAGE_NAME
        && meta.app_name == OFFICIAL_APP_NAME
        && meta.config_dir_name == OFFICIAL_CONFIG_DIR_NAME
}

/// First-time setup runs when all of these hold:
/// - this is the official Pi distribution (not a fork/rebrand)
/// - experimental features are enabled (PI_EXPERIMENTAL=1)
/// - the default agent directory is used (no custom agent dir override)
/// - setup was not completed before (settings.json does not exist)
pub fn should_run_first_time_setup(settings_path: Option<&str>) -> bool {
    let meta = current_distribution_metadata();
    if !is_official_distribution(&meta) {
        return false;
    }
    if !are_experimental_features_enabled() {
        return false;
    }
    if pi_js::env::var(*ENV_AGENT_DIR).is_some_and(|v| !v.is_empty()) {
        return false;
    }
    let resolved = settings_path.map(str::to_string).unwrap_or_else(get_settings_path);
    !pi_js::fs::exists(&resolved)
}

fn load_startup_resource_themes(resources: &[ResolvedResource]) -> Vec<Theme> {
    let mut themes = Vec::new();
    let mut seen = IndexSet::new();
    for resource in resources {
        if !resource.enabled {
            continue;
        }
        if let Ok(loaded) = load_theme_from_path(&resource.path, None) {
            if let Some(name) = loaded.name() {
                if !seen.insert(name.to_owned()) {
                    continue;
                }
            }
            themes.push(loaded);
        }
    }
    themes
}

async fn load_startup_themes(settings_manager: &SettingsManager) -> pi_js::Result<Vec<Theme>> {
    let global_settings_manager = SettingsManager::in_memory(
        Some(settings_manager.get_global_settings()),
        Some(SettingsManagerCreateOptions {
            project_trusted: Some(false),
        }),
    );
    let package_manager = DefaultPackageManager::new(PackageManagerOptions {
        cwd: pi_js::env::cwd(),
        agent_dir: get_agent_dir(),
        settings_manager: global_settings_manager,
        builtin_extensions: Vec::new(),
    });
    let resolved_paths = package_manager
        .resolve(Some(Arc::new(|_| Box::pin(async { Ok(MissingSourceAction::Skip) }))))
        .await?;
    Ok(load_startup_resource_themes(&resolved_paths.themes))
}

pub(crate) async fn create_startup_tui(settings_manager: &SettingsManager) -> pi_js::Result<Arc<TuiMainScreen>> {
    set_capability_overrides(settings_manager.get_terminal_capability_overrides());
    let _ = set_registered_themes(load_startup_themes(settings_manager).await?);
    mark_terminal_colors_pending();
    let initial_theme = resolve_theme_setting(settings_manager.get_theme_setting().as_deref(), get_terminal_theme())
        .unwrap_or_else(|| SYSTEM_THEME_NAME.to_owned());
    init_theme(Some(&initial_theme), None);
    let kb = KeybindingsManager::create(None);
    sync_tui_keybindings(&kb);
    let ui = Arc::new(TuiMainScreen::new(
        Arc::new(pi_tui::terminal::ProcessTerminal::new()),
        Some(settings_manager.get_show_hardware_cursor()),
        Some(&get_agent_dir()),
    ));
    ui.set_clear_on_shrink(settings_manager.get_clear_on_shrink());
    Ok(ui)
}

fn query_startup_terminal_colors(ui: Arc<dyn TUI + Send + Sync>, on_colors: Arc<dyn Fn() + Send + Sync>) {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(async move {
            let ui_cb = Arc::clone(&ui);
            request_terminal_colors(
                ui.as_ref(),
                Arc::new(move |colors| {
                    set_terminal_colors(colors);
                    on_colors();
                    ui_cb.invalidate();
                    ui_cb.request_render(None);
                }),
            )
            .await;
        });
    }
}

pub(crate) fn start_startup_tui(ui: Arc<dyn TUI + Send + Sync>, settings_manager: &SettingsManager) {
    ui.start();
    let theme_setting = settings_manager.get_theme_setting();
    query_startup_terminal_colors(
        ui,
        Arc::new(move || {
            let resolved = resolve_theme_setting(theme_setting.as_deref(), get_terminal_theme())
                .unwrap_or_else(|| SYSTEM_THEME_NAME.to_owned());
            let _ = set_theme(&resolved, None);
        }),
    );
}

async fn clear_startup_tui(ui: &TuiMainScreen) {
    ui.clear();
    ui.request_render(None);
    let _ = pi_js::time::sleep(25, None).await;
}

pub async fn show_startup_selector<T: Clone + Send + Sync + 'static>(
    settings_manager: &SettingsManager,
    title: &str,
    options: &[(String, T)],
) -> pi_js::Result<Option<T>> {
    let ui = create_startup_tui(settings_manager).await?;
    let (tx, rx) = tokio::sync::oneshot::channel::<Option<T>>();
    let tx_slot = Arc::new(Mutex::new(Some(tx)));

    let labels: Vec<String> = options.iter().map(|(label, _)| label.clone()).collect();
    let opts_for_sel: Vec<(String, T)> = options.to_vec();
    let tx_sel = Arc::clone(&tx_slot);
    let on_select: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |selected| {
        let sender = tx_sel.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(tx) = sender {
            let found = opts_for_sel
                .iter()
                .find(|(label, _)| label == selected)
                .map(|(_, v)| v.clone());
            let _ = tx.send(found);
        }
    });

    let tx_can = Arc::clone(&tx_slot);
    let on_cancel: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        let sender = tx_can.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(tx) = sender {
            let _ = tx.send(None);
        }
    });

    let selector = ExtensionSelectorComponent::new(
        title,
        labels,
        on_select,
        on_cancel,
        Some(ExtensionSelectorOptions {
            tui: Some(Arc::clone(&ui) as Arc<dyn TUI + Send + Sync>),
            ..Default::default()
        }),
    );
    ui.add_child(Arc::new(selector.clone()));
    ui.set_focus(Some(Arc::new(selector.clone())));
    start_startup_tui(Arc::clone(&ui) as Arc<dyn TUI + Send + Sync>, settings_manager);

    let result = rx.await.ok().flatten();
    selector.dispose();
    clear_startup_tui(&ui).await;
    ui.stop(None);
    Ok(result)
}

/// Show the first-time setup dialog and persist the result
pub async fn show_first_time_setup(settings_manager: &SettingsManager) -> pi_js::Result<()> {
    let ui = create_startup_tui(settings_manager).await?;
    let (tx, rx) = tokio::sync::oneshot::channel::<Option<FirstTimeSetupResult>>();
    let tx_slot = Arc::new(Mutex::new(Some(tx)));
    let preview_theme = Arc::new(Mutex::new(SYSTEM_THEME_NAME.to_owned()));

    ui.start();
    let _ = set_theme(SYSTEM_THEME_NAME, None);

    let preview_c = Arc::clone(&preview_theme);
    let ui_preview = Arc::clone(&ui);
    let on_theme_preview: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |name| {
        *preview_c.lock().unwrap_or_else(|e| e.into_inner()) = name.to_owned();
        let _ = set_theme(name, None);
        ui_preview.request_render(None);
    });

    let tx_sub = Arc::clone(&tx_slot);
    let on_submit: Arc<dyn Fn(FirstTimeSetupResult) + Send + Sync> = Arc::new(move |res| {
        if let Some(tx) = tx_sub.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = tx.send(Some(res));
        }
    });

    let tx_can = Arc::clone(&tx_slot);
    let on_cancel: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        if let Some(tx) = tx_can.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = tx.send(None);
        }
    });

    let component = FirstTimeSetupComponent::new(FirstTimeSetupOptions {
        on_theme_preview,
        on_submit,
        on_cancel,
    });
    ui.add_child(Arc::new(component.clone()));
    ui.set_focus(Some(Arc::new(component.clone())));
    ui.request_render(None);

    let preview_colors = Arc::clone(&preview_theme);
    query_startup_terminal_colors(
        Arc::clone(&ui) as Arc<dyn TUI + Send + Sync>,
        Arc::new(move || {
            let name = preview_colors.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let _ = set_theme(&name, None);
        }),
    );

    if let Some(result) = rx.await.ok().flatten() {
        settings_manager.set_theme(&result.theme);
        settings_manager.set_enable_analytics(result.share_analytics);
        settings_manager.flush().await;
    }
    clear_startup_tui(&ui).await;
    ui.stop(None);
    Ok(())
}

pub async fn show_startup_input(
    settings_manager: &SettingsManager,
    title: &str,
    placeholder: Option<&str>,
) -> pi_js::Result<Option<String>> {
    let ui = create_startup_tui(settings_manager).await?;
    let (tx, rx) = tokio::sync::oneshot::channel::<Option<String>>();
    let tx_slot = Arc::new(Mutex::new(Some(tx)));

    let tx_sub = Arc::clone(&tx_slot);
    let on_submit: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |val| {
        if let Some(tx) = tx_sub.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = tx.send(Some(val.to_owned()));
        }
    });

    let tx_can = Arc::clone(&tx_slot);
    let on_cancel: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        if let Some(tx) = tx_can.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = tx.send(None);
        }
    });

    let input = ExtensionInputComponent::new(
        title,
        placeholder,
        on_submit,
        on_cancel,
        Some(ExtensionInputOptions {
            tui: Some(Arc::clone(&ui) as Arc<dyn TUI + Send + Sync>),
            ..Default::default()
        }),
    );
    ui.add_child(Arc::new(input.clone()));
    ui.set_focus(Some(Arc::new(input.clone())));
    start_startup_tui(Arc::clone(&ui) as Arc<dyn TUI + Send + Sync>, settings_manager);

    let result = rx.await.ok().flatten();
    input.dispose();
    clear_startup_tui(&ui).await;
    ui.stop(None);
    Ok(result)
}

pub mod testing {
    use std::sync::Arc;

    use super::DistributionMetadata;

    pub fn override_distribution_metadata(
        f: impl Fn() -> DistributionMetadata + Send + Sync + 'static,
    ) -> pi_js::seam::Guard {
        super::DISTRIBUTION_METADATA_OVERRIDE.with(|s| s.set(Arc::new(f)))
    }
}
