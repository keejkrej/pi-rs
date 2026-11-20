//! Port of packages/coding-agent/src/extensions/mcp/ui.ts

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::Value;

use crate::core::extensions::types::{ExtensionCommandContext, KeybindingsManager, NotifyType, Theme};
use crate::extensions::wire::{AuthUrlComponent, DynamicBorder, get_select_list_theme, key_hint};
use pi_js::abort::AbortSignal;
use pi_js::{BoxFuture, Result, Unsubscribe};
use pi_tui::components::input::Input;
use pi_tui::components::select_list::{SelectItem, SelectList};
use pi_tui::components::spacer::Spacer;
use pi_tui::components::text::Text;
use pi_tui::tui::{Component, Container, Focusable, TUI};
use pi_tui::utils::{truncate_to_width, visible_width};

const MAX_VISIBLE_ITEMS: usize = 12;

#[derive(Clone, Debug, Default)]
pub struct McpMenu {
    pub title: String,
    pub details: Option<String>,
    pub error: Option<String>,
    pub items: Vec<SelectItem>,
    pub empty: Option<String>,
    pub selected: Option<String>,
    pub confirm_label: String,
    pub cancel_label: String,
}

pub type McpSubscribeFn = Arc<dyn Fn(Arc<dyn Fn() + Send + Sync>) -> Unsubscribe + Send + Sync>;

#[async_trait]
pub trait McpUi: Send + Sync {
    async fn menu(
        &self,
        build: Arc<dyn Fn() -> McpMenu + Send + Sync>,
        subscribe: Option<McpSubscribeFn>,
    ) -> Option<String>;
    fn status(&self, title: &str, message: &str, on_cancel: Option<Arc<dyn Fn() + Send + Sync>>);
    async fn redirect_url(&self, title: &str, authorization_url: &str, signal: AbortSignal) -> Option<String>;
}

fn frame(theme: &Theme, title: &str, body: Vec<Arc<dyn Component>>, footer: Option<&str>) -> Container {
    let container = Container::new();
    let t_top = theme.clone();
    container.add_child(Arc::new(DynamicBorder::new(Arc::new(move |s| t_top.fg("accent", s)))));
    container.add_child(Arc::new(Text::new(
        Some(&theme.fg("accent", &theme.bold(title))),
        Some(1),
        Some(0),
        None,
    )));
    for child in body {
        container.add_child(child);
    }
    if let Some(foot) = footer {
        container.add_child(Arc::new(Spacer::new(Some(1))));
        container.add_child(Arc::new(Text::new(
            Some(&theme.fg("dim", foot)),
            Some(1),
            Some(0),
            None,
        )));
    }
    let t_bot = theme.clone();
    container.add_child(Arc::new(DynamicBorder::new(Arc::new(move |s| t_bot.fg("accent", s)))));
    container
}

#[derive(Clone)]
enum ViewInputHandler {
    Component(Arc<dyn Component>),
    Callback(Arc<dyn Fn(&str) + Send + Sync>),
}

struct McpManagerViewState {
    content: Container,
    input_handler: Option<ViewInputHandler>,
    input_target: Option<Arc<dyn Focusable>>,
    focused: bool,
}

#[derive(Clone)]
pub struct McpManagerView {
    tui: Arc<dyn TUI>,
    theme: Theme,
    keybindings: KeybindingsManager,
    state: Arc<Mutex<McpManagerViewState>>,
}

impl McpManagerView {
    pub fn new(tui: Arc<dyn TUI>, theme: Theme, keybindings: KeybindingsManager) -> Self {
        let content = frame(
            &theme,
            "MCP servers",
            vec![Arc::new(Text::new(
                Some(&theme.fg("muted", "Loading…")),
                Some(1),
                Some(1),
                None,
            ))],
            None,
        );
        Self {
            tui,
            theme,
            keybindings,
            state: Arc::new(Mutex::new(McpManagerViewState {
                content,
                input_handler: None,
                input_target: None,
                focused: false,
            })),
        }
    }

    fn set_content(
        &self,
        content: Container,
        input_handler: Option<ViewInputHandler>,
        input_target: Option<Arc<dyn Focusable>>,
    ) {
        {
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ref prev) = st.input_target {
                prev.set_focused(false);
            }
            st.content = content;
            st.input_handler = input_handler;
            st.input_target = input_target;
            let focused = st.focused;
            if let Some(ref target) = st.input_target {
                target.set_focused(focused);
            }
        }
        self.tui.request_render(None);
    }
}

impl Focusable for McpManagerView {
    fn focused(&self) -> bool {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).focused
    }

    fn set_focused(&self, focused: bool) {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        st.focused = focused;
        if let Some(ref target) = st.input_target {
            target.set_focused(focused);
        }
    }
}

impl Component for McpManagerView {
    fn captures_viewport_input(&self) -> bool {
        match &self.state.lock().unwrap_or_else(|e| e.into_inner()).input_handler {
            Some(ViewInputHandler::Component(comp)) => comp.captures_viewport_input(),
            _ => false,
        }
    }

