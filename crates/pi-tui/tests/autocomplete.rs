//! Port of packages/tui/test/autocomplete.test.ts

use pi_js::abort::AbortController;
use pi_tui::CombinedAutocompleteProvider;
use pi_tui::autocomplete::{AutocompleteSuggestions, GetSuggestionsOptions};

struct TempTestDir {
    path: String,
}

impl TempTestDir {
    fn new(prefix: &str) -> Self {
        let base = pi_js::path::join(&[&pi_js::env::tmp_dir(), prefix]);
        let path = pi_js::fs::mkdtemp(&base).expect("mkdtemp should succeed");
        Self { path }
    }

    fn path(&self) -> &str {
        &self.path
    }
}

impl Drop for TempTestDir {
    fn drop(&mut self) {
        let _ = pi_js::fs::rm(&self.path, true, true);
    }
}

fn resolve_fd_path() -> Option<String> {
    let command = if pi_js::env::platform() == "win32" {
        "where"
    } else {
        "which"
    };
    let output = std::process::Command::new(command).arg("fd").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
}

fn setup_folder(base_dir: &str, dirs: &[&str], files: &[(&str, &str)]) {
    for dir in dirs {
        let full = pi_js::path::join(&[base_dir, dir]);
        pi_js::fs::mkdir(&full, true, None).expect("mkdir should succeed");
    }
    for (file_path, contents) in files {
        let full_path = pi_js::path::join(&[base_dir, file_path]);
        let parent = pi_js::path::dirname(&full_path);
        pi_js::fs::mkdir(&parent, true, None).expect("mkdir parent should succeed");
        pi_js::fs::write_file(&full_path, contents.as_bytes(), None).expect("write_file should succeed");
    }
}

async fn get_suggestions(
    provider: &CombinedAutocompleteProvider,
    lines: &[String],
    cursor_line: i64,
    cursor_col: i64,
    force: bool,
) -> Option<AutocompleteSuggestions> {
    let controller = AbortController::new();
    provider
        .get_suggestions(
            lines,
            cursor_line,
            cursor_col,
            GetSuggestionsOptions {
                signal: controller.signal(),
                force: Some(force),
            },
        )
        .await
}

#[tokio::test]
async fn extract_path_prefix_extracts_slash_from_hey_slash_when_forced() {
    let provider = CombinedAutocompleteProvider::new(None, "/tmp", None);
    let lines = vec!["hey /".to_string()];
    let result = get_suggestions(&provider, &lines, 0, 5, true).await;

    assert!(result.is_some(), "Should return suggestions for root directory");
    if let Some(res) = result {
        assert_eq!(res.prefix, "/", "Prefix should be '/'");
    }
}

#[tokio::test]
async fn extract_path_prefix_extracts_slash_a_from_slash_a_when_forced() {
    let provider = CombinedAutocompleteProvider::new(None, "/tmp", None);
    let lines = vec!["/A".to_string()];
    let result = get_suggestions(&provider, &lines, 0, 2, true).await;

    if let Some(res) = result {
        assert_eq!(res.prefix, "/A", "Prefix should be '/A'");
    }
}

#[tokio::test]
async fn extract_path_prefix_does_not_trigger_for_slash_commands() {
    let provider = CombinedAutocompleteProvider::new(None, "/tmp", None);
    let lines = vec!["/model".to_string()];
    let result = get_suggestions(&provider, &lines, 0, 6, true).await;

    assert!(result.is_none(), "Should not trigger for slash commands");
}

#[tokio::test]
async fn extract_path_prefix_triggers_for_absolute_paths_after_slash_command_argument() {
    let provider = CombinedAutocompleteProvider::new(None, "/tmp", None);
    let lines = vec!["/command /".to_string()];
    let result = get_suggestions(&provider, &lines, 0, 10, true).await;

    assert!(
        result.is_some(),
        "Should trigger for absolute paths in command arguments"
    );
    if let Some(res) = result {
        assert_eq!(res.prefix, "/", "Prefix should be '/'");
    }
}

fn setup_fd_dirs() -> Option<(TempTestDir, String, String, String)> {
    let fd_path = resolve_fd_path()?;
    let root = TempTestDir::new("pi-autocomplete-root-");
    let base_dir = pi_js::path::join(&[root.path(), "cwd"]);
    let outside_dir = pi_js::path::join(&[root.path(), "outside"]);
    pi_js::fs::mkdir(&base_dir, true, None).expect("mkdir cwd");
    pi_js::fs::mkdir(&outside_dir, true, None).expect("mkdir outside");
    Some((root, base_dir, outside_dir, fd_path))
}

#[tokio::test]
async fn fd_at_returns_all_files_and_folders_for_empty_query() {
    let Some((_root, base_dir, _outside, fd_path)) = setup_fd_dirs() else {
        return;
    };
    setup_folder(&base_dir, &["src"], &[("README.md", "readme")]);

    let provider = CombinedAutocompleteProvider::new(None, &base_dir, Some(&fd_path));
    let line = "@".to_string();
    let result = get_suggestions(&provider, &[line.clone()], 0, pi_js::str16::len(&line) as i64, false).await;

    let mut values: Vec<String> = result
        .expect("expected suggestions")
        .items
        .into_iter()
        .map(|item| item.value)
        .collect();
    values.sort();
    assert_eq!(values, vec!["@README.md", "@src/"]);
}

