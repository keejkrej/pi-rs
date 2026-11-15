//! Port of packages/tui/test/layout.test.ts

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use pi_tui::components::scroll_view::{ScrollViewFollow, ScrollViewOptions, ScrollViewScrollbar};
use pi_tui::components::stack::{StackChild, StackEntry, StackOptions, StackVisibleFn};
use pi_tui::layout::render_layout_frame;
use pi_tui::layout_node::{StackAlign, StackBasis};
use pi_tui::terminal_image::{EncodeKittyOptions, KittyImageMetadata, encode_kitty, register_kitty_image_metadata};
use pi_tui::{Component, HStack, ScrollView, Text, VStack, strip_terminal_sequences};

fn visible_lines(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .map(|line| strip_terminal_sequences(line).trim_end().to_string())
        .collect()
}

fn noop_render() -> Arc<dyn Fn() + Send + Sync> {
    Arc::new(|| {})
}

struct CustomComponent {
    render_fn: Arc<dyn Fn(usize) -> Vec<String> + Send + Sync>,
}

impl CustomComponent {
    fn new(render_fn: impl Fn(usize) -> Vec<String> + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self {
            render_fn: Arc::new(render_fn),
        })
    }
}

impl Component for CustomComponent {
    fn render(&self, width: usize) -> Vec<String> {
        (self.render_fn)(width)
    }

    fn invalidate(&self) {}
}

#[test]
fn allocates_vertical_grow_space_deterministically() {
    let frame = render_layout_frame(
        Arc::new(VStack::new(
            Some(vec![
                StackChild::Entry(StackEntry {
                    component: Arc::new(Text::new(Some("top"), Some(0), Some(0), None)),
                    basis: Some(StackBasis::Fixed(1.0)),
                    grow: None,
                    shrink: Some(0),
                    min_size: None,
                    max_size: None,
                    visible: None,
                }),
                StackChild::Entry(StackEntry {
                    component: Arc::new(Text::new(Some("body"), Some(0), Some(0), None)),
                    basis: Some(StackBasis::Fixed(0.0)),
                    grow: Some(1),
                    shrink: None,
                    min_size: None,
                    max_size: None,
                    visible: None,
                }),
            ]),
            None,
        )),
        10,
        4,
        noop_render(),
    );

    let heights: Vec<i64> = frame
        .root
        .children()
        .into_iter()
        .map(|child| child.rect().height)
        .collect();
    assert_eq!(heights, vec![1, 3]);
    assert_eq!(visible_lines(&frame.lines), vec!["top", "body", "", ""]);
}

#[test]
fn does_not_render_fixed_basis_scroll_content_during_stack_measurement() {
    let render_count = Arc::new(AtomicUsize::new(0));
    let count_clone = Arc::clone(&render_count);
    let transcript = ScrollView::new(
        CustomComponent::new(move |_| {
            count_clone.fetch_add(1, Ordering::SeqCst);
            vec!["one".to_string(), "two".to_string(), "three".to_string()]
        }),
        None,
    );
    let root = Arc::new(VStack::new(
        Some(vec![
            StackChild::Entry(StackEntry {
                component: Arc::new(transcript),
                basis: Some(StackBasis::Fixed(0.0)),
                grow: Some(1),
                shrink: None,
                min_size: None,
                max_size: None,
                visible: None,
            }),
            StackChild::Entry(StackEntry {
                component: Arc::new(Text::new(Some("dock"), Some(0), Some(0), None)),
                basis: Some(StackBasis::Auto),
                grow: None,
                shrink: None,
                min_size: None,
                max_size: None,
                visible: None,
            }),
        ]),
        None,
    ));

    render_layout_frame(root, 10, 3, noop_render());
    assert_eq!(render_count.load(Ordering::SeqCst), 1);
}

