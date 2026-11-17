//! Port of packages/coding-agent/test/suite/agent-session-mcp.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::{Arc, Mutex, PoisonError};

use indexmap::IndexMap;
use pi_agent_core::types::AgentMessage;
use pi_ai::providers::faux::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxResponseStep, faux_assistant_message, faux_tool_call,
};
use pi_ai::types::{Message, StopReason, SystemMessage, ToolResultMessage};
use pi_coding_agent::core::agent_session::ExtensionBindings;
use pi_coding_agent::core::extensions::types::{
    ExtensionApi, ExtensionError, ExtensionUIContext, ExtensionUIDialogOptions, InlineExtension, NotifyType,
    SessionEntry, ToolAnnotations, ToolDefinition,
};
use pi_coding_agent::core::resource_loader::{DefaultResourceLoader, DefaultResourceLoaderOptions};
use pi_coding_agent::core::session_manager::SessionManager;
use pi_coding_agent::extensions::codemode::{CodemodeExtensionOptions, create_codemode_extension};
use pi_coding_agent::extensions::mcp::config::{
    LoadedMcpConfig, McpExposure, McpHttpServerConfig, McpScope, McpServerConfig, McpServerEntry,
};
use pi_coding_agent::extensions::mcp::runtime::{CreatedTransport, McpTransportFactory};
use pi_coding_agent::extensions::mcp::tools::create_mcp_tool_name;
use pi_coding_agent::extensions::mcp::{MCP_SERVERS_SECTION, McpExtensionOptions, create_mcp_extension};
use pi_coding_agent::extensions::tool_search::create_tool_search_extension;
use pi_coding_agent::extensions::tool_search::tool::TOOL_SEARCH_DESCRIPTION;
use pi_js::vendor::typebox::t;
use pi_mcp::protocol::jsonrpc::{JsonRpcNotification, JsonRpcResponse, JsonRpcSuccessResponse, JsonRpcVersion};
use pi_mcp::testing::{InMemoryTransport, create_in_memory_transport_pair};
use pi_mcp::transports::transport::McpTransport;
use pi_mcp::{JsonRpcMessage, LATEST_PROTOCOL_VERSION};
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use support::suite::harness::{
    Harness, HarnessOptions, create_harness, get_assistant_texts, get_message_text, get_tool_result,
};

const TINY_PNG_BASE64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFBQIAX8jx0gAAAABJRU5ErkJggg==";

