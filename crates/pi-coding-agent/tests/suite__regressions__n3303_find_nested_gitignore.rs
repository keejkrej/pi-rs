//! Port of packages/coding-agent/test/suite/regressions/3303-find-nested-gitignore.test.ts

use pi_ai::types::UserContentBlock;
use pi_coding_agent::core::tools::find::create_find_tool;
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

#[tokio::test]
async fn flat_sibling_gitignore_scopes_only_inside_subtree() {
    let dir = TempDir::new().unwrap();
    std::fs::create_dir_all(dir.path().join("a")).unwrap();
    std::fs::create_dir_all(dir.path().join("b")).unwrap();
    std::fs::write(dir.path().join("a/.gitignore"), "ignored.txt\n").unwrap();
    std::fs::write(dir.path().join("a/ignored.txt"), "").unwrap();
    std::fs::write(dir.path().join("a/kept.txt"), "").unwrap();
    std::fs::write(dir.path().join("b/ignored.txt"), "").unwrap();
    std::fs::write(dir.path().join("b/kept.txt"), "").unwrap();
    std::fs::write(dir.path().join("root.txt"), "").unwrap();

    let files = run_find(&dir.path().to_string_lossy(), "**/*.txt").await;
    assert_eq!(files, vec!["a/kept.txt", "b/ignored.txt", "b/kept.txt", "root.txt"]);
}

#[tokio::test]
async fn deeply_nested_gitignore_scopes_each_to_its_own_subtree() {
    let dir = TempDir::new().unwrap();
    std::fs::create_dir_all(dir.path().join("a/deep")).unwrap();
    std::fs::create_dir_all(dir.path().join("b")).unwrap();
    std::fs::write(dir.path().join("a/.gitignore"), "ignored.txt\n").unwrap();
    std::fs::write(dir.path().join("a/deep/.gitignore"), "secret.txt\n").unwrap();
    std::fs::write(dir.path().join("a/ignored.txt"), "").unwrap();
    std::fs::write(dir.path().join("a/kept.txt"), "").unwrap();
    std::fs::write(dir.path().join("a/deep/ignored.txt"), "").unwrap();
    std::fs::write(dir.path().join("a/deep/secret.txt"), "").unwrap();
    std::fs::write(dir.path().join("a/deep/kept.txt"), "").unwrap();
    std::fs::write(dir.path().join("b/ignored.txt"), "").unwrap();
    std::fs::write(dir.path().join("b/kept.txt"), "").unwrap();
    std::fs::write(dir.path().join("root.txt"), "").unwrap();

    let files = run_find(&dir.path().to_string_lossy(), "**/*.txt").await;
    assert_eq!(
        files,
        vec![
            "a/deep/kept.txt",
            "a/kept.txt",
            "b/ignored.txt",
            "b/kept.txt",
            "root.txt",
        ]
    );
}
