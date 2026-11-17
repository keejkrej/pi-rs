//! Port of packages/coding-agent/examples/rpc-extension-ui.ts

use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use pi_js::time::{Interval, set_interval, sleep};
use pi_tui::components::input::InputOptions;
use pi_tui::{
    Component, Container, Focusable, Input, ProcessTerminal, SelectItem, SelectList, SelectListTheme, TuiMainScreen,
    matches_key, truncate_to_width,
};
use serde::Deserialize;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;

const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const BLUE: &str = "\x1b[34m";
const MAGENTA: &str = "\x1b[35m";
const RED: &str = "\x1b[31m";
const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExtensionUIRequest {
    id: String,
    method: String,
    title: Option<String>,
    options: Option<Vec<String>>,
    message: Option<String>,
    placeholder: Option<String>,
    prefill: Option<String>,
    notify_type: Option<String>,
    status_key: Option<String>,
    status_text: Option<String>,
    widget_key: Option<String>,
    widget_lines: Option<Vec<String>>,
    text: Option<String>,
}

struct OutputLogInner {
    lines: Vec<String>,
    max_lines: usize,
    visible_lines: usize,
}

#[derive(Clone)]
struct OutputLog {
    inner: Arc<Mutex<OutputLogInner>>,
}

impl OutputLog {
    fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(OutputLogInner {
                lines: Vec::new(),
                max_lines: 1000,
                visible_lines: 0,
            })),
        }
    }

    #[allow(dead_code)]
    fn set_visible_lines(&self, n: usize) {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.visible_lines = n;
    }

    fn append(&self, line: &str) {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.lines.push(line.to_string());
        if guard.lines.len() > guard.max_lines {
            let start = guard.lines.len() - guard.max_lines;
            guard.lines.drain(0..start);
        }
    }

    fn append_raw(&self, text: &str) {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(last) = guard.lines.last_mut() {
            last.push_str(text);
        } else {
            guard.lines.push(text.to_string());
        }
    }
}

impl Component for OutputLog {
    fn render(&self, width: usize) -> Vec<String> {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if guard.lines.is_empty() {
            return vec![String::new()];
        }
        let n = if guard.visible_lines > 0 {
            guard.visible_lines
        } else {
            guard.lines.len()
        };
        let start = guard.lines.len().saturating_sub(n);
        guard.lines[start..]
            .iter()
            .map(|l| truncate_to_width(l, width as i64, Some(""), None))
            .collect()
    }

    fn invalidate(&self) {}
}

struct LoadingState {
    dots: usize,
    interval: Option<Interval>,
}

#[derive(Clone)]
struct LoadingIndicator {
    state: Arc<Mutex<LoadingState>>,
}

impl LoadingIndicator {
    fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(LoadingState {
                dots: 1,
                interval: None,
            })),
        }
    }

    fn start(&self, tui: Arc<TuiMainScreen>) {
        let old = {
            let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
            guard.dots = 1;
            guard.interval.take()
        };
        if let Some(iv) = old {
            iv.clear();
        }
        let state_ref = Arc::clone(&self.state);
        let iv = set_interval(400, move || {
            {
                let mut guard = state_ref.lock().unwrap_or_else(|e| e.into_inner());
                guard.dots = (guard.dots % 3) + 1;
            }
            tui.request_render(None);
        });
        let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        guard.interval = Some(iv);
    }

    fn stop(&self) {
        let old = {
            let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
            guard.interval.take()
        };
        if let Some(iv) = old {
            iv.clear();
        }
    }
}

impl Component for LoadingIndicator {
    fn render(&self, _width: usize) -> Vec<String> {
        let dots = {
            let guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
            guard.dots
        };
        vec![format!(
            "{BLUE}{BOLD}Agent:{RESET} {DIM}Working{}{RESET}",
            ".".repeat(dots)
        )]
    }

    fn invalidate(&self) {}
}