fn default_server_tools() -> Vec<Value> {
    vec![
        json!({
            "name": "search",
            "description": "Search the docs.",
            "inputSchema": { "type": "object", "properties": { "query": { "type": "string" } }, "required": ["query"] },
            "outputSchema": {
                "type": "object",
                "properties": { "hits": { "type": "array", "items": { "type": "string" } } },
                "required": ["hits"]
            }
        }),
        json!({
            "name": "fail",
            "description": "Always fails.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "title": "Fail", "destructiveHint": true, "readOnlyHint": false, "idempotentHint": "yes" }
        }),
        json!({
            "name": "shot",
            "description": "Returns an image.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
    ]
}

#[derive(Clone)]
struct FakeServerOptions {
    list_tools: Arc<dyn Fn() -> Vec<Value> + Send + Sync>,
    resources: bool,
    instructions: Option<String>,
    initialize_delay_ms: Option<u64>,
    never_respond_initialize: bool,
}

impl Default for FakeServerOptions {
    fn default() -> Self {
        Self {
            list_tools: Arc::new(default_server_tools),
            resources: false,
            instructions: None,
            initialize_delay_ms: None,
            never_respond_initialize: false,
        }
    }
}

fn create_fake_server(calls: Arc<Mutex<Vec<String>>>, options: FakeServerOptions) -> (InMemoryTransport, InMemoryTransport) {
    let pair = create_in_memory_transport_pair();
    let server = pair.server.clone();
    let srv_for_msg = server.clone();
    let _unsub = server.on_message(Arc::new(move |message| {
        let JsonRpcMessage::Request(request) = message else {
            return;
        };
        let req_id = request.id.clone();
        let method = request.method.clone();
        let params = request.params.clone();
        let srv = srv_for_msg.clone();
        let opts = options.clone();
        let calls_ref = Arc::clone(&calls);

        if method == "initialize" && opts.never_respond_initialize {
            return;
        }
        let respond = move || {
            let result = match method.as_str() {
                "initialize" => {
                    let mut capabilities = json!({ "tools": {} });
                    if opts.resources {
                        capabilities["resources"] = json!({});
                    }
                    let mut out = json!({
                        "protocolVersion": LATEST_PROTOCOL_VERSION,
                        "capabilities": capabilities,
                        "serverInfo": { "name": "docs", "version": "1.0.0" },
                    });
                    if let Some(ref instr) = opts.instructions {
                        out["instructions"] = Value::String(instr.clone());
                    }
                    out
                }
                "tools/list" => json!({ "tools": (opts.list_tools)() }),
                "resources/list" => json!({
                    "resources": [
                        { "uri": "docs://intro", "name": "intro", "mimeType": "text/markdown", "_meta": { "x": 1 } },
                        { "uri": "ui://docs/viewer", "name": "viewer", "mimeType": "text/html;profile=mcp-app" }
                    ]
                }),
                "resources/templates/list" => json!({
                    "resourceTemplates": [
                        { "uriTemplate": "docs://pages/{slug}", "name": "page", "icons": [{ "src": "data:image/png;base64,AAAA" }] }
                    ]
                }),
                "resources/read" => {
                    let uri = params
                        .as_ref()
                        .and_then(|p| p.get("uri"))
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    calls_ref
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(format!("read:{uri}"));
                    json!({ "contents": [{ "uri": uri, "mimeType": "text/markdown", "text": format!("# {uri}") }] })
                }
                "tools/call" => {
                    let name = params
                        .as_ref()
                        .and_then(|p| p.get("name"))
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    let args_val = params
                        .as_ref()
                        .and_then(|p| p.get("arguments"))
                        .cloned()
                        .unwrap_or_else(|| json!({}));
                    calls_ref
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(format!("{name}:{}", pi_js::json::stringify(&args_val)));
                    if name == "search" {
                        let query = args_val.get("query").and_then(Value::as_str).unwrap_or("");
                        let hits = vec![format!("{query} guide"), format!("{query} faq")];
                        json!({
                            "content": [{ "type": "text", "text": hits.join("\n") }],
                            "structuredContent": { "hits": hits }
                        })
                    } else if name == "shot" {
                        json!({
                            "content": [{ "type": "image", "data": TINY_PNG_BASE64, "mimeType": "image/png" }]
                        })
                    } else {
                        json!({
                            "content": [{ "type": "text", "text": "server exploded" }],
                            "isError": true
                        })
                    }
                }
                _ => json!({}),
            };
            JsonRpcMessage::Response(JsonRpcResponse::Success(JsonRpcSuccessResponse {
                jsonrpc: JsonRpcVersion::V2,
                id: req_id,
                result,
            }))
        };

        let delay = if request.method == "initialize" {
            opts.initialize_delay_ms
        } else {
            None
        };
        tokio::spawn(async move {
            if let Some(ms) = delay.filter(|d| *d > 0) {
                pi_js::time::sleep(ms).await;
            } else {
                tokio::task::yield_now().await;
            }
            let _ = srv.send(respond()).await;
        });
    }));
    std::mem::forget(_unsub);
    (pair.client, server)
}

struct RecordingUiContext {
    notifications: Arc<Mutex<Vec<String>>>,
}

#[async_trait::async_trait]
impl ExtensionUIContext for RecordingUiContext {
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
        _opts: Option<ExtensionUIDialogOptions>,
    ) -> pi_js::Result<Option<String>> {
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

async fn wait_until<F>(mut cond: F, label: &str)
where
    F: FnMut() -> bool,
{
    for _ in 0..300 {
        if cond() {
            return;
        }
        pi_js::time::sleep(10).await;
    }
    assert!(cond(), "timed out waiting for: {label}");
}

#[derive(Default)]
struct McpSetupOptions {
    auto_enable_codemode: Option<bool>,
    built_in_tools: Option<Vec<String>>,
    extension_factories: Vec<InlineExtension>,
    tool_exposure: Option<IndexMap<String, McpExposure>>,
    resources: bool,
    without_tool_search: bool,
    description: Option<String>,
    instructions: Option<String>,
    list_tools: Option<Arc<dyn Fn() -> Vec<Value> + Send + Sync>>,
}

struct McpSetup {
    harness: Harness,
    calls: Arc<Mutex<Vec<String>>>,
    servers: Arc<Mutex<Vec<InMemoryTransport>>>,
    notifications: Arc<Mutex<Vec<String>>>,
}

async fn setup_mcp(exposure: McpExposure, opts: McpSetupOptions) -> McpSetup {
    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let notifications = Arc::new(Mutex::new(Vec::<String>::new()));
    let servers = Arc::new(Mutex::new(Vec::<InMemoryTransport>::new()));
    let entry = McpServerEntry {
        name: "docs".to_string(),
        config: McpServerConfig::Http(McpHttpServerConfig {
            exposure: Some(exposure),
            description: opts.description,
            tool_exposure: opts.tool_exposure,
            enabled: None,
            timeout: None,
            r#type: None,
            url: "http://unused.invalid".to_string(),
            headers: None,
            oauth: None,
        }),
        auth_provider: None,
        source: "test".to_string(),
        scope: None,
    };
    let auto_enable = opts.auto_enable_codemode;
    let calls_c = Arc::clone(&calls);
    let srvs_c = Arc::clone(&servers);
    let list_fn = opts.list_tools.unwrap_or_else(|| Arc::new(default_server_tools));
    let resources = opts.resources;
    let instructions = opts.instructions;

    let transport_factory: McpTransportFactory = Arc::new(move |_entry, _url, _auth| {
        let (client, server) = create_fake_server(
            Arc::clone(&calls_c),
            FakeServerOptions {
                list_tools: Arc::clone(&list_fn),
                resources,
                instructions: instructions.clone(),
                initialize_delay_ms: None,
                never_respond_initialize: false,
            },
        );
        srvs_c.lock().unwrap_or_else(PoisonError::into_inner).push(server.clone());
        tokio::spawn(async move {
            let _ = server.start().await;
        });
        Ok(CreatedTransport::Custom(Arc::new(client)))
    });

    let mut factories = opts.extension_factories;
    factories.push(InlineExtension::Factory(create_codemode_extension(
        CodemodeExtensionOptions::default(),
    )));
    if !opts.without_tool_search {
        factories.push(InlineExtension::Factory(create_tool_search_extension()));
    }
    factories.push(InlineExtension::Factory(create_mcp_extension(McpExtensionOptions {
        load_config: Some(Arc::new(move |_| LoadedMcpConfig {
            servers: vec![entry.clone()],
            auto_enable_codemode: auto_enable,
            errors: vec![],
        })),
        create_transport: Some(transport_factory),
        ..Default::default()
    })));

    let harness = create_harness(HarnessOptions {
        initial_active_tool_names: Some(opts.built_in_tools.unwrap_or_default()),
        extension_factories: Some(factories),
        ..Default::default()
    })
    .await;

    let ui = Arc::new(RecordingUiContext {
        notifications: Arc::clone(&notifications),
    });
    harness
        .session
        .bind_extensions(ExtensionBindings {
            ui_context: Some(ui),
            ..Default::default()
        })
        .await
        .unwrap();

    wait_until(
        || {
            harness
                .session
                .get_all_tools()
                .into_iter()
                .any(|t| t.name == "mcp__docs__search")
        },
        "mcp__docs__search tool registered",
    )
    .await;

    McpSetup {
        harness,
        calls,
        servers,
        notifications,
    }
}

fn declared_tool_names(harness: &Harness) -> Vec<String> {
    harness
        .session
        .messages()
        .into_iter()
        .filter_map(|m| match m {
            AgentMessage::Message(Message::System(sys)) => sys.tools_added,
            _ => None,
        })
        .flatten()
        .map(|t| t.name)
        .collect()
}

fn servers_section(harness: &Harness) -> Option<String> {
    let mut section: Option<String> = None;
    for m in harness.session.messages() {
        if let AgentMessage::Message(Message::System(sys)) = m
            && let Some(sections) = sys.sections
            && let Some(val) = sections.get(MCP_SERVERS_SECTION)
        {
            section = val.clone();
        }
    }
    section
}

fn tool_result(harness: &Harness, name: &str) -> ToolResultMessage {
    get_tool_result(harness, name).expect("tool result")
}

#[tokio::test]
async fn exposes_codemode_only_mcp_tools_through_codemode_and_hides_them_from_the_model() {
    let st = setup_mcp(McpExposure::Codemode, McpSetupOptions::default()).await;
    let search_name = create_mcp_tool_name("docs", "search", |_| false);
    let code = format!(
        r#"
        const [a, b] = await Promise.allSettled([
            tools.{search_name}({{ query: "mcp" }}),
            tools.{search_name}({{ query: "pi" }}),
        ]);
        const failure = await tools.mcp__docs__fail({{}});
        const shot = await tools.mcp__docs__shot({{}});
        image(shot.content[0]);
        text(JSON.stringify({{
            hits: [...a.value.structuredContent.hits, ...b.value.structuredContent.hits],
            failed: failure.isError,
            failure: failure.content[0].text,
            found: ALL_TOOLS.filter((tool) => tool.name.includes("search")).map((tool) => tool.name),
        }}));
        "#
    );
    st.harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Blocks(vec![faux_tool_call("codemode", json!({ "code": code }))]),
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

    st.harness.session.prompt("search the docs", None).await.unwrap();

    assert_eq!(st.harness.session.get_active_tool_names(), vec!["codemode"]);
    assert_eq!(declared_tool_names(&st.harness), vec!["codemode"]);
    assert_eq!(
        st.harness.session.get_callable_tool_names(),
        vec![search_name.clone(), "mcp__docs__fail".to_string(), "mcp__docs__shot".to_string()]
    );
    let cm_desc = st
        .harness
        .session
        .agent()
        .state()
        .tools
        .into_iter()
        .find(|t| t.name == "codemode")
        .map(|t| t.description)
        .unwrap_or_default();
    assert!(!cm_desc.contains("mcp__docs"));
    assert!(!cm_desc.contains("Shared MCP Types:"));

    let result = tool_result(&st.harness, "codemode");
    assert!(!result.is_error);
    assert_eq!(result.content.len(), 3);
    assert_eq!(
        result.content[1],
        pi_ai::types::UserContentBlock::Image(pi_ai::ImageContent {
            data: TINY_PNG_BASE64.to_string(),
            mime_type: "image/png".to_string(),
        })
    );
    let txt = match &result.content[2] {
        pi_ai::types::UserContentBlock::Text(t) => &t.text,
        _ => panic!("expected text block"),
    };
    let parsed: Value = pi_js::json::parse(txt).unwrap();
    assert_eq!(
        parsed,
        json!({
            "hits": ["mcp guide", "mcp faq", "pi guide", "pi faq"],
            "failed": true,
            "failure": "server exploded",
            "found": [search_name],
        })
    );
    assert_eq!(
        *st.calls.lock().unwrap_or_else(PoisonError::into_inner),
        vec![
            "search:{\"query\":\"mcp\"}",
            "search:{\"query\":\"pi\"}",
            "fail:{}",
            "shot:{}",
        ]
    );
    st.harness.cleanup();
}

#[tokio::test]
async fn keeps_codemode_only_mcp_tools_callable_across_tree_navigation() {
    let st = setup_mcp(McpExposure::Codemode, McpSetupOptions::default()).await;
    let search_name = create_mcp_tool_name("docs", "search", |_| false);
    st.harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("one".to_string()),
            None,
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("two".to_string()),
            None,
        )),
    ]);
    st.harness.session.prompt("first", None).await.unwrap();
    st.harness.session.prompt("second", None).await.unwrap();
    assert_eq!(st.harness.session.get_active_tool_names(), vec!["codemode"]);
    assert!(st.harness.session.get_callable_tool_names().contains(&search_name));

    let first_assistant_id = st
        .harness
        .session_manager
        .get_branch(None)
        .into_iter()
        .find_map(|e| match e {
            SessionEntry::Message(m) if m.message.role() == Some("assistant") => Some(m.id),
            _ => None,
        })
        .expect("No assistant entry");
    st.harness
        .session
        .navigate_tree(&first_assistant_id, None)
        .await
        .unwrap();

    assert_eq!(st.harness.session.get_active_tool_names(), vec!["codemode"]);
    assert!(st.harness.session.get_callable_tool_names().contains(&search_name));
    st.harness.cleanup();
}

