//! Port of packages/coding-agent/test/suite/regressions/11156-pi-ai-models-virtual-module.test.ts

use pi_coding_agent::core::extensions::load_extensions;
use pi_coding_agent::core::extensions::virtual_modules::resolve_virtual_module;
use tempfile::TempDir;

// Regression test for #11156: extensions importing @earendil-works/pi-ai/models
// (e.g. for clampThinkingLevel) failed with "Cannot find module" in embedded
// runtimes because the subpath was missing from the virtual module map.
#[tokio::test]
async fn exposes_pi_ai_models_entrypoint_with_clamp_thinking_level_under_both_scopes() {
    let modern = resolve_virtual_module("@earendil-works/pi-ai/models").expect("@earendil-works/pi-ai/models missing");
    let legacy = resolve_virtual_module("@mariozechner/pi-ai/models").expect("@mariozechner/pi-ai/models missing");
    assert_eq!(modern, legacy);

    let temp = TempDir::new().expect("tempdir");
    let ext_path = temp.path().join("check-models-virtual.ts");
    std::fs::write(
        &ext_path,
        r#"
import { clampThinkingLevel } from "@earendil-works/pi-ai/models";
import { clampThinkingLevel as legacyClamp } from "@mariozechner/pi-ai/models";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

export default function (pi: ExtensionAPI) {
    if (typeof clampThinkingLevel !== "function" || clampThinkingLevel !== legacyClamp) {
        throw new Error("clampThinkingLevel missing or mismatched");
    }
    const clamped = clampThinkingLevel({ reasoning: true } as any, "xhigh");
    pi.on("session_start", () => {});
    if (clamped !== "high") {
        throw new Error("unexpected clamp result: " + clamped);
    }
}
"#,
    )
    .expect("write extension");

    let cwd = temp.path().to_str().expect("utf8");
    let loaded = load_extensions(&[ext_path.to_str().expect("utf8").to_string()], cwd, None, None).await;
    assert!(loaded.errors.is_empty(), "errors: {:?}", loaded.errors);
    assert_eq!(loaded.extensions.len(), 1);
}