    fn render(&self, width: usize) -> Vec<String> {
        let content = self.state.lock().unwrap_or_else(|e| e.into_inner()).content.clone();
        content
            .render(width)
            .into_iter()
            .map(|line| {
                if visible_width(&line) > width as i64 {
                    truncate_to_width(&line, width as i64, Some(""), None)
                } else {
                    line
                }
            })
            .collect()
    }

    fn handle_input(&self, data: &str) {
        let handler = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .input_handler
            .clone();
        match handler {
            Some(ViewInputHandler::Component(comp)) => comp.handle_input(data),
            Some(ViewInputHandler::Callback(f)) => f(data),
            None => {}
        }
        self.tui.request_render(None);
    }

    fn invalidate(&self) {
        let content = self.state.lock().unwrap_or_else(|e| e.into_inner()).content.clone();
        content.invalidate();
    }
}

#[async_trait]
impl McpUi for McpManagerView {
    async fn menu(
        &self,
        build: Arc<dyn Fn() -> McpMenu + Send + Sync>,
        subscribe: Option<McpSubscribeFn>,
    ) -> Option<String> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let tx_slot = Arc::new(Mutex::new(Some(tx)));
        let unsub_slot: Arc<Mutex<Option<Unsubscribe>>> = Arc::new(Mutex::new(None));
        let selected_slot: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

        let finish: Arc<dyn Fn(Option<String>) + Send + Sync> = {
            let tx_slot = Arc::clone(&tx_slot);
            let unsub_slot = Arc::clone(&unsub_slot);
            Arc::new(move |val| {
                if let Some(u) = unsub_slot.lock().unwrap_or_else(|e| e.into_inner()).take() {
                    u.call();
                }
                if let Some(sender) = tx_slot.lock().unwrap_or_else(|e| e.into_inner()).take() {
                    let _ = sender.send(val);
                }
            })
        };

        let render_menu: Arc<dyn Fn() + Send + Sync> = {
            let this = self.clone();
            let build = Arc::clone(&build);
            let selected_slot = Arc::clone(&selected_slot);
            let finish = Arc::clone(&finish);
            Arc::new(move || {
                let menu = build();
                let wanted = selected_slot
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone()
                    .or_else(|| menu.selected.clone());
                let mut body: Vec<Arc<dyn Component>> = Vec::new();
                if let Some(ref det) = menu.details
                    && !det.is_empty()
                {
                    body.push(Arc::new(Text::new(
                        Some(&this.theme.fg("muted", det)),
                        Some(1),
                        Some(0),
                        None,
                    )));
                }
                if let Some(ref err) = menu.error
                    && !err.is_empty()
                {
                    body.push(Arc::new(Text::new(
                        Some(&this.theme.fg("error", err)),
                        Some(1),
                        Some(0),
                        None,
                    )));
                }
                body.push(Arc::new(Spacer::new(Some(1))));
                if menu.items.is_empty() {
                    body.push(Arc::new(Text::new(
                        Some(
                            &this
                                .theme
                                .fg("muted", menu.empty.as_deref().unwrap_or("Nothing to show.")),
                        ),
                        Some(1),
                        Some(0),
                        None,
                    )));
                    let kb = this.keybindings.clone();
                    let finish_cancel = Arc::clone(&finish);
                    let footer = key_hint("tui.select.cancel", &menu.cancel_label, &this.theme);
                    this.set_content(
                        frame(&this.theme, &menu.title, body, Some(&footer)),
                        Some(ViewInputHandler::Callback(Arc::new(move |data| {
                            if kb.matches(data, "tui.select.cancel") {
                                finish_cancel(None);
                            }
                        }))),
                        None,
                    );
                    return;
                }
                let footer = format!(
                    "{} • {}",
                    key_hint("tui.select.confirm", &menu.confirm_label, &this.theme),
                    key_hint("tui.select.cancel", &menu.cancel_label, &this.theme),
                );
                let max_vis = menu.items.len().min(MAX_VISIBLE_ITEMS) as i64;
                let list = SelectList::new(menu.items.clone(), max_vis, get_select_list_theme(&this.theme), None);
                if let Some(ref w) = wanted
                    && let Some(idx) = menu.items.iter().position(|item| item.value == *w)
                {
                    list.set_selected_index(idx as i64);
                }
                *selected_slot.lock().unwrap_or_else(|e| e.into_inner()) = list.get_selected_item().map(|i| i.value);
                let sel_change = Arc::clone(&selected_slot);
                list.set_on_selection_change(Some(Arc::new(move |item| {
                    *sel_change.lock().unwrap_or_else(|e| e.into_inner()) = Some(item.value.clone());
                })));
                let f_sel = Arc::clone(&finish);
                list.set_on_select(Some(Arc::new(move |item| f_sel(Some(item.value.clone())))));
                let f_can = Arc::clone(&finish);
                list.set_on_cancel(Some(Arc::new(move || f_can(None))));
                let list_comp: Arc<dyn Component> = Arc::new(list);
                body.push(Arc::clone(&list_comp));
                this.set_content(
                    frame(&this.theme, &menu.title, body, Some(&footer)),
                    Some(ViewInputHandler::Component(list_comp)),
                    None,
                );
            })
        };

