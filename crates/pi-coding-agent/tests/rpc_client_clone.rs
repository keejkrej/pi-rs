//! Port of packages/coding-agent/test/rpc-client-clone.test.ts

use pi_coding_agent::modes::rpc::rpc_client::{RpcClient, RpcClientOptions, testing};
use pi_coding_agent::modes::rpc::rpc_types::{CancelledResult, RpcCommand, RpcResponse};
use pi_js::testing::Recorder;
use pretty_assertions::assert_eq as pretty_assert_eq;
use serde_json::json;

mod rpc_client_clone {
    use super::*;

    #[tokio::test]
    async fn sends_the_clone_rpc_command() {
        let client = RpcClient::new(RpcClientOptions::default());
        let sent_commands = Recorder::<RpcCommand>::new();
        let sent_ref = sent_commands.clone();

        let _guard = testing::override_send(move |command| {
            sent_ref.push(command);
            Box::pin(async { RpcResponse::success(None, "clone", &CancelledResult { cancelled: false }) })
        });

        let result = client.clone().await.expect("clone should succeed");

        let recorded = sent_commands.take();
        pretty_assert_eq!(recorded, vec![RpcCommand::Clone { id: None }]);
        pretty_assert_eq!(
            pi_js::json::parse(&pi_js::json::stringify(&recorded[0])).expect("valid json"),
            json!({ "type": "clone" })
        );
        pretty_assert_eq!(result, CancelledResult { cancelled: false });
    }
}
