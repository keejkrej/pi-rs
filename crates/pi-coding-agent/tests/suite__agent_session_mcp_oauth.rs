//! Port of packages/coding-agent/test/suite/agent-session-mcp-oauth.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::{Arc, Mutex, PoisonError};

use pi_agent_core::types::AgentMessage;
use pi_ai::providers::faux::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxResponseStep, faux_assistant_message, faux_tool_call,
};
use pi_ai::types::{Message, StopReason, ToolResultMessage};
use pi_coding_agent::core::agent_session::ExtensionBindings;
use pi_coding_agent::core::auth_storage::{AuthStorageBackend, InMemoryAuthStorageBackend, LockResult};
use pi_coding_agent::core::extensions::types::{ExtensionUIContext, ExtensionUIDialogOptions, NotifyType};
use pi_coding_agent::extensions::mcp::cli::{McpCommandOptions, run_mcp_command};
use pi_coding_agent::extensions::mcp::config::{
    LoadedMcpConfig, McpExposure, McpHttpServerConfig, McpOAuthConfig, McpServerConfig, McpServerEntry,
};
use pi_coding_agent::extensions::mcp::oauth::McpOAuthCredentialStore;
use pi_coding_agent::extensions::mcp::{McpExtensionOptions, create_mcp_extension};
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use support::suite::harness::{Harness, HarnessOptions, create_harness, get_message_text};
use support::suite::mcp_oauth_server::{OAuthMcpServerHandle, StartOAuthMcpServerOptions, start_oauth_mcp_server};
use tokio::sync::{Notify, oneshot};

#[derive(Clone, Copy, PartialEq, Eq)]
enum BrowserBehavior {
    Follow,
    Paste,
}

struct OAuthUiContext {
    notifications: Arc<Mutex<Vec<String>>>,
    browser: BrowserBehavior,
    redirect_rx: Arc<Mutex<Option<oneshot::Receiver<String>>>>,
}

#[async_trait::async_trait]
impl ExtensionUIContext for OAuthUiContext {
    async fn select(
        &self,
        _title: &str,
        _options: Vec<String>,
        _opts: Option<ExtensionUIDialogOptions>,
    ) -> pi_js::Result<Option<String>> {
        Ok(None)
    }

    async fn confirm(
        &self,
        _title: &str,
        _message: &str,
        _opts: Option<ExtensionUIDialogOptions>,
    ) -> pi_js::Result<bool> {
        Ok(false)
    }

    async fn input(
        &self,
        _title: &str,
        _placeholder: Option<&str>,
        opts: Option<ExtensionUIDialogOptions>,
    ) -> pi_js::Result<Option<String>> {
        if self.browser == BrowserBehavior::Paste {
            let rx = self.redirect_rx.lock().unwrap_or_else(PoisonError::into_inner).take();
            if let Some(rx) = rx {
                return Ok(rx.await.ok());
            }
            return Ok(None);
        }
        let notify = Arc::new(Notify::new());
        let n_c = Arc::clone(&notify);
        let signal = opts.and_then(|o| o.signal);
        let _sub = signal.as_ref().map(|s| {
            s.on_abort(move |_| {
                n_c.notify_one();
            })
        });
        if signal.as_ref().is_some_and(|s| s.aborted()) {
            return Ok(None);
        }
        notify.notified().await;
        Ok(None)
    }

    fn notify(&self, message: &str, _notify_type: Option<NotifyType>) {
        self.notifications
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(message.to_string());
    }

    fn on_terminal_input(
        &self,
        _handler: pi_coding_agent::core::extensions::types::TerminalInputHandler,
    ) -> pi_js::Unsubscribe {
        pi_js::Unsubscribe::noop()
    }

