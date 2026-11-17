//! Port of packages/coding-agent/test/tool-execution-component.test.ts

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use pi_ai::{ImageContent, TextContent, UserContentBlock};
use pi_coding_agent::core::keybindings::KEYBINDINGS;
use pi_coding_agent::modes::interactive::components::tool_execution::{
    RenderShell, ToolExecutionComponent, ToolExecutionOptions, ToolExecutionResult, ToolRenderers, testing,
};
use pi_coding_agent::modes::interactive::theme::theme::init_theme;
use pi_tui::terminal::{Terminal, TerminalInputHandler, TerminalResizeHandler};
use pi_tui::tui::{
    OverlayHandle, OverlayOptions, QueryTerminalColorsOptions, TUI, TuiInputListener, TuiMode, TuiStopOptions,
};
use pi_tui::{
    Component, Container, ImageProtocol, KeybindingsManager, TUI_KEYBINDINGS, TerminalCapabilities,
    TerminalColorScheme, TerminalColors, Text, TuiMouseButton, TuiMouseEvent, TuiMouseEventResult, TuiMouseEventType,
    reset_capabilities_cache, set_capabilities, set_keybindings, strip_terminal_sequences,
};
use serde_json::json;
use serial_test::serial;

// Small 2x2 blue JPEG image
const TINY_JPEG: &str = "/9j/4AAQSkZJRgABAQAAAQABAAD/2wBDAAMCAgMCAgMDAwMEAwMEBQgFBQQEBQoHBwYIDAoMDAsKCwsNDhIQDQ4RDgsLEBYQERMUFRUVDA8XGBYUGBIUFRT/2wBDAQMEBAUEBQkFBQkUDQsNFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBT/wAARCAACAAIDAREAAhEBAxEB/8QAFAABAAAAAAAAAAAAAAAAAAAACf/EABQQAQAAAAAAAAAAAAAAAAAAAAD/xAAVAQEBAAAAAAAAAAAAAAAAAAAGCf/EABQRAQAAAAAAAAAAAAAAAAAAAAD/2gAMAwEAAhEDEQA/AD3VTB3/2Q==";

struct FakeTerminal;