#[tokio::test]
async fn fd_at_recognizes_at_after_cjk_punctuation_without_consuming_preceding_text() {
    let Some((_root, base_dir, _outside, fd_path)) = setup_fd_dirs() else {
        return;
    };
    setup_folder(&base_dir, &[], &[("README.md", "readme")]);
    let provider = CombinedAutocompleteProvider::new(None, &base_dir, Some(&fd_path));

    let mut befores: Vec<String> = vec!["查看，".to_string(), "\u{3000}".to_string()];
    for ch in "，．：；！？（）［］｛｝“”‘’…—。、「」『』《》【】".chars() {
        befores.push(ch.to_string());
    }

    for before in befores {
        for force in [false, true] {
            let line = format!("{before}@REA");
            let col = pi_js::str16::len(&line) as i64;
            let result = get_suggestions(&provider, &[line.clone()], 0, col, force)
                .await
                .unwrap_or_else(|| panic!("expected result for {line}"));
            assert_eq!(result.prefix, "@REA");
            let values: Vec<String> = result.items.iter().map(|item| item.value.clone()).collect();
            assert_eq!(values, vec!["@README.md"]);
            let applied = provider.apply_completion(&[line], 0, col, &result.items[0], &result.prefix);
            let expected_line = format!("{before}@README.md ");
            assert_eq!(applied.lines[0], expected_line);
            assert_eq!(applied.cursor_col, pi_js::str16::len(&expected_line) as i64);
        }
    }
}

#[tokio::test]
async fn fd_at_recognizes_at_after_opening_wrappers_like_paren_and_backticks() {
    let Some((_root, base_dir, _outside, fd_path)) = setup_fd_dirs() else {
        return;
    };
    setup_folder(&base_dir, &[], &[("README.md", "readme")]);
    let provider = CombinedAutocompleteProvider::new(None, &base_dir, Some(&fd_path));

    for before in ["(", "see (", "[", "`", "<", "{"] {
        let line = format!("{before}@REA");
        let col = pi_js::str16::len(&line) as i64;
        let result = get_suggestions(&provider, &[line.clone()], 0, col, false)
            .await
            .unwrap_or_else(|| panic!("expected result for {line}"));
        assert_eq!(result.prefix, "@REA");
        let applied = provider.apply_completion(&[line], 0, col, &result.items[0], &result.prefix);
        assert_eq!(applied.lines[0], format!("{before}@README.md "));
    }
    let embedded = "foo(@REA".to_string();
    let res = get_suggestions(
        &provider,
        &[embedded.clone()],
        0,
        pi_js::str16::len(&embedded) as i64,
        false,
    )
    .await;
    assert!(res.is_none());
}

#[tokio::test]
async fn fd_at_preserves_cjk_characters_and_embedded_at_in_attachment_paths() {
    let Some((_root, base_dir, _outside, fd_path)) = setup_fd_dirs() else {
        return;
    };
    setup_folder(
        &base_dir,
        &[],
        &[("文档/说明.md", "text"), ("文档@备份/说明.md", "backup")],
    );
    let provider = CombinedAutocompleteProvider::new(None, &base_dir, Some(&fd_path));

    for before in ["", "查看，"] {
        for directory in ["文档", "文档@备份"] {
            let prefix = format!("@{directory}/说");
            let line = format!("{before}{prefix}");
            let col = pi_js::str16::len(&line) as i64;
            let result = get_suggestions(&provider, &[line.clone()], 0, col, false)
                .await
                .unwrap_or_else(|| panic!("expected result for {line}"));
            assert_eq!(result.prefix, prefix);
            let values: Vec<String> = result.items.iter().map(|item| item.value.clone()).collect();
            assert_eq!(values, vec![format!("@{directory}/说明.md")]);
        }
    }
}

#[tokio::test]
async fn fd_at_completes_quoted_cjk_attachments_after_prose_without_losing_path_segments_or_quotes() {
    let Some((_root, base_dir, _outside, fd_path)) = setup_fd_dirs() else {
        return;
    };
    let provider = CombinedAutocompleteProvider::new(None, &base_dir, Some(&fd_path));

    for separator in [" ", "\u{3000}", "，", "。"] {
        let directory = format!("我的{separator}文档");
        let rel_file = format!("{directory}/说明.md");
        setup_folder(
            &base_dir,
            &[],
            &[(&rel_file, "text"), ("文档/说明.md", "not the quoted path")],
        );
        let line = format!("查看：@\"{directory}/说\"后文");
        let cursor_col = pi_js::str16::index_of(&line, "\"后文", 0).expect("marker should exist") as i64;
        let result = get_suggestions(&provider, &[line.clone()], 0, cursor_col, false)
            .await
            .expect("expected suggestions");
        assert_eq!(result.prefix, format!("@\"{directory}/说"));
        let values: Vec<String> = result.items.iter().map(|item| item.value.clone()).collect();
        assert_eq!(values, vec![format!("@\"{directory}/说明.md\"")]);
        let applied = provider.apply_completion(&[line], 0, cursor_col, &result.items[0], &result.prefix);
        assert_eq!(applied.lines[0], format!("查看：@\"{directory}/说明.md\" 后文"));
        assert_eq!(
            applied.cursor_col,
            pi_js::str16::len(&format!("查看：@\"{directory}/说明.md\" ")) as i64
        );
    }
}

