//! Port of packages/coding-agent/examples/extensions/overlay-qa-tests.ts

use std::process::{ExitCode, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

use async_trait::async_trait;
use pi_coding_agent::core::extensions::types::{
    DoneFn, Extension, ExtensionApi, ExtensionCustomOptions, ExtensionFactory, InlineExtension, NotifyType,
    OverlayOptionsSource, RegisteredCommandOptions, Theme,
};
use pi_tui::tui::OverlayMarginValue;
use pi_tui::{
    Component, Focusable, Input, Key, OverlayAnchor, OverlayHandle, OverlayMargin, OverlayOptions, SizeValue, TUI,
    matches_key, truncate_to_width, visible_width,
};
use serde_json::Value;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::Notify;

fn render_box(theme: &Theme, lines: &[String], width: usize, title: Option<&str>) -> Vec<String> {
    let inner_w = width.saturating_sub(2).max(1);
    let mut result = Vec::with_capacity(lines.len() + 2);

    let title_str = match title {
        Some(t) => truncate_to_width(&format!(" {t} "), inner_w as i64, Some("..."), None),
        None => String::new(),
    };
    let title_w = visible_width(&title_str).max(0) as usize;
    let left_len = inner_w.saturating_sub(title_w) / 2;
    let right_len = inner_w.saturating_sub(title_w).saturating_sub(left_len);
    result.push(format!(
        "{}{}{}",
        theme.fg("border", &format!("╭{}", "─".repeat(left_len))),
        theme.fg("accent", &title_str),
        theme.fg("border", &format!("{}╮", "─".repeat(right_len)))
    ));

    for line in lines {
        result.push(format!(
            "{}{}{}",
            theme.fg("border", "│"),
            truncate_to_width(line, inner_w as i64, Some("..."), Some(true)),
            theme.fg("border", "│")
        ));
    }

    result.push(theme.fg("border", &format!("╰{}╯", "─".repeat(inner_w))));
    result
}

fn anchor_label(anchor: OverlayAnchor) -> &'static str {
    match anchor {
        OverlayAnchor::TopLeft => "top-left",
        OverlayAnchor::TopCenter => "top-center",
        OverlayAnchor::TopRight => "top-right",
        OverlayAnchor::LeftCenter => "left-center",
        OverlayAnchor::Center => "center",
        OverlayAnchor::RightCenter => "right-center",
        OverlayAnchor::BottomLeft => "bottom-left",
        OverlayAnchor::BottomCenter => "bottom-center",
        OverlayAnchor::BottomRight => "bottom-right",
    }
}

struct AnchorTestComponent {
    theme: Theme,
    anchor: OverlayAnchor,
    done: DoneFn,
}

impl Component for AnchorTestComponent {
    fn has_handle_input(&self) -> bool {
        true
    }

    fn handle_input(&self, data: &str) {
        if matches_key(data, Key::escape) || matches_key(data, &Key::ctrl("c")) {
            (self.done)(Value::String("cancel".to_string()));
        } else if matches_key(data, Key::r#return) {
            (self.done)(Value::String("confirm".to_string()));
        } else if matches_key(data, Key::space) || matches_key(data, Key::right) {
            (self.done)(Value::String("next".to_string()));
        }
    }

    fn render(&self, width: usize) -> Vec<String> {
        let th = &self.theme;
        let lines = vec![
            String::new(),
            format!(" Current: {}", th.fg("accent", anchor_label(self.anchor))),
            String::new(),
            format!(" {}", th.fg("dim", "Space/→ = next anchor")),
            format!(" {}", th.fg("dim", "Enter = confirm")),
            format!(" {}", th.fg("dim", "Esc = cancel")),
            String::new(),
        ];
        render_box(th, &lines, width, Some("Anchor Test"))
    }

    fn invalidate(&self) {}
}

struct MarginConfig {
    name: &'static str,
    options: OverlayOptions,
}

struct MarginTestComponent {
    theme: Theme,
    name: &'static str,
    done: DoneFn,
}

impl Component for MarginTestComponent {
    fn has_handle_input(&self) -> bool {
        true
    }

    fn handle_input(&self, data: &str) {
        if matches_key(data, Key::escape) || matches_key(data, &Key::ctrl("c")) {
            (self.done)(Value::String("close".to_string()));
        } else if matches_key(data, Key::space) || matches_key(data, Key::right) {
            (self.done)(Value::String("next".to_string()));
        }
    }

    fn render(&self, width: usize) -> Vec<String> {
        let th = &self.theme;
        let lines = vec![
            String::new(),
            format!(" {}", th.fg("accent", self.name)),
            String::new(),
            format!(" {}", th.fg("dim", "Space/→ = next config")),
            format!(" {}", th.fg("dim", "Esc = close")),
            String::new(),
        ];
        render_box(th, &lines, width, Some("Margin Test"))
    }

    fn invalidate(&self) {}
}

struct StackOverlayComponent {
    theme: Theme,
    num: usize,
    position: &'static str,
    done: DoneFn,
}

impl Component for StackOverlayComponent {
    fn has_handle_input(&self) -> bool {
        true
    }

    fn handle_input(&self, data: &str) {
        if matches_key(data, Key::escape) || matches_key(data, &Key::ctrl("c")) || matches_key(data, Key::r#return) {
            (self.done)(Value::String(format!("Overlay {}", self.num)));
        }
    }

    fn render(&self, width: usize) -> Vec<String> {
        let th = &self.theme;
        let colors = ["error", "success", "accent"];
        let color = colors[(self.num - 1) % colors.len()];
        let inner_w = width.saturating_sub(2).max(1);
        let border = |s: &str| th.fg(color, s);
        let pad_line = |s: &str| truncate_to_width(s, inner_w as i64, Some("..."), Some(true));

        let mut lines = Vec::new();
        lines.push(border(&format!("╭{}╮", "─".repeat(inner_w))));
        lines.push(format!(
            "{}{}{}",
            border("│"),
            pad_line(&format!(" Overlay {}", th.fg("accent", &format!("#{}", self.num)))),
            border("│")
        ));
        lines.push(format!(
            "{}{}{}",
            border("│"),
            pad_line(&format!(" Layer: {}", th.fg(color, self.position))),
            border("│")
        ));
        lines.push(format!("{}{}{}", border("│"), pad_line(""), border("│")));
        let pattern = format!(" {} ", "░".repeat(inner_w.saturating_sub(2)));
        for _ in 0..5 {
            lines.push(format!("{}{}{}", border("│"), pad_line(&pattern), border("│")));
        }
        lines.push(format!("{}{}{}", border("│"), pad_line(""), border("│")));
        lines.push(format!(
            "{}{}{}",
            border("│"),
            pad_line(&th.fg("dim", " Press Enter/Esc to close")),
            border("│")
        ));
        lines.push(border(&format!("╰{}╯", "─".repeat(inner_w))));
        lines
    }

    fn invalidate(&self) {}
}

struct StreamingOverflowState {
    lines: Vec<String>,
    scroll_offset: usize,
    finished: bool,
}

struct StreamingOverflowInner {
    tui: Arc<dyn TUI>,
    theme: Theme,
    max_visible_lines: usize,
    disposed: AtomicBool,
    stop_notify: Notify,
    done: DoneFn,
    mutable: Mutex<StreamingOverflowState>,
}

struct StreamingOverflowComponent {
    inner: Arc<StreamingOverflowInner>,
}

impl StreamingOverflowComponent {
    fn new(tui: Arc<dyn TUI>, theme: Theme, done: DoneFn) -> Self {
        let inner = Arc::new(StreamingOverflowInner {
            tui,
            theme,
            max_visible_lines: 15,
            disposed: AtomicBool::new(false),
            stop_notify: Notify::new(),
            done,
            mutable: Mutex::new(StreamingOverflowState {
                lines: Vec::new(),
                scroll_offset: 0,
                finished: false,
            }),
        });
        Self::start_process(&inner);
        Self { inner }
    }

    fn start_process(inner: &Arc<StreamingOverflowInner>) {
        let weak = Arc::downgrade(inner);
        let script = r#"
			echo "Starting streaming overflow test (30+ seconds)..."
			echo "This simulates subagent output with colors, hyperlinks, and long paths"
			echo ""
			for i in $(seq 1 100); do
				DIR="/Users/nicobailon/Documents/development/pi-mono/packages/coding-agent/src/modes/interactive"
				FILE="${DIR}/components/very-long-component-name-that-exceeds-width-${i}.ts"
				echo -e "\033]8;;file://${FILE}\007▶ read: ${FILE}\033]8;;\007"

				if [ $((i % 5)) -eq 0 ]; then
					echo -e "  \033[32m✓ Successfully processed ${i} files in /Users/nicobailon/Documents/development/pi-mono\033[0m"
				fi
				if [ $((i % 7)) -eq 0 ]; then
					echo -e "  \033[33m⚠ Warning: potential issue detected at line ${i} in very-long-component-name-that-exceeds-width.ts\033[0m"
				fi
				if [ $((i % 11)) -eq 0 ]; then
					echo -e "  \033[31m✗ Error: file not found /some/really/long/path/that/definitely/exceeds/the/overlay/width/limit/file-${i}.ts\033[0m"
				fi
				sleep 0.3
			done
			echo ""
			echo -e "\033[32m✓ Complete - 100 files processed in 30 seconds\033[0m"
			echo "Press Esc to close"
        "#;

        tokio::spawn(async move {
            let Ok(mut child) = Command::new("bash")
                .args(["-c", script])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .spawn()
            else {
                if let Some(st) = weak.upgrade() {
                    st.mutable.lock().unwrap_or_else(|e| e.into_inner()).finished = true;
                    st.tui.request_render(false);
                }
                return;
            };

            let stdout = child.stdout.take();
            let stderr = child.stderr.take();
            let weak_out = Weak::clone(&weak);
            let out_task = tokio::spawn(async move {
                if let Some(mut reader) = stdout {
                    let mut buf = [0u8; 4096];
                    while let Ok(n) = reader.read(&mut buf).await {
                        if n == 0 {
                            break;
                        }
                        let Some(st) = weak_out.upgrade() else {
                            break;
                        };
                        if st.disposed.load(Ordering::SeqCst) {
                            break;
                        }
                        let text = String::from_utf8_lossy(&buf[..n]);
                        {
                            let mut m = st.mutable.lock().unwrap_or_else(|e| e.into_inner());
                            for line in text.split('\n') {
                                if !line.is_empty() {
                                    m.lines.push(line.to_string());
                                }
                            }
                            m.scroll_offset = m.lines.len().saturating_sub(st.max_visible_lines);
                        }
                        st.tui.request_render(false);
                    }
                }
            });

            let weak_err = Weak::clone(&weak);
            let err_task = tokio::spawn(async move {
                if let Some(mut reader) = stderr {
                    let mut buf = [0u8; 4096];
                    while let Ok(n) = reader.read(&mut buf).await {
                        if n == 0 {
                            break;
                        }
                        let Some(st) = weak_err.upgrade() else {
                            break;
                        };
                        if st.disposed.load(Ordering::SeqCst) {
                            break;
                        }
                        let text = String::from_utf8_lossy(&buf[..n]).trim().to_string();
                        if !text.is_empty() {
                            let err_line = st.theme.fg("error", &text);
                            st.mutable
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .lines
                                .push(err_line);
                            st.tui.request_render(false);
                        }
                    }
                }
            });

            if let Some(st) = weak.upgrade() {
                tokio::select! {
                    _ = child.wait() => {}
                    _ = st.stop_notify.notified() => {
                        let _ = child.kill().await;
                        let _ = child.wait().await;
                    }
                }
                let _ = out_task.await;
                let _ = err_task.await;
                if !st.disposed.load(Ordering::SeqCst) {
                    st.mutable.lock().unwrap_or_else(|e| e.into_inner()).finished = true;
                    st.tui.request_render(false);
                }
            }
        });
    }
}

