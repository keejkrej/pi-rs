//! Port of packages/coding-agent/examples/extensions/border-status-editor.ts

use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_coding_agent::core::extensions::types::{
    AgentSettledEvent, AgentStartEvent, ExecOptions, Extension, ExtensionApi, ExtensionContext, ExtensionFactory,
    InlineExtension, SessionShutdownEvent, SessionStartEvent,
};
use pi_tui::autocomplete::AutocompleteProvider;
use pi_tui::editor_component::{EditorBorderColor, EditorComponent, EditorTextCallback};
use pi_tui::tui::{TuiMouseEvent, TuiMouseEventResult};
use pi_tui::{Component, Editor, EditorOptions, Focusable, TUI, truncate_to_width, visible_width};

const SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

fn fit_border(left: &str, right: &str, width: usize, border: &dyn Fn(&str) -> String) -> String {
    if width == 0 {
        return String::new();
    }
    if width == 1 {
        return border("─");
    }

    let mut left_text = left.to_string();
    let mut right_text = right.to_string();
    let fixed_width: i64 = 2;
    let minimum_gap: i64 = 3;
    let target_width = width as i64;

    loop {
        let rw = visible_width(&right_text);
        if fixed_width + visible_width(&left_text) + rw + minimum_gap <= target_width || rw <= 0 {
            break;
        }
        let next = truncate_to_width(&right_text, (rw - 1).max(0), Some(""), None);
        if visible_width(&next) >= rw {
            break;
        }
        right_text = next;
    }

    loop {
        let lw = visible_width(&left_text);
        if fixed_width + lw + visible_width(&right_text) + minimum_gap <= target_width || lw <= 0 {
            break;
        }
        let next = truncate_to_width(&left_text, (lw - 1).max(0), Some(""), None);
        if visible_width(&next) >= lw {
            break;
        }
        left_text = next;
    }

    let gap_width =
        (target_width - fixed_width - visible_width(&left_text) - visible_width(&right_text)).max(0) as usize;
    format!(
        "{}{}{}{}{}",
        border("─"),
        left_text,
        border(&"─".repeat(gap_width)),
        right_text,
        border("─")
    )
}

fn format_cwd(cwd: &str) -> String {
    if let Some(home) = pi_js::env::var("HOME")
        && !home.is_empty()
        && let Some(rest) = cwd.strip_prefix(&home)
    {
        return format!("~{rest}");
    }
    cwd.to_string()
}

fn format_context(ctx: &ExtensionContext) -> String {
    let usage = ctx.get_context_usage().ok().flatten();
    let context_window = usage
        .as_ref()
        .map(|u| u.context_window)
        .or_else(|| ctx.model().ok().flatten().map(|m| m.context_window));

    match (context_window, usage.and_then(|u| u.percent)) {
        (Some(cw), Some(pct)) if cw > 0 => {
            format!("ctx {}%/{}k", pct.round() as i64, ((cw as f64) / 1000.0).round() as i64)
        }
        _ => "ctx ?".to_string(),
    }
}

struct EmptyFooter;

impl Component for EmptyFooter {
    fn render(&self, _width: usize) -> Vec<String> {
        Vec::new()
    }

    fn invalidate(&self) {}
}

struct BorderEditorShared {
    is_working: bool,
    spinner_index: usize,
    spinner_timer: Option<pi_js::time::Interval>,
    active_tui: Option<Arc<dyn TUI>>,
    branch: Option<String>,
}

impl BorderEditorShared {
    fn new() -> Self {
        Self {
            is_working: false,
            spinner_index: 0,
            spinner_timer: None,
            active_tui: None,
            branch: None,
        }
    }

    fn stop_spinner(&mut self) {
        if let Some(handle) = self.spinner_timer.take() {
            handle.clear();
        }
    }
}

struct BorderStatusEditor {
    // WIRE(@tui-w3): switch inner Editor to CustomEditor once @tui merges.
    inner: Editor,
    shared: Arc<Mutex<BorderEditorShared>>,
    ctx: ExtensionContext,
    pi: ExtensionApi,
}

