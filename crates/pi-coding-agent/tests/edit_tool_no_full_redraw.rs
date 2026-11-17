//! Port of packages/coding-agent/test/edit-tool-no-full-redraw.test.ts

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use pi_ai::{TextContent, UserContentBlock};
use pi_coding_agent::modes::interactive::components::diff::render_diff;
use pi_coding_agent::modes::interactive::components::tool_execution::{
    RenderShell, ToolExecutionComponent, ToolExecutionResult, ToolRenderers,
};
use pi_coding_agent::modes::interactive::theme::theme::{Theme, ThemeBg, ThemeColor, init_theme};
use pi_tui::components::box_::TuiBox;
use pi_tui::terminal::{Terminal, TerminalInputHandler, TerminalResizeHandler};
use pi_tui::{Component, Container, Spacer, TUI, Text, TuiMainScreen, strip_terminal_sequences};
use serde_json::json;
use serial_test::serial;

#[derive(Clone)]
struct FakeTerminal {
    writes: Arc<Mutex<Vec<String>>>,
}

impl FakeTerminal {
    fn new() -> Self {
        Self {
            writes: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn full_clear_count(&self) -> usize {
        self.writes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|w| w.contains("\x1b[2J\x1b[H\x1b[3J"))
            .count()
    }
}

#[async_trait]
impl Terminal for FakeTerminal {
    fn start(&self, _on_input: TerminalInputHandler, _on_resize: TerminalResizeHandler) {}
    fn stop(&self) {}
    async fn drain_input(&self, _max_ms: Option<i64>, _idle_ms: Option<i64>) {}
    fn write(&self, data: &str) {
        self.writes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(data.to_string());
    }
    fn columns(&self) -> i64 {
        80
    }
    fn rows(&self) -> i64 {
        24
    }
    fn kitty_protocol_active(&self) -> bool {
        true
    }
    fn move_by(&self, _lines: i64) {}
    fn hide_cursor(&self) {}
    fn show_cursor(&self) {}
    fn clear_line(&self) {}
    fn clear_from_cursor(&self) {}
    fn clear_screen(&self) {}
    fn set_title(&self, _title: &str) {}
    fn set_progress(&self, _active: bool) {}
}

async fn wait_for_render() {
    tokio::time::sleep(Duration::from_millis(25)).await;
}

async fn wait_for_rendered_text(get_render: impl Fn() -> String, expected_text: &str, on_retry: impl Fn()) -> String {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(2000);
    let mut last_render = String::new();
    while tokio::time::Instant::now() < deadline {
        on_retry();
        wait_for_render().await;
        last_render = get_render();
        if last_render.contains(expected_text) {
            return last_render;
        }
    }
    panic!("Timed out waiting for render to include {expected_text:?}. Last render:\n{last_render}");
}

fn build_large_diff() -> String {
    let targets = [50, 150, 250, 350, 450, 550, 650, 750, 850, 950];
    let mut hunks = Vec::with_capacity(targets.len());
    for ln in targets {
        hunks.push(format!(
            " {line_prev} line {prev}\n-{line_cur} line {ln}\n+{line_cur} line {ln} changed\n {line_next} line {next}",
            line_prev = ln - 1,
            prev = ln - 1,
            line_cur = ln,
            line_next = ln + 1,
            next = ln + 1,
        ));
    }
    hunks.join("\n     ...\n")
}

use pi_coding_agent::core::tools::edit::{
    EditDiffPreview, EditReplacement, compute_edits_diff, create_edit_tool_definition,
};
#[derive(Clone)]
struct MockEditCallState {
    box_comp: TuiBox,
    box_arc: Arc<dyn Component>,
    preview: Option<Result<String, String>>,
    preview_pending: bool,
    settled_error: bool,
}

fn edit_header_bg(
    preview: Option<&Result<String, String>>,
    settled_error: bool,
    theme: Theme,
) -> Arc<dyn Fn(&str) -> String + Send + Sync> {
    let token = match preview {
        Some(Ok(_)) => ThemeBg::ToolSuccessBg,
        Some(Err(_)) => ThemeBg::ToolErrorBg,
        None if settled_error => ThemeBg::ToolErrorBg,
        None => ThemeBg::ToolPendingBg,
    };
    Arc::new(move |text: &str| theme.bg(token, text))
}

fn rebuild_mock_edit_call_box(state: &MockEditCallState, path: &str, theme: &Theme, output_pad: i64) {
    state.box_comp.set_bg_fn(Some(edit_header_bg(
        state.preview.as_ref(),
        state.settled_error,
        theme.clone(),
    )));
    state.box_comp.set_padding_x(output_pad);
    state.box_comp.clear();
    let header = format!("{} {}", theme.fg(ThemeColor::ToolTitle, &theme.bold("edit")), path);
    state
        .box_comp
        .add_child(Arc::new(Text::new(Some(&header), Some(0), Some(0), None)));

    if let Some(ref preview) = state.preview {
        state.box_comp.add_child(Arc::new(Spacer::new(Some(1))));
        let body = match preview {
            Ok(diff) => render_diff(diff, None),
            Err(err) => theme.fg(ThemeColor::Error, err),
        };
        state
            .box_comp
            .add_child(Arc::new(Text::new(Some(&body), Some(0), Some(0), None)));
    }
    state.box_comp.invalidate();
}

fn mock_edit_renderers(preview_diff: Option<String>, preflight_error: Option<String>) -> ToolRenderers {
    let preview_diff = Arc::new(preview_diff);
    let preflight_error = Arc::new(preflight_error);
    ToolRenderers {
        render_shell: Some(RenderShell::Self_),
        render_call: Some(Arc::new(move |args, theme, ctx| {
            let path = args
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("file.txt")
                .to_string();
            let should_compute = {
                let mut guard = ctx.state.lock().unwrap_or_else(|e| e.into_inner());
                let state = if let Some(existing) = guard.as_mut().and_then(|b| b.downcast_mut::<MockEditCallState>()) {
                    existing
                } else {
                    let box_comp = TuiBox::new(Some(1), Some(1), None);
                    let box_arc: Arc<dyn Component> = Arc::new(box_comp.clone());
                    *guard = Some(Box::new(MockEditCallState {
                        box_comp,
                        box_arc,
                        preview: None,
                        preview_pending: false,
                        settled_error: false,
                    }));
                    match guard.as_mut().and_then(|b| b.downcast_mut::<MockEditCallState>()) {
                        Some(s) => s,
                        None => unreachable!(),
                    }
                };

                let trigger = ctx.args_complete && state.preview.is_none() && !state.preview_pending;
                if trigger {
                    state.preview_pending = true;
                }
                rebuild_mock_edit_call_box(state, &path, &theme, ctx.output_pad);
                (trigger, Arc::clone(&state.box_arc))
            };

            if should_compute.0 {
                let state_arc = Arc::clone(&ctx.state);
                let invalidate = Arc::clone(&ctx.invalidate);
                let pre_err = Arc::clone(&preflight_error);
                let pre_diff = Arc::clone(&preview_diff);
                if let Ok(handle) = tokio::runtime::Handle::try_current() {
                    handle.spawn(async move {
                        tokio::task::yield_now().await;
                        {
                            let mut guard = state_arc.lock().unwrap_or_else(|e| e.into_inner());
                            if let Some(state) = guard.as_mut().and_then(|b| b.downcast_mut::<MockEditCallState>()) {
                                state.preview_pending = false;
                                if let Some(ref err) = *pre_err {
                                    state.preview = Some(Err(err.clone()));
                                } else if let Some(ref diff) = *pre_diff {
                                    state.preview = Some(Ok(diff.clone()));
                                }
                            }
                        }
                        invalidate();
                    });
                }
            }

            should_compute.1
        })),
        render_result: Some(Arc::new(|result, _opts, theme, ctx| {
            let path = ctx
                .args
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("file.txt")
                .to_string();
            let result_diff = if !ctx.is_error {
                result
                    .details
                    .as_ref()
                    .and_then(|d| d.get("diff"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            } else {
                None
            };

            let preview_snapshot = {
                let mut guard = ctx.state.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(state) = guard.as_mut().and_then(|b| b.downcast_mut::<MockEditCallState>()) {
                    let mut changed = false;
                    if let Some(ref diff) = result_diff
                        && state.preview.as_ref() != Some(&Ok(diff.clone()))
                    {
                        state.preview = Some(Ok(diff.clone()));
                        state.preview_pending = false;
                        changed = true;
                    }
                    if state.settled_error != ctx.is_error {
                        state.settled_error = ctx.is_error;
                        changed = true;
                    }
                    if changed {
                        rebuild_mock_edit_call_box(state, &path, &theme, ctx.output_pad);
                    }
                    state.preview.clone()
                } else {
                    None
                }
            };

            let container = Container::new();
            if ctx.is_error {
                let err_text = result
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        UserContentBlock::Text(t) => Some(t.text.as_str()),
                        UserContentBlock::Image(_) => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let preview_err = preview_snapshot
                    .as_ref()
                    .and_then(|p| p.as_ref().err())
                    .map(String::as_str);
                if !err_text.is_empty() && preview_err != Some(err_text.as_str()) {
                    container.add_child(Arc::new(Spacer::new(Some(1))));
                    container.add_child(Arc::new(Text::new(
                        Some(&theme.fg(ThemeColor::Error, &err_text)),
                        Some(ctx.output_pad),
                        Some(0),
                        None,
                    )));
                }
            } else if let Some(diff) = result_diff {
                let preview_diff_str = preview_snapshot
                    .as_ref()
                    .and_then(|p| p.as_ref().ok())
                    .map(String::as_str);
                if preview_diff_str != Some(diff.as_str()) {
                    container.add_child(Arc::new(Spacer::new(Some(1))));
                    container.add_child(Arc::new(Text::new(
                        Some(&render_diff(&diff, None)),
                        Some(ctx.output_pad),
                        Some(0),
                        None,
                    )));
                }
            }
            Arc::new(container)
        })),
    }
}

#[tokio::test]
#[serial]
async fn renders_the_large_diff_in_call_preview_and_does_not_full_redraw_when_result_settles() {
    init_theme(Some("dark"), None);
    let diff = build_large_diff();
    let terminal = Arc::new(FakeTerminal::new());
    let tui = Arc::new(TuiMainScreen::new(
        Arc::clone(&terminal) as Arc<dyn Terminal>,
        None,
        None,
    ));
    let tui_trait: Arc<dyn TUI + Send + Sync> = tui.clone();

    let root = Container::new();
    for i in 0..200 {
        root.add_child(Arc::new(Text::new(
            Some(&format!("history {i}")),
            Some(0),
            Some(0),
            None,
        )));
    }

    let component = ToolExecutionComponent::new(
        "edit",
        "tool-call-1",
        json!({ "path": "large-edit.txt", "edits": [] }),
        None,
        Some(mock_edit_renderers(Some(diff.clone()), None)),
        Arc::clone(&tui_trait),
        ".",
    );
    root.add_child(Arc::new(component.clone()));
    tui.add_child(Arc::new(root));
    tui.start();
    wait_for_render().await;

    component.set_args_complete();
    tui.request_render(None);
    wait_for_render().await;
    wait_for_render().await;

    let call_only_render = wait_for_rendered_text(
        || strip_terminal_sequences(&component.render(80).join("\n")),
        "line 50 changed",
        || tui.request_render(Some(true)),
    )
    .await;
    assert!(call_only_render.contains("edit"));
    assert!(call_only_render.contains("line 950 changed"));

    let redraws_before_result = tui.full_redraws();
    let clears_before_result = terminal.full_clear_count();
    component.update_result(
        ToolExecutionResult {
            content: vec![UserContentBlock::Text(TextContent::new(
                "Successfully replaced 10 block(s) in large-edit.txt.",
            ))],
            details: Some(json!({ "diff": diff, "firstChangedLine": 50 })),
            is_error: false,
            duration_ms: None,
        },
        Some(false),
    );
    tui.request_render(None);
    wait_for_render().await;

    assert_eq!(tui.full_redraws(), redraws_before_result);
    assert_eq!(terminal.full_clear_count(), clears_before_result);

    let settled_render = strip_terminal_sequences(&component.render(80).join("\n"));
    assert!(settled_render.contains("line 50 changed"));
    assert!(settled_render.contains("line 950 changed"));
    assert!(!settled_render.contains("Successfully replaced"));

    tui.stop(None);
}

#[tokio::test]
#[serial]
async fn reconstructs_the_boxed_preview_from_a_settled_result_without_args_complete() {
    init_theme(Some("dark"), None);
    let diff = build_large_diff();
    let terminal = Arc::new(FakeTerminal::new());
    let tui = Arc::new(TuiMainScreen::new(terminal, None, None));
    let tui_trait: Arc<dyn TUI + Send + Sync> = tui.clone();

    let component = ToolExecutionComponent::new(
        "edit",
        "tool-call-replay",
        json!({ "path": "replay-edit.txt", "edits": [] }),
        None,
        Some(mock_edit_renderers(Some(diff.clone()), None)),
        tui_trait,
        ".",
    );
    tui.add_child(Arc::new(component.clone()));
    tui.start();
    wait_for_render().await;

    component.update_result(
        ToolExecutionResult {
            content: vec![UserContentBlock::Text(TextContent::new(
                "Successfully replaced 2 block(s) in replay-edit.txt.",
            ))],
            details: Some(json!({ "diff": diff, "firstChangedLine": 50 })),
            is_error: false,
            duration_ms: None,
        },
        Some(false),
    );
    wait_for_render().await;
    wait_for_render().await;

    let rendered = strip_terminal_sequences(&component.render(80).join("\n"));
    assert!(rendered.contains("line 50 changed"));
    assert!(rendered.contains("line 150 changed"));

    tui.stop(None);
}

#[tokio::test]
#[serial]
async fn shows_a_preflight_error_without_rendering_a_diff_when_edits_do_not_apply() {
    init_theme(Some("dark"), None);
    let terminal = Arc::new(FakeTerminal::new());
    let tui = Arc::new(TuiMainScreen::new(terminal, None, None));
    let tui_trait: Arc<dyn TUI + Send + Sync> = tui.clone();

    let component = ToolExecutionComponent::new(
        "edit",
        "tool-call-2",
        json!({
            "path": "missing-edit.txt",
            "edits": [{ "oldText": "does not exist", "newText": "replacement" }]
        }),
        None,
        Some(mock_edit_renderers(
            None,
            Some("Could not find exact text to replace in missing-edit.txt".to_string()),
        )),
        tui_trait,
        ".",
    );
    tui.add_child(Arc::new(component.clone()));
    tui.start();
    wait_for_render().await;

    component.set_args_complete();
    tui.request_render(None);
    wait_for_render().await;
    wait_for_render().await;

    let rendered = wait_for_rendered_text(
        || strip_terminal_sequences(&component.render(80).join("\n")),
        "Could not find",
        || tui.request_render(Some(true)),
    )
    .await;
    assert!(!rendered.contains("+1 "));
    assert!(!rendered.contains("-1 "));

    tui.stop(None);
}

#[tokio::test]
#[serial]
async fn real_edit_tool_definition_and_compute_edits_diff_settle_without_full_redraw() {
    init_theme(Some("dark"), None);
    let dir = tempfile::tempdir().expect("tempdir");
    let cwd = dir.path().to_string_lossy().into_owned();
    let file_path = dir.path().join("notes.txt");
    std::fs::write(&file_path, "line 1\nline 2 old\nline 3\n").expect("write notes.txt");

    let preview = compute_edits_diff(
        "notes.txt",
        &[EditReplacement {
            old_text: "line 2 old".to_string(),
            new_text: "line 2 new".to_string(),
        }],
        &cwd,
        None,
    )
    .await;
    let diff = match preview {
        EditDiffPreview::Success { diff, .. } => diff,
        EditDiffPreview::Error { error } => panic!("unexpected compute_edits_diff error: {error}"),
    };
    assert!(diff.contains("-2 line 2 old") && diff.contains("+2 line 2 new"));

    let edit_def = create_edit_tool_definition(&cwd, None);
    let terminal = Arc::new(FakeTerminal::new());
    let tui = Arc::new(TuiMainScreen::new(
        Arc::clone(&terminal) as Arc<dyn Terminal>,
        None,
        None,
    ));
    let tui_trait: Arc<dyn TUI + Send + Sync> = tui.clone();

    let component = ToolExecutionComponent::new(
        "edit",
        "real-edit-1",
        json!({
            "path": "notes.txt",
            "edits": [{ "oldText": "line 2 old", "newText": "line 2 new" }]
        }),
        None,
        Some(ToolRenderers::from_tool_definition(&edit_def)),
        tui_trait,
        &cwd,
    );
    tui.add_child(Arc::new(component.clone()));
    tui.start();
    wait_for_render().await;

    let redraws_before = tui.full_redraws();
    let clears_before = terminal.full_clear_count();
    component.update_result(
        ToolExecutionResult {
            content: vec![UserContentBlock::Text(TextContent::new(
                "Successfully replaced 1 block in notes.txt.",
            ))],
            details: Some(json!({ "diff": diff, "firstChangedLine": 2 })),
            is_error: false,
            duration_ms: None,
        },
        Some(false),
    );
    tui.request_render(None);
    wait_for_render().await;

    assert_eq!(tui.full_redraws(), redraws_before);
    assert_eq!(terminal.full_clear_count(), clears_before);
    let rendered = strip_terminal_sequences(&component.render(80).join("\n"));
    assert!(rendered.contains("notes.txt"), "rendered:\n{rendered}");
    assert!(rendered.contains("line 2 new"), "rendered:\n{rendered}");

    tui.stop(None);
}