    fn set_status(&self, _key: &str, _text: Option<&str>) {}
    fn set_working_message(&self, _message: Option<&str>) {}
    fn set_working_visible(&self, _visible: bool) {}
    fn set_working_indicator(
        &self,
        _options: Option<pi_coding_agent::core::extensions::types::WorkingIndicatorOptions>,
    ) {
    }
    fn set_hidden_thinking_label(&self, _label: Option<&str>) {}
    fn set_widget_lines(
        &self,
        _key: &str,
        _content: Option<Vec<String>>,
        _options: Option<pi_coding_agent::core::extensions::types::ExtensionWidgetOptions>,
    ) {
    }
    fn set_widget_factory(
        &self,
        _key: &str,
        _factory: Option<pi_coding_agent::core::extensions::types::WidgetFactory>,
        _options: Option<pi_coding_agent::core::extensions::types::ExtensionWidgetOptions>,
    ) {
    }
    fn set_footer(&self, _factory: Option<pi_coding_agent::core::extensions::types::FooterFactory>) {}
    fn set_header(&self, _factory: Option<pi_coding_agent::core::extensions::types::HeaderFactory>) {}
    fn set_title(&self, _title: &str) {}
    async fn custom(
        &self,
        _factory: pi_coding_agent::core::extensions::types::CustomUiFactory,
        _options: Option<pi_coding_agent::core::extensions::types::ExtensionCustomOptions>,
    ) -> pi_js::Result<Value> {
        Ok(Value::Null)
    }
    fn paste_to_editor(&self, _text: &str) {}
    fn set_editor_text(&self, _text: &str) {}
    fn get_editor_text(&self) -> String {
        String::new()
    }
    async fn editor(&self, _title: &str, _prefill: Option<&str>) -> pi_js::Result<Option<String>> {
        Ok(None)
    }
    fn add_autocomplete_provider(
        &self,
        _factory: pi_coding_agent::core::extensions::types::AutocompleteProviderFactory,
    ) {
    }
    fn set_editor_component(&self, _factory: Option<pi_coding_agent::core::extensions::types::EditorFactory>) {}
    fn get_editor_component(&self) -> Option<pi_coding_agent::core::extensions::types::EditorFactory> {
        None
    }
    fn theme(&self) -> pi_coding_agent::core::extensions::types::Theme {
        pi_coding_agent::core::extensions::types::Theme::default()
    }
    fn get_all_themes(&self) -> Vec<pi_coding_agent::core::extensions::types::ThemeInfo> {
        Vec::new()
    }
    fn get_theme(&self, _name: &str) -> Option<pi_coding_agent::core::extensions::types::Theme> {
        None
    }
    fn set_theme_by_name(&self, _name: &str) -> pi_coding_agent::core::extensions::types::SetThemeResult {
        pi_coding_agent::core::extensions::types::SetThemeResult {
            success: false,
            error: None,
        }
    }
    fn set_theme(
        &self,
        _theme: pi_coding_agent::core::extensions::types::Theme,
    ) -> pi_coding_agent::core::extensions::types::SetThemeResult {
        pi_coding_agent::core::extensions::types::SetThemeResult {
            success: false,
            error: None,
        }
    }
    fn get_tools_expanded(&self) -> bool {
        false
    }
    fn set_tools_expanded(&self, _expanded: bool) {}
}

struct OAuthTestSetup {
    harness: Harness,
    server: OAuthMcpServerHandle,
    notifications: Arc<Mutex<Vec<String>>>,
    backend: Arc<InMemoryAuthStorageBackend>,
    opened: Arc<Mutex<Vec<url::Url>>>,
}