#[test]
fn paints_only_clipped_rows_from_very_large_scroll_content() {
    // PORT: JS uses a sparse array of length 1_000_000_000; Rust Vec<String> is dense,
    // so use a large dense Vec (100_000 rows) to verify clipped tail painting without OOM.
    let line_count = 100_000usize;
    let mut lines = vec![String::new(); line_count];
    lines[line_count - 4] = "before".to_string();
    lines[line_count - 3] = "visible 1".to_string();
    lines[line_count - 2] = "visible 2".to_string();
    lines[line_count - 1] = "visible 3".to_string();
    let shared_lines = Arc::new(lines);
    let transcript = ScrollView::new(
        CustomComponent::new(move |_| (*shared_lines).clone()),
        Some(ScrollViewOptions {
            follow: Some(ScrollViewFollow::End),
            ..Default::default()
        }),
    );

    let frame = render_layout_frame(Arc::new(transcript), 10, 3, noop_render());
    assert_eq!(visible_lines(&frame.lines), vec!["visible 1", "visible 2", "visible 3"]);
}

#[test]
fn shrinks_entries_to_their_minimum_sizes() {
    let frame = render_layout_frame(
        Arc::new(VStack::new(
            Some(vec![
                StackChild::Entry(StackEntry {
                    component: Arc::new(Text::new(Some("a1\na2\na3"), Some(0), Some(0), None)),
                    basis: None,
                    grow: None,
                    shrink: Some(1),
                    min_size: Some(1),
                    max_size: None,
                    visible: None,
                }),
                StackChild::Entry(StackEntry {
                    component: Arc::new(Text::new(Some("b1\nb2\nb3"), Some(0), Some(0), None)),
                    basis: None,
                    grow: None,
                    shrink: Some(0),
                    min_size: None,
                    max_size: None,
                    visible: None,
                }),
            ]),
            None,
        )),
        10,
        4,
        noop_render(),
    );

    let heights: Vec<i64> = frame
        .root
        .children()
        .into_iter()
        .map(|child| child.rect().height)
        .collect();
    assert_eq!(heights, vec![1, 3]);
    assert_eq!(visible_lines(&frame.lines), vec!["a1", "b1", "b2", "b3"]);
}

#[test]
fn includes_nested_minimum_sizes_in_intrinsic_stack_measurement() {
    let dock = Arc::new(VStack::new(
        Some(vec![
            StackChild::Component(Arc::new(Text::new(Some("top1\ntop2\ntop3"), Some(0), Some(0), None))),
            StackChild::Entry(StackEntry {
                component: Arc::new(Text::new(Some("selector"), Some(0), Some(0), None)),
                basis: None,
                grow: None,
                shrink: None,
                min_size: Some(3),
                max_size: None,
                visible: None,
            }),
            StackChild::Component(Arc::new(Text::new(Some("below"), Some(0), Some(0), None))),
            StackChild::Entry(StackEntry {
                component: Arc::new(Text::new(Some("footer"), Some(0), Some(0), None)),
                basis: None,
                grow: None,
                shrink: None,
                min_size: Some(1),
                max_size: None,
                visible: None,
            }),
        ]),
        None,
    ));

    let frame = render_layout_frame(
        Arc::new(VStack::new(
            Some(vec![
                StackChild::Entry(StackEntry {
                    component: Arc::new(Text::new(Some("body"), Some(0), Some(0), None)),
                    basis: Some(StackBasis::Fixed(0.0)),
                    grow: Some(1),
                    shrink: None,
                    min_size: Some(1),
                    max_size: None,
                    visible: None,
                }),
                StackChild::Entry(StackEntry {
                    component: dock,
                    basis: Some(StackBasis::Auto),
                    grow: None,
                    shrink: None,
                    min_size: Some(1),
                    max_size: None,
                    visible: None,
                }),
            ]),
            None,
        )),
        10,
        9,
        noop_render(),
    );

    assert_eq!(
        visible_lines(&frame.lines),
        vec!["body", "top1", "top2", "top3", "selector", "", "", "below", "footer"]
    );
}