#[tokio::test]
async fn fd_at_does_not_interpret_email_addresses_or_at_after_ascii_or_cjk_letters_as_attachment_prefixes() {
    let Some((_root, base_dir, _outside, fd_path)) = setup_fd_dirs() else {
        return;
    };
    setup_folder(&base_dir, &[], &[("README.md", "readme"), ("example.com", "text")]);
    let provider = CombinedAutocompleteProvider::new(None, &base_dir, Some(&fd_path));

    for before in [
        "user",
        "查看",
        "あ",
        "カ",
        "한",
        "ㄅ",
        "𠮷",
        "か\u{3099}",
        "禰\u{e0100}",
        "々",
        "Ａ",
    ] {
        for name in ["REA", "example.com"] {
            let line = format!("{before}@{name}");
            let col = pi_js::str16::len(&line) as i64;
            let res = get_suggestions(&provider, &[line.clone()], 0, col, false).await;
            assert!(res.is_none(), "expected null for {line}");
        }
    }
}

#[tokio::test]
async fn fd_at_matches_file_with_extension_in_query() {
    let Some((_root, base_dir, _outside, fd_path)) = setup_fd_dirs() else {
        return;
    };
    setup_folder(&base_dir, &[], &[("file.txt", "content")]);
    let provider = CombinedAutocompleteProvider::new(None, &base_dir, Some(&fd_path));
    let line = "@file.txt".to_string();
    let result = get_suggestions(&provider, &[line.clone()], 0, pi_js::str16::len(&line) as i64, false)
        .await
        .expect("expected suggestions");
    assert!(result.items.iter().any(|item| item.value == "@file.txt"));
}

#[tokio::test]
async fn fd_at_matches_regex_metacharacters_in_queries_literally() {
    let Some((_root, base_dir, _outside, fd_path)) = setup_fd_dirs() else {
        return;
    };
    setup_folder(
        &base_dir,
        &[],
        &[
            ("pages/[id].tsx", "x"),
            ("pages/index.tsx", "x"),
            ("pages/(group).ts", "x"),
        ],
    );
    let provider = CombinedAutocompleteProvider::new(None, &base_dir, Some(&fd_path));
    for (line, expected) in [
        ("@pages/[", "@pages/[id].tsx"),
        ("@pages/(", "@pages/(group).ts"),
        ("@[id", "@pages/[id].tsx"),
    ] {
        let line_str = line.to_string();
        let result = get_suggestions(&provider, &[line_str], 0, pi_js::str16::len(line) as i64, false).await;
        let values: Vec<String> = result
            .map(|r| r.items.into_iter().map(|i| i.value).collect())
            .unwrap_or_default();
        assert_eq!(values, vec![expected.to_string()], "{line}");
    }
}

#[tokio::test]
async fn fd_at_filters_are_case_insensitive() {
    let Some((_root, base_dir, _outside, fd_path)) = setup_fd_dirs() else {
        return;
    };
    setup_folder(&base_dir, &["src"], &[("README.md", "readme")]);
    let provider = CombinedAutocompleteProvider::new(None, &base_dir, Some(&fd_path));
    let line = "@re".to_string();
    let result = get_suggestions(&provider, &[line.clone()], 0, pi_js::str16::len(&line) as i64, false)
        .await
        .expect("expected suggestions");
    let mut values: Vec<String> = result.items.into_iter().map(|item| item.value).collect();
    values.sort();
    assert_eq!(values, vec!["@README.md"]);
}

#[tokio::test]
async fn fd_at_ranks_directories_before_files() {
    let Some((_root, base_dir, _outside, fd_path)) = setup_fd_dirs() else {
        return;
    };
    setup_folder(&base_dir, &["src"], &[("src.txt", "text")]);
    let provider = CombinedAutocompleteProvider::new(None, &base_dir, Some(&fd_path));
    let line = "@src".to_string();
    let result = get_suggestions(&provider, &[line.clone()], 0, pi_js::str16::len(&line) as i64, false)
        .await
        .expect("expected suggestions");
    assert_eq!(result.items.first().map(|i| i.value.as_str()), Some("@src/"));
    assert!(result.items.iter().any(|item| item.value == "@src.txt"));
}

#[tokio::test]
async fn fd_at_returns_nested_file_paths() {
    let Some((_root, base_dir, _outside, fd_path)) = setup_fd_dirs() else {
        return;
    };
    setup_folder(&base_dir, &[], &[("src/index.ts", "export {};\n")]);
    let provider = CombinedAutocompleteProvider::new(None, &base_dir, Some(&fd_path));
    let line = "@index".to_string();
    let result = get_suggestions(&provider, &[line.clone()], 0, pi_js::str16::len(&line) as i64, false)
        .await
        .expect("expected suggestions");
    assert!(result.items.iter().any(|item| item.value == "@src/index.ts"));
}

#[tokio::test]
async fn fd_at_matches_deeply_nested_paths() {
    let Some((_root, base_dir, _outside, fd_path)) = setup_fd_dirs() else {
        return;
    };
    setup_folder(
        &base_dir,
        &[],
        &[
            ("packages/tui/src/autocomplete.ts", "export {};"),
            ("packages/ai/src/autocomplete.ts", "export {};"),
        ],
    );
    let provider = CombinedAutocompleteProvider::new(None, &base_dir, Some(&fd_path));
    let line = "@tui/src/auto".to_string();
    let result = get_suggestions(&provider, &[line.clone()], 0, pi_js::str16::len(&line) as i64, false)
        .await
        .expect("expected suggestions");
    assert!(
        result
            .items
            .iter()
            .any(|item| item.value == "@packages/tui/src/autocomplete.ts")
    );
    assert!(
        !result
            .items
            .iter()
            .any(|item| item.value == "@packages/ai/src/autocomplete.ts")
    );
}

