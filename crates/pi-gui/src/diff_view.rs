//! Git diff file list, unified line-numbered hunks, and stage/unstage/revert triggers.

use rune::{Color, Cursor, Cx, El, Icon, ScrollSpec, TextStyle, col, el, icon, rich, row, text};
use rune_kit::Kit;
use rune_kit::syntax::{SyntaxTheme, highlight, lang_for_path};

use crate::models::{DiffFileViewModel, DiffHunkViewModel, DiffLineKind, DiffLineViewModel};
use crate::state::{OverlayKind, PiGuiAction, PiGuiState};
use crate::theme::{WakuPalette, WakuRadius};

static ICON_CHEVRON_DOWN: Icon = Icon {
    name: "waku-diff-chev-down",
    view: 16.0,
    body: r#"<path d="M4.8 6.4L8 9.6l3.2-3.2" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#,
};

static ICON_ROTATE_CW: Icon = Icon {
    name: "waku-diff-refresh",
    view: 24.0,
    body: r#"<path d="M21 12a9 9 0 1 1-9-9c2.52 0 4.93 1 6.74 2.74L21 8" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" fill="none"/><path d="M21 3v5h-5" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#,
};

static ICON_FILE: Icon = Icon {
    name: "waku-diff-file",
    view: 24.0,
    body: r#"<path d="M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" fill="none"/><path d="M14 2v4a2 2 0 0 0 2 2h4" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#,
};

