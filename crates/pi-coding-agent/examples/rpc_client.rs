//! Port of packages/coding-agent/examples/rpc-client.ts

use std::process::ExitCode;

fn main() -> ExitCode {
    // WIRE(@tui-w3): wire RpcClient from pi_coding_agent::modes::rpc::rpc_client once @tui merges.
    pi_js::console::error("rpc_client: not wired yet (pending Stage 3B-2 @tui merge)");
    ExitCode::from(1)
}
