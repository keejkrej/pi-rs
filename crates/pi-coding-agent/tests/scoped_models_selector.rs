//! Port of packages/coding-agent/test/scoped-models-selector.test.ts

use std::sync::{Arc, Mutex};

use pi_ai::Model;
use pi_coding_agent::core::keybindings::KEYBINDINGS;
use pi_coding_agent::modes::interactive::components::scoped_models_selector::{
    ModelsCallbacks, ModelsConfig, ScopedModelsSelectorComponent,
};
use pi_coding_agent::modes::interactive::theme::theme::init_theme;
use pi_tui::{KeybindingsConfig, KeybindingsManager, TUI_KEYBINDINGS, set_keybindings, strip_terminal_sequences};
use serde_json::json;
use serial_test::serial;

struct TestGuard;

impl Drop for TestGuard {
    fn drop(&mut self) {
        init_theme(Some("dark"), None);
        set_keybindings(KeybindingsManager::new(TUI_KEYBINDINGS.clone(), None));
    }
}

fn app_keybindings(user_bindings: Option<KeybindingsConfig>) -> KeybindingsManager {
    KeybindingsManager::new(KEYBINDINGS.clone(), user_bindings)
}

fn setup() -> TestGuard {
    init_theme(Some("dark"), None);
    set_keybindings(app_keybindings(None));
    TestGuard
}

#[derive(Clone)]
struct ModelState {
    id: &'static str,
    name: &'static str,
    enabled: bool,
}

fn make_model(id: &str, name: &str) -> Model {
    serde_json::from_value(json!({
        "id": id,
        "name": name,
        "api": "anthropic-messages",
        "provider": "anthropic",
        "baseUrl": "https://api.anthropic.com",
        "reasoning": false,
        "input": ["text"],
        "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
        "contextWindow": 200_000,
        "maxTokens": 8192,
    }))
    .expect("test model parses")
}

fn create_selector(states: Vec<ModelState>) -> (ScopedModelsSelectorComponent, Arc<Mutex<Vec<ModelState>>>) {
    let all_models: Vec<Model> = states.iter().map(|s| make_model(s.id, s.name)).collect();
    let provider = all_models[0].provider.clone();
    let enabled_model_ids: Vec<String> = states
        .iter()
        .filter(|s| s.enabled)
        .map(|s| format!("{provider}/{}", s.id))
        .collect();
    let shared = Arc::new(Mutex::new(states));
    let shared_clone = Arc::clone(&shared);
    let provider_clone = provider;

    let selector = ScopedModelsSelectorComponent::new(
        ModelsConfig {
            all_models,
            enabled_model_ids: Some(enabled_model_ids),
            refresh_status: None,
        },
        ModelsCallbacks {
            on_change: Arc::new(move |enabled| {
                let mut guard = shared_clone.lock().unwrap();
                for model in guard.iter_mut() {
                    let full_id = format!("{provider_clone}/{}", model.id);
                    model.enabled = enabled.as_ref().is_none_or(|ids| ids.contains(&full_id));
                }
            }),
            on_persist: Arc::new(|_| {}),
            on_cancel: Arc::new(|| {}),
        },
    );
    (selector, shared)
}

fn get_marker_states(selector: &ScopedModelsSelectorComponent, models: &[ModelState]) -> Vec<bool> {
    let lines: Vec<String> = strip_terminal_sequences(&selector.render(120).join("\n"))
        .split('\n')
        .map(str::to_string)
        .collect();
    models
        .iter()
        .map(|model| {
            let needle = format!("{} [", model.id);
            let line = lines
                .iter()
                .find(|candidate| candidate.contains(&needle))
                .unwrap_or_else(|| panic!("Expected rendered row for {}", model.id));
            let chars: Vec<char> = line.chars().collect();
            let after_prefix: String = chars.into_iter().skip(2).collect();
            after_prefix.starts_with("✓ ")
        })
        .collect()
}

fn enabled_flags(shared: &Arc<Mutex<Vec<ModelState>>>) -> Vec<bool> {
    shared.lock().unwrap().iter().map(|m| m.enabled).collect()
}

