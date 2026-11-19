//! Workspace file tree (`rune_kit::Tree`) and `rune-highlights` code preview view.

use indexmap::IndexSet;
use rune::{Cx, El, ScrollSpec, col, el, rich, row, text};
use rune_highlights::Lang;
use rune_kit::syntax::{SyntaxTheme, highlight, lang_for_path};
use rune_kit::{Badge, BadgeVariant, FileNode, Kit, Tree, TreeItem, TreeStyle};

use crate::models::OpenFileView;
use crate::state::{PiGuiAction, PiGuiState};
use crate::theme::{LayoutMetrics, WakuPalette};

/// Renders Waku's side-by-side 184px workspace file tree and syntax-highlighted file preview pane.
pub fn view(s: &PiGuiState, k: &Kit, pal: &WakuPalette, _cx: &mut Cx<PiGuiState>) -> El {
    let tree_section = col()
        .key("right_panel.files.tree")
        .role("list")
        .w(LayoutMetrics::DEFAULT_FILE_TREE_WIDTH)
        .h_full()
        .border_layout()
        .border_sides([0.0, 1.0, 0.0, 0.0], pal.border)
        .child(render_tree_section(s, k, pal));

    let preview = if let Some(file) = &s.open_file {
        el().grow(1.0)
            .min_w(0.0)
            .min_h(0.0)
            .h_full()
            .child(render_open_file_pane(k, pal, file))
    } else {
        col().grow(1.0).min_w(0.0).h_full().center().child(
            text("Select a file to preview")
                .font(k.ty.small)
                .color(pal.text_tertiary),
        )
    };

    row()
        .key("file-view")
        .grow(1.0)
        .min_h(0.0)
        .w_full()
        .h_full()
        .child(tree_section)
        .child(preview)
}

fn render_tree_section(s: &PiGuiState, k: &Kit, pal: &WakuPalette) -> El {
    let header = row()
        .key("file-tree-header")
        .w_full()
        .px(10.0)
        .py(8.0)
        .justify_between()
        .center_y()
        .border_sides([0.0, 0.0, 1.0, 0.0], pal.border)
        .child(text("Files").font(k.ty.ui_medium).color(pal.text))
        .child(
            Badge::new(format!("{}", s.files.len()))
                .variant(BadgeVariant::Subtle)
                .build(k),
        );

    if s.file_tree.visible_len() > 0 {
        let modified_paths: IndexSet<String> = s.files.iter().filter(|f| f.modified).map(|f| f.path.clone()).collect();
        let accent = pal.accent;
        let tree_style = TreeStyle::from_kit(k).chevron_only_folders(true).guides_on_hover(0.35);
        let tree_el = Tree::new(
            |st: &PiGuiState| &st.file_tree,
            |st: &mut PiGuiState| &mut st.file_tree,
            FileNode::label,
        )
        .name("Workspace Files")
        .style(tree_style)
        .row(move |node, row_info| {
            let mut item = TreeItem::file(node, row_info);
            if modified_paths.contains(&node.path) {
                item = item.dot(accent).label_color(accent).badge("M");
            }
            item
        })
        .on_open(|st: &mut PiGuiState, id, tx| {
            st.dispatch(PiGuiAction::OpenFile(id.to_owned()), Some(tx));
        })
        .on_select(|st: &mut PiGuiState, id, tx| {
            if let Some(entry) = st.files.iter().find(|f| f.path == id)
                && !entry.is_dir
            {
                st.dispatch(PiGuiAction::OpenFile(id.to_owned()), Some(tx));
            }
        })
        .build(s, k)
        .grow(1.0)
        .min_h(0.0)
        .w_full();

        return col()
            .key("file-tree-pane")
            .grow(1.0)
            .min_h(0.0)
            .w_full()
            .child(header)
            .child(tree_el);
    }

    let mut fallback = col()
        .key("file-tree-fallback")
        .grow(1.0)
        .min_h(0.0)
        .w_full()
        .padding(8.0)
        .gap(2.0)
        .scroll(ScrollSpec { stick_bottom: false });

    if s.files.is_empty() {
        fallback = fallback.child(
            text("No workspace files loaded")
                .key("file-view-empty")
                .font(k.ty.small)
                .color(pal.text_tertiary)
                .padding(8.0),
        );
    } else {
        for entry in &s.files {
            let path = entry.path.clone();
            let is_dir = entry.is_dir;
            fallback = fallback.child(
                row()
                    .key(format!("file-entry-{}", entry.path))
                    .w_full()
                    .pl(8.0 + (entry.depth as f32) * 12.0)
                    .pr(8.0)
                    .py(4.0)
                    .gap(6.0)
                    .justify_between()
                    .center_y()
                    .radius(k.radius.xs)
                    .hover(pal.overlay)
                    .on_press(move |st: &mut PiGuiState| {
                        if !is_dir {
                            st.dispatch(PiGuiAction::OpenFile(path.clone()), None);
                        }
                    })
                    .child(
                        text(&entry.name)
                            .font(k.ty.ui)
                            .color(if entry.modified { pal.accent } else { pal.text })
                            .truncate(),
                    )
                    .child(
                        text(if entry.modified { "M" } else { "" })
                            .font(k.ty.small_medium)
                            .color(pal.accent),
                    ),
            );
        }
    }

    col()
        .key("file-tree-pane")
        .grow(1.0)
        .min_h(0.0)
        .w_full()
        .child(header)
        .child(fallback)
}