async fn setup(browser: BrowserBehavior, oauth: Option<McpOAuthConfig>) -> OAuthTestSetup {
    let server = start_oauth_mcp_server(StartOAuthMcpServerOptions::default())
        .await
        .expect("start oauth mcp server");
    let backend = Arc::new(InMemoryAuthStorageBackend::new());
    let entry = McpServerEntry {
        name: "issues".to_string(),
        config: McpServerConfig::Http(McpHttpServerConfig {
            exposure: Some(McpExposure::Direct),
            description: None,
            tool_exposure: None,
            enabled: None,
            timeout: None,
            r#type: None,
            url: server.url.clone(),
            headers: None,
            oauth,
            auth: None,
        }),
        source: "test".to_string(),
        scope: None,
        override_path: None,
    };
    let notifications = Arc::new(Mutex::new(Vec::<String>::new()));
    let opened = Arc::new(Mutex::new(Vec::<url::Url>::new()));
    let (redirect_tx, redirect_rx) = oneshot::channel::<String>();
    let redirect_tx = Arc::new(Mutex::new(Some(redirect_tx)));
    let redirect_rx = Arc::new(Mutex::new(Some(redirect_rx)));

    let op_c = Arc::clone(&opened);
    let rtx_c = Arc::clone(&redirect_tx);
    let entry_c = entry.clone();
    let backend_dyn: Arc<dyn AuthStorageBackend> = backend.clone();
    let creds = McpOAuthCredentialStore::new(Some(backend_dyn), None);
    let harness = create_harness(HarnessOptions {
        initial_active_tool_names: Some(vec![]),
        extension_factories: Some(vec![
            pi_coding_agent::core::extensions::types::InlineExtension::Factory(create_mcp_extension(
                McpExtensionOptions {
                    load_config: Some(Arc::new(move |_| LoadedMcpConfig {
                        servers: vec![entry_c.clone()],
                        auto_enable_codemode: None,
                        errors: vec![],
                        project_config: None,
                    })),
                    credentials: Some(creds),
                    open_url: Some(Arc::new(move |url_str| {
                        if let Ok(parsed) = url::Url::parse(url_str) {
                            op_c.lock().unwrap_or_else(PoisonError::into_inner).push(parsed);
                        }
                        let url_owned = url_str.to_string();
                        if browser == BrowserBehavior::Follow {
                            tokio::spawn(async move {
                                let client = reqwest::Client::new();
                                let _ = client.get(&url_owned).send().await;
                            });
                        } else {
                            let tx_slot = Arc::clone(&rtx_c);
                            tokio::spawn(async move {
                                let client = reqwest::Client::builder()
                                    .redirect(reqwest::redirect::Policy::none())
                                    .build()
                                    .unwrap();
                                let loc = match client.get(&url_owned).send().await {
                                    Ok(resp) => resp
                                        .headers()
                                        .get("location")
                                        .and_then(|v| v.to_str().ok())
                                        .unwrap_or("")
                                        .to_string(),
                                    Err(_) => String::new(),
                                };
                                if let Some(tx) = tx_slot.lock().unwrap_or_else(PoisonError::into_inner).take() {
                                    let _ = tx.send(loc);
                                }
                            });
                        }
                    })),
                    ..Default::default()
                },
            )),
        ]),
        ..Default::default()
    })
    .await;

    let ui = Arc::new(OAuthUiContext {
        notifications: Arc::clone(&notifications),
        browser,
        redirect_rx,
    });
    harness
        .session
        .bind_extensions(ExtensionBindings {
            ui_context: Some(ui),
            ..Default::default()
        })
        .await
        .unwrap();

    OAuthTestSetup {
        harness,
        server,
        notifications,
        backend,
        opened,
    }
}

async fn call_whoami(harness: &Harness) -> ToolResultMessage {
    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Blocks(vec![pi_ai::providers::faux::FauxContentBlock::ToolCall(
                faux_tool_call("mcp__issues__whoami", Default::default(), None),
            )]),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("done".to_string()),
            None,
        )),
    ]);
    let before = harness.session.messages().len();
    harness.session.prompt("who am i", None).await.unwrap();
    harness
        .session
        .messages()
        .into_iter()
        .skip(before)
        .find_map(|m| match m {
            AgentMessage::Message(Message::ToolResult(tr)) => Some(tr),
            _ => None,
        })
        .expect("no tool result")
}

async fn free_port() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    listener.local_addr().unwrap().port()
}

#[tokio::test]
async fn signs_in_through_the_browser_refreshes_expired_tokens_and_signs_out() {
    let ts = setup(BrowserBehavior::Follow, None).await;

    ts.harness.session.prompt("/mcp", None).await.unwrap();
    {
        let notes = ts.notifications.lock().unwrap_or_else(PoisonError::into_inner).clone();
        assert!(notes.contains(&"MCP servers need attention:\n  issues: needs sign-in\nRun /mcp to fix.".to_string()));
        assert_eq!(
            notes.last().map(String::as_str),
            Some("issues: needs sign-in, run /mcp login issues (direct)")
        );
    }

    ts.harness.session.prompt("/mcp login issues", None).await.unwrap();
    assert_eq!(
        ts.notifications
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .last()
            .map(String::as_str),
        Some("Signed in to MCP server \"issues\" (1 tools).")
    );
    assert_eq!(ts.server.log(), vec!["401 none", "register", "token code"]);
    let stored = ts
        .backend
        .with_lock(|curr| Ok(LockResult::unchanged(curr.map(str::to_string))))
        .unwrap()
        .unwrap_or_default();
    assert!(stored.contains("\"access_token\": \"access-1\"") || stored.contains("\"access_token\":\"access-1\""));

    let who1 = call_whoami(&ts.harness).await;
    assert_eq!(
        get_message_text(&AgentMessage::from(Message::ToolResult(who1))),
        "token access-1"
    );

    ts.server.expire_access_tokens();
    let who2 = call_whoami(&ts.harness).await;
    assert_eq!(
        get_message_text(&AgentMessage::from(Message::ToolResult(who2))),
        "token access-2"
    );
    let log2 = ts.server.log();
    assert_eq!(
        &log2[log2.len() - 3..],
        &["401 access-1", "token refresh", "call access-2"]
    );

    ts.backend
        .with_lock(|curr| {
            let mut states: indexmap::IndexMap<String, Value> =
                curr.and_then(|s| pi_js::json::parse_as(s).ok()).unwrap_or_default();
            let expired_at = Value::from(pi_js::time::now_ms() - 1_000);
            for val in states.values_mut() {
                if let Some(obj) = val.as_object_mut() {
                    obj.insert("tokensExpireAt".to_string(), expired_at.clone());
                }
            }
            Ok(LockResult::write((), pi_js::json::stringify(&states)))
        })
        .unwrap();
    let who3 = call_whoami(&ts.harness).await;
    assert_eq!(
        get_message_text(&AgentMessage::from(Message::ToolResult(who3))),
        "token access-3"
    );
    let log3 = ts.server.log();
    assert_eq!(&log3[log3.len() - 2..], &["token refresh", "call access-3"]);

    ts.harness.session.prompt("/mcp logout issues", None).await.unwrap();
    assert_eq!(
        ts.notifications
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .last()
            .map(String::as_str),
        Some("Signed out of MCP server \"issues\".")
    );
    let logged_out = call_whoami(&ts.harness).await;
    assert!(logged_out.is_error);
    assert_eq!(
        get_message_text(&AgentMessage::from(Message::ToolResult(logged_out))),
        "MCP server \"issues\" requires sign-in. Run /mcp to sign in."
    );

    ts.server.close().await;
    ts.harness.cleanup();
}