async fn run_dash_underscore_routing(reverse: bool) {
    let list_fn: Arc<dyn Fn() -> Vec<Value> + Send + Sync> = Arc::new(move || {
        let mut t = vec![
            json!({ "name": "read-file", "description": "dashed", "inputSchema": { "type": "object", "properties": {} } }),
            json!({ "name": "read_file", "description": "underscored", "inputSchema": { "type": "object", "properties": {} } }),
        ];
        if reverse {
            t.reverse();
        }
        t.extend(default_server_tools());
        t
    });
    let st = setup_mcp(
        McpExposure::Codemode,
        McpSetupOptions {
            list_tools: Some(list_fn),
            ..Default::default()
        },
    )
    .await;
    let code = r#"for (const query of ["dashed", "underscored"]) await tools[(await searchTools(query))[0].name]({});"#;
    st.harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Blocks(vec![faux_tool_call("codemode", json!({ "code": code }))]),
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
    st.harness.session.prompt("go", None).await.unwrap();
    assert_eq!(
        *st.calls.lock().unwrap_or_else(PoisonError::into_inner),
        vec!["read-file:{}", "read_file:{}"]
    );
    st.harness.cleanup();
}

#[tokio::test]
async fn routes_tools_whose_names_differ_only_in_dash_and_underscore_forward() {
    run_dash_underscore_routing(false).await;
}

#[tokio::test]
async fn routes_tools_whose_names_differ_only_in_dash_and_underscore_reverse() {
    run_dash_underscore_routing(true).await;
}

#[tokio::test]
async fn rejects_direct_model_calls_to_codemode_only_mcp_tools() {
    let st = setup_mcp(McpExposure::Codemode, McpSetupOptions::default()).await;
    let search_name = create_mcp_tool_name("docs", "search", |_| false);
    st.harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Blocks(vec![faux_tool_call(&search_name, json!({ "query": "x" }))]),
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
    st.harness.session.prompt("go", None).await.unwrap();
    let res = tool_result(&st.harness, "mcp__docs__search");
    assert!(res.is_error);
    assert_eq!(
        get_message_text(&AgentMessage::from(Message::ToolResult(res))),
        "Tool mcp__docs__search not found"
    );
    assert!(st.calls.lock().unwrap_or_else(PoisonError::into_inner).is_empty());
    st.harness.cleanup();
}

#[tokio::test]
async fn declares_directly_exposed_mcp_tools_to_the_model() {
    let st = setup_mcp(McpExposure::Direct, McpSetupOptions::default()).await;
    st.harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Blocks(vec![faux_tool_call(
                "mcp__docs__search",
                json!({ "query": "direct" }),
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
    st.harness.session.prompt("go", None).await.unwrap();

    assert_eq!(
        declared_tool_names(&st.harness),
        vec!["mcp__docs__search", "mcp__docs__fail", "mcp__docs__shot"]
    );
    assert!(!st.harness.session.get_active_tool_names().contains(&"codemode".to_string()));
    let fail_ann = st
        .harness
        .session
        .get_all_tools()
        .into_iter()
        .find(|t| t.name == "mcp__docs__fail")
        .and_then(|t| t.annotations);
    assert_eq!(
        fail_ann,
        Some(ToolAnnotations {
            destructive_hint: Some(true),
            read_only_hint: Some(false),
            idempotent_hint: None,
            open_world_hint: None,
        })
    );
    let search_ann = st
        .harness
        .session
        .get_all_tools()
        .into_iter()
        .find(|t| t.name == "mcp__docs__search")
        .and_then(|t| t.annotations);
    assert_eq!(search_ann, None);
    let res = tool_result(&st.harness, "mcp__docs__search");
    assert_eq!(
        get_message_text(&AgentMessage::from(Message::ToolResult(res))),
        "direct guide\ndirect faq"
    );
    st.harness.cleanup();
}

async fn run_list_changed_test(exposure: McpExposure) {
    let current_tools = Arc::new(Mutex::new(default_server_tools()));
    let ct_c = Arc::clone(&current_tools);
    let st = setup_mcp(
        exposure,
        McpSetupOptions {
            list_tools: Some(Arc::new(move || {
                ct_c.lock().unwrap_or_else(PoisonError::into_inner).clone()
            })),
            ..Default::default()
        },
    )
    .await;
    st.harness.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text("ready".to_string()),
        None,
    ))]);
    st.harness.session.prompt("start", None).await.unwrap();

    let reachable = |h: &Harness| -> Vec<String> {
        if exposure == McpExposure::Direct {
            h.session.get_active_tool_names()
        } else {
            h.session.get_callable_tool_names()
        }
    };
    assert!(reachable(&st.harness).contains(&"mcp__docs__fail".to_string()));

    *current_tools.lock().unwrap_or_else(PoisonError::into_inner) = default_server_tools()
        .into_iter()
        .filter(|t| t.get("name").and_then(Value::as_str) != Some("fail"))
        .collect();
    let srv = st.servers.lock().unwrap_or_else(PoisonError::into_inner)[0].clone();
    srv.send(JsonRpcMessage::Notification(JsonRpcNotification {
        jsonrpc: JsonRpcVersion::V2,
        method: "notifications/tools/list_changed".to_string(),
        params: None,
    }))
    .await
    .unwrap();
    wait_until(
        || !reachable(&st.harness).contains(&"mcp__docs__fail".to_string()),
        "fail tool withdrawn",
    )
    .await;
    assert!(reachable(&st.harness).contains(&"mcp__docs__search".to_string()));

    *current_tools.lock().unwrap_or_else(PoisonError::into_inner) = default_server_tools();
    srv.send(JsonRpcMessage::Notification(JsonRpcNotification {
        jsonrpc: JsonRpcVersion::V2,
        method: "notifications/tools/list_changed".to_string(),
        params: None,
    }))
    .await
    .unwrap();
    wait_until(
        || reachable(&st.harness).contains(&"mcp__docs__fail".to_string()),
        "fail tool restored",
    )
    .await;
    st.harness.cleanup();
}

#[tokio::test]
async fn withdraws_and_restores_direct_mcp_tools_the_server_changes() {
    run_list_changed_test(McpExposure::Direct).await;
}

#[tokio::test]
async fn withdraws_and_restores_codemode_mcp_tools_the_server_changes() {
    run_list_changed_test(McpExposure::Codemode).await;
}