fn render_open_file_pane(k: &Kit, pal: &WakuPalette, file: &OpenFileView) -> El {
    let resolved_lang = file
        .language
        .as_deref()
        .and_then(|name| Lang::from_name(name).map(|l| l.name()))
        .or_else(|| lang_for_path(&file.path).and_then(|name| Lang::from_name(&name).map(|l| l.name())));
    let lang_label = resolved_lang.unwrap_or("plain");

    let mut controls = row().gap(6.0).center_y();
    if file.dirty {
        controls = controls.child(Badge::new("Unsaved").variant(BadgeVariant::Accent).build(k));
    }
    controls = controls
        .child(Badge::new(lang_label).variant(BadgeVariant::Subtle).build(k))
        .child(
            text(format!("{} lines", file.line_count))
                .font(k.ty.small)
                .color(pal.text_tertiary),
        );

    let file_header = row()
        .key("open-file-header")
        .w_full()
        .px(12.0)
        .py(8.0)
        .gap(8.0)
        .justify_between()
        .center_y()
        .border_sides([0.0, 0.0, 1.0, 0.0], pal.border)
        .child(text(&file.path).font(k.ty.ui_medium).color(pal.text).truncate())
        .child(controls);

    let syntax_theme = SyntaxTheme::from_kit(k);
    let mut lines_col = col()
        .key("open-file-lines")
        .grow(1.0)
        .min_h(0.0)
        .min_w(0.0)
        .w_full()
        .padding(10.0)
        .gap(2.0)
        .fill(pal.inset)
        .scroll(ScrollSpec { stick_bottom: false });

    for (idx, line) in file.content.lines().enumerate() {
        let rendered_line = if let Some(lang) = resolved_lang {
            let hl = highlight(Some(lang), line);
            if hl.is_empty() {
                text(line).font(k.ty.mono).color(pal.text)
            } else {
                let spans = syntax_theme.spans(&hl, 0);
                rich(line, spans).font(k.ty.mono).color(pal.text)
            }
        } else {
            text(line).font(k.ty.mono).color(pal.text)
        }
        .grow(1.0)
        .min_w(0.0)
        .shrink()
        .wrap_anywhere();
        lines_col = lines_col.child(
            row()
                .key(format!("file-line-{idx}"))
                .w_full()
                .min_w(0.0)
                .gap(10.0)
                .items_start()
                .child(
                    el().w(34.0)
                        .shrink()
                        .child(text(format!("{:>4}", idx + 1)).font(k.ty.mono).color(pal.text_ghost)),
                )
                .child(rendered_line),
        );
    }

    col()
        .key("open-file-view")
        .grow(1.0)
        .min_h(0.0)
        .min_w(0.0)
        .w_full()
        .child(file_header)
        .child(lines_col)
}