#[test]
fn omits_gaps_around_invisible_entries() {
    let visible_false: StackVisibleFn = Arc::new(|_| false);
    let stack = VStack::new(
        Some(vec![
            StackChild::Component(Arc::new(Text::new(Some("one"), Some(0), Some(0), None))),
            StackChild::Entry(StackEntry {
                component: Arc::new(Text::new(Some("hidden"), Some(0), Some(0), None)),
                basis: None,
                grow: None,
                shrink: None,
                min_size: None,
                max_size: None,
                visible: Some(visible_false),
            }),
            StackChild::Component(Arc::new(Text::new(Some("two"), Some(0), Some(0), None))),
        ]),
        Some(StackOptions {
            gap: Some(1),
            align: None,
        }),
    );

    let trimmed: Vec<String> = stack
        .render(10)
        .into_iter()
        .map(|line| line.trim_end().to_string())
        .collect();
    assert_eq!(trimmed, vec!["one", "", "two"]);
}

#[test]
fn crops_kitty_images_at_a_scroll_views_lower_boundary() {
    let image_id = 124;
    let image_line = encode_kitty(
        "AAAA",
        Some(EncodeKittyOptions {
            columns: Some(2),
            rows: Some(3),
            image_id: Some(image_id),
            move_cursor: Some(false),
        }),
    );
    register_kitty_image_metadata(KittyImageMetadata {
        image_id,
        columns: 2,
        rows: 3,
        width_px: 100,
        height_px: 100,
    });
    let transcript = ScrollView::new(
        CustomComponent::new(move |_| {
            vec![
                "one".to_string(),
                "two".to_string(),
                image_line.clone(),
                String::new(),
                String::new(),
            ]
        }),
        None,
    );
    let frame = render_layout_frame(
        Arc::new(VStack::new(
            Some(vec![
                StackChild::Entry(StackEntry {
                    component: Arc::new(transcript),
                    basis: Some(StackBasis::Fixed(0.0)),
                    grow: Some(1),
                    shrink: None,
                    min_size: None,
                    max_size: None,
                    visible: None,
                }),
                StackChild::Component(Arc::new(Text::new(Some("dock"), Some(0), Some(0), None))),
            ]),
            None,
        )),
        20,
        4,
        noop_render(),
    );

    assert!(frame.lines.get(2).is_some_and(|line| line.contains("y=0,h=34,r=1")));
}

#[test]
fn composes_horizontal_children_at_allocated_widths() {
    let frame = render_layout_frame(
        Arc::new(HStack::new(
            Some(vec![
                StackChild::Entry(StackEntry {
                    component: Arc::new(Text::new(Some("left"), Some(0), Some(0), None)),
                    basis: Some(StackBasis::Fixed(6.0)),
                    grow: None,
                    shrink: Some(0),
                    min_size: None,
                    max_size: None,
                    visible: None,
                }),
                StackChild::Entry(StackEntry {
                    component: Arc::new(Text::new(Some("right"), Some(0), Some(0), None)),
                    basis: Some(StackBasis::Fixed(6.0)),
                    grow: None,
                    shrink: Some(0),
                    min_size: None,
                    max_size: None,
                    visible: None,
                }),
            ]),
            None,
        )),
        12,
        1,
        noop_render(),
    );
    assert_eq!(visible_lines(&frame.lines), vec!["left  right"]);
}

#[test]
fn does_not_paint_zero_width_horizontal_children() {
    let frame = render_layout_frame(
        Arc::new(HStack::new(
            Some(vec![
                StackChild::Entry(StackEntry {
                    component: Arc::new(Text::new(Some("hidden"), Some(0), Some(0), None)),
                    basis: Some(StackBasis::Fixed(0.0)),
                    grow: None,
                    shrink: Some(0),
                    min_size: None,
                    max_size: None,
                    visible: None,
                }),
                StackChild::Entry(StackEntry {
                    component: Arc::new(Text::new(Some("shown"), Some(0), Some(0), None)),
                    basis: Some(StackBasis::Fixed(0.0)),
                    grow: Some(1),
                    shrink: None,
                    min_size: None,
                    max_size: None,
                    visible: None,
                }),
            ]),
            None,
        )),
        5,
        1,
        noop_render(),
    );
    assert_eq!(visible_lines(&frame.lines), vec!["shown"]);
}