#[tokio::test]
async fn lists_and_reads_resources_with_codexs_resource_tools() {
    let st = setup_mcp(
        McpExposure::Direct,
        McpSetupOptions {
            resources: true,
            ..Default::default()
        },
    )
    .await;
    st.harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Blocks(vec![
                faux_tool_call("list_mcp_resources", json!({})),
                faux_tool_call("list_mcp_resource_templates", json!({ "server": "docs" })),
                faux_tool_call("read_mcp_resource", json!({ "server": "docs", "uri": "docs://pages/setup" })),
            ]),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Blocks(vec![faux_tool_call(
                "read_mcp_resource",
                json!({ "server": "nope", "uri": "docs://x" }),
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

    st.harness.session.prompt("read", None).await.unwrap();

    let active = st.harness.session.get_active_tool_names();
    for expected in [
        "list_mcp_resources",
        "list_mcp_resource_templates",
        "read_mcp_resource",
    ] {
        assert!(active.contains(&expected.to_string()));
    }
    let listed: Value = pi_js::json::parse(&get_message_text(&AgentMessage::from(Message::ToolResult(
        tool_result(&st.harness, "list_mcp_resources"),
    ))))
    .unwrap();
    assert_eq!(
        listed,
        json!({ "resources": [{ "server": "docs", "uri": "docs://intro", "name": "intro", "mimeType": "text/markdown" }] })
    );
    let templates: Value = pi_js::json::parse(&get_message_text(&AgentMessage::from(Message::ToolResult(
        tool_result(&st.harness, "list_mcp_resource_templates"),
    ))))
    .unwrap();
    assert_eq!(
        templates,
        json!({
            "server": "docs",
            "resourceTemplates": [{ "server": "docs", "uriTemplate": "docs://pages/{slug}", "name": "page" }]
        })
    );
    let reads: Vec<ToolResultMessage> = st
        .harness
        .session
        .messages()
        .into_iter()
        .filter_map(|m| match m {
            AgentMessage::Message(Message::ToolResult(tr)) if tr.tool_name == "read_mcp_resource" => Some(tr),
            _ => None,
        })
        .collect();
    assert_eq!(
        get_message_text(&AgentMessage::from(Message::ToolResult(reads[0].clone()))),
        "# docs://pages/setup"
    );
    assert!(reads[1].is_error);
    assert_eq!(
        get_message_text(&AgentMessage::from(Message::ToolResult(reads[1].clone()))),
        "MCP server \"nope\" has no resources. Servers with resources: docs"
    );
    assert_eq!(
        *st.calls.lock().unwrap_or_else(PoisonError::into_inner),
        vec!["read:docs://pages/setup"]
    );
    let ann = st
        .harness
        .session
        .get_all_tools()
        .into_iter()
        .find(|t| t.name == "read_mcp_resource")
        .and_then(|t| t.annotations);
    assert_eq!(
        ann,
        Some(ToolAnnotations {
            read_only_hint: Some(true),
            destructive_hint: None,
            idempotent_hint: None,
            open_world_hint: None,
        })
    );
    st.harness.cleanup();
}

#[tokio::test]
async fn makes_the_resource_tools_callable_from_codemode_for_codemode_servers() {
    let st = setup_mcp(
        McpExposure::Codemode,
        McpSetupOptions {
            resources: true,
            ..Default::default()
        },
    )
    .await;
    let code = "const listed = await tools.list_mcp_resources({ server: \"docs\" });\nconst read = await tools.read_mcp_resource({ server: \"docs\", uri: listed.resources[0].uri });\nreturn { uris: listed.resources.map((r) => r.uri), text: read.contents[0].text };";
    st.harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Blocks(vec![faux_tool_call("codemode", json!({ "code": code }))]),
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

    st.harness.session.prompt("read", None).await.unwrap();

    assert_eq!(st.harness.session.get_active_tool_names(), vec!["codemode"]);
    let raw = get_message_text(&AgentMessage::from(Message::ToolResult(tool_result(
        &st.harness,
        "codemode",
    ))));
    let last_line = raw.lines().last().unwrap_or("");
    let parsed: Value = pi_js::json::parse(last_line).unwrap();
    assert_eq!(
        parsed,
        json!({ "uris": ["docs://intro"], "text": "# docs://intro" })
    );
    assert_eq!(
        *st.calls.lock().unwrap_or_else(PoisonError::into_inner),
        vec!["read:docs://intro"]
    );
    st.harness.cleanup();
}

#[tokio::test]
async fn applies_per_tool_exposure_overrides() {
    let mut tool_exposure = IndexMap::new();
    tool_exposure.insert("search".to_string(), McpExposure::Direct);
    tool_exposure.insert("s*".to_string(), McpExposure::Codemode);

    let st = setup_mcp(
        McpExposure::Hidden,
        McpSetupOptions {
            tool_exposure: Some(tool_exposure),
            ..Default::default()
        },
    )
    .await;
    st.harness.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text("ready".to_string()),
        None,
    ))]);
    st.harness.session.prompt("start", None).await.unwrap();

    assert_eq!(
        declared_tool_names(&st.harness),
        vec!["codemode", "mcp__docs__search"]
    );
    assert_eq!(
        st.harness.session.get_callable_tool_names(),
        vec!["mcp__docs__search", "mcp__docs__shot"]
    );
    st.harness.cleanup();
}

#[tokio::test]
async fn describes_the_server_with_its_configured_description_and_returns_its_instructions_to_scripts() {
    let st = setup_mcp(
        McpExposure::Codemode,
        McpSetupOptions {
            description: Some("Search the product docs".to_string()),
            instructions: Some("Always search before reading.".to_string()),
            built_in_tools: Some(vec!["tool_search".to_string()]),
            ..Default::default()
        },
    )
    .await;
    let code = r#"const docs = await describeNamespace("mcp__docs");
const aliases = await Promise.all(["docs", "mcp__docs"].map((name) => describeNamespace(name)));
return { docs, sameForAliases: aliases.every((alias) => JSON.stringify(alias) === JSON.stringify(docs)), none: await describeNamespace("mcp__nope") };"#;
    st.harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Blocks(vec![faux_tool_call("codemode", json!({ "code": code }))]),
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

    st.harness.session.prompt("go", None).await.unwrap();

    let description = |name: &str| -> String {
        st.harness
            .session
            .agent()
            .state()
            .tools
            .into_iter()
            .find(|t| t.name == name)
            .map(|t| t.description)
            .unwrap_or_default()
    };
    assert!(!description("codemode").contains("mcp__docs"));
    assert_eq!(description("tool_search"), TOOL_SEARCH_DESCRIPTION);
    assert!(!description("codemode").contains("Always search"));
    assert!(!description("tool_search").contains("Always search"));

    let raw = get_message_text(&AgentMessage::from(Message::ToolResult(tool_result(
        &st.harness,
        "codemode",
    ))));
    let parsed: Value = pi_js::json::parse(raw.lines().last().unwrap_or("")).unwrap();
    assert_eq!(
        parsed,
        json!({
            "docs": {
                "name": "mcp__docs",
                "description": "Search the product docs",
                "instructions": "Always search before reading.",
                "tools": ["mcp__docs__search", "mcp__docs__fail", "mcp__docs__shot"]
            },
            "sameForAliases": true
        })
    );
    assert!(
        servers_section(&st.harness)
            .unwrap_or_default()
            .contains("- mcp__docs (codemode): Search the product docs")
    );
    st.harness.cleanup();
}

#[tokio::test]
async fn lists_servers_by_the_first_line_of_their_instructions_without_a_configured_description() {
    let st = setup_mcp(
        McpExposure::Deferred,
        McpSetupOptions {
            instructions: Some("Docs search.\nLong guidance.".to_string()),
            ..Default::default()
        },
    )
    .await;
    st.harness.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text("done".to_string()),
        None,
    ))]);

    st.harness.session.prompt("go", None).await.unwrap();

    let section = servers_section(&st.harness).unwrap_or_default();
    assert!(section.contains("- mcp__docs (tool_search): Docs search.\n"));
    assert!(!section.contains("Long guidance"));
    st.harness.cleanup();
}

#[tokio::test]
async fn does_not_activate_codemode_when_auto_enable_codemode_is_false() {
    let st = setup_mcp(
        McpExposure::Codemode,
        McpSetupOptions {
            auto_enable_codemode: Some(false),
            ..Default::default()
        },
    )
    .await;
    st.harness.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text("ready".to_string()),
        None,
    ))]);
    st.harness.session.prompt("start", None).await.unwrap();

    assert!(st.harness.session.get_active_tool_names().is_empty());
    assert!(
        st.harness
            .session
            .get_callable_tool_names()
            .contains(&"mcp__docs__search".to_string())
    );
    assert_eq!(
        *st.notifications.lock().unwrap_or_else(PoisonError::into_inner),
        vec![
            "MCP tools are only reachable from the codemode or tool_search tool, but neither is active (autoEnableCodemode is false); they cannot be called."
        ]
    );
    st.harness.cleanup();
}

#[tokio::test]
async fn treats_codemode_mcp_tools_as_reachable_through_an_active_tool_search() {
    let st = setup_mcp(
        McpExposure::Codemode,
        McpSetupOptions {
            auto_enable_codemode: Some(false),
            built_in_tools: Some(vec!["tool_search".to_string()]),
            ..Default::default()
        },
    )
    .await;
    st.harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Blocks(vec![faux_tool_call(
                "tool_search",
                json!({ "query": "search the docs", "limit": 1 }),
            )]),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Blocks(vec![faux_tool_call(
                "mcp__docs__search",
                json!({ "query": "loaded" }),
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
    st.harness.session.prompt("go", None).await.unwrap();

    assert_eq!(
        st.harness.session.get_active_tool_names(),
        vec!["tool_search", "mcp__docs__search"]
    );
    assert_eq!(
        get_message_text(&AgentMessage::from(Message::ToolResult(tool_result(
            &st.harness,
            "mcp__docs__search"
        )))),
        "loaded guide\nloaded faq"
    );
    assert!(st.notifications.lock().unwrap_or_else(PoisonError::into_inner).is_empty());
    st.harness.cleanup();
}

#[tokio::test]
async fn reaches_deferred_mcp_tools_through_codemode_when_tool_search_is_not_available() {
    let st = setup_mcp(
        McpExposure::Deferred,
        McpSetupOptions {
            built_in_tools: Some(vec!["codemode".to_string()]),
            without_tool_search: true,
            ..Default::default()
        },
    )
    .await;
    st.harness.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text("ready".to_string()),
        None,
    ))]);
    st.harness.session.prompt("start", None).await.unwrap();

    assert_eq!(st.harness.session.get_active_tool_names(), vec!["codemode"]);
    assert!(
        st.harness
            .session
            .get_callable_tool_names()
            .contains(&"mcp__docs__search".to_string())
    );
    assert!(st.notifications.lock().unwrap_or_else(PoisonError::into_inner).is_empty());
    st.harness.cleanup();
}