#[async_trait]
impl Terminal for FakeTerminal {
    fn start(&self, _on_input: TerminalInputHandler, _on_resize: TerminalResizeHandler) {}
    fn stop(&self) {}
    async fn drain_input(&self, _max_ms: Option<i64>, _idle_ms: Option<i64>) {}
    fn write(&self, _data: &str) {}
    fn columns(&self) -> i64 {
        120
    }
    fn rows(&self) -> i64 {
        24
    }
    fn kitty_protocol_active(&self) -> bool {
        false
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

#[derive(Clone)]
struct FakeTui {
    inner: pi_tui::TuiMainScreen,
    render_requests: Arc<AtomicUsize>,
}

impl FakeTui {
    fn new() -> Self {
        Self {
            inner: pi_tui::TuiMainScreen::new(Arc::new(FakeTerminal), None, None),
            render_requests: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn request_count(&self) -> usize {
        self.render_requests.load(Ordering::SeqCst)
    }
}

impl Component for FakeTui {
    fn render(&self, width: usize) -> Vec<String> {
        self.inner.render(width)
    }
    fn handle_input(&self, data: &str) {
        self.inner.handle_input(data);
    }
    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.inner.handle_mouse(event)
    }
    fn wants_key_release(&self) -> bool {
        self.inner.wants_key_release()
    }
    fn invalidate(&self) {
        self.inner.invalidate();
    }
}

#[async_trait]
impl TUI for FakeTui {
    fn mode(&self) -> TuiMode {
        self.inner.mode()
    }
    fn children(&self) -> Vec<Arc<dyn Component>> {
        self.inner.children()
    }
    fn set_children(&self, children: Vec<Arc<dyn Component>>) {
        self.inner.set_children(children);
    }
    fn terminal(&self) -> Arc<dyn Terminal> {
        self.inner.terminal()
    }
    fn on_debug(&self) -> Option<Arc<dyn Fn() + Send + Sync>> {
        self.inner.on_debug()
    }
    fn set_on_debug(&self, on_debug: Option<Arc<dyn Fn() + Send + Sync>>) {
        self.inner.set_on_debug(on_debug);
    }
    fn full_redraws(&self) -> i64 {
        self.inner.full_redraws()
    }
    fn add_child(&self, component: Arc<dyn Component>) {
        self.inner.add_child(component);
    }
    fn remove_child(&self, component: &Arc<dyn Component>) {
        self.inner.remove_child(component);
    }
    fn clear(&self) {
        self.inner.clear();
    }
    fn get_show_hardware_cursor(&self) -> bool {
        self.inner.get_show_hardware_cursor()
    }
    fn set_show_hardware_cursor(&self, enabled: bool) {
        self.inner.set_show_hardware_cursor(enabled);
    }
    fn get_clear_on_shrink(&self) -> bool {
        self.inner.get_clear_on_shrink()
    }
    fn set_clear_on_shrink(&self, enabled: bool) {
        self.inner.set_clear_on_shrink(enabled);
    }
    fn set_focus(&self, component: Option<Arc<dyn Component>>) {
        self.inner.set_focus(component);
    }
    fn show_overlay(&self, component: Arc<dyn Component>, options: Option<OverlayOptions>) -> OverlayHandle {
        self.inner.show_overlay(component, options)
    }
    fn hide_overlay(&self) {
        self.inner.hide_overlay();
    }
    fn has_overlay(&self) -> bool {
        self.inner.has_overlay()
    }
    fn start(&self) {
        self.inner.start();
    }
    fn stop(&self, options: Option<TuiStopOptions>) {
        self.inner.stop(options);
    }
    fn render_now(&self, force: Option<bool>) {
        self.inner.render_now(force);
    }
    fn request_render(&self, _force: Option<bool>) {
        self.render_requests.fetch_add(1, Ordering::SeqCst);
    }
    fn add_input_listener(&self, listener: TuiInputListener) -> pi_js::Unsubscribe {
        self.inner.add_input_listener(listener)
    }
    fn remove_input_listener(&self, listener: &TuiInputListener) {
        self.inner.remove_input_listener(listener);
    }
    fn on_terminal_color_scheme_change(
        &self,
        listener: Arc<dyn Fn(TerminalColorScheme) + Send + Sync>,
    ) -> pi_js::Unsubscribe {
        self.inner.on_terminal_color_scheme_change(listener)
    }
    fn set_terminal_color_scheme_notifications(&self, enabled: bool) {
        self.inner.set_terminal_color_scheme_notifications(enabled);
    }
    async fn query_terminal_colors(&self, options: QueryTerminalColorsOptions) -> TerminalColors {
        self.inner.query_terminal_colors(options).await
    }
}

struct TestGuard;

impl Drop for TestGuard {
    fn drop(&mut self) {
        reset_capabilities_cache();
        testing::reset_png_transcoder_state();
        init_theme(Some("dark"), None);
        set_keybindings(KeybindingsManager::new(TUI_KEYBINDINGS.clone(), None));
    }
}

fn setup() -> (TestGuard, FakeTui, Arc<dyn TUI + Send + Sync>) {
    init_theme(Some("dark"), None);
    set_keybindings(KeybindingsManager::new(KEYBINDINGS.clone(), None));
    let fake = FakeTui::new();
    let ui: Arc<dyn TUI + Send + Sync> = Arc::new(fake.clone());
    (TestGuard, fake, ui)
}

fn text_result(text: &str, is_error: bool) -> ToolExecutionResult {
    ToolExecutionResult {
        content: vec![UserContentBlock::Text(pi_ai::TextContent::new(text))],
        details: None,
        is_error,
        duration_ms: None,
    }
}

// Issue #10292: the component loads the PNG transcoder itself, so this works in any TUI host.
// Issue #8577: a replaced partial image must not resurface.
#[tokio::test]
#[serial]
async fn converts_non_png_tool_images_once_the_transcoder_loads() {
    let (_g, _fake, ui) = setup();
    testing::reset_png_transcoder_state();
    set_capabilities(TerminalCapabilities {
        images: Some(ImageProtocol::Kitty),
        true_color: true,
        hyperlinks: true,
    });

    let component = ToolExecutionComponent::new("tool", "id", json!({}), None, None, ui, ".");
    component.update_result(
        ToolExecutionResult {
            content: vec![UserContentBlock::Image(ImageContent::new("cGFydGlhbA==", "image/jpeg"))],
            details: None,
            is_error: false,
            duration_ms: None,
        },
        Some(true),
    );
    component.update_result(
        ToolExecutionResult {
            content: vec![UserContentBlock::Image(ImageContent::new(TINY_JPEG, "image/jpeg"))],
            details: None,
            is_error: false,
            duration_ms: None,
        },
        Some(false),
    );

    let mut rendered = String::new();
    for _ in 0..20 {
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(5)).await;
        rendered = component.render(120).join("\n");
        if rendered.contains(";iVBORw0KGgo") {
            break;
        }
    }
    assert!(
        rendered.contains(";iVBORw0KGgo"),
        "expected converted PNG sequence, got: {rendered}"
    );
    assert!(!rendered.contains("cGFydGlhbA=="));

    // Invalidation reuses the converted Image, so the Kitty image ID stays the same.
    component.invalidate();
    assert_eq!(component.render(120).join("\n"), rendered);
}

#[test]
#[serial]
fn stacks_custom_call_and_result_renderers_like_the_old_implementation() {
    let (_g, _fake, ui) = setup();
    let renderers = ToolRenderers {
        render_shell: None,
        render_call: Some(Arc::new(|_, _, _| {
            Arc::new(Text::new(Some("custom call"), Some(0), Some(0), None))
        })),
        render_result: Some(Arc::new(|_, _, _, _| {
            Arc::new(Text::new(Some("custom result"), Some(0), Some(0), None))
        })),
    };

    let component = ToolExecutionComponent::new("custom_tool", "tool-1", json!({}), None, Some(renderers), ui, ".");
    assert!(strip_terminal_sequences(&component.render(120).join("\n")).contains("custom call"));

    component.update_result(text_result("done", false), Some(false));
    let rendered = strip_terminal_sequences(&component.render(120).join("\n"));
    assert!(rendered.contains("custom call"));
    assert!(rendered.contains("custom result"));
}

#[test]
#[serial]
fn self_rendered_empty_tool_rows_take_no_layout_space() {
    let (_g, _fake, ui) = setup();
    let renderers = ToolRenderers {
        render_shell: Some(RenderShell::Self_),
        render_call: Some(Arc::new(|_, _, _| {
            Arc::new(Text::new(Some(""), Some(0), Some(0), None))
        })),
        render_result: Some(Arc::new(|_, _, _, _| {
            Arc::new(Text::new(Some(""), Some(0), Some(0), None))
        })),
    };

    let component = ToolExecutionComponent::new(
        "custom_tool",
        "tool-empty-self-render",
        json!({}),
        None,
        Some(renderers),
        ui,
        ".",
    );
    assert_eq!(component.render(120), Vec::<String>::new());

    component.update_result(
        ToolExecutionResult {
            content: Vec::new(),
            details: Some(json!({})),
            is_error: false,
            duration_ms: None,
        },
        Some(false),
    );
    assert_eq!(component.render(120), Vec::<String>::new());
}

#[test]
#[serial]
fn preserves_legacy_file_path_rendering_compatibility_for_built_in_tools() {
    let (_g, _fake, ui) = setup();
    let component = ToolExecutionComponent::new(
        "read",
        "tool-3",
        json!({ "file_path": "README.md" }),
        None,
        None,
        ui,
        ".",
    );
    let rendered = strip_terminal_sequences(&component.render(120).join("\n"));
    assert!(rendered.contains("read"));
    assert!(rendered.contains("README.md"));
}

#[test]
#[serial]
fn inherits_missing_call_or_result_slot_from_fallbacks() {
    let (_g, _fake, ui) = setup();
    let call_only = ToolRenderers {
        render_shell: None,
        render_call: Some(Arc::new(|_, _, _| {
            Arc::new(Text::new(Some("override call"), Some(0), Some(0), None))
        })),
        render_result: None,
    };
    let comp1 = ToolExecutionComponent::new(
        "read",
        "tool-4b",
        json!({ "path": "notes.txt" }),
        None,
        Some(call_only),
        Arc::clone(&ui),
        ".",
    );
    comp1.update_result(text_result("hello", false), Some(false));
    let out1 = strip_terminal_sequences(&comp1.render(120).join("\n"));
    assert!(out1.contains("override call"));
    assert!(out1.contains("hello"));

    let result_only = ToolRenderers {
        render_shell: None,
        render_call: None,
        render_result: Some(Arc::new(|_, _, _, _| {
            Arc::new(Text::new(Some("override result"), Some(0), Some(0), None))
        })),
    };
    let comp2 = ToolExecutionComponent::new(
        "read",
        "tool-4c",
        json!({ "path": "README.md" }),
        None,
        Some(result_only),
        ui,
        ".",
    );
    comp2.update_result(text_result("hello", false), Some(false));
    let out2 = strip_terminal_sequences(&comp2.render(120).join("\n"));
    assert!(out2.contains("read"));
    assert!(out2.contains("README.md"));
    assert!(out2.contains("override result"));
}

#[test]
#[serial]
fn shares_renderer_state_and_exposes_args_duration_and_output_pad_in_context() {
    let (_g, _fake, ui) = setup();
    let renderers = ToolRenderers {
        render_shell: None,
        render_call: Some(Arc::new(|_args, _theme, ctx| {
            let mut state = ctx.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.is_none() {
                *state = Some(Box::new("shared-token".to_string()));
            }
            let tok = state
                .as_ref()
                .and_then(|b| b.downcast_ref::<String>())
                .cloned()
                .unwrap_or_default();
            Arc::new(Text::new(
                Some(&format!("custom call {tok} pad={}", ctx.output_pad)),
                Some(0),
                Some(0),
                None,
            ))
        })),
        render_result: Some(Arc::new(|_res, _opts, _theme, ctx| {
            let tok = ctx
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .and_then(|b| b.downcast_ref::<String>())
                .cloned()
                .unwrap_or_default();
            let foo = ctx.args.get("foo").and_then(|v| v.as_str()).unwrap_or("");
            let dur = ctx
                .duration_ms
                .map(|d| format!("{d:.0}"))
                .unwrap_or_else(|| "none".to_string());
            Arc::new(Text::new(
                Some(&format!(
                    "custom result {tok} arg:{foo} dur={dur} pad={}",
                    ctx.output_pad
                )),
                Some(0),
                Some(0),
                None,
            ))
        })),
    };

    let component = ToolExecutionComponent::new(
        "custom_tool",
        "tool-5",
        json!({ "foo": "bar" }),
        Some(ToolExecutionOptions {
            show_images: None,
            image_width_cells: None,
            output_pad: Some(2),
        }),
        Some(renderers),
        ui,
        ".",
    );

    component.update_result(
        ToolExecutionResult {
            content: vec![UserContentBlock::Text(TextContent::new("partial"))],
            details: None,
            is_error: false,
            duration_ms: Some(4200.0),
        },
        Some(true),
    );
    let partial_out = strip_terminal_sequences(&component.render(120).join("\n"));
    assert!(partial_out.contains("dur=none"), "partial_out={partial_out}");
    assert!(partial_out.contains("pad=2"), "partial_out={partial_out}");

    component.update_result(
        ToolExecutionResult {
            content: vec![UserContentBlock::Text(TextContent::new("done"))],
            details: None,
            is_error: false,
            duration_ms: Some(4200.0),
        },
        Some(false),
    );
    let final_out = strip_terminal_sequences(&component.render(120).join("\n"));
    assert!(final_out.contains("custom call shared-token pad=2"));
    assert!(final_out.contains("custom result shared-token arg:bar dur=4200 pad=2"));

    component.set_output_pad(0);
    let zero_pad_out = strip_terminal_sequences(&component.render(120).join("\n"));
    assert!(zero_pad_out.contains("pad=0"));
    assert!(
        component
            .render(120)
            .into_iter()
            .map(|l| strip_terminal_sequences(&l))
            .any(|l| l.starts_with("custom call shared-token"))
    );
}

#[test]
#[serial]
fn shows_arguments_in_the_fallback_call_header() {
    let (_g, _fake, ui) = setup();
    let long_value = "x".repeat(200);
    let component = ToolExecutionComponent::new(
        "custom_tool",
        "tool-args",
        json!({
            "query": "pi",
            "long": long_value,
            "text": "line one\nline two"
        }),
        None,
        Some(ToolRenderers::default()),
        ui,
        ".",
    );

    let collapsed = strip_terminal_sequences(&component.render(300).join("\n"));
    assert!(
        collapsed.contains("custom_tool query=\"pi\" long=\"xxx"),
        "collapsed={collapsed}"
    );
    assert!(collapsed.contains("..."));
    assert!(!collapsed.contains(&long_value));

    component.set_expanded(true);
    let expanded = strip_terminal_sequences(&component.render(300).join("\n"));
    assert!(expanded.contains("  query: pi"), "expanded={expanded}");
    assert!(expanded.contains(&long_value));
    let expanded_lines: Vec<String> = expanded.split('\n').map(|l| l.trim_end().to_string()).collect();
    let Some(text_line) = expanded_lines
        .iter()
        .position(|line| line.ends_with("  text: line one"))
    else {
        panic!("text line exists: {expanded}");
    };
    let next_line = &expanded_lines[text_line + 1];
    assert!(
        next_line.starts_with(' ') && next_line.ends_with("    line two"),
        "next_line={next_line:?}"
    );
}

#[test]
#[serial]
fn collapses_fallback_results_until_expanded_and_toggles_on_click() {
    let (_g, _fake, ui) = setup();
    let component = ToolExecutionComponent::new(
        "custom_tool",
        "tool-6",
        json!({ "foo": "bar" }),
        None,
        Some(ToolRenderers::default()),
        ui,
        ".",
    );
    let output = (1..=15)
        .map(|index| format!("line-{index}"))
        .collect::<Vec<_>>()
        .join("\n");
    component.update_result(text_result(&output, false), Some(false));

    let collapsed_lines = component.render(120);
    let collapsed = strip_terminal_sequences(&collapsed_lines.join("\n"));
    assert!(collapsed.contains("custom_tool"));
    assert!(collapsed.contains("line-10"));
    assert!(!collapsed.contains("line-11"));
    assert!(collapsed.contains("5 more lines"));
    assert!(collapsed.contains("to expand"));

    let Some(result_row) = collapsed_lines
        .iter()
        .position(|line| strip_terminal_sequences(line).contains("line-1"))
    else {
        panic!("result row exists");
    };
    let mouse_res = component.handle_mouse(&TuiMouseEvent {
        r#type: TuiMouseEventType::Click,
        button: TuiMouseButton::Left,
        x: 2,
        y: result_row as i64,
        screen_x: 2,
        screen_y: result_row as i64,
        width: 120,
        height: collapsed_lines.len() as i64,
        shift: false,
        alt: false,
        ctrl: false,
        wheel_delta: None,
        click_count: Some(1),
    });
    assert_eq!(mouse_res.and_then(|r| r.handled), Some(true));

    let expanded = strip_terminal_sequences(&component.render(120).join("\n"));
    assert!(expanded.contains("line-15"));
    assert!(!expanded.contains("more lines"));
}

// #9265: every argument delta re-ran the call renderer; a streamed `write` re-highlighted its whole content each time.
#[test]
#[serial]
fn renders_streamed_partial_arguments_once_per_frame_with_the_latest_arguments() {
    let (_g, fake, ui) = setup();
    let render_calls = Arc::new(AtomicUsize::new(0));

    let make_write = |counter: Option<Arc<AtomicUsize>>| ToolRenderers {
        render_shell: None,
        render_call: Some(Arc::new(move |args, _theme, _ctx| {
            if let Some(ref c) = counter {
                c.fetch_add(1, Ordering::SeqCst);
            }
            let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
            let line_count = content.split('\n').count().saturating_sub(1);
            Arc::new(Text::new(
                Some(&format!("write src/file.ts ({line_count} total)")),
                Some(0),
                Some(0),
                None,
            ))
        })),
        render_result: None,
    };
    let counted = make_write(Some(Arc::clone(&render_calls)));
    let uncounted = make_write(None);

    let component =
        ToolExecutionComponent::new("write", "tool-9", json!({}), None, Some(counted), Arc::clone(&ui), ".");
    let calls_before = render_calls.load(Ordering::SeqCst);
    let mut content = String::new();
    for index in 0..200 {
        content.push_str(&format!("const line{index} = {index};\n"));
        component.update_args(json!({ "path": "src/file.ts", "content": content }));
    }
    assert_eq!(render_calls.load(Ordering::SeqCst), calls_before);
    assert!(fake.request_count() > 0);

    let streamed = strip_terminal_sequences(&component.render(120).join("\n"));
    assert_eq!(render_calls.load(Ordering::SeqCst) - calls_before, 1);

    let direct = ToolExecutionComponent::new(
        "write",
        "tool-10",
        json!({ "path": "src/file.ts", "content": content }),
        None,
        Some(uncounted.clone()),
        Arc::clone(&ui),
        ".",
    );
    assert_eq!(streamed, strip_terminal_sequences(&direct.render(120).join("\n")));

    component.update_args(json!({ "path": "src/file.ts", "content": format!("{content}const tail = 1;\n") }));
    let final_args = json!({ "path": "src/file.ts", "content": format!("{content}const tail = 2;\n") });
    component.update_args(final_args.clone());
    let next = strip_terminal_sequences(&component.render(120).join("\n"));
    assert_eq!(render_calls.load(Ordering::SeqCst) - calls_before, 2);
    assert!(next.contains("201 total"), "next={next}");
    assert_eq!(
        next,
        strip_terminal_sequences(
            &ToolExecutionComponent::new("write", "t", final_args, None, Some(uncounted), ui, ".")
                .render(120)
                .join("\n")
        )
    );

    // Rendering again without new arguments does not run the renderer.
    component.render(120);
    assert_eq!(render_calls.load(Ordering::SeqCst) - calls_before, 2);
}

#[test]
#[serial]
fn self_rendered_mouse_forwarding_respects_header_spacer() {
    let (_g, _fake, ui) = setup();
    let clicked = Arc::new(AtomicUsize::new(0));
    let clicked_c = Arc::clone(&clicked);

    let renderers = ToolRenderers {
        render_shell: Some(RenderShell::Self_),
        render_call: Some(Arc::new(move |_, _, _| {
            let container = Container::new();
            let c = Arc::clone(&clicked_c);
            container.add_child(Arc::new(pi_tui::MouseRegion::new(
                Arc::new(Text::new(Some("self row"), Some(0), Some(0), None)),
                Arc::new(move |event| {
                    if event.r#type == TuiMouseEventType::Click {
                        c.fetch_add(1, Ordering::SeqCst);
                        return Some(TuiMouseEventResult {
                            handled: Some(true),
                            ..Default::default()
                        });
                    }
                    None
                }),
            )));
            Arc::new(container)
        })),
        render_result: None,
    };

    let component = ToolExecutionComponent::new("self_tool", "self-1", json!({}), None, Some(renderers), ui, ".");
    let lines = component.render(80);
    assert_eq!(lines.len(), 2); // leading "" spacer + "self row"

    // Row 0 is the spacer: ignored.
    assert!(
        component
            .handle_mouse(&TuiMouseEvent {
                r#type: TuiMouseEventType::Click,
                button: TuiMouseButton::Left,
                x: 1,
                y: 0,
                screen_x: 1,
                screen_y: 0,
                width: 80,
                height: 2,
                shift: false,
                alt: false,
                ctrl: false,
                wheel_delta: None,
                click_count: Some(1),
            })
            .is_none()
    );
    assert_eq!(clicked.load(Ordering::SeqCst), 0);

    // Row 1 forwards with y - 1 = 0 to self_render_container.
    let res = component.handle_mouse(&TuiMouseEvent {
        r#type: TuiMouseEventType::Click,
        button: TuiMouseButton::Left,
        x: 1,
        y: 1,
        screen_x: 1,
        screen_y: 1,
        width: 80,
        height: 2,
        shift: false,
        alt: false,
        ctrl: false,
        wheel_delta: None,
        click_count: Some(1),
    });
    assert_eq!(res.and_then(|r| r.handled), Some(true));
    assert_eq!(clicked.load(Ordering::SeqCst), 1);
}

#[test]
#[serial]
fn falls_back_cleanly_when_custom_renderers_panic() {
    let (_g, _fake, ui) = setup();
    let renderers = ToolRenderers {
        render_shell: None,
        render_call: Some(Arc::new(|_, _, _| panic!("boom call"))),
        render_result: Some(Arc::new(|_, _, _, _| panic!("boom result"))),
    };
    let component = ToolExecutionComponent::new(
        "broken_tool",
        "tool-panic",
        json!({ "a": 1 }),
        None,
        Some(renderers),
        ui,
        ".",
    );
    component.update_result(text_result("fallback output", false), Some(false));
    let rendered = strip_terminal_sequences(&component.render(120).join("\n"));
    assert!(rendered.contains("broken_tool a=1"));
    assert!(rendered.contains("fallback output"));
}

// WIRE(@tui-w3): port built-in bash/read/edit/write tool integration tests
// (`create_bash_tool_definition`, `create_read_tool_definition`, `create_write_tool_definition`,
// `with_built_in_renderers`) once codemode-w3 merges onto main.
