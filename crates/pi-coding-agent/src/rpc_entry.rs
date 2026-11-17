//! Port of packages/coding-agent/src/rpc-entry.ts

use std::process::ExitCode;

use crate::config::APP_NAME;
use crate::core::http_dispatcher::configure_http_dispatcher;
use crate::main_::main;

/// Entry point for RPC subprocess mode (`--mode rpc` prepended to argv).
pub fn run() -> ExitCode {
    let _ = &*APP_NAME;
    pi_js::env::set_var("PI_CODING_AGENT", "true");
    pi_js::env::set_var("AI_AGENT", "pi");
    pi_js::ensure_crypto_provider();

    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(err) => {
            pi_js::console::error(&format!("Failed to initialize async runtime: {err}"));
            return ExitCode::from(1);
        }
    };

    let _ = configure_http_dispatcher(None);
    let mut args = vec!["--mode".to_string(), "rpc".to_string()];
    args.extend(std::env::args().skip(1));

    match rt.block_on(main(&args, None)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(pi_js::Error::Exit(code)) => ExitCode::from(code.clamp(0, 255) as u8),
        Err(err) => {
            pi_js::console::error(&format!("Error: {err}"));
            ExitCode::from(1)
        }
    }
}
