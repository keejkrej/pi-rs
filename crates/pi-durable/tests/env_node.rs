//! Port of packages/durable/test/env-node.test.ts

use std::sync::Arc;
use std::time::Duration;

use indexmap::IndexMap;
use pi_chord::context::{BACKGROUND_CONTEXT, with_abort_signal};
use pi_durable::env::node::{
    NodeExecutionEnv, NodeExecutionEnvOptions, NodeTextLineReader, kill_process_tree,
};
use pi_durable::env::{
    CreateDirOptions, CreateTempFileOptions, EnvCleanup, ExecutionErrorCode, FileContent,
    FileError, FileErrorCode, FileKind, FileSystem, ReadTextLinesOptions, RemoveOptions, Shell,
    ShellExecOptions, ShellExecResult, ShellSpillOptions, TextLine, get_or_throw,
};
use pi_js::abort::AbortController;
use pi_js::text::decode_utf8;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

#[derive(Default)]
struct CleanupTracker {
    dirs: Vec<TempDir>,
    extra_dirs: Vec<String>,
    restore_perms: Vec<String>,
}

impl CleanupTracker {
    fn temp_dir(&mut self) -> String {
        let dir = tempfile::Builder::new()
            .prefix("pi-durable-env-")
            .tempdir()
            .expect("create temp dir");
        let path = dir.path().to_string_lossy().into_owned();
        self.dirs.push(dir);
        path
    }
}

impl Drop for CleanupTracker {
    fn drop(&mut self) {
        for p in &self.restore_perms {
            let _ = pi_js::fs::chmod(p, 0o700);
        }
        for p in &self.extra_dirs {
            let _ = pi_js::fs::rm(p, true, true);
        }
    }
}

fn aborted_context() -> pi_chord::Context {
    let controller = AbortController::new();
    controller.abort(None);
    with_abort_signal(&BACKGROUND_CONTEXT, controller.signal())
}

async fn collect_shell_output(
    env: &NodeExecutionEnv,
    command: &str,
    options: Option<ShellExecOptions>,
    context: &pi_chord::Context,
) -> (
    std::result::Result<ShellExecResult, pi_durable::env::ExecutionError>,
    String,
) {
    let output = Arc::new(std::sync::Mutex::new(String::new()));
    let output_clone = Arc::clone(&output);
    let mut opts = options.unwrap_or_default();
    let prev = opts.on_output.take();
    opts.on_output = Some(Arc::new(move |text, ctx| {
        if let Some(prev_cb) = &prev {
            prev_cb(text, ctx)?;
        }
        output_clone
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push_str(text);
        Ok(())
    }));
    let res = env.exec(command, Some(opts), context).await;
    let collected = output.lock().unwrap_or_else(|e| e.into_inner()).clone();
    (res, collected)
}

fn to_bash_single_quoted_arg(value: &str) -> String {
    let replaced = value.replace('\\', "/").replace('\'', "'\"'\"'");
    format!("'{replaced}'")
}

#[tokio::test]
async fn reads_writes_lists_and_removes_files_and_directories() {
    let mut tracker = CleanupTracker::default();
    let root = tracker.temp_dir();
    let env = NodeExecutionEnv::new(&root);

    assert_eq!(
        get_or_throw(env.absolute_path("nested/child", &BACKGROUND_CONTEXT).await).unwrap(),
        pi_js::path::join(&[&root, "nested/child"])
    );
    assert_eq!(
        get_or_throw(env.join_path(&[&root, "nested", "child"], &BACKGROUND_CONTEXT).await).unwrap(),
        pi_js::path::join(&[&root, "nested", "child"])
    );

    get_or_throw(
        env.create_dir("nested/child", None, &BACKGROUND_CONTEXT)
            .await,
    )
    .unwrap();
    get_or_throw(
        env.write_file(
            "nested/child/file.txt",
            FileContent::from("hel"),
            &BACKGROUND_CONTEXT,
        )
        .await,
    )
    .unwrap();
    get_or_throw(
        env.append_file(
            "nested/child/file.txt",
            FileContent::from("lo"),
            &BACKGROUND_CONTEXT,
        )
        .await,
    )
    .unwrap();

    assert_eq!(
        get_or_throw(
            env.read_text_file("nested/child/file.txt", &BACKGROUND_CONTEXT)
                .await
        )
        .unwrap(),
        "hello"
    );
    assert_eq!(
        get_or_throw(
            env.read_text_lines(
                "nested/child/file.txt",
                Some(ReadTextLinesOptions { max_lines: Some(1) }),
                &BACKGROUND_CONTEXT,
            )
            .await
        )
        .unwrap(),
        vec!["hello".to_string()]
    );
    let bin = get_or_throw(
        env.read_binary_file("nested/child/file.txt", &BACKGROUND_CONTEXT)
            .await,
    )
    .unwrap();
    assert_eq!(decode_utf8(&bin), "hello");

    let entries = get_or_throw(env.list_dir("nested/child", &BACKGROUND_CONTEXT).await).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "file.txt");
    assert_eq!(entries[0].path, pi_js::path::join(&[&root, "nested/child/file.txt"]));
    assert_eq!(entries[0].kind, FileKind::File);
    assert_eq!(entries[0].size, 5);
    assert!(entries[0].mtime_ms.is_finite());

    assert!(get_or_throw(env.exists("nested/child/file.txt", &BACKGROUND_CONTEXT).await).unwrap());
    get_or_throw(
        env.remove("nested/child/file.txt", None, &BACKGROUND_CONTEXT)
            .await,
    )
    .unwrap();
    assert!(!get_or_throw(env.exists("nested/child/file.txt", &BACKGROUND_CONTEXT).await).unwrap());
}