#[tokio::test]
async fn warns_when_deferred_mcp_tools_have_neither_tool_search_nor_codemode() {
    let st = setup_mcp(
        McpExposure::Deferred,
        McpSetupOptions {
            without_tool_search: true,
            ..Default::default()
        },
    )
    .await;
    st.harness.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text("ready".to_string()),
        None,
    ))]);
    st.harness.session.prompt("start", None).await.unwrap();

    assert!(st.harness.session.get_active_tool_names().is_empty());
    assert_eq!(
        *st.notifications.lock().unwrap_or_else(PoisonError::into_inner),
        vec!["MCP tools are only reachable from the codemode or tool_search tool, but neither is active; they cannot be called."]
    );
    st.harness.cleanup();
}

#[tokio::test]
async fn does_not_activate_another_extensions_tool_named_codemode() {
    let other_exec = InlineExtension::Factory(Arc::new(|pi: ExtensionApi| async move {
        pi.register_tool(ToolDefinition {
            name: "codemode".to_string(),
            label: "codemode".to_string(),
            description: "Another extension's codemode tool.".to_string(),
            parameters: t::object(&[]),
            prompt_snippet: None,
            prompt_guidelines: None,
            exposure: None,
            default_active: Some(false),
            namespace: None,
            output_schema: None,
            annotations: None,
            constrained_sampling: None,
            execution_mode: None,
            prepare_arguments: None,
            render_shell: None,
            render_call: None,
            render_result: None,
            execute: Arc::new(|_id, _params, _sig, _upd, _ctx| {
                Box::pin(async {
                    Ok(pi_agent_core::types::AgentToolResult {
                        content: vec![],
                        details: None,
                        ..Default::default()
                    })
                })
            }),
        })
    }));

    let st = setup_mcp(
        McpExposure::Codemode,
        McpSetupOptions {
            extension_factories: vec![other_exec],
            ..Default::default()
        },
    )
    .await;
    st.harness.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text("ready".to_string()),
        None,
    ))]);
    st.harness.session.prompt("start", None).await.unwrap();

    assert!(st.harness.session.get_active_tool_names().is_empty());
    st.harness.cleanup();
}

async fn setup_slow(
    name: &str,
    exposure: Option<McpExposure>,
    tool_exposure: Option<IndexMap<String, McpExposure>>,
    instructions: Option<&str>,
    initialize_delay_ms: Option<u64>,
    never_respond: bool,
    startup_wait_ms: Option<u64>,
) -> (Harness, Arc<Mutex<Vec<String>>>, Arc<Mutex<Vec<String>>>) {
    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let notifications = Arc::new(Mutex::new(Vec::<String>::new()));
    let entry = McpServerEntry {
        name: name.to_string(),
        config: McpServerConfig::Http(McpHttpServerConfig {
            exposure,
            description: None,
            tool_exposure,
            enabled: None,
            timeout: None,
            r#type: None,
            url: "http://unused.invalid".to_string(),
            headers: None,
            oauth: None,
        }),
        auth_provider: None,
        source: "test".to_string(),
        scope: None,
    };

    let calls_c = Arc::clone(&calls);
    let instr_c = instructions.map(str::to_string);
    let transport_factory: McpTransportFactory = Arc::new(move |_entry, _url, _auth| {
        let (client, server) = create_fake_server(
            Arc::clone(&calls_c),
            FakeServerOptions {
                list_tools: Arc::new(default_server_tools),
                resources: false,
                instructions: instr_c.clone(),
                initialize_delay_ms,
                never_respond_initialize: never_respond,
            },
        );
        tokio::spawn(async move {
            let _ = server.start().await;
        });
        Ok(CreatedTransport::Custom(Arc::new(client)))
    });

    let harness = create_harness(HarnessOptions {
        initial_active_tool_names: Some(vec![]),
        extension_factories: Some(vec![
            InlineExtension::Factory(create_codemode_extension(CodemodeExtensionOptions::default())),
            InlineExtension::Factory(create_tool_search_extension()),
            InlineExtension::Factory(create_mcp_extension(McpExtensionOptions {
                load_config: Some(Arc::new(move |_| LoadedMcpConfig {
                    servers: vec![entry.clone()],
                    auto_enable_codemode: None,
                    errors: vec![],
                })),
                create_transport: Some(transport_factory),
                startup_wait_ms,
                ..Default::default()
            })),
        ]),
        ..Default::default()
    })
    .await;

    let ui = Arc::new(RecordingUiContext {
        notifications: Arc::clone(&notifications),
    });
    harness
        .session
        .bind_extensions(ExtensionBindings {
            ui_context: Some(ui),
            ..Default::default()
        })
        .await
        .unwrap();

    (harness, calls, notifications)
}

#[tokio::test]
async fn does_not_hold_the_first_prompt_for_codemode_servers_that_are_still_connecting() {
    let (harness, _, _) = setup_slow("slow", None, None, None, None, true, None).await;
    harness.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text("ready".to_string()),
        None,
    ))]);

    harness.session.prompt("start", None).await.unwrap();

    assert_eq!(get_assistant_texts(&harness), vec!["ready"]);
    assert_eq!(declared_tool_names(&harness), vec!["codemode"]);
    harness.cleanup();
}

#[tokio::test]
async fn keeps_the_codemode_description_unchanged_when_the_server_connects() {
    let (harness, _, _) = setup_slow("slow", None, None, None, Some(20), false, None).await;
    let get_desc = || {
        harness
            .session
            .agent()
            .state()
            .tools
            .into_iter()
            .find(|t| t.name == "codemode")
            .map(|t| t.description)
    };
    let before = get_desc().expect("codemode description");
    wait_until(
        || {
            harness
                .session
                .get_callable_tool_names()
                .contains(&"mcp__slow__search".to_string())
        },
        "mcp__slow__search callable",
    )
    .await;
    assert_eq!(get_desc(), Some(before));
    harness.cleanup();
}

#[tokio::test]
async fn lists_a_server_before_it_connects_and_appends_its_summary_with_the_next_prompt() {
    let (harness, _, _) = setup_slow("slow", None, None, Some("Slow docs.\nMore."), Some(30), false, None).await;
    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("one".to_string()),
            None,
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("two".to_string()),
            None,
        )),
    ]);

    harness.session.prompt("first", None).await.unwrap();
    wait_until(
        || {
            harness
                .session
                .get_callable_tool_names()
                .contains(&"mcp__slow__search".to_string())
        },
        "mcp__slow__search connected",
    )
    .await;
    harness.session.prompt("second", None).await.unwrap();

    let all_msgs = harness.session.messages();
    let sys_msgs: Vec<(usize, SystemMessage)> = all_msgs
        .iter()
        .enumerate()
        .filter_map(|(idx, m)| match m {
            AgentMessage::Message(Message::System(sys)) => Some((idx, sys.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(sys_msgs.len(), 2);
    let sec0 = sys_msgs[0]
        .1
        .sections
        .as_ref()
        .and_then(|s| s.get(MCP_SERVERS_SECTION))
        .and_then(|v| v.as_deref())
        .unwrap_or("");
    assert!(sec0.contains("- mcp__slow (codemode)\n"));
    let sec1 = sys_msgs[1]
        .1
        .sections
        .as_ref()
        .and_then(|s| s.get(MCP_SERVERS_SECTION))
        .and_then(|v| v.as_deref())
        .unwrap_or("");
    assert!(sec1.contains("- mcp__slow (codemode): Slow docs."));
    let first_assistant_idx = all_msgs.iter().position(|m| m.role() == Some("assistant")).unwrap();
    assert!(sys_msgs[1].0 > first_assistant_idx);
    harness.cleanup();
}

#[tokio::test]
async fn waits_for_servers_with_direct_tools_before_listing_the_servers() {
    let mut tool_exp = IndexMap::new();
    tool_exp.insert("shot".to_string(), McpExposure::Codemode);
    let (harness, _, _) = setup_slow(
        "slow",
        Some(McpExposure::Direct),
        Some(tool_exp),
        Some("Slow docs."),
        Some(30),
        false,
        None,
    )
    .await;
    harness.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text("ready".to_string()),
        None,
    ))]);

    harness.session.prompt("start", None).await.unwrap();

    assert!(
        servers_section(&harness)
            .unwrap_or_default()
            .contains("- mcp__slow (codemode): Slow docs.")
    );
    harness.cleanup();
}