impl Drop for StreamingOverflowInner {
    fn drop(&mut self) {
        self.disposed.store(true, Ordering::SeqCst);
        self.stop_notify.notify_waiters();
    }
}

impl Component for StreamingOverflowComponent {
    fn has_handle_input(&self) -> bool {
        true
    }

    fn handle_input(&self, data: &str) {
        if matches_key(data, Key::escape) || matches_key(data, &Key::ctrl("c")) {
            self.inner.disposed.store(true, Ordering::SeqCst);
            self.inner.stop_notify.notify_waiters();
            (self.inner.done)(Value::Null);
        } else if matches_key(data, Key::up) {
            {
                let mut m = self.inner.mutable.lock().unwrap_or_else(|e| e.into_inner());
                m.scroll_offset = m.scroll_offset.saturating_sub(1);
            }
            self.inner.tui.request_render(false);
        } else if matches_key(data, Key::down) {
            {
                let mut m = self.inner.mutable.lock().unwrap_or_else(|e| e.into_inner());
                let max_scroll = m.lines.len().saturating_sub(self.inner.max_visible_lines);
                m.scroll_offset = (m.scroll_offset + 1).min(max_scroll);
            }
            self.inner.tui.request_render(false);
        }
    }

    fn render(&self, width: usize) -> Vec<String> {
        let th = &self.inner.theme;
        let inner_w = width.saturating_sub(2).max(1);
        let pad_line = |s: &str| truncate_to_width(s, inner_w as i64, Some("..."), Some(true));
        let border = |s: &str| th.fg("border", s);

        let (lines_snapshot, scroll_offset, finished) = {
            let m = self.inner.mutable.lock().unwrap_or_else(|e| e.into_inner());
            (m.lines.clone(), m.scroll_offset, m.finished)
        };

        let mut result = Vec::new();
        let title = truncate_to_width(
            &format!(" Streaming Output ({} lines) ", lines_snapshot.len()),
            inner_w as i64,
            Some("..."),
            None,
        );
        let title_pad = inner_w.saturating_sub(visible_width(&title).max(0) as usize);
        result.push(format!(
            "{}{}{}",
            border("╭"),
            th.fg("accent", &title),
            border(&format!("{}╮", "─".repeat(title_pad)))
        ));

        let can_up = scroll_offset > 0;
        let can_down = scroll_offset + self.inner.max_visible_lines < lines_snapshot.len();
        let below = lines_snapshot
            .len()
            .saturating_sub(self.inner.max_visible_lines)
            .saturating_sub(scroll_offset);
        let scroll_info = format!("↑{scroll_offset} | ↓{below}");
        result.push(format!(
            "{}{}{}",
            border("│"),
            pad_line(if can_up || can_down {
                &th.fg("dim", &format!(" {scroll_info}"))
            } else {
                ""
            }),
            border("│")
        ));

        let end = (scroll_offset + self.inner.max_visible_lines).min(lines_snapshot.len());
        let visible_slice = if scroll_offset < end {
            &lines_snapshot[scroll_offset..end]
        } else {
            &[]
        };
        for line in visible_slice {
            result.push(format!(
                "{}{}{}",
                border("│"),
                pad_line(&format!(" {line}")),
                border("│")
            ));
        }
        for _ in visible_slice.len()..self.inner.max_visible_lines {
            result.push(format!("{}{}{}", border("│"), pad_line(""), border("│")));
        }

        let status = if finished {
            th.fg("success", "✓ Done")
        } else {
            th.fg("warning", "● Running")
        };
        result.push(format!(
            "{}{}{}",
            border("│"),
            pad_line(&format!(" {status} {}", th.fg("dim", "| ↑↓ scroll | Esc close"))),
            border("│")
        ));
        result.push(border(&format!("╰{}╯", "─".repeat(inner_w))));
        result
    }

    fn invalidate(&self) {}
}

struct EdgeTestComponent {
    theme: Theme,
    done: DoneFn,
}

impl Component for EdgeTestComponent {
    fn has_handle_input(&self) -> bool {
        true
    }

    fn handle_input(&self, data: &str) {
        if matches_key(data, Key::escape) || matches_key(data, &Key::ctrl("c")) {
            (self.done)(Value::Null);
        }
    }

    fn render(&self, width: usize) -> Vec<String> {
        let th = &self.theme;
        let lines = vec![
            String::new(),
            " This overlay is at the".to_string(),
            " right edge of terminal.".to_string(),
            String::new(),
            format!(" {}", th.fg("dim", "Verify right border")),
            format!(" {}", th.fg("dim", "aligns with edge.")),
            String::new(),
            format!(" {}", th.fg("dim", "Press Esc to close")),
            String::new(),
        ];
        render_box(th, &lines, width, Some("Edge Test"))
    }

    fn invalidate(&self) {}
}

struct PercentTestComponent {
    theme: Theme,
    name: &'static str,
    done: DoneFn,
}

impl Component for PercentTestComponent {
    fn has_handle_input(&self) -> bool {
        true
    }

    fn handle_input(&self, data: &str) {
        if matches_key(data, Key::escape) || matches_key(data, &Key::ctrl("c")) {
            (self.done)(Value::String("close".to_string()));
        } else if matches_key(data, Key::space) || matches_key(data, Key::right) {
            (self.done)(Value::String("next".to_string()));
        }
    }

    fn render(&self, width: usize) -> Vec<String> {
        let th = &self.theme;
        let lines = vec![
            String::new(),
            format!(" {}", th.fg("accent", self.name)),
            String::new(),
            format!(" {}", th.fg("dim", "Space/→ = next")),
            format!(" {}", th.fg("dim", "Esc = close")),
            String::new(),
        ];
        render_box(th, &lines, width, Some("Percent Test"))
    }

    fn invalidate(&self) {}
}

struct MaxHeightTestComponent {
    theme: Theme,
    done: DoneFn,
}

impl Component for MaxHeightTestComponent {
    fn has_handle_input(&self) -> bool {
        true
    }

    fn handle_input(&self, data: &str) {
        if matches_key(data, Key::escape) || matches_key(data, &Key::ctrl("c")) {
            (self.done)(Value::Null);
        }
    }

    fn render(&self, width: usize) -> Vec<String> {
        let th = &self.theme;
        let mut content_lines = vec![
            th.fg("warning", " ⚠ Rendering 21 lines, maxHeight: 10"),
            th.fg("dim", " Lines 11-21 truncated (no bottom border)"),
            String::new(),
        ];
        for i in 1..=14 {
            content_lines.push(format!(" Line {i} of 14"));
        }
        content_lines.push(String::new());
        content_lines.push(th.fg("dim", " Press Esc to close"));
        render_box(th, &content_lines, width, Some("MaxHeight Test"))
    }

    fn invalidate(&self) {}
}

struct SidepanelComponent {
    tui: Arc<dyn TUI>,
    theme: Theme,
    items: [&'static str; 5],
    selected_index: Mutex<usize>,
    done: DoneFn,
}

impl SidepanelComponent {
    fn new(tui: Arc<dyn TUI>, theme: Theme, done: DoneFn) -> Self {
        Self {
            tui,
            theme,
            items: ["Dashboard", "Messages", "Settings", "Help", "About"],
            selected_index: Mutex::new(0),
            done,
        }
    }
}

impl Component for SidepanelComponent {
    fn has_handle_input(&self) -> bool {
        true
    }

    fn handle_input(&self, data: &str) {
        if matches_key(data, Key::escape) || matches_key(data, &Key::ctrl("c")) {
            (self.done)(Value::Null);
        } else if matches_key(data, Key::up) {
            {
                let mut idx = self.selected_index.lock().unwrap_or_else(|e| e.into_inner());
                *idx = idx.saturating_sub(1);
            }
            self.tui.request_render(false);
        } else if matches_key(data, Key::down) {
            {
                let mut idx = self.selected_index.lock().unwrap_or_else(|e| e.into_inner());
                *idx = (*idx + 1).min(self.items.len() - 1);
            }
            self.tui.request_render(false);
        } else if matches_key(data, Key::r#return) {
            self.tui.request_render(false);
        }
    }