#[tokio::test]
async fn fd_at_matches_directory_in_middle_of_path_with_full_path() {
    let Some((_root, base_dir, _outside, fd_path)) = setup_fd_dirs() else {
        return;
    };
    setup_folder(
        &base_dir,
        &[],
        &[
            ("src/components/Button.tsx", "export {};"),
            ("src/utils/helpers.ts", "export {};"),
        ],
    );
    let provider = CombinedAutocompleteProvider::new(None, &base_dir, Some(&fd_path));
    let line = "@components/".to_string();
    let result = get_suggestions(&provider, &[line.clone()], 0, pi_js::str16::len(&line) as i64, false)
        .await
        .expect("expected suggestions");
    assert!(
        result
            .items
            .iter()
            .any(|item| item.value == "@src/components/Button.tsx")
    );
    assert!(!result.items.iter().any(|item| item.value == "@src/utils/helpers.ts"));
}

#[tokio::test]
async fn fd_at_scopes_fuzzy_search_to_relative_directories_and_searches_recursively() {
    let Some((_root, base_dir, outside_dir, fd_path)) = setup_fd_dirs() else {
        return;
    };
    setup_folder(
        &outside_dir,
        &[],
        &[
            ("nested/alpha.ts", "export {};"),
            ("nested/deeper/also-alpha.ts", "export {};"),
            ("nested/deeper/zzz.ts", "export {};"),
        ],
    );
    let provider = CombinedAutocompleteProvider::new(None, &base_dir, Some(&fd_path));
    let line = "@../outside/a".to_string();
    let result = get_suggestions(&provider, &[line.clone()], 0, pi_js::str16::len(&line) as i64, false)
        .await
        .expect("expected suggestions");
    assert!(
        result
            .items
            .iter()
            .any(|item| item.value == "@../outside/nested/alpha.ts")
    );
    assert!(
        result
            .items
            .iter()
            .any(|item| item.value == "@../outside/nested/deeper/also-alpha.ts")
    );
    assert!(
        !result
            .items
            .iter()
            .any(|item| item.value == "@../outside/nested/deeper/zzz.ts")
    );
}

#[tokio::test]
async fn fd_at_ranks_shallower_same_score_matches_before_deeper_matches() {
    let Some((_root, base_dir, _outside, fd_path)) = setup_fd_dirs() else {
        return;
    };
    setup_folder(
        &base_dir,
        &[
            "scope/aaa/venv/lib/python3.12/site-packages/pkg/core/profile",
            "scope/projects",
        ],
        &[],
    );
    let provider = CombinedAutocompleteProvider::new(None, &base_dir, Some(&fd_path));
    let line = "@scope/pro".to_string();
    let result = get_suggestions(&provider, &[line.clone()], 0, pi_js::str16::len(&line) as i64, false)
        .await
        .expect("expected suggestions");
    assert_eq!(result.items.first().map(|i| i.value.as_str()), Some("@scope/projects/"));
    assert!(
        result
            .items
            .iter()
            .any(|i| i.value == "@scope/aaa/venv/lib/python3.12/site-packages/pkg/core/profile/")
    );
}

#[tokio::test]
async fn fd_at_includes_scoped_direct_children_when_recursive_matches_are_flooded() {
    let Some((_root, base_dir, _outside, fd_path)) = setup_fd_dirs() else {
        return;
    };
    let flooded_owned: Vec<String> = (0..250)
        .map(|index| {
            format!(
                "scope/a{:03}/venv/lib/python3.12/site-packages/pkg/core/profile",
                index + 1
            )
        })
        .collect();
    let mut dirs: Vec<&str> = Vec::with_capacity(251);
    dirs.push("scope/projects");
    for d in &flooded_owned {
        dirs.push(d);
    }
    setup_folder(&base_dir, &dirs, &[]);

    let provider = CombinedAutocompleteProvider::new(None, &base_dir, Some(&fd_path));
    let line = "@scope/pro".to_string();
    let result = get_suggestions(&provider, &[line.clone()], 0, pi_js::str16::len(&line) as i64, false)
        .await
        .expect("expected suggestions");
    assert_eq!(result.items.first().map(|i| i.value.as_str()), Some("@scope/projects/"));
    assert!(
        result.items.iter().any(|i| i.value.contains("/profile/")),
        "Should keep deep fuzzy matches after direct children"
    );
}

#[tokio::test]
async fn fd_at_quotes_paths_containing_whitespace_or_cjk_punctuation() {
    let Some((_root, base_dir, _outside, fd_path)) = setup_fd_dirs() else {
        return;
    };
    let provider = CombinedAutocompleteProvider::new(None, &base_dir, Some(&fd_path));
    for separator in [" ", "\u{3000}", "，", "。"] {
        let directory = format!("my{separator}folder");
        let file = format!("{directory}/test.txt");
        setup_folder(&base_dir, &[], &[(&file, "content")]);
        let line = "@my".to_string();
        let col = pi_js::str16::len(&line) as i64;
        let result = get_suggestions(&provider, &[line.clone()], 0, col, false)
            .await
            .expect("expected suggestions");
        let expected_dir_val = format!("@\"{directory}/\"");
        let item = result
            .items
            .iter()
            .find(|entry| entry.value == expected_dir_val)
            .unwrap_or_else(|| panic!("expected {directory}"));
        let applied = provider.apply_completion(&[line], 0, col, item, &result.prefix);
        let continued = get_suggestions(&provider, &applied.lines, 0, applied.cursor_col, false)
            .await
            .expect("expected continued suggestions");
        assert_eq!(continued.prefix, format!("@\"{directory}/"));
        assert!(
            continued
                .items
                .iter()
                .any(|entry| entry.value == format!("@\"{directory}/test.txt\""))
        );
    }
}

