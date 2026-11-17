//! Port of packages/coding-agent/src/experimental/cli.ts

use crate::cli::setup::setup_cli;
use crate::experimental::commands::run_experimental_command;
use crate::main_::main;

/// Development CLI entrypoint with experimental `server` and `client` dispatch.
pub async fn run_experimental_cli(args: &[String]) -> i32 {
    setup_cli();
    if let Some(exit_code) = run_experimental_command(args).await {
        return exit_code;
    }
    match main(args, None).await {
        Ok(()) => 0,
        Err(pi_js::Error::Exit(code)) => code,
        Err(err) => {
            pi_js::console::error(&format!("Error: {}", err.message()));
            1
        }
    }
}