#[test]
fn tracks_follow_end_state_and_returns_unused_scroll_delta() {
    let scroll_view = ScrollView::new(
        Arc::new(Text::new(Some("1\n2\n3\n4\n5\n6"), Some(0), Some(0), None)),
        Some(ScrollViewOptions {
            follow: Some(ScrollViewFollow::End),
            primary: Some(true),
            ..Default::default()
        }),
    );
    render_layout_frame(Arc::new(scroll_view.clone()), 10, 3, noop_render());
    assert_eq!(scroll_view.scroll_top(), 3);
    assert!(scroll_view.is_following_end());

    assert_eq!(scroll_view.scroll_by(-2.0), 0);
    assert_eq!(scroll_view.scroll_top(), 1);
    assert!(!scroll_view.is_following_end());
    assert_eq!(scroll_view.scroll_by(-3.0), -2);
    assert_eq!(scroll_view.scroll_top(), 0);
    assert_eq!(scroll_view.scroll_by(10.0), 7);
    assert_eq!(scroll_view.scroll_top(), 3);
    assert!(scroll_view.is_following_end());
}

#[tokio::test]
async fn renders_a_proportional_glyph_scrollbar_with_an_expanded_active_thumb() {
    let source_lines = [
        "abcd界", "abcde2", "abcde3", "abcde4", "abcde5", "abcde6", "abcde7", "abcde8",
    ];
    let content_background = "\x1b[42m";
    let track_color = "\x1b[38;5;2m";
    let thumb_color = "\x1b[38;5;1m";
    let scrollbar_track_style: Arc<dyn Fn(&str) -> String + Send + Sync> =
        Arc::new(move |text| format!("{track_color}{text}\x1b[39m"));
    let scrollbar_thumb_style: Arc<dyn Fn(&str) -> String + Send + Sync> =
        Arc::new(move |text| format!("{thumb_color}{text}\x1b[39m"));
    let content = Arc::new(Text::new(
        Some(&source_lines.join("\n")),
        Some(0),
        Some(0),
        Some(Arc::new(move |text| format!("{content_background}{text}\x1b[49m"))),
    ));
    let scroll_view = ScrollView::new(
        Arc::clone(&content) as Arc<dyn Component>,
        Some(ScrollViewOptions {
            scrollbar: Some(ScrollViewScrollbar::Auto),
            scrollbar_track_style: Some(Arc::clone(&scrollbar_track_style)),
            scrollbar_thumb_style: Some(Arc::clone(&scrollbar_thumb_style)),
            scrollbar_hide_delay_ms: Some(10),
            ..Default::default()
        }),
    );
    let render = || render_layout_frame(Arc::new(scroll_view.clone()), 6, 4, noop_render()).lines;
    let visible =
        |lines: &[String]| -> Vec<String> { lines.iter().map(|line| strip_terminal_sequences(line)).collect() };

    let mut lines = render();
    assert_eq!(
        visible(&lines),
        source_lines[0..4].iter().map(|s| (*s).to_string()).collect::<Vec<_>>()
    );

    scroll_view.scroll_by(2.0);
    lines = render();
    assert_eq!(visible(&lines), vec!["abcde│", "abcde┃", "abcde┃", "abcde│"]);
    assert_eq!(
        lines.iter().map(|line| line.contains(track_color)).collect::<Vec<_>>(),
        vec![true, false, false, true]
    );
    assert_eq!(
        lines.iter().map(|line| line.contains(thumb_color)).collect::<Vec<_>>(),
        vec![false, true, true, false]
    );

    scroll_view.set_scrollbar_active(true);
    lines = render();
    assert_eq!(visible(&lines), vec!["abcde│", "abcde█", "abcde█", "abcde│"]);
    assert_eq!(
        lines.iter().map(|line| line.contains(thumb_color)).collect::<Vec<_>>(),
        vec![false, true, true, false]
    );
    assert!(lines[1].rfind(content_background) < lines[1].rfind(thumb_color));

    scroll_view.set_scrollbar_active(false);
    tokio::time::sleep(Duration::from_millis(30)).await;
    lines = render();
    assert_eq!(
        visible(&lines),
        source_lines[2..6].iter().map(|s| (*s).to_string()).collect::<Vec<_>>()
    );

    scroll_view.scroll_to_end();
    lines = render();
    assert_eq!(visible(&lines), vec!["abcde│", "abcde│", "abcde┃", "abcde┃"]);

    scroll_view.scroll_to_start();
    lines = render();
    assert_eq!(visible(&lines)[0], "abcd ┃");

    let followed_content = Arc::new(Text::new(Some(&source_lines.join("\n")), Some(0), Some(0), None));
    let followed = ScrollView::new(
        Arc::clone(&followed_content) as Arc<dyn Component>,
        Some(ScrollViewOptions {
            follow: Some(ScrollViewFollow::End),
            scrollbar: Some(ScrollViewScrollbar::Auto),
            scrollbar_track_style: Some(Arc::clone(&scrollbar_track_style)),
            scrollbar_thumb_style: Some(Arc::clone(&scrollbar_thumb_style)),
            ..Default::default()
        }),
    );
    render_layout_frame(Arc::new(followed.clone()), 6, 4, noop_render());
    assert_eq!(followed.scroll_top(), 4);
    followed_content.set_text(&format!("{}\nabcde9", source_lines.join("\n")));
    let growth_frame = render_layout_frame(Arc::new(followed.clone()), 6, 4, noop_render());
    assert_eq!(followed.scroll_top(), 5);
    assert!(
        growth_frame
            .lines
            .iter()
            .all(|line| !strip_terminal_sequences(line).contains(['│', '┃']))
    );

    let fitting_content = Arc::new(Text::new(Some("1\n2"), Some(0), Some(0), None));
    let automatic = ScrollView::new(
        Arc::clone(&fitting_content) as Arc<dyn Component>,
        Some(ScrollViewOptions {
            scrollbar: Some(ScrollViewScrollbar::Auto),
            scrollbar_thumb_style: Some(Arc::clone(&scrollbar_thumb_style)),
            ..Default::default()
        }),
    );
    render_layout_frame(Arc::new(automatic.clone()), 6, 4, noop_render());
    automatic.scroll_by(1.0);
    assert!(
        render_layout_frame(Arc::new(automatic), 6, 4, noop_render())
            .lines
            .iter()
            .all(|line| !strip_terminal_sequences(line).contains(['│', '┃']))
    );

    let always_fitting = ScrollView::new(
        fitting_content,
        Some(ScrollViewOptions {
            scrollbar: Some(ScrollViewScrollbar::Always),
            scrollbar_thumb_style: Some(Arc::clone(&scrollbar_thumb_style)),
            ..Default::default()
        }),
    );
    let always_fitting_frame = render_layout_frame(Arc::new(always_fitting), 6, 4, noop_render());
    assert_eq!(always_fitting_frame.root.children()[0].rect().width, 5);
    assert!(
        visible(&always_fitting_frame.lines)
            .iter()
            .all(|line| line.ends_with('┃'))
    );

    let always_overflowing = ScrollView::new(
        content,
        Some(ScrollViewOptions {
            scrollbar: Some(ScrollViewScrollbar::Always),
            scrollbar_track_style: Some(scrollbar_track_style),
            scrollbar_thumb_style: Some(Arc::clone(&scrollbar_thumb_style)),
            ..Default::default()
        }),
    );
    let always_overflowing_frame = render_layout_frame(Arc::new(always_overflowing), 6, 4, noop_render());
    assert_eq!(always_overflowing_frame.root.children()[0].rect().width, 5);
    let always_overflowing_visible = visible(&always_overflowing_frame.lines);
    assert_eq!(
        always_overflowing_visible
            .iter()
            .filter(|line| line.ends_with('┃'))
            .count(),
        2
    );
    assert_eq!(
        always_overflowing_visible
            .iter()
            .filter(|line| line.ends_with('│'))
            .count(),
        2
    );
    for line in &always_overflowing_frame.lines {
        let scrollbar_style_index = [line.rfind(track_color), line.rfind(thumb_color)]
            .into_iter()
            .flatten()
            .max()
            .expect("scrollbar color should appear");
        let reserved_column_reset_index = line[..scrollbar_style_index]
            .rfind("\x1b[0m\x1b]8;;\x07")
            .expect("segment reset should appear before scrollbar style");
        assert!(reserved_column_reset_index > line.rfind(content_background).unwrap());
    }

    let thumb_height_for = |content_height: usize| -> usize {
        let sized = ScrollView::new(
            Arc::new(Text::new(
                Some(&vec!["x"; content_height].join("\n")),
                Some(0),
                Some(0),
                None,
            )),
            Some(ScrollViewOptions {
                scrollbar: Some(ScrollViewScrollbar::Auto),
                scrollbar_thumb_style: Some(Arc::clone(&scrollbar_thumb_style)),
                ..Default::default()
            }),
        );
        render_layout_frame(Arc::new(sized.clone()), 6, 20, noop_render());
        sized.scroll_by(1.0);
        render_layout_frame(Arc::new(sized), 6, 20, noop_render())
            .lines
            .iter()
            .filter(|line| strip_terminal_sequences(line).ends_with('┃'))
            .count()
    };
    assert_eq!(thumb_height_for(21), 19);
    assert_eq!(thumb_height_for(40), 10);
    assert_eq!(thumb_height_for(100), 4);
    assert_eq!(thumb_height_for(400), 2);
}

