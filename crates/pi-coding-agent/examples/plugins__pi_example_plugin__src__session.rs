//! Port of packages/coding-agent/examples/plugins/pi-example-plugin/src/session.ts

use std::process::ExitCode;

fn main() -> ExitCode {
    pi_js::console::error(
        "pi-example-plugin/session: not available in pi-rs: JavaScript facet bundles are not supported by pi-rs",
    );
    ExitCode::from(1)
}
