//! Workspace file tree browser and syntax-highlighted file preview view.

use rune::{Cx, El, ScrollSpec, col, el, row, text};
use rune_highlights::Lang;
use rune_kit::Kit;

use crate::state::{InspectorTab, PiGuiAction, PiGuiState};
use crate::theme::WakuPalette;

/// Renders either the workspace file list or the currently opened file preview.
pub fn view(s: &PiGuiState, k: &Kit, pal: &WakuPalette, _cx: &mut Cx<PiGuiState>) -> El {
    let mut container = col()
        .key("file-view")
        .grow(1.0)
        .min_h(0.0)
        .w_full()
        .padding(12.0)
        .gap(8.0)
        .scroll(ScrollSpec { stick_bottom: false });

    if s.inspector_tab == InspectorTab::File
        && let Some(file) = &s.open_file
    {
        let lang_name = file
            .language
            .as_deref()
            .and_then(|name| Lang::from_name(name).map(|_| name))
            .unwrap_or("plain");
        return container
            .child(
                row()
                    .w_full()
                    .justify_between()
                    .child(text(&file.path).font(k.ty.ui_medium).color(pal.text))
                    .child(
                        text(format!("{} lines ({lang_name})", file.line_count))
                            .font(k.ty.small)
                            .color(pal.text_tertiary),
                    ),
            )
            .child(
                el().w_full()
                    .padding(10.0)
                    .radius(k.radius.sm)
                    .fill(pal.inset)
                    .child(text(&file.content).font(k.ty.mono).color(pal.text_secondary)),
            );
    }

    if s.files.is_empty() {
        return container.child(
            text("No workspace files loaded")
                .key("file-view-empty")
                .font(k.ty.small)
                .color(pal.text_tertiary),
        );
    }

    for entry in &s.files {
        let path = entry.path.clone();
        container = container.child(
            el().key(format!("file-entry-{}", entry.path))
                .w_full()
                .pl(8.0 + (entry.depth as f32) * 12.0)
                .pr(8.0)
                .py(4.0)
                .radius(k.radius.sm)
                .hover(pal.overlay)
                .on_press(move |st: &mut PiGuiState| st.dispatch(PiGuiAction::OpenFile(path.clone()), None))
                .child(
                    text(&entry.name)
                        .font(k.ty.ui)
                        .color(if entry.modified { pal.accent } else { pal.text }),
                ),
        );
    }
    container
}
