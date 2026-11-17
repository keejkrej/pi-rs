// PORT: QuickJS plugin runtime enabled per user requirement (overrides §10.6)
//! Port of packages/coding-agent/test/extensions-input-event.test.ts

use std::fs;

use pi_ai::types::ImageContent;
use pi_coding_agent::core::extensions::{
    ExtensionRunner, InputDeliveryBehavior, InputEventResult, InputSource, ModelRegistry, SessionManager,
    discover_and_load_extensions,
};

async fn create_runner(extensions: &[&str]) -> (tempfile::TempDir, ExtensionRunner) {
    let dir = tempfile::tempdir().expect("create tempdir");
    let root = dir.path();
    let ext_dir = root.join("extensions");
    fs::create_dir_all(&ext_dir).unwrap();
    for (i, code) in extensions.iter().enumerate() {
        fs::write(ext_dir.join(format!("e{i}.ts")), code).unwrap();
    }
    let root_str = root.to_str().unwrap();
    let result = discover_and_load_extensions(&[], root_str, Some(root_str), None).await;
    assert_eq!(result.errors, vec![], "unexpected load errors");
    assert_eq!(result.extensions.len(), extensions.len(), "extensions count mismatch");
    let runner = ExtensionRunner::new(
        result.extensions,
        result.runtime,
        root_str,
        SessionManager::default(),
        ModelRegistry::default(),
    );
    (dir, runner)
}

#[tokio::test]
async fn returns_continue_when_no_handlers_undefined_or_explicit_continue() {
    let (_d0, r0) = create_runner(&[]).await;
    assert_eq!(
        r0.emit_input("x", None, InputSource::Interactive, None).await,
        InputEventResult::Continue
    );

    let (_d1, r1) = create_runner(&[r#"export default p => p.on("input", async () => {});"#]).await;
    assert_eq!(
        r1.emit_input("x", None, InputSource::Interactive, None).await,
        InputEventResult::Continue
    );

    let (_d2, r2) =
        create_runner(&[r#"export default p => p.on("input", async () => ({ action: "continue" }));"#]).await;
    assert_eq!(
        r2.emit_input("x", None, InputSource::Interactive, None).await,
        InputEventResult::Continue
    );
}

#[tokio::test]
async fn transforms_text_and_preserves_images_when_omitted() {
    let (_d, r) = create_runner(&[
        r#"export default p => p.on("input", async e => ({ action: "transform", text: "T:" + e.text }));"#,
    ])
    .await;
    let imgs = vec![ImageContent {
        data: "orig".to_string(),
        mime_type: "image/png".to_string(),
    }];
    let res = r
        .emit_input("hi", Some(imgs.clone()), InputSource::Interactive, None)
        .await;
    assert_eq!(
        res,
        InputEventResult::Transform {
            text: "T:hi".to_string(),
            images: Some(imgs),
        }
    );
}

#[tokio::test]
async fn transforms_and_replaces_images_when_provided() {
    let (_d, r) = create_runner(&[
        r#"export default p => p.on("input", async () => ({ action: "transform", text: "X", images: [{ type: "image", data: "new", mimeType: "image/jpeg" }] }));"#,
    ])
    .await;
    let res = r
        .emit_input(
            "hi",
            Some(vec![ImageContent {
                data: "orig".to_string(),
                mime_type: "image/png".to_string(),
            }]),
            InputSource::Interactive,
            None,
        )
        .await;
    assert_eq!(
        res,
        InputEventResult::Transform {
            text: "X".to_string(),
            images: Some(vec![ImageContent {
                data: "new".to_string(),
                mime_type: "image/jpeg".to_string(),
            }]),
        }
    );
}

#[tokio::test]
async fn chains_transforms_across_multiple_handlers() {
    let (_d, r) = create_runner(&[
        r#"export default p => p.on("input", async e => ({ action: "transform", text: e.text + "[1]" }));"#,
        r#"export default p => p.on("input", async e => ({ action: "transform", text: e.text + "[2]" }));"#,
    ])
    .await;
    let res = r.emit_input("X", None, InputSource::Interactive, None).await;
    assert_eq!(
        res,
        InputEventResult::Transform {
            text: "X[1][2]".to_string(),
            images: None,
        }
    );
}

#[tokio::test]
async fn short_circuits_on_handled_and_skips_subsequent_handlers() {
    let dir = tempfile::tempdir().unwrap();
    let flag_file = dir.path().join("second-ran.txt");
    let flag_str = flag_file.to_string_lossy().replace('\\', "\\\\");
    let second_code = format!(
        r#"
        import * as fs from "node:fs";
        export default p => p.on("input", async () => {{
            fs.writeFileSync("{flag_str}", "ran");
        }});
        "#
    );
    let (_d, r) = create_runner(&[
        r#"export default p => p.on("input", async () => ({ action: "handled" }));"#,
        &second_code,
    ])
    .await;
    let res = r.emit_input("X", None, InputSource::Interactive, None).await;
    assert_eq!(res, InputEventResult::Handled);
    assert!(!flag_file.exists(), "second handler must not run after handled");
}

#[tokio::test]
async fn passes_source_and_streaming_behavior_correctly() {
    let (_d, r) = create_runner(&[
        r#"export default p => p.on("input", async e => ({ action: "transform", text: `${e.source}:${e.streamingBehavior ?? "none"}` }));"#,
    ])
    .await;
    for (src, name) in [
        (InputSource::Interactive, "interactive"),
        (InputSource::Rpc, "rpc"),
        (InputSource::Extension, "extension"),
    ] {
        let res = r.emit_input("x", None, src, None).await;
        assert_eq!(
            res,
            InputEventResult::Transform {
                text: format!("{name}:none"),
                images: None,
            }
        );
    }

    let steer = r
        .emit_input("x", None, InputSource::Interactive, Some(InputDeliveryBehavior::Steer))
        .await;
    assert_eq!(
        steer,
        InputEventResult::Transform {
            text: "interactive:steer".to_string(),
            images: None,
        }
    );

    let follow_up = r
        .emit_input(
            "x",
            None,
            InputSource::Interactive,
            Some(InputDeliveryBehavior::FollowUp),
        )
        .await;
    assert_eq!(
        follow_up,
        InputEventResult::Transform {
            text: "interactive:followUp".to_string(),
            images: None,
        }
    );
}

#[tokio::test]
async fn catches_handler_errors_and_continues_and_checks_has_handlers() {
    let (_d0, empty) = create_runner(&[]).await;
    assert!(!empty.has_handlers("input"));

    let (_d, r) =
        create_runner(&[r#"export default p => p.on("input", async () => { throw new Error("boom"); });"#]).await;
    assert!(r.has_handlers("input"));

    let errs = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let errs_clone = std::sync::Arc::clone(&errs);
    let _unsub = r.on_error_typed(std::sync::Arc::new(move |e| {
        errs_clone.lock().unwrap().push(e.error);
    }));

    let res = r.emit_input("x", None, InputSource::Interactive, None).await;
    assert_eq!(res, InputEventResult::Continue);
    let recorded = errs.lock().unwrap();
    assert!(recorded.iter().any(|m| m.contains("boom")), "errors: {recorded:?}");
}