#[tokio::test]
async fn expands_home_relative_paths_and_file_urls() {
    let mut tracker = CleanupTracker::default();
    let root = tracker.temp_dir();
    let env = NodeExecutionEnv::new(&root);

    assert_eq!(
        get_or_throw(env.absolute_path("~/pi-node-env-test", &BACKGROUND_CONTEXT).await).unwrap(),
        pi_js::path::join(&[&pi_js::env::home_dir(), "pi-node-env-test"])
    );
    let file_path = pi_js::path::join(&[&root, "file with spaces.txt"]);
    let file_url = url::Url::from_file_path(&file_path)
        .expect("file url")
        .to_string();
    assert_eq!(
        get_or_throw(env.absolute_path(&file_url, &BACKGROUND_CONTEXT).await).unwrap(),
        file_path
    );
}

#[tokio::test]
async fn returns_file_info_for_files_dirs_and_symlinks_without_following() {
    let mut tracker = CleanupTracker::default();
    let root = tracker.temp_dir();
    let env = NodeExecutionEnv::new(&root);

    get_or_throw(
        env.create_dir("dir", Some(CreateDirOptions { recursive: Some(true) }), &BACKGROUND_CONTEXT)
            .await,
    )
    .unwrap();
    get_or_throw(
        env.write_file(
            "dir/file.txt",
            FileContent::from("hello"),
            &BACKGROUND_CONTEXT,
        )
        .await,
    )
    .unwrap();
    pi_js::fs::promises::symlink(
        &pi_js::path::join(&[&root, "dir/file.txt"]),
        &pi_js::path::join(&[&root, "file-link"]),
    )
    .await
    .unwrap();
    pi_js::fs::promises::symlink(
        &pi_js::path::join(&[&root, "dir"]),
        &pi_js::path::join(&[&root, "dir-link"]),
    )
    .await
    .unwrap();

    let dir_info = get_or_throw(env.file_info("dir", &BACKGROUND_CONTEXT).await).unwrap();
    assert_eq!(dir_info.name, "dir");
    assert_eq!(dir_info.path, pi_js::path::join(&[&root, "dir"]));
    assert_eq!(dir_info.kind, FileKind::Directory);

    let file_info = get_or_throw(env.file_info("dir/file.txt", &BACKGROUND_CONTEXT).await).unwrap();
    assert_eq!(file_info.name, "file.txt");
    assert_eq!(file_info.kind, FileKind::File);
    assert_eq!(file_info.size, 5);

    let file_link_info = get_or_throw(env.file_info("file-link", &BACKGROUND_CONTEXT).await).unwrap();
    assert_eq!(file_link_info.kind, FileKind::Symlink);

    let dir_link_info = get_or_throw(env.file_info("dir-link", &BACKGROUND_CONTEXT).await).unwrap();
    assert_eq!(dir_link_info.kind, FileKind::Symlink);

    let canonical = get_or_throw(env.canonical_path("file-link", &BACKGROUND_CONTEXT).await).unwrap();
    let expected = pi_js::fs::promises::realpath(&pi_js::path::join(&[&root, "dir/file.txt"]))
        .await
        .unwrap();
    assert_eq!(canonical, expected);
}

#[tokio::test]
async fn lists_symlinks_as_symlinks() {
    let mut tracker = CleanupTracker::default();
    let root = tracker.temp_dir();
    let env = NodeExecutionEnv::new(&root);

    get_or_throw(
        env.write_file("target.txt", FileContent::from("hello"), &BACKGROUND_CONTEXT)
            .await,
    )
    .unwrap();
    pi_js::fs::promises::symlink(
        &pi_js::path::join(&[&root, "target.txt"]),
        &pi_js::path::join(&[&root, "link.txt"]),
    )
    .await
    .unwrap();

    let mut entries = get_or_throw(env.list_dir(".", &BACKGROUND_CONTEXT).await).unwrap();
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    assert_eq!(
        entries
            .into_iter()
            .map(|e| (e.name, e.kind))
            .collect::<Vec<_>>(),
        vec![
            ("link.txt".to_string(), FileKind::Symlink),
            ("target.txt".to_string(), FileKind::File),
        ]
    );
}

#[tokio::test]
async fn stops_reading_text_lines_at_requested_limit() {
    let mut tracker = CleanupTracker::default();
    let root = tracker.temp_dir();
    let env = NodeExecutionEnv::new(&root);
    get_or_throw(
        env.write_file(
            "file.txt",
            FileContent::from("one\ntwo\nthree"),
            &BACKGROUND_CONTEXT,
        )
        .await,
    )
    .unwrap();
    let lines = get_or_throw(
        env.read_text_lines(
            "file.txt",
            Some(ReadTextLinesOptions { max_lines: Some(1) }),
            &BACKGROUND_CONTEXT,
        )
        .await,
    )
    .unwrap();
    assert_eq!(lines, vec!["one".to_string()]);
}