    fn render(&self, width: usize) -> Vec<String> {
        let th = &self.theme;
        let inner_w = width.saturating_sub(2).max(1);
        let pad_line = |s: &str| truncate_to_width(s, inner_w as i64, Some("..."), Some(true));
        let border = |s: &str| th.fg("border", s);
        let selected_idx = *self.selected_index.lock().unwrap_or_else(|e| e.into_inner());

        let mut lines = Vec::new();
        lines.push(border(&format!("╭{}╮", "─".repeat(inner_w))));
        lines.push(format!(
            "{}{}{}",
            border("│"),
            pad_line(&th.fg("accent", " Responsive Sidepanel")),
            border("│")
        ));
        lines.push(format!(
            "{}{}{}",
            border("├"),
            border(&"─".repeat(inner_w)),
            border("┤")
        ));

        for (i, item) in self.items.iter().enumerate() {
            let is_selected = i == selected_idx;
            let prefix = if is_selected {
                th.fg("accent", "→ ")
            } else {
                "  ".to_string()
            };
            let text = if is_selected {
                th.fg("accent", item)
            } else {
                (*item).to_string()
            };
            lines.push(format!(
                "{}{}{}",
                border("│"),
                pad_line(&format!("{prefix}{text}")),
                border("│")
            ));
        }

        lines.push(format!(
            "{}{}{}",
            border("├"),
            border(&"─".repeat(inner_w)),
            border("┤")
        ));
        lines.push(format!(
            "{}{}{}",
            border("│"),
            pad_line(&th.fg("warning", " ⚠ Resize terminal < 100 cols")),
            border("│")
        ));
        lines.push(format!(
            "{}{}{}",
            border("│"),
            pad_line(&th.fg("warning", "   to see panel auto-hide")),
            border("│")
        ));
        lines.push(format!(
            "{}{}{}",
            border("│"),
            pad_line(&th.fg("dim", " Uses visible: (w) => w >= 100")),
            border("│")
        ));
        lines.push(format!(
            "{}{}{}",
            border("│"),
            pad_line(&th.fg("dim", " ↑↓ navigate | Esc close")),
            border("│")
        ));
        lines.push(border(&format!("╰{}╯", "─".repeat(inner_w))));
        lines
    }

    fn invalidate(&self) {}
}

struct AnimationState {
    frame: usize,
    interval: Option<pi_js::time::Interval>,
    fps: usize,
    last_fps_update: f64,
    frames_since_last_fps: usize,
}

struct AnimationDemoInner {
    tui: Arc<dyn TUI>,
    theme: Theme,
    done: DoneFn,
    mutable: Mutex<AnimationState>,
}

struct AnimationDemoComponent {
    inner: Arc<AnimationDemoInner>,
}

impl AnimationDemoComponent {
    fn new(tui: Arc<dyn TUI>, theme: Theme, done: DoneFn) -> Self {
        let inner = Arc::new(AnimationDemoInner {
            tui,
            theme,
            done,
            mutable: Mutex::new(AnimationState {
                frame: 0,
                interval: None,
                fps: 0,
                last_fps_update: pi_js::time::now_ms(),
                frames_since_last_fps: 0,
            }),
        });
        let weak = Arc::downgrade(&inner);
        let handle = pi_js::time::set_interval(33, move || {
            let Some(st) = weak.upgrade() else {
                return;
            };
            {
                let mut m = st.mutable.lock().unwrap_or_else(|e| e.into_inner());
                m.frame += 1;
                m.frames_since_last_fps += 1;
                let now = pi_js::time::now_ms();
                if now - m.last_fps_update >= 1000.0 {
                    m.fps = m.frames_since_last_fps;
                    m.frames_since_last_fps = 0;
                    m.last_fps_update = now;
                }
            }
            st.tui.request_render(false);
        });
        inner.mutable.lock().unwrap_or_else(|e| e.into_inner()).interval = Some(handle);
        Self { inner }
    }

    fn dispose(&self) {
        let old = self
            .inner
            .mutable
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .interval
            .take();
        if let Some(h) = old {
            h.clear();
        }
    }
}

impl Drop for AnimationDemoInner {
    fn drop(&mut self) {
        if let Some(h) = self.mutable.lock().unwrap_or_else(|e| e.into_inner()).interval.take() {
            h.clear();
        }
    }
}

impl Component for AnimationDemoComponent {
    fn has_handle_input(&self) -> bool {
        true
    }

    fn handle_input(&self, data: &str) {
        if matches_key(data, Key::escape) || matches_key(data, &Key::ctrl("c")) {
            self.dispose();
            (self.inner.done)(Value::Null);
        }
    }

    fn render(&self, width: usize) -> Vec<String> {
        let th = &self.inner.theme;
        let inner_w = width.saturating_sub(2).max(1);
        let pad_line = |s: &str| truncate_to_width(s, inner_w as i64, Some("..."), Some(true));
        let border = |s: &str| th.fg("border", s);
        let (frame, fps) = {
            let m = self.inner.mutable.lock().unwrap_or_else(|e| e.into_inner());
            (m.frame, m.fps)
        };

        let mut lines = Vec::new();
        lines.push(border(&format!("╭{}╮", "─".repeat(inner_w))));
        lines.push(format!(
            "{}{}{}",
            border("│"),
            pad_line(&th.fg("accent", " Animation Demo (~30 FPS)")),
            border("│")
        ));
        lines.push(format!("{}{}{}", border("│"), pad_line(""), border("│")));
        lines.push(format!(
            "{}{}{}",
            border("│"),
            pad_line(&format!(" Frame: {}", th.fg("accent", &frame.to_string()))),
            border("│")
        ));
        lines.push(format!(
            "{}{}{}",
            border("│"),
            pad_line(&format!(" FPS: {}", th.fg("success", &fps.to_string()))),
            border("│")
        ));
        lines.push(format!("{}{}{}", border("│"), pad_line(""), border("│")));

        let bar_width = inner_w.saturating_sub(4).max(12);
        let pos = ((((frame as f64) / 10.0).sin() + 1.0) * ((bar_width.saturating_sub(10)) as f64) / 2.0)
            .floor()
            .max(0.0) as usize;
        let right_spaces = bar_width.saturating_sub(10).saturating_sub(pos);
        let bar = format!(
            "{}{}{}",
            " ".repeat(pos),
            th.fg("accent", "██████████"),
            " ".repeat(right_spaces)
        );
        lines.push(format!(
            "{}{}{}",
            border("│"),
            pad_line(&format!(" {bar}")),
            border("│")
        ));

        let spin_chars = ["◐", "◓", "◑", "◒"];
        let spin = spin_chars[frame % spin_chars.len()];
        lines.push(format!(
            "{}{}{}",
            border("│"),
            pad_line(&format!(" Spinner: {}", th.fg("warning", spin))),
            border("│")
        ));

        let hue = (frame * 3) % 360;
        let rgb = hsl_to_rgb((hue as f64) / 360.0, 0.8, 0.5);
        let color_block = format!("\x1b[48;2;{};{};{}m{}\x1b[0m", rgb.0, rgb.1, rgb.2, "  ".repeat(10));
        lines.push(format!(
            "{}{}{}",
            border("│"),
            pad_line(&format!(" Color: {color_block}")),
            border("│")
        ));

        lines.push(format!("{}{}{}", border("│"), pad_line(""), border("│")));
        lines.push(format!(
            "{}{}{}",
            border("│"),
            pad_line(&th.fg("dim", " This proves overlays can handle")),
            border("│")
        ));
        lines.push(format!(
            "{}{}{}",
            border("│"),
            pad_line(&th.fg("dim", " real-time game-like rendering.")),
            border("│")
        ));
        lines.push(format!(
            "{}{}{}",
            border("│"),
            pad_line(&th.fg("dim", " (pi-doom uses same approach)")),
            border("│")
        ));
        lines.push(format!("{}{}{}", border("│"), pad_line(""), border("│")));
        lines.push(format!(
            "{}{}{}",
            border("│"),
            pad_line(&th.fg("dim", " Press Esc to close")),
            border("│")
        ));
        lines.push(border(&format!("╰{}╯", "─".repeat(inner_w))));
        lines
    }

    fn invalidate(&self) {}
}

fn hsl_to_rgb(h: f64, s: f64, l: f64) -> (u8, u8, u8) {
    if s == 0.0 {
        let v = (l * 255.0).round() as u8;
        return (v, v, v);
    }
    let hue2rgb = |p: f64, q: f64, mut t: f64| -> f64 {
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
        if t < 1.0 / 6.0 {
            return p + (q - p) * 6.0 * t;
        }
        if t < 1.0 / 2.0 {
            return q;
        }
        if t < 2.0 / 3.0 {
            return p + (q - p) * (2.0 / 3.0 - t) * 6.0;
        }
        p
    };
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let r = (hue2rgb(p, q, h + 1.0 / 3.0) * 255.0).round() as u8;
    let g = (hue2rgb(p, q, h) * 255.0).round() as u8;
    let b = (hue2rgb(p, q, h - 1.0 / 3.0) * 255.0).round() as u8;
    (r, g, b)
}

struct ToggleDemoState {
    toggle_count: usize,
    is_toggling: bool,
}

struct ToggleDemoInner {
    tui: Arc<dyn TUI>,
    theme: Theme,
    done: DoneFn,
    global_handle: Arc<Mutex<Option<OverlayHandle>>>,
    mutable: Mutex<ToggleDemoState>,
}

struct ToggleDemoComponent {
    inner: Arc<ToggleDemoInner>,
}

impl Component for ToggleDemoComponent {
    fn has_handle_input(&self) -> bool {
        true
    }

    fn handle_input(&self, data: &str) {
        if matches_key(data, Key::escape) || matches_key(data, &Key::ctrl("c")) {
            (self.inner.done)(Value::Null);
            return;
        }
        if matches_key(data, "t") {
            let handle = self
                .inner
                .global_handle
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            let Some(h) = handle else {
                return;
            };
            {
                let mut m = self.inner.mutable.lock().unwrap_or_else(|e| e.into_inner());
                if m.is_toggling {
                    return;
                }
                m.is_toggling = true;
                m.toggle_count += 1;
            }
            h.set_hidden(true);

            let weak = Arc::downgrade(&self.inner);
            let _ = pi_js::time::set_timeout(1000, move || {
                let Some(st) = weak.upgrade() else {
                    return;
                };
                let restore_handle = st.global_handle.lock().unwrap_or_else(|e| e.into_inner()).clone();
                if let Some(rh) = restore_handle {
                    rh.set_hidden(false);
                    st.mutable.lock().unwrap_or_else(|e| e.into_inner()).is_toggling = false;
                    st.tui.request_render(false);
                }
            });
        }
    }

