//! Port of packages/coding-agent/examples/extensions/with-deps/index.ts

use std::process::ExitCode;

fn main() -> ExitCode {
    pi_js::console::error("with-deps: not available in pi-rs: depends on npm package ms");
    ExitCode::from(1)
}
