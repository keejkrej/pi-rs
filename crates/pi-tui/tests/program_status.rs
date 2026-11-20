//! Port of packages/tui/test/program-status.test.ts

use pi_tui::program_status::{
    ProgramStatus, ProgramStatusKind, ProgramStatusState, format_program_status, is_program_status_reply,
};

fn decode_message(sequence: &str) -> Option<String> {
    let (_, rest) = sequence.split_once(":msg=")?;
    let encoded = rest.strip_suffix("\x1b\\").unwrap_or(rest);
    String::from_utf8(pi_js::b64::decode(encoded)).ok()
}

// #10607
#[test]
fn encodes_state_app_kind_and_base64_message() {
    let encoded = pi_js::b64::encode(b"Allow bash?");
    assert_eq!(
        format_program_status(&ProgramStatus {
            state: ProgramStatusState::Blocked,
            app: Some("pi".to_string()),
            kind: Some(ProgramStatusKind::Permission),
            message: Some("Allow bash?".to_string()),
        }),
        format!("\x1b]7501;state=blocked:app=pi:kind=permission:msg={encoded}\x1b\\")
    );
    assert_eq!(
        format_program_status(&ProgramStatus {
            state: ProgramStatusState::Clear,
            ..Default::default()
        }),
        "\x1b]7501;state=clear\x1b\\"
    );
}

#[test]
fn omits_kind_outside_blocked_invalid_app_names_and_empty_messages() {
    assert_eq!(
        format_program_status(&ProgramStatus {
            state: ProgramStatusState::Working,
            app: Some("my app".to_string()),
            kind: Some(ProgramStatusKind::Auth),
            message: Some(" \n ".to_string()),
        }),
        "\x1b]7501;state=working\x1b\\"
    );
    let max_app = "a".repeat(32);
    assert!(
        format_program_status(&ProgramStatus {
            state: ProgramStatusState::Idle,
            app: Some(max_app.clone()),
            ..Default::default()
        })
        .contains(&format!(":app={max_app}\x1b"))
    );
    assert!(
        !format_program_status(&ProgramStatus {
            state: ProgramStatusState::Idle,
            app: Some("a".repeat(33)),
            ..Default::default()
        })
        .contains("app=")
    );
}

#[test]
fn replaces_control_characters_in_program_status_message() {
    let sequence = format_program_status(&ProgramStatus {
        state: ProgramStatusState::Error,
        message: Some("first\nsecond\x1b[31m\u{009b}third\t".to_string()),
        ..Default::default()
    });
    assert_eq!(decode_message(&sequence).as_deref(), Some("first second [31m third"));
}

#[test]
fn cuts_long_messages_at_a_utf8_boundary_within_spec_limits() {
    let message = "é".repeat(1500);
    let sequence = format_program_status(&ProgramStatus {
        state: ProgramStatusState::Working,
        message: Some(message),
        ..Default::default()
    });
    let (_, encoded) = sequence.split_once(":msg=").expect("msg param");
    let encoded = encoded.strip_suffix("\x1b\\").expect("ST terminator");
    assert!(encoded.len() <= 2732);
    let decoded = String::from_utf8(pi_js::b64::decode(encoded)).expect("valid UTF-8");
    assert_eq!(decoded.len(), 2048);
    assert!(decoded.len() <= 2048);
}

#[test]
fn recognizes_program_status_query_replies() {
    assert!(is_program_status_reply("\x1b]7501;?\x1b\\"));
    assert!(is_program_status_reply("\x1b]7501;?;extra=1\x07"));
    assert!(!is_program_status_reply("\x1b]7501;state=working\x1b\\"));
    assert!(!is_program_status_reply("\x1b]10;rgb:ffff/ffff/ffff\x07"));
}
