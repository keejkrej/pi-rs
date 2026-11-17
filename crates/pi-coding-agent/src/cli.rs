//! Port of packages/coding-agent/src/cli.ts

// @generated-mods begin (scaffold-owned, do not edit)
pub mod args;
pub mod auth_check;
pub mod auth_command;
pub mod config_selector;
pub mod credential_print;
pub mod experimental;
pub mod file_processor;
pub mod initial_message;
pub mod list_models;
pub mod project_trust;
pub mod session_picker;
pub mod setup;
pub mod startup_ui;
// @generated-mods end

use std::process::ExitCode;

use crate::core::extensions::types::InlineExtension;
use crate::main_::{MainOptions, main};

/// Run the coding agent CLI with default extensions.
pub fn run() -> ExitCode {
    run_with(Vec::new())
}

/// Run the coding agent CLI with custom extension factories appended to built-ins.
pub fn run_with(extension_factories: Vec<InlineExtension>) -> ExitCode {
    setup::setup_cli();
    pi_js::ensure_crypto_provider();
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(err) => {
            pi_js::console::error(&format!("Failed to initialize async runtime: {err}"));
            return ExitCode::from(1);
        }
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let opts = (!extension_factories.is_empty()).then_some(MainOptions {
        extension_factories: Some(extension_factories),
    });
    match rt.block_on(main(&args, opts)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(pi_js::Error::Exit(code)) => {
            let clamped = code.clamp(0, 255) as u8;
            ExitCode::from(clamped)
        }
        Err(err) => {
            pi_js::console::error(&format!("Error: {err}"));
            ExitCode::from(1)
        }
    }
}
