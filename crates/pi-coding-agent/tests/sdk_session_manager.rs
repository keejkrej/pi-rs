//! Port of packages/coding-agent/test/sdk-session-manager.test.ts

use pi_ai::ModelThinkingLevel;
use pi_ai::compat::get_model;
use pi_ai::types::UserContentBlock;
use pi_coding_agent::core::sdk::{CreateAgentSessionOptions, create_agent_session};
use pi_coding_agent::core::session_manager::SessionManager;
use pretty_assertions::assert_eq;
use serde_json::json;

fn setup_dirs() -> (String, String, String) {
    let temp_dir = pi_js::fs::mkdtemp(&pi_js::path::join(&[&pi_js::env::tmp_dir(), "pi-sdk-session-test-"])).unwrap();
    let cwd = pi_js::path::join(&[&temp_dir, "project"]);
    let agent_dir = pi_js::path::join(&[&temp_dir, "agent"]);
    pi_js::fs::mkdir(&cwd, true, None).unwrap();
    pi_js::fs::mkdir(&agent_dir, true, None).unwrap();
    (temp_dir, cwd, agent_dir)
}

#[tokio::test]
async fn uses_agent_dir_for_the_default_persisted_session_path() {
    let (temp_dir, cwd, agent_dir) = setup_dirs();
    let model = get_model("anthropic", "claude-sonnet-4-5").unwrap();

    let created = create_agent_session(Some(CreateAgentSessionOptions {
        cwd: Some(cwd.clone()),
        agent_dir: Some(agent_dir.clone()),
        model: Some(model),
        ..Default::default()
    }))
    .await
    .unwrap();
    let session = created.session;

    let stripped = cwd.trim_start_matches(['/', '\\']).replace(['/', '\\', ':'], "-");
    let safe_path = format!("--{stripped}--");
    let expected_session_dir = pi_js::path::join(&[&agent_dir, "sessions", &safe_path]);
    let session_dir = session.session_manager().get_session_dir();
    let session_file = session.session_manager().get_session_file();

    assert_eq!(session_dir, expected_session_dir);
    assert!(
        session_file
            .as_deref()
            .is_some_and(|p| p.starts_with(&format!("{expected_session_dir}/")))
    );

    session.dispose();
    let _ = pi_js::fs::rm(&temp_dir, true, true);
}

#[tokio::test]
async fn keeps_an_explicit_session_manager_override() {
    let (temp_dir, cwd, agent_dir) = setup_dirs();
    let model = get_model("anthropic", "claude-sonnet-4-5").unwrap();

    let session_manager = SessionManager::in_memory(Some(&cwd), None, None).unwrap();
    let created = create_agent_session(Some(CreateAgentSessionOptions {
        cwd: Some(cwd),
        agent_dir: Some(agent_dir),
        model: Some(model),
        session_manager: Some(session_manager.clone()),
        ..Default::default()
    }))
    .await
    .unwrap();
    let session = created.session;

    assert!(!session.session_manager().is_persisted());
    assert_eq!(
        session.session_manager().get_session_id(),
        session_manager.get_session_id()
    );

    session.dispose();
    let _ = pi_js::fs::rm(&temp_dir, true, true);
}

#[tokio::test]
async fn derives_cwd_from_an_explicit_session_manager_when_cwd_is_omitted() {
    let (temp_dir, _cwd, agent_dir) = setup_dirs();
    let model = get_model("anthropic", "claude-sonnet-4-5").unwrap();

    let session_cwd = pi_js::path::join(&[&temp_dir, "session-project"]);
    pi_js::fs::mkdir(&session_cwd, true, None).unwrap();
    let session_manager = SessionManager::in_memory(Some(&session_cwd), None, None).unwrap();

    let created = create_agent_session(Some(CreateAgentSessionOptions {
        agent_dir: Some(agent_dir),
        model: Some(model),
        session_manager: Some(session_manager.clone()),
        ..Default::default()
    }))
    .await
    .unwrap();
    let session = created.session;

    assert_eq!(
        session.session_manager().get_session_id(),
        session_manager.get_session_id()
    );
    assert!(
        session
            .system_prompt()
            .contains(&format!("<cwd>\n{session_cwd}\n</cwd>"))
    );

    let bash_tool = session
        .agent()
        .state()
        .tools
        .into_iter()
        .find(|t| t.name == "bash")
        .unwrap();
    let result = (bash_tool.execute)("test".to_string(), json!({ "command": "pwd" }), None, None)
        .await
        .unwrap();
    let output: String = result
        .content
        .iter()
        .filter_map(|item| match item {
            UserContentBlock::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect();

    assert_eq!(
        pi_js::fs::realpath(output.trim()).unwrap(),
        pi_js::fs::realpath(&session_cwd).unwrap()
    );

    session.dispose();
    let _ = pi_js::fs::rm(&temp_dir, true, true);
}

#[tokio::test]
async fn exposes_current_session_state_to_the_built_in_bash_tool() {
    let (temp_dir, cwd, agent_dir) = setup_dirs();
    let model = get_model("anthropic", "claude-sonnet-4-5").unwrap();

    let created = create_agent_session(Some(CreateAgentSessionOptions {
        cwd: Some(cwd),
        agent_dir: Some(agent_dir),
        model: Some(model.clone()),
        thinking_level: Some(ModelThinkingLevel::High),
        ..Default::default()
    }))
    .await
    .unwrap();
    let session = created.session;

    assert!(session.session_file().is_some());
    assert!(
        session
            .system_prompt()
            .contains("You can inspect PI_* environment variables for current model and session details.")
    );

    let bash_tool = session
        .agent()
        .state()
        .tools
        .into_iter()
        .find(|t| t.name == "bash")
        .unwrap();
    let result = (bash_tool.execute)(
        "test".to_string(),
        json!({
            "command": r#"printf '%s\n' "$PI_SESSION_ID" "$PI_SESSION_FILE" "$PI_PROVIDER" "$PI_MODEL" "$PI_REASONING_LEVEL""#
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let output: String = result
        .content
        .iter()
        .filter_map(|item| match item {
            UserContentBlock::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect();

    let lines: Vec<&str> = output.trim().split('\n').collect();
    let session_file = session.session_file().unwrap();
    assert_eq!(
        lines,
        vec![
            session.session_id().as_str(),
            session_file.as_str(),
            model.provider.as_str(),
            model.id.as_str(),
            session.thinking_level().as_str(),
        ]
    );

    session.dispose();
    let _ = pi_js::fs::rm(&temp_dir, true, true);
}
