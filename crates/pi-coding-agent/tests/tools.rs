//! Port of packages/coding-agent/test/tools.test.ts#L1-484, #L976-993, #L1100-1419 (unit coding-agent-tools-file-edit)

// @generated-mods begin (scaffold-owned, do not edit)
#[path = "tools/part_2.rs"]
mod part_2;
// @generated-mods end

use std::sync::Arc;

use pi_agent_core::types::AgentToolResult;
use pi_ai::types::UserContentBlock;
use pi_coding_agent::core::tools::edit::{
    EditDiffPreview, EditOperations, EditReplacement, EditToolOptions, compute_edits_diff, create_edit_tool,
};
use pi_coding_agent::core::tools::edit_diff::{
    apply_edits_to_normalized_content, apply_replacements_to_original, normalize_to_lf,
};
use pi_coding_agent::core::tools::ls::{LsToolDetails, create_ls_tool};
use pi_coding_agent::core::tools::read::{ReadToolDetails, create_read_tool};
use pi_coding_agent::core::tools::write::create_write_tool;
use pi_js::BoxFuture;
use pi_js::vendor::jsdiff::patch::apply::apply_patch;
use serde_json::{Value, json};
use tempfile::TempDir;

const TINY_PNG_BASE64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFBQIAX8jx0gAAAABJRU5ErkJggg==";

