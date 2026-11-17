//! Binary entry point for `pi-gui`.

use std::process::ExitCode;

fn main() -> ExitCode {
    if let Err(err) = pi_gui::run() {
        pi_js::console::error(&format!("pi-gui error: {err}"));
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
