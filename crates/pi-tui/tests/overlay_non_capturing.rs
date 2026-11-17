//! Port of packages/tui/test/overlay-non-capturing.test.ts

mod support;

use std::sync::{Arc, Mutex};

use pi_tui::tui::{Component, Container, Focusable, OverlayOptions, OverlayUnfocusOptions, SizeValue};
use pi_tui::tui_main_screen::TuiMainScreen;

use support::virtual_terminal::VirtualTerminal;

struct StaticOverlay {
    lines: Vec<String>,
}

impl StaticOverlay {
    fn new(lines: Vec<String>) -> Self {
        Self { lines }
    }
}

impl Component for StaticOverlay {
    fn render(&self, _width: usize) -> Vec<String> {
        self.lines.clone()
    }

    fn invalidate(&self) {}
}

struct EmptyContent;

impl Component for EmptyContent {
    fn render(&self, _width: usize) -> Vec<String> {
        Vec::new()
    }

    fn invalidate(&self) {}
}

type InputHook = Arc<dyn Fn(&str) + Send + Sync>;

struct FocusableOverlay {
    focused: Mutex<bool>,
    inputs: Mutex<Vec<String>>,
    lines: Vec<String>,
    custom_handle_input: Mutex<Option<InputHook>>,
}

impl FocusableOverlay {
    fn new(lines: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            focused: Mutex::new(false),
            inputs: Mutex::new(Vec::new()),
            lines: lines.iter().map(|s| (*s).to_string()).collect(),
            custom_handle_input: Mutex::new(None),
        })
    }

    fn focused(&self) -> bool {
        *self.focused.lock().unwrap()
    }

    fn inputs(&self) -> Vec<String> {
        self.inputs.lock().unwrap().clone()
    }

    fn set_custom_handle_input<F>(&self, hook: F)
    where
        F: Fn(&str) + Send + Sync + 'static,
    {
        *self.custom_handle_input.lock().unwrap() = Some(Arc::new(hook));
    }
}

impl Focusable for FocusableOverlay {
    fn focused(&self) -> bool {
        *self.focused.lock().unwrap()
    }

    fn set_focused(&self, focused: bool) {
        *self.focused.lock().unwrap() = focused;
    }
}

impl Component for FocusableOverlay {
    fn render(&self, _width: usize) -> Vec<String> {
        self.lines.clone()
    }

    fn has_handle_input(&self) -> bool {
        true
    }

    fn handle_input(&self, data: &str) {
        let hook = self.custom_handle_input.lock().unwrap().clone();
        if let Some(hook) = hook {
            hook(data);
        } else {
            self.inputs.lock().unwrap().push(data.to_string());
        }
    }

    fn invalidate(&self) {}

    fn as_focusable(&self) -> Option<&dyn Focusable> {
        Some(self)
    }
}

async fn render_and_flush(tui: &TuiMainScreen, terminal: &VirtualTerminal) {
    tui.request_render(Some(true));
    tokio::task::yield_now().await;
    terminal.wait_for_render().await;
}

mod focus_management {
    use super::*;

