//! Port of packages/coding-agent/examples/extensions/custom-provider-anthropic/index.ts

use std::process::ExitCode;

fn main() -> ExitCode {
    pi_js::console::error(
        "custom-provider-anthropic: not available in pi-rs: depends on npm package @anthropic-ai/sdk",
    );
    ExitCode::from(1)
}
