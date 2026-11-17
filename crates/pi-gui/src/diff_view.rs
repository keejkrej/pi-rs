//! Diff summary and per-file hunk viewer for the inspector panel.

use rune::{Cx, El, ScrollSpec, col, el, row, text};
use rune_kit::Kit;

use crate::state::{DiffLineKind, PiGuiAction, PiGuiState};
use crate::theme::WakuPalette;

/// Renders changed files and diff hunks in the inspector panel.
pub fn view(s: &PiGuiState, k: &Kit, pal: &WakuPalette, _cx: &mut Cx<PiGuiState>) -> El {
    let mut body = col()
        .key("diff-view")
        .grow(1.0)
        .min_h(0.0)
        .w_full()
        .padding(12.0)
        .gap(8.0)
        .scroll(ScrollSpec { stick_bottom: false });

    body = body.child(
        row()
            .key("diff-header")
            .w_full()
            .justify_between()
            .child(
                text(format!("{} changed files", s.diff.files.len()))
                    .font(k.ty.ui_medium)
                    .color(pal.text),
            )
            .child(
                text(format!("+{} -{}", s.diff.total_additions, s.diff.total_deletions))
                    .font(k.ty.small)
                    .color(pal.text_secondary),
            ),
    );

    if s.diff.files.is_empty() {
        return body.child(
            text("Working tree clean")
                .key("diff-empty")
                .font(k.ty.small)
                .color(pal.text_tertiary),
        );
    }

    for file in &s.diff.files {
        let path = file.path.clone();
        let selected = s.diff.selected_path.as_deref() == Some(file.path.as_str());
        let mut card = col()
            .key(format!("diff-file-{}", file.path))
            .w_full()
            .radius(k.radius.sm)
            .border(1.0, if selected { pal.accent } else { pal.border })
            .fill(pal.raised)
            .child(
                row()
                    .w_full()
                    .px(10.0)
                    .py(6.0)
                    .justify_between()
                    .hover(pal.overlay)
                    .on_press(move |st: &mut PiGuiState| {
                        st.dispatch(PiGuiAction::SelectDiffFile(path.clone()), None);
                    })
                    .child(text(&file.path).font(k.ty.ui_medium).color(pal.text))
                    .child(
                        text(format!("+{} -{}", file.additions, file.deletions))
                            .font(k.ty.small)
                            .color(pal.text_secondary),
                    ),
            );

        if !file.collapsed {
            for hunk in &file.hunks {
                let mut hunk_col = col()
                    .w_full()
                    .px(8.0)
                    .py(4.0)
                    .fill(pal.inset)
                    .child(text(&hunk.header).font(k.ty.mono).color(pal.text_tertiary));
                for line in &hunk.lines {
                    let color = match line.kind {
                        DiffLineKind::Added => pal.success,
                        DiffLineKind::Removed => pal.danger,
                        DiffLineKind::Context => pal.text_secondary,
                    };
                    hunk_col = hunk_col.child(el().child(text(&line.text).font(k.ty.mono).color(color)));
                }
                card = card.child(hunk_col);
            }
        }
        body = body.child(card);
    }
    body
}