#[tokio::test]
async fn leaves_servers_with_only_direct_tools_out_of_the_servers_section() {
    let (harness, _, _) = setup_slow("slow", Some(McpExposure::Direct), None, None, Some(0), false, None).await;
    harness.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text("ready".to_string()),
        None,
    ))]);

    harness.session.prompt("start", None).await.unwrap();

    assert_eq!(servers_section(&harness), None);
    harness.cleanup();
}

#[tokio::test]
async fn waits_for_the_servers_a_codemode_script_names() {
    let (harness, calls, _) = setup_slow("slow", None, None, None, Some(30), false, None).await;
    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Blocks(vec![faux_tool_call(
                "codemode",
                json!({ "code": "return (await tools.mcp__slow__search({ query: \"q\" })).structuredContent;" }),
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

    harness.session.prompt("go", None).await.unwrap();

    let result = tool_result(&harness, "codemode");
    assert!(!result.is_error);
    let txt = get_message_text(&AgentMessage::from(Message::ToolResult(result)));
    let parsed: Value = pi_js::json::parse(txt.lines().last().unwrap_or("")).unwrap();
    assert_eq!(parsed, json!({ "hits": ["q guide", "q faq"] }));
    assert_eq!(
        *calls.lock().unwrap_or_else(PoisonError::into_inner),
        vec!["search:{\"query\":\"q\"}"]
    );
    harness.cleanup();
}

#[tokio::test]
async fn waits_for_servers_whose_script_identifiers_differ_from_their_names() {
    let (harness, calls, _) = setup_slow("slow-docs", None, None, None, Some(30), false, None).await;
    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Blocks(vec![faux_tool_call(
                "codemode",
                json!({ "code": "await tools.mcp__slow_docs__search({ query: \"q\" }); return (await describeNamespace(\"slow_docs\")).name;" }),
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

    harness.session.prompt("go", None).await.unwrap();

    let result = tool_result(&harness, "codemode");
    assert!(!result.is_error);
    let txt = get_message_text(&AgentMessage::from(Message::ToolResult(result)));
    assert_eq!(txt.lines().last(), Some("mcp__slow_docs"));
    assert_eq!(
        *calls.lock().unwrap_or_else(PoisonError::into_inner),
        vec!["search:{\"query\":\"q\"}"]
    );
    harness.cleanup();
}

#[tokio::test]
async fn does_not_wait_for_servers_a_codemode_script_does_not_name() {
    let (harness, _, _) = setup_slow("slow", None, None, None, None, true, None).await;
    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Blocks(vec![faux_tool_call("codemode", json!({ "code": "return 1 + 1;" }))]),
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

    harness.session.prompt("go", None).await.unwrap();

    let txt = get_message_text(&AgentMessage::from(Message::ToolResult(tool_result(
        &harness,
        "codemode",
    ))));
    assert_eq!(txt.lines().last(), Some("2"));
    harness.cleanup();
}

#[tokio::test]
async fn waits_for_servers_before_tool_search_searches() {
    let (harness, _, _) = setup_slow("slow", Some(McpExposure::Deferred), None, None, Some(30), false, None).await;
    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Blocks(vec![faux_tool_call(
                "tool_search",
                json!({ "query": "search the docs", "limit": 1 }),
            )]),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Blocks(vec![faux_tool_call(
                "mcp__slow__search",
                json!({ "query": "late" }),
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

    harness.session.prompt("go", None).await.unwrap();

    assert_eq!(
        get_message_text(&AgentMessage::from(Message::ToolResult(tool_result(
            &harness,
            "mcp__slow__search"
        )))),
        "late guide\nlate faq"
    );
    harness.cleanup();
}

#[tokio::test]
async fn holds_the_first_prompt_for_servers_with_direct_tools() {
    let (harness, _, _) = setup_slow("slow", Some(McpExposure::Direct), None, None, Some(30), false, None).await;
    harness.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text("ready".to_string()),
        None,
    ))]);

    harness.session.prompt("start", None).await.unwrap();

    assert!(declared_tool_names(&harness).contains(&"mcp__slow__search".to_string()));
    harness.cleanup();
}

#[tokio::test]
async fn holds_the_first_prompt_for_servers_with_direct_tools_only_up_to_startup_wait_ms() {
    let (harness, _, notifications) =
        setup_slow("slow", Some(McpExposure::Direct), None, None, None, true, Some(20)).await;
    harness.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text("ready".to_string()),
        None,
    ))]);

    harness.session.prompt("start", None).await.unwrap();

    assert_eq!(get_assistant_texts(&harness), vec!["ready"]);
    assert!(harness.session.get_active_tool_names().is_empty());
    assert_eq!(
        *notifications.lock().unwrap_or_else(PoisonError::into_inner),
        vec!["MCP servers are still connecting; their tools become available once connected."]
    );
    harness.cleanup();
}

#[tokio::test]
async fn does_not_let_codemode_call_itself_or_inactive_direct_tools() {
    let st = setup_mcp(McpExposure::Direct, McpSetupOptions::default()).await;
    st.harness.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text("ready".to_string()),
        None,
    ))]);
    st.harness.session.prompt("start", None).await.unwrap();
    st.harness
        .session
        .set_active_tools_by_name(&["codemode".to_string(), "mcp__docs__search".to_string()]);

    assert_eq!(
        st.harness.session.get_callable_tool_names(),
        vec!["mcp__docs__search"]
    );
    st.harness.cleanup();
}

#[tokio::test]
async fn finds_tools_from_scripts_with_search_tools_and_describe_tool() {
    let st = setup_mcp(McpExposure::Codemode, McpSetupOptions::default()).await;
    let code = r#"
        const [match] = await searchTools("search the docs", { limit: 1 });
        const none = await searchTools("docs", { namespace: "mcp__other" });
        const declaration = await describeTool(match.name);
        const result = await tools[match.name]({ query: "found" });
        text(JSON.stringify({
            name: match.name,
            sameAsAllTools: ALL_TOOLS.find((tool) => tool.name === match.name).description === match.description,
            none: none.length,
            declared: declaration.includes("codemode tool declaration:"),
            missing: (await describeTool("nope")) === undefined,
            hits: result.structuredContent.hits,
        }));
    "#;
    st.harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Blocks(vec![faux_tool_call("codemode", json!({ "code": code }))]),
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

    st.harness.session.prompt("go", None).await.unwrap();

    let result = tool_result(&st.harness, "codemode");
    assert!(!result.is_error);
    let txt = match &result.content[1] {
        pi_ai::types::UserContentBlock::Text(t) => &t.text,
        _ => panic!("expected text"),
    };
    let parsed: Value = pi_js::json::parse(txt).unwrap();
    assert_eq!(
        parsed,
        json!({
            "name": "mcp__docs__search",
            "sameAsAllTools": true,
            "none": 0,
            "declared": true,
            "missing": true,
            "hits": ["found guide", "found faq"],
        })
    );
    assert_eq!(
        *st.calls.lock().unwrap_or_else(PoisonError::into_inner),
        vec!["search:{\"query\":\"found\"}"]
    );
    st.harness.cleanup();
}