// #5916
#[test]
#[serial]
fn shows_model_names_that_differ_from_the_id() {
    let _g = setup();
    let (selector, _) = create_selector(vec![
        ModelState {
            id: "model-a",
            name: "Model A",
            enabled: true,
        },
        ModelState {
            id: "model-b",
            name: "Renamed B",
            enabled: true,
        },
    ]);
    let provider = "anthropic";
    let rows: Vec<String> = strip_terminal_sequences(&selector.render(120).join("\n"))
        .split('\n')
        .map(|line| {
            let sliced: String = line.chars().skip(4).collect();
            sliced.trim_end().to_string()
        })
        .collect();
    assert!(rows.contains(&format!("model-a [{provider}]")));
    assert!(rows.contains(&format!("model-b (Renamed B) [{provider}]")));
}

#[test]
#[serial]
fn marks_every_model_after_enabling_all() {
    let _g = setup();
    let initial = vec![
        ModelState {
            id: "model-a",
            name: "Model A",
            enabled: true,
        },
        ModelState {
            id: "model-b",
            name: "Model B",
            enabled: false,
        },
        ModelState {
            id: "model-c",
            name: "Model C",
            enabled: false,
        },
    ];
    let (selector, shared) = create_selector(initial.clone());

    selector.handle_input("\x01");

    assert_eq!(enabled_flags(&shared), vec![true, true, true]);
    assert_eq!(get_marker_states(&selector, &initial), vec![true, true, true]);
    assert!(strip_terminal_sequences(&selector.render(120).join("\n")).contains("all enabled"));
}

#[test]
#[serial]
fn disables_only_the_selected_model_after_enabling_all() {
    let _g = setup();
    let initial = vec![
        ModelState {
            id: "model-a",
            name: "Model A",
            enabled: true,
        },
        ModelState {
            id: "model-b",
            name: "Model B",
            enabled: false,
        },
        ModelState {
            id: "model-c",
            name: "Model C",
            enabled: false,
        },
    ];
    let (selector, shared) = create_selector(initial.clone());

    selector.handle_input("\x01");
    selector.handle_input("\r");

    assert_eq!(enabled_flags(&shared), vec![false, true, true]);
    assert_eq!(get_marker_states(&selector, &initial), vec![false, true, true]);
}

#[test]
#[serial]
fn enables_only_the_selected_model_after_clearing_all() {
    let _g = setup();
    let initial = vec![
        ModelState {
            id: "model-a",
            name: "Model A",
            enabled: true,
        },
        ModelState {
            id: "model-b",
            name: "Model B",
            enabled: true,
        },
        ModelState {
            id: "model-c",
            name: "Model C",
            enabled: true,
        },
    ];
    let (selector, shared) = create_selector(initial.clone());

    selector.handle_input("\x18");
    assert_eq!(enabled_flags(&shared), vec![false, false, false]);
    assert_eq!(get_marker_states(&selector, &initial), vec![false, false, false]);

    selector.handle_input("\r");
    assert_eq!(enabled_flags(&shared), vec![true, false, false]);
    assert_eq!(get_marker_states(&selector, &initial), vec![true, false, false]);
}

#[test]
#[serial]
fn restores_the_all_enabled_state_after_re_enabling_the_last_disabled_model() {
    let _g = setup();
    let initial = vec![
        ModelState {
            id: "model-a",
            name: "Model A",
            enabled: true,
        },
        ModelState {
            id: "model-b",
            name: "Model B",
            enabled: false,
        },
        ModelState {
            id: "model-c",
            name: "Model C",
            enabled: false,
        },
    ];
    let (selector, shared) = create_selector(initial.clone());

    selector.handle_input("\x01"); // enable all -> None
    selector.handle_input("\r"); // disable model-a; enabled models re-sort first: [b, c, a]
    selector.handle_input("\x1b[B");
    selector.handle_input("\x1b[B"); // move selection back to model-a
    selector.handle_input("\r"); // re-enable model-a

    assert_eq!(enabled_flags(&shared), vec![true, true, true]);
    assert_eq!(get_marker_states(&selector, &initial), vec![true, true, true]);
    assert!(strip_terminal_sequences(&selector.render(120).join("\n")).contains("all enabled"));
}
