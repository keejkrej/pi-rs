//! Port of packages/coding-agent/src/modes/interactive/tui-renderer.ts

use std::sync::Arc;

use async_trait::async_trait;
use pi_tui::tui::{
    OverlayHandle, OverlayOptions, QueryTerminalColorsOptions, TUI, TuiInputListener, TuiMode, TuiStopOptions,
    ViewportTUI,
};
use pi_tui::tui_alt_screen::{CopySelectionFn, CopySelectionResult, TuiAltScreenOptions};
use pi_tui::{
    Component, ProcessTerminal, Terminal, TerminalColorScheme, TerminalColors, TuiAltScreen, TuiMainScreen,
    TuiMouseEvent, TuiMouseEventResult, WheelScrollLines,
};

use super::components::keybinding_hints::key_display_text;
use super::theme::theme::{ThemeBg, ThemeColor, theme};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InteractiveTuiMode {
    Regular,
    Fullscreen,
}

#[derive(Clone, Default)]
pub struct InteractiveTuiOptions {
    pub tui_mode: Option<InteractiveTuiMode>,
    pub show_hardware_cursor: bool,
    pub log_directory: String,
    pub terminal: Option<Arc<dyn Terminal>>,
    pub on_right_click_paste: Option<Arc<dyn Fn() + Send + Sync>>,
    pub fullscreen_copy_on_select: Option<bool>,
    pub fullscreen_wheel_scroll_lines: Option<WheelScrollLines>,
}

fn open_browser_wire(_target: &str) {
    // WIRE(@tui-w3): Replace with `crate::utils::open_browser::open_browser` once @mcp merges onto main.
}

async fn copy_to_clipboard_wire(_text: &str) -> pi_js::Result<()> {
    // WIRE(@tui-w3): Replace with `crate::utils::clipboard::copy_to_clipboard` once @mcp merges onto main.
    Ok(())
}

/// Composition root TUI handle shared by coding-agent presentations.
#[derive(Clone)]
pub enum InteractiveTui {
    Main(Arc<TuiMainScreen>),
    Alt(Arc<TuiAltScreen>),
}

impl InteractiveTui {
    pub fn as_alt_screen(&self) -> Option<&Arc<TuiAltScreen>> {
        match self {
            Self::Alt(alt) => Some(alt),
            Self::Main(_) => None,
        }
    }

    pub fn as_tui_arc(&self) -> Arc<dyn TUI + Send + Sync> {
        match self {
            Self::Main(main) => Arc::clone(main) as Arc<dyn TUI + Send + Sync>,
            Self::Alt(alt) => Arc::clone(alt) as Arc<dyn TUI + Send + Sync>,
        }
    }

    pub fn get_focused_component(&self) -> Option<Arc<dyn Component>> {
        match self {
            Self::Main(main) => main.get_focused_component(),
            Self::Alt(alt) => alt.get_focused_component(),
        }
    }

    fn as_tui(&self) -> &(dyn TUI + Send + Sync) {
        match self {
            Self::Main(main) => main.as_ref(),
            Self::Alt(alt) => alt.as_ref(),
        }
    }
}

impl Component for InteractiveTui {
    fn render(&self, width: usize) -> Vec<String> {
        self.as_tui().render(width)
    }

    fn handle_input(&self, data: &str) {
        self.as_tui().handle_input(data)
    }

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.as_tui().handle_mouse(event)
    }

    fn wants_key_release(&self) -> bool {
        self.as_tui().wants_key_release()
    }

    fn invalidate(&self) {
        self.as_tui().invalidate()
    }

    fn has_handle_input(&self) -> bool {
        self.as_tui().has_handle_input()
    }

    fn is_container_handle_mouse(&self) -> bool {
        self.as_tui().is_container_handle_mouse()
    }

    fn container_children(&self) -> Option<Vec<Arc<dyn Component>>> {
        self.as_tui().container_children()
    }
}

