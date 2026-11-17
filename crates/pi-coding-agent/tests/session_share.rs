//! Port of `packages/coding-agent/test/session-share.test.ts`.
// WIRE(@tui-w3): interactive `shareSession` gist-upload orchestration awaits `modes::interactive::session_share`.

use indexmap::IndexSet;
use pi_agent_core::{AgentMessage, AgentState, ThinkingLevel};
use pi_ai::compat::get_model;
use pi_ai::{Message, UserMessage, UserMessageContent};
use pi_coding_agent::core::export_html::{ExportOptions, export_session_to_html};
use pi_coding_agent::core::session_manager::{NewSessionOptions, SessionManager};
use tempfile::TempDir;

fn make_session(dir: &std::path::Path, id: &str, text: &str) -> SessionManager {
    let cwd = dir.join(format!("cwd-{id}"));
    let sess_dir = dir.join(format!("sessions-{id}"));
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::create_dir_all(&sess_dir).unwrap();
    let sm = SessionManager::create(
        cwd.to_str().unwrap(),
        Some(sess_dir.to_str().unwrap()),
        Some(NewSessionOptions {
            id: Some(id.to_owned()),
            parent_session: None,
        }),
    )
    .unwrap();
    sm.append_message(AgentMessage::Message(Message::User(UserMessage {
        content: UserMessageContent::Text(text.to_owned()),
        timestamp: 1,
    })))
    .unwrap();
    sm
}

#[tokio::test]
async fn keeps_concurrent_session_exports_isolated() {
    let tmp = TempDir::new().unwrap();
    let sm_a = make_session(tmp.path(), "session-A", "marker-payload-A");
    let sm_b = make_session(tmp.path(), "session-B", "marker-payload-B");

    let out_a = tmp.path().join("share-A.html");
    let out_b = tmp.path().join("share-B.html");
    let model = get_model("anthropic", "claude-opus-4-6").unwrap();
    let state_a = AgentState {
        system_prompt: "prompt-A".to_owned(),
        model: model.clone(),
        thinking_level: ThinkingLevel::Off,
        tools: Vec::new(),
        messages: Vec::new(),
        is_streaming: false,
        stream_message: None,
        pending_tool_calls: IndexSet::new(),
        error: None,
    };
    let state_b = AgentState {
        system_prompt: "prompt-B".to_owned(),
        model,
        thinking_level: ThinkingLevel::Off,
        tools: Vec::new(),
        messages: Vec::new(),
        is_streaming: false,
        stream_message: None,
        pending_tool_calls: IndexSet::new(),
        error: None,
    };

    let (res_a, res_b) = tokio::join!(
        export_session_to_html(
            &sm_a,
            Some(&state_a),
            Some(ExportOptions {
                output_path: Some(out_a.to_string_lossy().into_owned()),
                theme_name: None,
                tool_renderer: None,
                theme_override: None,
            }),
        ),
        export_session_to_html(
            &sm_b,
            Some(&state_b),
            Some(ExportOptions {
                output_path: Some(out_b.to_string_lossy().into_owned()),
                theme_name: None,
                tool_renderer: None,
                theme_override: None,
            }),
        ),
    );
    res_a.unwrap();
    res_b.unwrap();

    let html_a = std::fs::read_to_string(&out_a).unwrap();
    let html_b = std::fs::read_to_string(&out_b).unwrap();
    let decode_data = |html: &str| -> String {
        let marker = "<script id=\"session-data\" type=\"application/json\">";
        let start = html.find(marker).unwrap() + marker.len();
        let end = html[start..].find("</script>").unwrap() + start;
        String::from_utf8(pi_js::b64::decode(&html[start..end])).unwrap()
    };
    let data_a = decode_data(&html_a);
    let data_b = decode_data(&html_b);
    assert!(data_a.contains("marker-payload-A"));
    assert!(!data_a.contains("marker-payload-B"));
    assert!(data_b.contains("marker-payload-B"));
    assert!(!data_b.contains("marker-payload-A"));
}
