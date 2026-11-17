//! Port of packages/coding-agent/examples/extensions/doom-overlay/index.ts

// @generated-mods begin (scaffold-owned, do not edit)
pub mod doom_component;
pub mod doom_engine;
pub mod doom_keys;
pub mod wad_finder;
// @generated-mods end

use std::process::ExitCode;

fn main() -> ExitCode {
    let _ = (
        doom_component::MODULE_INFO,
        doom_engine::MODULE_INFO,
        doom_keys::MODULE_INFO,
        wad_finder::MODULE_INFO,
    );
    pi_js::console::error("doom-overlay: not available in pi-rs: depends on Doom WASM/WAD JS runtime assets");
    ExitCode::from(1)
}