#[tokio::test]
async fn returns_file_error_for_missing_paths_and_listing_non_dirs() {
    let mut tracker = CleanupTracker::default();
    let root = tracker.temp_dir();
    let env = NodeExecutionEnv::new(&root);

    let info_err = env
        .file_info("missing.txt", &BACKGROUND_CONTEXT)
        .await
        .unwrap_err();
    assert_eq!(info_err.name(), "FileError");
    assert_eq!(info_err.code, FileErrorCode::NotFound);
    assert_eq!(
        info_err.path.as_deref(),
        Some(pi_js::path::join(&[&root, "missing.txt"]).as_str())
    );
    assert!(!get_or_throw(env.exists("missing.txt", &BACKGROUND_CONTEXT).await).unwrap());

    get_or_throw(
        env.write_file("file.txt", FileContent::from("hello"), &BACKGROUND_CONTEXT)
            .await,
    )
    .unwrap();
    let list_err = env.list_dir("file.txt", &BACKGROUND_CONTEXT).await.unwrap_err();
    assert_eq!(list_err.code, FileErrorCode::NotDirectory);
}

#[tokio::test]
async fn appends_atomically_renames_creates_temp_and_honors_dir_remove_options() {
    let mut tracker = CleanupTracker::default();
    let root = tracker.temp_dir();
    let env = NodeExecutionEnv::new(&root);

    get_or_throw(
        env.append_file(
            "new/nested/file.txt",
            FileContent::from("a"),
            &BACKGROUND_CONTEXT,
        )
        .await,
    )
    .unwrap();
    get_or_throw(
        env.append_file(
            "new/nested/file.txt",
            FileContent::from("b"),
            &BACKGROUND_CONTEXT,
        )
        .await,
    )
    .unwrap();
    assert_eq!(
        get_or_throw(
            env.read_text_file("new/nested/file.txt", &BACKGROUND_CONTEXT)
                .await
        )
        .unwrap(),
        "ab"
    );

    get_or_throw(
        env.write_file("source.txt", FileContent::from("new"), &BACKGROUND_CONTEXT)
            .await,
    )
    .unwrap();
    get_or_throw(
        env.write_file("destination.txt", FileContent::from("old"), &BACKGROUND_CONTEXT)
            .await,
    )
    .unwrap();
    get_or_throw(
        env.rename_file("source.txt", "destination.txt", &BACKGROUND_CONTEXT)
            .await,
    )
    .unwrap();
    assert!(!get_or_throw(env.exists("source.txt", &BACKGROUND_CONTEXT).await).unwrap());
    assert_eq!(
        get_or_throw(env.read_text_file("destination.txt", &BACKGROUND_CONTEXT).await).unwrap(),
        "new"
    );

    let rename_err = env
        .rename_file("missing-source.txt", "destination.txt", &BACKGROUND_CONTEXT)
        .await
        .unwrap_err();
    assert_eq!(rename_err.code, FileErrorCode::NotFound);
    assert_eq!(
        rename_err.path.as_deref(),
        Some(pi_js::path::join(&[&root, "missing-source.txt"]).as_str())
    );
    assert_eq!(
        get_or_throw(env.read_text_file("destination.txt", &BACKGROUND_CONTEXT).await).unwrap(),
        "new"
    );

    let temp_dir = get_or_throw(
        env.create_temp_dir(Some("node-env-test-"), &BACKGROUND_CONTEXT)
            .await,
    )
    .unwrap();
    tracker.extra_dirs.push(temp_dir.clone());
    assert!(pi_js::fs::promises::access(&temp_dir, 0).await.is_ok());

    let temp_file = get_or_throw(
        env.create_temp_file(
            Some(CreateTempFileOptions {
                prefix: Some("prefix-".to_string()),
                suffix: Some(".txt".to_string()),
            }),
            &BACKGROUND_CONTEXT,
        )
        .await,
    )
    .unwrap();
    tracker
        .extra_dirs
        .push(pi_js::path::dirname(&temp_file));
    assert!(temp_file.ends_with(".txt"));
    assert_eq!(
        get_or_throw(env.read_text_file(&temp_file, &BACKGROUND_CONTEXT).await).unwrap(),
        ""
    );

    let create_non_rec = env
        .create_dir(
            "missing/child",
            Some(CreateDirOptions {
                recursive: Some(false),
            }),
            &BACKGROUND_CONTEXT,
        )
        .await
        .unwrap_err();
    assert_eq!(create_non_rec.code, FileErrorCode::NotFound);

    get_or_throw(
        env.write_file("dir/child/file.txt", FileContent::from("hello"), &BACKGROUND_CONTEXT)
            .await,
    )
    .unwrap();
    assert!(
        env.remove(
            "dir",
            Some(RemoveOptions {
                recursive: Some(false),
                force: None,
            }),
            &BACKGROUND_CONTEXT,
        )
        .await
        .is_err()
    );
    get_or_throw(
        env.remove(
            "dir",
            Some(RemoveOptions {
                recursive: Some(true),
                force: None,
            }),
            &BACKGROUND_CONTEXT,
        )
        .await,
    )
    .unwrap();
    assert!(!get_or_throw(env.exists("dir", &BACKGROUND_CONTEXT).await).unwrap());
    assert!(
        env.remove(
            "missing",
            Some(RemoveOptions {
                recursive: None,
                force: Some(false),
            }),
            &BACKGROUND_CONTEXT,
        )
        .await
        .is_err()
    );
    get_or_throw(
        env.remove(
            "missing",
            Some(RemoveOptions {
                recursive: None,
                force: Some(true),
            }),
            &BACKGROUND_CONTEXT,
        )
        .await,
    )
    .unwrap();
}