struct PromptInput {
    input: Arc<Input>,
    on_ctrl_d: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl PromptInput {
    fn new() -> Self {
        Self {
            input: Arc::new(Input::new(Some(InputOptions::default()))),
            on_ctrl_d: Mutex::new(None),
        }
    }

    fn set_on_ctrl_d(&self, cb: Arc<dyn Fn() + Send + Sync>) {
        let mut guard = self.on_ctrl_d.lock().unwrap_or_else(|e| e.into_inner());
        *guard = Some(cb);
    }
}

impl Component for PromptInput {
    fn render(&self, width: usize) -> Vec<String> {
        let mut lines = vec![format!("{GREEN}{BOLD}You:{RESET}")];
        lines.extend(self.input.render(width));
        lines
    }

    fn handle_input(&self, data: &str) {
        if matches_key(data, "ctrl+d") {
            let cb = self.on_ctrl_d.lock().unwrap_or_else(|e| e.into_inner()).clone();
            if let Some(cb) = cb {
                cb();
            }
            return;
        }
        self.input.handle_input(data);
    }

    fn has_handle_input(&self) -> bool {
        true
    }

    fn invalidate(&self) {
        self.input.invalidate();
    }

    fn as_focusable(&self) -> Option<&dyn Focusable> {
        Some(&*self.input)
    }
}

struct SelectDialog {
    list: Arc<SelectList>,
    title: String,
}

impl SelectDialog {
    fn new(
        title: String,
        options: Vec<String>,
        on_select: Arc<dyn Fn(&str) + Send + Sync>,
        on_cancel: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        let items: Vec<SelectItem> = options
            .into_iter()
            .map(|o| SelectItem {
                value: o.clone(),
                label: o,
                description: None,
            })
            .collect();
        let max_visible = items.len().clamp(1, 8) as i64;
        let theme = SelectListTheme {
            selected_prefix: Arc::new(|t| format!("{MAGENTA}{t}{RESET}")),
            selected_text: Arc::new(|t| format!("{MAGENTA}{t}{RESET}")),
            description: Arc::new(|t| format!("{DIM}{t}{RESET}")),
            scroll_info: Arc::new(|t| format!("{DIM}{t}{RESET}")),
            no_match: Arc::new(|t| format!("{YELLOW}{t}{RESET}")),
        };
        let list = Arc::new(SelectList::new(items, max_visible, theme, None));
        list.set_on_select(Some(Arc::new(move |item| {
            on_select(&item.value);
        })));
        list.set_on_cancel(Some(on_cancel));
        Self { list, title }
    }
}

impl Component for SelectDialog {
    fn render(&self, width: usize) -> Vec<String> {
        let mut lines = vec![format!("{MAGENTA}{BOLD}{}{RESET}", self.title)];
        lines.extend(self.list.render(width));
        lines.push(format!("{DIM}Up/Down, Enter to select, Esc to cancel{RESET}"));
        lines
    }

    fn handle_input(&self, data: &str) {
        self.list.handle_input(data);
    }

    fn has_handle_input(&self) -> bool {
        true
    }

    fn invalidate(&self) {
        self.list.invalidate();
    }
}

struct InputDialog {
    dialog_input: Arc<Input>,
    title: String,
    on_ctrl_d: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl InputDialog {
    fn new(
        title: String,
        prefill: Option<String>,
        on_submit: Arc<dyn Fn(&str) + Send + Sync>,
        on_escape: Arc<dyn Fn() + Send + Sync>,
        on_ctrl_d: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        let dialog_input = Arc::new(Input::new(Some(InputOptions::default())));
        if let Some(p) = prefill {
            dialog_input.set_value(&p);
        }
        dialog_input.set_on_submit(Some(on_submit));
        dialog_input.set_on_escape(Some(on_escape));
        Self {
            dialog_input,
            title,
            on_ctrl_d: Mutex::new(Some(on_ctrl_d)),
        }
    }
}

impl Component for InputDialog {
    fn render(&self, width: usize) -> Vec<String> {
        let mut lines = vec![format!("{MAGENTA}{BOLD}{}{RESET}", self.title)];
        lines.extend(self.dialog_input.render(width));
        lines.push(format!("{DIM}Enter to submit, Esc to cancel{RESET}"));
        lines
    }

    fn handle_input(&self, data: &str) {
        if matches_key(data, "ctrl+d") {
            let cb = self.on_ctrl_d.lock().unwrap_or_else(|e| e.into_inner()).clone();
            if let Some(cb) = cb {
                cb();
            }
            return;
        }
        self.dialog_input.handle_input(data);
    }

    fn has_handle_input(&self) -> bool {
        true
    }