#[tokio::test]
async fn activates_tool_search_for_deferred_mcp_tools_and_keeps_loaded_tools_declared_on_the_branch() {
    let st = setup_mcp(McpExposure::Deferred, McpSetupOptions::default()).await;
    let search_name = create_mcp_tool_name("docs", "search", |_| false);
    st.harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Blocks(vec![faux_tool_call(
                "tool_search",
                json!({ "query": "search the docs", "limit": 1 }),
            )]),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Blocks(vec![faux_tool_call(
                &search_name,
                json!({ "query": "loaded" }),
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

    st.harness.session.prompt("find a docs tool", None).await.unwrap();

    assert_eq!(
        st.harness.session.get_active_tool_names(),
        vec!["tool_search".to_string(), search_name.clone()]
    );
    let ts_desc = st
        .harness
        .session
        .agent()
        .state()
        .tools
        .into_iter()
        .find(|t| t.name == "tool_search")
        .map(|t| t.description)
        .unwrap_or_default();
    assert_eq!(ts_desc, TOOL_SEARCH_DESCRIPTION);

    assert_eq!(
        get_message_text(&AgentMessage::from(Message::ToolResult(tool_result(
            &st.harness,
            "tool_search"
        )))),
        format!("Loaded 1 tool. They are available from your next call:\n- {search_name}: Search the docs.")
    );
    let load_messages: Vec<Vec<String>> = st
        .harness
        .session
        .messages()
        .into_iter()
        .filter_map(|m| match m {
            AgentMessage::Message(Message::System(sys)) => {
                let added = sys.tools_added?;
                if added.iter().any(|t| t.name == search_name) {
                    Some(added.into_iter().map(|t| t.name).collect())
                } else {
                    None
                }
            }
            _ => None,
        })
        .collect();
    assert_eq!(load_messages, vec![vec![search_name.clone()]]);
    assert_eq!(
        get_message_text(&AgentMessage::from(Message::ToolResult(tool_result(
            &st.harness,
            &search_name
        )))),
        "loaded guide\nloaded faq"
    );
    assert_eq!(
        *st.calls.lock().unwrap_or_else(PoisonError::into_inner),
        vec!["search:{\"query\":\"loaded\"}"]
    );

    let branch = st.harness.session_manager.get_branch(None);
    let first_user_id = branch
        .iter()
        .find_map(|e| match e {
            SessionEntry::Message(m) if m.message.role() == Some("user") => Some(m.id.clone()),
            _ => None,
        })
        .expect("first user entry");
    let last_id = branch.last().expect("last entry").id().to_string();
    st.harness.session.navigate_tree(&first_user_id, None).await.unwrap();
    assert!(!st.harness.session.get_active_tool_names().contains(&search_name));
    st.harness.session.navigate_tree(&last_id, None).await.unwrap();
    assert!(st.harness.session.get_active_tool_names().contains(&search_name));
    st.harness.cleanup();
}

#[tokio::test]
async fn finds_nothing_to_load_when_every_matching_tool_is_already_declared() {
    let st = setup_mcp(
        McpExposure::Direct,
        McpSetupOptions {
            built_in_tools: Some(vec!["tool_search".to_string()]),
            ..Default::default()
        },
    )
    .await;
    st.harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Blocks(vec![faux_tool_call("tool_search", json!({ "query": "docs" }))]),
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
    st.harness.session.prompt("go", None).await.unwrap();
    assert_eq!(
        get_message_text(&AgentMessage::from(Message::ToolResult(tool_result(
            &st.harness,
            "tool_search"
        )))),
        "No matching tools found."
    );
    st.harness.cleanup();
}

async fn setup_registered(
    plugins: Vec<InlineExtension>,
    configured: Vec<McpServerEntry>,
) -> (Harness, Arc<Mutex<Vec<McpServerEntry>>>) {
    let connected = Arc::new(Mutex::new(Vec::<McpServerEntry>::new()));
    let conn_c = Arc::clone(&connected);
    let mut factories = plugins;
    factories.push(InlineExtension::Factory(create_codemode_extension(
        CodemodeExtensionOptions::default(),
    )));
    factories.push(InlineExtension::Factory(create_mcp_extension(McpExtensionOptions {
        load_config: Some(Arc::new(move |_| LoadedMcpConfig {
            servers: configured.clone(),
            auto_enable_codemode: None,
            errors: vec![],
        })),
        create_transport: Some(Arc::new(move |entry, _url, _auth| {
            conn_c.lock().unwrap_or_else(PoisonError::into_inner).push(entry.clone());
            let (client, server) = create_fake_server(Arc::new(Mutex::new(Vec::new())), FakeServerOptions::default());
            tokio::spawn(async move {
                let _ = server.start().await;
            });
            Ok(CreatedTransport::Custom(Arc::new(client)))
        })),
        ..Default::default()
    })));

    let harness = create_harness(HarnessOptions {
        initial_active_tool_names: Some(vec![]),
        extension_factories: Some(factories),
        ..Default::default()
    })
    .await;
    harness.session.bind_extensions(Default::default()).await.unwrap();
    (harness, connected)
}

#[tokio::test]
async fn connects_servers_registered_while_extensions_load() {
    let ext = InlineExtension::Factory(Arc::new(|pi: ExtensionApi| async move {
        pi.register_mcp_server("plugin", json!({ "url": "http://plugin.invalid", "exposure": "direct" }))
    }));
    let (harness, connected) = setup_registered(vec![ext], vec![]).await;
    harness.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text("ready".to_string()),
        None,
    ))]);
    harness.session.prompt("start", None).await.unwrap();

    let conns: Vec<(String, Option<McpScope>)> = connected
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .map(|e| (e.name.clone(), e.scope))
        .collect();
    assert_eq!(conns, vec![("plugin".to_string(), Some(McpScope::Extension))]);
    assert!(
        harness
            .session
            .get_active_tool_names()
            .contains(&"mcp__plugin__search".to_string())
    );
    harness.cleanup();
}

#[tokio::test]
async fn connects_and_disconnects_servers_registered_during_the_session() {
    let api_slot = Arc::new(Mutex::new(Option::<ExtensionApi>::None));
    let slot_c = Arc::clone(&api_slot);
    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let s = Arc::clone(&slot_c);
        async move {
            *s.lock().unwrap_or_else(PoisonError::into_inner) = Some(pi);
            Ok(())
        }
    }));
    let (harness, connected) = setup_registered(vec![ext], vec![]).await;
    let pi = api_slot
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
        .expect("extension api");

    pi.register_mcp_server("late", json!({ "url": "http://late.invalid" }))
        .unwrap();
    wait_until(
        || {
            harness
                .session
                .get_callable_tool_names()
                .contains(&"mcp__late__search".to_string())
        },
        "late search registered",
    )
    .await;
    let names: Vec<String> = connected
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .map(|e| e.name.clone())
        .collect();
    assert_eq!(names, vec!["late"]);
    assert!(harness.session.get_active_tool_names().contains(&"codemode".to_string()));

    pi.unregister_mcp_server("late").unwrap();
    wait_until(
        || {
            !harness
                .session
                .get_callable_tool_names()
                .contains(&"mcp__late__search".to_string())
        },
        "late search unregistered",
    )
    .await;
    harness.cleanup();
}

async fn run_prefers_mcp_json_server(ext_name: &str) {
    let configured = McpServerEntry {
        name: "my-docs".to_string(),
        config: McpServerConfig::Http(McpHttpServerConfig {
            exposure: None,
            description: None,
            tool_exposure: None,
            enabled: None,
            timeout: None,
            r#type: None,
            url: "http://config.invalid".to_string(),
            headers: None,
            oauth: None,
        }),
        auth_provider: None,
        source: "mcp.json".to_string(),
        scope: None,
    };
    let name = ext_name.to_string();
    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let n = name.clone();
        async move { pi.register_mcp_server(&n, json!({ "url": "http://plugin.invalid" })) }
    }));
    let (harness, connected) = setup_registered(vec![ext], vec![configured.clone()]).await;
    wait_until(
        || *connected.lock().unwrap_or_else(PoisonError::into_inner) == vec![configured.clone()],
        "configured server connected",
    )
    .await;
    pi_js::time::sleep(10).await;
    assert_eq!(
        *connected.lock().unwrap_or_else(PoisonError::into_inner),
        vec![configured]
    );
    harness.cleanup();
}

#[tokio::test]
async fn prefers_the_mcp_json_server_over_a_registered_my_docs_dash() {
    run_prefers_mcp_json_server("my-docs").await;
}

#[tokio::test]
async fn prefers_the_mcp_json_server_over_a_registered_my_docs_underscore() {
    run_prefers_mcp_json_server("my_docs").await;
}

#[tokio::test]
async fn rejects_names_another_extension_registered() {
    let errors = Arc::new(Mutex::new(Vec::<String>::new()));
    let err_c = Arc::clone(&errors);
    let ext1 = InlineExtension::Factory(Arc::new(|pi: ExtensionApi| async move {
        pi.register_mcp_server("taken", json!({ "url": "http://x.invalid" }))?;
        pi.register_mcp_server("taken", json!({ "url": "http://y.invalid" }))?;
        pi.register_mcp_server("my-server", json!({ "url": "http://x.invalid" }))?;
        Ok(())
    }));
    let ext2 = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let errs = Arc::clone(&err_c);
        async move {
            for name in ["taken", "my_server"] {
                if let Err(e) = pi.register_mcp_server(name, json!({ "url": "http://z.invalid" })) {
                    errs.lock().unwrap_or_else(PoisonError::into_inner).push(e.message().to_string());
                }
            }
            Ok(())
        }
    }));
    let (harness, _) = setup_registered(vec![ext1, ext2], vec![]).await;
    let recorded = errors.lock().unwrap_or_else(PoisonError::into_inner).clone();
    assert_eq!(recorded.len(), 2);
    assert!(recorded[0].contains("MCP server \"taken\" is already registered by extension"));
    assert!(recorded[1].contains("MCP server \"my_server\" conflicts with registered server \"my-server\""));
    harness.cleanup();
}

