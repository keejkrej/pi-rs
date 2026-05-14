pub mod autocomplete;
pub mod components;
pub mod editor_component;
pub mod fuzzy;
pub mod keybindings;
pub mod keys;
pub mod kill_ring;
pub mod stdin_buffer;
pub mod terminal;
pub mod terminal_image;
pub mod tui;
pub mod undo_stack;
pub mod utils;

pub use autocomplete::{
    AutocompleteItem, AutocompleteProvider, AutocompleteSuggestions, CombinedAutocompleteProvider,
    SlashCommand,
};
pub use components::{
    Box, CancellableLoader, DefaultTextStyle, Editor, EditorOptions, EditorTheme, Image,
    ImageOptions, ImageTheme, Input, Loader, LoaderIndicatorOptions, Markdown, MarkdownTheme,
    SelectItem, SelectList, SelectListLayoutOptions, SelectListTheme,
    SelectListTruncatePrimaryContext, SettingItem, SettingsList, SettingsListOptions,
    SettingsListTheme, Spacer, Text, TextChunk, TruncatedText, word_wrap_line,
};
pub use editor_component::EditorComponent;
pub use fuzzy::{FuzzyMatch, fuzzy_filter, fuzzy_match};
pub use keybindings::{
    Keybinding, KeybindingConflict, KeybindingDefinition, KeybindingsConfig, KeybindingsManager,
    TUI_KEYBINDINGS, get_keybindings, set_keybindings, tui_keybindings,
};
pub use keys::{
    Key, KeyEventType, KeyId, decode_kitty_printable, decode_printable_key, is_key_release,
    is_key_repeat, is_kitty_protocol_active, matches_key, parse_key, set_kitty_protocol_active,
};
pub use kill_ring::KillRing;
pub use stdin_buffer::{StdinBuffer, StdinBufferEvent, StdinBufferEventMap, StdinBufferOptions};
pub use terminal::{ProcessTerminal, Terminal};
pub use terminal_image::{
    CellDimensions, ImageDimensions, ImageProtocol, ImageRenderOptions, RenderedImage,
    TerminalCapabilities, allocate_image_id, calculate_image_cell_size, calculate_image_rows,
    delete_all_kitty_images, delete_kitty_image, detect_capabilities, encode_iterm2, encode_kitty,
    get_capabilities, get_cell_dimensions, get_gif_dimensions, get_image_dimensions,
    get_jpeg_dimensions, get_png_dimensions, get_webp_dimensions, hyperlink, image_fallback,
    is_image_line, render_image, reset_capabilities_cache, set_capabilities, set_cell_dimensions,
};
pub use tui::{
    CURSOR_MARKER, Component, Container, Focusable, OverlayAnchor, OverlayHandle, OverlayMargin,
    OverlayOptions, SizeValue, TUI,
};
pub use undo_stack::UndoStack;
pub use utils::{
    AnsiCode, apply_background_to_line, extract_ansi_code, normalize_terminal_output,
    slice_by_column, slice_with_width, truncate_to_width, truncate_to_width_with, visible_width,
    wrap_text, wrap_text_with_ansi,
};
