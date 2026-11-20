use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use pi_mcp::{LlmContent, MCP_OUTPUT_MAX_BYTES, McpOutputData, McpOutputSaver, limit_mcp_content};
use pretty_assertions::assert_eq;

fn image() -> LlmContent {
    LlmContent::Image {
        data: "AAAA".to_string(),
        mime_type: "image/png".to_string(),
    }
}

fn text_of(content: &[LlmContent]) -> &str {
    match content.first() {
        Some(LlmContent::Text { text }) => text.as_str(),
        _ => panic!("expected a text block first"),
    }
}

#[tokio::test]
async fn returns_content_within_limit_without_saving() {
    let content = vec![
        LlmContent::Text {
            text: "x".repeat(MCP_OUTPUT_MAX_BYTES),
        },
        image(),
    ];
    let saves = Arc::new(AtomicUsize::new(0));
    let saves_cb = saves.clone();
    let saver: McpOutputSaver = Arc::new(move |_, _| {
        saves_cb.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok("/tmp/full.txt".to_string()) })
    });
    let limited = limit_mcp_content(content.clone(), Some(saver)).await;
    assert_eq!(limited.content, content);
    assert_eq!(limited.full_output_path, None);
    assert_eq!(saves.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn cuts_the_middle_of_longer_text_saves_full_text_and_keeps_images() {
    let full = (1..=3000).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
    let saved = Arc::new(Mutex::new(Vec::<(McpOutputData, String)>::new()));
    let saved_cb = saved.clone();
    let split = full[100..].find('\n').map(|i| 100 + i).unwrap();
    let input = vec![
        LlmContent::Text {
            text: full[..split].to_string(),
        },
        image(),
        LlmContent::Text {
            text: full[split + 1..].to_string(),
        },
    ];
    let saver: McpOutputSaver = Arc::new(move |data, ext| {
        let saved = saved_cb.clone();
        Box::pin(async move {
            saved.lock().unwrap().push((data, ext));
            Ok("/tmp/full.txt".to_string())
        })
    });
    let limited = limit_mcp_content(input, Some(saver)).await;
    assert_eq!(
        *saved.lock().unwrap(),
        vec![(McpOutputData::Text(full.clone()), ".txt".to_string())]
    );
    assert_eq!(limited.full_output_path.as_deref(), Some("/tmp/full.txt"));
    assert_eq!(limited.content.len(), 2);
    assert_eq!(limited.content[1], image());
    let text = text_of(&limited.content);
    let expected_prefix = format!(
        "Warning: truncated output (original token count: {})\nTotal output lines: 3000\n\nline 1\nline 2\n",
        full.len().div_ceil(4)
    );
    assert!(text.starts_with(&expected_prefix), "text: {text}");
    assert!(text.contains(" chars truncated…"), "text: {text}");
    assert!(
        text.ends_with("line 3000\n\n[Full output: /tmp/full.txt (read it with offset/limit)]"),
        "text: {text}"
    );
    assert!(text.len() < MCP_OUTPUT_MAX_BYTES + 1024);
}

#[tokio::test]
async fn reports_failed_save_and_names_no_file_without_a_saver() {
    let content = vec![LlmContent::Text {
        text: "y".repeat(MCP_OUTPUT_MAX_BYTES + 1),
    }];
    let failing_saver: McpOutputSaver = Arc::new(|_, _| Box::pin(async { Err(pi_js::Error::msg("disk full")) }));
    let failed = limit_mcp_content(content.clone(), Some(failing_saver)).await;
    assert_eq!(failed.full_output_path, None);
    assert!(
        text_of(&failed.content).ends_with("\n\n[Could not save the full output: disk full]"),
        "failed text: {}",
        text_of(&failed.content)
    );

    let unsaved = limit_mcp_content(content, None).await;
    assert_eq!(unsaved.full_output_path, None);
    let txt = text_of(&unsaved.content);
    assert!(txt.starts_with("Warning: truncated output "));
    assert!(txt.contains(" chars truncated…"));
    assert!(txt.ends_with('y'));
}

#[tokio::test]
async fn cuts_multibyte_text_only_at_character_boundaries() {
    let text = format!("a{}end", "é".repeat(20_000));
    let limited = limit_mcp_content(vec![LlmContent::Text { text: text.clone() }], None).await;
    let body = text_of(&limited.content);
    assert!(!body.contains('\u{FFFD}'));
    assert!(body.ends_with("end"));

    let marker_start = body.find('…').expect("marker start");
    let marker_rel_end = body[marker_start + '…'.len_utf8()..].find('…').expect("marker end");
    let marker_end = marker_start + '…'.len_utf8() + marker_rel_end + '…'.len_utf8();
    let marker = &body[marker_start..marker_end];
    let removed_chars: usize = marker
        .trim_matches('…')
        .strip_suffix(" chars truncated")
        .unwrap()
        .parse()
        .unwrap();

    let body_after_header = &body[body.find("\n\n").unwrap() + 2..];
    let parts: Vec<&str> = body_after_header.split(marker).collect();
    assert_eq!(parts.len(), 2);
    assert!(parts[0].len() <= MCP_OUTPUT_MAX_BYTES / 2);
    assert!(parts[1].len() <= MCP_OUTPUT_MAX_BYTES / 2);
    assert_eq!(
        parts[0].chars().count() + parts[1].chars().count() + removed_chars,
        text.chars().count()
    );
}
