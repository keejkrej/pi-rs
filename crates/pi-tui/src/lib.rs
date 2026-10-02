//! Port of packages/tui/src/index.ts

// @generated-mods begin (scaffold-owned, do not edit)
pub mod alt_screen_search;
pub mod autocomplete;
pub mod colors;
pub mod components;
pub mod editor_component;
pub mod fuzzy;
pub mod keybindings;
pub mod keys;
pub mod kill_ring;
pub mod latex;
pub mod layout;
pub mod layout_node;
pub mod native_modifiers;
pub mod native_platform;
pub mod oklab;
pub mod stdin_buffer;
pub mod terminal;
pub mod terminal_colors;
pub mod terminal_image;
pub mod tui;
pub mod tui_alt_screen;
pub mod tui_main_screen;
pub mod undo_stack;
pub mod utils;
pub mod vendor;
pub mod wheel_scroll;
pub mod word_navigation;
// @generated-mods end
pub use pi_js::{Error, Result};

// Core TUI interfaces and classes
pub use crate::vendor::marked::{Marked, Token, Tokens};
// Autocomplete support
pub use crate::autocomplete::{
    AutocompleteItem, AutocompleteProvider, AutocompleteSuggestions, CombinedAutocompleteProvider, SlashCommand,
};
// Colors and styling
pub use crate::colors::{
    Color, ColorMixSpace, IndexedColor, OkhslChannels, OklchChannels, OklchColorValue, RgbColorValue,
    TerminalColorMode, TextAttributes, TextStyle, background_ansi, color_to_hex, color_to_okhsl, color_to_oklch,
    color_to_rgb, foreground_ansi, indexed_color, mix_colors, okhsl_color, oklch_color, parse_color, rgb_color,
    style_text, style_text_with_ansi,
};
// Components
pub use crate::components::box_::TuiBox as Box;
pub use crate::components::cancellable_loader::CancellableLoader;
pub use crate::components::editor::{Editor, EditorOptions, EditorTheme};
pub use crate::components::h_stack::HStack;
pub use crate::components::image::{Image, ImageOptions, ImageTheme};
pub use crate::components::input::Input;
pub use crate::components::loader::{Loader, LoaderIndicatorOptions};
pub use crate::components::markdown::{DefaultTextStyle, Markdown, MarkdownOptions, MarkdownTheme};
pub use crate::components::mouse_region::{MouseRegion, MouseRegionHandler};
pub use crate::components::scroll_view::{
    ScrollView, ScrollViewOptions, ScrollViewScrollToOptions, ScrollViewScrollbar,
};
pub use crate::components::select_list::{
    SelectItem, SelectList, SelectListLayoutOptions, SelectListTheme, SelectListTruncatePrimaryContext,
};
pub use crate::components::settings_list::{SettingItem, SettingsList, SettingsListTheme};
pub use crate::components::spacer::Spacer;
pub use crate::components::text::Text;
pub use crate::components::truncated_text::TruncatedText;
pub use crate::components::v_stack::{StackChild, StackEntry, StackEntryOptions, StackOptions, VStack};
// Editor component interface (for custom editors)
pub use crate::editor_component::EditorComponent;
// Fuzzy matching
pub use crate::fuzzy::{FuzzyMatch, fuzzy_filter, fuzzy_match};
// Keybindings
pub use crate::keybindings::{
    Keybinding, KeybindingConflict, KeybindingDefinition, KeybindingDefinitions, Keybindings, KeybindingsConfig,
    KeybindingsManager, TUI_KEYBINDINGS, get_keybindings, set_keybindings,
};
// Keyboard input handling
pub use crate::keys::{
    Key, KeyEventType, KeyId, decode_kitty_printable, is_key_release, is_key_repeat, is_kitty_protocol_active,
    matches_key, parse_key, set_kitty_protocol_active,
};
// LaTeX rendering
pub use crate::latex::{RenderLatexOptions, render_latex};
// Native platform integration
pub use crate::native_platform::{NativeClipboard, get_native_clipboard};
pub use crate::oklab::oklab_to_okhsl_lightness;
// Input buffering for batch splitting
pub use crate::stdin_buffer::{StdinBuffer, StdinBufferEventMap, StdinBufferOptions};
// Terminal interface and implementations
pub use crate::terminal::{ProcessTerminal, Terminal, is_apple_terminal_session};
// Terminal colors
pub use crate::terminal_colors::{RgbColor, TerminalColorScheme, TerminalColors, parse_terminal_color_scheme_report};
// Terminal image support
pub use crate::terminal_image::{
    CellDimensions, ImageDimensions, ImageProtocol, ImageRenderOptions, TerminalCapabilities, allocate_image_id,
    calculate_image_rows, delete_all_kitty_images, delete_kitty_image, detect_capabilities, encode_iterm2,
    encode_kitty, get_capabilities, get_cell_dimensions, get_gif_dimensions, get_image_dimensions, get_jpeg_dimensions,
    get_png_dimensions, get_terminal_color_mode, get_webp_dimensions, hyperlink, image_fallback, render_image,
    reset_capabilities_cache, set_capabilities, set_capability_overrides, set_cell_dimensions,
};
pub use crate::tui::{
    CURSOR_MARKER, Component, Container, Focusable, OverlayAnchor, OverlayBounds, OverlayHandle, OverlayMargin,
    OverlayOptions, OverlayUnfocusOptions, SizeValue, TUI, TuiInputListener, TuiInputListenerResult, TuiMode,
    TuiMouseButton, TuiMouseEvent, TuiMouseEventResult, TuiMouseEventType, TuiStopOptions, ViewportTUI,
    composite_tui_line, is_focusable, is_viewport_tui,
};
pub use crate::tui_alt_screen::{TuiAltScreen, TuiAltScreenOptions};
pub use crate::tui_main_screen::{TuiMainScreen, TuiMainScreenRenderState};
// Utilities
pub use crate::utils::{
    get_osc8_link_at_column, slice_by_column, strip_terminal_sequences, truncate_to_width, visible_width,
    wrap_text_with_ansi,
};
pub use crate::wheel_scroll::WheelScrollLines;