#[tokio::test]
async fn accepts_a_pasted_redirect_url_when_the_browser_cannot_reach_the_callback() {
    let ts = setup(BrowserBehavior::Paste, None).await;

    ts.harness.session.prompt("/mcp login", None).await.unwrap();
    assert_eq!(
        ts.notifications
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .last()
            .map(String::as_str),
        Some("Signed in to MCP server \"issues\" (1 tools).")
    );
    let who = call_whoami(&ts.harness).await;
    assert_eq!(
        get_message_text(&AgentMessage::from(Message::ToolResult(who))),
        "token access-1"
    );
    assert!(ts.server.log().contains(&"token code".to_string()));

    ts.server.close().await;
    ts.harness.cleanup();
}

#[tokio::test]
async fn uses_the_configured_callback_url_and_scope() {
    let port = free_port().await;
    let callback_url = format!("http://localhost:{port}/callback");
    let ts = setup(
        BrowserBehavior::Follow,
        Some(McpOAuthConfig {
            callback_url: Some(callback_url.clone()),
            scope: Some("issues:read".to_string()),
            ..Default::default()
        }),
    )
    .await;

    ts.harness.session.prompt("/mcp login issues", None).await.unwrap();
    assert_eq!(
        ts.notifications
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .last()
            .map(String::as_str),
        Some("Signed in to MCP server \"issues\" (1 tools).")
    );
    {
        let urls = ts.opened.lock().unwrap_or_else(PoisonError::into_inner).clone();
        let params: indexmap::IndexMap<String, String> = urls[0].query_pairs().into_owned().collect();
        assert_eq!(
            params.get("redirect_uri").map(String::as_str),
            Some(callback_url.as_str())
        );
        assert_eq!(params.get("scope").map(String::as_str), Some("issues:read"));
    }
    let who = call_whoami(&ts.harness).await;
    assert_eq!(
        get_message_text(&AgentMessage::from(Message::ToolResult(who))),
        "token access-1"
    );

    ts.server.close().await;
    ts.harness.cleanup();
}

#[tokio::test]
async fn registers_with_the_configured_client_name() {
    let ts = setup(
        BrowserBehavior::Follow,
        Some(McpOAuthConfig {
            client_name: Some("Claude Code".to_string()),
            ..Default::default()
        }),
    )
    .await;
    ts.harness.session.prompt("/mcp login issues", None).await.unwrap();
    assert_eq!(
        ts.notifications
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .last()
            .map(String::as_str),
        Some("Signed in to MCP server \"issues\" (1 tools).")
    );
    let client_names: Vec<String> = ts
        .server
        .registrations()
        .into_iter()
        .filter_map(|m| m.get("client_name").and_then(Value::as_str).map(str::to_string))
        .collect();
    assert_eq!(client_names, vec!["Claude Code"]);

    let fallback = setup(BrowserBehavior::Follow, None).await;
    fallback
        .harness
        .session
        .prompt("/mcp login issues", None)
        .await
        .unwrap();
    let fb_names: Vec<String> = fallback
        .server
        .registrations()
        .into_iter()
        .filter_map(|m| m.get("client_name").and_then(Value::as_str).map(str::to_string))
        .collect();
    assert_eq!(fb_names, vec!["pi"]);

    fallback.server.close().await;
    fallback.harness.cleanup();
    ts.server.close().await;
    ts.harness.cleanup();
}

