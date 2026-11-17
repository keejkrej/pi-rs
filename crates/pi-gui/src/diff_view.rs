//! Git diff file list, unified line-numbered hunks, and stage/unstage/revert triggers.

use rune::{Color, Cx, El, ScrollSpec, col, el, rich, row, text};
use rune_kit::syntax::{SyntaxTheme, highlight, lang_for_path};
use rune_kit::{Badge, BadgeVariant, Button, ButtonVariant, ControlSize, Kit};

use crate::models::{DiffFileViewModel, DiffHunkViewModel, DiffLineKind, DiffLineViewModel};
use crate::state::{OverlayKind, PiGuiAction, PiGuiState};
use crate::theme::{WakuPalette, srgba};

/// Renders the workspace git diff summary and line-numbered unified diff viewer.
pub fn view(s: &PiGuiState, k: &Kit, pal: &WakuPalette, _cx: &mut Cx<PiGuiState>) -> El {
    let mut header = row()
        .key("diff-header")
        .w_full()
        .px(12.0)
        .py(8.0)
        .gap(8.0)
        .justify_between()
        .center_y()
        .border_sides([0.0, 0.0, 1.0, 0.0], pal.border);

    let mut left_info = row().gap(6.0).center_y().child(
        text(format!("{} changed files", s.diff.files.len()))
            .font(k.ty.ui_medium)
            .color(pal.text),
    );
    if let Some(branch) = &s.diff.branch {
        left_info = left_info.child(Badge::new(branch).variant(BadgeVariant::Subtle).build(k));
    }
    header = header.child(left_info).child(
        row()
            .gap(6.0)
            .center_y()
            .child(
                text(format!("+{}", s.diff.total_additions))
                    .font(k.ty.small_medium)
                    .color(pal.success),
            )
            .child(
                text(format!("-{}", s.diff.total_deletions))
                    .font(k.ty.small_medium)
                    .color(pal.danger),
            ),
    );

    let mut scroll_body = col()
        .key("diff-scroll")
        .grow(1.0)
        .min_h(0.0)
        .w_full()
        .padding(12.0)
        .gap(10.0)
        .scroll(ScrollSpec { stick_bottom: false });

    if s.diff.files.is_empty() {
        scroll_body = scroll_body.child(
            col()
                .key("diff-empty")
                .w_full()
                .padding(16.0)
                .gap(4.0)
                .radius(k.radius.sm)
                .fill(pal.inset)
                .border(1.0, pal.border)
                .child(
                    text("Working tree clean")
                        .font(k.ty.ui_medium)
                        .color(pal.text_secondary),
                )
                .child(
                    text("Modified, staged, and untracked files will appear here as you work.")
                        .font(k.ty.small)
                        .color(pal.text_tertiary)
                        .wrap(),
                ),
        );
        return col()
            .key("diff-view")
            .grow(1.0)
            .min_h(0.0)
            .w_full()
            .child(header)
            .child(scroll_body);
    }

    scroll_body = scroll_body.child(render_changed_file_strip(s, k, pal));

    let syntax_theme = SyntaxTheme::from_kit(k);
    for file in &s.diff.files {
        scroll_body = scroll_body.child(render_diff_file_card(s, k, pal, &syntax_theme, file));
    }

    col()
        .key("diff-view")
        .grow(1.0)
        .min_h(0.0)
        .w_full()
        .child(header)
        .child(scroll_body)
}

fn status_badge_style(status: &str, pal: &WakuPalette) -> (&'static str, Color, Color) {
    let lower = status.to_ascii_lowercase();
    if lower.contains("untracked") || status == "?" || status == "??" {
        ("U", pal.accent, k_tint(pal.accent, 0.14))
    } else if lower.contains("staged") || status == "M " || status == "A" || status == "added" {
        ("S", pal.success, k_tint(pal.success, 0.14))
    } else if lower.contains("deleted") || status == "D" {
        ("D", pal.danger, pal.danger_soft)
    } else {
        ("M", pal.warning, k_tint(pal.warning, 0.14))
    }
}

fn k_tint(base: Color, alpha: f32) -> Color {
    Color { a: alpha, ..base }
}

fn split_file_path(path: &str) -> (&str, &str) {
    match path.rsplit_once('/') {
        Some((dir, name)) => (dir, name),
        None => ("", path),
    }
}

