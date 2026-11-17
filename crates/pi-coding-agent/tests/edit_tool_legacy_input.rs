//! Port of packages/coding-agent/test/edit-tool-legacy-input.test.ts

use pi_ai::types::UserContentBlock;
use pi_coding_agent::core::tools::edit::{create_edit_tool, create_edit_tool_definition};
use serde_json::json;
use tempfile::TempDir;

#[test]
fn keeps_legacy_fields_out_of_public_schema() {
    let def = create_edit_tool_definition(&pi_js::env::cwd(), None);
    let schema = def.parameters.to_value();
    let props = schema["properties"].as_object().unwrap();
    assert!(!props.contains_key("oldText"));
    assert!(!props.contains_key("newText"));
}

#[test]
fn folds_top_level_old_text_new_text_into_edits() {
    let def = create_edit_tool_definition(&pi_js::env::cwd(), None);
    let prepare = def.prepare_arguments.as_ref().unwrap();
    let prepared = prepare(json!({
        "path": "file.txt",
        "oldText": "before",
        "newText": "after",
    }));
    assert_eq!(
        prepared,
        json!({
            "path": "file.txt",
            "edits": [{ "oldText": "before", "newText": "after" }],
        })
    );
}

#[test]
fn appends_legacy_replacement_to_existing_edits() {
    let def = create_edit_tool_definition(&pi_js::env::cwd(), None);
    let prepare = def.prepare_arguments.as_ref().unwrap();
    let prepared = prepare(json!({
        "path": "file.txt",
        "edits": [{ "oldText": "a", "newText": "b" }],
        "oldText": "c",
        "newText": "d",
    }));
    assert_eq!(
        prepared,
        json!({
            "path": "file.txt",
            "edits": [
                { "oldText": "a", "newText": "b" },
                { "oldText": "c", "newText": "d" },
            ],
        })
    );
}

#[test]
fn passes_through_valid_and_non_object_input_unchanged() {
    let def = create_edit_tool_definition(&pi_js::env::cwd(), None);
    let prepare = def.prepare_arguments.as_ref().unwrap();
    let valid = json!({
        "path": "file.txt",
        "edits": [{ "oldText": "a", "newText": "b" }],
    });
    assert_eq!(prepare(valid.clone()), valid);
    assert_eq!(prepare(json!(null)), json!(null));
    assert_eq!(prepare(json!("garbage")), json!("garbage"));
}

#[tokio::test]
async fn prepared_args_execute_correctly() {
    let dir = TempDir::new().unwrap();
    let file_path = dir.path().join("legacy.txt");
    std::fs::write(&file_path, "before\n").unwrap();

    let cwd = dir.path().to_string_lossy().into_owned();
    let def = create_edit_tool_definition(&cwd, None);
    let prepare = def.prepare_arguments.as_ref().unwrap();
    let prepared = prepare(json!({
        "path": "legacy.txt",
        "oldText": "before",
        "newText": "after",
    }));

    let tool = create_edit_tool(&cwd, None);
    let res = (tool.execute)("tool-1".into(), prepared, None, None).await.unwrap();
    assert_eq!(res.content.len(), 1);
    match &res.content[0] {
        UserContentBlock::Text(t) => {
            assert_eq!(t.text, "Successfully replaced 1 block(s) in legacy.txt.");
        }
        other => panic!("unexpected content: {other:?}"),
    }
    assert_eq!(std::fs::read_to_string(&file_path).unwrap(), "after\n");
}

#[test]
fn parses_edits_from_json_string() {
    let def = create_edit_tool_definition(&pi_js::env::cwd(), None);
    let prepare = def.prepare_arguments.as_ref().unwrap();
    let edits_json = pi_js::json::stringify(&json!([{ "oldText": "a", "newText": "b" }]));
    let prepared = prepare(json!({
        "path": "file.txt",
        "edits": edits_json,
    }));
    assert_eq!(
        prepared,
        json!({
            "path": "file.txt",
            "edits": [{ "oldText": "a", "newText": "b" }],
        })
    );
}

#[test]
fn leaves_edits_alone_when_string_is_invalid_json() {
    let def = create_edit_tool_definition(&pi_js::env::cwd(), None);
    let prepare = def.prepare_arguments.as_ref().unwrap();
    let prepared = prepare(json!({
        "path": "file.txt",
        "edits": "not json",
    }));
    assert_eq!(
        prepared,
        json!({
            "path": "file.txt",
            "edits": "not json",
        })
    );
}