    #[tokio::test]
    async fn non_capturing_overlay_preserves_focus_on_creation() {
        let terminal = Arc::new(VirtualTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let editor = FocusableOverlay::new(&["EDITOR"]);
        let overlay = FocusableOverlay::new(&["OVERLAY"]);
        tui.add_child(Arc::new(EmptyContent));
        tui.set_focus(Some(editor.clone()));
        tui.start();
        tui.show_overlay(
            overlay.clone(),
            Some(OverlayOptions {
                non_capturing: Some(true),
                ..Default::default()
            }),
        );
        render_and_flush(&tui, &terminal).await;
        assert!(editor.focused());
        assert!(!overlay.focused());
        tui.stop(None);
    }

    #[tokio::test]
    async fn focus_transfers_focus_to_the_overlay() {
        let terminal = Arc::new(VirtualTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let editor = FocusableOverlay::new(&["EDITOR"]);
        let overlay = FocusableOverlay::new(&["OVERLAY"]);
        tui.add_child(Arc::new(EmptyContent));
        tui.set_focus(Some(editor.clone()));
        tui.start();
        let handle = tui.show_overlay(
            overlay.clone(),
            Some(OverlayOptions {
                non_capturing: Some(true),
                ..Default::default()
            }),
        );
        handle.focus();
        render_and_flush(&tui, &terminal).await;
        assert!(!editor.focused());
        assert!(overlay.focused());
        assert!(handle.is_focused());
        tui.stop(None);
    }

    #[tokio::test]
    async fn unfocus_restores_previous_focus() {
        let terminal = Arc::new(VirtualTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let editor = FocusableOverlay::new(&["EDITOR"]);
        let overlay = FocusableOverlay::new(&["OVERLAY"]);
        tui.add_child(Arc::new(EmptyContent));
        tui.set_focus(Some(editor.clone()));
        tui.start();
        let handle = tui.show_overlay(
            overlay.clone(),
            Some(OverlayOptions {
                non_capturing: Some(true),
                ..Default::default()
            }),
        );
        handle.focus();
        handle.unfocus(None);
        render_and_flush(&tui, &terminal).await;
        assert!(editor.focused());
        assert!(!overlay.focused());
        assert!(!handle.is_focused());
        tui.stop(None);
    }

    #[tokio::test]
    async fn set_hidden_false_on_non_capturing_overlay_does_not_auto_focus() {
        let terminal = Arc::new(VirtualTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let editor = FocusableOverlay::new(&["EDITOR"]);
        let overlay = FocusableOverlay::new(&["OVERLAY"]);
        tui.add_child(Arc::new(EmptyContent));
        tui.set_focus(Some(editor.clone()));
        tui.start();
        let handle = tui.show_overlay(
            overlay.clone(),
            Some(OverlayOptions {
                non_capturing: Some(true),
                ..Default::default()
            }),
        );
        handle.set_hidden(true);
        handle.set_hidden(false);
        render_and_flush(&tui, &terminal).await;
        assert!(editor.focused());
        assert!(!overlay.focused());
        tui.stop(None);
    }

    #[tokio::test]
    async fn hide_when_overlay_is_not_focused_does_not_change_focus() {
        let terminal = Arc::new(VirtualTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let editor = FocusableOverlay::new(&["EDITOR"]);
        let overlay = FocusableOverlay::new(&["OVERLAY"]);
        tui.add_child(Arc::new(EmptyContent));
        tui.set_focus(Some(editor.clone()));
        tui.start();
        let handle = tui.show_overlay(
            overlay,
            Some(OverlayOptions {
                non_capturing: Some(true),
                ..Default::default()
            }),
        );
        handle.hide();
        render_and_flush(&tui, &terminal).await;
        assert!(editor.focused());
        tui.stop(None);
    }

    #[tokio::test]
    async fn hide_when_focused_restores_focus_correctly() {
        let terminal = Arc::new(VirtualTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let editor = FocusableOverlay::new(&["EDITOR"]);
        let overlay = FocusableOverlay::new(&["OVERLAY"]);
        tui.add_child(Arc::new(EmptyContent));
        tui.set_focus(Some(editor.clone()));
        tui.start();
        let handle = tui.show_overlay(
            overlay.clone(),
            Some(OverlayOptions {
                non_capturing: Some(true),
                ..Default::default()
            }),
        );
        handle.focus();
        handle.hide();
        render_and_flush(&tui, &terminal).await;
        assert!(editor.focused());
        assert!(!overlay.focused());
        tui.stop(None);
    }

    #[tokio::test]
    async fn capturing_overlay_removed_with_non_capturing_below_restores_focus_to_editor() {
        let terminal = Arc::new(VirtualTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let editor = FocusableOverlay::new(&["EDITOR"]);
        let non_capturing = FocusableOverlay::new(&["NC"]);
        let capturing = FocusableOverlay::new(&["CAP"]);
        tui.add_child(Arc::new(EmptyContent));
        tui.set_focus(Some(editor.clone()));
        tui.start();
        tui.show_overlay(
            non_capturing.clone(),
            Some(OverlayOptions {
                non_capturing: Some(true),
                ..Default::default()
            }),
        );
        let handle = tui.show_overlay(capturing.clone(), None);
        assert!(capturing.focused());
        handle.hide();
        render_and_flush(&tui, &terminal).await;
        assert!(editor.focused());
        assert!(!non_capturing.focused());
        tui.stop(None);
    }

    #[tokio::test]
    async fn sub_overlay_cleanup_then_hide_overlay_restores_focus_and_input_to_editor() {
        let terminal = Arc::new(VirtualTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let editor = FocusableOverlay::new(&["EDITOR"]);
        let timer = FocusableOverlay::new(&["TIMER"]);
        let controller = FocusableOverlay::new(&["CTRL"]);
        tui.add_child(Arc::new(EmptyContent));
        tui.set_focus(Some(editor.clone()));
        tui.start();
        let timer_handle = tui.show_overlay(
            timer.clone(),
            Some(OverlayOptions {
                non_capturing: Some(true),
                ..Default::default()
            }),
        );
        tui.show_overlay(controller.clone(), None);
        assert!(controller.focused());
        assert!(!editor.focused());
        timer_handle.hide();
        tui.hide_overlay();
        render_and_flush(&tui, &terminal).await;
        assert!(editor.focused());
        assert!(!controller.focused());
        assert!(!timer.focused());
        terminal.send_input("x");
        render_and_flush(&tui, &terminal).await;
        assert_eq!(editor.inputs(), vec!["x".to_string()]);
        assert!(controller.inputs().is_empty());
        assert!(timer.inputs().is_empty());
        tui.stop(None);
    }

    #[tokio::test]
    async fn removed_focused_child_overlay_does_not_become_parent_overlay_fallback() {
        let terminal = Arc::new(VirtualTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let editor = FocusableOverlay::new(&["EDITOR"]);
        let child = FocusableOverlay::new(&["CHILD"]);
        let parent = FocusableOverlay::new(&["PARENT"]);
        tui.add_child(Arc::new(EmptyContent));
        tui.set_focus(Some(editor.clone()));
        tui.start();
        let child_handle = tui.show_overlay(
            child.clone(),
            Some(OverlayOptions {
                non_capturing: Some(true),
                ..Default::default()
            }),
        );
        child_handle.focus();
        let parent_handle = tui.show_overlay(parent.clone(), None);
        assert!(parent.focused());

        child_handle.hide();
        parent_handle.hide();
        terminal.send_input("x");
        render_and_flush(&tui, &terminal).await;

        assert_eq!(editor.inputs(), vec!["x".to_string()]);
        assert!(child.inputs().is_empty());
        assert!(parent.inputs().is_empty());
        assert!(editor.focused());
        tui.stop(None);
    }

    #[tokio::test]
    async fn microtask_deferred_sub_overlay_pattern_restores_focus() {
        let terminal = Arc::new(VirtualTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let editor = FocusableOverlay::new(&["EDITOR"]);
        let timer = FocusableOverlay::new(&["TIMER"]);
        let controller = FocusableOverlay::new(&["CTRL"]);
        tui.add_child(Arc::new(EmptyContent));
        tui.set_focus(Some(editor.clone()));
        tui.start();

        let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
        let timer_handle = tui.show_overlay(
            timer.clone(),
            Some(OverlayOptions {
                non_capturing: Some(true),
                ..Default::default()
            }),
        );
        let tui_deferred = tui.clone();
        let controller_deferred = controller.clone();
        tokio::spawn(async move {
            tui_deferred.show_overlay(controller_deferred, None);
        });

        tokio::task::yield_now().await;
        render_and_flush(&tui, &terminal).await;

        assert!(controller.focused());
        assert!(!editor.focused());

        timer_handle.hide();
        tui.hide_overlay();
        let _ = done_tx.send(());
        let _ = done_rx.await;
        render_and_flush(&tui, &terminal).await;

        assert!(editor.focused(), "editor should regain focus");
        assert!(!controller.focused());
        assert!(!timer.focused());

        terminal.send_input("x");
        render_and_flush(&tui, &terminal).await;
        assert_eq!(editor.inputs(), vec!["x".to_string()]);
        assert!(controller.inputs().is_empty());
        tui.stop(None);
    }

    #[tokio::test]
    async fn handle_input_redirection_skips_non_capturing_overlays_when_focused_overlay_becomes_invisible() {
        let terminal = Arc::new(VirtualTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let editor = FocusableOverlay::new(&["EDITOR"]);
        let fallback_capturing = FocusableOverlay::new(&["FALLBACK"]);
        let non_capturing = FocusableOverlay::new(&["NC"]);
        let primary = FocusableOverlay::new(&["PRIMARY"]);
        let is_visible = Arc::new(Mutex::new(true));
        tui.add_child(Arc::new(EmptyContent));
        tui.set_focus(Some(editor));
        tui.start();

        tui.show_overlay(fallback_capturing.clone(), None);
        tui.show_overlay(
            non_capturing.clone(),
            Some(OverlayOptions {
                non_capturing: Some(true),
                ..Default::default()
            }),
        );
        let is_visible_ref = Arc::clone(&is_visible);
        tui.show_overlay(
            primary.clone(),
            Some(OverlayOptions {
                visible: Some(Arc::new(move |_, _| *is_visible_ref.lock().unwrap())),
                ..Default::default()
            }),
        );
        assert!(primary.focused());
        *is_visible.lock().unwrap() = false;
        terminal.send_input("x");
        render_and_flush(&tui, &terminal).await;
        assert!(primary.inputs().is_empty());
        assert!(non_capturing.inputs().is_empty());
        assert_eq!(fallback_capturing.inputs(), vec!["x".to_string()]);
        assert!(fallback_capturing.focused());
        tui.stop(None);
    }

    #[tokio::test]
    async fn active_base_focus_replacement_receives_close_input_before_overlay_restore() {
        let terminal = Arc::new(VirtualTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let editor = FocusableOverlay::new(&["EDITOR"]);
        let replacement = FocusableOverlay::new(&["REPLACEMENT"]);
        let overlay = FocusableOverlay::new(&["OVERLAY"]);

        {
            let overlay_weak = Arc::downgrade(&overlay);
            let tui_c = tui.clone();
            let rep_c = replacement.clone();
            overlay.set_custom_handle_input(move |data| {
                if let Some(ov) = overlay_weak.upgrade() {
                    ov.inputs.lock().unwrap().push(data.to_string());
                }
                if data == "b" {
                    tui_c.set_focus(Some(rep_c.clone()));
                }
            });
        }
        {
            let rep_weak = Arc::downgrade(&replacement);
            let tui_c = tui.clone();
            let ed_c = editor.clone();
            replacement.set_custom_handle_input(move |data| {
                if let Some(rep) = rep_weak.upgrade() {
                    rep.inputs.lock().unwrap().push(data.to_string());
                }
                if data == "\r" {
                    tui_c.set_focus(Some(ed_c.clone()));
                }
            });
        }

        tui.add_child(Arc::new(EmptyContent));
        tui.set_focus(Some(editor));
        tui.start();
        tui.show_overlay(overlay.clone(), None);
        assert!(overlay.focused());
        terminal.send_input("b");
        render_and_flush(&tui, &terminal).await;
        assert!(replacement.focused());

        terminal.send_input("\r");
        render_and_flush(&tui, &terminal).await;
        assert_eq!(replacement.inputs(), vec!["\r".to_string()]);
        assert_eq!(overlay.inputs(), vec!["b".to_string()]);
        assert!(overlay.focused());

        terminal.send_input("x");
        render_and_flush(&tui, &terminal).await;
        assert_eq!(overlay.inputs(), vec!["b".to_string(), "x".to_string()]);
        tui.stop(None);
    }

    #[tokio::test]
    async fn active_replacement_still_receives_input_when_it_is_another_overlay_pre_focus() {
        let terminal = Arc::new(VirtualTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let editor = FocusableOverlay::new(&["EDITOR"]);
        let replacement = FocusableOverlay::new(&["REPLACEMENT"]);
        let passive = FocusableOverlay::new(&["PASSIVE"]);
        let overlay = FocusableOverlay::new(&["OVERLAY"]);

        {
            let overlay_weak = Arc::downgrade(&overlay);
            let tui_c = tui.clone();
            let rep_c = replacement.clone();
            overlay.set_custom_handle_input(move |data| {
                if let Some(ov) = overlay_weak.upgrade() {
                    ov.inputs.lock().unwrap().push(data.to_string());
                }
                if data == "b" {
                    tui_c.set_focus(Some(rep_c.clone()));
                }
            });
        }
        {
            let rep_weak = Arc::downgrade(&replacement);
            let tui_c = tui.clone();
            let ed_c = editor.clone();
            replacement.set_custom_handle_input(move |data| {
                if let Some(rep) = rep_weak.upgrade() {
                    rep.inputs.lock().unwrap().push(data.to_string());
                }
                if data == "\r" {
                    tui_c.set_focus(Some(ed_c.clone()));
                }
            });
        }

        tui.add_child(Arc::new(EmptyContent));
        tui.set_focus(Some(editor.clone()));
        tui.start();
        tui.set_focus(Some(replacement.clone()));
        tui.show_overlay(
            passive,
            Some(OverlayOptions {
                non_capturing: Some(true),
                ..Default::default()
            }),
        );
        tui.set_focus(Some(editor));
        tui.show_overlay(overlay.clone(), None);
        terminal.send_input("b");
        render_and_flush(&tui, &terminal).await;
        assert!(replacement.focused());

        terminal.send_input("1");
        terminal.send_input("\r");
        render_and_flush(&tui, &terminal).await;
        assert_eq!(replacement.inputs(), vec!["1".to_string(), "\r".to_string()]);
        assert_eq!(overlay.inputs(), vec!["b".to_string()]);
        assert!(overlay.focused());
        tui.stop(None);
    }

    #[tokio::test]
    async fn blocked_replacement_can_move_focus_internally_before_overlay_restore() {
        let terminal = Arc::new(VirtualTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let base = Container::new();
        let editor = FocusableOverlay::new(&["EDITOR"]);
        let first_replacement = FocusableOverlay::new(&["FIRST"]);
        let second_replacement = FocusableOverlay::new(&["SECOND"]);
        let overlay = FocusableOverlay::new(&["OVERLAY"]);

        {
            let ov_weak = Arc::downgrade(&overlay);
            let tui_c = tui.clone();
            let first_c = first_replacement.clone();
            overlay.set_custom_handle_input(move |data| {
                if let Some(ov) = ov_weak.upgrade() {
                    ov.inputs.lock().unwrap().push(data.to_string());
                }
                if data == "b" {
                    tui_c.set_focus(Some(first_c.clone()));
                }
            });
        }
        {
            let first_weak = Arc::downgrade(&first_replacement);
            let tui_c = tui.clone();
            let second_c = second_replacement.clone();
            first_replacement.set_custom_handle_input(move |data| {
                if let Some(rep) = first_weak.upgrade() {
                    rep.inputs.lock().unwrap().push(data.to_string());
                }
                if data == "n" {
                    tui_c.set_focus(Some(second_c.clone()));
                }
            });
        }
        {
            let second_weak = Arc::downgrade(&second_replacement);
            let base_c = base.clone();
            let tui_c = tui.clone();
            let ed_c = editor.clone();
            second_replacement.set_custom_handle_input(move |data| {
                if let Some(rep) = second_weak.upgrade() {
                    rep.inputs.lock().unwrap().push(data.to_string());
                }
                if data == "\r" {
                    base_c.clear();
                    base_c.add_child(ed_c.clone());
                    tui_c.set_focus(Some(ed_c.clone()));
                }
            });
        }

        base.add_child(editor.clone());
        base.add_child(first_replacement.clone());
        base.add_child(second_replacement.clone());
        tui.add_child(Arc::new(base));
        tui.set_focus(Some(editor));
        tui.start();

        tui.show_overlay(overlay.clone(), None);
        terminal.send_input("b");
        render_and_flush(&tui, &terminal).await;
        terminal.send_input("n");
        render_and_flush(&tui, &terminal).await;
        terminal.send_input("2");
        terminal.send_input("\r");
        render_and_flush(&tui, &terminal).await;

        assert_eq!(overlay.inputs(), vec!["b".to_string()]);
        assert_eq!(first_replacement.inputs(), vec!["n".to_string()]);
        assert_eq!(second_replacement.inputs(), vec!["2".to_string(), "\r".to_string()]);
        assert!(overlay.focused());
        tui.stop(None);
    }

    #[tokio::test]
    async fn removed_replacement_restores_overlay_even_when_overlay_pre_focus_differs_from_next_focus() {
        let terminal = Arc::new(VirtualTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let base = Container::new();
        let editor = FocusableOverlay::new(&["EDITOR"]);
        let palette = FocusableOverlay::new(&["PALETTE"]);
        let replacement = FocusableOverlay::new(&["REPLACEMENT"]);
        let overlay = FocusableOverlay::new(&["OVERLAY"]);

        {
            let ov_weak = Arc::downgrade(&overlay);
            let tui_c = tui.clone();
            let rep_c = replacement.clone();
            overlay.set_custom_handle_input(move |data| {
                if let Some(ov) = ov_weak.upgrade() {
                    ov.inputs.lock().unwrap().push(data.to_string());
                }
                if data == "b" {
                    tui_c.set_focus(Some(rep_c.clone()));
                }
            });
        }
        {
            let rep_weak = Arc::downgrade(&replacement);
            let base_c = base.clone();
            let tui_c = tui.clone();
            let ed_c = editor.clone();
            replacement.set_custom_handle_input(move |data| {
                if let Some(rep) = rep_weak.upgrade() {
                    rep.inputs.lock().unwrap().push(data.to_string());
                }
                if data == "\r" {
                    base_c.clear();
                    base_c.add_child(ed_c.clone());
                    tui_c.set_focus(Some(ed_c.clone()));
                }
            });
        }

        base.add_child(editor.clone());
        base.add_child(palette.clone());
        base.add_child(replacement.clone());
        tui.add_child(Arc::new(base));
        tui.set_focus(Some(palette));
        tui.start();

        tui.show_overlay(overlay.clone(), None);
        terminal.send_input("b");
        render_and_flush(&tui, &terminal).await;
        terminal.send_input("\r");
        terminal.send_input("x");
        render_and_flush(&tui, &terminal).await;

        assert_eq!(overlay.inputs(), vec!["b".to_string(), "x".to_string()]);
        assert_eq!(replacement.inputs(), vec!["\r".to_string()]);
        assert!(editor.inputs().is_empty());
        assert!(overlay.focused());
        tui.stop(None);
    }

    #[tokio::test]
    async fn unfocus_target_releases_a_blocked_overlay_while_replacement_remains_focused() {
        let terminal = Arc::new(VirtualTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let fallback = FocusableOverlay::new(&["FALLBACK"]);
        let target = FocusableOverlay::new(&["TARGET"]);
        let replacement = FocusableOverlay::new(&["REPLACEMENT"]);
        let overlay = FocusableOverlay::new(&["OVERLAY"]);

        {
            let rep_weak = Arc::downgrade(&replacement);
            let tui_c = tui.clone();
            let fb_c = fallback.clone();
            replacement.set_custom_handle_input(move |data| {
                if let Some(rep) = rep_weak.upgrade() {
                    rep.inputs.lock().unwrap().push(data.to_string());
                }
                if data == "\r" {
                    tui_c.set_focus(Some(fb_c.clone()));
                }
            });
        }

        tui.add_child(Arc::new(EmptyContent));
        tui.start();
        let overlay_handle = tui.show_overlay(overlay.clone(), None);
        {
            let ov_weak = Arc::downgrade(&overlay);
            let tui_c = tui.clone();
            let rep_c = replacement.clone();
            let target_c = target.clone();
            let handle_c = overlay_handle;
            overlay.set_custom_handle_input(move |data| {
                if let Some(ov) = ov_weak.upgrade() {
                    ov.inputs.lock().unwrap().push(data.to_string());
                }
                if data == "b" {
                    tui_c.set_focus(Some(rep_c.clone()));
                    handle_c.unfocus(Some(OverlayUnfocusOptions {
                        target: Some(target_c.clone()),
                    }));
                }
            });
        }

        terminal.send_input("b");
        render_and_flush(&tui, &terminal).await;
        assert!(replacement.focused());
        terminal.send_input("\r");
        terminal.send_input("x");
        render_and_flush(&tui, &terminal).await;

        assert_eq!(overlay.inputs(), vec!["b".to_string()]);
        assert_eq!(replacement.inputs(), vec!["\r".to_string()]);
        assert!(fallback.inputs().is_empty());
        assert_eq!(target.inputs(), vec!["x".to_string()]);
        tui.stop(None);
    }

    #[tokio::test]
    async fn handle_input_restores_focus_to_a_visible_focused_overlay_after_base_focus_steal() {
        let terminal = Arc::new(VirtualTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let editor = FocusableOverlay::new(&["EDITOR"]);
        let replacement = FocusableOverlay::new(&["REPLACEMENT"]);
        let overlay = FocusableOverlay::new(&["OVERLAY"]);
        tui.add_child(Arc::new(EmptyContent));
        tui.set_focus(Some(editor.clone()));
        tui.start();
        tui.show_overlay(overlay.clone(), None);
        assert!(overlay.focused());
        tui.set_focus(Some(replacement));
        tui.set_focus(Some(editor.clone()));
        terminal.send_input("x");
        render_and_flush(&tui, &terminal).await;
        assert_eq!(overlay.inputs(), vec!["x".to_string()]);
        assert!(editor.inputs().is_empty());
        assert!(overlay.focused());
        tui.stop(None);
    }

    #[tokio::test]
    async fn handle_input_restores_focus_to_explicitly_focused_raw_sub_overlay_after_base_focus_steal() {
        let terminal = Arc::new(VirtualTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let editor = FocusableOverlay::new(&["EDITOR"]);
        let controller = FocusableOverlay::new(&["CONTROLLER"]);
        let sub_overlay = FocusableOverlay::new(&["SUB"]);
        tui.add_child(Arc::new(EmptyContent));
        tui.set_focus(Some(editor.clone()));
        tui.start();
        tui.show_overlay(controller.clone(), None);
        let sub_handle = tui.show_overlay(
            sub_overlay.clone(),
            Some(OverlayOptions {
                non_capturing: Some(true),
                ..Default::default()
            }),
        );
        sub_handle.focus();
        tui.set_focus(Some(editor.clone()));
        terminal.send_input("x");
        render_and_flush(&tui, &terminal).await;
        assert_eq!(sub_overlay.inputs(), vec!["x".to_string()]);
        assert!(controller.inputs().is_empty());
        assert!(editor.inputs().is_empty());
        tui.stop(None);
    }

    #[tokio::test]
    async fn passive_non_capturing_overlay_does_not_regain_input_after_base_focus() {
        let terminal = Arc::new(VirtualTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let editor = FocusableOverlay::new(&["EDITOR"]);
        let passive = FocusableOverlay::new(&["PASSIVE"]);
        tui.add_child(Arc::new(EmptyContent));
        tui.set_focus(Some(editor.clone()));
        tui.start();
        tui.show_overlay(
            passive.clone(),
            Some(OverlayOptions {
                non_capturing: Some(true),
                ..Default::default()
            }),
        );
        terminal.send_input("x");
        render_and_flush(&tui, &terminal).await;
        assert_eq!(editor.inputs(), vec!["x".to_string()]);
        assert!(passive.inputs().is_empty());
        assert!(editor.focused());
        tui.stop(None);
    }

    #[tokio::test]
    async fn explicitly_focused_non_capturing_overlay_regains_input_after_base_focus_steal() {
        let terminal = Arc::new(VirtualTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let editor = FocusableOverlay::new(&["EDITOR"]);
        let overlay = FocusableOverlay::new(&["NC"]);
        tui.add_child(Arc::new(EmptyContent));
        tui.set_focus(Some(editor.clone()));
        tui.start();
        let handle = tui.show_overlay(
            overlay.clone(),
            Some(OverlayOptions {
                non_capturing: Some(true),
                ..Default::default()
            }),
        );
        handle.focus();
        tui.set_focus(Some(editor.clone()));
        terminal.send_input("x");
        render_and_flush(&tui, &terminal).await;
        assert_eq!(overlay.inputs(), vec!["x".to_string()]);
        assert!(editor.inputs().is_empty());
        tui.stop(None);
    }

    #[tokio::test]
    async fn unfocus_prevents_visible_overlay_from_regaining_input() {
        let terminal = Arc::new(VirtualTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let editor = FocusableOverlay::new(&["EDITOR"]);
        let overlay = FocusableOverlay::new(&["OVERLAY"]);
        tui.add_child(Arc::new(EmptyContent));
        tui.set_focus(Some(editor.clone()));
        tui.start();
        let handle = tui.show_overlay(overlay.clone(), None);
        handle.unfocus(None);
        terminal.send_input("x");
        render_and_flush(&tui, &terminal).await;
        assert_eq!(editor.inputs(), vec!["x".to_string()]);
        assert!(overlay.inputs().is_empty());
        assert!(editor.focused());
        tui.stop(None);
    }

    #[tokio::test]
    async fn set_focus_null_explicitly_clears_visible_overlay_restore() {
        let terminal = Arc::new(VirtualTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let overlay = FocusableOverlay::new(&["OVERLAY"]);
        tui.add_child(Arc::new(EmptyContent));
        tui.start();
        tui.show_overlay(overlay.clone(), None);
        tui.set_focus(None);
        terminal.send_input("x");
        render_and_flush(&tui, &terminal).await;
        assert!(overlay.inputs().is_empty());
        assert!(!overlay.focused());
        tui.stop(None);
    }

    #[tokio::test]
    async fn blocked_replacement_set_focus_null_resumes_the_visible_overlay() {
        let terminal = Arc::new(VirtualTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let replacement = FocusableOverlay::new(&["REPLACEMENT"]);
        let overlay = FocusableOverlay::new(&["OVERLAY"]);

        {
            let rep_weak = Arc::downgrade(&replacement);
            let tui_c = tui.clone();
            replacement.set_custom_handle_input(move |data| {
                if let Some(rep) = rep_weak.upgrade() {
                    rep.inputs.lock().unwrap().push(data.to_string());
                }
                if data == "\r" {
                    tui_c.set_focus(None);
                }
            });
        }
        {
            let ov_weak = Arc::downgrade(&overlay);
            let tui_c = tui.clone();
            let rep_c = replacement.clone();
            overlay.set_custom_handle_input(move |data| {
                if let Some(ov) = ov_weak.upgrade() {
                    ov.inputs.lock().unwrap().push(data.to_string());
                }
                if data == "b" {
                    tui_c.set_focus(Some(rep_c.clone()));
                }
            });
        }

        tui.add_child(Arc::new(EmptyContent));
        tui.start();
        tui.show_overlay(overlay.clone(), None);
        terminal.send_input("b");
        render_and_flush(&tui, &terminal).await;
        terminal.send_input("\r");
        terminal.send_input("x");
        render_and_flush(&tui, &terminal).await;
        assert_eq!(replacement.inputs(), vec!["\r".to_string()]);
        assert_eq!(overlay.inputs(), vec!["b".to_string(), "x".to_string()]);
        assert!(overlay.focused());
        tui.stop(None);
    }

    #[tokio::test]
    async fn temporarily_invisible_focused_overlay_falls_back_without_losing_restore_eligibility() {
        let terminal = Arc::new(VirtualTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let editor = FocusableOverlay::new(&["EDITOR"]);
        let overlay = FocusableOverlay::new(&["OVERLAY"]);
        let visible = Arc::new(Mutex::new(true));
        tui.add_child(Arc::new(EmptyContent));
        tui.set_focus(Some(editor.clone()));
        tui.start();

        let visible_ref = Arc::clone(&visible);
        tui.show_overlay(
            overlay.clone(),
            Some(OverlayOptions {
                visible: Some(Arc::new(move |_, _| *visible_ref.lock().unwrap())),
                ..Default::default()
            }),
        );
        tui.set_focus(Some(editor.clone()));
        *visible.lock().unwrap() = false;
        terminal.send_input("x");
        render_and_flush(&tui, &terminal).await;
        assert_eq!(editor.inputs(), vec!["x".to_string()]);
        assert!(overlay.inputs().is_empty());

        *visible.lock().unwrap() = true;
        terminal.send_input("y");
        render_and_flush(&tui, &terminal).await;
        assert_eq!(editor.inputs(), vec!["x".to_string()]);
        assert_eq!(overlay.inputs(), vec!["y".to_string()]);
        tui.stop(None);
    }

    #[tokio::test]
    async fn temporarily_invisible_focused_overlay_with_null_pre_focus_restores_when_visible_again() {
        let terminal = Arc::new(VirtualTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let overlay = FocusableOverlay::new(&["OVERLAY"]);
        let visible = Arc::new(Mutex::new(true));
        tui.add_child(Arc::new(EmptyContent));
        tui.start();

        let visible_ref = Arc::clone(&visible);
        tui.show_overlay(
            overlay.clone(),
            Some(OverlayOptions {
                visible: Some(Arc::new(move |_, _| *visible_ref.lock().unwrap())),
                ..Default::default()
            }),
        );
        *visible.lock().unwrap() = false;
        terminal.send_input("x");
        render_and_flush(&tui, &terminal).await;
        assert!(overlay.inputs().is_empty());

        *visible.lock().unwrap() = true;
        terminal.send_input("y");
        render_and_flush(&tui, &terminal).await;
        assert_eq!(overlay.inputs(), vec!["y".to_string()]);
        tui.stop(None);
    }

    #[tokio::test]
    async fn cyclic_overlay_pre_focus_ancestry_does_not_hang_focus_changes() {
        let terminal = Arc::new(VirtualTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let editor = FocusableOverlay::new(&["EDITOR"]);
        let overlay = FocusableOverlay::new(&["OVERLAY"]);
        tui.add_child(Arc::new(EmptyContent));
        tui.set_focus(Some(overlay.clone()));
        tui.start();

        let handle = tui.show_overlay(
            overlay.clone(),
            Some(OverlayOptions {
                non_capturing: Some(true),
                ..Default::default()
            }),
        );
        handle.focus();
        tui.set_focus(Some(editor.clone()));
        terminal.send_input("x");
        render_and_flush(&tui, &terminal).await;
        assert_eq!(editor.inputs(), vec!["x".to_string()]);
        assert!(overlay.inputs().is_empty());
        tui.stop(None);
    }

    #[tokio::test]
    async fn handle_input_restores_the_focus_order_top_overlay_after_base_focus_steal() {
        let terminal = Arc::new(VirtualTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let editor = FocusableOverlay::new(&["EDITOR"]);
        let lower = FocusableOverlay::new(&["LOWER"]);
        let upper = FocusableOverlay::new(&["UPPER"]);
        tui.add_child(Arc::new(EmptyContent));
        tui.set_focus(Some(editor.clone()));
        tui.start();

        let lower_handle = tui.show_overlay(lower.clone(), None);
        tui.show_overlay(upper.clone(), None);
        lower_handle.focus();
        tui.set_focus(Some(editor.clone()));
        terminal.send_input("x");
        render_and_flush(&tui, &terminal).await;
        assert_eq!(lower.inputs(), vec!["x".to_string()]);
        assert!(upper.inputs().is_empty());
        assert!(editor.inputs().is_empty());
        tui.stop(None);
    }

    #[tokio::test]
    async fn hide_overlay_does_not_reassign_focus_when_topmost_overlay_is_non_capturing() {
        let terminal = Arc::new(VirtualTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let editor = FocusableOverlay::new(&["EDITOR"]);
        let capturing = FocusableOverlay::new(&["CAP"]);
        let non_capturing = FocusableOverlay::new(&["NC"]);
        tui.add_child(Arc::new(EmptyContent));
        tui.set_focus(Some(editor));
        tui.start();

        tui.show_overlay(capturing.clone(), None);
        tui.show_overlay(
            non_capturing,
            Some(OverlayOptions {
                non_capturing: Some(true),
                ..Default::default()
            }),
        );
        assert!(capturing.focused());
        tui.hide_overlay();
        render_and_flush(&tui, &terminal).await;
        assert!(capturing.focused());
        tui.stop(None);
    }

    #[tokio::test]
    async fn multiple_capturing_and_non_capturing_overlays_restore_focus_through_removals() {
        let terminal = Arc::new(VirtualTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let editor = FocusableOverlay::new(&["EDITOR"]);
        let c1 = FocusableOverlay::new(&["C1"]);
        let n1 = FocusableOverlay::new(&["N1"]);
        let c2 = FocusableOverlay::new(&["C2"]);
        let n2 = FocusableOverlay::new(&["N2"]);
        tui.add_child(Arc::new(EmptyContent));
        tui.set_focus(Some(editor.clone()));
        tui.start();

        let c1_handle = tui.show_overlay(c1.clone(), None);
        tui.show_overlay(
            n1,
            Some(OverlayOptions {
                non_capturing: Some(true),
                ..Default::default()
            }),
        );
        let c2_handle = tui.show_overlay(c2.clone(), None);
        tui.show_overlay(
            n2,
            Some(OverlayOptions {
                non_capturing: Some(true),
                ..Default::default()
            }),
        );
        assert!(c2.focused());
        c2_handle.hide();
        render_and_flush(&tui, &terminal).await;
        assert!(c1.focused());
        c1_handle.hide();
        render_and_flush(&tui, &terminal).await;
        assert!(editor.focused());
        tui.stop(None);
    }

    #[tokio::test]
    async fn capturing_overlay_unfocus_on_topmost_capturing_overlay_falls_back_to_pre_focus() {
        let terminal = Arc::new(VirtualTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let editor = FocusableOverlay::new(&["EDITOR"]);
        let capturing = FocusableOverlay::new(&["CAP"]);
        tui.add_child(Arc::new(EmptyContent));
        tui.set_focus(Some(editor.clone()));
        tui.start();

        let handle = tui.show_overlay(capturing.clone(), None);
        assert!(capturing.focused());
        handle.unfocus(None);
        render_and_flush(&tui, &terminal).await;
        assert!(editor.focused());
        assert!(!capturing.focused());
        tui.stop(None);
    }
}

mod no_op_guards {
    use super::*;

    #[tokio::test]
    async fn focus_on_hidden_overlay_is_a_no_op() {
        let terminal = Arc::new(VirtualTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let editor = FocusableOverlay::new(&["EDITOR"]);
        let overlay = FocusableOverlay::new(&["OVERLAY"]);
        tui.add_child(Arc::new(EmptyContent));
        tui.set_focus(Some(editor.clone()));
        tui.start();

        let handle = tui.show_overlay(
            overlay,
            Some(OverlayOptions {
                non_capturing: Some(true),
                ..Default::default()
            }),
        );
        handle.set_hidden(true);
        handle.focus();
        render_and_flush(&tui, &terminal).await;
        assert!(editor.focused());
        assert!(!handle.is_focused());
        tui.stop(None);
    }

    #[tokio::test]
    async fn focus_after_hide_is_a_no_op() {
        let terminal = Arc::new(VirtualTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let editor = FocusableOverlay::new(&["EDITOR"]);
        let overlay = FocusableOverlay::new(&["OVERLAY"]);
        tui.add_child(Arc::new(EmptyContent));
        tui.set_focus(Some(editor.clone()));
        tui.start();

        let handle = tui.show_overlay(
            overlay,
            Some(OverlayOptions {
                non_capturing: Some(true),
                ..Default::default()
            }),
        );
        handle.hide();
        handle.focus();
        render_and_flush(&tui, &terminal).await;
        assert!(editor.focused());
        assert!(!handle.is_focused());
        tui.stop(None);
    }

    #[tokio::test]
    async fn unfocus_when_overlay_does_not_have_focus_is_a_no_op() {
        let terminal = Arc::new(VirtualTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let editor = FocusableOverlay::new(&["EDITOR"]);
        let overlay = FocusableOverlay::new(&["OVERLAY"]);
        tui.add_child(Arc::new(EmptyContent));
        tui.set_focus(Some(editor.clone()));
        tui.start();

        let handle = tui.show_overlay(
            overlay.clone(),
            Some(OverlayOptions {
                non_capturing: Some(true),
                ..Default::default()
            }),
        );
        handle.unfocus(None);
        render_and_flush(&tui, &terminal).await;
        assert!(editor.focused());
        assert!(!overlay.focused());
        tui.stop(None);
    }

    #[tokio::test]
    async fn unfocus_with_null_pre_focus_clears_focus_and_does_not_route_input_back_to_overlay() {
        let terminal = Arc::new(VirtualTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let overlay = FocusableOverlay::new(&["OVERLAY"]);
        tui.add_child(Arc::new(EmptyContent));
        tui.start();

        let handle = tui.show_overlay(overlay.clone(), None);
        assert!(overlay.focused());
        handle.unfocus(None);
        assert!(!overlay.focused());
        terminal.send_input("x");
        render_and_flush(&tui, &terminal).await;
        assert!(overlay.inputs().is_empty());
        assert!(!handle.is_focused());
        tui.stop(None);
    }
}

mod focus_cycle_prevention {
    use super::*;

    #[tokio::test]
    async fn toggle_focus_between_non_capturing_overlays_then_unfocus_returns_to_editor() {
        let terminal = Arc::new(VirtualTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let editor = FocusableOverlay::new(&["EDITOR"]);
        let a = FocusableOverlay::new(&["A"]);
        let b = FocusableOverlay::new(&["B"]);
        tui.add_child(Arc::new(EmptyContent));
        tui.set_focus(Some(editor.clone()));
        tui.start();

        let a_handle = tui.show_overlay(
            a.clone(),
            Some(OverlayOptions {
                non_capturing: Some(true),
                ..Default::default()
            }),
        );
        let b_handle = tui.show_overlay(
            b.clone(),
            Some(OverlayOptions {
                non_capturing: Some(true),
                ..Default::default()
            }),
        );
        a_handle.focus();
        b_handle.focus();
        a_handle.focus();
        a_handle.unfocus(None);
        render_and_flush(&tui, &terminal).await;
        assert!(editor.focused());
        assert!(!a.focused());
        assert!(!b.focused());
        tui.stop(None);
    }

    #[tokio::test]
    async fn explicit_unfocus_target_supports_cycling_between_three_overlays_and_editor() {
        let terminal = Arc::new(VirtualTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let editor = FocusableOverlay::new(&["EDITOR"]);
        let a = FocusableOverlay::new(&["A"]);
        let b = FocusableOverlay::new(&["B"]);
        let c = FocusableOverlay::new(&["C"]);
        tui.add_child(Arc::new(EmptyContent));
        tui.set_focus(Some(editor.clone()));
        tui.start();

        let a_handle = tui.show_overlay(a.clone(), None);
        let b_handle = tui.show_overlay(b.clone(), None);
        let c_handle = tui.show_overlay(c.clone(), None);

        a_handle.focus();
        terminal.send_input("a");
        render_and_flush(&tui, &terminal).await;
        b_handle.focus();
        terminal.send_input("b");
        render_and_flush(&tui, &terminal).await;
        c_handle.focus();
        terminal.send_input("c");
        render_and_flush(&tui, &terminal).await;
        c_handle.unfocus(Some(OverlayUnfocusOptions {
            target: Some(editor.clone()),
        }));
        terminal.send_input("e");
        render_and_flush(&tui, &terminal).await;
        a_handle.focus();
        terminal.send_input("A");
        render_and_flush(&tui, &terminal).await;
        a_handle.unfocus(Some(OverlayUnfocusOptions {
            target: Some(editor.clone()),
        }));
        terminal.send_input("E");
        render_and_flush(&tui, &terminal).await;

        assert_eq!(a.inputs(), vec!["a".to_string(), "A".to_string()]);
        assert_eq!(b.inputs(), vec!["b".to_string()]);
        assert_eq!(c.inputs(), vec!["c".to_string()]);
        assert_eq!(editor.inputs(), vec!["e".to_string(), "E".to_string()]);
        assert!(editor.focused());
        tui.stop(None);
    }

    #[tokio::test]
    async fn explicit_null_unfocus_target_clears_focus_without_restoring_overlays() {
        let terminal = Arc::new(VirtualTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let overlay = FocusableOverlay::new(&["OVERLAY"]);
        tui.add_child(Arc::new(EmptyContent));
        tui.start();

        let handle = tui.show_overlay(overlay.clone(), None);
        handle.unfocus(Some(OverlayUnfocusOptions { target: None }));
        terminal.send_input("x");
        render_and_flush(&tui, &terminal).await;
        assert!(overlay.inputs().is_empty());
        assert!(!handle.is_focused());
        tui.stop(None);
    }

    #[tokio::test]
    async fn hiding_focused_overlay_falls_back_to_next_visual_frontmost_overlay() {
        let terminal = Arc::new(VirtualTerminal::new(80, 24));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let editor = FocusableOverlay::new(&["EDITOR"]);
        let a = FocusableOverlay::new(&["A"]);
        let b = FocusableOverlay::new(&["B"]);
        let c = FocusableOverlay::new(&["C"]);
        tui.add_child(Arc::new(EmptyContent));
        tui.set_focus(Some(editor));
        tui.start();

        let a_handle = tui.show_overlay(a.clone(), None);
        let b_handle = tui.show_overlay(b, None);
        tui.show_overlay(c.clone(), None);
        a_handle.focus();
        b_handle.focus();
        b_handle.set_hidden(true);
        terminal.send_input("x");
        render_and_flush(&tui, &terminal).await;
        assert_eq!(a.inputs(), vec!["x".to_string()]);
        assert!(c.inputs().is_empty());
        assert!(a.focused());
        tui.stop(None);
    }
}

mod rendering_order {
    use super::*;

    fn top_left_options() -> Option<OverlayOptions> {
        Some(OverlayOptions {
            row: Some(SizeValue::Absolute(0.0)),
            col: Some(SizeValue::Absolute(0.0)),
            width: Some(SizeValue::Absolute(1.0)),
            non_capturing: Some(true),
            ..Default::default()
        })
    }

    #[tokio::test]
    async fn focus_on_already_focused_overlay_bumps_visual_order() {
        let terminal = Arc::new(VirtualTerminal::new(20, 6));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let editor = FocusableOverlay::new(&["EDITOR"]);
        tui.add_child(Arc::new(EmptyContent));
        tui.set_focus(Some(editor));
        tui.start();

        let a_handle = tui.show_overlay(Arc::new(StaticOverlay::new(vec!["A".to_string()])), top_left_options());
        tui.show_overlay(Arc::new(StaticOverlay::new(vec!["B".to_string()])), top_left_options());
        a_handle.focus();
        tui.show_overlay(Arc::new(StaticOverlay::new(vec!["C".to_string()])), top_left_options());
        render_and_flush(&tui, &terminal).await;
        assert_eq!(terminal.get_viewport()[0].chars().next(), Some('C'));

        a_handle.focus();
        render_and_flush(&tui, &terminal).await;
        assert_eq!(terminal.get_viewport()[0].chars().next(), Some('A'));
        assert!(a_handle.is_focused());
        tui.stop(None);
    }

    #[tokio::test]
    async fn default_rendering_order_for_overlapping_overlays_follows_creation_order() {
        let terminal = Arc::new(VirtualTerminal::new(20, 6));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        tui.add_child(Arc::new(EmptyContent));
        tui.start();

        tui.show_overlay(Arc::new(StaticOverlay::new(vec!["A".to_string()])), top_left_options());
        tui.show_overlay(Arc::new(StaticOverlay::new(vec!["B".to_string()])), top_left_options());
        render_and_flush(&tui, &terminal).await;
        assert_eq!(terminal.get_viewport()[0].chars().next(), Some('B'));
        tui.stop(None);
    }

    #[tokio::test]
    async fn focus_on_lower_overlay_renders_it_on_top() {
        let terminal = Arc::new(VirtualTerminal::new(20, 6));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        tui.add_child(Arc::new(EmptyContent));
        tui.start();

        let lower = tui.show_overlay(Arc::new(StaticOverlay::new(vec!["A".to_string()])), top_left_options());
        tui.show_overlay(Arc::new(StaticOverlay::new(vec!["B".to_string()])), top_left_options());
        render_and_flush(&tui, &terminal).await;
        assert_eq!(terminal.get_viewport()[0].chars().next(), Some('B'));
        lower.focus();
        render_and_flush(&tui, &terminal).await;
        assert_eq!(terminal.get_viewport()[0].chars().next(), Some('A'));
        tui.stop(None);
    }

    #[tokio::test]
    async fn focusing_middle_overlay_places_it_on_top_while_preserving_others_relative_order() {
        let terminal = Arc::new(VirtualTerminal::new(20, 6));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        tui.add_child(Arc::new(EmptyContent));
        tui.start();

        tui.show_overlay(Arc::new(StaticOverlay::new(vec!["A".to_string()])), top_left_options());
        let middle = tui.show_overlay(Arc::new(StaticOverlay::new(vec!["B".to_string()])), top_left_options());
        let top = tui.show_overlay(Arc::new(StaticOverlay::new(vec!["C".to_string()])), top_left_options());
        render_and_flush(&tui, &terminal).await;
        assert_eq!(terminal.get_viewport()[0].chars().next(), Some('C'));
        middle.focus();
        render_and_flush(&tui, &terminal).await;
        assert_eq!(terminal.get_viewport()[0].chars().next(), Some('B'));
        middle.hide();
        render_and_flush(&tui, &terminal).await;
        assert_eq!(terminal.get_viewport()[0].chars().next(), Some('C'));
        top.hide();
        render_and_flush(&tui, &terminal).await;
        assert_eq!(terminal.get_viewport()[0].chars().next(), Some('A'));
        tui.stop(None);
    }

    #[tokio::test]
    async fn capturing_overlay_hidden_and_shown_again_renders_on_top_after_unhide() {
        let terminal = Arc::new(VirtualTerminal::new(20, 6));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        tui.add_child(Arc::new(EmptyContent));
        tui.start();

        tui.show_overlay(Arc::new(StaticOverlay::new(vec!["A".to_string()])), top_left_options());
        let capturing = tui.show_overlay(
            Arc::new(StaticOverlay::new(vec!["B".to_string()])),
            Some(OverlayOptions {
                row: Some(SizeValue::Absolute(0.0)),
                col: Some(SizeValue::Absolute(0.0)),
                width: Some(SizeValue::Absolute(1.0)),
                ..Default::default()
            }),
        );
        render_and_flush(&tui, &terminal).await;
        assert_eq!(terminal.get_viewport()[0].chars().next(), Some('B'));
        capturing.set_hidden(true);
        tui.show_overlay(Arc::new(StaticOverlay::new(vec!["C".to_string()])), top_left_options());
        render_and_flush(&tui, &terminal).await;
        assert_eq!(terminal.get_viewport()[0].chars().next(), Some('C'));
        capturing.set_hidden(false);
        render_and_flush(&tui, &terminal).await;
        assert_eq!(terminal.get_viewport()[0].chars().next(), Some('B'));
        tui.stop(None);
    }

    #[tokio::test]
    async fn unfocus_does_not_change_visual_order_until_another_overlay_is_focused() {
        let terminal = Arc::new(VirtualTerminal::new(20, 6));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let editor = FocusableOverlay::new(&["EDITOR"]);
        tui.add_child(Arc::new(EmptyContent));
        tui.set_focus(Some(editor));
        tui.start();

        let a = tui.show_overlay(Arc::new(StaticOverlay::new(vec!["A".to_string()])), top_left_options());
        let b = tui.show_overlay(Arc::new(StaticOverlay::new(vec!["B".to_string()])), top_left_options());
        render_and_flush(&tui, &terminal).await;
        assert_eq!(terminal.get_viewport()[0].chars().next(), Some('B'));
        a.focus();
        render_and_flush(&tui, &terminal).await;
        assert_eq!(terminal.get_viewport()[0].chars().next(), Some('A'));
        a.unfocus(None);
        render_and_flush(&tui, &terminal).await;
        assert_eq!(terminal.get_viewport()[0].chars().next(), Some('A'));
        b.focus();
        render_and_flush(&tui, &terminal).await;
        assert_eq!(terminal.get_viewport()[0].chars().next(), Some('B'));
        tui.stop(None);
    }
}
