//! Port of packages/coding-agent/test/themed-text.test.ts

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use pi_coding_agent::modes::interactive::components::themed_text::ThemedText;
use pi_coding_agent::modes::interactive::theme::theme::{ThemeColor, init_theme, theme};
use pretty_assertions::assert_eq;
use serial_test::serial;

struct ThemeResetGuard;

impl Drop for ThemeResetGuard {
    fn drop(&mut self) {
        init_theme(Some("dark"), None);
    }
}

#[test]
#[serial]
fn builds_lazily_and_rebuilds_with_the_current_theme_after_invalidation() {
    let _guard = ThemeResetGuard;
    init_theme(Some("dark"), None);

    let builds = Arc::new(AtomicUsize::new(0));
    let builds_c = Arc::clone(&builds);
    let text = ThemedText::new(
        Arc::new(move || {
            builds_c.fetch_add(1, Ordering::SeqCst);
            theme().fg(ThemeColor::Accent, "hello")
        }),
        None,
        None,
    );

    assert_eq!(builds.load(Ordering::SeqCst), 0);
    let dark = text.render(20).join("");

    init_theme(Some("light"), None);
    assert_eq!(text.render(20).join(""), dark);
    text.invalidate();
    let light = text.render(20).join("");
    assert!(light.contains(&theme().get_fg_ansi(ThemeColor::Accent)));
    assert_eq!(builds.load(Ordering::SeqCst), 2);
}
