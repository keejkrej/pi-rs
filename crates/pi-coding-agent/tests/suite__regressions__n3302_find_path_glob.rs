//! Port of packages/coding-agent/test/suite/regressions/3302-find-path-glob.test.ts

use pi_ai::types::UserContentBlock;
use pi_coding_agent::core::tools::find::{FindToolDetails, create_find_tool};
use serde_json::json;
use tempfile::TempDir;

async fn run_find(root: &str, pattern: &str) -> Vec<String> {
    let tool = create_find_tool(root, None);
    let res = (tool.execute)("call-1".into(), json!({ "pattern": pattern }), None, None)
        .await
        .unwrap();

    let text = match &res.content[0] {
        UserContentBlock::Text(t) => t.text.as_str(),
        _ => "",
    };
    if text == "No files found matching pattern" {
        return Vec::new();
    }
    let mut lines: Vec<String> = text
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty() && !l.starts_with('['))
        .collect();
    lines.sort();
    lines
}

fn setup_fixture() -> TempDir {
    let dir = TempDir::new().unwrap();
    std::fs::create_dir_all(dir.path().join("some/parent/child")).unwrap();
    std::fs::create_dir_all(dir.path().join("src/foo/bar")).unwrap();
    std::fs::write(dir.path().join("some/parent/child/file.ext"), "").unwrap();
    std::fs::write(dir.path().join("some/parent/child/test.spec.ts"), "").unwrap();
    std::fs::write(dir.path().join("src/foo/bar/example.spec.ts"), "").unwrap();
    std::fs::write(dir.path().join("src/direct.spec.ts"), "").unwrap();
    dir
}

#[tokio::test]
async fn basename_pattern_still_matches() {
    let dir = setup_fixture();
    let files = run_find(&dir.path().to_string_lossy(), "*.spec.ts").await;
    assert_eq!(
        files,
        vec![
            "some/parent/child/test.spec.ts",
            "src/direct.spec.ts",
            "src/foo/bar/example.spec.ts",
        ]
    );
}

#[tokio::test]
async fn directory_prefixed_pattern_with_globstar_tail_matches_subtree() {
    let dir = setup_fixture();
    let files = run_find(&dir.path().to_string_lossy(), "some/parent/child/**").await;
    assert!(files.contains(&"some/parent/child/file.ext".to_string()));
    assert!(files.contains(&"some/parent/child/test.spec.ts".to_string()));
    assert!(!files.contains(&"some/parent/child/".to_string()));
}

#[tokio::test]
async fn leading_globstar_wildcard_with_path_segments_matches() {
    let dir = setup_fixture();
    let files = run_find(&dir.path().to_string_lossy(), "**/parent/child/*").await;
    assert!(files.contains(&"some/parent/child/file.ext".to_string()));
    assert!(files.contains(&"some/parent/child/test.spec.ts".to_string()));
}

#[tokio::test]
async fn src_globstar_spec_ts_matches_direct_and_nested() {
    let dir = setup_fixture();
    let files = run_find(&dir.path().to_string_lossy(), "src/**/*.spec.ts").await;
    assert_eq!(files, vec!["src/direct.spec.ts", "src/foo/bar/example.spec.ts"]);
}

#[tokio::test]
async fn src_star_spec_ts_does_not_cross_directories() {
    let dir = setup_fixture();
    let files = run_find(&dir.path().to_string_lossy(), "src/*.spec.ts").await;
    assert_eq!(files, vec!["src/direct.spec.ts"]);
}

#[tokio::test]
async fn path_patterns_support_braces_classes_and_literal_regex_chars() {
    let dir = setup_fixture();
    std::fs::write(dir.path().join("src/a&b (1).ts"), "").unwrap();
    let root = dir.path().to_string_lossy().into_owned();
    assert_eq!(
        run_find(&root, "{src,some}/**/[et]*.spec.ts").await,
        vec!["some/parent/child/test.spec.ts", "src/foo/bar/example.spec.ts",]
    );
    assert_eq!(run_find(&root, "src/[!x]&b (1).ts").await, vec!["src/a&b (1).ts"]);
}

#[tokio::test]
async fn path_patterns_stop_at_result_limit() {
    let dir = setup_fixture();
    let tool = create_find_tool(&dir.path().to_string_lossy(), None);
    let res = (tool.execute)(
        "call-1".into(),
        json!({ "pattern": "src/**/*.spec.ts", "limit": 1 }),
        None,
        None,
    )
    .await
    .unwrap();
    let details: FindToolDetails = serde_json::from_value(res.details.unwrap()).unwrap();
    assert_eq!(details.result_limit_reached, Some(1));
    let text = match &res.content[0] {
        UserContentBlock::Text(t) => t.text.as_str(),
        _ => "",
    };
    let first = text.lines().next().unwrap();
    assert!(first == "src/direct.spec.ts" || first == "src/foo/bar/example.spec.ts");
}

#[cfg(unix)]
#[tokio::test]
async fn posix_backslash_still_escapes_glob_characters() {
    let dir = setup_fixture();
    std::fs::write(dir.path().join("src/*.ts"), "").unwrap();
    std::fs::write(dir.path().join("src/other.ts"), "").unwrap();
    let files = run_find(&dir.path().to_string_lossy(), r"src/\*.ts").await;
    assert_eq!(files, vec!["src/*.ts"]);
}
