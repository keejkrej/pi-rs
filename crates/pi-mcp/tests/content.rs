use pi_mcp::{CallToolResult, ContentBlock, LlmContent, to_llm_content};
use pretty_assertions::assert_eq;
use serde_json::{from_value, json};

#[test]
fn passes_text_and_images_through_and_replaces_other_blocks_with_text() {
    let blocks: Vec<ContentBlock> = from_value(json!([
        { "type": "text", "text": "hello", "annotations": { "priority": 1 } },
        { "type": "image", "data": "aW1n", "mimeType": "image/png", "_meta": { "x": 1 } },
        { "type": "audio", "data": "YXVk", "mimeType": "audio/wav" },
        { "type": "resource_link", "uri": "file:///a.txt", "name": "a.txt" },
        { "type": "resource", "resource": { "uri": "file:///b.txt", "text": "inline" } },
        { "type": "resource", "resource": { "uri": "file:///c.png", "mimeType": "image/png", "blob": "Yw==" } },
        { "type": "resource", "resource": { "uri": "file:///d.bin", "blob": "ZA==" } }
    ]))
    .expect("valid content blocks");

    let result = CallToolResult {
        content: blocks,
        structured_content: None,
        is_error: None,
        meta: None,
    };

    let expected: Vec<LlmContent> = from_value(json!([
        { "type": "text", "text": "hello" },
        { "type": "image", "data": "aW1n", "mimeType": "image/png" },
        { "type": "text", "text": "[audio audio/wav omitted]" },
        { "type": "text", "text": "a.txt: file:///a.txt" },
        { "type": "text", "text": "inline" },
        { "type": "image", "data": "Yw==", "mimeType": "image/png" },
        { "type": "text", "text": "[binary resource file:///d.bin (unknown type) omitted]" }
    ]))
    .expect("valid expected llm content");

    assert_eq!(to_llm_content(&result), expected);
}

#[test]
fn falls_back_to_the_structured_content_as_json_when_there_are_no_blocks() {
    let result_empty: CallToolResult = from_value(json!({ "content": [], "structuredContent": { "n": 1 } })).unwrap();
    let expected_json: Vec<LlmContent> = from_value(json!([{ "type": "text", "text": "{\n  \"n\": 1\n}" }])).unwrap();
    assert_eq!(to_llm_content(&result_empty), expected_json);

    let result_with_block: CallToolResult = from_value(json!({
        "content": [{ "type": "text", "text": "n=1" }],
        "structuredContent": { "n": 1 }
    }))
    .unwrap();
    let expected_text: Vec<LlmContent> = from_value(json!([{ "type": "text", "text": "n=1" }])).unwrap();
    assert_eq!(to_llm_content(&result_with_block), expected_text);
}