fn get_text_output(result: &AgentToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|c| match c {
            UserContentBlock::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn create_tiny_bmp_1x1_red_24bpp() -> Vec<u8> {
    let mut buffer = vec![0u8; 58];
    let total_len = (buffer.len() as u32).to_le_bytes();
    buffer[0..2].copy_from_slice(b"BM");
    buffer[2..6].copy_from_slice(&total_len);
    buffer[10..14].copy_from_slice(&54u32.to_le_bytes());
    buffer[14..18].copy_from_slice(&40u32.to_le_bytes());
    buffer[18..22].copy_from_slice(&1i32.to_le_bytes());
    buffer[22..26].copy_from_slice(&1i32.to_le_bytes());
    buffer[26..28].copy_from_slice(&1u16.to_le_bytes());
    buffer[28..30].copy_from_slice(&24u16.to_le_bytes());
    buffer[34..38].copy_from_slice(&4u32.to_le_bytes());
    buffer[56] = 0xff;
    buffer
}

// =========================================================================
// Read tool
// =========================================================================

#[tokio::test]
async fn read_tool_reads_contents_within_limits() {
    let dir = TempDir::new().unwrap();
    let test_file = dir.path().join("test.txt");
    let content = "Hello, world!\nLine 2\nLine 3";
    std::fs::write(&test_file, content).unwrap();

    let tool = create_read_tool(&dir.path().to_string_lossy(), None);
    let result = (tool.execute)(
        "test-call-1".into(),
        json!({ "path": test_file.to_string_lossy() }),
        None,
        None,
    )
    .await
    .unwrap();

    assert_eq!(get_text_output(&result), content);
    assert!(!get_text_output(&result).contains("Use offset="));
    assert!(result.details.is_none());
    assert_eq!(result.structured_content, Some(Value::String(content.to_string())));
}

#[tokio::test]
async fn read_tool_handles_non_existent_files() {
    let dir = TempDir::new().unwrap();
    let missing = dir.path().join("nonexistent.txt");
    let tool = create_read_tool(&dir.path().to_string_lossy(), None);

    let err = (tool.execute)(
        "test-call-2".into(),
        json!({ "path": missing.to_string_lossy() }),
        None,
        None,
    )
    .await
    .unwrap_err();
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("enoent") || msg.contains("not found") || msg.contains("no such file"),
        "{err}"
    );
}

#[tokio::test]
async fn read_tool_truncates_files_exceeding_line_limit() {
    let dir = TempDir::new().unwrap();
    let test_file = dir.path().join("large.txt");
    let lines: Vec<String> = (1..=2500).map(|i| format!("Line {i}")).collect();
    std::fs::write(&test_file, lines.join("\n")).unwrap();

    let tool = create_read_tool(&dir.path().to_string_lossy(), None);
    let result = (tool.execute)(
        "test-call-3".into(),
        json!({ "path": test_file.to_string_lossy() }),
        None,
        None,
    )
    .await
    .unwrap();

    let output = get_text_output(&result);
    assert!(output.contains("Line 1"));
    assert!(output.contains("Line 2000"));
    assert!(!output.contains("Line 2001"));
    assert!(output.contains("[Showing lines 1-2000 of 2500. Use offset=2001 to continue.]"));
}

#[tokio::test]
async fn read_tool_truncates_when_byte_limit_exceeded() {
    let dir = TempDir::new().unwrap();
    let test_file = dir.path().join("large-bytes.txt");
    let lines: Vec<String> = (1..=500).map(|i| format!("Line {i}: {}", "x".repeat(200))).collect();
    std::fs::write(&test_file, lines.join("\n")).unwrap();

    let tool = create_read_tool(&dir.path().to_string_lossy(), None);
    let result = (tool.execute)(
        "test-call-4".into(),
        json!({ "path": test_file.to_string_lossy() }),
        None,
        None,
    )
    .await
    .unwrap();

    let output = get_text_output(&result);
    assert!(output.contains("Line 1:"));
    assert!(output.contains(" limit). Use offset="), "{output}");
}

#[tokio::test]
async fn read_tool_handles_offset_and_limit() {
    let dir = TempDir::new().unwrap();
    let test_file = dir.path().join("offset.txt");
    let lines: Vec<String> = (1..=100).map(|i| format!("Line {i}")).collect();
    std::fs::write(&test_file, lines.join("\n")).unwrap();

    let tool = create_read_tool(&dir.path().to_string_lossy(), None);

    let r_off = (tool.execute)(
        "test-call-5".into(),
        json!({ "path": test_file.to_string_lossy(), "offset": 51 }),
        None,
        None,
    )
    .await
    .unwrap();
    let out_off = get_text_output(&r_off);
    assert!(!out_off.contains("Line 50\n"));
    assert!(out_off.contains("Line 51"));
    assert!(out_off.contains("Line 100"));
    assert!(!out_off.contains("Use offset="));

    let r_lim = (tool.execute)(
        "test-call-6".into(),
        json!({ "path": test_file.to_string_lossy(), "limit": 10 }),
        None,
        None,
    )
    .await
    .unwrap();
    let out_lim = get_text_output(&r_lim);
    assert!(out_lim.contains("Line 1"));
    assert!(out_lim.contains("Line 10"));
    assert!(!out_lim.contains("Line 11"));
    assert!(out_lim.contains("[90 more lines in file. Use offset=11 to continue.]"));

    let r_both = (tool.execute)(
        "test-call-7".into(),
        json!({ "path": test_file.to_string_lossy(), "offset": 41, "limit": 20 }),
        None,
        None,
    )
    .await
    .unwrap();
    let out_both = get_text_output(&r_both);
    assert!(!out_both.contains("Line 40\n"));
    assert!(out_both.contains("Line 41"));
    assert!(out_both.contains("Line 60"));
    assert!(!out_both.contains("Line 61"));
    assert!(out_both.contains("[40 more lines in file. Use offset=61 to continue.]"));
}

#[tokio::test]
async fn read_tool_errors_when_offset_beyond_file_length() {
    let dir = TempDir::new().unwrap();
    let test_file = dir.path().join("short.txt");
    std::fs::write(&test_file, "Line 1\nLine 2\nLine 3").unwrap();

    let tool = create_read_tool(&dir.path().to_string_lossy(), None);
    let err = (tool.execute)(
        "test-call-8".into(),
        json!({ "path": test_file.to_string_lossy(), "offset": 100 }),
        None,
        None,
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("beyond end of file"), "{err}");
}

#[tokio::test]
async fn read_tool_includes_truncation_details_and_images() {
    let dir = TempDir::new().unwrap();
    let big_file = dir.path().join("big-details.txt");
    let lines: Vec<String> = (1..=2500).map(|i| format!("Line {i}")).collect();
    std::fs::write(&big_file, lines.join("\n")).unwrap();

    let tool = create_read_tool(&dir.path().to_string_lossy(), None);
    let result = (tool.execute)(
        "test-call-9".into(),
        json!({ "path": big_file.to_string_lossy() }),
        None,
        None,
    )
    .await
    .unwrap();

    let details: ReadToolDetails = serde_json::from_value(result.details.unwrap()).unwrap();
    let trunc = details.truncation.unwrap();
    assert!(trunc.truncated);
    assert_eq!(trunc.total_lines, 2500);
    assert_eq!(trunc.output_lines, 2000);

    // Image magic detection even without .png extension
    let img_no_ext = dir.path().join("image.dat");
    std::fs::write(&img_no_ext, pi_js::b64::decode(TINY_PNG_BASE64)).unwrap();
    let img_res = (tool.execute)(
        "test-call-img".into(),
        json!({ "path": img_no_ext.to_string_lossy() }),
        None,
        None,
    )
    .await
    .unwrap();
    let note = get_text_output(&img_res);
    assert!(note.contains("Read image file [image/png]"));
    let img_block = img_res
        .content
        .iter()
        .find_map(|c| match c {
            UserContentBlock::Image(i) => Some(i),
            _ => None,
        })
        .unwrap();
    assert_eq!(img_block.mime_type, "image/png");
    assert_eq!(
        img_res.structured_content,
        Some(json!({
            "type": "image",
            "data": img_block.data,
            "mimeType": "image/png",
            "note": note,
        }))
    );

    // BMP conversion to PNG
    let bmp_file = dir.path().join("sample.bmp");
    std::fs::write(&bmp_file, create_tiny_bmp_1x1_red_24bpp()).unwrap();
    let bmp_res = (tool.execute)(
        "test-call-bmp".into(),
        json!({ "path": bmp_file.to_string_lossy() }),
        None,
        None,
    )
    .await
    .unwrap();
    let bmp_note = get_text_output(&bmp_res);
    assert!(bmp_note.contains("Read image file [image/png]"));
    assert!(bmp_note.contains("[Image converted from image/bmp to image/png.]"));
    let bmp_img = bmp_res
        .content
        .iter()
        .find_map(|c| match c {
            UserContentBlock::Image(i) => Some(i),
            _ => None,
        })
        .unwrap();
    assert_eq!(bmp_img.mime_type, "image/png");
    assert_eq!(pi_js::b64::decode(&bmp_img.data)[0], 0x89);

    // Non-image content with .png extension treated as text
    let fake_png = dir.path().join("fake.png");
    std::fs::write(&fake_png, "definitely not a png").unwrap();
    let fake_res = (tool.execute)(
        "test-call-fake-png".into(),
        json!({ "path": fake_png.to_string_lossy() }),
        None,
        None,
    )
    .await
    .unwrap();
    assert!(get_text_output(&fake_res).contains("definitely not a png"));
    assert!(!fake_res.content.iter().any(|c| matches!(c, UserContentBlock::Image(_))));
}

// =========================================================================
// Write tool
// =========================================================================

#[tokio::test]
async fn write_tool_writes_and_creates_parent_dirs() {
    let dir = TempDir::new().unwrap();
    let tool = create_write_tool(&dir.path().to_string_lossy(), None);

    let f1 = dir.path().join("write-test.txt");
    let res1 = (tool.execute)(
        "test-write-1".into(),
        json!({ "path": f1.to_string_lossy(), "content": "Test content" }),
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        get_text_output(&res1),
        format!("Successfully wrote to {}", f1.to_string_lossy())
    );
    assert!(res1.details.is_none());
    assert_eq!(std::fs::read_to_string(&f1).unwrap(), "Test content");

    let nested = dir.path().join("nested/deep/file.txt");
    let res2 = (tool.execute)(
        "test-write-2".into(),
        json!({ "path": nested.to_string_lossy(), "content": "Nested content" }),
        None,
        None,
    )
    .await
    .unwrap();
    assert!(get_text_output(&res2).contains("Successfully wrote"));
    assert_eq!(std::fs::read_to_string(&nested).unwrap(), "Nested content");
}

// =========================================================================
// Edit tool & multi-edit & diff preview
// =========================================================================

#[tokio::test]
async fn edit_tool_replaces_text_and_produces_applicable_patch() {
    let dir = TempDir::new().unwrap();
    let test_file = dir.path().join("edit-test.txt");
    let original = "Hello, world!";
    std::fs::write(&test_file, original).unwrap();

    let tool = create_edit_tool(&dir.path().to_string_lossy(), None);
    let result = (tool.execute)(
        "test-edit-1".into(),
        json!({
            "path": test_file.to_string_lossy(),
            "edits": [{ "oldText": "world", "newText": "testing" }],
        }),
        None,
        None,
    )
    .await
    .unwrap();

    assert!(get_text_output(&result).contains("Successfully replaced"));
    let details = result.details.unwrap();
    assert!(details["diff"].as_str().unwrap().contains("testing"));
    let patch = details["patch"].as_str().unwrap();
    assert!(patch.contains("--- "));
    assert!(patch.contains("+++ "));
    assert!(patch.contains("@@"));
    assert!(patch.contains("-Hello, world!"));
    assert!(patch.contains("+Hello, testing!"));
    assert_eq!(apply_patch(original, patch, None).unwrap().unwrap(), "Hello, testing!");
}

#[tokio::test]
async fn edit_tool_error_cases_and_disjoint_multi_edit() {
    let dir = TempDir::new().unwrap();
    let cwd = dir.path().to_string_lossy().into_owned();
    let tool = create_edit_tool(&cwd, None);

    // Non-existent file -> ENOENT
    let missing = dir.path().join("missing.txt");
    let err_missing = (tool.execute)(
        "e1".into(),
        json!({ "path": missing.to_string_lossy(), "edits": [{ "oldText": "a", "newText": "b" }] }),
        None,
        None,
    )
    .await
    .unwrap_err();
    assert!(err_missing.to_string().contains("ENOENT"), "{err_missing}");

    // Not found
    let f = dir.path().join("notfound.txt");
    std::fs::write(&f, "Hello, world!").unwrap();
    let err_nf = (tool.execute)(
        "e2".into(),
        json!({ "path": f.to_string_lossy(), "edits": [{ "oldText": "nonexistent", "newText": "x" }] }),
        None,
        None,
    )
    .await
    .unwrap_err();
    assert!(err_nf.to_string().contains("Could not find"), "{err_nf}");

    // Duplicate match
    let dup = dir.path().join("dup.txt");
    std::fs::write(&dup, "foo\nbar\nfoo\n").unwrap();
    let err_dup = (tool.execute)(
        "e3".into(),
        json!({ "path": dup.to_string_lossy(), "edits": [{ "oldText": "foo", "newText": "baz" }] }),
        None,
        None,
    )
    .await
    .unwrap_err();
    assert!(err_dup.to_string().contains("Found 2 occurrences"), "{err_dup}");

    // Disjoint multi-edit
    let multi = dir.path().join("multi.txt");
    std::fs::write(&multi, "alpha\nbeta\ngamma\ndelta\n").unwrap();
    let multi_res = (tool.execute)(
        "e4".into(),
        json!({
            "path": multi.to_string_lossy(),
            "edits": [
                { "oldText": "alpha", "newText": "ALPHA" },
                { "oldText": "gamma", "newText": "GAMMA" },
            ]
        }),
        None,
        None,
    )
    .await
    .unwrap();
    assert!(get_text_output(&multi_res).contains("Successfully replaced 2 block(s)"));
    assert_eq!(std::fs::read_to_string(&multi).unwrap(), "ALPHA\nbeta\nGAMMA\ndelta\n");

    // Large gap collapse in diff
    let big = dir.path().join("big-diff.txt");
    let big_lines: Vec<String> = (1..=600).map(|i| format!("line {i}")).collect();
    std::fs::write(&big, big_lines.join("\n")).unwrap();
    let big_res = (tool.execute)(
        "e5".into(),
        json!({
            "path": big.to_string_lossy(),
            "edits": [
                { "oldText": "line 100", "newText": "LINE 100" },
                { "oldText": "line 300", "newText": "LINE 300" },
                { "oldText": "line 500", "newText": "LINE 500" },
            ]
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let diff = big_res.details.unwrap()["diff"].as_str().unwrap().to_string();
    assert!(diff.contains("LINE 100"));
    assert!(diff.contains("LINE 300"));
    assert!(diff.contains("LINE 500"));
    assert!(diff.contains("..."));
    assert!(!diff.contains("line 250"));
    assert!(diff.lines().count() < 50);

    // Match edits against original, not incrementally
    let orig2 = dir.path().join("non-inc2.txt");
    std::fs::write(&orig2, "foo\nbar\nbaz\n").unwrap();
    let _ = (tool.execute)(
        "e6b".into(),
        json!({
            "path": orig2.to_string_lossy(),
            "edits": [
                { "oldText": "foo\nbar", "newText": "foo bar" },
                { "oldText": "baz\n", "newText": "BAR\nbaz\n" },
            ]
        }),
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read_to_string(&orig2).unwrap(), "foo bar\nBAR\nbaz\n");

    // Empty edits fails
    assert!(
        (tool.execute)(
            "e7".into(),
            json!({ "path": orig2.to_string_lossy(), "edits": [] }),
            None,
            None
        )
        .await
        .is_err()
    );

    // Overlapping edits fails without mutating file
    let ov = dir.path().join("overlap.txt");
    let ov_content = "alpha\nbeta\ngamma\n";
    std::fs::write(&ov, ov_content).unwrap();
    assert!(
        (tool.execute)(
            "e8".into(),
            json!({
                "path": ov.to_string_lossy(),
                "edits": [
                    { "oldText": "alpha\nbeta", "newText": "A" },
                    { "oldText": "beta\ngamma", "newText": "B" },
                ]
            }),
            None,
            None
        )
        .await
        .is_err()
    );
    assert_eq!(std::fs::read_to_string(&ov).unwrap(), ov_content);

    // One bad edit fails without partial application
    assert!(
        (tool.execute)(
            "e9".into(),
            json!({
                "path": ov.to_string_lossy(),
                "edits": [
                    { "oldText": "alpha", "newText": "A" },
                    { "oldText": "missing", "newText": "B" },
                ]
            }),
            None,
            None
        )
        .await
        .is_err()
    );
    assert_eq!(std::fs::read_to_string(&ov).unwrap(), ov_content);
}

#[tokio::test]
async fn edit_tool_eacces_unknown_error_and_diff_preview() {
    let dir = TempDir::new().unwrap();
    let cwd = dir.path().to_string_lossy().into_owned();

    // Unknown error message via custom EditOperations
    struct FailingAccessOps;
    impl EditOperations for FailingAccessOps {
        fn read_file(&self, _p: &str) -> BoxFuture<pi_js::Result<Vec<u8>>> {
            Box::pin(async { Ok(b"hello\n".to_vec()) })
        }
        fn write_file(&self, _p: &str, _c: &str) -> BoxFuture<pi_js::Result<()>> {
            Box::pin(async { Ok(()) })
        }
        fn access(&self, _p: &str) -> BoxFuture<pi_js::Result<()>> {
            Box::pin(async { Err(pi_js::Error::msg("disk offline")) })
        }
    }
    let broken_tool = create_edit_tool(
        &cwd,
        Some(EditToolOptions {
            operations: Some(Arc::new(FailingAccessOps)),
        }),
    );
    let err = (broken_tool.execute)(
        "broken".into(),
        json!({ "path": "broken.txt", "edits": [{ "oldText": "hello", "newText": "world" }] }),
        None,
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(err.to_string(), "Could not edit file: broken.txt. Error: disk offline.");

    // compute_edits_diff ENOENT preview
    let missing = dir.path().join("missing-preview.txt");
    let missing_str = missing.to_string_lossy().into_owned();
    assert_eq!(
        compute_edits_diff(
            &missing_str,
            &[EditReplacement {
                old_text: "hello".into(),
                new_text: "world".into(),
            }],
            &cwd,
            None
        )
        .await,
        EditDiffPreview::Error {
            error: format!("Could not edit file: {missing_str}. Error code: ENOENT.")
        }
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let ro = dir.path().join("unreadable-preview.txt");
        std::fs::write(&ro, "hello\n").unwrap();
        std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o222)).unwrap();
        let ro_str = ro.to_string_lossy().into_owned();
        let res = compute_edits_diff(
            &ro_str,
            &[EditReplacement {
                old_text: "hello".into(),
                new_text: "world".into(),
            }],
            &cwd,
            None,
        )
        .await;
        std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            res,
            EditDiffPreview::Error {
                error: format!("Could not edit file: {ro_str}. Error code: EACCES.")
            }
        );
    }
}

// =========================================================================
// Ls tool
// =========================================================================

#[tokio::test]
async fn ls_tool_lists_dotfiles_dirs_and_respects_limits() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join(".hidden-file"), "secret").unwrap();
    std::fs::create_dir(dir.path().join(".hidden-dir")).unwrap();

    let cwd = dir.path().to_string_lossy().into_owned();
    let ls_tool = create_ls_tool(&cwd, None);

    let res = (ls_tool.execute)("ls1".into(), json!({ "path": cwd }), None, None)
        .await
        .unwrap();
    let out = get_text_output(&res);
    assert!(out.contains(".hidden-file"));
    assert!(out.contains(".hidden-dir/"));

    let dir2 = TempDir::new().unwrap();
    for name in ["a.txt", "b.txt", "c.txt"] {
        std::fs::write(dir2.path().join(name), name).unwrap();
    }
    let cwd2 = dir2.path().to_string_lossy().into_owned();

    for lim in [json!(0), json!(-5), json!(0.5), json!(1.9)] {
        let r = (ls_tool.execute)("ls2".into(), json!({ "path": cwd2, "limit": lim }), None, None)
            .await
            .unwrap();
        assert_eq!(
            get_text_output(&r),
            "a.txt\n\n[1 entries limit reached. Use limit=2 for more]"
        );
        let details: LsToolDetails = serde_json::from_value(r.details.unwrap()).unwrap();
        assert_eq!(details.entry_limit_reached, Some(1));
    }

    let exact = (ls_tool.execute)("ls3".into(), json!({ "path": cwd2, "limit": 3 }), None, None)
        .await
        .unwrap();
    assert_eq!(get_text_output(&exact), "a.txt\nb.txt\nc.txt");
    assert!(exact.details.is_none());
}