#[async_trait]
impl TUI for InteractiveTui {
    fn mode(&self) -> TuiMode {
        self.as_tui().mode()
    }
    fn children(&self) -> Vec<Arc<dyn Component>> {
        self.as_tui().children()
    }
    fn set_children(&self, children: Vec<Arc<dyn Component>>) {
        self.as_tui().set_children(children)
    }
    fn terminal(&self) -> Arc<dyn Terminal> {
        self.as_tui().terminal()
    }
    fn on_debug(&self) -> Option<Arc<dyn Fn() + Send + Sync>> {
        self.as_tui().on_debug()
    }
    fn set_on_debug(&self, on_debug: Option<Arc<dyn Fn() + Send + Sync>>) {
        self.as_tui().set_on_debug(on_debug)
    }
    fn full_redraws(&self) -> i64 {
        self.as_tui().full_redraws()
    }
    fn add_child(&self, component: Arc<dyn Component>) {
        self.as_tui().add_child(component)
    }
    fn remove_child(&self, component: &Arc<dyn Component>) {
        self.as_tui().remove_child(component)
    }
    fn clear(&self) {
        self.as_tui().clear()
    }
    fn get_show_hardware_cursor(&self) -> bool {
        self.as_tui().get_show_hardware_cursor()
    }
    fn set_show_hardware_cursor(&self, enabled: bool) {
        self.as_tui().set_show_hardware_cursor(enabled)
    }
    fn get_clear_on_shrink(&self) -> bool {
        self.as_tui().get_clear_on_shrink()
    }
    fn set_clear_on_shrink(&self, enabled: bool) {
        self.as_tui().set_clear_on_shrink(enabled)
    }
    fn set_focus(&self, component: Option<Arc<dyn Component>>) {
        self.as_tui().set_focus(component)
    }
    fn show_overlay(&self, component: Arc<dyn Component>, options: Option<OverlayOptions>) -> OverlayHandle {
        self.as_tui().show_overlay(component, options)
    }
    fn hide_overlay(&self) {
        self.as_tui().hide_overlay()
    }
    fn has_overlay(&self) -> bool {
        self.as_tui().has_overlay()
    }
    fn start(&self) {
        self.as_tui().start()
    }
    fn stop(&self, options: Option<TuiStopOptions>) {
        self.as_tui().stop(options)
    }
    fn render_now(&self, force: Option<bool>) {
        self.as_tui().render_now(force)
    }
    fn request_render(&self, force: Option<bool>) {
        self.as_tui().request_render(force)
    }
    fn add_input_listener(&self, listener: TuiInputListener) -> pi_js::Unsubscribe {
        self.as_tui().add_input_listener(listener)
    }
    fn remove_input_listener(&self, listener: &TuiInputListener) {
        self.as_tui().remove_input_listener(listener)
    }
    fn on_terminal_color_scheme_change(
        &self,
        listener: Arc<dyn Fn(TerminalColorScheme) + Send + Sync>,
    ) -> pi_js::Unsubscribe {
        self.as_tui().on_terminal_color_scheme_change(listener)
    }
    fn set_terminal_color_scheme_notifications(&self, enabled: bool) {
        self.as_tui().set_terminal_color_scheme_notifications(enabled)
    }
    async fn query_terminal_colors(&self, options: QueryTerminalColorsOptions) -> TerminalColors {
        self.as_tui().query_terminal_colors(options).await
    }
    fn as_viewport(&self) -> Option<&dyn ViewportTUI> {
        self.as_tui().as_viewport()
    }
}

pub fn create_interactive_tui(options: InteractiveTuiOptions) -> InteractiveTui {
    let terminal: Arc<dyn Terminal> = options.terminal.unwrap_or_else(|| Arc::new(ProcessTerminal::new()));
    let mode = options.tui_mode.unwrap_or(InteractiveTuiMode::Regular);
    let log_dir = (!options.log_directory.is_empty()).then_some(options.log_directory.as_str());

    if mode == InteractiveTuiMode::Fullscreen {
        let style_search_match = |text: &str| {
            let t = theme();
            t.bg(ThemeBg::SearchMatchBg, &t.fg(ThemeColor::SearchMatchText, text))
        };
        let copy_selection: CopySelectionFn = Arc::new(|text: String| {
            Box::pin(async move {
                match copy_to_clipboard_wire(&text).await {
                    Ok(()) => CopySelectionResult::Bool(true),
                    Err(err) => CopySelectionResult::Message(err.to_string()),
                }
            })
        });
        let alt_opts = TuiAltScreenOptions {
            wheel_scroll_lines: Some(options.fullscreen_wheel_scroll_lines.unwrap_or(WheelScrollLines::AUTO)),
            mouse: None,
            search_match_style: Some(Arc::new(move |text: &str| theme().underline(&style_search_match(text)))),
            search_current_match_style: Some(Arc::new(move |text: &str| {
                let t = theme();
                t.bold(&t.inverse(&style_search_match(text)))
            })),
            search_navigation_button_style: Some(Arc::new(move |text: &str, hovered: bool| {
                if hovered {
                    theme().underline(text)
                } else {
                    text.to_string()
                }
            })),
            scroll_to_end_indicator: Some(Arc::new(|| {
                let shortcut = key_display_text(&"tui.altScreen.bottom".to_string());
                let suffix = if shortcut.is_empty() {
                    String::new()
                } else {
                    format!(" · {shortcut}")
                };
                let label = format!(" ↓ Jump to latest message{suffix} ");
                let t = theme();
                t.bg(ThemeBg::SelectedBg, &t.fg(ThemeColor::Text, &label))
            })),
            open_url: Some(Arc::new(|url: &str| open_browser_wire(url))),
            on_right_click_paste: options.on_right_click_paste,
            copy_on_select: options.fullscreen_copy_on_select,
            copy_selection: Some(copy_selection),
        };
        return InteractiveTui::Alt(Arc::new(TuiAltScreen::new(
            terminal,
            Some(options.show_hardware_cursor),
            log_dir,
            Some(alt_opts),
        )));
    }

    InteractiveTui::Main(Arc::new(TuiMainScreen::new(
        terminal,
        Some(options.show_hardware_cursor),
        log_dir,
    )))
}