#[tokio::test]
async fn adds_the_listening_port_to_a_callback_url_without_one() {
    let ts = setup(
        BrowserBehavior::Follow,
        Some(McpOAuthConfig {
            callback_url: Some("http://127.0.0.1/oauth/done".to_string()),
            ..Default::default()
        }),
    )
    .await;
    ts.harness.session.prompt("/mcp login issues", None).await.unwrap();
    assert_eq!(
        ts.notifications
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .last()
            .map(String::as_str),
        Some("Signed in to MCP server \"issues\" (1 tools).")
    );
    {
        let urls = ts.opened.lock().unwrap_or_else(PoisonError::into_inner).clone();
        let redirect_uri = urls[0]
            .query_pairs()
            .find(|(k, _)| k == "redirect_uri")
            .map(|(_, v)| v.into_owned())
            .unwrap_or_default();
        assert!(redirect_uri.starts_with("http://127.0.0.1:"));
        assert!(redirect_uri.ends_with("/oauth/done"));
    }

    let port = free_port().await;
    let fixed = setup(
        BrowserBehavior::Follow,
        Some(McpOAuthConfig {
            callback_url: Some("http://127.0.0.1/oauth/done".to_string()),
            callback_port: Some(i64::from(port)),
            ..Default::default()
        }),
    )
    .await;
    fixed.harness.session.prompt("/mcp login issues", None).await.unwrap();
    {
        let urls = fixed.opened.lock().unwrap_or_else(PoisonError::into_inner).clone();
        let redirect_uri = urls[0]
            .query_pairs()
            .find(|(k, _)| k == "redirect_uri")
            .map(|(_, v)| v.into_owned())
            .unwrap_or_default();
        assert_eq!(redirect_uri, format!("http://127.0.0.1:{port}/oauth/done"));
    }

    fixed.server.close().await;
    fixed.harness.cleanup();
    ts.server.close().await;
    ts.harness.cleanup();
}

#[tokio::test]
async fn uses_credentials_from_pi_mcp_login_on_the_next_turn() {
    let ts = setup(BrowserBehavior::Follow, None).await;
    let agent_dir = pi_js::fs::mkdtemp(&pi_js::path::join(&[&pi_js::env::tmp_dir(), "pi-mcp-login-"])).unwrap();
    pi_js::fs::write_file(
        &pi_js::path::join(&[&agent_dir, "mcp.json"]),
        pi_js::json::stringify(&json!({ "mcpServers": { "issues": { "url": ts.server.url } } })).as_bytes(),
        None,
    )
    .unwrap();

    let output = Arc::new(Mutex::new(Vec::<String>::new()));
    let out1 = Arc::clone(&output);
    let out2 = Arc::clone(&output);
    let backend_dyn: Arc<dyn AuthStorageBackend> = ts.backend.clone();
    let exit_code = run_mcp_command(
        &["login".to_string(), "issues".to_string()],
        McpCommandOptions {
            cwd: agent_dir.clone(),
            agent_dir: agent_dir.clone(),
            credentials: Some(McpOAuthCredentialStore::new(Some(backend_dyn), None)),
            provider_token: None,
            open_url: Some(Arc::new(|url_str| {
                let u = url_str.to_string();
                tokio::spawn(async move {
                    let _ = reqwest::Client::new().get(&u).send().await;
                });
            })),
            log: Some(Arc::new(move |line| {
                out1.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(line.to_string());
            })),
            error: Some(Arc::new(move |line| {
                out2.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(line.to_string());
            })),
        },
    )
    .await
    .unwrap();
    assert_eq!(exit_code, 0);
    assert_eq!(
        output
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .last()
            .map(String::as_str),
        Some("Signed in to MCP server \"issues\" (1 tools).")
    );

    let who = call_whoami(&ts.harness).await;
    assert_eq!(
        get_message_text(&AgentMessage::from(Message::ToolResult(who))),
        "token access-1"
    );

    let _ = pi_js::fs::rm(&agent_dir, true, true);
    ts.server.close().await;
    ts.harness.cleanup();
}