impl Component for BorderStatusEditor {
    fn render(&self, width: usize) -> Vec<String> {
        let mut lines = self.inner.render(width);
        if lines.len() < 2 {
            return lines;
        }

        let (is_working, spinner_index, branch) = {
            let guard = self.shared.lock().unwrap_or_else(|e| e.into_inner());
            (guard.is_working, guard.spinner_index, guard.branch.clone())
        };

        let Ok(ui) = self.ctx.ui() else {
            return lines;
        };
        let thm = ui.theme();

        let model = match self.ctx.model().ok().flatten() {
            Some(m) => format!("{}/{}", m.provider, m.id),
            None => "no model".to_string(),
        };
        let thinking = self
            .pi
            .get_thinking_level()
            .map(|lvl| format!("{lvl:?}").to_lowercase())
            .unwrap_or_else(|_| "off".to_string());
        let cwd = self.ctx.cwd().unwrap_or_default();

        let top_left = if is_working {
            thm.fg(
                "accent",
                &format!(" {} ", SPINNER_FRAMES[spinner_index % SPINNER_FRAMES.len()]),
            )
        } else {
            String::new()
        };
        let top_right = "";
        let bottom_left = thm.fg("muted", &format!(" {model} · {thinking} "));
        let branch_suffix = match branch {
            Some(b) if !b.is_empty() => format!(" ({b})"),
            _ => String::new(),
        };
        let bottom_right = thm.fg(
            "muted",
            &format!(
                " {} · {}{} ",
                format_context(&self.ctx),
                format_cwd(&cwd),
                branch_suffix
            ),
        );

        let border_color = self.inner.border_color();
        let bc = |text: &str| (border_color)(text);

        let last_idx = lines.len() - 1;
        lines[0] = fit_border(&top_left, top_right, width, &bc);
        lines[last_idx] = fit_border(&bottom_left, &bottom_right, width, &bc);
        lines
    }

    fn has_handle_input(&self) -> bool {
        true
    }

    fn handle_input(&self, data: &str) {
        self.inner.handle_input(data);
    }

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.inner.handle_mouse(event)
    }

    fn invalidate(&self) {
        self.inner.invalidate();
    }

    fn as_focusable(&self) -> Option<&dyn Focusable> {
        Some(&self.inner)
    }
}

impl EditorComponent for BorderStatusEditor {
    fn get_text(&self) -> String {
        self.inner.get_text()
    }

    fn set_text(&self, text: &str) {
        self.inner.set_text(text);
    }

    fn on_submit(&self) -> Option<EditorTextCallback> {
        self.inner.on_submit()
    }

    fn set_on_submit(&self, on_submit: Option<EditorTextCallback>) {
        self.inner.set_on_submit(on_submit);
    }

    fn on_change(&self) -> Option<EditorTextCallback> {
        self.inner.on_change()
    }

    fn set_on_change(&self, on_change: Option<EditorTextCallback>) {
        self.inner.set_on_change(on_change);
    }

    fn add_to_history(&self, text: &str) {
        self.inner.add_to_history(text);
    }

    fn insert_text_at_cursor(&self, text: &str) {
        self.inner.insert_text_at_cursor(text);
    }

    fn get_expanded_text(&self) -> String {
        self.inner.get_expanded_text()
    }

    fn set_autocomplete_provider(&self, provider: Arc<dyn AutocompleteProvider>) {
        self.inner.set_autocomplete_provider(provider);
    }

    fn border_color(&self) -> Option<EditorBorderColor> {
        Some(self.inner.border_color())
    }

    fn set_border_color(&self, border_color: Option<EditorBorderColor>) {
        if let Some(bc) = border_color {
            self.inner.set_border_color(bc);
        }
    }

    fn set_padding_x(&self, padding: f64) {
        self.inner.set_padding_x(padding);
    }

    fn set_autocomplete_max_visible(&self, max_visible: f64) {
        self.inner.set_autocomplete_max_visible(max_visible);
    }
}

pub struct BorderStatusEditorExtension;