fn render_changed_file_strip(s: &PiGuiState, k: &Kit, pal: &WakuPalette) -> El {
    let mut strip = col()
        .key("diff-file-list")
        .w_full()
        .gap(4.0)
        .padding(8.0)
        .radius(k.radius.sm)
        .fill(pal.inset)
        .border(1.0, pal.border);

    for file in &s.diff.files {
        let selected = s.diff.selected_path.as_deref() == Some(file.path.as_str());
        let select_path = file.path.clone();
        let (badge_txt, badge_fg, badge_bg) = status_badge_style(&file.status, pal);
        let (dir, name) = split_file_path(&file.path);

        let mut name_row = row().grow(1.0).min_w(0.0).gap(4.0).center_y().child(
            text(name)
                .font(if selected { k.ty.ui_medium } else { k.ty.ui })
                .color(pal.text)
                .truncate(),
        );
        if !dir.is_empty() {
            name_row = name_row.child(text(dir).font(k.ty.small).color(pal.text_tertiary).truncate());
        }

        strip = strip.child(
            row()
                .key(format!("diff-list-item-{}", file.path))
                .w_full()
                .px(8.0)
                .py(5.0)
                .gap(8.0)
                .center_y()
                .radius(k.radius.xs)
                .fill(if selected {
                    pal.sidebar_item_background
                } else {
                    Color::TRANSPARENT
                })
                .hover(pal.overlay)
                .on_press(move |st: &mut PiGuiState| {
                    st.dispatch(PiGuiAction::SelectDiffFile(select_path.clone()), None);
                })
                .child(
                    el().size(18.0, 18.0)
                        .center()
                        .radius(k.radius.xs)
                        .fill(badge_bg)
                        .child(text(badge_txt).font(k.ty.small_medium).color(badge_fg)),
                )
                .child(name_row)
                .child(
                    row()
                        .gap(4.0)
                        .center_y()
                        .child(text(format!("+{}", file.additions)).font(k.ty.small).color(pal.success))
                        .child(text(format!("-{}", file.deletions)).font(k.ty.small).color(pal.danger)),
                ),
        );
    }
    strip
}

fn render_diff_file_card(
    s: &PiGuiState,
    k: &Kit,
    pal: &WakuPalette,
    syntax_theme: &SyntaxTheme,
    file: &DiffFileViewModel,
) -> El {
    let selected = s.diff.selected_path.as_deref() == Some(file.path.as_str());
    let select_path = file.path.clone();
    let open_path = file.path.clone();
    let stage_path = file.path.clone();
    let revert_path = file.path.clone();
    let is_staged = file.status.to_ascii_lowercase().contains("staged") || file.status == "A";
    let (badge_txt, badge_fg, badge_bg) = status_badge_style(&file.status, pal);

    let file_actions = row()
        .gap(4.0)
        .center_y()
        .child(
            Button::new(if is_staged { "Unstage" } else { "Stage" })
                .variant(ButtonVariant::Ghost)
                .control_size(ControlSize::Small)
                .on_press(move |st: &mut PiGuiState| {
                    let cmd = if is_staged {
                        format!("git reset HEAD -- '{}'", stage_path.replace('\'', "'\\''"))
                    } else {
                        format!("git add -- '{}'", stage_path.replace('\'', "'\\''"))
                    };
                    st.dispatch(PiGuiAction::RunTerminalCommand(cmd), None);
                })
                .build(k)
                .key(format!("diff-stage-{}", file.path)),
        )
        .child(
            Button::new("Revert")
                .variant(ButtonVariant::GhostMuted)
                .control_size(ControlSize::Small)
                .on_press(move |st: &mut PiGuiState| {
                    st.dispatch(
                        PiGuiAction::OpenOverlay(OverlayKind::Confirm {
                            title: "Revert file changes?".to_string(),
                            message: format!("Discard local changes to {revert_path}?"),
                            action_id: format!("revert:{revert_path}"),
                        }),
                        None,
                    );
                })
                .build(k)
                .key(format!("diff-revert-{}", file.path)),
        )
        .child(
            Button::new("Open")
                .variant(ButtonVariant::Ghost)
                .control_size(ControlSize::Small)
                .on_press(move |st: &mut PiGuiState| {
                    st.dispatch(PiGuiAction::OpenFile(open_path.clone()), None);
                })
                .build(k)
                .key(format!("diff-open-{}", file.path)),
        );

    let file_header = row()
        .w_full()
        .px(10.0)
        .py(6.0)
        .gap(8.0)
        .justify_between()
        .center_y()
        .hover(pal.overlay)
        .on_press(move |st: &mut PiGuiState| {
            st.dispatch(PiGuiAction::SelectDiffFile(select_path.clone()), None);
        })
        .child(
            row()
                .grow(1.0)
                .min_w(0.0)
                .gap(6.0)
                .center_y()
                .child(
                    el().size(18.0, 18.0)
                        .center()
                        .radius(k.radius.xs)
                        .fill(badge_bg)
                        .child(text(badge_txt).font(k.ty.small_medium).color(badge_fg)),
                )
                .child(text(&file.path).font(k.ty.ui_medium).color(pal.text).truncate())
                .child(text(format!("+{}", file.additions)).font(k.ty.small).color(pal.success))
                .child(text(format!("-{}", file.deletions)).font(k.ty.small).color(pal.danger)),
        )
        .child(file_actions);

    let mut card = col()
        .key(format!("diff-file-{}", file.path))
        .w_full()
        .radius(k.radius.sm)
        .border(1.0, if selected { pal.accent } else { pal.border })
        .fill(pal.raised)
        .child(file_header);

    if !file.collapsed {
        let lang = lang_for_path(&file.path);
        for (hunk_idx, hunk) in file.hunks.iter().enumerate() {
            card = card.child(render_hunk(k, pal, syntax_theme, lang.as_deref(), hunk_idx, hunk));
        }
    }
    card
}