#[tokio::test]
async fn reports_registered_servers_when_no_extension_connects_them() {
    let ext = InlineExtension::Factory(Arc::new(|pi: ExtensionApi| async move {
        pi.register_mcp_server("orphan", json!({ "url": "http://orphan.invalid" }))
    }));
    let harness = create_harness(HarnessOptions {
        extension_factories: Some(vec![ext]),
        ..Default::default()
    })
    .await;
    let errors = Arc::new(Mutex::new(Vec::<String>::new()));
    let errs_c = Arc::clone(&errors);
    harness
        .session
        .bind_extensions(ExtensionBindings {
            on_error: Some(Arc::new(move |e: ExtensionError| {
                errs_c.lock().unwrap_or_else(PoisonError::into_inner).push(e.error);
            })),
            ..Default::default()
        })
        .await
        .unwrap();

    let recorded = errors.lock().unwrap_or_else(PoisonError::into_inner).clone();
    assert_eq!(recorded.len(), 1);
    assert!(recorded[0].contains("MCP server \"orphan\" is registered, but no loaded extension"));
    harness.cleanup();
}

async fn setup_resume_reload(
    session_manager: Option<SessionManager>,
    extra_factories: Vec<InlineExtension>,
    initialize_delay_ms: Option<u64>,
) -> (Harness, Arc<Mutex<Vec<String>>>) {
    let connected = Arc::new(Mutex::new(Vec::<String>::new()));
    let conn_c = Arc::clone(&connected);
    let server_entry = McpServerEntry {
        name: "docs".to_string(),
        config: McpServerConfig::Http(McpHttpServerConfig {
            exposure: Some(McpExposure::Deferred),
            description: None,
            tool_exposure: None,
            enabled: None,
            timeout: None,
            r#type: None,
            url: "http://unused.invalid".to_string(),
            headers: None,
            oauth: None,
        }),
        auth_provider: None,
        source: "test".to_string(),
        scope: None,
    };

    let mut factories = extra_factories;
    factories.push(InlineExtension::Factory(create_tool_search_extension()));
    factories.push(InlineExtension::Factory(create_mcp_extension(McpExtensionOptions {
        load_config: Some(Arc::new(move |_| LoadedMcpConfig {
            servers: vec![server_entry.clone()],
            auto_enable_codemode: None,
            errors: vec![],
        })),
        create_transport: Some(Arc::new(move |entry, _url, _auth| {
            conn_c
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(entry.name.clone());
            let (client, server) = create_fake_server(
                Arc::new(Mutex::new(Vec::new())),
                FakeServerOptions {
                    initialize_delay_ms,
                    ..Default::default()
                },
            );
            tokio::spawn(async move {
                let _ = server.start().await;
            });
            Ok(CreatedTransport::Custom(Arc::new(client)))
        })),
        ..Default::default()
    })));

    let resource_loader = Arc::new(DefaultResourceLoader::new(DefaultResourceLoaderOptions {
        extension_factories: Some(factories),
        no_extensions: Some(true),
        no_skills: Some(true),
        no_prompt_templates: Some(true),
        no_themes: Some(true),
        ..Default::default()
    }));
    let harness = create_harness(HarnessOptions {
        resource_loader: Some(resource_loader),
        session_manager,
        ..Default::default()
    })
    .await;
    harness.session.bind_extensions(Default::default()).await.unwrap();
    (harness, connected)
}

async fn load_docs_search(harness: &Harness) {
    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Blocks(vec![faux_tool_call(
                "tool_search",
                json!({ "query": "search the docs", "limit": 1 }),
            )]),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("loaded".to_string()),
            None,
        )),
    ]);
    harness.session.prompt("load", None).await.unwrap();
    assert!(
        harness
            .session
            .get_active_tool_names()
            .contains(&"mcp__docs__search".to_string())
    );
}

#[tokio::test]
async fn declares_tools_tool_search_loaded_again_on_resume_once_their_server_connects() {
    let (first, _) = setup_resume_reload(None, vec![], None).await;
    load_docs_search(&first).await;

    let sm = first.session_manager.clone();
    let (second, _) = setup_resume_reload(Some(sm), vec![], None).await;
    wait_until(
        || {
            second
                .session
                .get_active_tool_names()
                .contains(&"mcp__docs__search".to_string())
        },
        "restored mcp__docs__search",
    )
    .await;
    second.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Blocks(vec![faux_tool_call(
                "mcp__docs__search",
                json!({ "query": "again" }),
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
    second.session.prompt("use it", None).await.unwrap();

    assert_eq!(
        get_message_text(&AgentMessage::from(Message::ToolResult(tool_result(
            &second,
            "mcp__docs__search"
        )))),
        "again guide\nagain faq"
    );
    let removals = second
        .session
        .messages()
        .into_iter()
        .filter(|m| match m {
            AgentMessage::Message(Message::System(sys)) => sys.tools_removed.as_ref().is_some_and(|r| !r.is_empty()),
            _ => false,
        })
        .count();
    assert_eq!(removals, 0);
    second.cleanup();
    first.cleanup();
}

async fn run_restored_tools_with_loadout(loadout: Option<Vec<String>>, kept: bool) {
    let (first, _) = setup_resume_reload(None, vec![], None).await;
    load_docs_search(&first).await;

    let set_loadout = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let lo = loadout.clone();
        async move {
            let pi_ev = pi.clone();
            pi.on::<pi_coding_agent::core::extensions::types::SessionStartEvent>(move |_e, _ctx| {
                let pi_h = pi_ev.clone();
                let lo = lo.clone();
                Box::pin(async move {
                    let next = match lo {
                        Some(list) => list,
                        None => {
                            let mut cur = pi_h.get_active_tools()?;
                            cur.push("read".to_string());
                            cur
                        }
                    };
                    pi_h.set_active_tools(&next)?;
                    Ok(None)
                })
            });
            Ok(())
        }
    }));

    let (second, _) = setup_resume_reload(Some(first.session_manager.clone()), vec![set_loadout], None).await;
    wait_until(
        || {
            second
                .session
                .get_all_tools()
                .into_iter()
                .any(|t| t.name == "mcp__docs__search")
        },
        "mcp__docs__search registered on resume",
    )
    .await;
    assert_eq!(
        second
            .session
            .get_active_tool_names()
            .contains(&"mcp__docs__search".to_string()),
        kept
    );
    second.cleanup();
    first.cleanup();
}

#[tokio::test]
async fn drops_restored_tools_when_an_extension_sets_the_loadout_before_they_register() {
    run_restored_tools_with_loadout(Some(vec!["read".to_string()]), false).await;
}

#[tokio::test]
async fn keeps_restored_tools_when_an_extension_appends_to_the_loadout_before_they_register() {
    run_restored_tools_with_loadout(None, true).await;
}

#[tokio::test]
async fn does_not_activate_restored_tools_that_register_after_the_next_prompt_starts() {
    let (first, _) = setup_resume_reload(None, vec![], None).await;
    load_docs_search(&first).await;

    let (second, _) = setup_resume_reload(Some(first.session_manager.clone()), vec![], Some(200)).await;
    second.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text("done".to_string()),
        None,
    ))]);
    second.session.prompt("go", None).await.unwrap();
    wait_until(
        || {
            second
                .session
                .get_all_tools()
                .into_iter()
                .any(|t| t.name == "mcp__docs__search")
        },
        "delayed mcp__docs__search registered",
    )
    .await;
    assert!(!second
        .session
        .get_active_tool_names()
        .contains(&"mcp__docs__search".to_string()));
    second.cleanup();
    first.cleanup();
}

#[tokio::test]
async fn declares_tools_tool_search_loaded_again_after_reload() {
    let (harness, connected) = setup_resume_reload(None, vec![], None).await;
    load_docs_search(&harness).await;

    harness.session.reload(None).await.unwrap();

    wait_until(
        || *connected.lock().unwrap_or_else(PoisonError::into_inner) == vec!["docs", "docs"],
        "reconnected docs server",
    )
    .await;
    wait_until(
        || {
            harness
                .session
                .get_active_tool_names()
                .contains(&"mcp__docs__search".to_string())
        },
        "mcp__docs__search active after reload",
    )
    .await;
    harness.cleanup();
}