#[tokio::test]
async fn returns_aborted_without_side_effects_for_pre_aborted_file_ops() {
    let mut tracker = CleanupTracker::default();
    let root = tracker.temp_dir();
    let env = NodeExecutionEnv::new(&root);
    get_or_throw(
        env.write_file("file.txt", FileContent::from("hello"), &BACKGROUND_CONTEXT)
            .await,
    )
    .unwrap();
    let ctx = aborted_context();

    let codes = vec![
        env.read_text_file("file.txt", &ctx).await.unwrap_err().code,
        env.read_text_lines("file.txt", None, &ctx)
            .await
            .unwrap_err()
            .code,
        env.read_binary_file("file.txt", &ctx)
            .await
            .unwrap_err()
            .code,
        env.open_text_line_reader("file.txt", &ctx)
            .await
            .err()
            .expect("aborted")
            .code,
        env.write_file("other.txt", FileContent::from("hello"), &ctx)
            .await
            .unwrap_err()
            .code,
        env.append_file("file.txt", FileContent::from(" world"), &ctx)
            .await
            .unwrap_err()
            .code,
        env.truncate_file("file.txt", 1, &ctx).await.unwrap_err().code,
        env.flush_file("file.txt", &ctx).await.unwrap_err().code,
        env.rename_file("file.txt", "renamed.txt", &ctx)
            .await
            .unwrap_err()
            .code,
        env.file_info("file.txt", &ctx).await.unwrap_err().code,
        env.list_dir(".", &ctx).await.unwrap_err().code,
        env.canonical_path("file.txt", &ctx).await.unwrap_err().code,
        env.exists("file.txt", &ctx).await.unwrap_err().code,
        env.create_dir("dir", None, &ctx).await.unwrap_err().code,
        env.remove("file.txt", None, &ctx).await.unwrap_err().code,
        env.create_temp_dir(None, &ctx).await.unwrap_err().code,
        env.create_temp_file(None, &ctx).await.unwrap_err().code,
    ];
    for code in codes {
        assert_eq!(code, FileErrorCode::Aborted);
    }
    assert_eq!(
        get_or_throw(env.read_text_file("file.txt", &BACKGROUND_CONTEXT).await).unwrap(),
        "hello"
    );
    let list = get_or_throw(env.list_dir(".", &BACKGROUND_CONTEXT).await).unwrap();
    assert_eq!(
        list.into_iter().map(|e| e.name).collect::<Vec<_>>(),
        vec!["file.txt".to_string()]
    );
}

#[tokio::test]
async fn truncates_extends_and_flushes_files() {
    let mut tracker = CleanupTracker::default();
    let root = tracker.temp_dir();
    let env = NodeExecutionEnv::new(&root);

    let bytes = vec![0x61u8, 0xc3, 0xa9, 0x0a, 0x62, 0x0a];
    get_or_throw(
        env.write_file(
            "file.bin",
            FileContent::from(&bytes),
            &BACKGROUND_CONTEXT,
        )
        .await,
    )
    .unwrap();
    get_or_throw(env.truncate_file("file.bin", 2, &BACKGROUND_CONTEXT).await).unwrap();
    assert_eq!(
        get_or_throw(env.read_binary_file("file.bin", &BACKGROUND_CONTEXT).await).unwrap(),
        vec![0x61, 0xc3]
    );
    get_or_throw(env.truncate_file("file.bin", 4, &BACKGROUND_CONTEXT).await).unwrap();
    assert_eq!(
        get_or_throw(env.read_binary_file("file.bin", &BACKGROUND_CONTEXT).await).unwrap(),
        vec![0x61, 0xc3, 0, 0]
    );
    get_or_throw(env.truncate_file("file.bin", 0, &BACKGROUND_CONTEXT).await).unwrap();
    assert_eq!(
        get_or_throw(env.file_info("file.bin", &BACKGROUND_CONTEXT).await)
            .unwrap()
            .size,
        0
    );

    get_or_throw(
        env.write_file("file.txt", FileContent::from("hello"), &BACKGROUND_CONTEXT)
            .await,
    )
    .unwrap();
    for bad in [
        -1.0,
        1.5,
        f64::NAN,
        f64::INFINITY,
        (pi_js::num::MAX_SAFE_INTEGER + 1) as f64,
    ] {
        let err = env
            .truncate_file_number("file.txt", bad, &BACKGROUND_CONTEXT)
            .await
            .unwrap_err();
        assert_eq!(err.code, FileErrorCode::Invalid);
    }
    assert_eq!(
        get_or_throw(env.read_text_file("file.txt", &BACKGROUND_CONTEXT).await).unwrap(),
        "hello"
    );

    let missing_trunc = env
        .truncate_file("missing.txt", 0, &BACKGROUND_CONTEXT)
        .await
        .unwrap_err();
    assert_eq!(missing_trunc.code, FileErrorCode::NotFound);
    assert!(!get_or_throw(env.exists("missing.txt", &BACKGROUND_CONTEXT).await).unwrap());

    get_or_throw(env.flush_file("file.txt", &BACKGROUND_CONTEXT).await).unwrap();
    assert_eq!(
        get_or_throw(env.read_text_file("file.txt", &BACKGROUND_CONTEXT).await).unwrap(),
        "hello"
    );
    assert_eq!(
        env.flush_file("missing.txt", &BACKGROUND_CONTEXT)
            .await
            .unwrap_err()
            .code,
        FileErrorCode::NotFound
    );
    get_or_throw(env.create_dir("dir", None, &BACKGROUND_CONTEXT).await).unwrap();
    assert_eq!(
        env.flush_file("dir", &BACKGROUND_CONTEXT)
            .await
            .unwrap_err()
            .code,
        FileErrorCode::IsDirectory
    );

    EnvCleanup::cleanup(&env, &BACKGROUND_CONTEXT).await;
}