        render_menu();
        if let Some(sub) = subscribe {
            let r = Arc::clone(&render_menu);
            let tx_check = Arc::clone(&tx_slot);
            let unsub = sub(Arc::new(move || {
                if tx_check.lock().unwrap_or_else(|e| e.into_inner()).is_some() {
                    r();
                }
            }));
            *unsub_slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(unsub);
        }

        rx.await.ok().flatten()
    }

    fn status(&self, title: &str, message: &str, on_cancel: Option<Arc<dyn Fn() + Send + Sync>>) {
        let body: Vec<Arc<dyn Component>> = vec![
            Arc::new(Spacer::new(Some(1))),
            Arc::new(Text::new(
                Some(&self.theme.fg("muted", message)),
                Some(1),
                Some(0),
                None,
            )),
        ];
        if let Some(cancel_cb) = on_cancel {
            let kb = self.keybindings.clone();
            let footer = key_hint("tui.select.cancel", "cancel", &self.theme);
            self.set_content(
                frame(&self.theme, title, body, Some(&footer)),
                Some(ViewInputHandler::Callback(Arc::new(move |data| {
                    if kb.matches(data, "tui.select.cancel") {
                        cancel_cb();
                    }
                }))),
                None,
            );
        } else {
            self.set_content(frame(&self.theme, title, body, None), None, None);
        }
    }

    async fn redirect_url(&self, title: &str, authorization_url: &str, signal: AbortSignal) -> Option<String> {
        if signal.aborted() {
            return None;
        }
        let (tx, rx) = tokio::sync::oneshot::channel();
        let tx_slot = Arc::new(Mutex::new(Some(tx)));
        let finish: Arc<dyn Fn(Option<String>) + Send + Sync> = {
            let tx_slot = Arc::clone(&tx_slot);
            Arc::new(move |val| {
                if let Some(sender) = tx_slot.lock().unwrap_or_else(|e| e.into_inner()).take() {
                    let _ = sender.send(val);
                }
            })
        };
        let f_abort = Arc::clone(&finish);
        let _guard = signal.on_abort(move |_| f_abort(None));

        let input = Input::new(None);
        let link = AuthUrlComponent::new(Arc::clone(&self.tui), authorization_url);
        let body: Vec<Arc<dyn Component>> = vec![
            Arc::new(Spacer::new(Some(1))),
            Arc::new(Text::new(
                Some(
                    &self
                        .theme
                        .fg("muted", "Approve access in your browser. If it did not open, visit:"),
                ),
                Some(1),
                Some(0),
                None,
            )),
            Arc::new(link.clone()),
            Arc::new(Spacer::new(Some(1))),
            Arc::new(Text::new(
                Some(&self.theme.fg(
                    "muted",
                    "If the browser runs on another machine, paste the URL it was redirected to:",
                )),
                Some(1),
                Some(0),
                None,
            )),
            Arc::new(input.clone()),
        ];

        let footer = format!(
            "{} • {}",
            key_hint("tui.select.confirm", "submit", &self.theme),
            key_hint("tui.select.cancel", "cancel", &self.theme),
        );
        let kb = self.keybindings.clone();
        let inp_handler = input.clone();
        self.set_content(
            frame(&self.theme, title, body, Some(&footer)),
            Some(ViewInputHandler::Callback(Arc::new(move |data| {
                if kb.matches(data, "tui.select.confirm") {
                    let val = pi_js::str16::trim(&inp_handler.get_value()).to_string();
                    if !val.is_empty() {
                        finish(Some(val));
                    }
                    return;
                }
                if kb.matches(data, "tui.select.cancel") {
                    finish(None);
                    return;
                }
                if kb.matches(data, "app.message.copy") {
                    let link_copy = link.clone();
                    let copy_override =
                        crate::modes::interactive::components::login_dialog::current_copy_to_clipboard_override();
                    if let Ok(handle) = tokio::runtime::Handle::try_current() {
                        handle.spawn(async move {
                            link_copy.copy_with_override(copy_override).await;
                        });
                    }
                    return;
                }
                inp_handler.handle_input(data);
            }))),
            Some(Arc::new(input)),
        );

        rx.await.ok().flatten()
    }
}

pub async fn show_mcp_manager<F>(ctx: &ExtensionCommandContext, manage: F) -> Result<()>
where
    F: FnOnce(Arc<dyn McpUi>) -> BoxFuture<Result<()>> + Send + 'static,
{
    let ui = ctx.base.ui()?;
    let ui_notify = Arc::clone(&ui);
    ui.custom(
        Box::new(move |tui, theme, keybindings, done| {
            Box::pin(async move {
                let view = McpManagerView::new(tui, theme, keybindings);
                let ui_handle: Arc<dyn McpUi> = Arc::new(view.clone());
                let notify = Arc::clone(&ui_notify);
                tokio::spawn(async move {
                    if let Err(err) = manage(ui_handle).await {
                        notify.notify(&err.message(), Some(NotifyType::Error));
                    }
                    done(Value::Null);
                });
                Ok(Arc::new(view) as Arc<dyn Component>)
            })
        }),
        None,
    )
    .await?;
    Ok(())
}
