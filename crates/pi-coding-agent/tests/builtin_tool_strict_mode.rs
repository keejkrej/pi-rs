//! Port of packages/coding-agent/test/builtin-tool-strict-mode.test.ts (unit coding-agent-session-bootstrap)

// @generated-mods begin (scaffold-owned, do not edit)
#[path = "builtin_tool_strict_mode/part_2.rs"]
mod part_2;
// @generated-mods end

use std::sync::Arc;

use pi_ai::compat::get_model;
use pi_coding_agent::core::agent_session::ExtensionBindings;
use pi_coding_agent::core::extensions::types::{
    ConstrainedSamplingOption, ExtensionApi, InlineExtension, SessionStartEvent,
};
use pi_coding_agent::core::resource_loader::{DefaultResourceLoader, DefaultResourceLoaderOptions, ResourceLoader};
use pi_coding_agent::core::sdk::{CreateAgentSessionOptions, create_agent_session};
use pi_coding_agent::core::session_manager::SessionManager;
use pi_coding_agent::core::settings_manager::{Settings, SettingsManager};
use pi_coding_agent::core::tools::{ToolName, create_all_tool_definitions};
use pretty_assertions::assert_eq;
use serde_json::json;

const STRICT_TOOL_NAMES: [ToolName; 5] = [
    ToolName::Read,
    ToolName::Bash,
    ToolName::Powershell,
    ToolName::Edit,
    ToolName::Write,
];

fn s(items: &[&str]) -> Vec<String> {
    items.iter().map(|v| (*v).to_string()).collect()
}

#[tokio::test]
async fn allows_extensions_to_re_register_tools_without_strict_sampling() {
    for active_tools in [
        vec![],
        s(&["read"]),
        s(&["read", "bash", "powershell", "edit", "write"]),
    ] {
        let cwd =
            pi_js::fs::mkdtemp(&pi_js::path::join(&[&pi_js::env::tmp_dir(), "pi-non-strict-tools-"])).unwrap();
        let agent_dir = pi_js::path::join(&[&cwd, "agent"]);
        let settings_manager = SettingsManager::in_memory(
            Some(Settings {
                default_tools: Some(active_tools.clone()),
                ..Default::default()
            }),
            None,
        );
        let cwd_for_ext = cwd.clone();
        let active_for_ext = active_tools.clone();
        let factory = Arc::new(move |pi: ExtensionApi| {
            let cwd_inner = cwd_for_ext.clone();
            let active_inner = active_for_ext.clone();
            async move {
                let cwd_ev = cwd_inner.clone();
                let active_ev = active_inner.clone();
                pi.on::<SessionStartEvent>(move |_ev, _ctx| {
                    let cwd_run = cwd_ev.clone();
                    let active_run = active_ev.clone();
                    let pi_handle = pi.clone();
                    Box::pin(async move {
                        let definitions = create_all_tool_definitions(&cwd_run, None);
                        for name in STRICT_TOOL_NAMES {
                            if let Some(mut def) = definitions.get(&name).cloned() {
                                def.constrained_sampling = Some(ConstrainedSamplingOption::Disabled(false));
                                pi_handle.register_tool(def)?;
                            }
                        }
                        pi_handle.set_active_tools(active_run)?;
                        Ok(None)
                    })
                });
                Ok(())
            }
        });

        let resource_loader = Arc::new(DefaultResourceLoader::new(DefaultResourceLoaderOptions {
            cwd: Some(cwd.clone()),
            agent_dir: Some(agent_dir.clone()),
            settings_manager: Some(settings_manager.clone()),
            no_extensions: Some(true),
            no_skills: Some(true),
            no_prompt_templates: Some(true),
            no_themes: Some(true),
            extension_factories: Some(vec![InlineExtension::Factory(factory)]),
            ..Default::default()
        }));

        let cleanup = |dir: &str| {
            let _ = pi_js::fs::rm(dir, true, true);
        };

        resource_loader.reload(None).await.unwrap();
        let model = get_model("anthropic", "claude-sonnet-4-5").unwrap();
        let created = create_agent_session(Some(CreateAgentSessionOptions {
            cwd: Some(cwd.clone()),
            agent_dir: Some(agent_dir),
            model: Some(model),
            settings_manager: Some(settings_manager),
            session_manager: Some(SessionManager::in_memory(Some(&cwd), None, None).unwrap()),
            resource_loader: Some(resource_loader),
            ..Default::default()
        }))
        .await
        .unwrap();

        let session = created.session;
        let original_prompt = session.system_prompt();
        session.bind_extensions(ExtensionBindings::default()).await.unwrap();

        assert_eq!(session.get_active_tool_names(), active_tools);
        assert_eq!(session.system_prompt(), original_prompt);
        for name in STRICT_TOOL_NAMES {
            let def = session.get_tool_definition(name.as_str()).unwrap();
            assert_eq!(
                def.constrained_sampling,
                Some(ConstrainedSamplingOption::Disabled(false))
            );
        }
        for tool in session.agent().state().tools {
            assert_eq!(tool.constrained_sampling, None);
        }

        if active_tools.iter().any(|name| name == "read") {
            let sample_path = pi_js::path::join(&[&cwd, "sample.txt"]);
            pi_js::fs::write_file(&sample_path, b"still works", None).unwrap();
            let read_tool = session
                .agent()
                .state()
                .tools
                .into_iter()
                .find(|t| t.name == "read")
                .unwrap();
            let result = (read_tool.execute)(
                "read-test".to_string(),
                json!({ "path": "sample.txt" }),
                None,
                None,
            )
            .await
            .unwrap();
            assert_eq!(result.content.len(), 1);
            let text = match &result.content[0] {
                pi_ai::types::UserContentBlock::Text(t) => t.text.as_str(),
                _ => "",
            };
            assert_eq!(text, "still works");
        }

        session.dispose();
        cleanup(&cwd);
    }
}