#[tokio::test]
async fn text_line_reader_handles_crlf_empty_multibyte_and_cancel() {
    let mut tracker = CleanupTracker::default();
    let root = tracker.temp_dir();
    let env = NodeExecutionEnv::new(&root);

    get_or_throw(
        env.write_file(
            "lines.txt",
            FileContent::from("one\r\n\ntwo\npartial"),
            &BACKGROUND_CONTEXT,
        )
        .await,
    )
    .unwrap();
    let mut reader = env
        .open_text_line_reader("lines.txt", &BACKGROUND_CONTEXT)
        .await
        .unwrap();
    let mut lines = Vec::new();
    while let Some(line) = reader.read_line(&BACKGROUND_CONTEXT).await.unwrap() {
        lines.push(line);
    }
    assert_eq!(
        lines,
        vec![
            TextLine {
                text: "one\r".to_string(),
                terminated: true
            },
            TextLine {
                text: String::new(),
                terminated: true
            },
            TextLine {
                text: "two".to_string(),
                terminated: true
            },
            TextLine {
                text: "partial".to_string(),
                terminated: false
            },
        ]
    );
    assert_eq!(reader.read_line(&BACKGROUND_CONTEXT).await.unwrap(), None);
    reader.close(&BACKGROUND_CONTEXT).await;
    reader.close(&BACKGROUND_CONTEXT).await;
    assert_eq!(
        reader.read_line(&BACKGROUND_CONTEXT).await.unwrap_err().code,
        FileErrorCode::Invalid
    );

    get_or_throw(
        env.write_file("empty.txt", FileContent::from(""), &BACKGROUND_CONTEXT)
            .await,
    )
    .unwrap();
    get_or_throw(
        env.write_file("newline.txt", FileContent::from("\n"), &BACKGROUND_CONTEXT)
            .await,
    )
    .unwrap();
    let mut empty = env
        .open_text_line_reader("empty.txt", &BACKGROUND_CONTEXT)
        .await
        .unwrap();
    assert_eq!(empty.read_line(&BACKGROUND_CONTEXT).await.unwrap(), None);
    empty.close(&BACKGROUND_CONTEXT).await;

    let mut nl = env
        .open_text_line_reader("newline.txt", &BACKGROUND_CONTEXT)
        .await
        .unwrap();
    assert_eq!(
        nl.read_line(&BACKGROUND_CONTEXT).await.unwrap(),
        Some(TextLine {
            text: String::new(),
            terminated: true
        })
    );
    assert_eq!(nl.read_line(&BACKGROUND_CONTEXT).await.unwrap(), None);
    nl.close(&BACKGROUND_CONTEXT).await;

    let first = format!("{}😀tail", "a".repeat(64 * 1024 - 2));
    let second = "é".repeat(100_000);
    get_or_throw(
        env.write_file(
            "large.txt",
            FileContent::from(&format!("{first}\n{second}")),
            &BACKGROUND_CONTEXT,
        )
        .await,
    )
    .unwrap();
    let mut large = env
        .open_text_line_reader("large.txt", &BACKGROUND_CONTEXT)
        .await
        .unwrap();
    assert_eq!(
        large.read_line(&BACKGROUND_CONTEXT).await.unwrap(),
        Some(TextLine {
            text: first,
            terminated: true
        })
    );
    assert_eq!(
        large.read_line(&BACKGROUND_CONTEXT).await.unwrap(),
        Some(TextLine {
            text: second,
            terminated: false
        })
    );
    assert_eq!(large.read_line(&BACKGROUND_CONTEXT).await.unwrap(), None);
    large.close(&BACKGROUND_CONTEXT).await;

    // Pending read cancelled mid-read preserves offset for retry.
    let path = pi_js::path::join(&[&root, "cancel.txt"]);
    get_or_throw(
        env.write_file(
            "cancel.txt",
            FileContent::from("one\ntwo\n"),
            &BACKGROUND_CONTEXT,
        )
        .await,
    )
    .unwrap();
    let _ = path;
    let mut reader_box = env
        .open_node_text_line_reader("cancel.txt", &BACKGROUND_CONTEXT)
        .await
        .unwrap();
    reader_box.set_read_delay_for_test(Some(150));
    let controller = AbortController::new();
    let ctx = with_abort_signal(&BACKGROUND_CONTEXT, controller.signal());
    let trigger = controller.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        trigger.abort(None);
    });
    assert_eq!(
        reader_box.read_line(&ctx).await.unwrap_err().code,
        FileErrorCode::Aborted
    );
    assert_eq!(
        reader_box.read_line(&BACKGROUND_CONTEXT).await.unwrap(),
        Some(TextLine {
            text: "one".to_string(),
            terminated: true
        })
    );
    assert_eq!(
        reader_box.read_line(&BACKGROUND_CONTEXT).await.unwrap(),
        Some(TextLine {
            text: "two".to_string(),
            terminated: true
        })
    );
    assert_eq!(
        reader_box.read_line(&BACKGROUND_CONTEXT).await.unwrap(),
        None
    );
    reader_box.close(&BACKGROUND_CONTEXT).await;
    let _ = std::any::type_name::<NodeTextLineReader>();

    assert_eq!(
        env.open_text_line_reader("missing.txt", &BACKGROUND_CONTEXT)
            .await
            .err()
            .unwrap()
            .code,
        FileErrorCode::NotFound
    );
}