fn render_hunk(
    k: &Kit,
    pal: &WakuPalette,
    syntax_theme: &SyntaxTheme,
    lang: Option<&str>,
    hunk_idx: usize,
    hunk: &DiffHunkViewModel,
) -> El {
    let mut hunk_col = col()
        .key(format!("diff-hunk-{hunk_idx}"))
        .w_full()
        .fill(pal.inset)
        .border_sides([1.0, 0.0, 0.0, 0.0], pal.border)
        .child(
            el().w_full()
                .px(10.0)
                .py(4.0)
                .fill(pal.overlay)
                .child(text(&hunk.header).font(k.ty.mono).color(pal.text_tertiary)),
        );

    for (line_idx, line) in hunk.lines.iter().enumerate() {
        hunk_col = hunk_col.child(render_diff_line(k, pal, syntax_theme, lang, line_idx, line));
    }
    hunk_col
}

fn render_diff_line(
    k: &Kit,
    pal: &WakuPalette,
    syntax_theme: &SyntaxTheme,
    lang: Option<&str>,
    line_idx: usize,
    line: &DiffLineViewModel,
) -> El {
    let add_tint = if pal.is_dark {
        srgba(0.18, 0.65, 0.36, 0.14)
    } else {
        srgba(0.16, 0.60, 0.34, 0.12)
    };
    let (row_bg, marker, marker_color, text_fallback) = match line.kind {
        DiffLineKind::Added => (add_tint, "+", pal.success, pal.text),
        DiffLineKind::Removed => (pal.danger_soft, "-", pal.danger, pal.text),
        DiffLineKind::Context => (Color::TRANSPARENT, " ", pal.text_ghost, pal.text_secondary),
    };
    let old_num = line.old_line.map_or_else(String::new, |n| n.to_string());
    let new_num = line.new_line.map_or_else(String::new, |n| n.to_string());

    let code_el = if let Some(lang_name) = lang {
        let found = highlight(Some(lang_name), &line.text);
        if found.is_empty() {
            text(&line.text).font(k.ty.mono).color(text_fallback)
        } else {
            let spans = syntax_theme.spans(&found, 0);
            rich(&line.text, spans).font(k.ty.mono).color(text_fallback)
        }
    } else {
        text(&line.text).font(k.ty.mono).color(text_fallback)
    };

    row()
        .key(format!("diff-line-{line_idx}"))
        .w_full()
        .px(8.0)
        .py(1.5)
        .gap(6.0)
        .center_y()
        .fill(row_bg)
        .child(el().w(28.0).child(text(old_num).font(k.ty.mono).color(pal.text_ghost)))
        .child(el().w(28.0).child(text(new_num).font(k.ty.mono).color(pal.text_ghost)))
        .child(el().w(12.0).child(text(marker).font(k.ty.mono).color(marker_color)))
        .child(el().grow(1.0).min_w(0.0).child(code_el))
}