/// Renders Waku's 44px diff toolbar, Commit trigger, and borderless line-numbered unified diff viewer.
pub fn view(s: &PiGuiState, k: &Kit, pal: &WakuPalette, _cx: &mut Cx<PiGuiState>) -> El {
    let source_chip = row()
        .key("diff-source-chip")
        .role("button")
        .label_static("Diff source")
        .h(28.0)
        .px(10.0)
        .gap(6.0)
        .center_y()
        .radius(WakuRadius::SM)
        .fill(pal.surface)
        .border_layout()
        .border(1.0, pal.border)
        .hover(pal.overlay)
        .cursor(Cursor::Pointer)
        .on_press(|st: &mut PiGuiState| st.dispatch(PiGuiAction::CycleDiffSource, None))
        .child(
            text(s.diff_source.label())
                .font(TextStyle::new(12.5, 400.0))
                .color(pal.text),
        )
        .child(icon(&ICON_CHEVRON_DOWN, 11.0).color(pal.text_tertiary));

    let refresh_btn = el()
        .key("diff-refresh-btn")
        .role("button")
        .label_static("Refresh diff")
        .size(28.0, 28.0)
        .radius(7.0)
        .center()
        .hover(pal.overlay)
        .cursor(Cursor::Pointer)
        .on_press(|st: &mut PiGuiState| st.dispatch(PiGuiAction::RefreshDiff, None))
        .child(icon(&ICON_ROTATE_CW, 12.0).color(pal.text_tertiary));

    let commit_btn = row()
        .key("right_panel.changes.commit_btn")
        .role("button")
        .label_static("Commit")
        .w(84.0)
        .h(26.0)
        .center()
        .gap(6.0)
        .radius(6.0)
        .fill(pal.raised)
        .border_layout()
        .border(1.0, pal.border)
        .hover(pal.overlay_strong)
        .cursor(Cursor::Pointer)
        .on_press(|st: &mut PiGuiState| {
            st.dispatch(PiGuiAction::OpenOverlay(OverlayKind::CommitDialog), None);
        })
        .child(text("Commit").font(TextStyle::new(12.5, 500.0)).color(pal.text));

    let header = row()
        .key("diff-header")
        .w_full()
        .h(44.0)
        .pl(12.0)
        .pr(14.0)
        .gap(8.0)
        .center_y()
        .shrink()
        .border_layout()
        .border_sides([0.0, 0.0, 1.0, 0.0], pal.border)
        .child(source_chip)
        .child(
            text(format!("+{}", s.diff.total_additions))
                .font(TextStyle::new(12.5, 500.0))
                .color(pal.success),
        )
        .child(
            text(format!("-{}", s.diff.total_deletions))
                .font(TextStyle::new(12.5, 500.0))
                .color(pal.danger),
        )
        .child(el().grow(1.0))
        .child(refresh_btn)
        .child(commit_btn);

    let mut scroll_body = col()
        .key("diff-scroll")
        .grow(1.0)
        .min_h(0.0)
        .w_full()
        .scroll(ScrollSpec { stick_bottom: false });

    if s.diff.files.is_empty() {
        scroll_body = scroll_body.child(
            col()
                .key("diff-empty")
                .w_full()
                .px(24.0)
                .py(32.0)
                .gap(4.0)
                .center()
                .child(
                    text("No changes")
                        .font(TextStyle::new(13.0, 500.0))
                        .color(pal.text_secondary),
                )
                .child(
                    text("Modified, staged, and untracked files will appear here as you work.")
                        .font(TextStyle::new(12.5, 400.0))
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
        scroll_body = scroll_body.child(render_diff_file_section(s, k, pal, &syntax_theme, file));
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
        .py(4.0)
        .gap(1.0)
        .border_layout()
        .border_sides([0.0, 0.0, 1.0, 0.0], pal.border);

    for file in &s.diff.files {
        let selected = s.diff.selected_path.as_deref() == Some(file.path.as_str());
        let select_path = file.path.clone();
        let (badge_txt, badge_fg, badge_bg) = status_badge_style(&file.status, pal);
        let (dir, name) = split_file_path(&file.path);

        let mut name_row = row().grow(1.0).min_w(0.0).gap(6.0).center_y().child(
            text(name)
                .font(TextStyle::new(12.5, if selected { 500.0 } else { 400.0 }))
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
                .h(28.0)
                .px(12.0)
                .gap(8.0)
                .center_y()
                .fill(if selected {
                    pal.sidebar_item_background
                } else {
                    Color::TRANSPARENT
                })
                .hover(pal.overlay)
                .cursor(Cursor::Pointer)
                .on_press(move |st: &mut PiGuiState| {
                    st.dispatch(PiGuiAction::SelectDiffFile(select_path.clone()), None);
                })
                .child(
                    el().size(16.0, 16.0)
                        .center()
                        .radius(4.0)
                        .fill(badge_bg)
                        .child(text(badge_txt).font(TextStyle::new(10.5, 600.0)).color(badge_fg)),
                )
                .child(name_row)
                .child(
                    row()
                        .gap(6.0)
                        .center_y()
                        .shrink()
                        .child(
                            text(format!("+{}", file.additions))
                                .font(TextStyle::new(12.0, 400.0))
                                .color(pal.success),
                        )
                        .child(
                            text(format!("-{}", file.deletions))
                                .font(TextStyle::new(12.0, 400.0))
                                .color(pal.danger),
                        ),
                ),
        );
    }
    strip
}

fn diff_inline_action(
    key: String,
    label: &'static str,
    pal: &WakuPalette,
    on_press: impl Fn(&mut PiGuiState) + 'static,
) -> El {
    row()
        .key(key)
        .role("button")
        .label_static(label)
        .h(22.0)
        .px(6.0)
        .center()
        .radius(4.0)
        .hover(pal.overlay)
        .cursor(Cursor::Pointer)
        .on_press(on_press)
        .child(text(label).font(TextStyle::new(11.5, 400.0)).color(pal.text_tertiary))
}

fn render_diff_file_section(
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

    let file_actions = row()
        .gap(2.0)
        .center_y()
        .shrink()
        .child(diff_inline_action(
            format!("diff-stage-{}", file.path),
            if is_staged { "Unstage" } else { "Stage" },
            pal,
            move |st: &mut PiGuiState| {
                let cmd = if is_staged {
                    format!("git reset HEAD -- '{}'", stage_path.replace('\'', "'\\''"))
                } else {
                    format!("git add -- '{}'", stage_path.replace('\'', "'\\''"))
                };
                st.dispatch(PiGuiAction::RunTerminalCommand(cmd), None);
            },
        ))
        .child(diff_inline_action(
            format!("diff-revert-{}", file.path),
            "Revert",
            pal,
            move |st: &mut PiGuiState| {
                st.dispatch(
                    PiGuiAction::OpenOverlay(OverlayKind::Confirm {
                        title: "Revert file changes?".to_string(),
                        message: format!("Discard local changes to {revert_path}?"),
                        action_id: format!("revert:{revert_path}"),
                    }),
                    None,
                );
            },
        ))
        .child(diff_inline_action(
            format!("diff-open-{}", file.path),
            "Open",
            pal,
            move |st: &mut PiGuiState| {
                st.dispatch(PiGuiAction::OpenFile(open_path.clone()), None);
            },
        ));

    let file_header = row()
        .w_full()
        .h(34.0)
        .px(12.0)
        .gap(8.0)
        .justify_between()
        .center_y()
        .fill(if selected { pal.raised } else { pal.surface })
        .border_layout()
        .border_sides([0.0, 0.0, 1.0, 0.0], pal.border)
        .hover(pal.overlay)
        .cursor(Cursor::Pointer)
        .on_press(move |st: &mut PiGuiState| {
            st.dispatch(PiGuiAction::SelectDiffFile(select_path.clone()), None);
        })
        .child(
            row()
                .grow(1.0)
                .min_w(0.0)
                .gap(8.0)
                .center_y()
                .child(icon(&ICON_FILE, 13.0).color(pal.text_tertiary).shrink())
                .child(
                    text(&file.path)
                        .font(TextStyle::new(12.5, 500.0))
                        .color(pal.text_secondary)
                        .truncate()
                        .grow(1.0)
                        .min_w(0.0),
                )
                .child(
                    text(format!("+{}", file.additions))
                        .font(TextStyle::new(12.5, 400.0))
                        .color(pal.success)
                        .shrink(),
                )
                .child(
                    text(format!("-{}", file.deletions))
                        .font(TextStyle::new(12.5, 400.0))
                        .color(pal.danger)
                        .shrink(),
                ),
        )
        .child(file_actions);

    let mut section = col()
        .key(format!("diff-file-{}", file.path))
        .w_full()
        .min_w(0.0)
        .border_layout()
        .border_sides([0.0, 0.0, 1.0, 0.0], pal.border)
        .child(file_header);

    if !file.collapsed {
        let lang = lang_for_path(&file.path);
        for (hunk_idx, hunk) in file.hunks.iter().enumerate() {
            section = section.child(render_hunk(k, pal, syntax_theme, lang.as_deref(), hunk_idx, hunk));
        }
    }
    section
}

fn render_hunk(
    k: &Kit,
    pal: &WakuPalette,
    syntax_theme: &SyntaxTheme,
    lang: Option<&str>,
    hunk_idx: usize,
    hunk: &DiffHunkViewModel,
) -> El {
    let mut hunk_col = col().key(format!("diff-hunk-{hunk_idx}")).w_full().child(
        row()
            .w_full()
            .min_h(24.0)
            .px(12.0)
            .py(4.0)
            .center_y()
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
    let (row_bg, marker, marker_color, text_fallback, display_text) = match line.kind {
        DiffLineKind::Added => (
            pal.diff_added_bg(),
            "+",
            pal.success,
            pal.text,
            line.text.strip_prefix('+').unwrap_or(&line.text),
        ),
        DiffLineKind::Removed => (
            pal.diff_removed_bg(),
            "-",
            pal.danger,
            pal.text,
            line.text.strip_prefix('-').unwrap_or(&line.text),
        ),
        DiffLineKind::Context => (
            Color::TRANSPARENT,
            " ",
            pal.text_ghost,
            pal.text_secondary,
            line.text.strip_prefix(' ').unwrap_or(&line.text),
        ),
    };
    let old_num = line.old_line.map_or_else(String::new, |n| n.to_string());
    let new_num = line.new_line.map_or_else(String::new, |n| n.to_string());

    let code_el = if let Some(lang_name) = lang {
        let found = highlight(Some(lang_name), display_text);
        if found.is_empty() {
            text(display_text).font(k.ty.mono).color(text_fallback)
        } else {
            let spans = syntax_theme.spans(&found, 0);
            rich(display_text, spans).font(k.ty.mono).color(text_fallback)
        }
    } else {
        text(display_text).font(k.ty.mono).color(text_fallback)
    }
    .grow(1.0)
    .min_w(0.0)
    .shrink()
    .wrap_anywhere();

    row()
        .key(format!("diff-line-{line_idx}"))
        .w_full()
        .min_w(0.0)
        .px(8.0)
        .py(1.5)
        .gap(6.0)
        .items_start()
        .fill(row_bg)
        .child(
            el().w(28.0)
                .shrink()
                .child(text(old_num).font(k.ty.mono).color(pal.text_ghost)),
        )
        .child(
            el().w(28.0)
                .shrink()
                .child(text(new_num).font(k.ty.mono).color(pal.text_ghost)),
        )
        .child(
            el().w(12.0)
                .shrink()
                .child(text(marker).font(k.ty.mono).color(marker_color)),
        )
        .child(code_el)
}
