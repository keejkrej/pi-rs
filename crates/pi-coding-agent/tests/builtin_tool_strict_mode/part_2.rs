//! Port of packages/coding-agent/test/builtin-tool-strict-mode.test.ts

use pi_ai::types::{ConstrainedSamplingConfig, JsonSchemaStrict};
use pi_coding_agent::core::extensions::types::ConstrainedSamplingOption;
use pi_coding_agent::core::tools::{ToolName, create_all_tool_definitions, create_all_tools, wrap_tool_definition};
use pi_js::env::testing::EnvGuard;
use serde_json::json;
use serial_test::serial;

const STRICT_TOOLS: [ToolName; 5] = [
    ToolName::Read,
    ToolName::Bash,
    ToolName::Powershell,
    ToolName::Edit,
    ToolName::Write,
];

const NON_STRICT_TOOLS: [ToolName; 3] = [ToolName::Grep, ToolName::Find, ToolName::Ls];

#[test]
#[serial]
fn prefers_strict_sampling_regardless_of_pi_experimental() {
    for experimental in [None, Some("0"), Some("1")] {
        let _guard = EnvGuard::set("PI_EXPERIMENTAL", experimental);

        let cwd = pi_js::env::cwd();
        let definitions = create_all_tool_definitions(&cwd, None);
        let tools = create_all_tools(&cwd, None);

        for name in STRICT_TOOLS {
            let def = &definitions[&name];
            assert_eq!(
                def.constrained_sampling,
                Some(ConstrainedSamplingOption::Config(
                    ConstrainedSamplingConfig::JsonSchema {
                        strict: JsonSchemaStrict::Prefer,
                    }
                ))
            );
            assert_eq!(
                tools[&name].constrained_sampling,
                Some(ConstrainedSamplingConfig::JsonSchema {
                    strict: JsonSchemaStrict::Prefer,
                })
            );
        }

        for name in NON_STRICT_TOOLS {
            assert!(definitions[&name].constrained_sampling.is_none());
            assert!(tools[&name].constrained_sampling.is_none());
        }

        assert_eq!(
            definitions[&ToolName::Read].parameters.to_value()["required"],
            json!(["path"])
        );
        assert_eq!(
            definitions[&ToolName::Bash].parameters.to_value()["required"],
            json!(["command"])
        );
    }
}

#[test]
fn preserves_explicit_opt_outs_when_wrapping_definitions() {
    let cwd = pi_js::env::cwd();
    let definitions = create_all_tool_definitions(&cwd, None);

    for name in STRICT_TOOLS {
        let definition = &definitions[&name];
        let mut overridden = definition.clone();
        overridden.constrained_sampling = Some(ConstrainedSamplingOption::Disabled(false));
        let wrapped = wrap_tool_definition(overridden, None);
        assert!(wrapped.constrained_sampling.is_none());
        assert_eq!(
            definition.constrained_sampling,
            Some(ConstrainedSamplingOption::Config(
                ConstrainedSamplingConfig::JsonSchema {
                    strict: JsonSchemaStrict::Prefer,
                }
            ))
        );
    }
}

// WIRE(@mcp-w3 / @ca-cli): Full AgentSession + SettingsManager + DefaultResourceLoader strict opt-out integration tests enable once session/settings/resource-loader land.
#[cfg(any())]
mod pending_session_integration {}
