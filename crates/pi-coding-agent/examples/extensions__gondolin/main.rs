//! Port of packages/coding-agent/examples/extensions/gondolin/index.ts

use std::process::ExitCode;

fn main() -> ExitCode {
    pi_js::console::error("gondolin: not available in pi-rs: depends on npm package @earendil-works/gondolin");
    ExitCode::from(1)
}