#[tokio::test]
async fn fd_at_includes_hidden_paths_but_excludes_git() {
    let Some((_root, base_dir, _outside, fd_path)) = setup_fd_dirs() else {
        return;
    };
    setup_folder(
        &base_dir,
        &[".pi", ".github", ".git"],
        &[
            (".pi/config.json", "{}"),
            (".github/workflows/ci.yml", "name: ci"),
            (".git/config", "[core]"),
        ],
    );
    let provider = CombinedAutocompleteProvider::new(None, &base_dir, Some(&fd_path));
    let line = "@".to_string();
    let result = get_suggestions(&provider, &[line.clone()], 0, pi_js::str16::len(&line) as i64, false)
        .await
        .expect("expected suggestions");
    let values: Vec<String> = result.items.into_iter().map(|i| i.value).collect();
    assert!(values.iter().any(|v| v == "@.pi/"));
    assert!(values.iter().any(|v| v == "@.github/"));
    assert!(!values.iter().any(|v| v == "@.git" || v.starts_with("@.git/")));
}

#[tokio::test]
async fn fd_at_follows_symlinked_directories_for_fuzzy_search() {
    let Some((_root, base_dir, outside_dir, fd_path)) = setup_fd_dirs() else {
        return;
    };
    setup_folder(&base_dir, &[], &[("dir/some_file.txt", "real")]);
    setup_folder(&outside_dir, &[], &[("some_file.txt", "symlinked")]);
    let link_path = pi_js::path::join(&[&base_dir, "symlinked_dir"]);
    pi_js::fs::symlink("../outside", &link_path).expect("symlink dir");

    let provider = CombinedAutocompleteProvider::new(None, &base_dir, Some(&fd_path));
    let line = "@some".to_string();
    let result = get_suggestions(&provider, &[line.clone()], 0, pi_js::str16::len(&line) as i64, false)
        .await
        .expect("expected suggestions");
    assert!(result.items.iter().any(|i| i.value == "@dir/some_file.txt"));
    assert!(result.items.iter().any(|i| i.value == "@symlinked_dir/some_file.txt"));
}

#[tokio::test]
async fn fd_at_returns_symlinked_directories_when_matching_their_name() {
    let Some((_root, base_dir, outside_dir, fd_path)) = setup_fd_dirs() else {
        return;
    };
    setup_folder(&outside_dir, &[], &[("nested/file.txt", "symlinked")]);
    let link_path = pi_js::path::join(&[&base_dir, "symlinked_dir"]);
    pi_js::fs::symlink("../outside", &link_path).expect("symlink dir");

    let provider = CombinedAutocompleteProvider::new(None, &base_dir, Some(&fd_path));
    let line = "@symlinked".to_string();
    let result = get_suggestions(&provider, &[line.clone()], 0, pi_js::str16::len(&line) as i64, false)
        .await
        .expect("expected suggestions");
    assert!(result.items.iter().any(|i| i.value == "@symlinked_dir/"));
}

#[tokio::test]
async fn fd_at_returns_symlinked_files_without_requiring_type_l() {
    let Some((_root, base_dir, _outside, fd_path)) = setup_fd_dirs() else {
        return;
    };
    setup_folder(&base_dir, &[], &[("original.txt", "content")]);
    let link_path = pi_js::path::join(&[&base_dir, "link.txt"]);
    pi_js::fs::symlink("original.txt", &link_path).expect("symlink file");

    let provider = CombinedAutocompleteProvider::new(None, &base_dir, Some(&fd_path));
    let line = "@link".to_string();
    let result = get_suggestions(&provider, &[line.clone()], 0, pi_js::str16::len(&line) as i64, false)
        .await
        .expect("expected suggestions");
    assert!(result.items.iter().any(|i| i.value == "@link.txt"));
}

#[tokio::test]
async fn fd_at_returns_same_suggestions_when_cwd_path_contains_query() {
    let Some((root, _base_dir, _outside, fd_path)) = setup_fd_dirs() else {
        return;
    };
    let normal_base_dir = pi_js::path::join(&[root.path(), "cwd-normal"]);
    let query_in_path_base_dir = pi_js::path::join(&[root.path(), "cwd-plan-repro"]);
    pi_js::fs::mkdir(&normal_base_dir, true, None).expect("mkdir normal");
    pi_js::fs::mkdir(&query_in_path_base_dir, true, None).expect("mkdir repro");

    let dirs = ["packages/coding-agent/examples/extensions/plan-mode"];
    let files = [
        (
            "packages/coding-agent/examples/extensions/plan-mode/README.md",
            "readme",
        ),
        ("packages/tui/docs/plan.md", "plan"),
    ];
    setup_folder(&normal_base_dir, &dirs, &files);
    setup_folder(&query_in_path_base_dir, &dirs, &files);

    let query = "@plan".to_string();
    let col = pi_js::str16::len(&query) as i64;
    let normal_provider = CombinedAutocompleteProvider::new(None, &normal_base_dir, Some(&fd_path));
    let query_in_path_provider = CombinedAutocompleteProvider::new(None, &query_in_path_base_dir, Some(&fd_path));

    let normal_result = get_suggestions(&normal_provider, &[query.clone()], 0, col, false).await;
    let query_in_path_result = get_suggestions(&query_in_path_provider, &[query], 0, col, false).await;

    let normalize = |result: Option<AutocompleteSuggestions>| -> Vec<String> {
        let mut list: Vec<String> = result
            .map(|r| {
                r.items
                    .into_iter()
                    .map(|item| format!("{} :: {}", item.label, item.description.as_deref().unwrap_or("")))
                    .collect()
            })
            .unwrap_or_default();
        list.sort();
        list
    };

    let norm = normalize(normal_result);
    assert_eq!(normalize(query_in_path_result), norm);
    assert!(
        norm.iter()
            .any(|s| s == "plan-mode/ :: packages/coding-agent/examples/extensions/plan-mode")
    );
    assert!(norm.iter().any(|s| s == "plan.md :: packages/tui/docs/plan.md"));
}

