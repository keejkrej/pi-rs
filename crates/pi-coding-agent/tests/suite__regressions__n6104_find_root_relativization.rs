//! Port of packages/coding-agent/test/suite/regressions/6104-find-root-relativization.test.ts

use std::sync::Arc;

use pi_ai::types::UserContentBlock;
use pi_coding_agent::core::tools::find::{
    FindGlobOptions, FindOperations, FindPathMode, FindToolOptions, create_find_tool, relativize_find_result_path,
};
use pi_js::BoxFuture;
use serde_json::json;

#[test]
fn windows_drive_root_relativization() {
    let search_root = "I:\\";
    assert_eq!(
        relativize_find_result_path(
            "I:\\AI\\Models\\TextGen\\gemma4\\",
            search_root,
            Some(FindPathMode::Win32)
        ),
        "AI/Models/TextGen/gemma4/"
    );
    assert_eq!(
        relativize_find_result_path("I:/AI/Models/TextGen/gemma4/", search_root, Some(FindPathMode::Win32)),
        "AI/Models/TextGen/gemma4/"
    );
    assert_eq!(
        relativize_find_result_path("I:\\AI\\Models\\", "I:\\AI", Some(FindPathMode::Win32)),
        "Models/"
    );
    assert_eq!(
        relativize_find_result_path("I:\\AI\\Models2\\file.txt", "I:\\AI\\Models", Some(FindPathMode::Win32)),
        "../Models2/file.txt"
    );
    assert_eq!(
        relativize_find_result_path("AI\\Models\\TextGen\\gemma4\\", search_root, Some(FindPathMode::Win32)),
        "AI/Models/TextGen/gemma4/"
    );
}

#[test]
fn posix_root_relativization() {
    assert_eq!(
        relativize_find_result_path("/home/user/file.txt", "/", Some(FindPathMode::Posix)),
        "home/user/file.txt"
    );
    assert_eq!(
        relativize_find_result_path("/home/user/project/", "/", Some(FindPathMode::Posix)),
        "home/user/project/"
    );
    assert_eq!(
        relativize_find_result_path("/home/user/file\\", "/home/user", Some(FindPathMode::Posix)),
        "file\\"
    );
}

#[test]
fn absolute_results_outside_search_path() {
    assert_eq!(
        relativize_find_result_path("/tmp/results/file.txt", "/workspace/project", Some(FindPathMode::Posix)),
        "../../tmp/results/file.txt"
    );
    assert_eq!(
        relativize_find_result_path("/tmp/results/dir/", "/workspace/project", Some(FindPathMode::Posix)),
        "../../tmp/results/dir/"
    );
}

#[tokio::test]
async fn through_find_tool_relativizes_custom_glob_results_against_root() {
    struct RootGlobOps;
    impl FindOperations for RootGlobOps {
        fn exists(&self, _absolute_path: &str) -> BoxFuture<bool> {
            Box::pin(async { true })
        }
        fn glob(&self, _pattern: &str, _cwd: &str, _options: FindGlobOptions) -> BoxFuture<pi_js::Result<Vec<String>>> {
            Box::pin(async {
                Ok(vec![
                    "/home/user/project/".to_string(),
                    "/home/user/project/file.txt".to_string(),
                ])
            })
        }
    }

    let find = create_find_tool(
        "/",
        Some(FindToolOptions {
            operations: Some(Arc::new(RootGlobOps)),
        }),
    );
    let res = (find.execute)("call-1".into(), json!({ "pattern": "**" }), None, None)
        .await
        .unwrap();
    let text = match &res.content[0] {
        UserContentBlock::Text(t) => t.text.as_str(),
        _ => "",
    };
    assert_eq!(text, "home/user/project/\nhome/user/project/file.txt");
}