#[tokio::test]
async fn shell_executes_env_overrides_cleanup_and_streams() {
    let mut tracker = CleanupTracker::default();
    let root = tracker.temp_dir();

    let mut env_overrides = IndexMap::new();
    env_overrides.insert("NODE_ENV_TEST".to_string(), "ok".to_string());
    let env = NodeExecutionEnv::new(&root);
    let (res, output) = collect_shell_output(
        &env,
        "printf '%s:%s' \"$PWD\" \"$NODE_ENV_TEST\"",
        Some(ShellExecOptions {
            env: Some(env_overrides),
            ..Default::default()
        }),
        &BACKGROUND_CONTEXT,
    )
    .await;
    assert_eq!(res.unwrap().exit_code, 0);
    let real_root = pi_js::fs::promises::realpath(&root).await.unwrap();
    assert_eq!(output, format!("{real_root}:ok"));

    let mut base_env = IndexMap::new();
    base_env.insert(
        "PI_SESSION_FILE".to_string(),
        "/stale/parent.jsonl".to_string(),
    );
    base_env.insert("PI_CODING_AGENT".to_string(), "true".to_string());
    base_env.insert(
        "PI_NODE_ENV_PRESERVED_TEST".to_string(),
        "preserved".to_string(),
    );
    let configured = NodeExecutionEnv::with_options(NodeExecutionEnvOptions {
        cwd: root.clone(),
        shell_path: None,
        shell_env: Some(base_env.clone()),
    });
    for (overrides, expected) in [
        (None, "x:/stale/parent.jsonl"),
        (
            Some({
                let mut m = IndexMap::new();
                m.insert("PI_SESSION_FILE".to_string(), String::new());
                m
            }),
            "x:",
        ),
        (
            Some({
                let mut m = IndexMap::new();
                m.insert(
                    "PI_SESSION_FILE".to_string(),
                    "/sessions/current.jsonl".to_string(),
                );
                m
            }),
            "x:/sessions/current.jsonl",
        ),
    ] {
        let (r, out) = collect_shell_output(
            &configured,
            "printf '%s:%s|%s|%s' \"${PI_SESSION_FILE+x}\" \"${PI_SESSION_FILE-}\" \"$PI_CODING_AGENT\" \"$PI_NODE_ENV_PRESERVED_TEST\"",
            Some(ShellExecOptions {
                env: overrides,
                ..Default::default()
            }),
            &BACKGROUND_CONTEXT,
        )
        .await;
        r.unwrap();
        assert_eq!(out, format!("{expected}|true|preserved"));
    }

    // Replace rather than inherit default env.
    let inherited_key = "PI_NODE_ENV_INHERITED_TEST";
    let explicit_key = "PI_NODE_ENV_EXPLICIT_TEST";
    pi_js::env::set_var(inherited_key, "host");
    let mut explicit = IndexMap::new();
    explicit.insert(explicit_key.to_string(), "explicit".to_string());
    let (r, out) = collect_shell_output(
        &configured,
        &format!(
            "printf '%s:%s:%s' \"${{{inherited_key}-}}\" \"${{PI_CODING_AGENT-}}\" \"${{{explicit_key}-}}\""
        ),
        Some(ShellExecOptions {
            inherit_env: Some(false),
            env: Some(explicit),
            ..Default::default()
        }),
        &BACKGROUND_CONTEXT,
    )
    .await;
    pi_js::env::remove_var(inherited_key);
    r.unwrap();
    assert_eq!(out, "::explicit");

    // Combined stdout & stderr + split UTF-8 bytes
    let (r, out) = collect_shell_output(&env, "printf out; printf err >&2", None, &BACKGROUND_CONTEXT).await;
    assert_eq!(r.unwrap().exit_code, 0);
    assert!(out.contains("out"));
    assert!(out.contains("err"));

    // Split multi-byte UTF-8 across writes
    let (r, out) = collect_shell_output(
        &env,
        "printf '\\360\\237'; sleep 0.05; printf '\\230\\200'",
        None,
        &BACKGROUND_CONTEXT,
    )
    .await;
    assert_eq!(r.unwrap().exit_code, 0);
    assert_eq!(out, "😀");
}