#[tokio::test]
async fn fd_at_continues_autocomplete_inside_quoted_at_paths() {
    let Some((_root, base_dir, _outside, fd_path)) = setup_fd_dirs() else {
        return;
    };
    setup_folder(
        &base_dir,
        &[],
        &[("my folder/test.txt", "content"), ("my folder/other.txt", "content")],
    );
    let provider = CombinedAutocompleteProvider::new(None, &base_dir, Some(&fd_path));
    let line = "@\"my folder/\"".to_string();
    let result = get_suggestions(
        &provider,
        &[line.clone()],
        0,
        pi_js::str16::len(&line) as i64 - 1,
        false,
    )
    .await
    .expect("Should return suggestions for quoted folder path");
    assert!(result.items.iter().any(|i| i.value == "@\"my folder/test.txt\""));
    assert!(result.items.iter().any(|i| i.value == "@\"my folder/other.txt\""));
}

#[tokio::test]
async fn fd_at_applies_quoted_at_completion_without_duplicating_closing_quote() {
    let Some((_root, base_dir, _outside, fd_path)) = setup_fd_dirs() else {
        return;
    };
    setup_folder(&base_dir, &[], &[("my folder/test.txt", "content")]);
    let provider = CombinedAutocompleteProvider::new(None, &base_dir, Some(&fd_path));
    let line = "@\"my folder/te\"".to_string();
    let cursor_col = pi_js::str16::len(&line) as i64 - 1;
    let result = get_suggestions(&provider, &[line.clone()], 0, cursor_col, false)
        .await
        .expect("Should return suggestions for quoted @ path");
    let item = result
        .items
        .iter()
        .find(|entry| entry.value == "@\"my folder/test.txt\"")
        .expect("Should find test.txt suggestion");
    let applied = provider.apply_completion(&[line], 0, cursor_col, item, &result.prefix);
    assert_eq!(applied.lines[0], "@\"my folder/test.txt\" ");
}

#[tokio::test]
async fn dot_slash_completes_chinese_path_prefixes_after_whitespace_or_cjk_punctuation_on_tab() {
    let base_dir = TempTestDir::new("pi-autocomplete-");
    setup_folder(
        base_dir.path(),
        &[],
        &[("说明.md", "file"), ("文档/说明.md", "nested file")],
    );
    let provider = CombinedAutocompleteProvider::new(None, base_dir.path(), None);

    let mut completions = vec![
        ("说".to_string(), "说明.md".to_string()),
        ("文".to_string(), "文档/".to_string()),
        ("文档/说".to_string(), "文档/说明.md".to_string()),
        ("./文档/说".to_string(), "./文档/说明.md".to_string()),
    ];
    if pi_js::env::platform() != "win32" {
        completions.push((
            format!("{}/文档/说", base_dir.path()),
            format!("{}/文档/说明.md", base_dir.path()),
        ));
    }

    for separator in " \t\u{3000}\u{00a0}，：；。！？（「《".chars() {
        for (prefix, value) in &completions {
            let before = format!("查看𠮷{separator}");
            let line = format!("{before}{prefix} 后文");
            let cursor_col = (pi_js::str16::len(&before) + pi_js::str16::len(prefix)) as i64;
            let result = get_suggestions(&provider, &[line.clone()], 0, cursor_col, true)
                .await
                .unwrap_or_else(|| panic!("expected result for {line}"));
            assert_eq!(result.prefix, *prefix);
            let values: Vec<String> = result.items.iter().map(|i| i.value.clone()).collect();
            assert_eq!(values, vec![value.clone()]);
            let applied = provider.apply_completion(&[line], 0, cursor_col, &result.items[0], &result.prefix);
            assert_eq!(applied.lines[0], format!("{before}{value} 后文"));
            assert_eq!(
                applied.cursor_col,
                (pi_js::str16::len(&before) + pi_js::str16::len(value)) as i64
            );
        }
    }
}

#[tokio::test]
async fn dot_slash_treats_unquoted_separators_as_boundaries_even_when_literal_path_exists() {
    let base_dir = TempTestDir::new("pi-autocomplete-");
    setup_folder(base_dir.path(), &[], &[("归档/说明.md", "other")]);
    let provider = CombinedAutocompleteProvider::new(None, base_dir.path(), None);

    for separator in [" ", "\u{3000}", "，", "。"] {
        let directory = format!("资料{separator}归档");
        let file = format!("{directory}/说明.md");
        setup_folder(base_dir.path(), &[], &[(&file, "archive")]);

        for marker in ["", "@"] {
            let line = format!("{marker}{directory}/说");
            let col = pi_js::str16::len(&line) as i64;
            let result = get_suggestions(&provider, &[line.clone()], 0, col, true)
                .await
                .unwrap_or_else(|| panic!("expected result for {line}"));
            assert_eq!(result.prefix, "归档/说");
            let values: Vec<String> = result.items.iter().map(|i| i.value.clone()).collect();
            assert_eq!(values, vec!["归档/说明.md"]);
        }

        let quoted = format!("查看，\"{directory}/说\"后文");
        let cursor_col = pi_js::str16::index_of(&quoted, "\"后文", 0).expect("marker should exist") as i64;
        let result = get_suggestions(&provider, &[quoted.clone()], 0, cursor_col, true)
            .await
            .expect("expected suggestions");
        assert_eq!(result.prefix, format!("\"{directory}/说"));
        let values: Vec<String> = result.items.iter().map(|i| i.value.clone()).collect();
        assert_eq!(values, vec![format!("\"{directory}/说明.md\"")]);
        let applied = provider.apply_completion(&[quoted], 0, cursor_col, &result.items[0], &result.prefix);
        assert_eq!(applied.lines[0], format!("查看，\"{directory}/说明.md\"后文"));

        let missing = format!("查看，\"不存在{separator}归档/说");
        let missing_res = get_suggestions(
            &provider,
            &[missing.clone()],
            0,
            pi_js::str16::len(&missing) as i64,
            true,
        )
        .await;
        assert!(missing_res.is_none());
    }
}