    fn render(&self, width: usize) -> Vec<String> {
        let th = &self.inner.theme;
        let toggle_count = self
            .inner
            .mutable
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .toggle_count;
        let lines = vec![
            String::new(),
            th.fg("accent", " Toggle Demo"),
            String::new(),
            " This overlay demonstrates the".to_string(),
            " onHandle callback API.".to_string(),
            String::new(),
            format!(" Toggle count: {}", th.fg("accent", &toggle_count.to_string())),
            String::new(),
            th.fg("dim", " Press 't' to hide for 1 second"),
            th.fg("dim", " (demonstrates setHidden API)"),
            String::new(),
            th.fg("dim", " In real usage, a global keybinding"),
            th.fg("dim", " would toggle visibility externally."),
            String::new(),
            th.fg("dim", " Press Esc to close"),
            String::new(),
        ];
        render_box(th, &lines, width, Some("Toggle Demo"))
    }

    fn invalidate(&self) {}
}

struct TimerPanel {
    theme: Theme,
    seconds: Mutex<usize>,
}

impl TimerPanel {
    fn tick(&self) {
        *self.seconds.lock().unwrap_or_else(|e| e.into_inner()) += 1;
    }
}

impl Component for TimerPanel {
    fn render(&self, width: usize) -> Vec<String> {
        let secs_total = *self.seconds.lock().unwrap_or_else(|e| e.into_inner());
        let mins = secs_total / 60;
        let secs = secs_total % 60;
        let time = format!("{mins:02}:{secs:02}");
        let lines = vec![
            format!(" {}", self.theme.fg("accent", &time)),
            self.theme.fg("dim", " nonCapturing: true"),
        ];
        render_box(&self.theme, &lines, width, Some("Timer"))
    }

    fn invalidate(&self) {}
}

struct PassiveState {
    typed: String,
    timer_handle: Option<OverlayHandle>,
    interval: Option<pi_js::time::Interval>,
    input_count: usize,
    last_input_debug: String,
}

struct PassiveDemoInner {
    tui: Arc<dyn TUI>,
    theme: Theme,
    focused: AtomicBool,
    timer_component: Arc<TimerPanel>,
    done: DoneFn,
    mutable: Mutex<PassiveState>,
}

struct PassiveDemoController {
    inner: Arc<PassiveDemoInner>,
}

impl PassiveDemoController {
    fn new(tui: Arc<dyn TUI>, theme: Theme, done: DoneFn) -> Self {
        let timer_component = Arc::new(TimerPanel {
            theme: theme.clone(),
            seconds: Mutex::new(0),
        });
        let timer_handle = tui.show_overlay(
            Arc::clone(&timer_component) as Arc<dyn Component>,
            Some(OverlayOptions {
                non_capturing: Some(true),
                anchor: Some(OverlayAnchor::TopRight),
                width: Some(SizeValue::Absolute(22.0)),
                margin: Some(OverlayMarginValue::Sides(OverlayMargin {
                    top: Some(1),
                    right: Some(2),
                    ..Default::default()
                })),
                ..Default::default()
            }),
        );

        let inner = Arc::new(PassiveDemoInner {
            tui: Arc::clone(&tui),
            theme,
            focused: AtomicBool::new(false),
            timer_component: Arc::clone(&timer_component),
            done,
            mutable: Mutex::new(PassiveState {
                typed: String::new(),
                timer_handle: Some(timer_handle),
                interval: None,
                input_count: 0,
                last_input_debug: String::new(),
            }),
        });

        let weak = Arc::downgrade(&inner);
        let handle = pi_js::time::set_interval(1000, move || {
            let Some(st) = weak.upgrade() else {
                return;
            };
            st.timer_component.tick();
            st.tui.request_render(false);
        });
        inner.mutable.lock().unwrap_or_else(|e| e.into_inner()).interval = Some(handle);
        Self { inner }
    }

    fn cleanup_inner(inner: &PassiveDemoInner) {
        let (interval, timer_handle) = {
            let mut m = inner.mutable.lock().unwrap_or_else(|e| e.into_inner());
            (m.interval.take(), m.timer_handle.take())
        };
        if let Some(h) = interval {
            h.clear();
        }
        if let Some(oh) = timer_handle {
            oh.hide();
        }
    }
}

impl Drop for PassiveDemoInner {
    fn drop(&mut self) {
        PassiveDemoController::cleanup_inner(self);
    }
}

impl Focusable for PassiveDemoController {
    fn focused(&self) -> bool {
        self.inner.focused.load(Ordering::Relaxed)
    }

    fn set_focused(&self, focused: bool) {
        self.inner.focused.store(focused, Ordering::Relaxed);
    }
}

impl Component for PassiveDemoController {
    fn has_handle_input(&self) -> bool {
        true
    }

    fn as_focusable(&self) -> Option<&dyn Focusable> {
        Some(self)
    }

    fn handle_input(&self, data: &str) {
        if matches_key(data, Key::escape) || matches_key(data, &Key::ctrl("c")) {
            Self::cleanup_inner(&self.inner);
            (self.inner.done)(Value::Null);
            return;
        }
        let mut m = self.inner.mutable.lock().unwrap_or_else(|e| e.into_inner());
        m.input_count += 1;
        let c0 = data.encode_utf16().next().unwrap_or(0);
        m.last_input_debug = format!("len={} c0={c0}", data.len());
        if matches_key(data, Key::backspace) {
            m.typed.pop();
        } else {
            let mut chars = data.chars();
            if let (Some(ch), None) = (chars.next(), chars.next())
                && (ch as u32) >= 32
            {
                m.typed.push(ch);
            }
        }
    }

    fn render(&self, width: usize) -> Vec<String> {
        let th = &self.inner.theme;
        let is_focused = self.inner.focused.load(Ordering::Relaxed);
        let (typed, input_count, last_debug) = {
            let m = self.inner.mutable.lock().unwrap_or_else(|e| e.into_inner());
            (m.typed.clone(), m.input_count, m.last_input_debug.clone())
        };
        let display = if !typed.is_empty() {
            typed
        } else {
            th.fg("dim", "(type here)")
        };
        let last = if last_debug.is_empty() {
            "none".to_string()
        } else {
            last_debug
        };
        let lines = vec![
            String::new(),
            format!(
                " {}",
                th.fg("dim", &format!("focused={is_focused} inputs={input_count}"))
            ),
            format!(" {}", th.fg("dim", &format!("last: {last}"))),
            String::new(),
            format!(" > {display}"),
            String::new(),
            th.fg("dim", " Type to prove input goes here."),
            th.fg("dim", " Press Esc to close both."),
            String::new(),
        ];
        render_box(th, &lines, width, Some("Non-Capturing Demo"))
    }

    fn invalidate(&self) {}
}

struct FocusPanelConfig {
    label: &'static str,
    color: &'static str,
    row: f64,
    col: f64,
    width: f64,
}

const FOCUS_PANEL_CONFIGS: [FocusPanelConfig; 3] = [
    FocusPanelConfig {
        label: "Alpha",
        color: "error",
        row: 2.0,
        col: 4.0,
        width: 34.0,
    },
    FocusPanelConfig {
        label: "Beta",
        color: "success",
        row: 5.0,
        col: 28.0,
        width: 34.0,
    },
    FocusPanelConfig {
        label: "Gamma",
        color: "accent",
        row: 8.0,
        col: 52.0,
        width: 34.0,
    },
];

struct FocusPanelEntry {
    panel: Arc<FocusPanel>,
    handle: OverlayHandle,
}

struct FocusDemoInner {
    tui: Arc<dyn TUI>,
    theme: Theme,
    done: DoneFn,
    entries: Mutex<Vec<FocusPanelEntry>>,
    closed: AtomicBool,
}

struct FocusPanel {
    label: &'static str,
    color: &'static str,
    theme: Theme,
    controller: Weak<FocusDemoInner>,
    focused: AtomicBool,
    closed: AtomicBool,
    input: Input,
    inputs: Mutex<Vec<String>>,
}

impl FocusPanel {
    fn new(theme: Theme, config: &FocusPanelConfig, controller: Weak<FocusDemoInner>) -> Self {
        Self {
            label: config.label,
            color: config.color,
            theme,
            controller,
            focused: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            input: Input::new(None),
            inputs: Mutex::new(Vec::new()),
        }
    }
}

impl Focusable for FocusPanel {
    fn focused(&self) -> bool {
        self.focused.load(Ordering::Relaxed)
    }

    fn set_focused(&self, focused: bool) {
        self.focused.store(focused, Ordering::Relaxed);
    }
}

impl Component for FocusPanel {
    fn has_handle_input(&self) -> bool {
        true
    }

    fn as_focusable(&self) -> Option<&dyn Focusable> {
        Some(self)
    }

    fn handle_input(&self, data: &str) {
        if matches_key(data, Key::tab) {
            if let Some(ctrl) = self.controller.upgrade() {
                FocusDemoController::focus_next_from(&ctrl, self, 1);
            }
            return;
        }
        if matches_key(data, "shift+tab") {
            if let Some(ctrl) = self.controller.upgrade() {
                FocusDemoController::focus_next_from(&ctrl, self, -1);
            }
            return;
        }
        if matches_key(data, Key::escape) || matches_key(data, &Key::ctrl("d")) {
            if let Some(ctrl) = self.controller.upgrade() {
                FocusDemoController::dismiss_panel(&ctrl, self);
            }
            return;
        }
        if matches_key(data, &Key::ctrl("c")) {
            if let Some(ctrl) = self.controller.upgrade() {
                FocusDemoController::close_inner(&ctrl);
            }
            return;
        }

        if matches_key(data, Key::r#return) {
            self.inputs
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push("Enter".to_string());
        } else if matches_key(data, Key::up) {
            self.inputs
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push("↑".to_string());
        } else if matches_key(data, Key::down) {
            self.inputs
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push("↓".to_string());
        } else if matches_key(data, Key::left) {
            self.input.handle_input(data);
            self.inputs
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push("←".to_string());
        } else if matches_key(data, Key::right) {
            self.input.handle_input(data);
            self.inputs
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push("→".to_string());
        } else if matches_key(data, Key::backspace) {
            self.input.handle_input(data);
            self.inputs
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push("Backspace".to_string());
        } else {
            self.input.handle_input(data);
            let encoded =
                pi_js::json::stringify(&Value::String(data.to_string())).unwrap_or_else(|_| format!("{data:?}"));
            self.inputs.lock().unwrap_or_else(|e| e.into_inner()).push(encoded);
        }
    }

