//! Port of packages/coding-agent/src/modes/interactive/components/easter-egg-3d.lazy.ts
//! and packages/coding-agent/src/modes/interactive/components/armin-3d.lazy.ts

use std::sync::Arc;

use pi_tui::{TUI, TuiAltScreen};

use super::easter_egg_3d::{EasterEgg3d, play_easter_egg_3d};

/// Plays a 3D easter egg (see easter_egg_3d.rs). Only fullscreen mode (`TuiAltScreen`) can show it, because
/// it dissolves the rendered screen. The screen is captured before starting. Returns false when it cannot play.
fn play_easter_egg_3d_lazy(alt_screen: Option<&Arc<TuiAltScreen>>, egg: EasterEgg3d) -> bool {
    let Some(alt_screen) = alt_screen else {
        return false;
    };
    if alt_screen.has_overlay() {
        return true;
    }
    let screen = alt_screen.get_screen_lines();
    let tui: Arc<dyn TUI + Send + Sync> = Arc::clone(alt_screen) as Arc<dyn TUI + Send + Sync>;
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(async move {
            play_easter_egg_3d(tui, screen, egg).await;
        });
    }
    true
}

/// Plays the 3D pi logo, lifting off the header logo whose top-left cell is at `column`, `row`.
pub fn play_pi_logo_3d(alt_screen: Option<&Arc<TuiAltScreen>>, column: usize, row: usize) {
    let _ = play_easter_egg_3d_lazy(alt_screen, EasterEgg3d::PiLogo { column, row });
}

/// Plays the 3D Armin. Returns false when it cannot play, so the caller can fall back to the inline version.
pub fn play_armin_3d(alt_screen: Option<&Arc<TuiAltScreen>>) -> bool {
    play_easter_egg_3d_lazy(alt_screen, EasterEgg3d::Armin)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::time::Duration;

    use async_trait::async_trait;
    use pi_tui::terminal::{Terminal, TerminalInputHandler, TerminalResizeHandler};
    use serial_test::serial;

    use super::*;

    struct TestTerminal {
        on_input: Mutex<Option<TerminalInputHandler>>,
    }

    impl TestTerminal {
        fn new() -> Self {
            Self {
                on_input: Mutex::new(None),
            }
        }
    }

    #[async_trait]
    impl Terminal for TestTerminal {
        fn start(&self, on_input: TerminalInputHandler, _on_resize: TerminalResizeHandler) {
            *self.on_input.lock().unwrap() = Some(on_input);
        }
        fn stop(&self) {
            *self.on_input.lock().unwrap() = None;
        }
        async fn drain_input(&self, _max_ms: Option<i64>, _idle_ms: Option<i64>) {}
        fn write(&self, _data: &str) {}
        fn columns(&self) -> i64 {
            80
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

    #[test]
    fn returns_false_without_alt_screen() {
        assert!(!play_armin_3d(None));
        play_pi_logo_3d(None, 0, 0);
    }

    #[tokio::test(start_paused = true)]
    #[serial]
    async fn plays_3d_easter_eggs_when_alt_screen_present() {
        let term = Arc::new(TestTerminal::new());
        let alt = Arc::new(TuiAltScreen::new(term, None, None, None));

        assert!(play_armin_3d(Some(&alt)));
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(120)).await;
        tokio::task::yield_now().await;
        assert!(alt.has_overlay());

        // Calling while an overlay is already active still returns true without spawning a second overlay
        assert!(play_armin_3d(Some(&alt)));

        // Auto-finish after 1000ms without render
        tokio::time::advance(Duration::from_millis(1100)).await;
        tokio::task::yield_now().await;
        assert!(!alt.has_overlay());

        play_pi_logo_3d(Some(&alt), 2, 1);
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(120)).await;
        tokio::task::yield_now().await;
        assert!(alt.has_overlay());

        tokio::time::advance(Duration::from_millis(1100)).await;
        tokio::task::yield_now().await;
        assert!(!alt.has_overlay());
    }
}