#[tokio::test]
async fn dot_slash_handles_empty_prefix_after_whitespace_or_cjk_punctuation_consistently() {
    let base_dir = TempTestDir::new("pi-autocomplete-");
    setup_folder(base_dir.path(), &[], &[("说明.md", "text")]);
    let provider = CombinedAutocompleteProvider::new(None, base_dir.path(), None);

    for separator in [" ", "\t", "\u{3000}", "，", "。"] {
        for force in [false, true] {
            let line = format!("查看{separator}");
            let col = pi_js::str16::len(&line) as i64;
            let result = get_suggestions(&provider, &[line.clone()], 0, col, force)
                .await
                .unwrap_or_else(|| panic!("expected result for {line}"));
            assert_eq!(result.prefix, "");
            let values: Vec<String> = result.items.iter().map(|i| i.value.clone()).collect();
            assert_eq!(values, vec!["说明.md"]);
        }
    }
    assert!(
        get_suggestions(&provider, &[String::new()], 0, 0, false)
            .await
            .is_none()
    );
}

#[tokio::test]
async fn dot_slash_completes_paths_after_opening_wrappers_like_brackets_and_backticks() {
    let base_dir = TempTestDir::new("pi-autocomplete-");
    setup_folder(base_dir.path(), &[], &[("src/main.ts", "x")]);
    let provider = CombinedAutocompleteProvider::new(None, base_dir.path(), None);

    for wrapper in ["(", "[", "{", "<", "`", "((", "(`"] {
        for prefix in ["src/ma", "./src/ma"] {
            let before = format!("see {wrapper}");
            let line = format!("{before}{prefix}");
            let col = pi_js::str16::len(&line) as i64;
            let result = get_suggestions(&provider, &[line.clone()], 0, col, true)
                .await
                .unwrap_or_else(|| panic!("expected result for {line}"));
            assert_eq!(result.prefix, prefix);
            let value = prefix.replace("src/ma", "src/main.ts");
            let values: Vec<String> = result.items.iter().map(|i| i.value.clone()).collect();
            assert_eq!(values, vec![value.clone()]);
            let applied = provider.apply_completion(&[line], 0, col, &result.items[0], &result.prefix);
            assert_eq!(applied.lines[0], format!("{before}{value}"));
        }
    }
}

#[tokio::test]
async fn dot_slash_completes_quoted_paths_after_opening_wrappers() {
    let base_dir = TempTestDir::new("pi-autocomplete-");
    setup_folder(base_dir.path(), &[], &[("my dir/main.ts", "x")]);
    let provider = CombinedAutocompleteProvider::new(None, base_dir.path(), None);

    let line = "see (\"my dir/ma".to_string();
    let result = get_suggestions(&provider, &[line.clone()], 0, pi_js::str16::len(&line) as i64, true)
        .await
        .expect("expected suggestions");
    assert_eq!(result.prefix, "\"my dir/ma");
    let values: Vec<String> = result.items.iter().map(|i| i.value.clone()).collect();
    assert_eq!(values, vec!["\"my dir/main.ts\""]);
}

#[tokio::test]
async fn dot_slash_keeps_wrappers_that_are_closed_inside_the_path() {
    let base_dir = TempTestDir::new("pi-autocomplete-");
    setup_folder(
        base_dir.path(),
        &[],
        &[("[slug]/page.tsx", "x"), ("(group)/layout.tsx", "x")],
    );
    let provider = CombinedAutocompleteProvider::new(None, base_dir.path(), None);

    for (prefix, value) in [
        ("[slug]/pa", "[slug]/page.tsx"),
        ("(group)/la", "(group)/layout.tsx"),
        ("./[slug]/pa", "./[slug]/page.tsx"),
    ] {
        let line = format!("see {prefix}");
        let result = get_suggestions(&provider, &[line.clone()], 0, pi_js::str16::len(&line) as i64, true)
            .await
            .unwrap_or_else(|| panic!("expected result for {line}"));
        assert_eq!(result.prefix, prefix);
        let values: Vec<String> = result.items.iter().map(|i| i.value.clone()).collect();
        assert_eq!(values, vec![value.to_string()]);
    }
}