    fn render(&self, width: usize) -> Vec<String> {
        let th = &self.theme;
        let inner_w = width.saturating_sub(2).max(1);
        let is_focused = self.focused.load(Ordering::Relaxed);
        let border_color = if is_focused { self.color } else { "dim" };
        let border = |s: &str| th.fg(border_color, s);
        let pad_line = |s: &str| truncate_to_width(s, inner_w as i64, Some("..."), Some(true));

        let recent = {
            let guard = self.inputs.lock().unwrap_or_else(|e| e.into_inner());
            if guard.is_empty() {
                "(none)".to_string()
            } else {
                let start = guard.len().saturating_sub(6);
                guard[start..].join(" ")
            }
        };

        self.input.set_focused(is_focused);
        let rendered_input = self.input.render(inner_w.saturating_sub(8).max(1));
        let input_line = rendered_input.first().map(String::as_str).unwrap_or("");

        let mut lines = Vec::new();
        lines.push(border(&format!("╭{}╮", "─".repeat(inner_w))));
        let status_label = if is_focused {
            th.fg("success", "FOCUSED")
        } else {
            th.fg("dim", "visible")
        };
        lines.push(format!(
            "{}{}{}",
            border("│"),
            pad_line(&format!(" {} {status_label}", th.fg(self.color, self.label))),
            border("│")
        ));
        lines.push(format!("{}{}{}", border("│"), pad_line(""), border("│")));
        lines.push(format!(
            "{}{}{}",
            border("│"),
            pad_line(&format!(" Input: {input_line}")),
            border("│")
        ));
        lines.push(format!(
            "{}{}{}",
            border("│"),
            pad_line(&format!(" Keys: {recent}")),
            border("│")
        ));
        lines.push(format!(
            "{}{}{}",
            border("│"),
            pad_line(&th.fg("dim", " Tab/Shift+Tab focus")),
            border("│")
        ));
        lines.push(format!(
            "{}{}{}",
            border("│"),
            pad_line(&th.fg("dim", " Esc/Ctrl+D dismiss")),
            border("│")
        ));
        lines.push(border(&format!("╰{}╯", "─".repeat(inner_w))));
        lines
    }

    fn invalidate(&self) {}
}

struct FocusDemoController {
    inner: Arc<FocusDemoInner>,
}

impl FocusDemoController {
    fn new(tui: Arc<dyn TUI>, theme: Theme, done: DoneFn) -> Self {
        let inner = Arc::new(FocusDemoInner {
            tui: Arc::clone(&tui),
            theme: theme.clone(),
            done,
            entries: Mutex::new(Vec::new()),
            closed: AtomicBool::new(false),
        });

        let mut created = Vec::with_capacity(FOCUS_PANEL_CONFIGS.len());
        for config in &FOCUS_PANEL_CONFIGS {
            let panel = Arc::new(FocusPanel::new(theme.clone(), config, Arc::downgrade(&inner)));
            let handle = tui.show_overlay(
                Arc::clone(&panel) as Arc<dyn Component>,
                Some(OverlayOptions {
                    non_capturing: Some(true),
                    row: Some(SizeValue::Absolute(config.row)),
                    col: Some(SizeValue::Absolute(config.col)),
                    width: Some(SizeValue::Absolute(config.width)),
                    ..Default::default()
                }),
            );
            created.push(FocusPanelEntry { panel, handle });
        }
        *inner.entries.lock().unwrap_or_else(|e| e.into_inner()) = created;
        Self::focus_first_open(&inner);
        Self { inner }
    }

    fn focus_first_open(inner: &FocusDemoInner) {
        let first_handle = {
            let entries = inner.entries.lock().unwrap_or_else(|e| e.into_inner());
            entries
                .iter()
                .find(|e| !e.panel.closed.load(Ordering::Relaxed))
                .map(|e| e.handle.clone())
        };
        if let Some(handle) = first_handle {
            handle.focus();
            inner.tui.request_render(false);
        }
    }

    fn focus_next_from(inner: &FocusDemoInner, current: *const FocusPanel, direction: isize) {
        let next_handle = {
            let entries = inner.entries.lock().unwrap_or_else(|e| e.into_inner());
            let open: Vec<&FocusPanelEntry> = entries
                .iter()
                .filter(|e| !e.panel.closed.load(Ordering::Relaxed))
                .collect();
            let Some(pos) = open.iter().position(|e| std::ptr::eq(Arc::as_ptr(&e.panel), current)) else {
                return;
            };
            let len = open.len() as isize;
            let next_pos = (((pos as isize + direction) % len + len) % len) as usize;
            open.get(next_pos).map(|e| e.handle.clone())
        };
        if let Some(handle) = next_handle {
            handle.focus();
            inner.tui.request_render(false);
        }
    }

    fn dismiss_panel(inner: &FocusDemoInner, target: *const FocusPanel) {
        let (dismissed_handle, next_handle, remaining_empty) = {
            let entries = inner.entries.lock().unwrap_or_else(|e| e.into_inner());
            let open: Vec<&FocusPanelEntry> = entries
                .iter()
                .filter(|e| !e.panel.closed.load(Ordering::Relaxed))
                .collect();
            let Some(pos) = open.iter().position(|e| std::ptr::eq(Arc::as_ptr(&e.panel), target)) else {
                return;
            };
            let entry = open[pos];
            entry.panel.closed.store(true, Ordering::Relaxed);
            let dismissed = entry.handle.clone();

            let remaining: Vec<&FocusPanelEntry> = open
                .into_iter()
                .filter(|c| !std::ptr::eq(Arc::as_ptr(&c.panel), target))
                .collect();
            if remaining.is_empty() {
                (dismissed, None, true)
            } else {
                let next_idx = pos % remaining.len();
                (dismissed, Some(remaining[next_idx].handle.clone()), false)
            }
        };

        dismissed_handle.hide();
        if remaining_empty {
            Self::close_inner(inner);
            return;
        }
        if let Some(nh) = next_handle {
            nh.focus();
            inner.tui.request_render(false);
        }
    }

    fn hide_panels(inner: &FocusDemoInner) {
        let to_hide: Vec<OverlayHandle> = {
            let mut entries = inner.entries.lock().unwrap_or_else(|e| e.into_inner());
            let handles = entries
                .iter()
                .filter(|e| !e.panel.closed.swap(true, Ordering::Relaxed))
                .map(|e| e.handle.clone())
                .collect();
            entries.clear();
            handles
        };
        for handle in to_hide {
            handle.hide();
        }
    }

    fn close_inner(inner: &FocusDemoInner) {
        if inner.closed.swap(true, Ordering::Relaxed) {
            return;
        }
        Self::hide_panels(inner);
        (inner.done)(Value::Null);
    }
}

impl Drop for FocusDemoInner {
    fn drop(&mut self) {
        if !self.closed.swap(true, Ordering::Relaxed) {
            FocusDemoController::hide_panels(self);
        }
    }
}

impl Component for FocusDemoController {
    fn has_handle_input(&self) -> bool {
        true
    }

    fn handle_input(&self, data: &str) {
        if matches_key(data, Key::escape) || matches_key(data, &Key::ctrl("c")) {
            Self::close_inner(&self.inner);
        } else if matches_key(data, Key::tab) {
            Self::focus_first_open(&self.inner);
        }
    }

    fn render(&self, width: usize) -> Vec<String> {
        let th = &self.inner.theme;
        let focused_label = {
            let entries = self.inner.entries.lock().unwrap_or_else(|e| e.into_inner());
            entries
                .iter()
                .find(|e| e.handle.is_focused())
                .map(|e| e.panel.label)
                .unwrap_or("Controller")
        };
        let lines = vec![
            String::new(),
            format!(" Current focus: {}", th.fg("accent", focused_label)),
            String::new(),
            " Three overlapping panels above are".to_string(),
            format!(" {} overlays controlled with", th.fg("accent", "nonCapturing")),
            " raw OverlayHandle.focus()/hide().".to_string(),
            String::new(),
            " Type in the focused panel's input.".to_string(),
            " Focused panel renders on top.".to_string(),
            String::new(),
            th.fg("dim", " Tab/Shift+Tab = cycle panels"),
            th.fg("dim", " Esc/Ctrl+D = dismiss panel"),
            th.fg("dim", " Ctrl+C = close all"),
            String::new(),
        ];
        render_box(th, &lines, width, Some("Focus + Input Demo"))
    }

    fn invalidate(&self) {}
}

struct StreamingPanelState {
    handle: Option<OverlayHandle>,
    typed: String,
}

struct StreamingInputPanel {
    label: &'static str,
    color: &'static str,
    theme: Theme,
    on_tab: Arc<dyn Fn() + Send + Sync>,
    on_close: Arc<dyn Fn() + Send + Sync>,
    mutable: Mutex<StreamingPanelState>,
}

impl StreamingInputPanel {
    fn new(
        theme: Theme,
        label: &'static str,
        color: &'static str,
        on_tab: Arc<dyn Fn() + Send + Sync>,
        on_close: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        Self {
            label,
            color,
            theme,
            on_tab,
            on_close,
            mutable: Mutex::new(StreamingPanelState {
                handle: None,
                typed: String::new(),
            }),
        }
    }

    fn set_handle(&self, handle: OverlayHandle) {
        self.mutable.lock().unwrap_or_else(|e| e.into_inner()).handle = Some(handle);
    }
}

impl Component for StreamingInputPanel {
    fn has_handle_input(&self) -> bool {
        true
    }

    fn handle_input(&self, data: &str) {
        if matches_key(data, Key::tab) {
            (self.on_tab)();
        } else if matches_key(data, Key::escape) || matches_key(data, &Key::ctrl("c")) {
            (self.on_close)();
        } else if matches_key(data, Key::backspace) {
            self.mutable.lock().unwrap_or_else(|e| e.into_inner()).typed.pop();
        } else {
            let mut chars = data.chars();
            if let (Some(ch), None) = (chars.next(), chars.next())
                && (ch as u32) >= 32
            {
                self.mutable.lock().unwrap_or_else(|e| e.into_inner()).typed.push(ch);
            }
        }
    }