#[test]
fn preserves_only_the_underlying_background_beneath_overlay_scrollbar_glyphs() {
    let background = "\x1b[42m";
    let border_foreground = "\x1b[31m";
    let content = CustomComponent::new(move |width| {
        vec![
            format!(
                "{background}{}{border_foreground}│\x1b[39m\x1b[49m",
                "x".repeat(width.saturating_sub(1))
            );
            8
        ]
    });
    let identity: Arc<dyn Fn(&str) -> String + Send + Sync> = Arc::new(|text| text.to_string());
    let scroll_view = ScrollView::new(
        content,
        Some(ScrollViewOptions {
            scrollbar: Some(ScrollViewScrollbar::Auto),
            scrollbar_track_style: Some(Arc::clone(&identity)),
            scrollbar_thumb_style: Some(identity),
            ..Default::default()
        }),
    );
    render_layout_frame(Arc::new(scroll_view.clone()), 6, 4, noop_render());
    scroll_view.scroll_by(1.0);
    let frame = render_layout_frame(Arc::new(scroll_view), 6, 4, noop_render());

    let stripped: Vec<String> = frame.lines.iter().map(|line| strip_terminal_sequences(line)).collect();
    assert_eq!(stripped, vec!["xxxxx│", "xxxxx┃", "xxxxx┃", "xxxxx│"]);
    for line in &frame.lines {
        assert!(line.contains(background));
        assert!(!line.contains(border_foreground));
        assert!(line.contains(&format!("\x1b[0m\x1b]8;;\x07{background}")));
    }
}