    fn invalidate(&self) {
        self.dialog_input.invalidate();
    }

    fn as_focusable(&self) -> Option<&dyn Focusable> {
        Some(&*self.dialog_input)
    }
}

struct AppController {
    tui: Arc<TuiMainScreen>,
    root: Arc<Container>,
    output_log: OutputLog,
    loading_indicator: LoadingIndicator,
    prompt_input: Arc<PromptInput>,
    stdin_tx: mpsc::UnboundedSender<String>,
    exit_tx: mpsc::UnboundedSender<i32>,
    is_streaming: AtomicBool,
    has_text_output: AtomicBool,
    active_dialog: Mutex<Option<Arc<dyn Component>>>,
}

impl AppController {
    fn send_json(&self, value: Value) {
        let line = pi_js::json::stringify(&value);
        let _ = self.stdin_tx.send(format!("{line}\n"));
    }

    fn request_exit(&self, code: i32) {
        let _ = self.exit_tx.send(code);
    }

    fn set_bottom_component(&self, component: Arc<dyn Component>, focus: Arc<dyn Component>) {
        let streaming = self.is_streaming.load(Ordering::SeqCst);
        self.root.clear();
        self.root.add_child(Arc::new(self.output_log.clone()));
        if streaming {
            self.root.add_child(Arc::new(self.loading_indicator.clone()));
        }
        self.root.add_child(component);
        self.tui.set_focus(Some(focus));
        self.tui.request_render(None);
    }

    fn show_prompt(&self) {
        {
            let mut guard = self.active_dialog.lock().unwrap_or_else(|e| e.into_inner());
            *guard = None;
        }
        self.set_bottom_component(self.prompt_input.clone(), self.prompt_input.clone());
    }

    fn show_dialog(&self, dialog: Arc<dyn Component>) {
        {
            let mut guard = self.active_dialog.lock().unwrap_or_else(|e| e.into_inner());
            *guard = Some(dialog.clone());
        }
        self.set_bottom_component(dialog.clone(), dialog);
    }

    fn show_loading(&self) {
        if !self.is_streaming.swap(true, Ordering::SeqCst) {
            self.has_text_output.store(false, Ordering::SeqCst);
            let dialog = { self.active_dialog.lock().unwrap_or_else(|e| e.into_inner()).clone() };
            self.root.clear();
            self.root.add_child(Arc::new(self.output_log.clone()));
            self.root.add_child(Arc::new(self.loading_indicator.clone()));
            match dialog {
                Some(d) => self.root.add_child(d),
                None => {
                    self.root.add_child(self.prompt_input.clone());
                    self.tui.set_focus(Some(self.prompt_input.clone()));
                }
            }
            self.loading_indicator.start(self.tui.clone());
            self.tui.request_render(None);
        }
    }

    fn hide_loading(&self) {
        self.loading_indicator.stop();
        let dialog = { self.active_dialog.lock().unwrap_or_else(|e| e.into_inner()).clone() };
        self.root.clear();
        self.root.add_child(Arc::new(self.output_log.clone()));
        match dialog {
            Some(d) => self.root.add_child(d),
            None => {
                self.root.add_child(self.prompt_input.clone());
                self.tui.set_focus(Some(self.prompt_input.clone()));
            }
        }
        self.tui.request_render(None);
    }

    fn show_select_dialog(
        self: &Arc<Self>,
        title: String,
        options: Vec<String>,
        on_done: Arc<dyn Fn(Option<String>) + Send + Sync>,
    ) {
        let app_ok = Arc::clone(self);
        let done_ok = Arc::clone(&on_done);
        let app_cancel = Arc::clone(self);
        let done_cancel = Arc::clone(&on_done);
        let dialog = Arc::new(SelectDialog::new(
            title,
            options,
            Arc::new(move |value| {
                app_ok.show_prompt();
                done_ok(Some(value.to_string()));
            }),
            Arc::new(move || {
                app_cancel.show_prompt();
                done_cancel(None);
            }),
        ));
        self.show_dialog(dialog);
    }

