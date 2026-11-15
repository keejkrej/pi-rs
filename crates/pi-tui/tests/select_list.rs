//! Port of packages/tui/test/select-list.test.ts

use std::sync::{Arc, Mutex};

use pi_tui::{
    SelectItem, SelectList, SelectListLayoutOptions, SelectListTheme, SelectListTruncatePrimaryContext, visible_width,
};

fn test_theme() -> SelectListTheme {
    let identity: pi_tui::components::select_list::SelectListStyleFn = Arc::new(|text: &str| text.to_string());
    SelectListTheme {
        selected_prefix: Arc::clone(&identity),
        selected_text: Arc::clone(&identity),
        description: Arc::clone(&identity),
        scroll_info: Arc::clone(&identity),
        no_match: identity,
    }
}

fn visible_index_of(line: &str, text: &str) -> i64 {
    let index = line.find(text).expect("substring should exist in line");
    visible_width(&line[..index])
}

#[test]
fn normalizes_multiline_descriptions_to_single_line() {
    let items = vec![SelectItem {
        value: "test".to_string(),
        label: "test".to_string(),
        description: Some("Line one\nLine two\nLine three".to_string()),
    }];

    let list = SelectList::new(items, 5, test_theme(), None);
    let rendered = list.render(100);

    assert!(!rendered.is_empty());
    assert!(!rendered[0].contains('\n'));
    assert!(rendered[0].contains("Line one Line two Line three"));
}

#[test]
fn keeps_descriptions_aligned_when_the_primary_text_is_truncated() {
    let items = vec![
        SelectItem {
            value: "short".to_string(),
            label: "short".to_string(),
            description: Some("short description".to_string()),
        },
        SelectItem {
            value: "very-long-command-name-that-needs-truncation".to_string(),
            label: "very-long-command-name-that-needs-truncation".to_string(),
            description: Some("long description".to_string()),
        },
    ];

    let list = SelectList::new(items, 5, test_theme(), None);
    let rendered = list.render(80);

    assert_eq!(
        visible_index_of(&rendered[0], "short description"),
        visible_index_of(&rendered[1], "long description")
    );
}

#[test]
fn uses_the_configured_minimum_primary_column_width() {
    let items = vec![
        SelectItem {
            value: "a".to_string(),
            label: "a".to_string(),
            description: Some("first".to_string()),
        },
        SelectItem {
            value: "bb".to_string(),
            label: "bb".to_string(),
            description: Some("second".to_string()),
        },
    ];

    let list = SelectList::new(
        items,
        5,
        test_theme(),
        Some(SelectListLayoutOptions {
            min_primary_column_width: Some(12),
            max_primary_column_width: Some(20),
            truncate_primary: None,
        }),
    );
    let rendered = list.render(80);

    assert_eq!(
        rendered[0]
            .find("first")
            .map(|b| rendered[0][..b].encode_utf16().count()),
        Some(14)
    );
    assert_eq!(
        rendered[1]
            .find("second")
            .map(|b| rendered[1][..b].encode_utf16().count()),
        Some(14)
    );
}

#[test]
fn uses_the_configured_maximum_primary_column_width() {
    let items = vec![
        SelectItem {
            value: "very-long-command-name-that-needs-truncation".to_string(),
            label: "very-long-command-name-that-needs-truncation".to_string(),
            description: Some("first".to_string()),
        },
        SelectItem {
            value: "short".to_string(),
            label: "short".to_string(),
            description: Some("second".to_string()),
        },
    ];

    let list = SelectList::new(
        items,
        5,
        test_theme(),
        Some(SelectListLayoutOptions {
            min_primary_column_width: Some(12),
            max_primary_column_width: Some(20),
            truncate_primary: None,
        }),
    );
    let rendered = list.render(80);

    assert_eq!(visible_index_of(&rendered[0], "first"), 22);
    assert_eq!(visible_index_of(&rendered[1], "second"), 22);
}

#[test]
fn allows_overriding_primary_truncation_while_preserving_description_alignment() {
    let items = vec![
        SelectItem {
            value: "very-long-command-name-that-needs-truncation".to_string(),
            label: "very-long-command-name-that-needs-truncation".to_string(),
            description: Some("first".to_string()),
        },
        SelectItem {
            value: "short".to_string(),
            label: "short".to_string(),
            description: Some("second".to_string()),
        },
    ];

    let list = SelectList::new(
        items,
        5,
        test_theme(),
        Some(SelectListLayoutOptions {
            min_primary_column_width: Some(12),
            max_primary_column_width: Some(12),
            truncate_primary: Some(Arc::new(|ctx: &SelectListTruncatePrimaryContext| {
                let len = pi_js::str16::len(&ctx.text) as i64;
                if len <= ctx.max_width {
                    return ctx.text.clone();
                }
                let head = pi_js::str16::slice(&ctx.text, 0, Some(0_i64.max(ctx.max_width - 1)));
                format!("{head}…")
            })),
        }),
    );
    let rendered = list.render(80);

    assert!(rendered[0].contains('…'));
    assert_eq!(
        visible_index_of(&rendered[0], "first"),
        visible_index_of(&rendered[1], "second")
    );
}

#[test]
fn moves_by_one_visible_page_with_selection_page_keys() {
    let items: Vec<SelectItem> = (0..12)
        .map(|index| SelectItem {
            value: format!("item-{index}"),
            label: format!("Item {index}"),
            description: None,
        })
        .collect();
    let list = SelectList::new(items, 5, test_theme(), None);
    let selections: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let selections_clone = Arc::clone(&selections);
    list.set_on_selection_change(Some(Arc::new(move |item| {
        selections_clone.lock().unwrap().push(item.value.clone());
    })));

    for key in ["\x1b[6~", "\x1b[6~", "\x1b[6~", "\x1b[6~", "\x1b[5~"] {
        list.handle_input(key);
    }

    assert_eq!(
        *selections.lock().unwrap(),
        vec!["item-5", "item-10", "item-11", "item-6"]
    );

    list.set_filter("no match");
    list.handle_input("\x1b[6~");
    assert_eq!(
        *selections.lock().unwrap(),
        vec!["item-5", "item-10", "item-11", "item-6"]
    );
}
