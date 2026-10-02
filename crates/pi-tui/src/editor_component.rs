//! Port of packages/tui/src/editor-component.ts

use std::sync::Arc;

use crate::autocomplete::AutocompleteProvider;
use crate::tui::Component;

pub type EditorTextCallback = Arc<dyn Fn(&str) + Send + Sync>;
pub type EditorBorderColor = Arc<dyn Fn(&str) -> String + Send + Sync>;

/// Interface for custom editor components.
///
/// This allows extensions to provide their own editor implementation
/// (e.g., vim mode, emacs mode, custom keybindings) while maintaining
/// compatibility with the core application.
///
/// `handleInput` is [`Component::handle_input`] (required for an editor; the TS `Component` method is optional).
/// Implementors must return true from [`Component::has_handle_input`]. Focusable editors also return
/// `Some(self)` from [`Component::as_focusable`].
pub trait EditorComponent: Component {
    // =========================================================================
    // Core text access (required)
    // =========================================================================

    /// Get the current text content.
    fn get_text(&self) -> String;

    /// Set the text content.
    fn set_text(&self, text: &str);

    // =========================================================================
    // Callbacks (required)
    // =========================================================================

    /// Called when user submits (e.g., Enter key).
    fn on_submit(&self) -> Option<EditorTextCallback>;

    fn set_on_submit(&self, on_submit: Option<EditorTextCallback>);

    /// Called when text changes.
    fn on_change(&self) -> Option<EditorTextCallback>;

    fn set_on_change(&self, on_change: Option<EditorTextCallback>);

    // =========================================================================
    // History support (optional)
    // =========================================================================

    /// Add text to history for up/down navigation.
    fn add_to_history(&self, _text: &str) {}

    // =========================================================================
    // Advanced text manipulation (optional)
    // =========================================================================

    /// Insert text at current cursor position.
    fn insert_text_at_cursor(&self, _text: &str) {}

    /// Get text with any markers expanded (e.g., paste markers).
    /// Falls back to getText() if not implemented.
    fn get_expanded_text(&self) -> String {
        self.get_text()
    }

    // =========================================================================
    // Autocomplete support (optional)
    // =========================================================================

    /// Set the autocomplete provider.
    fn set_autocomplete_provider(&self, _provider: Arc<dyn AutocompleteProvider>) {}

    // =========================================================================
    // Appearance (optional)
    // =========================================================================

    /// Border color function. `None` is a missing property.
    fn border_color(&self) -> Option<EditorBorderColor> {
        None
    }

    /// Assignment form of `borderColor`.
    fn set_border_color(&self, _border_color: Option<EditorBorderColor>) {}

    /// Set horizontal padding.
    fn set_padding_x(&self, _padding: f64) {}

    /// Set max visible items in autocomplete dropdown.
    fn set_autocomplete_max_visible(&self, _max_visible: f64) {}
}