#[test]
fn updates_reserved_scrollbar_layout_at_runtime() {
    let scroll_view = ScrollView::new(
        Arc::new(Text::new(Some("123456"), Some(0), Some(0), None)),
        Some(ScrollViewOptions {
            scrollbar: Some(ScrollViewScrollbar::Always),
            ..Default::default()
        }),
    );
    let render = || {
        render_layout_frame(
            Arc::new(HStack::new(
                Some(vec![StackChild::Component(Arc::new(scroll_view.clone()))]),
                Some(StackOptions {
                    gap: None,
                    align: Some(StackAlign::Start),
                }),
            )),
            6,
            2,
            noop_render(),
        )
    };
    let always = render();
    assert_eq!(visible_lines(&always.lines), vec!["12345┃", "6    ┃"]);
    assert_eq!(always.root.children()[0].rect().width, 6);
    assert_eq!(always.root.children()[0].children()[0].rect().width, 5);

    scroll_view.set_scrollbar(ScrollViewScrollbar::Hidden);
    assert_eq!(render().root.children()[0].children()[0].rect().width, 6);
    assert!(!scroll_view.is_scrollbar_visible());
}

#[test]
fn measures_nested_scroll_content_from_constrained_child_geometry() {
    let inner = ScrollView::new(
        Arc::new(Text::new(Some("1\n2\n3\n4\n5\n6"), Some(0), Some(0), None)),
        None,
    );
    let outer = ScrollView::new(
        Arc::new(VStack::new(
            Some(vec![
                StackChild::Entry(StackEntry {
                    component: Arc::new(inner.clone()),
                    basis: Some(StackBasis::Fixed(2.0)),
                    grow: None,
                    shrink: None,
                    min_size: None,
                    max_size: None,
                    visible: None,
                }),
                StackChild::Component(Arc::new(Text::new(Some("tail"), Some(0), Some(0), None))),
            ]),
            None,
        )),
        None,
    );
    render_layout_frame(Arc::new(outer.clone()), 10, 2, noop_render());

    assert_eq!(inner.viewport_height(), 2);
    assert_eq!(outer.scroll_by(10.0), 9);
    assert_eq!(outer.scroll_top(), 1);
}