    fn render(&self, width: usize) -> Vec<String> {
        let th = &self.theme;
        let (handle, typed) = {
            let m = self.mutable.lock().unwrap_or_else(|e| e.into_inner());
            (m.handle.clone(), m.typed.clone())
        };
        let focused = handle.as_ref().is_some_and(|h| h.is_focused());
        let inner_w = width.saturating_sub(2).max(1);
        let border = |s: &str| th.fg(self.color, s);
        let pad_line = |s: &str| -> String {
            let w = visible_width(s).max(0) as usize;
            format!("{s}{}", " ".repeat(inner_w.saturating_sub(w)))
        };

        let input_display = if !typed.is_empty() {
            typed
        } else {
            th.fg("dim", "(type here)")
        };
        let truncated_input =
            truncate_to_width(&format!(" > {input_display}"), inner_w as i64, Some("..."), Some(true));

        let mut lines = Vec::new();
        lines.push(border(&format!("╭{}╮", "─".repeat(inner_w))));
        lines.push(format!(
            "{}{}{}",
            border("│"),
            pad_line(&format!(" {}", th.fg("accent", self.label))),
            border("│")
        ));
        lines.push(format!("{}{}{}", border("│"), pad_line(""), border("│")));
        if focused {
            lines.push(format!(
                "{}{}{}",
                border("│"),
                pad_line(&th.fg("success", " ● FOCUSED")),
                border("│")
            ));
            lines.push(format!(
                "{}{}{}",
                border("│"),
                pad_line(&th.fg("dim", " (receiving input)")),
                border("│")
            ));
        } else {
            lines.push(format!(
                "{}{}{}",
                border("│"),
                pad_line(&th.fg("dim", " ○ unfocused")),
                border("│")
            ));
            lines.push(format!("{}{}{}", border("│"), pad_line(""), border("│")));
        }
        lines.push(format!("{}{}{}", border("│"), truncated_input, border("│")));
        lines.push(format!("{}{}{}", border("│"), pad_line(""), border("│")));
        lines.push(format!(
            "{}{}{}",
            border("│"),
            pad_line(&th.fg("dim", " Tab | Esc")),
            border("│")
        ));
        lines.push(border(&format!("╰{}╯", "─".repeat(inner_w))));
        lines
    }

    fn invalidate(&self) {}
}

struct StreamingControllerState {
    panels: Vec<Arc<StreamingInputPanel>>,
    handles: Vec<OverlayHandle>,
    focus_index: isize,
    stream_lines: Vec<String>,
    stream_interval: Option<pi_js::time::Interval>,
    line_count: usize,
}

struct StreamingInputControllerInner {
    tui: Arc<dyn TUI>,
    theme: Theme,
    done: DoneFn,
    mutable: Mutex<StreamingControllerState>,
}

struct StreamingInputController {
    inner: Arc<StreamingInputControllerInner>,
}

impl StreamingInputController {
    fn new(tui: Arc<dyn TUI>, theme: Theme, done: DoneFn) -> Self {
        let inner = Arc::new(StreamingInputControllerInner {
            tui: Arc::clone(&tui),
            theme: theme.clone(),
            done,
            mutable: Mutex::new(StreamingControllerState {
                panels: Vec::new(),
                handles: Vec::new(),
                focus_index: -1,
                stream_lines: Vec::new(),
                stream_interval: None,
                line_count: 0,
            }),
        });

        let colors = ["error", "success", "accent"];
        let labels = ["Panel A", "Panel B", "Panel C"];

        let mut panels = Vec::new();
        let mut handles = Vec::new();
        for i in 0..3 {
            let weak_tab = Arc::downgrade(&inner);
            let weak_close = Arc::downgrade(&inner);
            let panel = Arc::new(StreamingInputPanel::new(
                theme.clone(),
                labels[i],
                colors[i],
                Arc::new(move || {
                    if let Some(st) = weak_tab.upgrade() {
                        Self::cycle_focus_inner(&st);
                    }
                }),
                Arc::new(move || {
                    if let Some(st) = weak_close.upgrade() {
                        Self::close_inner(&st);
                    }
                }),
            ));
            let handle = tui.show_overlay(
                Arc::clone(&panel) as Arc<dyn Component>,
                Some(OverlayOptions {
                    non_capturing: Some(true),
                    row: Some(SizeValue::Absolute((1 + i * 9) as f64)),
                    col: Some(SizeValue::Absolute(2.0)),
                    width: Some(SizeValue::Absolute(35.0)),
                    ..Default::default()
                }),
            );
            panel.set_handle(handle.clone());
            panels.push(panel);
            handles.push(handle);
        }

        let weak_tick = Arc::downgrade(&inner);
        let interval = pi_js::time::set_interval(500, move || {
            let Some(st) = weak_tick.upgrade() else {
                return;
            };
            {
                let mut m = st.mutable.lock().unwrap_or_else(|e| e.into_inner());
                m.line_count += 1;
                let count = m.line_count;
                let ts = chrono::Local::now().format("%H:%M:%S").to_string();
                m.stream_lines.push(format!("[{ts}] Streaming line {count}..."));
                if m.stream_lines.len() > 8 {
                    let excess = m.stream_lines.len() - 8;
                    m.stream_lines.drain(..excess);
                }
            }
            st.tui.request_render(false);
        });

        {
            let mut m = inner.mutable.lock().unwrap_or_else(|e| e.into_inner());
            m.panels = panels;
            m.handles = handles;
            m.stream_interval = Some(interval);
        }

        Self { inner }
    }

    fn cycle_focus_inner(inner: &StreamingInputControllerInner) {
        let (old_handle, new_handle) = {
            let mut m = inner.mutable.lock().unwrap_or_else(|e| e.into_inner());
            let old_h = if m.focus_index >= 0 {
                m.handles.get(m.focus_index as usize).cloned()
            } else {
                None
            };
            m.focus_index += 1;
            if m.focus_index >= m.handles.len() as isize {
                m.focus_index = -1;
            }
            let new_h = if m.focus_index >= 0 {
                m.handles.get(m.focus_index as usize).cloned()
            } else {
                None
            };
            (old_h, new_h)
        };
        if let Some(h) = old_handle {
            h.unfocus(None);
        }
        if let Some(h) = new_handle {
            h.focus();
        }
        inner.tui.request_render(false);
    }

    fn cleanup_inner(inner: &StreamingInputControllerInner) {
        let (interval, handles) = {
            let mut m = inner.mutable.lock().unwrap_or_else(|e| e.into_inner());
            let iv = m.stream_interval.take();
            let hs = std::mem::take(&mut m.handles);
            m.panels.clear();
            (iv, hs)
        };
        if let Some(iv) = interval {
            iv.clear();
        }
        for h in handles {
            h.hide();
        }
    }

    fn close_inner(inner: &StreamingInputControllerInner) {
        Self::cleanup_inner(inner);
        (inner.done)(Value::Null);
    }
}

impl Drop for StreamingInputControllerInner {
    fn drop(&mut self) {
        StreamingInputController::cleanup_inner(self);
    }
}

impl Component for StreamingInputController {
    fn has_handle_input(&self) -> bool {
        true
    }

    fn handle_input(&self, data: &str) {
        if matches_key(data, Key::escape) || matches_key(data, &Key::ctrl("c")) {
            Self::close_inner(&self.inner);
        } else if matches_key(data, Key::tab) {
            Self::cycle_focus_inner(&self.inner);
        }
    }

    fn render(&self, width: usize) -> Vec<String> {
        let th = &self.inner.theme;
        let (focused_label, stream_lines) = {
            let m = self.inner.mutable.lock().unwrap_or_else(|e| e.into_inner());
            let label = if m.focus_index == -1 {
                th.fg("success", "Controller (this panel)")
            } else {
                m.panels
                    .get(m.focus_index as usize)
                    .map(|p| p.label.to_string())
                    .unwrap_or_else(|| "?".to_string())
            };
            (label, m.stream_lines.clone())
        };

        let dash_count = width.saturating_sub(2) / 2;
        let divider = th.fg("dim", &" ─".repeat(dash_count));

        let mut lines = vec![
            String::new(),
            format!(" Current focus: {}", th.fg("accent", &focused_label)),
            String::new(),
            " Simulated streaming output:".to_string(),
            divider.clone(),
        ];

        for line in stream_lines {
            lines.push(format!(" {}", th.fg("dim", &line)));
        }
        while lines.len() < 12 {
            lines.push(String::new());
        }

        lines.push(divider);
        lines.push(String::new());
        lines.push(format!(
            " Three {} input panels on the left.",
            th.fg("accent", "nonCapturing")
        ));
        lines.push(" Tab cycles: Controller → Panel A → B → C → Controller".to_string());
        lines.push(" Type in each panel to test input routing.".to_string());
        lines.push(String::new());
        lines.push(th.fg("dim", " Tab = cycle focus | Esc = close all"));
        lines.push(String::new());

        render_box(th, &lines, width, Some("Streaming + Input Test"))
    }

    fn invalidate(&self) {}
}

pub struct OverlayQaTestsExtension;

#[async_trait]
impl Extension for OverlayQaTestsExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        let global_toggle_handle: Arc<Mutex<Option<OverlayHandle>>> = Arc::new(Mutex::new(None));

        pi.register_command(
            "overlay-animation",
            RegisteredCommandOptions {
                description: Some("Test real-time animation in overlay (~30 FPS)".to_string()),
                get_argument_completions: None,
                handler: Arc::new(|_args, ctx| {
                    Box::pin(async move {
                        let _ = ctx
                            .as_context()
                            .ui()?
                            .custom(
                                Box::new(|tui, theme, _kb, done| {
                                    Box::pin(async move {
                                        Ok(Arc::new(AnimationDemoComponent::new(tui, theme, done))
                                            as Arc<dyn Component>)
                                    })
                                }),
                                Some(ExtensionCustomOptions {
                                    overlay: Some(true),
                                    overlay_options: Some(OverlayOptionsSource::Static(OverlayOptions {
                                        anchor: Some(OverlayAnchor::Center),
                                        width: Some(SizeValue::Absolute(50.0)),
                                        max_height: Some(SizeValue::Absolute(20.0)),
                                        ..Default::default()
                                    })),
                                    on_handle: None,
                                }),
                            )
                            .await?;
                        Ok(())
                    })
                }),
            },
        )?;

