//! Port of packages/telemetry/test/render.test.ts

use pi_telemetry::render::render_trace;
use pi_telemetry::{
    AttributeValue, SpanAttributes, SpanStatus, SpanStatusError, StoredSpan, StoredSpanEvent, StoredSpanLink,
};
use pretty_assertions::assert_eq;

fn span(id: &str) -> StoredSpan {
    StoredSpan {
        id: id.to_string(),
        trace_id: "trace".to_string(),
        parent_id: None,
        name: id.to_string(),
        started_ms: 0.0,
        ended_ms: 10.0,
        attributes: SpanAttributes::new(),
        events: Vec::new(),
        links: Vec::new(),
        status: SpanStatus::Ok,
    }
}

#[test]
fn renders_an_unordered_trace_with_attributes_durations_errors_events_and_links() {
    let mut root_attrs = SpanAttributes::new();
    root_attrs.insert("request".to_string(), AttributeValue::String("a b".to_string()));
    root_attrs.insert("attempt".to_string(), AttributeValue::Number(2.0));
    root_attrs.insert("cached".to_string(), AttributeValue::Boolean(false));
    root_attrs.insert(
        "tags".to_string(),
        AttributeValue::StringArray(vec!["a".to_string(), "b".to_string()]),
    );
    root_attrs.insert("sizes".to_string(), AttributeValue::NumberArray(vec![1.0, 2.0]));
    root_attrs.insert("flags".to_string(), AttributeValue::BooleanArray(vec![true, false]));

    let mut checkpoint_attrs = SpanAttributes::new();
    checkpoint_attrs.insert("phase".to_string(), AttributeValue::String("tools".to_string()));

    let mut link_attrs = SpanAttributes::new();
    link_attrs.insert("reason".to_string(), AttributeValue::String("steer".to_string()));

    let mut root = span("root");
    root.started_ms = 1000.0;
    root.ended_ms = 1020.0;
    root.attributes = root_attrs;
    root.events = vec![
        StoredSpanEvent {
            name: "checkpoint".to_string(),
            time_ms: 2.0,
            attributes: checkpoint_attrs,
        },
        StoredSpanEvent {
            name: "tick".to_string(),
            time_ms: 3.5,
            attributes: SpanAttributes::new(),
        },
    ];
    root.links = vec![
        StoredSpanLink {
            trace_id: "remote-trace".to_string(),
            span_id: "remote".to_string(),
            attributes: Some(link_attrs),
        },
        StoredSpanLink {
            trace_id: "other-trace".to_string(),
            span_id: "other".to_string(),
            attributes: None,
        },
    ];

    let mut first = span("first");
    first.parent_id = Some("root".to_string());
    first.started_ms = 1005.0;
    first.ended_ms = 1009.5;
    first.status = SpanStatus::Error {
        error: Some(SpanStatusError {
            name: "Error".to_string(),
            message: "failed call".to_string(),
        }),
    };

    let mut nested = span("nested");
    nested.parent_id = Some("first".to_string());
    nested.started_ms = 1006.0;
    nested.ended_ms = 1007.0;

    let mut second = span("second");
    second.parent_id = Some("root".to_string());
    second.started_ms = 1010.0;
    second.ended_ms = 1012.0;
    second.status = SpanStatus::Error { error: None };

    let spans = vec![second, nested, first, root];
    let before = spans.clone();
    assert_eq!(
        render_trace(&spans),
        [
            "root request=\"a b\" attempt=2 cached=false tags=[\"a\",\"b\"] sizes=[1,2] flags=[true,false] (20ms)",
            "  \u{00b7} checkpoint phase=\"tools\" (+2ms)",
            "  \u{00b7} tick (+3.5ms)",
            "  \u{2192} link trace remote-trace span remote reason=\"steer\"",
            "  \u{2192} link trace other-trace span other",
            "  first (4.5ms) error Error: failed call",
            "    nested (1ms)",
            "  second (2ms) error",
        ]
        .join("\n")
    );
    assert_eq!(spans, before);
}

#[test]
fn treats_missing_parents_as_roots_and_orders_roots_and_children_by_start_time() {
    let mut later = span("later");
    later.parent_id = Some("external".to_string());
    later.started_ms = 20.0;
    later.ended_ms = 30.0;

    let mut child = span("child");
    child.parent_id = Some("later".to_string());
    child.started_ms = 25.0;
    child.ended_ms = 30.0;

    let mut earlier = span("earlier");
    earlier.started_ms = 10.0;
    earlier.ended_ms = 15.0;
    earlier.links = vec![StoredSpanLink {
        trace_id: "trace".to_string(),
        span_id: "later".to_string(),
        attributes: None,
    }];

    assert_eq!(
        render_trace(&[later, child, earlier]),
        [
            "earlier (5ms)",
            "  \u{2192} link trace trace span later",
            "later (10ms)",
            "  child (5ms)"
        ]
        .join("\n")
    );
}

