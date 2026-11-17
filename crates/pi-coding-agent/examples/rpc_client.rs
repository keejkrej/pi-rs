//! Port of packages/coding-agent/examples/rpc-client.ts

use std::io::Write;
use std::process::ExitCode;
use std::sync::Arc;

use pi_coding_agent::modes::json_event::{JsonAgentSessionEvent, JsonAssistantMessageEvent};
use pi_coding_agent::modes::rpc::rpc_client::{RpcClient, RpcClientOptions};

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let prompt = if args.is_empty() {
        "Explain this repository in one paragraph.".to_string()
    } else {
        args.join(" ")
    };

    let client = RpcClient::new(RpcClientOptions {
        cli_path: pi_js::env::var("PI_CLI_PATH"),
        args: Some(vec!["--no-session".to_string()]),
        ..Default::default()
    });

    let unsub = client.on_event(Arc::new(|event| match event {
        JsonAgentSessionEvent::MessageUpdate {
            assistant_message_event: JsonAssistantMessageEvent::TextDelta { delta, .. },
            ..
        } => {
            let mut out = std::io::stdout().lock();
            let _ = out.write_all(delta.as_bytes());
            let _ = out.flush();
        }
        JsonAgentSessionEvent::ToolExecutionStart { tool_name, .. } => {
            let mut err = std::io::stderr().lock();
            let _ = writeln!(err, "\n[tool: {tool_name}]");
            let _ = err.flush();
        }
        _ => {}
    }));

    let result: pi_js::Result<()> = async {
        client.start().await?;
        client.prompt_and_wait(&prompt, None, None).await?;
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out);
        let _ = out.flush();
        Ok(())
    }
    .await;

    unsub.call();
    client.stop().await;

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            pi_js::console::error(&format!("rpc_client error: {err}"));
            ExitCode::from(1)
        }
    }
}