        pi.register_command(
            "overlay-anchors",
            RegisteredCommandOptions {
                description: Some("Cycle through all anchor positions".to_string()),
                get_argument_completions: None,
                handler: Arc::new(|_args, ctx| {
                    Box::pin(async move {
                        let anchors = [
                            OverlayAnchor::TopLeft,
                            OverlayAnchor::TopCenter,
                            OverlayAnchor::TopRight,
                            OverlayAnchor::LeftCenter,
                            OverlayAnchor::Center,
                            OverlayAnchor::RightCenter,
                            OverlayAnchor::BottomLeft,
                            OverlayAnchor::BottomCenter,
                            OverlayAnchor::BottomRight,
                        ];
                        let ui = ctx.as_context().ui()?;
                        let mut index = 0usize;
                        loop {
                            let anchor = anchors[index];
                            let res = ui
                                .custom(
                                    Box::new(move |_tui, theme, _kb, done| {
                                        Box::pin(async move {
                                            Ok(Arc::new(AnchorTestComponent { theme, anchor, done })
                                                as Arc<dyn Component>)
                                        })
                                    }),
                                    Some(ExtensionCustomOptions {
                                        overlay: Some(true),
                                        overlay_options: Some(OverlayOptionsSource::Static(OverlayOptions {
                                            anchor: Some(anchor),
                                            width: Some(SizeValue::Absolute(40.0)),
                                            ..Default::default()
                                        })),
                                        on_handle: None,
                                    }),
                                )
                                .await?;

                            match res.as_str() {
                                Some("next") => {
                                    index = (index + 1) % anchors.len();
                                }
                                Some("confirm") => {
                                    ui.notify(&format!("Selected: {}", anchor_label(anchor)), Some(NotifyType::Info));
                                    break;
                                }
                                _ => break,
                            }
                        }
                        Ok(())
                    })
                }),
            },
        )?;

        pi.register_command(
            "overlay-margins",
            RegisteredCommandOptions {
                description: Some("Test margin and offset options".to_string()),
                get_argument_completions: None,
                handler: Arc::new(|_args, ctx| {
                    Box::pin(async move {
                        let configs = vec![
                            MarginConfig {
                                name: "No margin (top-left)",
                                options: OverlayOptions {
                                    anchor: Some(OverlayAnchor::TopLeft),
                                    width: Some(SizeValue::Absolute(35.0)),
                                    ..Default::default()
                                },
                            },
                            MarginConfig {
                                name: "Margin: 3 all sides",
                                options: OverlayOptions {
                                    anchor: Some(OverlayAnchor::TopLeft),
                                    width: Some(SizeValue::Absolute(35.0)),
                                    margin: Some(OverlayMarginValue::All(3)),
                                    ..Default::default()
                                },
                            },
                            MarginConfig {
                                name: "Margin: top=5, left=10",
                                options: OverlayOptions {
                                    anchor: Some(OverlayAnchor::TopLeft),
                                    width: Some(SizeValue::Absolute(35.0)),
                                    margin: Some(OverlayMarginValue::Sides(OverlayMargin {
                                        top: Some(5),
                                        left: Some(10),
                                        ..Default::default()
                                    })),
                                    ..Default::default()
                                },
                            },
                            MarginConfig {
                                name: "Center + offset (10, -3)",
                                options: OverlayOptions {
                                    anchor: Some(OverlayAnchor::Center),
                                    width: Some(SizeValue::Absolute(35.0)),
                                    offset_x: Some(10),
                                    offset_y: Some(-3),
                                    ..Default::default()
                                },
                            },
                            MarginConfig {
                                name: "Bottom-right, margin: 2",
                                options: OverlayOptions {
                                    anchor: Some(OverlayAnchor::BottomRight),
                                    width: Some(SizeValue::Absolute(35.0)),
                                    margin: Some(OverlayMarginValue::All(2)),
                                    ..Default::default()
                                },
                            },
                        ];

                        let ui = ctx.as_context().ui()?;
                        let mut index = 0usize;
                        loop {
                            let cfg = &configs[index];
                            let name = cfg.name;
                            let opts = cfg.options.clone();
                            let res = ui
                                .custom(
                                    Box::new(move |_tui, theme, _kb, done| {
                                        Box::pin(async move {
                                            Ok(Arc::new(MarginTestComponent { theme, name, done })
                                                as Arc<dyn Component>)
                                        })
                                    }),
                                    Some(ExtensionCustomOptions {
                                        overlay: Some(true),
                                        overlay_options: Some(OverlayOptionsSource::Static(opts)),
                                        on_handle: None,
                                    }),
                                )
                                .await?;

                            if res.as_str() == Some("next") {
                                index = (index + 1) % configs.len();
                            } else {
                                break;
                            }
                        }
                        Ok(())
                    })
                }),
            },
        )?;

        pi.register_command(
            "overlay-stack",
            RegisteredCommandOptions {
                description: Some("Test stacked overlays".to_string()),
                get_argument_completions: None,
                handler: Arc::new(|_args, ctx| {
                    Box::pin(async move {
                        let ui = ctx.as_context().ui()?;
                        let ui1 = Arc::clone(&ui);
                        let ui2 = Arc::clone(&ui);
                        let ui3 = Arc::clone(&ui);

                        let (r1, r2, r3) = tokio::join!(
                            async move {
                                ui1.notify("Showing overlay 1 (back)...", Some(NotifyType::Info));
                                ui1.custom(
                                    Box::new(|_tui, theme, _kb, done| {
                                        Box::pin(async move {
                                            Ok(Arc::new(StackOverlayComponent {
                                                theme,
                                                num: 1,
                                                position: "back (red border)",
                                                done,
                                            }) as Arc<dyn Component>)
                                        })
                                    }),
                                    Some(ExtensionCustomOptions {
                                        overlay: Some(true),
                                        overlay_options: Some(OverlayOptionsSource::Static(OverlayOptions {
                                            anchor: Some(OverlayAnchor::Center),
                                            width: Some(SizeValue::Absolute(50.0)),
                                            offset_x: Some(-8),
                                            offset_y: Some(-4),
                                            max_height: Some(SizeValue::Absolute(15.0)),
                                            ..Default::default()
                                        })),
                                        on_handle: None,
                                    }),
                                )
                                .await
                            },
                            async move {
                                pi_js::time::sleep(400, None).await;
                                ui2.notify("Showing overlay 2 (middle)...", Some(NotifyType::Info));
                                ui2.custom(
                                    Box::new(|_tui, theme, _kb, done| {
                                        Box::pin(async move {
                                            Ok(Arc::new(StackOverlayComponent {
                                                theme,
                                                num: 2,
                                                position: "middle (green border)",
                                                done,
                                            }) as Arc<dyn Component>)
                                        })
                                    }),
                                    Some(ExtensionCustomOptions {
                                        overlay: Some(true),
                                        overlay_options: Some(OverlayOptionsSource::Static(OverlayOptions {
                                            anchor: Some(OverlayAnchor::Center),
                                            width: Some(SizeValue::Absolute(50.0)),
                                            offset_x: Some(0),
                                            offset_y: Some(0),
                                            max_height: Some(SizeValue::Absolute(15.0)),
                                            ..Default::default()
                                        })),
                                        on_handle: None,
                                    }),
                                )
                                .await
                            },
                            async move {
                                pi_js::time::sleep(800, None).await;
                                ui3.notify("Showing overlay 3 (front)...", Some(NotifyType::Info));
                                ui3.custom(
                                    Box::new(|_tui, theme, _kb, done| {
                                        Box::pin(async move {
                                            Ok(Arc::new(StackOverlayComponent {
                                                theme,
                                                num: 3,
                                                position: "front (blue border)",
                                                done,
                                            }) as Arc<dyn Component>)
                                        })
                                    }),
                                    Some(ExtensionCustomOptions {
                                        overlay: Some(true),
                                        overlay_options: Some(OverlayOptionsSource::Static(OverlayOptions {
                                            anchor: Some(OverlayAnchor::Center),
                                            width: Some(SizeValue::Absolute(50.0)),
                                            offset_x: Some(8),
                                            offset_y: Some(4),
                                            max_height: Some(SizeValue::Absolute(15.0)),
                                            ..Default::default()
                                        })),
                                        on_handle: None,
                                    }),
                                )
                                .await
                            },
                        );

                        let labels: Vec<String> = [r1?, r2?, r3?]
                            .into_iter()
                            .map(|v| v.as_str().unwrap_or("").to_string())
                            .collect();
                        ui.notify(
                            &format!("Closed in order: {}", labels.join(", ")),
                            Some(NotifyType::Info),
                        );
                        Ok(())
                    })
                }),
            },
        )?;

        pi.register_command(
            "overlay-overflow",
            RegisteredCommandOptions {
                description: Some("Test width overflow with streaming process output".to_string()),
                get_argument_completions: None,
                handler: Arc::new(|_args, ctx| {
                    Box::pin(async move {
                        let _ = ctx
                            .as_context()
                            .ui()?
                            .custom(
                                Box::new(|tui, theme, _kb, done| {
                                    Box::pin(async move {
                                        Ok(Arc::new(StreamingOverflowComponent::new(tui, theme, done))
                                            as Arc<dyn Component>)
                                    })
                                }),
                                Some(ExtensionCustomOptions {
                                    overlay: Some(true),
                                    overlay_options: Some(OverlayOptionsSource::Static(OverlayOptions {
                                        anchor: Some(OverlayAnchor::Center),
                                        width: Some(SizeValue::Absolute(90.0)),
                                        max_height: Some(SizeValue::Absolute(20.0)),
                                        ..Default::default()
                                    })),
                                    on_handle: None,
                                }),
                            )
                            .await?;
                        Ok(())
                    })
                }),
            },
        )?;