#[test]
fn rebuilds_geometry_after_content_changes() {
    let text = Arc::new(Text::new(Some("one"), Some(0), Some(0), None));
    let root = Arc::new(VStack::new(
        Some(vec![StackChild::Component(Arc::clone(&text) as Arc<dyn Component>)]),
        None,
    ));
    let first = render_layout_frame(Arc::clone(&root) as Arc<dyn Component>, 10, 4, noop_render());
    text.set_text("one\ntwo\nthree");
    let second = render_layout_frame(root, 10, 4, noop_render());

    assert_eq!(first.root.children()[0].lines().map(|l| l.len()), Some(1));
    assert_eq!(second.root.children()[0].lines().map(|l| l.len()), Some(3));
}

#[test]
fn stops_kitty_reverse_scan_at_non_empty_content_line_above_scroll_top() {
    let image_id = 4321;
    let image_line = encode_kitty(
        "AAAA",
        Some(EncodeKittyOptions {
            columns: Some(2),
            rows: Some(4),
            image_id: Some(image_id),
            move_cursor: Some(false),
        }),
    );
    register_kitty_image_metadata(KittyImageMetadata {
        image_id,
        columns: 2,
        rows: 4,
        width_px: 100,
        height_px: 100,
    });
    let scroll_view = Arc::new(ScrollView::new(
        CustomComponent::new(move |_| {
            vec![
                image_line.clone(),
                "non-empty separator".to_string(),
                String::new(),
                "visible top".to_string(),
                "visible bottom".to_string(),
            ]
        }),
        None,
    ));
    render_layout_frame(Arc::clone(&scroll_view) as Arc<dyn Component>, 24, 2, noop_render());
    scroll_view.scroll_to(3.0, None);
    let frame = render_layout_frame(scroll_view, 24, 2, noop_render());

    assert_eq!(visible_lines(&frame.lines), vec!["visible top", "visible bottom"]);
    assert!(
        !frame.lines[0].contains("\x1b_G"),
        "reverse scan should stop at non-empty line above scroll_top instead of cropping earlier image"
    );
}
