//! Port of packages/coding-agent/test/oauth-selector.test.ts (unit coding-agent-interactive-components-dialogs)

use std::sync::Arc;

use async_trait::async_trait;
use pi_ai::auth::types::{AuthCheck, AuthType};
use pi_coding_agent::modes::interactive::components::oauth_selector::{
    AuthSelectorProvider, OAuthSelectorComponent, OAuthSelectorMode,
};
use pi_coding_agent::modes::interactive::theme::theme::{ThemeColor, init_theme, theme};
use pi_tui::terminal::{TerminalInputHandler, TerminalResizeHandler};
use pi_tui::{
    KeybindingsManager, TUI, TUI_KEYBINDINGS, Terminal, TuiMainScreen, set_keybindings, strip_terminal_sequences,
};
use serial_test::serial;

struct TestGuard;

impl Drop for TestGuard {
    fn drop(&mut self) {
        init_theme(Some("dark"), None);
        set_keybindings(KeybindingsManager::new(TUI_KEYBINDINGS.clone(), None));
    }
}

struct NoopTerminal;

#[async_trait]
impl Terminal for NoopTerminal {
    fn start(&self, _on_input: TerminalInputHandler, _on_resize: TerminalResizeHandler) {}
    fn stop(&self) {}
    async fn drain_input(&self, _max_ms: Option<i64>, _idle_ms: Option<i64>) {}
    fn write(&self, _data: &str) {}
    fn columns(&self) -> i64 {
        120
    }
    fn rows(&self) -> i64 {
        40
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

fn mock_tui() -> Arc<dyn TUI + Send + Sync> {
    Arc::new(TuiMainScreen::new(Arc::new(NoopTerminal), None, None))
}

fn noop_select() -> Arc<dyn Fn(&str, AuthType) + Send + Sync> {
    Arc::new(|_, _| {})
}

fn noop_cancel() -> Arc<dyn Fn() + Send + Sync> {
    Arc::new(|| {})
}

fn setup() -> TestGuard {
    init_theme(Some("dark"), None);
    set_keybindings(KeybindingsManager::new(TUI_KEYBINDINGS.clone(), None));
    TestGuard
}

#[test]
#[serial]
fn renders_an_option_without_compiled_auth_status_as_not_configured() {
    let _g = setup();
    let selector = OAuthSelectorComponent::new(
        OAuthSelectorMode::Login,
        vec![AuthSelectorProvider {
            id: "google".to_string(),
            name: "Google".to_string(),
            auth_type: AuthType::ApiKey,
            method: None,
            status: None,
            subscription: None,
        }],
        noop_select(),
        noop_cancel(),
        None,
        None,
    );

    let output = strip_terminal_sequences(&selector.render(120).join("\n"));
    assert!(output.contains("not configured"), "output={output}");
    assert!(!output.contains("✓ configured"), "output={output}");
}

#[test]
#[serial]
fn shows_oauth_auth_distinctly_in_the_api_key_selector() {
    let _g = setup();
    let selector = OAuthSelectorComponent::new(
        OAuthSelectorMode::Login,
        vec![AuthSelectorProvider {
            id: "anthropic".to_string(),
            name: "Anthropic".to_string(),
            auth_type: AuthType::ApiKey,
            method: None,
            status: Some(AuthCheck {
                auth_type: AuthType::OAuth,
                source: Some("OAuth".to_string()),
            }),
            subscription: None,
        }],
        noop_select(),
        noop_cancel(),
        None,
        None,
    );

    let output = strip_terminal_sequences(&selector.render(120).join("\n"));
    assert!(output.contains("subscription configured"), "output={output}");
}

#[test]
#[serial]
fn shows_environment_api_key_auth_as_configured() {
    let _g = setup();
    let selector = OAuthSelectorComponent::new(
        OAuthSelectorMode::Login,
        vec![AuthSelectorProvider {
            id: "openai".to_string(),
            name: "OpenAI".to_string(),
            auth_type: AuthType::ApiKey,
            method: None,
            status: Some(AuthCheck {
                auth_type: AuthType::ApiKey,
                source: Some("OPENAI_API_KEY".to_string()),
            }),
            subscription: None,
        }],
        noop_select(),
        noop_cancel(),
        None,
        None,
    );

    let output = strip_terminal_sequences(&selector.render(120).join("\n"));
    assert!(output.contains("✓ env: OPENAI_API_KEY"), "output={output}");
    assert!(!output.contains("not configured"), "output={output}");
}

#[test]
#[serial]
fn shows_models_json_api_key_auth_as_configured() {
    let _g = setup();
    let selector = OAuthSelectorComponent::new(
        OAuthSelectorMode::Login,
        vec![AuthSelectorProvider {
            id: "local-proxy".to_string(),
            name: "local-proxy".to_string(),
            auth_type: AuthType::ApiKey,
            method: None,
            status: Some(AuthCheck {
                auth_type: AuthType::ApiKey,
                source: Some("key in models.json".to_string()),
            }),
            subscription: None,
        }],
        noop_select(),
        noop_cancel(),
        None,
        None,
    );

    let output = strip_terminal_sequences(&selector.render(120).join("\n"));
    assert!(output.contains("✓ key in models.json"), "output={output}");
}

#[test]
#[serial]
fn shows_models_json_command_auth_as_configured() {
    let _g = setup();
    let selector = OAuthSelectorComponent::new(
        OAuthSelectorMode::Login,
        vec![AuthSelectorProvider {
            id: "op-proxy".to_string(),
            name: "op-proxy".to_string(),
            auth_type: AuthType::ApiKey,
            method: None,
            status: Some(AuthCheck {
                auth_type: AuthType::ApiKey,
                source: Some("command in models.json".to_string()),
            }),
            subscription: None,
        }],
        noop_select(),
        noop_cancel(),
        None,
        None,
    );

    let output = strip_terminal_sequences(&selector.render(120).join("\n"));
    assert!(output.contains("✓ command in models.json"), "output={output}");
}

#[test]
#[serial]
fn shows_the_brand_style_of_a_preferred_partner_only_on_its_selected_row() {
    let _g = setup();
    let tui = mock_tui();
    let providers = vec![
        AuthSelectorProvider {
            id: "radius".to_string(),
            name: "Radius".to_string(),
            auth_type: AuthType::OAuth,
            method: None,
            status: None,
            subscription: None,
        },
        AuthSelectorProvider {
            id: "openrouter".to_string(),
            name: "OpenRouter".to_string(),
            auth_type: AuthType::ApiKey,
            method: None,
            status: None,
            subscription: None,
        },
    ];
    let selector = OAuthSelectorComponent::new(
        OAuthSelectorMode::Login,
        providers,
        noop_select(),
        noop_cancel(),
        None,
        Some(tui),
    );
    let t = theme();

    let output = selector.render(120).join("\n");
    assert!(!output.contains(&t.fg(ThemeColor::Accent, "Radius")));
    assert!(output.contains(&t.fg(ThemeColor::Text, "OpenRouter")));
    assert!(strip_terminal_sequences(&output).contains("→ Radius"));

    selector.handle_input("\x1b[B");
    let moved = selector.render(120).join("\n");
    assert!(moved.contains(&t.fg(ThemeColor::Text, "Radius")));
    assert!(!moved.contains(&t.fg(ThemeColor::Accent, "OpenRouter")));
    assert!(strip_terminal_sequences(&moved).contains("→ OpenRouter"));

    selector.dispose();
}

#[test]
#[serial]
fn keeps_the_accent_style_without_a_tui_to_animate_in() {
    let _g = setup();
    let selector = OAuthSelectorComponent::new(
        OAuthSelectorMode::Login,
        vec![AuthSelectorProvider {
            id: "openrouter".to_string(),
            name: "OpenRouter".to_string(),
            auth_type: AuthType::ApiKey,
            method: None,
            status: None,
            subscription: None,
        }],
        noop_select(),
        noop_cancel(),
        None,
        None,
    );

    let output = selector.render(120).join("\n");
    assert!(output.contains(&theme().fg(ThemeColor::Accent, "OpenRouter")));
}

#[test]
#[serial]
fn marks_preferred_partners_with_a_star() {
    let _g = setup();
    let selector = OAuthSelectorComponent::new(
        OAuthSelectorMode::Login,
        vec![
            AuthSelectorProvider {
                id: "radius".to_string(),
                name: "Radius".to_string(),
                auth_type: AuthType::OAuth,
                method: None,
                status: None,
                subscription: None,
            },
            AuthSelectorProvider {
                id: "openrouter".to_string(),
                name: "OpenRouter".to_string(),
                auth_type: AuthType::ApiKey,
                method: None,
                status: None,
                subscription: None,
            },
            AuthSelectorProvider {
                id: "anthropic".to_string(),
                name: "Anthropic".to_string(),
                auth_type: AuthType::ApiKey,
                method: None,
                status: None,
                subscription: None,
            },
        ],
        noop_select(),
        noop_cancel(),
        None,
        None,
    );

    let output = strip_terminal_sequences(&selector.render(120).join("\n"));
    assert!(output.contains("→ Radius ✦ [subscription]"), "output={output}");
    assert!(output.contains("  OpenRouter ✦ [API key]"), "output={output}");
    assert!(output.contains("  Anthropic [API key]"), "output={output}");
}