#[tokio::test]
async fn shell_handles_errors_signals_timeouts_aborts_and_cleanup() {
    let mut tracker = CleanupTracker::default();
    let root = tracker.temp_dir();
    let env = NodeExecutionEnv::new(&root);

    let missing_cwd = NodeExecutionEnv::new(pi_js::path::join(&[&root, "missing"]));
    let err = missing_cwd
        .exec("printf ok", None, &BACKGROUND_CONTEXT)
        .await
        .unwrap_err();
    assert_eq!(err.code, ExecutionErrorCode::SpawnError);
    assert!(err.message.contains("Working directory does not exist"));

    let exit_res = env.exec("exit 7", None, &BACKGROUND_CONTEXT).await.unwrap();
    assert_eq!(exit_res.exit_code, 7);

    #[cfg(unix)]
    {
        let killed = env
            .exec("kill -9 $$", None, &BACKGROUND_CONTEXT)
            .await
            .unwrap();
        assert_eq!(killed.exit_code, 128 + 9);
    }

    let timeout_err = env
        .exec(
            "sleep 5",
            Some(ShellExecOptions {
                timeout: Some(0.01),
                ..Default::default()
            }),
            &BACKGROUND_CONTEXT,
        )
        .await
        .unwrap_err();
    assert_eq!(timeout_err.code, ExecutionErrorCode::Timeout);

    for bad_timeout in [0.0, -1.0, f64::NAN, f64::INFINITY, 2_147_484.0] {
        let err = env
            .exec(
                "touch spawned",
                Some(ShellExecOptions {
                    timeout: Some(bad_timeout),
                    ..Default::default()
                }),
                &BACKGROUND_CONTEXT,
            )
            .await
            .unwrap_err();
        assert_eq!(err.code, ExecutionErrorCode::Timeout);
        assert!(err.message.contains("Invalid timeout"));
    }
    assert!(!get_or_throw(env.exists("spawned", &BACKGROUND_CONTEXT).await).unwrap());

    let cb_err = env
        .exec(
            "printf out",
            Some(ShellExecOptions {
                on_output: Some(Arc::new(|_, _| Err(pi_js::Error::msg("callback failed")))),
                ..Default::default()
            }),
            &BACKGROUND_CONTEXT,
        )
        .await
        .unwrap_err();
    assert_eq!(cb_err.code, ExecutionErrorCode::CallbackError);
    assert_eq!(cb_err.message, "callback failed");

    let missing_shell = NodeExecutionEnv::with_options(NodeExecutionEnvOptions {
        cwd: root.clone(),
        shell_path: Some(pi_js::path::join(&[&root, "missing-shell"])),
        shell_env: None,
    });
    assert_eq!(
        missing_shell
            .exec("printf ok", None, &BACKGROUND_CONTEXT)
            .await
            .unwrap_err()
            .code,
        ExecutionErrorCode::ShellUnavailable
    );

    let not_exec = pi_js::path::join(&[&root, "not-executable-shell"]);
    get_or_throw(
        env.write_file(&not_exec, FileContent::from("not executable"), &BACKGROUND_CONTEXT)
            .await,
    )
    .unwrap();
    let bad_shell = NodeExecutionEnv::with_options(NodeExecutionEnvOptions {
        cwd: root.clone(),
        shell_path: Some(not_exec),
        shell_env: None,
    });
    assert_eq!(
        bad_shell
            .exec("printf ok", None, &BACKGROUND_CONTEXT)
            .await
            .unwrap_err()
            .code,
        ExecutionErrorCode::SpawnError
    );

    assert_eq!(
        env.exec("touch spawned", None, &aborted_context())
            .await
            .unwrap_err()
            .code,
        ExecutionErrorCode::Aborted
    );
    assert!(!get_or_throw(env.exists("spawned", &BACKGROUND_CONTEXT).await).unwrap());

    let controller = AbortController::new();
    let ctx = with_abort_signal(&BACKGROUND_CONTEXT, controller.signal());
    let fut = env.exec("sleep 5", None, &ctx);
    controller.abort(None);
    assert_eq!(fut.await.unwrap_err().code, ExecutionErrorCode::Aborted);

    // Cleanup terminates active shell processes.
    let cleanup_env = Arc::new(NodeExecutionEnv::new(&root));
    let exec_handle = {
        let e = Arc::clone(&cleanup_env);
        tokio::spawn(async move { e.exec("touch started; sleep 60", None, &BACKGROUND_CONTEXT).await })
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !cleanup_env
        .exists("started", &BACKGROUND_CONTEXT)
        .await
        .unwrap_or(false)
    {
        assert!(tokio::time::Instant::now() < deadline, "started file never created");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    EnvCleanup::cleanup(&*cleanup_env, &BACKGROUND_CONTEXT).await;
    let joined = tokio::time::timeout(Duration::from_secs(3), exec_handle)
        .await
        .expect("cleanup timeout")
        .expect("join ok");
    assert!(joined.is_ok());

    let _ = to_bash_single_quoted_arg("test");
    let _ = kill_process_tree as fn(u32);
}

#[tokio::test]
async fn shell_spills_large_and_timed_out_outputs_cleanly() {
    let mut tracker = CleanupTracker::default();
    let root = tracker.temp_dir();
    let env = NodeExecutionEnv::new(&root);

    let short = env
        .exec(
            "printf short",
            Some(ShellExecOptions {
                spill: Some(ShellSpillOptions {
                    after_bytes: 100,
                    after_lines: 10,
                }),
                ..Default::default()
            }),
            &BACKGROUND_CONTEXT,
        )
        .await
        .unwrap();
    assert_eq!(short.spill_path, None);

    // Raw invalid UTF-8 bytes preserved in spill while decoded stream still completes.
    let raw_spill = env
        .exec(
            "printf '\\146\\200\\000\\157'",
            Some(ShellExecOptions {
                spill: Some(ShellSpillOptions {
                    after_bytes: 1,
                    after_lines: 10,
                }),
                ..Default::default()
            }),
            &BACKGROUND_CONTEXT,
        )
        .await
        .unwrap();
    let spill_path = raw_spill.spill_path.expect("spill path");
    tracker.extra_dirs.push(pi_js::path::dirname(&spill_path));
    assert_eq!(
        get_or_throw(env.read_binary_file(&spill_path, &BACKGROUND_CONTEXT).await).unwrap(),
        vec![0x66, 0x80, 0x00, 0x6f]
    );

    // Timeout spill report
    let timed = env
        .exec(
            "printf 12345678901234567890; sleep 5",
            Some(ShellExecOptions {
                timeout: Some(0.3),
                spill: Some(ShellSpillOptions {
                    after_bytes: 10,
                    after_lines: 10,
                }),
                ..Default::default()
            }),
            &BACKGROUND_CONTEXT,
        )
        .await
        .unwrap_err();
    assert_eq!(timed.code, ExecutionErrorCode::Timeout);
    let timed_spill = timed.spill_path.expect("timeout spill path");
    tracker.extra_dirs.push(pi_js::path::dirname(&timed_spill));
    assert_eq!(
        get_or_throw(env.read_text_file(&timed_spill, &BACKGROUND_CONTEXT).await).unwrap(),
        "12345678901234567890"
    );

    // Failing temp file creation reports spill failure instead of losing output.
    let bad_root = root.clone();
    let failing_env = NodeExecutionEnv::new(&root).with_temp_file_hook(Arc::new(move |opts| {
        if opts.and_then(|o| o.prefix.as_deref()) == Some("pi-output-") {
            Some(Ok(pi_js::path::join(&[&bad_root, "missing", "spill.log"])))
        } else {
            None
        }
    }));
    let fail_err = failing_env
        .exec(
            "printf 12345678901234567890",
            Some(ShellExecOptions {
                spill: Some(ShellSpillOptions {
                    after_bytes: 10,
                    after_lines: 10,
                }),
                ..Default::default()
            }),
            &BACKGROUND_CONTEXT,
        )
        .await
        .unwrap_err();
    assert_eq!(fail_err.code, ExecutionErrorCode::Unknown);
    assert!(fail_err
        .message
        .contains("Failed to preserve complete shell output"));

    // Large output + many lines spill
    let (res, out) = collect_shell_output(
        &env,
        "i=1; while [ $i -le 5000 ]; do echo line-$i; i=$((i+1)); done",
        Some(ShellExecOptions {
            spill: Some(ShellSpillOptions {
                after_bytes: 1024 * 1024,
                after_lines: 100,
            }),
            ..Default::default()
        }),
        &BACKGROUND_CONTEXT,
    )
    .await;
    let ok_res = res.unwrap();
    let expected_out: String = (1..=5000).map(|i| format!("line-{i}\n")).collect();
    assert_eq!(out, expected_out);
    let line_spill = ok_res.spill_path.expect("line spill");
    tracker.extra_dirs.push(pi_js::path::dirname(&line_spill));
    let spilled_lines = get_or_throw(env.read_text_lines(&line_spill, None, &BACKGROUND_CONTEXT).await).unwrap();
    assert_eq!(spilled_lines.len(), 5000);
    assert_eq!(spilled_lines.last().map(String::as_str), Some("line-5000"));
}
