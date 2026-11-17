//! Port of packages/coding-agent/test/suite/regressions/6999-models-json-hot-reload.test.ts
//!
//! Port of packages/coding-agent/test/suite/regressions/6999-models-json-hot-reload.test.ts.
//! // once `modes/interactive/components/model_selector.rs` lands.

use std::fs;
use std::sync::Arc;

use pi_ai::auth::credential_store::InMemoryCredentialStore;
use pi_ai::models::ModelsRefreshOptions;
use pi_coding_agent::core::model_runtime::{CreateModelRuntimeOptions, ModelRuntime};
use serde_json::{Value, json};
use tempfile::TempDir;

fn models_json(provider: &str, model: &str) -> Value {
    json!({
        "providers": {
            provider: {
                "baseUrl": "https://example.test/v1",
                "api": "openai-completions",
                "apiKey": "test-key",
                "models": [{ "id": model }]
            }
        }
    })
}

#[tokio::test]
async fn reloads_models_json_when_refreshing_runtime_catalog() {
    let temp_dir: TempDir = tempfile::tempdir().expect("create tempdir");
    let models_path = temp_dir.path().join("models.json");
    let models_path_str = models_path.to_string_lossy().into_owned();

    fs::write(
        &models_path,
        pi_js::json::stringify(&models_json("old-provider", "old-model")),
    )
    .expect("write initial models.json");

    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(Arc::new(InMemoryCredentialStore::new())),
        models_path: Some(Some(models_path_str)),
        allow_model_network: Some(false),
        refresh_on_create: Some(false),
        ..Default::default()
    })
    .await
    .expect("create runtime");

    assert!(runtime.get_model("old-provider", "old-model").is_some());

    fs::write(
        &models_path,
        pi_js::json::stringify(&models_json("new-provider", "new-model")),
    )
    .expect("overwrite models.json");

    runtime
        .refresh(Some(ModelsRefreshOptions {
            allow_network: Some(false),
            force: Some(true),
            providers: None,
            signal: None,
        }))
        .await
        .expect("refresh runtime");

    assert!(runtime.get_model("new-provider", "new-model").is_some());
    assert!(runtime.get_model("old-provider", "old-model").is_none());
    let available = runtime.get_available_snapshot();
    assert!(
        available
            .iter()
            .any(|m| m.provider == "new-provider" && m.id == "new-model")
    );
    assert!(
        !available
            .iter()
            .any(|m| m.provider == "old-provider" && m.id == "old-model")
    );
}

#[tokio::test]
#[serial_test::serial]
async fn model_selector_shows_hot_reloaded_models_json_entry_on_open() {
    use std::sync::Arc;

    use pi_coding_agent::core::auth_storage::AuthStorage;
    use pi_coding_agent::core::model_runtime::{ModelRuntime, ModelRuntimeCreateOptions};
    use pi_coding_agent::modes::interactive::components::model_selector::ModelSelectorComponent;
    use pi_coding_agent::modes::interactive::model_catalog_refresh::ModelCatalogRuntime;
    use pi_coding_agent::modes::interactive::theme::theme::init_theme;
    use pi_tui::terminal::{Terminal, TerminalInputHandler, TerminalResizeHandler};
    use pi_tui::{TUI, TuiMainScreen, strip_terminal_sequences};
    use tempfile::TempDir;

    struct ThemeGuard;
    impl Drop for ThemeGuard {
        fn drop(&mut self) {
            init_theme(Some("dark"), None);
        }
    }
    struct DummyTerminal;
    #[async_trait::async_trait]
    impl Terminal for DummyTerminal {
        fn start(&self, _on_input: TerminalInputHandler, _on_resize: TerminalResizeHandler) {}
        fn stop(&self) {}
        async fn drain_input(&self, _max_ms: Option<i64>, _idle_ms: Option<i64>) {}
        fn write(&self, _data: &str) {}
        fn columns(&self) -> i64 {
            120
        }
        fn rows(&self) -> i64 {
            30
        }
        fn kitty_protocol_active(&self) -> bool {
            false
        }
        fn move_by(&self, _lines: i64) {}
        fn hide_cursor(&self) {}
        fn show_cursor(&self) {}
        fn clear_line(&self) {}
        fn clear_from_cursor(&self) {}
        fn clear_screen(&self) {}
        fn set_title(&self, _title: &str) {}
        fn set_progress(&self, _active: bool) {}
    }

    init_theme(Some("dark"), Some(false));
    let _guard = ThemeGuard;

    let tmp = TempDir::new().expect("tempdir");
    let models_path = tmp.path().join("models.json");
    std::fs::write(
        &models_path,
        pi_js::json::stringify(&models_json("custom-hot", "initial-hot-model")),
    )
    .expect("write initial models.json");

    let runtime = Arc::new(
        ModelRuntime::create(ModelRuntimeCreateOptions {
            credentials: Some(Arc::new(AuthStorage::in_memory(None))),
            models_path: Some(Some(models_path.to_string_lossy().into_owned())),
            allow_model_network: Some(false),
            refresh_on_create: Some(true),
            ..Default::default()
        })
        .await
        .expect("create ModelRuntime"),
    );

    std::fs::write(
        &models_path,
        pi_js::json::stringify(&models_json("custom-hot", "reloaded-hot-model")),
    )
    .expect("update models.json");
    ModelCatalogRuntime::refresh(&runtime, pi_js::abort::AbortSignal::never())
        .await
        .expect("refresh after models.json edit");

    let tui: Arc<dyn TUI + Send + Sync> = Arc::new(TuiMainScreen::new(Arc::new(DummyTerminal), Some(false), None));
    let selector = ModelSelectorComponent::new(
        tui,
        None,
        Arc::new(runtime),
        Vec::new(),
        Arc::new(|_| {}),
        Arc::new(|| {}),
        None,
        None,
        None,
    );

    let rendered = strip_terminal_sequences(&selector.render(120).join("\n"));
    assert!(
        rendered.contains("reloaded-hot-model"),
        "expected hot-reloaded model in ModelSelectorComponent render:\n{rendered}"
    );
}