#[test]
fn prints_cross_trace_links_without_following_them_including_cycles() {
    let mut server = span("server:1");
    server.name = "server".to_string();
    server.trace_id = "server-trace".to_string();
    server.links = vec![StoredSpanLink {
        trace_id: "worker-trace".to_string(),
        span_id: "worker:1".to_string(),
        attributes: None,
    }];

    let mut worker = span("worker:1");
    worker.name = "worker".to_string();
    worker.trace_id = "worker-trace".to_string();
    worker.links = vec![StoredSpanLink {
        trace_id: "server-trace".to_string(),
        span_id: "server:1".to_string(),
        attributes: None,
    }];

    assert_eq!(
        render_trace(&[server, worker]),
        [
            "server (10ms)",
            "  \u{2192} link trace worker-trace span worker:1",
            "worker (10ms)",
            "  \u{2192} link trace server-trace span server:1",
        ]
        .join("\n")
    );
}

#[test]
fn deduplicates_spans_merged_from_stores_by_global_id() {
    let mut root = span("server:1");
    root.name = "root".to_string();

    let mut child = span("worker:1");
    child.name = "child".to_string();
    child.parent_id = Some("server:1".to_string());
    child.events = vec![StoredSpanEvent {
        name: "done".to_string(),
        time_ms: 3.0,
        attributes: SpanAttributes::new(),
    }];

    let mut grandchild = span("worker:2");
    grandchild.name = "grandchild".to_string();
    grandchild.parent_id = Some("worker:1".to_string());

    let spans = vec![child.clone(), root.clone(), grandchild, root, child];
    assert_eq!(
        render_trace(&spans),
        "root (10ms)\n  child (10ms)\n    \u{00b7} done (+3ms)\n    grandchild (10ms)"
    );
}

#[test]
fn preserves_input_order_for_equal_start_times_without_mutating_the_input() {
    let root = span("root");
    let mut z = span("z");
    z.parent_id = Some("root".to_string());
    let mut a = span("a");
    a.parent_id = Some("root".to_string());
    let other = span("other");
    assert_eq!(
        render_trace(&[root, z, a, other]),
        "root (10ms)\n  z (10ms)\n  a (10ms)\nother (10ms)"
    );
}

#[test]
fn escapes_string_attribute_values_without_hiding_their_contents() {
    let mut root = span("root");
    root.attributes.insert(
        "text".to_string(),
        AttributeValue::String("line one\n\"line two\"".to_string()),
    );
    assert_eq!(
        render_trace(&[root]),
        "root text=\"line one\\n\\\"line two\\\"\" (10ms)"
    );
}

#[test]
fn renders_deep_trees_without_recursive_calls() {
    let spans: Vec<StoredSpan> = (0..2000)
        .map(|index| {
            let mut s = span(&index.to_string());
            if index > 0 {
                s.parent_id = Some((index - 1).to_string());
            }
            s
        })
        .collect();
    let rendered = render_trace(&spans);
    assert_eq!(rendered.lines().count(), spans.len());
    assert!(rendered.ends_with(&format!("{}1999 (10ms)", "  ".repeat(1999))));
}

#[test]
fn renders_lengths_of_time_to_the_microsecond_as_the_adapters_record_them() {
    let mut root = span("root");
    root.started_ms = 1_793_789_452_816.123;
    root.ended_ms = 1_793_789_452_816.431;
    root.events = vec![StoredSpanEvent {
        name: "half".to_string(),
        time_ms: 0.15400000002,
        attributes: SpanAttributes::new(),
    }];
    assert_eq!(render_trace(&[root]), "root (0.308ms)\n  \u{00b7} half (+0.154ms)");
}

#[test]
fn renders_an_empty_input_as_an_empty_string() {
    assert_eq!(render_trace(&[]), "");
}

#[test]
fn escapes_control_characters_in_ids_names_attribute_keys_and_error_messages() {
    let hostile = "\u{001b}]0;PWNED\u{0007}\u{001b}[2J";
    let mut root = span("root");
    root.name = format!("name{hostile}");
    root.attributes
        .insert(format!("key{hostile}"), AttributeValue::String(hostile.to_string()));
    root.events = vec![StoredSpanEvent {
        name: format!("event{hostile}"),
        time_ms: 1.0,
        attributes: SpanAttributes::new(),
    }];
    root.links = vec![StoredSpanLink {
        trace_id: hostile.to_string(),
        span_id: format!("span\u{009b}{hostile}"),
        attributes: None,
    }];
    root.status = SpanStatus::Error {
        error: Some(SpanStatusError {
            name: format!("error{hostile}"),
            message: format!("message\u{007f}{hostile}"),
        }),
    };

    let rendered = render_trace(&[root]);
    for line in rendered.lines() {
        assert!(
            !line
                .chars()
                .any(|ch| matches!(ch as u32, 0x0000..=0x001f | 0x007f..=0x009f)),
            "line contained raw control character: {line:?}"
        );
    }
    assert!(rendered.contains("name\\u001b]0;PWNED\\u0007\\u001b[2J"));
    assert!(rendered.contains("\u{2192} link trace \\u001b]0;PWNED"));
    assert!(rendered.contains("span\\u009b"));
    assert!(rendered.contains("message\\u007f"));
    assert!(rendered.contains("key\\u001b]0;PWNED\\u0007\\u001b[2J=\"\\u001b]0;PWNED\\u0007\\u001b[2J\""));
}
