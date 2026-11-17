//! Port of packages/coding-agent/examples/plugins/pi-example-plugin/src/contract.ts

use std::process::ExitCode;

fn main() -> ExitCode {
    pi_js::console::error(
        "pi-example-plugin/contract: not available in pi-rs: JavaScript facet bundles are not supported by pi-rs",
    );
    ExitCode::from(1)
}