#[async_trait]
impl Extension for BorderStatusEditorExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        let shared = Arc::new(Mutex::new(BorderEditorShared::new()));

        let shared_start = Arc::clone(&shared);
        pi.on::<AgentStartEvent>(move |_event, _ctx| {
            let shared_outer = Arc::clone(&shared_start);
            Box::pin(async move {
                let tui = {
                    let mut guard = shared_outer.lock().unwrap_or_else(|e| e.into_inner());
                    guard.is_working = true;
                    guard.stop_spinner();
                    let shared_tick = Arc::clone(&shared_outer);
                    let handle = pi_js::time::set_interval(80, move || {
                        let tick_tui = {
                            let mut g = shared_tick.lock().unwrap_or_else(|e| e.into_inner());
                            g.spinner_index = (g.spinner_index + 1) % SPINNER_FRAMES.len();
                            g.active_tui.clone()
                        };
                        if let Some(t) = tick_tui {
                            t.request_render(false);
                        }
                    });
                    guard.spinner_timer = Some(handle);
                    guard.active_tui.clone()
                };
                if let Some(t) = tui {
                    t.request_render(false);
                }
                Ok(None)
            })
        });

        let shared_settled = Arc::clone(&shared);
        pi.on::<AgentSettledEvent>(move |_event, _ctx| {
            let shared_inner = Arc::clone(&shared_settled);
            Box::pin(async move {
                let tui = {
                    let mut guard = shared_inner.lock().unwrap_or_else(|e| e.into_inner());
                    guard.is_working = false;
                    guard.stop_spinner();
                    guard.active_tui.clone()
                };
                if let Some(t) = tui {
                    t.request_render(false);
                }
                Ok(None)
            })
        });

        let shared_shutdown = Arc::clone(&shared);
        pi.on::<SessionShutdownEvent>(move |_event, _ctx| {
            let shared_inner = Arc::clone(&shared_shutdown);
            Box::pin(async move {
                let mut guard = shared_inner.lock().unwrap_or_else(|e| e.into_inner());
                guard.stop_spinner();
                guard.active_tui = None;
                Ok(None)
            })
        });

        let pi_session = pi.clone();
        let shared_session = Arc::clone(&shared);
        pi.on::<SessionStartEvent>(move |_event, ctx| {
            let pi_outer = pi_session.clone();
            let shared_outer = Arc::clone(&shared_session);
            Box::pin(async move {
                let ui = ctx.ui()?;
                ui.set_working_visible(false);
                ui.set_footer(Some(Arc::new(|_tui, _theme, _footer_data| Arc::new(EmptyFooter))));

                let pi_branch = pi_outer.clone();
                let shared_branch = Arc::clone(&shared_outer);
                let cwd = ctx.cwd().unwrap_or_default();
                tokio::spawn(async move {
                    let res = pi_branch
                        .exec(
                            "git",
                            &["branch".to_string(), "--show-current".to_string()],
                            Some(ExecOptions {
                                cwd: Some(cwd),
                                ..Default::default()
                            }),
                        )
                        .await;
                    let branch = res.ok().map(|r| r.stdout.trim().to_string()).filter(|s| !s.is_empty());
                    let tui = {
                        let mut g = shared_branch.lock().unwrap_or_else(|e| e.into_inner());
                        g.branch = branch;
                        g.active_tui.clone()
                    };
                    if let Some(t) = tui {
                        t.request_render(false);
                    }
                });

                let ctx_editor = ctx.clone();
                let pi_editor = pi_outer.clone();
                let shared_editor = Arc::clone(&shared_outer);
                ui.set_editor_component(Some(Arc::new(move |tui, editor_theme, _kb| {
                    {
                        let mut g = shared_editor.lock().unwrap_or_else(|e| e.into_inner());
                        g.active_tui = Some(Arc::clone(&tui));
                    }
                    let inner = Editor::new(
                        tui,
                        editor_theme,
                        Some(EditorOptions {
                            padding_x: Some(0.0),
                            autocomplete_max_visible: None,
                        }),
                    );
                    Arc::new(BorderStatusEditor {
                        inner,
                        shared: Arc::clone(&shared_editor),
                        ctx: ctx_editor.clone(),
                        pi: pi_editor.clone(),
                    })
                })));

                Ok(None)
            })
        });

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(BorderStatusEditorExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
