//! Port of packages/coding-agent/examples/extensions/rainbow-editor.ts

use std::process::ExitCode;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use async_trait::async_trait;
use pi_coding_agent::core::extensions::types::{
    Extension, ExtensionApi, ExtensionFactory, InlineExtension, KeybindingsManager, SessionStartEvent,
};
use pi_js::time::{Interval, set_interval};
use pi_tui::autocomplete::AutocompleteProvider;
use pi_tui::components::editor::{Editor, EditorTheme};
use pi_tui::editor_component::{EditorBorderColor, EditorComponent, EditorTextCallback};
use pi_tui::{Component, Focusable, TUI};

const COLORS: [[u8; 3]; 7] = [
    [233, 137, 115],
    [228, 186, 103],
    [141, 192, 122],
    [102, 194, 179],
    [121, 157, 207],
    [157, 134, 195],
    [206, 130, 172],
];
const RESET: &str = "\x1b[0m";

static ULTRATHINK_RE: LazyLock<Option<regex::Regex>> = LazyLock::new(|| regex::Regex::new(r"(?i)ultrathink").ok());

fn brighten(rgb: [u8; 3], factor: f64) -> String {
    let r = (rgb[0] as f64 + (255.0 - rgb[0] as f64) * factor).round() as u8;
    let g = (rgb[1] as f64 + (255.0 - rgb[1] as f64) * factor).round() as u8;
    let b = (rgb[2] as f64 + (255.0 - rgb[2] as f64) * factor).round() as u8;
    format!("\x1b[38;2;{r};{g};{b}m")
}

fn colorize(text: &str, shine_pos: isize) -> String {
    let mut out = String::new();
    for (i, ch) in text.chars().enumerate() {
        let base_color = COLORS[i % COLORS.len()];
        let mut factor = 0.0;
        if shine_pos >= 0 {
            let dist = (i as isize - shine_pos).abs();
            if dist == 0 {
                factor = 0.7;
            } else if dist == 1 {
                factor = 0.35;
            }
        }
        out.push_str(&brighten(base_color, factor));
        out.push(ch);
    }
    out.push_str(RESET);
    out
}

struct RainbowEditor {
    // WIRE(@tui-w3): switch inner Editor to CustomEditor once @tui merges.
    inner: Editor,
    tui: Arc<dyn TUI + Send + Sync>,
    animation_timer: Mutex<Option<Interval>>,
    frame: Arc<AtomicUsize>,
}

impl RainbowEditor {
    fn new(tui: Arc<dyn TUI + Send + Sync>, theme: EditorTheme, _kb: KeybindingsManager) -> Self {
        Self {
            inner: Editor::new(Arc::clone(&tui), theme, None),
            tui,
            animation_timer: Mutex::new(None),
            frame: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn has_ultrathink(&self) -> bool {
        let text = self.inner.get_text();
        ULTRATHINK_RE.as_ref().is_some_and(|re| re.is_match(&text))
    }

    fn start_animation(&self) {
        let already_running = self.animation_timer.lock().unwrap_or_else(|e| e.into_inner()).is_some();
        if already_running {
            return;
        }
        let frame_ref = Arc::clone(&self.frame);
        let tui_ref = Arc::clone(&self.tui);
        let iv = set_interval(60, move || {
            frame_ref.fetch_add(1, Ordering::SeqCst);
            tui_ref.request_render(None);
        });
        let old = self
            .animation_timer
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .replace(iv);
        if let Some(prev) = old {
            prev.clear();
        }
    }

    fn stop_animation(&self) {
        let old = self.animation_timer.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(iv) = old {
            iv.clear();
        }
    }
}

impl Drop for RainbowEditor {
    fn drop(&mut self) {
        self.stop_animation();
    }
}

impl Component for RainbowEditor {
    fn render(&self, width: usize) -> Vec<String> {
        let cycle = (self.frame.load(Ordering::SeqCst) % 20) as isize;
        let shine_pos = if cycle < 10 { cycle } else { -1 };
        let lines = self.inner.render(width);
        let Some(re) = ULTRATHINK_RE.as_ref() else {
            return lines;
        };
        lines
            .into_iter()
            .map(|line| {
                re.replace_all(&line, |caps: &regex::Captures<'_>| {
                    colorize(caps.get(0).map(|m| m.as_str()).unwrap_or(""), shine_pos)
                })
                .into_owned()
            })
            .collect()
    }

    fn handle_input(&self, data: &str) {
        self.inner.handle_input(data);
        if self.has_ultrathink() {
            self.start_animation();
        } else {
            self.stop_animation();
        }
    }

    fn has_handle_input(&self) -> bool {
        true
    }

    fn invalidate(&self) {
        self.inner.invalidate();
    }

    fn as_focusable(&self) -> Option<&dyn Focusable> {
        Some(&self.inner)
    }
}

impl EditorComponent for RainbowEditor {
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

pub struct RainbowEditorExtension;

#[async_trait]
impl Extension for RainbowEditorExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        pi.on::<SessionStartEvent>(|_event, ctx| {
            Box::pin(async move {
                ctx.ui()?.set_editor_component(Some(Arc::new(|tui, theme, kb| {
                    Arc::new(RainbowEditor::new(tui, theme, kb))
                })));
                Ok(None)
            })
        });

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(RainbowEditorExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