// =========================================================================
// Edit tool fuzzy matching & CRLF / BOM / #8544
// =========================================================================

#[tokio::test]
async fn edit_tool_fuzzy_matching_cases() {
    let dir = TempDir::new().unwrap();
    let cwd = dir.path().to_string_lossy().into_owned();
    let tool = create_edit_tool(&cwd, None);

    // Trailing whitespace stripped
    let f1 = dir.path().join("trailing-ws.txt");
    std::fs::write(&f1, "line one   \nline two  \nline three\n").unwrap();
    let _ = (tool.execute)(
        "f1".into(),
        json!({ "path": f1.to_string_lossy(), "edits": [{ "oldText": "line one\nline two\n", "newText": "replaced\n" }] }),
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read_to_string(&f1).unwrap(), "replaced\nline three\n");

    // Fullwidth Chinese punctuation
    let f2 = dir.path().join("chinese.txt");
    std::fs::write(&f2, "你好，世界\n你好（世界）\n").unwrap();
    let _ = (tool.execute)(
        "f2".into(),
        json!({ "path": f2.to_string_lossy(), "edits": [{ "oldText": "你好,世界\n你好(世界)\n", "newText": "你好，pi\n你好(pi)\n" }] }),
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read_to_string(&f2).unwrap(), "你好，pi\n你好(pi)\n");

    // Compatibility Unicode forms (NFKC)
    let f3 = dir.path().join("compat.txt");
    std::fs::write(&f3, "ＡＢＣ１２３\ncafe\u{0301}\n").unwrap();
    let _ = (tool.execute)(
        "f3".into(),
        json!({ "path": f3.to_string_lossy(), "edits": [{ "oldText": "ABC123\ncafé\n", "newText": "XYZ789\ncoffee\n" }] }),
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read_to_string(&f3).unwrap(), "XYZ789\ncoffee\n");

    // Smart quotes, Unicode dashes, NBSP
    let f4 = dir.path().join("smart.txt");
    std::fs::write(&f4, "console.log(\u{2018}hello\u{2019});\nconst msg = \u{201C}Hello World\u{201D};\nrange: 1\u{2013}5\nbreak\u{2014}here\nhello\u{00A0}world\n").unwrap();
    let _ = (tool.execute)(
        "f4".into(),
        json!({
            "path": f4.to_string_lossy(),
            "edits": [
                { "oldText": "console.log('hello');", "newText": "console.log('world');" },
                { "oldText": "const msg = \"Hello World\";", "newText": "const msg = \"Goodbye\";" },
                { "oldText": "range: 1-5\nbreak-here", "newText": "range: 10-50\nbreak--here" },
                { "oldText": "hello world", "newText": "hello universe" },
            ]
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let f4_out = std::fs::read_to_string(&f4).unwrap();
    assert!(f4_out.contains("world"));
    assert!(f4_out.contains("Goodbye"));
    assert!(f4_out.contains("10-50"));
    assert!(f4_out.contains("universe"));

    // Preserve untouched lines and produce applicable patch for fuzzy multi-edits
    let f5 = dir.path().join("preserve-fuzzy.txt");
    let orig = "intro\u{00A0}keep  \nfirst ‘one’\nuntouched — line  \nsecond “two”\n";
    let expected = "intro\u{00A0}keep  \nfirst 'ONE'\nuntouched — line  \nsecond \"TWO\"\n";
    std::fs::write(&f5, orig).unwrap();
    let res5 = (tool.execute)(
        "f5".into(),
        json!({
            "path": f5.to_string_lossy(),
            "edits": [
                { "oldText": "first 'one'", "newText": "first 'ONE'" },
                { "oldText": "second \"two\"", "newText": "second \"TWO\"" },
            ]
        }),
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read_to_string(&f5).unwrap(), expected);
    let patch = res5.details.unwrap()["patch"].as_str().unwrap().to_string();
    assert_eq!(apply_patch(orig, &patch, None).unwrap().unwrap(), expected);
}

#[tokio::test]
async fn edit_tool_crlf_bom_and_mixed_line_endings_n8544() {
    let dir = TempDir::new().unwrap();
    let cwd = dir.path().to_string_lossy().into_owned();
    let tool = create_edit_tool(&cwd, None);

    // LF oldText against CRLF preserves CRLF
    let f1 = dir.path().join("crlf.txt");
    std::fs::write(&f1, "line1\r\nline2\r\nline3\r\n").unwrap();
    let _ = (tool.execute)(
        "c1".into(),
        json!({ "path": f1.to_string_lossy(), "edits": [{ "oldText": "line2\nline3", "newText": "LINE2\nLINE3" }] }),
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read(&f1).unwrap(), b"line1\r\nLINE2\r\nLINE3\r\n");

    // Preserve UTF-8 BOM + CRLF in multi-edit
    let f2 = dir.path().join("bom-crlf.txt");
    std::fs::write(&f2, "\u{FEFF}first\r\nsecond\r\nthird\r\nfourth\r\n").unwrap();
    let _ = (tool.execute)(
        "c2".into(),
        json!({
            "path": f2.to_string_lossy(),
            "edits": [
                { "oldText": "second", "newText": "SECOND" },
                { "oldText": "fourth", "newText": "FOURTH" },
            ]
        }),
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(&f2).unwrap(),
        "\u{FEFF}first\r\nSECOND\r\nthird\r\nFOURTH\r\n"
    );

    // Mixed LF first then CRLF (#8544)
    let f3 = dir.path().join("mixed-lf-first.txt");
    std::fs::write(&f3, "a\nb\r\nX = 1\r\n").unwrap();
    let _ = (tool.execute)(
        "c3".into(),
        json!({ "path": f3.to_string_lossy(), "edits": [{ "oldText": "X = 1", "newText": "X = 2" }] }),
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read(&f3).unwrap(), b"a\nb\r\nX = 2\r\n");

    // Mixed CRLF first then LF (#8544)
    let f4 = dir.path().join("mixed-crlf-first.txt");
    std::fs::write(&f4, "a\r\nb\nc").unwrap();
    let _ = (tool.execute)(
        "c4".into(),
        json!({ "path": f4.to_string_lossy(), "edits": [{ "oldText": "c", "newText": "C" }] }),
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read(&f4).unwrap(), b"a\r\nb\nC");

    // Keep lone CR in untouched line (#8544)
    let f5 = dir.path().join("lone-cr.txt");
    std::fs::write(&f5, "a\rb\nX = 1\n").unwrap();
    let _ = (tool.execute)(
        "c5".into(),
        json!({ "path": f5.to_string_lossy(), "edits": [{ "oldText": "X = 1", "newText": "X = 2" }] }),
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read(&f5).unwrap(), b"a\rb\nX = 2\n");

    // Multi-edit adopts each block's local line ending (#8544)
    let f6 = dir.path().join("mixed-multi.txt");
    std::fs::write(&f6, "lf1\nlf2\ncr1\r\ncr2\r\n").unwrap();
    let _ = (tool.execute)(
        "c6".into(),
        json!({
            "path": f6.to_string_lossy(),
            "edits": [
                { "oldText": "lf1\n", "newText": "A\nB\n" },
                { "oldText": "cr1\n", "newText": "C\nD\n" },
            ]
        }),
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read(&f6).unwrap(), b"A\nB\nlf2\nC\r\nD\r\ncr2\r\n");

    // Fuzzy widening keeps untouched endings (#8544)
    let f7 = dir.path().join("mixed-fuzzy.txt");
    std::fs::write(&f7, "a\r\nfoo   \r\nb\n").unwrap();
    let _ = (tool.execute)(
        "c7".into(),
        json!({ "path": f7.to_string_lossy(), "edits": [{ "oldText": "foo\n", "newText": "FOO\n" }] }),
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read(&f7).unwrap(), b"a\r\nFOO\r\nb\n");

    // CR/LF seam invariants (#8544)
    for (idx, (old_t, new_t, written, diffed)) in [
        ("b", "\nB", "a\r\n\nB\nc", "a\n\nB\nc"),
        ("b", "", "a\r\n\nc", "a\n\nc"),
    ]
    .into_iter()
    .enumerate()
    {
        let seam = dir.path().join(format!("cr-seam-{idx}.txt"));
        std::fs::write(&seam, "a\rb\nc").unwrap();
        let res = (tool.execute)(
            format!("seam-{idx}"),
            json!({ "path": seam.to_string_lossy(), "edits": [{ "oldText": old_t, "newText": new_t }] }),
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read(&seam).unwrap(), written.as_bytes());
        let patch = res.details.unwrap()["patch"].as_str().unwrap().to_string();
        assert_eq!(apply_patch("a\nb\nc", &patch, None).unwrap().unwrap(), diffed);
    }

    let inserted_cr_written = apply_replacements_to_original(
        "a\rb\nc",
        &[pi_coding_agent::core::tools::edit_diff::TextReplacement {
            match_index: 2,
            match_length: 1,
            new_text: "\r".to_string(),
        }],
    );
    assert_eq!(normalize_to_lf(&inserted_cr_written), "a\n\n\nc");
}

#[test]
fn edit_diff_mixed_line_endings_property_fuzz_3000() {
    let mut seed: u64 = 1;
    let mut rand_n = |n: usize| -> usize {
        seed = (seed.wrapping_mul(1_103_515_245).wrapping_add(12345)) & 0x7fff_ffff;
        ((seed as f64 / 2_147_483_648.0) * (n as f64)).floor() as usize
    };
    let endings = ["\n", "\r\n", "\r", ""];
    let replacements = ["", "\n", "X\n", "\nY", "\r", "Z"];

    for _ in 0..3000 {
        let mut original = String::new();
        let lines = rand_n(12) + 1;
        for _ in 0..lines {
            let num = rand_n(100);
            let ws = if rand_n(5) == 0 { "  " } else { "" };
            let end = endings[rand_n(4)];
            original.push_str(&format!("w{num}{ws}{end}"));
        }
        let normalized = normalize_to_lf(&original);
        if normalized.is_empty() {
            continue;
        }
        let start = rand_n(normalized.len());
        let max_len = (1 + rand_n(12)).min(normalized.len() - start);
        let old_text = &normalized[start..start + max_len];
        let new_text = replacements[rand_n(6)];

        let Ok(applied) = apply_edits_to_normalized_content(&normalized, &[(old_text, new_text)], "f.txt") else {
            continue;
        };

        let written = apply_replacements_to_original(&original, &applied.replacements);
        assert_eq!(normalize_to_lf(&written), applied.new_content);
    }
}