    fn show_input_dialog(
        self: &Arc<Self>,
        title: String,
        prefill: Option<String>,
        on_done: Arc<dyn Fn(Option<String>) + Send + Sync>,
    ) {
        let app_ok = Arc::clone(self);
        let done_ok = Arc::clone(&on_done);
        let app_esc = Arc::clone(self);
        let done_esc = Arc::clone(&on_done);
        let app_ctrl_d = Arc::clone(self);
        let dialog = Arc::new(InputDialog::new(
            title,
            prefill,
            Arc::new(move |value| {
                app_ok.show_prompt();
                let trimmed = value.trim().to_string();
                done_ok(if trimmed.is_empty() { None } else { Some(trimmed) });
            }),
            Arc::new(move || {
                app_esc.show_prompt();
                done_esc(None);
            }),
            Arc::new(move || {
                app_ctrl_d.request_exit(0);
            }),
        ));
        self.show_dialog(dialog);
    }

    fn handle_extension_ui(self: &Arc<Self>, req: ExtensionUIRequest) {
        let id = req.id;
        match req.method.as_str() {
            "select" => {
                let app = Arc::clone(self);
                let title = req.title.unwrap_or_else(|| "Select".to_string());
                let options = req.options.unwrap_or_default();
                self.show_select_dialog(
                    title,
                    options,
                    Arc::new(move |value| {
                        if let Some(v) = value {
                            app.send_json(serde_json::json!({
                                "type": "extension_ui_response",
                                "id": id,
                                "value": v,
                            }));
                        } else {
                            app.send_json(serde_json::json!({
                                "type": "extension_ui_response",
                                "id": id,
                                "cancelled": true,
                            }));
                        }
                    }),
                );
            }
            "confirm" => {
                let app = Arc::clone(self);
                let base_title = req.title.unwrap_or_else(|| "Confirm".to_string());
                let title = match req.message {
                    Some(msg) if !msg.is_empty() => format!("{base_title}: {msg}"),
                    _ => base_title,
                };
                self.show_select_dialog(
                    title,
                    vec!["Yes".to_string(), "No".to_string()],
                    Arc::new(move |value| {
                        app.send_json(serde_json::json!({
                            "type": "extension_ui_response",
                            "id": id,
                            "confirmed": value.as_deref() == Some("Yes"),
                        }));
                    }),
                );
            }
            "input" => {
                let app = Arc::clone(self);
                let base_title = req.title.unwrap_or_else(|| "Input".to_string());
                let title = match req.placeholder {
                    Some(ph) if !ph.is_empty() => format!("{base_title} ({ph})"),
                    _ => base_title,
                };
                self.show_input_dialog(
                    title,
                    None,
                    Arc::new(move |value| {
                        if let Some(v) = value {
                            app.send_json(serde_json::json!({
                                "type": "extension_ui_response",
                                "id": id,
                                "value": v,
                            }));
                        } else {
                            app.send_json(serde_json::json!({
                                "type": "extension_ui_response",
                                "id": id,
                                "cancelled": true,
                            }));
                        }
                    }),
                );
            }
            "editor" => {
                let app = Arc::clone(self);
                let title = req.title.unwrap_or_else(|| "Editor".to_string());
                let prefill = req.prefill.map(|s| s.replace('\n', " "));
                self.show_input_dialog(
                    title,
                    prefill,
                    Arc::new(move |value| {
                        if let Some(v) = value {
                            app.send_json(serde_json::json!({
                                "type": "extension_ui_response",
                                "id": id,
                                "value": v,
                            }));
                        } else {
                            app.send_json(serde_json::json!({
                                "type": "extension_ui_response",
                                "id": id,
                                "cancelled": true,
                            }));
                        }
                    }),
                );
            }
            "notify" => {
                let notify_type = req.notify_type.as_deref().unwrap_or("info");
                let color = match notify_type {
                    "error" => RED,
                    "warning" => YELLOW,
                    _ => MAGENTA,
                };
                let msg = req.message.unwrap_or_default();
                self.output_log
                    .append(&format!("{color}{BOLD}Notification:{RESET} {msg}"));
                self.tui.request_render(None);
            }
            "setStatus" => {
                let key = req.status_key.unwrap_or_default();
                let text = req.status_text.unwrap_or_else(|| "(cleared)".to_string());
                self.output_log.append(&format!(
                    "{MAGENTA}{BOLD}Notification:{RESET} {DIM}[status: {key}]{RESET} {text}"
                ));
                self.tui.request_render(None);
            }
            "setWidget" => {
                if let Some(lines) = req.widget_lines
                    && !lines.is_empty()
                {
                    let key = req.widget_key.unwrap_or_default();
                    self.output_log.append(&format!(
                        "{MAGENTA}{BOLD}Notification:{RESET} {DIM}[widget: {key}]{RESET}"
                    ));
                    for wl in lines {
                        self.output_log.append(&format!("  {DIM}{wl}{RESET}"));
                    }
                    self.tui.request_render(None);
                }
            }
            "set_editor_text" => {
                self.prompt_input.input.set_value(req.text.as_deref().unwrap_or(""));
                self.tui.request_render(None);
            }
            _ => {}
        }
    }

