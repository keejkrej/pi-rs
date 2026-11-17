//! Port of packages/coding-agent/examples/extensions/sandbox/index.ts

use std::process::ExitCode;

fn main() -> ExitCode {
    pi_js::console::error("sandbox: not available in pi-rs: depends on npm package @anthropic-ai/sandbox-runtime");
    ExitCode::from(1)
}
