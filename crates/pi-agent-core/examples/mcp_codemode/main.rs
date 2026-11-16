//! Port of packages/agent/examples/mcp-codemode/main.ts

// @generated-mods begin (scaffold-owned, do not edit)
pub mod tools;
// @generated-mods end

use std::sync::Arc;

use pi_agent_core::{Agent, AgentEvent, AgentInitialState, AgentOptions, StreamFn};
use pi_ai::{AssistantMessageEvent, ModelsSimpleStreamOptions, create_models};
use pi_js::{BoxFuture, Error, Result, console};
use pi_mcp::{McpClient, McpClientOptions, McpTransport, StdioTransport, StdioTransportOptions};

use crate::tools::{create_codemode_tool, create_mcp_tools, create_nested_tool_runner};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    pi_js::ensure_crypto_provider();
    if let Err(err) = run().await {
        console::error(&err.to_js_string());
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let mut cli_args = std::env::args().skip(1);
    let Some(command) = cli_args.next() else {
        console::error("Usage: mcp_codemode <mcp server command> [args...]");
        std::process::exit(1);
    };
    let args: Vec<String> = cli_args.collect();

    let models = create_models(None);
    models.set_provider(pi_ai::providers::anthropic::anthropic_provider()?);
    let model = models
        .get_model("anthropic", "claude-sonnet-4-6")
        .ok_or_else(|| Error::msg("Model not found"))?;

    let client = McpClient::new(McpClientOptions {
        name: "pi-agent-example".to_string(),
        version: "1.0.0".to_string(),
        title: None,
        capabilities: None,
        protocol_version: None,
        request_timeout_ms: None,
        roots: None,
    });
    let transport: Arc<dyn McpTransport> = Arc::new(StdioTransport::new(StdioTransportOptions {
        command,
        args: Some(args),
        cwd: None,
        env: None,
        inherit_env: None,
        stderr: None,
        on_stderr: None,
        max_message_bytes: None,
        max_stderr_bytes: None,
        close_timeout_ms: None,
    }));
    client.connect(transport).await?;

    let run_result: Result<()> = async {
        let models_for_stream = models.clone();
        let stream_fn: StreamFn = Arc::new(move |m, ctx, opts| {
            let models = models_for_stream.clone();
            Box::pin(async move {
                let wrapped = opts.map(|base| ModelsSimpleStreamOptions {
                    base,
                    transform_headers: None,
                });
                Ok(models.stream_simple(&m, &ctx, wrapped))
            }) as BoxFuture<_>
        });

        let agent = Agent::new(AgentOptions {
            initial_state: Some(AgentInitialState {
                system_prompt: Some("You are a helpful assistant.".to_string()),
                model: Some(model),
                ..Default::default()
            }),
            stream_fn: Some(stream_fn),
            ..Default::default()
        });

        let mcp_tools = create_mcp_tools(client.clone(), "mcp").await?;
        let mut agent_tools = mcp_tools.clone();
        agent_tools.push(create_codemode_tool(
            &mcp_tools,
            create_nested_tool_runner(agent.clone()),
        ));
        agent.set_tools(agent_tools);

        let _unsub = agent.subscribe(|event| match event {
            AgentEvent::MessageUpdate {
                assistant_message_event: AssistantMessageEvent::TextDelta { delta, .. },
                ..
            } => {
                console::stdout_write(delta);
            }
            AgentEvent::ToolExecutionStart { tool_name, .. } => {
                console::log(&format!("\n[{tool_name}]"));
            }
            _ => {}
        });

        agent
            .prompt("Use codemode to list what the MCP tools can do, then summarize it in two sentences.")
            .await?;
        console::log("");
        Ok(())
    }
    .await;

    let close_result = client.close().await;
    run_result?;
    close_result?;
    Ok(())
}