    fn handle_slash_command(self: &Arc<Self>, cmd: &str) -> bool {
        match cmd {
            "/select" => {
                let app = Arc::clone(self);
                self.show_select_dialog(
                    "Pick a color".to_string(),
                    vec![
                        "Red".to_string(),
                        "Green".to_string(),
                        "Blue".to_string(),
                        "Yellow".to_string(),
                    ],
                    Arc::new(move |value| {
                        if let Some(v) = value {
                            app.output_log
                                .append(&format!("{MAGENTA}{BOLD}Notification:{RESET} You picked: {v}"));
                        } else {
                            app.output_log
                                .append(&format!("{MAGENTA}{BOLD}Notification:{RESET} Selection cancelled"));
                        }
                        app.tui.request_render(None);
                    }),
                );
                true
            }
            "/confirm" => {
                let app = Arc::clone(self);
                self.show_select_dialog(
                    "Are you sure?".to_string(),
                    vec!["Yes".to_string(), "No".to_string()],
                    Arc::new(move |value| {
                        let confirmed = value.as_deref() == Some("Yes");
                        app.output_log
                            .append(&format!("{MAGENTA}{BOLD}Notification:{RESET} Confirmed: {confirmed}"));
                        app.tui.request_render(None);
                    }),
                );
                true
            }
            "/input" => {
                let app = Arc::clone(self);
                self.show_input_dialog(
                    "Enter your name".to_string(),
                    None,
                    Arc::new(move |value| {
                        if let Some(v) = value {
                            app.output_log
                                .append(&format!("{MAGENTA}{BOLD}Notification:{RESET} You entered: {v}"));
                        } else {
                            app.output_log
                                .append(&format!("{MAGENTA}{BOLD}Notification:{RESET} Input cancelled"));
                        }
                        app.tui.request_render(None);
                    }),
                );
                true
            }
            "/editor" => {
                let app = Arc::clone(self);
                self.show_input_dialog(
                    "Edit text".to_string(),
                    Some("Hello, world!".to_string()),
                    Arc::new(move |value| {
                        if let Some(v) = value {
                            app.output_log
                                .append(&format!("{MAGENTA}{BOLD}Notification:{RESET} Submitted: {v}"));
                        } else {
                            app.output_log
                                .append(&format!("{MAGENTA}{BOLD}Notification:{RESET} Editor cancelled"));
                        }
                        app.tui.request_render(None);
                    }),
                );
                true
            }
            _ => false,
        }
    }