#[tokio::test]
async fn dot_slash_preserves_cjk_characters_in_unprefixed_tab_completions() {
    let base_dir = TempTestDir::new("pi-autocomplete-");
    setup_folder(base_dir.path(), &[], &[("文档/说明.md", "text")]);
    let provider = CombinedAutocompleteProvider::new(None, base_dir.path(), None);

    let line = "文档/说".to_string();
    let result = get_suggestions(&provider, &[line.clone()], 0, pi_js::str16::len(&line) as i64, true)
        .await
        .expect("expected suggestions");
    assert_eq!(result.prefix, line);
    let values: Vec<String> = result.items.iter().map(|i| i.value.clone()).collect();
    assert_eq!(values, vec!["文档/说明.md"]);
}

#[tokio::test]
async fn dot_slash_preserves_dot_slash_prefix_when_completing_paths() {
    let base_dir = TempTestDir::new("pi-autocomplete-");
    setup_folder(
        base_dir.path(),
        &[],
        &[("update.sh", "#!/bin/bash"), ("utils.ts", "export {};")],
    );
    let provider = CombinedAutocompleteProvider::new(None, base_dir.path(), None);

    let line = "./up".to_string();
    let result = get_suggestions(&provider, &[line.clone()], 0, pi_js::str16::len(&line) as i64, true)
        .await
        .expect("Should return suggestions for ./ path");
    assert!(result.items.iter().any(|i| i.value == "./update.sh"));
}

#[tokio::test]
async fn dot_slash_preserves_dot_slash_prefix_for_directory_completions() {
    let base_dir = TempTestDir::new("pi-autocomplete-");
    setup_folder(base_dir.path(), &["src"], &[("src/index.ts", "export {};")]);
    let provider = CombinedAutocompleteProvider::new(None, base_dir.path(), None);

    let line = "./sr".to_string();
    let result = get_suggestions(&provider, &[line.clone()], 0, pi_js::str16::len(&line) as i64, true)
        .await
        .expect("Should return suggestions for ./ directory path");
    assert!(result.items.iter().any(|i| i.value == "./src/"));
}

#[tokio::test]
async fn quoted_path_quotes_paths_containing_whitespace_or_cjk_punctuation_for_direct_completion() {
    let base_dir = TempTestDir::new("pi-autocomplete-");
    let provider = CombinedAutocompleteProvider::new(None, base_dir.path(), None);

    for separator in [" ", "\u{3000}", "，", "。"] {
        let directory = format!("my{separator}folder");
        let file = format!("{directory}/test.txt");
        setup_folder(base_dir.path(), &[], &[(&file, "content")]);
        let line = "my".to_string();
        let col = pi_js::str16::len(&line) as i64;
        let result = get_suggestions(&provider, &[line.clone()], 0, col, true)
            .await
            .expect("expected suggestions");
        let expected_dir = format!("\"{directory}/\"");
        let item = result
            .items
            .iter()
            .find(|entry| entry.value == expected_dir)
            .unwrap_or_else(|| panic!("expected {directory}"));
        let applied = provider.apply_completion(&[line], 0, col, item, &result.prefix);
        let continued = get_suggestions(&provider, &applied.lines, 0, applied.cursor_col, true)
            .await
            .expect("expected continued suggestions");
        assert_eq!(continued.prefix, format!("\"{directory}/"));
        let values: Vec<String> = continued.items.iter().map(|entry| entry.value.clone()).collect();
        assert_eq!(values, vec![format!("\"{directory}/test.txt\"")]);
    }
}

#[tokio::test]
async fn quoted_path_keeps_quoted_directories_before_files() {
    let base_dir = TempTestDir::new("pi-autocomplete-");
    setup_folder(base_dir.path(), &["z folder", "z，folder"], &[("a.txt", "text")]);
    let provider = CombinedAutocompleteProvider::new(None, base_dir.path(), None);
    let result = get_suggestions(&provider, &[String::new()], 0, 0, true)
        .await
        .expect("expected suggestions");
    let flags: Vec<bool> = result.items.iter().map(|item| item.label.ends_with('/')).collect();
    assert_eq!(flags, vec![true, true, false]);
}

#[tokio::test]
async fn quoted_path_continues_completion_inside_quoted_paths() {
    let base_dir = TempTestDir::new("pi-autocomplete-");
    setup_folder(
        base_dir.path(),
        &[],
        &[("my folder/test.txt", "content"), ("my folder/other.txt", "content")],
    );
    let provider = CombinedAutocompleteProvider::new(None, base_dir.path(), None);
    let line = "\"my folder/\"".to_string();
    let result = get_suggestions(&provider, &[line.clone()], 0, pi_js::str16::len(&line) as i64 - 1, true)
        .await
        .expect("Should return suggestions for quoted folder path");
    assert!(result.items.iter().any(|i| i.value == "\"my folder/test.txt\""));
    assert!(result.items.iter().any(|i| i.value == "\"my folder/other.txt\""));
}

#[tokio::test]
async fn quoted_path_applies_quoted_completion_without_duplicating_closing_quote() {
    let base_dir = TempTestDir::new("pi-autocomplete-");
    setup_folder(base_dir.path(), &[], &[("my folder/test.txt", "content")]);
    let provider = CombinedAutocompleteProvider::new(None, base_dir.path(), None);
    let line = "\"my folder/te\"".to_string();
    let cursor_col = pi_js::str16::len(&line) as i64 - 1;
    let result = get_suggestions(&provider, &[line.clone()], 0, cursor_col, true)
        .await
        .expect("Should return suggestions for quoted path");
    let item = result
        .items
        .iter()
        .find(|entry| entry.value == "\"my folder/test.txt\"")
        .expect("Should find test.txt suggestion");
    let applied = provider.apply_completion(&[line], 0, cursor_col, item, &result.prefix);
    assert_eq!(applied.lines[0], "\"my folder/test.txt\"");
}