/// Stable reference for components while `InteractiveMode` replaces the active renderer.
#[derive(Clone)]
pub struct InteractiveTuiReference {
    get_tui: Arc<dyn Fn() -> Arc<dyn TUI + Send + Sync> + Send + Sync>,
}

pub fn create_interactive_tui_reference(
    get_tui: Arc<dyn Fn() -> Arc<dyn TUI + Send + Sync> + Send + Sync>,
) -> InteractiveTuiReference {
    InteractiveTuiReference { get_tui }
}

impl Component for InteractiveTuiReference {
    fn render(&self, width: usize) -> Vec<String> {
        (self.get_tui)().render(width)
    }
    fn handle_input(&self, data: &str) {
        (self.get_tui)().handle_input(data)
    }
    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        (self.get_tui)().handle_mouse(event)
    }
    fn wants_key_release(&self) -> bool {
        (self.get_tui)().wants_key_release()
    }
    fn invalidate(&self) {
        (self.get_tui)().invalidate()
    }
    fn has_handle_input(&self) -> bool {
        (self.get_tui)().has_handle_input()
    }
    fn is_container_handle_mouse(&self) -> bool {
        (self.get_tui)().is_container_handle_mouse()
    }
    fn container_children(&self) -> Option<Vec<Arc<dyn Component>>> {
        (self.get_tui)().container_children()
    }
}

#[async_trait]
impl TUI for InteractiveTuiReference {
    fn mode(&self) -> TuiMode {
        (self.get_tui)().mode()
    }
    fn children(&self) -> Vec<Arc<dyn Component>> {
        (self.get_tui)().children()
    }
    fn set_children(&self, children: Vec<Arc<dyn Component>>) {
        (self.get_tui)().set_children(children)
    }
    fn terminal(&self) -> Arc<dyn Terminal> {
        (self.get_tui)().terminal()
    }
    fn on_debug(&self) -> Option<Arc<dyn Fn() + Send + Sync>> {
        (self.get_tui)().on_debug()
    }
    fn set_on_debug(&self, on_debug: Option<Arc<dyn Fn() + Send + Sync>>) {
        (self.get_tui)().set_on_debug(on_debug)
    }
    fn full_redraws(&self) -> i64 {
        (self.get_tui)().full_redraws()
    }
    fn add_child(&self, component: Arc<dyn Component>) {
        (self.get_tui)().add_child(component)
    }
    fn remove_child(&self, component: &Arc<dyn Component>) {
        (self.get_tui)().remove_child(component)
    }
    fn clear(&self) {
        (self.get_tui)().clear()
    }
    fn get_show_hardware_cursor(&self) -> bool {
        (self.get_tui)().get_show_hardware_cursor()
    }
    fn set_show_hardware_cursor(&self, enabled: bool) {
        (self.get_tui)().set_show_hardware_cursor(enabled)
    }
    fn get_clear_on_shrink(&self) -> bool {
        (self.get_tui)().get_clear_on_shrink()
    }
    fn set_clear_on_shrink(&self, enabled: bool) {
        (self.get_tui)().set_clear_on_shrink(enabled)
    }
    fn set_focus(&self, component: Option<Arc<dyn Component>>) {
        (self.get_tui)().set_focus(component)
    }
    fn show_overlay(&self, component: Arc<dyn Component>, options: Option<OverlayOptions>) -> OverlayHandle {
        (self.get_tui)().show_overlay(component, options)
    }
    fn hide_overlay(&self) {
        (self.get_tui)().hide_overlay()
    }
    fn has_overlay(&self) -> bool {
        (self.get_tui)().has_overlay()
    }
    fn start(&self) {
        (self.get_tui)().start()
    }
    fn stop(&self, options: Option<TuiStopOptions>) {
        (self.get_tui)().stop(options)
    }
    fn render_now(&self, force: Option<bool>) {
        (self.get_tui)().render_now(force)
    }
    fn request_render(&self, force: Option<bool>) {
        (self.get_tui)().request_render(force)
    }
    fn add_input_listener(&self, listener: TuiInputListener) -> pi_js::Unsubscribe {
        (self.get_tui)().add_input_listener(listener)
    }
    fn remove_input_listener(&self, listener: &TuiInputListener) {
        (self.get_tui)().remove_input_listener(listener)
    }
    fn on_terminal_color_scheme_change(
        &self,
        listener: Arc<dyn Fn(TerminalColorScheme) + Send + Sync>,
    ) -> pi_js::Unsubscribe {
        (self.get_tui)().on_terminal_color_scheme_change(listener)
    }
    fn set_terminal_color_scheme_notifications(&self, enabled: bool) {
        (self.get_tui)().set_terminal_color_scheme_notifications(enabled)
    }
    async fn query_terminal_colors(&self, options: QueryTerminalColorsOptions) -> TerminalColors {
        (self.get_tui)().query_terminal_colors(options).await
    }
    fn as_viewport(&self) -> Option<&dyn ViewportTUI> {
        // Static trait limitation without dynamic reference lifetime: only forward concrete ViewportTUI when held directly,
        // wait: `TuiBase` checks `as_viewport()` via `TUI`; cannot return borrowed lifetime from closure return value!
        None
    }
}