    fn handle_agent_line(self: &Arc<Self>, line: &str) {
        let Ok(data) = pi_js::json::parse(line) else {
            return;
        };
        let Some(event_type) = data.get("type").and_then(|v| v.as_str()) else {
            return;
        };

        if event_type == "response" && data.get("success").and_then(|v| v.as_bool()) == Some(false) {
            let cmd = data.get("command").and_then(|v| v.as_str()).unwrap_or("");
            let err = data.get("error").and_then(|v| v.as_str()).unwrap_or("");
            self.output_log.append(&format!("{RED}[error]{RESET} {cmd}: {err}"));
            self.tui.request_render(None);
            return;
        }

        match event_type {
            "agent_start" => {
                self.show_loading();
            }
            "extension_ui_request" => {
                if let Ok(req) = serde_json::from_value::<ExtensionUIRequest>(data) {
                    self.handle_extension_ui(req);
                }
            }
            "message_update" => {
                if let Some(evt) = data.get("assistantMessageEvent")
                    && evt.get("type").and_then(|v| v.as_str()) == Some("text_delta")
                    && let Some(delta) = evt.get("delta").and_then(|v| v.as_str())
                {
                    if !self.has_text_output.swap(true, Ordering::SeqCst) {
                        self.output_log.append("");
                        self.output_log.append(&format!("{BLUE}{BOLD}Agent:{RESET}"));
                    }
                    let parts: Vec<&str> = delta.split('\n').collect();
                    for (i, part) in parts.iter().enumerate() {
                        if i > 0 {
                            self.output_log.append("");
                        }
                        if !part.is_empty() {
                            self.output_log.append_raw(part);
                        }
                    }
                    self.tui.request_render(None);
                }
            }
            "tool_execution_start" => {
                let tool_name = data.get("toolName").and_then(|v| v.as_str()).unwrap_or("");
                self.output_log.append(&format!("{DIM}[tool: {tool_name}]{RESET}"));
                self.tui.request_render(None);
            }
            "tool_execution_end" => {
                let result_str = data
                    .get("result")
                    .map(pi_js::json::stringify)
                    .unwrap_or_else(|| "null".to_string());
                let preview: String = result_str.chars().take(120).collect();
                self.output_log.append(&format!("{DIM}[result: {preview}...]{RESET}"));
                self.tui.request_render(None);
            }
            "agent_settled" => {
                self.is_streaming.store(false, Ordering::SeqCst);
                self.hide_loading();
                self.output_log.append("");
                self.tui.request_render(None);
            }
            _ => {}
        }
    }
}

fn spawn_rpc_agent() -> pi_js::Result<Child> {
    if let Some(bin) = pi_js::env::var("PI_RPC_AGENT_BIN") {
        let mut cmd = Command::new(bin);
        cmd.args(["--mode", "rpc", "--no-session", "--no-extensions"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        return cmd.spawn().map_err(|e| pi_js::Error::msg(e.to_string()));
    }

    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let sibling_name = if cfg!(windows) {
            "extensions__rpc_demo.exe"
        } else {
            "extensions__rpc_demo"
        };
        let sibling = dir.join(sibling_name);
        if sibling.exists() {
            let mut cmd = Command::new(sibling);
            cmd.args(["--mode", "rpc", "--no-session", "--no-extensions"])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());
            return cmd.spawn().map_err(|e| pi_js::Error::msg(e.to_string()));
        }
    }

    let mut cmd = Command::new("cargo");
    cmd.args([
        "run",
        "--quiet",
        "-p",
        "pi-coding-agent",
        "--example",
        "extensions__rpc_demo",
        "--",
        "--mode",
        "rpc",
        "--no-session",
        "--no-extensions",
    ])
    .stdin(std::process::Stdio::piped())
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped());
    cmd.spawn().map_err(|e| pi_js::Error::msg(e.to_string()))
}

async fn run() -> pi_js::Result<i32> {
    let mut agent = spawn_rpc_agent()?;
    let stderr_buf = Arc::new(Mutex::new(String::new()));
    if let Some(stderr) = agent.stderr.take() {
        let buf_clone = Arc::clone(&stderr_buf);
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let mut guard = buf_clone.lock().unwrap_or_else(|e| e.into_inner());
                guard.push_str(&line);
                guard.push('\n');
            }
        });
    }

    let _ = sleep(500, None).await;
    if let Ok(Some(_)) = agent.try_wait() {
        let stderr_text = stderr_buf.lock().unwrap_or_else(|e| e.into_inner()).clone();
        pi_js::console::error(&format!("Agent exited immediately. Stderr:\n{stderr_text}"));
        return Ok(1);
    }

    let Some(mut child_stdin) = agent.stdin.take() else {
        return Err(pi_js::Error::msg("Failed to capture agent stdin"));
    };
    let Some(child_stdout) = agent.stdout.take() else {
        return Err(pi_js::Error::msg("Failed to capture agent stdout"));
    };

    let (stdin_tx, mut stdin_rx) = mpsc::unbounded_channel::<String>();
    tokio::spawn(async move {
        while let Some(msg) = stdin_rx.recv().await {
            if child_stdin.write_all(msg.as_bytes()).await.is_err() {
                break;
            }
            if child_stdin.flush().await.is_err() {
                break;
            }
        }
    });

    let terminal: Arc<dyn pi_tui::Terminal> = Arc::new(ProcessTerminal::new());
    let tui = Arc::new(TuiMainScreen::new(terminal, None, None));

    let output_log = OutputLog::new();
    let loading_indicator = LoadingIndicator::new();
    let prompt_input = Arc::new(PromptInput::new());

    let root = Arc::new(Container::new());
    root.add_child(Arc::new(output_log.clone()));
    root.add_child(prompt_input.clone());

    tui.add_child(root.clone());
    tui.set_focus(Some(prompt_input.clone()));

    let (exit_tx, mut exit_rx) = mpsc::unbounded_channel::<i32>();

    let app = Arc::new(AppController {
        tui: Arc::clone(&tui),
        root,
        output_log: output_log.clone(),
        loading_indicator,
        prompt_input: Arc::clone(&prompt_input),
        stdin_tx,
        exit_tx,
        is_streaming: AtomicBool::new(false),
        has_text_output: AtomicBool::new(false),
        active_dialog: Mutex::new(None),
    });

    {
        let app_submit = Arc::clone(&app);
        prompt_input.input.set_on_submit(Some(Arc::new(move |value| {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                return;
            }
            app_submit.prompt_input.input.set_value("");
            if app_submit.handle_slash_command(trimmed) {
                app_submit
                    .output_log
                    .append(&format!("{GREEN}{BOLD}You:{RESET} {trimmed}"));
                app_submit.tui.request_render(None);
                return;
            }
            app_submit
                .output_log
                .append(&format!("{GREEN}{BOLD}You:{RESET} {trimmed}"));
            app_submit.send_json(serde_json::json!({
                "type": "prompt",
                "message": trimmed,
            }));
            app_submit.tui.request_render(None);
        })));
    }

    {
        let app_ctrl_d = Arc::clone(&app);
        prompt_input.set_on_ctrl_d(Arc::new(move || {
            app_ctrl_d.request_exit(0);
        }));
    }

    {
        let app_esc = Arc::clone(&app);
        prompt_input.input.set_on_escape(Some(Arc::new(move || {
            if app_esc.is_streaming.load(Ordering::SeqCst) {
                app_esc.send_json(serde_json::json!({ "type": "abort" }));
                app_esc.output_log.append(&format!("{YELLOW}[aborted]{RESET}"));
                app_esc.tui.request_render(None);
            } else {
                app_esc.request_exit(0);
            }
        })));
    }

    let app_stdout = Arc::clone(&app);
    tokio::spawn(async move {
        let mut lines = BufReader::new(child_stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let trimmed = line.strip_suffix('\r').unwrap_or(&line);
            app_stdout.handle_agent_line(trimmed);
        }
    });

    output_log.append(&format!("{BOLD}RPC Chat{RESET}"));
    output_log.append(&format!(
        "{DIM}Type a message and press Enter. Esc to abort or exit. Ctrl+D to quit.{RESET}"
    ));
    output_log.append(&format!("{DIM}Slash commands: /select /confirm /input /editor{RESET}"));
    output_log.append("");

    tui.start();

    let exit_code = tokio::select! {
        code = exit_rx.recv() => {
            tui.stop(None);
            let _ = agent.kill().await;
            code.unwrap_or(0)
        }
        status = agent.wait() => {
            tui.stop(None);
            let code = status.ok().and_then(|s| s.code()).unwrap_or(0);
            let stderr_text = stderr_buf.lock().unwrap_or_else(|e| e.into_inner()).clone();
            if !stderr_text.is_empty() {
                pi_js::console::error(&stderr_text);
            }
            pi_js::console::log(&format!("Agent exited with code {code}"));
            code
        }
    };

    app.loading_indicator.stop();
    Ok(exit_code)
}

fn main() -> ExitCode {
    pi_js::ensure_crypto_provider();
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(err) => {
            pi_js::console::error(&format!("Failed to build runtime: {err}"));
            return ExitCode::from(1);
        }
    };
    match rt.block_on(run()) {
        Ok(code) if code == 0 => ExitCode::SUCCESS,
        Ok(code) => ExitCode::from(code.clamp(1, 255) as u8),
        Err(err) => {
            pi_js::console::error(&format!("{err}"));
            ExitCode::from(1)
        }
    }
}