        pi.register_command(
            "overlay-edge",
            RegisteredCommandOptions {
                description: Some("Test overlay positioned at terminal edge".to_string()),
                get_argument_completions: None,
                handler: Arc::new(|_args, ctx| {
                    Box::pin(async move {
                        let _ = ctx
                            .as_context()
                            .ui()?
                            .custom(
                                Box::new(|_tui, theme, _kb, done| {
                                    Box::pin(async move {
                                        Ok(Arc::new(EdgeTestComponent { theme, done }) as Arc<dyn Component>)
                                    })
                                }),
                                Some(ExtensionCustomOptions {
                                    overlay: Some(true),
                                    overlay_options: Some(OverlayOptionsSource::Static(OverlayOptions {
                                        anchor: Some(OverlayAnchor::RightCenter),
                                        width: Some(SizeValue::Absolute(40.0)),
                                        margin: Some(OverlayMarginValue::Sides(OverlayMargin {
                                            right: Some(0),
                                            ..Default::default()
                                        })),
                                        ..Default::default()
                                    })),
                                    on_handle: None,
                                }),
                            )
                            .await?;
                        Ok(())
                    })
                }),
            },
        )?;

        pi.register_command(
            "overlay-percent",
            RegisteredCommandOptions {
                description: Some("Test percentage-based positioning".to_string()),
                get_argument_completions: None,
                handler: Arc::new(|_args, ctx| {
                    Box::pin(async move {
                        let configs = [
                            ("rowPercent: 0 (top)", 0, 50),
                            ("rowPercent: 50 (middle)", 50, 50),
                            ("rowPercent: 100 (bottom)", 100, 50),
                            ("colPercent: 0 (left)", 50, 0),
                            ("colPercent: 100 (right)", 50, 100),
                        ];
                        let ui = ctx.as_context().ui()?;
                        let mut index = 0usize;
                        loop {
                            let (name, row, col) = configs[index];
                            let res = ui
                                .custom(
                                    Box::new(move |_tui, theme, _kb, done| {
                                        Box::pin(async move {
                                            Ok(Arc::new(PercentTestComponent { theme, name, done })
                                                as Arc<dyn Component>)
                                        })
                                    }),
                                    Some(ExtensionCustomOptions {
                                        overlay: Some(true),
                                        overlay_options: Some(OverlayOptionsSource::Static(OverlayOptions {
                                            width: Some(SizeValue::Absolute(30.0)),
                                            row: Some(SizeValue::Percent(format!("{row}%"))),
                                            col: Some(SizeValue::Percent(format!("{col}%"))),
                                            ..Default::default()
                                        })),
                                        on_handle: None,
                                    }),
                                )
                                .await?;
                            if res.as_str() == Some("next") {
                                index = (index + 1) % configs.len();
                            } else {
                                break;
                            }
                        }
                        Ok(())
                    })
                }),
            },
        )?;

        pi.register_command(
            "overlay-maxheight",
            RegisteredCommandOptions {
                description: Some("Test maxHeight truncation".to_string()),
                get_argument_completions: None,
                handler: Arc::new(|_args, ctx| {
                    Box::pin(async move {
                        let _ = ctx
                            .as_context()
                            .ui()?
                            .custom(
                                Box::new(|_tui, theme, _kb, done| {
                                    Box::pin(async move {
                                        Ok(Arc::new(MaxHeightTestComponent { theme, done }) as Arc<dyn Component>)
                                    })
                                }),
                                Some(ExtensionCustomOptions {
                                    overlay: Some(true),
                                    overlay_options: Some(OverlayOptionsSource::Static(OverlayOptions {
                                        anchor: Some(OverlayAnchor::Center),
                                        width: Some(SizeValue::Absolute(50.0)),
                                        max_height: Some(SizeValue::Absolute(10.0)),
                                        ..Default::default()
                                    })),
                                    on_handle: None,
                                }),
                            )
                            .await?;
                        Ok(())
                    })
                }),
            },
        )?;

        pi.register_command(
            "overlay-sidepanel",
            RegisteredCommandOptions {
                description: Some("Test responsive sidepanel (hides when terminal < 100 cols)".to_string()),
                get_argument_completions: None,
                handler: Arc::new(|_args, ctx| {
                    Box::pin(async move {
                        let _ = ctx
                            .as_context()
                            .ui()?
                            .custom(
                                Box::new(|tui, theme, _kb, done| {
                                    Box::pin(async move {
                                        Ok(Arc::new(SidepanelComponent::new(tui, theme, done)) as Arc<dyn Component>)
                                    })
                                }),
                                Some(ExtensionCustomOptions {
                                    overlay: Some(true),
                                    overlay_options: Some(OverlayOptionsSource::Static(OverlayOptions {
                                        anchor: Some(OverlayAnchor::RightCenter),
                                        width: Some(SizeValue::Percent("25%".to_string())),
                                        min_width: Some(30),
                                        margin: Some(OverlayMarginValue::Sides(OverlayMargin {
                                            right: Some(1),
                                            ..Default::default()
                                        })),
                                        visible: Some(Arc::new(|term_width, _term_height| term_width >= 100)),
                                        ..Default::default()
                                    })),
                                    on_handle: None,
                                }),
                            )
                            .await?;
                        Ok(())
                    })
                }),
            },
        )?;

        let handle_ref = Arc::clone(&global_toggle_handle);
        pi.register_command(
            "overlay-toggle",
            RegisteredCommandOptions {
                description: Some("Test overlay toggle (press 't' to toggle visibility)".to_string()),
                get_argument_completions: None,
                handler: Arc::new(move |_args, ctx| {
                    let toggle_store = Arc::clone(&handle_ref);
                    Box::pin(async move {
                        let store_for_comp = Arc::clone(&toggle_store);
                        let store_for_cb = Arc::clone(&toggle_store);
                        let _ = ctx
                            .as_context()
                            .ui()?
                            .custom(
                                Box::new(move |tui, theme, _kb, done| {
                                    Box::pin(async move {
                                        let inner = Arc::new(ToggleDemoInner {
                                            tui,
                                            theme,
                                            done,
                                            global_handle: store_for_comp,
                                            mutable: Mutex::new(ToggleDemoState {
                                                toggle_count: 0,
                                                is_toggling: false,
                                            }),
                                        });
                                        Ok(Arc::new(ToggleDemoComponent { inner }) as Arc<dyn Component>)
                                    })
                                }),
                                Some(ExtensionCustomOptions {
                                    overlay: Some(true),
                                    overlay_options: Some(OverlayOptionsSource::Static(OverlayOptions {
                                        anchor: Some(OverlayAnchor::Center),
                                        width: Some(SizeValue::Absolute(50.0)),
                                        ..Default::default()
                                    })),
                                    on_handle: Some(Arc::new(move |handle| {
                                        *store_for_cb.lock().unwrap_or_else(|e| e.into_inner()) = Some(handle);
                                    })),
                                }),
                            )
                            .await?;
                        *toggle_store.lock().unwrap_or_else(|e| e.into_inner()) = None;
                        Ok(())
                    })
                }),
            },
        )?;

        pi.register_command(
            "overlay-passive",
            RegisteredCommandOptions {
                description: Some(
                    "Test non-capturing overlay (passive info panel alongside active overlay)".to_string(),
                ),
                get_argument_completions: None,
                handler: Arc::new(|_args, ctx| {
                    Box::pin(async move {
                        let ui = ctx.as_context().ui()?;
                        ui.set_editor_text("");
                        let _ = ui
                            .custom(
                                Box::new(|tui, theme, _kb, done| {
                                    Box::pin(async move {
                                        Ok(Arc::new(PassiveDemoController::new(tui, theme, done))
                                            as Arc<dyn Component>)
                                    })
                                }),
                                Some(ExtensionCustomOptions {
                                    overlay: Some(true),
                                    overlay_options: Some(OverlayOptionsSource::Static(OverlayOptions {
                                        anchor: Some(OverlayAnchor::Center),
                                        width: Some(SizeValue::Absolute(48.0)),
                                        ..Default::default()
                                    })),
                                    on_handle: None,
                                }),
                            )
                            .await?;
                        Ok(())
                    })
                }),
            },
        )?;

        pi.register_command(
            "overlay-focus",
            RegisteredCommandOptions {
                description: Some(
                    "Test focus cycling, input routing, dismissal, and rendering order with overlays".to_string(),
                ),
                get_argument_completions: None,
                handler: Arc::new(|_args, ctx| {
                    Box::pin(async move {
                        let ui = ctx.as_context().ui()?;
                        ui.set_editor_text("");
                        let _ = ui
                            .custom(
                                Box::new(|tui, theme, _kb, done| {
                                    Box::pin(async move {
                                        Ok(Arc::new(FocusDemoController::new(tui, theme, done)) as Arc<dyn Component>)
                                    })
                                }),
                                Some(ExtensionCustomOptions {
                                    overlay: Some(true),
                                    overlay_options: Some(OverlayOptionsSource::Static(OverlayOptions {
                                        anchor: Some(OverlayAnchor::BottomCenter),
                                        width: Some(SizeValue::Absolute(55.0)),
                                        margin: Some(OverlayMarginValue::Sides(OverlayMargin {
                                            bottom: Some(1),
                                            ..Default::default()
                                        })),
                                        ..Default::default()
                                    })),
                                    on_handle: None,
                                }),
                            )
                            .await?;
                        Ok(())
                    })
                }),
            },
        )?;

        pi.register_command(
            "overlay-streaming",
            RegisteredCommandOptions {
                description: Some("Multiple input panels with simulated streaming (Tab to cycle focus)".to_string()),
                get_argument_completions: None,
                handler: Arc::new(|_args, ctx| {
                    Box::pin(async move {
                        let ui = ctx.as_context().ui()?;
                        ui.set_editor_text("");
                        let _ = ui
                            .custom(
                                Box::new(|tui, theme, _kb, done| {
                                    Box::pin(async move {
                                        Ok(Arc::new(StreamingInputController::new(tui, theme, done))
                                            as Arc<dyn Component>)
                                    })
                                }),
                                Some(ExtensionCustomOptions {
                                    overlay: Some(true),
                                    overlay_options: Some(OverlayOptionsSource::Static(OverlayOptions {
                                        anchor: Some(OverlayAnchor::BottomCenter),
                                        width: Some(SizeValue::Absolute(60.0)),
                                        margin: Some(OverlayMarginValue::Sides(OverlayMargin {
                                            bottom: Some(1),
                                            ..Default::default()
                                        })),
                                        ..Default::default()
                                    })),
                                    on_handle: None,
                                }),
                            )
                            .await?;
                        Ok(())
                    })
                }),
            },
        )?;

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(OverlayQaTestsExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
