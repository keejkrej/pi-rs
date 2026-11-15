//! Port of packages/chord/test/delta-tracker/tracker.test.ts

use pi_chord::delta::tracker::{DraftInput, DraftOverlay, DraftValue, JsonValue, Tracker, track};
use pi_chord::delta::{
    AppendVerb, MoveVerb, Op, OpAppend, OpMove, OpReplace, OpSet, OpSplice, OpTruncate, ReplaceVerb, Seg, SetVerb,
    SpliceVerb, TruncateVerb, apply, decoder, encoder,
};
use serde_json::{Map, Value, json};

fn replay(base: &Value, ops: &[Op]) -> Value {
    apply(Some(base.clone()), ops).unwrap()
}

fn settle_overlay<F>(tracker: &Tracker<Value>, mutate: F) -> Value
where
    F: FnOnce(&DraftOverlay),
{
    let base_root = tracker.value();
    let base = base_root.clone();
    let change = tracker.begin_change();
    let overlay = change.overlay().unwrap();
    mutate(&overlay);
    let prepared = change.prepare().unwrap();
    let candidate = prepared.value().clone();
    assert_eq!(replay(&base, prepared.ops()), candidate);
    assert_eq!(base_root, base);
    tracker.adopt(&prepared).unwrap();
    assert_eq!(base_root, base);
    assert_eq!(tracker.value(), *prepared.value());
    assert_eq!(tracker.value(), candidate);
    candidate
}

// =========================================================================
// delta tracker transactional overlay lifecycle
// =========================================================================

#[test]
fn materializes_an_immutable_next_revision_and_adopts_it_by_pointer_swap() {
    let initial = json!({ "count": 1, "nested": { "text": "a" }, "values": [1] });
    let tracker = track(initial.clone());
    assert_eq!(tracker.value(), initial);

    let change = tracker.begin_change();
    let overlay = change.overlay().unwrap();
    overlay.set("count", 2_i64).unwrap();
    let nested = overlay.get("nested").unwrap().as_overlay().unwrap().clone();
    let next_text = format!("{}b", nested.get("text").unwrap().as_json().unwrap().as_str().unwrap());
    nested.set("text", next_text).unwrap();
    let values = overlay.get("values").unwrap().as_overlay().unwrap().clone();
    values.push(&[2_i64.into()]).unwrap();

    assert_eq!(initial, json!({ "count": 1, "nested": { "text": "a" }, "values": [1] }),);
    let prepared = change.prepare().unwrap();
    let next = prepared.value().clone();
    assert_eq!(prepared.base_revision(), 0);
    assert_eq!(prepared.base(), &initial);
    assert_ne!(next, initial);
    assert_eq!(
        next,
        json!({ "count": 2, "nested": { "text": "ab" }, "values": [1, 2] }),
    );
    assert_eq!(
        prepared.ops(),
        &[
            Op::Set(OpSet(SetVerb::S, vec![Seg::String("count".into())], json!(2))),
            Op::Append(OpAppend(
                AppendVerb::A,
                vec![Seg::String("nested".into()), Seg::String("text".into())],
                "b".into(),
            )),
            Op::Splice(OpSplice(
                SpliceVerb::P,
                vec![Seg::String("values".into())],
                1,
                0,
                vec![json!(2)],
            )),
        ]
    );
    assert_eq!(tracker.value(), initial);
    tracker.adopt(&prepared).unwrap();
    assert_eq!(initial, json!({ "count": 1, "nested": { "text": "a" }, "values": [1] }),);
    assert_eq!(tracker.value(), next);
    assert_eq!(prepared.value(), &next);
    assert_eq!(prepared.base(), &initial);
    assert_eq!(overlay.get("count").unwrap_err().name(), "TypeError");
}

#[test]
fn keeps_a_transaction_live_across_steps_and_seals_it_only_at_prepare() {
    // PORT: Synchronous step boundary stands in for JS `await Promise.resolve()` suspending an open draft.
    let tracker = track(json!({ "left": 0, "nested": { "right": 0 } }));
    let change = tracker.begin_change();
    let overlay = change.overlay().unwrap();
    overlay.set("left", 1_i64).unwrap();
    overlay
        .get("nested")
        .unwrap()
        .as_overlay()
        .unwrap()
        .set("right", 2_i64)
        .unwrap();
    let prepared = change.prepare().unwrap();
    assert_eq!(prepared.value(), &json!({ "left": 1, "nested": { "right": 2 } }));
    let err = overlay.set("left", 3_i64).unwrap_err();
    assert!(err.message().contains("settled"));
    tracker.adopt(&prepared).unwrap();
}

#[test]
fn allows_then_inspection_on_a_settled_draft_without_reporting_a_false_failure() {
    let initial: Value = pi_js::json::parse(r#"{"then":"document-value","value":1}"#).unwrap();
    let tracker = track(initial);
    let change = tracker.begin_change();
    let draft = change.overlay().unwrap();
    assert_eq!(draft.get("then").unwrap().as_json().unwrap(), &json!("document-value"));
    draft.set("value", 2_i64).unwrap();
    let prepared = change.prepare().unwrap();

    // Settled overlays expose `then` as `Undefined` (so JS/async inspection doesn't fail),
    // while other reads continue throwing settled errors.
    assert!(draft.get("then").unwrap().is_undefined());
    assert!(draft.get("value").unwrap_err().message().contains("settled"));

    tracker.adopt(&prepared).unwrap();
    assert_eq!(tracker.value()["then"], json!("document-value"));
    assert_eq!(tracker.value()["value"], json!(2));

    let rows: Vec<Value> = (0..4_100_i64).map(|value| json!({ "value": value })).collect();
    let large_tracker = track(json!({ "rows": rows }));
    let large_change = large_tracker.begin_change();
    let large_draft = large_change.overlay().unwrap();
    let large_rows = large_draft.get("rows").unwrap().as_overlay().unwrap().clone();
    for i in 0..4_100_i64 {
        let row = large_rows.get_index(i).unwrap().as_overlay().unwrap().clone();
        assert!(row.get("value").unwrap().as_json().unwrap().as_i64().unwrap() >= 0);
    }
    large_rows
        .get_index(0)
        .unwrap()
        .as_overlay()
        .unwrap()
        .set("value", -1_i64)
        .unwrap();
    let large_prepared = large_change.prepare().unwrap();
    assert!(large_draft.get("then").unwrap().is_undefined());
    assert!(large_draft.get("rows").unwrap_err().message().contains("settled"));
    large_tracker.adopt(&large_prepared).unwrap();
}

#[test]
fn aborts_idempotently_and_revokes_held_descendants() {
    let tracker = track(json!({ "child": { "value": 1 } }));
    let change = tracker.begin_change();
    let child = change
        .overlay()
        .unwrap()
        .get("child")
        .unwrap()
        .as_overlay()
        .unwrap()
        .clone();
    child.set("value", 2_i64).unwrap();
    change.abort();
    change.abort();
    assert_eq!(tracker.value()["child"]["value"], json!(1));
    assert_eq!(child.get("value").unwrap_err().name(), "TypeError");
    assert!(change.prepare().err().unwrap().message().contains("settled"));
}

#[test]
fn allows_competing_contexts_and_invalidates_loser_views() {
    let tracker = track(json!({ "value": 0 }));
    let first = tracker.begin_change();
    let second = tracker.begin_change();
    first.overlay().unwrap().set("value", 1_i64).unwrap();
    second.overlay().unwrap().set("value", 2_i64).unwrap();
    let first_prepared = first.prepare().unwrap();
    let second_prepared = second.prepare().unwrap();
    let held = second_prepared.value().clone();

    tracker.adopt(&first_prepared).unwrap();
    assert_eq!(tracker.value()["value"], json!(1));
    assert_eq!(held["value"], json!(2));
    assert!(tracker.adopt(&second_prepared).unwrap_err().message().contains("stale"));
    assert!(
        tracker
            .adopt(&first_prepared)
            .unwrap_err()
            .message()
            .contains("already been used")
    );
}

#[test]
fn invalidates_competing_open_overlays_without_changing_their_base_revision() {
    let initial = json!({ "rows": [{ "value": 0 }, { "value": 1 }] });
    let tracker = track(initial.clone());
    let stale_change = tracker.begin_change();
    let held = stale_change
        .overlay()
        .unwrap()
        .get("rows")
        .unwrap()
        .as_overlay()
        .unwrap()
        .get_index(1)
        .unwrap()
        .as_overlay()
        .unwrap()
        .clone();
    held.set("value", 2_i64).unwrap();

    let winner = tracker.begin_change();
    winner
        .overlay()
        .unwrap()
        .get("rows")
        .unwrap()
        .as_overlay()
        .unwrap()
        .get_index(0)
        .unwrap()
        .as_overlay()
        .unwrap()
        .set("value", 3_i64)
        .unwrap();
    let prepared = winner.prepare().unwrap();
    tracker.adopt(&prepared).unwrap();

    assert_eq!(initial, json!({ "rows": [{ "value": 0 }, { "value": 1 }] }));
    assert_eq!(tracker.value(), json!({ "rows": [{ "value": 3 }, { "value": 1 }] }));
    assert!(held.get("value").unwrap_err().message().contains("settled"));
    assert!(stale_change.prepare().err().unwrap().message().contains("settled"));
    stale_change.abort();
}

#[test]
fn adopts_object_edits_as_ordinary_fast_layout_immutable_revisions() {
    let initial = json!({ "first": 1, "second": 2 });
    let tracker = track(initial.clone());
    let change = tracker.begin_change();
    let overlay = change.overlay().unwrap();
    overlay.set("first", 3_i64).unwrap();
    overlay.delete("second").unwrap();
    overlay.set("third", 4_i64).unwrap();

    let desc = overlay.get_descriptor("third").unwrap().unwrap();
    assert!(desc.writable && desc.enumerable && desc.configurable);
    assert_eq!(desc.value.as_json().unwrap(), &json!(4));

    let prepared = change.prepare().unwrap();
    tracker.adopt(&prepared).unwrap();
    assert_ne!(tracker.value(), initial);
    assert_eq!(initial, json!({ "first": 1, "second": 2 }));
    assert_eq!(tracker.value(), json!({ "first": 3, "third": 4 }));
    let current = tracker.value();
    let keys: Vec<&str> = current.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(keys, vec!["first", "third"]);
}

#[test]
fn supports_adopt_then_publish_ordering_and_retained_publication_reads() {
    let tracker = track(json!({ "value": 0, "nested": { "count": 0 } }));
    let change = tracker.begin_change();
    let overlay = change.overlay().unwrap();
    overlay.set("value", 1_i64).unwrap();
    overlay
        .get("nested")
        .unwrap()
        .as_overlay()
        .unwrap()
        .set("count", 1_i64)
        .unwrap();
    let prepared = change.prepare().unwrap();
    let operations = prepared.ops().to_vec();
    tracker.adopt(&prepared).unwrap();

    assert_eq!(prepared.ops(), operations.as_slice());
    let published = (!prepared.ops().is_empty()).then(|| prepared.value().clone());
    assert_eq!(operations.len(), 2);
    assert_eq!(published.as_ref(), Some(&tracker.value()));
    assert_eq!(
        published.as_ref().unwrap(),
        &json!({ "value": 1, "nested": { "count": 1 } }),
    );

    let next = tracker.begin_change();
    next.overlay().unwrap().set("value", 2_i64).unwrap();
    let next_prepared = next.prepare().unwrap();
    tracker.adopt(&next_prepared).unwrap();

    assert_ne!(published.as_ref().unwrap(), &tracker.value());
    assert_eq!(prepared.value(), &json!({ "value": 1, "nested": { "count": 1 } }));
    assert_eq!(tracker.value(), json!({ "value": 2, "nested": { "count": 1 } }));
}

#[test]
fn keeps_replacement_base_and_value_structurally_compatible_and_readable_after_adoption() {
    let original = json!({ "value": 0 });
    let replacement = json!({ "value": 1 });
    let tracker = track(original.clone());
    let prepared = tracker.prepare_replace(replacement.clone()).unwrap();
    assert_eq!(prepared.base(), &original);
    assert_eq!(prepared.value(), &replacement);
    tracker.adopt(&prepared).unwrap();
    assert_eq!(prepared.base(), &original);
    assert_eq!(prepared.value(), &replacement);
    assert_eq!(prepared.value(), &tracker.value());
}

#[test]
fn keeps_aborted_and_stale_materialized_candidates_readable() {
    let tracker = track(json!({ "value": 0 }));
    let aborted_change = tracker.begin_change();
    aborted_change.overlay().unwrap().set("value", 1_i64).unwrap();
    let aborted = aborted_change.prepare().unwrap();
    let aborted_view = aborted.value().clone();
    aborted.abort();
    assert_eq!(aborted_view["value"], json!(1));
    assert_eq!(aborted.value()["value"], json!(1));

    let loser_change = tracker.begin_change();
    loser_change.overlay().unwrap().set("value", 2_i64).unwrap();
    let loser = loser_change.prepare().unwrap();
    let loser_view = loser.value().clone();
    let winner = tracker.prepare_replace(json!({ "value": 3 })).unwrap();
    tracker.adopt(&winner).unwrap();
    assert_eq!(loser_view["value"], json!(2));
    assert_eq!(loser.value()["value"], json!(2));
}

#[test]
fn lets_a_settled_change_abort_its_prepared_result_without_retaining_its_context() {
    let tracker = track(json!({ "value": 0, "nested": { "value": 1 } }));
    let change = tracker.begin_change();
    change.overlay().unwrap().set("value", 1_i64).unwrap();
    let prepared = change.prepare().unwrap();
    let operations = prepared.ops().to_vec();
    change.abort();
    change.abort();
    assert_eq!(prepared.ops(), operations.as_slice());
    assert_eq!(prepared.value(), &json!({ "value": 1, "nested": { "value": 1 } }),);
    assert!(tracker.adopt(&prepared).unwrap_err().message().contains("aborted"));
}

#[test]
fn rejects_foreign_and_aborted_prepared_values() {
    let first = track(json!({ "value": 0 }));
    let second = track(json!({ "value": 0 }));
    let prepared = first.prepare_replace(json!({ "value": 1 })).unwrap();
    assert!(
        second
            .adopt(&prepared)
            .unwrap_err()
            .message()
            .contains("different tracker")
    );
    prepared.abort();
    prepared.abort();
    assert!(first.adopt(&prepared).unwrap_err().message().contains("aborted"));
}

#[test]
fn reads_deleted_own_properties_as_absent() {
    // PORT: Rust `JsonValue::Object` has no JS `Object.prototype` inheritance chain.
    let tracker = track(json!({ "value": 1 }));
    let change = tracker.begin_change();
    let overlay = change.overlay().unwrap();
    assert!(overlay.delete("value").unwrap());
    overlay.set("trackerInherited", 1_i64).unwrap();
    assert!(overlay.delete("trackerInherited").unwrap());
    assert!(overlay.get("value").unwrap().is_undefined());
    assert!(overlay.get("trackerInherited").unwrap().is_undefined());
    assert!(overlay.own_keys().unwrap().is_empty());

    let prepared = change.prepare().unwrap();
    assert!(prepared.value().get("value").is_none());
    tracker.adopt(&prepared).unwrap();
    assert!(tracker.value().as_object().unwrap().is_empty());
}

#[test]
fn detaches_old_handles_for_deeply_equal_object_and_array_assignments_before_normalizing() {
    let initial = json!({
        "object": { "nested": { "value": 1 } },
        "array": [{ "value": 1 }],
        "rows": [{ "value": 1 }],
    });
    let tracker = track(initial.clone());
    let change = tracker.begin_change();
    let overlay = change.overlay().unwrap();

    let old_object = overlay.get("object").unwrap().as_overlay().unwrap().clone();
    let old_array = overlay.get("array").unwrap().as_overlay().unwrap().clone();
    let rows = overlay.get("rows").unwrap().as_overlay().unwrap().clone();
    let old_row = rows.get_index(0).unwrap().as_overlay().unwrap().clone();

    overlay.set("object", json!({ "nested": { "value": 1 } })).unwrap();
    overlay.set("array", json!([{ "value": 1 }])).unwrap();
    rows.set_index(0, json!({ "value": 1 })).unwrap();

    assert_ne!(overlay.get("object").unwrap().as_overlay().unwrap(), &old_object);
    assert_ne!(overlay.get("array").unwrap().as_overlay().unwrap(), &old_array);
    assert_ne!(rows.get_index(0).unwrap().as_overlay().unwrap(), &old_row);

    old_object
        .get("nested")
        .unwrap()
        .as_overlay()
        .unwrap()
        .set("value", 9_i64)
        .unwrap();
    old_array
        .get_index(0)
        .unwrap()
        .as_overlay()
        .unwrap()
        .set("value", 9_i64)
        .unwrap();
    old_row.set("value", 9_i64).unwrap();

    assert_eq!(
        overlay.to_json().unwrap(),
        json!({
            "object": { "nested": { "value": 1 } },
            "array": [{ "value": 1 }],
            "rows": [{ "value": 1 }],
        }),
    );

    let prepared = change.prepare().unwrap();
    assert!(prepared.ops().is_empty());
    tracker.adopt(&prepared).unwrap();
    assert_eq!(tracker.value(), initial);
}

#[test]
fn normalizes_a_deeply_equal_replacement_without_changing_committed_identity() {
    let initial = json!({ "nested": { "value": 1 }, "rows": [1, 2, 3] });
    let tracker = track(initial.clone());
    let prepared = tracker
        .prepare_replace(json!({ "nested": { "value": 1 }, "rows": [1, 2, 3] }))
        .unwrap();
    assert!(prepared.ops().is_empty());
    tracker.adopt(&prepared).unwrap();
    assert_eq!(tracker.value(), initial);
}

#[test]
fn takes_o1_immutable_ownership_for_replacement_and_its_operation_payload() {
    let tracker = track(json!({ "value": 0, "rows": [] }));
    let replacement = json!({ "value": 1, "rows": [{ "value": 2 }] });
    let prepared = tracker.prepare_replace(replacement.clone()).unwrap();
    assert_eq!(prepared.base(), &tracker.value());
    assert_eq!(prepared.value(), &replacement);
    assert_eq!(
        prepared.ops(),
        &[Op::Replace(OpReplace(ReplaceVerb::R, replacement.clone()))],
    );
    tracker.adopt(&prepared).unwrap();
    assert_eq!(tracker.value(), replacement);
}

// =========================================================================
// delta tracker policy view
// =========================================================================

#[test]
fn supports_native_reads_descriptors_keys_iteration_map_and_json() {
    let tracker = track(json!({ "values": [1, 2, 3], "object": { "a": 1 } }));
    let change = tracker.begin_change();
    let overlay = change.overlay().unwrap();
    let values = overlay.get("values").unwrap().as_overlay().unwrap().clone();
    values.splice(Some(1), Some(1), &[4_i64.into(), 5_i64.into()]).unwrap();
    let object = overlay.get("object").unwrap().as_overlay().unwrap().clone();
    object.set("b", 2_i64).unwrap();

    assert!(values.is_array().unwrap());
    assert_eq!(values.len().unwrap(), 4);
    let collected: Vec<i64> = (0..values.len().unwrap())
        .map(|i| {
            values
                .get_index(i64::try_from(i).unwrap())
                .unwrap()
                .as_json()
                .unwrap()
                .as_i64()
                .unwrap()
        })
        .collect();
    assert_eq!(collected, vec![1, 4, 5, 3]);
    assert_eq!(collected.iter().map(|v| v * 2).collect::<Vec<_>>(), vec![2, 8, 10, 6],);
    let own_keys = values.own_keys().unwrap();
    assert_eq!(own_keys, vec!["0", "1", "2", "3", "length"]);
    let enumerable_keys: Vec<String> = own_keys
        .into_iter()
        .filter(|k| values.get_descriptor(k).unwrap().is_some_and(|d| d.enumerable))
        .collect();
    assert_eq!(enumerable_keys, vec!["0", "1", "2", "3"]);
    assert_eq!(
        values.get_descriptor("2").unwrap().unwrap().value.as_json().unwrap(),
        &json!(5),
    );
    assert_eq!(object.own_keys().unwrap(), vec!["a", "b"]);
    assert_eq!(
        overlay.to_json().unwrap(),
        json!({ "values": [1, 4, 5, 3], "object": { "a": 1, "b": 2 } }),
    );
    change.abort();
}

#[test]
fn keeps_document_keys_distinct_from_object_proxy_target_fields() {
    let tracker = track(json!({
        "object": {
            "context": 1,
            "base": 2,
            "parent": 3,
            "dirty": 4,
            "target": 5,
            "proxy": 6,
        },
    }));
    let change = tracker.begin_change();
    let object = change
        .overlay()
        .unwrap()
        .get("object")
        .unwrap()
        .as_overlay()
        .unwrap()
        .clone();
    object.set("context", 7_i64).unwrap();
    object.set("proxy", 8_i64).unwrap();
    assert_eq!(
        object.own_keys().unwrap(),
        vec!["context", "base", "parent", "dirty", "target", "proxy"],
    );
    let prepared = change.prepare().unwrap();
    assert_eq!(
        prepared.value()["object"],
        json!({ "context": 7, "base": 2, "parent": 3, "dirty": 4, "target": 5, "proxy": 8 }),
    );
    tracker.adopt(&prepared).unwrap();
}

#[test]
fn keeps_string_truncation_and_append_operation_forms() {
    // PORT: JS `valueOf()` side-effect traps cannot occur on strongly-typed Rust indices;
    // verify canonical truncate + append ops and codec wire roundtrip directly.
    let tracker = track(json!({ "text": "abcdefgh", "values": [1, 2, 3], "marker": 0 }));
    let base = tracker.value();
    let change = tracker.begin_change();
    let overlay = change.overlay().unwrap();
    let prev = overlay
        .get("text")
        .unwrap()
        .as_json()
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned();
    overlay.set("text", format!("{}xyz", &prev[3..])).unwrap();
    overlay
        .get("values")
        .unwrap()
        .as_overlay()
        .unwrap()
        .splice(Some(1), Some(1), &[4_i64.into()])
        .unwrap();
    overlay.set("marker", 1_i64).unwrap();

    let prepared = change.prepare().unwrap();
    assert_eq!(
        prepared.value(),
        &json!({ "text": "defghxyz", "values": [1, 4, 3], "marker": 1 }),
    );
    assert!(prepared.ops().contains(&Op::Truncate(OpTruncate(
        TruncateVerb::T,
        vec![Seg::String("text".into())],
        3,
    ))));
    assert!(prepared.ops().contains(&Op::Append(OpAppend(
        AppendVerb::A,
        vec![Seg::String("text".into())],
        "xyz".into(),
    ))));
    let encoded = encoder().encode(prepared.ops());
    let decoded = decoder().decode(&encoded).unwrap();
    assert_eq!(replay(&base, &decoded), *prepared.value());
    tracker.adopt(&prepared).unwrap();
}

#[test]
fn emits_deletion_and_supports_root_arrays() {
    let object_tracker = track(json!({ "keep": 1, "remove": 2 }));
    settle_overlay(&object_tracker, |overlay| {
        assert!(overlay.delete("remove").unwrap());
    });
    assert_eq!(object_tracker.value(), json!({ "keep": 1 }));

    let array_tracker = track(json!([1, 2, 3]));
    settle_overlay(&array_tracker, |overlay| {
        overlay.reverse().unwrap();
        overlay.push(&[4_i64.into()]).unwrap();
    });
    assert_eq!(array_tracker.value(), json!([3, 2, 1, 4]));
}

#[test]
fn preserves_native_object_key_order_after_delete_and_readd() {
    let tracker = track(json!({ "first": 1, "second": 2 }));
    let change = tracker.begin_change();
    let overlay = change.overlay().unwrap();
    assert!(overlay.delete("first").unwrap());
    overlay.set("first", 1_i64).unwrap();
    assert_eq!(overlay.own_keys().unwrap(), vec!["second", "first"]);
    let prepared = change.prepare().unwrap();
    tracker.adopt(&prepared).unwrap();
    let current = tracker.value();
    let keys: Vec<&str> = current.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(keys, vec!["second", "first"]);
}

#[test]
fn orders_new_integer_like_object_keys_before_strings_for_recipe_visible_reads() {
    let tracker = track(json!({ "object": { "label": "x" }, "first": "" }));
    let base = tracker.value();
    let change = tracker.begin_change();
    let overlay = change.overlay().unwrap();
    let object = overlay.get("object").unwrap().as_overlay().unwrap().clone();
    object.set("2", "two").unwrap();
    object.set("1", "one").unwrap();
    assert_eq!(object.own_keys().unwrap(), vec!["1", "2", "label"]);
    let first_key = object.own_keys().unwrap().into_iter().next().unwrap();
    overlay.set("first", first_key).unwrap();

    let prepared = change.prepare().unwrap();
    assert_eq!(prepared.value()["first"], json!("1"));
    let prepared_keys: Vec<&str> = prepared.value()["object"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(prepared_keys, vec!["1", "2", "label"]);
    assert_eq!(replay(&base, prepared.ops()), *prepared.value());
    tracker.adopt(&prepared).unwrap();
    assert_eq!(tracker.value()["first"], json!("1"));
    let current = tracker.value();
    let adopted_keys: Vec<&str> = current["object"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(adopted_keys, vec!["1", "2", "label"]);
}

#[test]
fn normalizes_deeply_equal_container_assignments_before_direct_candidate_materialization() {
    let initial = json!({ "child": { "a": 1, "b": 2 }, "count": 0 });
    let tracker = track(initial.clone());
    let change = tracker.begin_change();
    let overlay = change.overlay().unwrap();
    let mut reversed_child = Map::new();
    reversed_child.insert("b".into(), json!(2));
    reversed_child.insert("a".into(), json!(1));
    overlay.set("child", Value::Object(reversed_child)).unwrap();
    overlay.set("count", 1_i64).unwrap();

    let prepared = change.prepare().unwrap();
    let replayed = replay(&initial, prepared.ops());
    assert_eq!(
        prepared.ops(),
        &[Op::Set(OpSet(SetVerb::S, vec![Seg::String("count".into())], json!(1)))],
    );
    let prepared_keys: Vec<&str> = prepared.value()["child"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    let replayed_keys: Vec<&str> = replayed["child"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(prepared_keys, vec!["a", "b"]);
    assert_eq!(replayed_keys, vec!["a", "b"]);
    assert_eq!(*prepared.value(), replayed);
    tracker.adopt(&prepared).unwrap();
}

#[test]
fn keeps_native_integer_and_string_ordering_across_deletion_and_readdition() {
    let tracker = track(json!({
        "object": { "1": "one", "2": "two", "first": "a", "second": "b" },
    }));
    let change = tracker.begin_change();
    let object = change
        .overlay()
        .unwrap()
        .get("object")
        .unwrap()
        .as_overlay()
        .unwrap()
        .clone();
    object.delete("2").unwrap();
    object.set("2", "two").unwrap();
    object.delete("first").unwrap();
    object.set("first", "a").unwrap();
    object.set("3", "three").unwrap();
    object.set("0", "zero").unwrap();

    let expected_order = vec!["0", "1", "2", "3", "second", "first"];
    assert_eq!(object.own_keys().unwrap(), expected_order);

    let prepared = change.prepare().unwrap();
    let prepared_keys: Vec<&str> = prepared.value()["object"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(prepared_keys, expected_order);
    tracker.adopt(&prepared).unwrap();
    let current = tracker.value();
    let adopted_keys: Vec<&str> = current["object"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(adopted_keys, expected_order);
}

#[test]
fn orders_integer_like_keys_on_plain_map_objects() {
    // PORT: Rust `serde_json::Map` has no prototype distinction (`Object.create(null)` vs `{}`).
    let tracker = track(json!({ "object": { "label": "x" } }));
    let base = tracker.value();
    let change = tracker.begin_change();
    let object = change
        .overlay()
        .unwrap()
        .get("object")
        .unwrap()
        .as_overlay()
        .unwrap()
        .clone();
    object.set("2", "two").unwrap();
    object.set("1", "one").unwrap();
    assert_eq!(object.own_keys().unwrap(), vec!["1", "2", "label"]);
    let prepared = change.prepare().unwrap();
    let keys: Vec<&str> = prepared.value()["object"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys, vec!["1", "2", "label"]);
    assert_eq!(replay(&base, prepared.ops()), *prepared.value());
    tracker.adopt(&prepared).unwrap();
}

#[test]
fn does_not_report_inherited_methods_as_own_array_properties() {
    // PORT: `DraftOverlay` methods are inherent Rust methods, not JS own properties or prototype properties.
    let tracker = track(json!({ "values": [1] }));
    let change = tracker.begin_change();
    let values = change
        .overlay()
        .unwrap()
        .get("values")
        .unwrap()
        .as_overlay()
        .unwrap()
        .clone();
    assert!(values.get_descriptor("map").unwrap().is_none());
    assert!(!values.has("map").unwrap());
    assert_eq!(values.own_keys().unwrap(), vec!["0", "length"]);
    assert!(!values.get_descriptor("length").unwrap().unwrap().enumerable);
    change.abort();
}

#[test]
fn matches_native_splice_fill_and_copy_within_behaviors() {
    // PORT: Rust `i64` arguments have no JS `valueOf()` coercion reentrancy; verify negative/out-of-range ES indexing.
    let tracker = track(json!({ "values": [1, 2, 3] }));
    settle_overlay(&tracker, |overlay| {
        let values = overlay.get("values").unwrap().as_overlay().unwrap().clone();
        values.splice(Some(1), Some(1), &[9_i64.into()]).unwrap();
        values.fill(7_i64, Some(1), Some(3)).unwrap();
        values.copy_within(1, 0, Some(2)).unwrap();
    });
    assert_eq!(tracker.value()["values"], json!([1, 1, 7]));
}

#[test]
fn keeps_repeated_identical_writes_instead_of_reverting_their_pending_override() {
    let tracker = track(json!({ "value": 0, "values": [0] }));
    settle_overlay(&tracker, |overlay| {
        overlay.set("value", 1_i64).unwrap();
        overlay.set("value", 1_i64).unwrap();
        let values = overlay.get("values").unwrap().as_overlay().unwrap().clone();
        values.set_index(0, JsonValue::Null).unwrap();
        values.set_index(0, JsonValue::Null).unwrap();
        values.push(&[2_i64.into()]).unwrap();
        values.set_index(1, JsonValue::Null).unwrap();
        values.set_index(1, JsonValue::Null).unwrap();
    });
    assert_eq!(tracker.value(), json!({ "value": 1, "values": [null, null] }));
}

#[test]
fn rejects_array_index_gaps_without_mutation_while_allowing_replacement_and_append() {
    let tracker = track(json!({ "values": [1, 2] }));
    let change = tracker.begin_change();
    let overlay = change.overlay().unwrap();
    let values = overlay.get("values").unwrap().as_overlay().unwrap().clone();
    assert!(values.set_index(3, 4_i64).unwrap_err().message().contains("holes"));
    assert_eq!(values.to_json().unwrap(), json!([1, 2]));
    assert_eq!(tracker.value()["values"], json!([1, 2]));

    values.set_index(1, 9_i64).unwrap();
    values.set_index(2, 3_i64).unwrap();
    let prepared = change.prepare().unwrap();
    assert_eq!(prepared.value()["values"], json!([1, 9, 3]));
    assert_eq!(replay(&tracker.value(), prepared.ops()), *prepared.value());
    tracker.adopt(&prepared).unwrap();
    assert_eq!(tracker.value()["values"], json!([1, 9, 3]));
}

#[test]
fn supports_optional_deletion_explicit_null_length_growth_shrinking_and_default_sort() {
    let tracker = track(json!({ "optional": "remove", "values": [3, 1, 2] }));
    settle_overlay(&tracker, |overlay| {
        overlay.set("optional", DraftInput::Undefined).unwrap();
        let values = overlay.get("values").unwrap().as_overlay().unwrap().clone();
        values.set_len(5).unwrap();
        assert_eq!(values.to_json().unwrap(), json!([3, 1, 2, null, null]));
        values.set_len(4).unwrap();
        values.sort().unwrap();
        assert!(values.delete_index(0).unwrap_err().message().contains("holes"));
    });
    assert_eq!(tracker.value(), json!({ "values": [1, 2, 3, null] }));
}

#[test]
fn enumerates_wide_objects_without_duplicate_keys() {
    let tracker = track(json!({ "values": {} }));
    let change = tracker.begin_change();
    let values = change
        .overlay()
        .unwrap()
        .get("values")
        .unwrap()
        .as_overlay()
        .unwrap()
        .clone();
    for index in 0..20_000_i64 {
        values.set(&format!("field{index}"), index).unwrap();
    }
    let keys = values.own_keys().unwrap();
    assert_eq!(keys.len(), 20_000);
    assert_eq!(keys[0], "field0");
    assert_eq!(keys.last().unwrap(), "field19999");
    change.abort();
}

#[test]
fn does_not_dirty_read_only_traversals() {
    let tracker = track(json!({ "nested": { "rows": [{ "value": 1 }] } }));
    let change = tracker.begin_change();
    let val = change
        .overlay()
        .unwrap()
        .get("nested")
        .unwrap()
        .as_overlay()
        .unwrap()
        .get("rows")
        .unwrap()
        .as_overlay()
        .unwrap()
        .get_index(0)
        .unwrap()
        .as_overlay()
        .unwrap()
        .get("value")
        .unwrap()
        .as_json()
        .unwrap()
        .clone();
    assert_eq!(val, json!(1));
    let prepared = change.prepare().unwrap();
    assert!(prepared.ops().is_empty());
    tracker.adopt(&prepared).unwrap();
}

#[test]
fn uses_one_proxy_identity_per_accessed_container() {
    let tracker = track(json!({ "nested": { "value": 1 }, "rows": [{ "value": 2 }] }));
    let change = tracker.begin_change();
    let overlay = change.overlay().unwrap();
    assert_eq!(
        overlay.get("nested").unwrap().as_overlay().unwrap(),
        overlay.get("nested").unwrap().as_overlay().unwrap(),
    );
    let rows1 = overlay.get("rows").unwrap().as_overlay().unwrap().clone();
    let rows2 = overlay.get("rows").unwrap().as_overlay().unwrap().clone();
    assert_eq!(rows1, rows2);
    assert_eq!(
        rows1.get_index(0).unwrap().as_overlay().unwrap(),
        rows1.get_index(0).unwrap().as_overlay().unwrap(),
    );
    change.abort();
}

// =========================================================================
// delta tracker by-value placements
// =========================================================================

#[test]
fn clones_property_index_push_unshift_splice_fill_and_copy_within_placements() {
    let tracker = track(json!({
        "property": null,
        "values": [{ "value": 0 }, { "value": 1 }, { "value": 2 }],
    }));
    let external = json!({ "value": 5 });
    let change = tracker.begin_change();
    let overlay = change.overlay().unwrap();
    overlay.set("property", external.clone()).unwrap();
    let values = overlay.get("values").unwrap().as_overlay().unwrap().clone();
    values.set_index(0, external.clone()).unwrap();
    values.push(&[external.clone().into()]).unwrap();
    values.unshift(&[external.clone().into()]).unwrap();
    values.splice(Some(2), Some(0), &[external.into()]).unwrap();
    let first = values.get_index(0).unwrap().as_overlay().unwrap().clone();
    values.fill(first, Some(1), Some(3)).unwrap();
    values.copy_within(3, 0, Some(2)).unwrap();

    let prepared = change.prepare().unwrap();
    assert_eq!(prepared.value()["property"], json!({ "value": 5 }));
    assert_eq!(
        &prepared.value()["values"].as_array().unwrap()[..5],
        &[
            json!({ "value": 5 }),
            json!({ "value": 5 }),
            json!({ "value": 5 }),
            json!({ "value": 5 }),
            json!({ "value": 5 }),
        ],
    );
    tracker.adopt(&prepared).unwrap();
}

#[test]
fn rejects_non_strict_json_placements_before_changing_the_draft() {
    let initial = json!({
        "optional": "remove",
        "payload": null,
        "number": 0,
        "values": [1, 2],
    });
    let tracker = track(initial.clone());
    let change = tracker.begin_change();
    let overlay = change.overlay().unwrap();
    let values = overlay.get("values").unwrap().as_overlay().unwrap().clone();

    assert!(
        values
            .set_index(0, DraftInput::Undefined)
            .unwrap_err()
            .message()
            .contains("strict JSON")
    );
    assert!(
        values
            .push(&[json!({ "staged": true }).into(), DraftInput::Undefined])
            .unwrap_err()
            .message()
            .contains("strict JSON")
    );
    assert!(
        values
            .unshift(&[DraftInput::Undefined])
            .unwrap_err()
            .message()
            .contains("strict JSON")
    );
    assert!(
        values
            .splice(Some(1), Some(0), &[DraftInput::Undefined])
            .unwrap_err()
            .message()
            .contains("strict JSON")
    );
    assert!(
        values
            .fill(DraftInput::Undefined, None, None)
            .unwrap_err()
            .message()
            .contains("strict JSON")
    );
    assert!(
        overlay
            .set("number", f64::NAN)
            .unwrap_err()
            .message()
            .contains("strict JSON")
    );
    assert!(
        overlay
            .set("number", f64::INFINITY)
            .unwrap_err()
            .message()
            .contains("strict JSON")
    );
    // PORT: JS-specific non-JSON object traps (Date, function, Symbol, BigInt, sparse array, cycles, getters)
    // cannot be constructed in statically typed `serde_json::Value`.

    assert_eq!(overlay.to_json().unwrap(), initial);
    overlay.set("optional", DraftInput::Undefined).unwrap();
    overlay.set("payload", json!({ "valid": true })).unwrap();
    values.push(&[json!({ "valid": true }).into()]).unwrap();

    let prepared = change.prepare().unwrap();
    assert_eq!(
        prepared.value(),
        &json!({ "payload": { "valid": true }, "number": 0, "values": [1, 2, { "valid": true }] }),
    );
    assert_eq!(replay(&initial, prepared.ops()), *prepared.value());
    tracker.adopt(&prepared).unwrap();
}

#[test]
fn expands_repeated_source_aliases_into_independent_placements() {
    let tracker = track(json!({ "left": null, "right": null, "rows": [] }));
    let shared = json!({ "nested": { "value": 1 } });
    let change = tracker.begin_change();
    let overlay = change.overlay().unwrap();
    overlay.set("left", shared.clone()).unwrap();
    overlay.set("right", shared.clone()).unwrap();
    let rows = overlay.get("rows").unwrap().as_overlay().unwrap().clone();
    rows.push(&[shared.clone().into(), shared.into()]).unwrap();

    overlay
        .get("left")
        .unwrap()
        .as_overlay()
        .unwrap()
        .get("nested")
        .unwrap()
        .as_overlay()
        .unwrap()
        .set("value", 9_i64)
        .unwrap();
    rows.get_index(0)
        .unwrap()
        .as_overlay()
        .unwrap()
        .get("nested")
        .unwrap()
        .as_overlay()
        .unwrap()
        .set("value", 8_i64)
        .unwrap();

    let prepared = change.prepare().unwrap();
    assert_eq!(
        prepared.value(),
        &json!({
            "left": { "nested": { "value": 9 } },
            "right": { "nested": { "value": 1 } },
            "rows": [{ "nested": { "value": 8 } }, { "nested": { "value": 1 } }],
        }),
    );
    assert_ne!(
        prepared.value()["left"].as_object().unwrap() as *const _,
        prepared.value()["right"].as_object().unwrap() as *const _,
    );
    assert_ne!(
        prepared.value()["rows"][0].as_object().unwrap() as *const _,
        prepared.value()["rows"][1].as_object().unwrap() as *const _,
    );
    assert_eq!(replay(&tracker.value(), prepared.ops()), *prepared.value());
    tracker.adopt(&prepared).unwrap();
}

#[test]
fn deep_clones_draft_sourced_placements_within_and_across_revisions() {
    let tracker = track(json!({
        "a": { "child": { "value": 1 } },
        "b": null,
        "rows": [{ "child": { "value": 1 } }, { "child": { "value": 2 } }],
    }));
    let first = tracker.begin_change();
    let f_ov = first.overlay().unwrap();
    let a = f_ov.get("a").unwrap().as_overlay().unwrap().clone();
    f_ov.set("b", a).unwrap();
    f_ov.get("b")
        .unwrap()
        .as_overlay()
        .unwrap()
        .get("child")
        .unwrap()
        .as_overlay()
        .unwrap()
        .set("value", 3_i64)
        .unwrap();
    let f_rows = f_ov.get("rows").unwrap().as_overlay().unwrap().clone();
    let row0 = f_rows.get_index(0).unwrap().as_overlay().unwrap().clone();
    f_rows.set_index(1, row0).unwrap();

    let first_prepared = first.prepare().unwrap();
    assert_eq!(first_prepared.value()["a"]["child"]["value"], json!(1));
    assert_eq!(first_prepared.value()["b"]["child"]["value"], json!(3));
    assert_ne!(
        first_prepared.value()["a"].as_object().unwrap() as *const _,
        first_prepared.value()["b"].as_object().unwrap() as *const _,
    );
    assert_ne!(
        first_prepared.value()["rows"][0]["child"].as_object().unwrap() as *const _,
        first_prepared.value()["rows"][1]["child"].as_object().unwrap() as *const _,
    );
    tracker.adopt(&first_prepared).unwrap();

    let base = tracker.value();
    let second = tracker.begin_change();
    second
        .overlay()
        .unwrap()
        .get("rows")
        .unwrap()
        .as_overlay()
        .unwrap()
        .get_index(1)
        .unwrap()
        .as_overlay()
        .unwrap()
        .get("child")
        .unwrap()
        .as_overlay()
        .unwrap()
        .set("value", 4_i64)
        .unwrap();
    let second_prepared = second.prepare().unwrap();
    let row_vals: Vec<i64> = second_prepared.value()["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["child"]["value"].as_i64().unwrap())
        .collect();
    assert_eq!(row_vals, vec![1, 4]);
    assert_eq!(replay(&base, second_prepared.ops()), *second_prepared.value());
    tracker.adopt(&second_prepared).unwrap();
}

#[test]
fn distinguishes_raw_committed_references_from_draft_references() {
    let tracker = track(json!({ "source": { "value": 1 }, "rawCopy": null, "draftCopy": null }));
    let raw = tracker.value()["source"].clone();
    let change = tracker.begin_change();
    let overlay = change.overlay().unwrap();
    let source_overlay = overlay.get("source").unwrap().as_overlay().unwrap().clone();
    source_overlay.set("value", 2_i64).unwrap();
    overlay.set("rawCopy", raw).unwrap();
    overlay.set("draftCopy", source_overlay).unwrap();

    let prepared = change.prepare().unwrap();
    assert_eq!(
        prepared.value(),
        &json!({ "source": { "value": 2 }, "rawCopy": { "value": 1 }, "draftCopy": { "value": 2 } }),
    );
    tracker.adopt(&prepared).unwrap();
}

#[test]
fn folds_edits_to_introduced_object_and_array_subtrees_into_placement_payloads() {
    let tracker = track(json!({ "nested": null, "rows": [] }));
    let change = tracker.begin_change();
    let overlay = change.overlay().unwrap();
    overlay.set("nested", json!({ "rows": [{ "value": 1 }] })).unwrap();
    let nested_rows = overlay
        .get("nested")
        .unwrap()
        .as_overlay()
        .unwrap()
        .get("rows")
        .unwrap()
        .as_overlay()
        .unwrap()
        .clone();
    nested_rows
        .get_index(0)
        .unwrap()
        .as_overlay()
        .unwrap()
        .set("value", 2_i64)
        .unwrap();
    nested_rows.push(&[json!({ "value": 3 }).into()]).unwrap();

    let rows = overlay.get("rows").unwrap().as_overlay().unwrap().clone();
    rows.push(&[json!({ "values": [1] }).into()]).unwrap();
    rows.get_index(0)
        .unwrap()
        .as_overlay()
        .unwrap()
        .get("values")
        .unwrap()
        .as_overlay()
        .unwrap()
        .push(&[2_i64.into()])
        .unwrap();

    let prepared = change.prepare().unwrap();
    assert_eq!(
        prepared.ops(),
        &[
            Op::Set(OpSet(
                SetVerb::S,
                vec![Seg::String("nested".into())],
                json!({ "rows": [{ "value": 2 }, { "value": 3 }] }),
            )),
            Op::Splice(OpSplice(
                SpliceVerb::P,
                vec![Seg::String("rows".into())],
                0,
                0,
                vec![json!({ "values": [1, 2] })],
            )),
        ]
    );
    assert_eq!(replay(&tracker.value(), prepared.ops()), *prepared.value());
    tracker.adopt(&prepared).unwrap();
}

#[test]
fn folds_pathological_operation_counts_without_payload_cost_estimation() {
    let mut wide = Map::new();
    for index in 0..5_000 {
        wide.insert(format!("field{index}"), json!(0));
    }
    let wide_tracker = track(Value::Object(wide));
    let wide_base = wide_tracker.value();
    let wide_change = wide_tracker.begin_change();
    let wide_ov = wide_change.overlay().unwrap();
    for index in 0..5_000 {
        wide_ov.set(&format!("field{index}"), 1_i64).unwrap();
    }
    let wide_prepared = wide_change.prepare().unwrap();
    assert_eq!(wide_prepared.ops().len(), 1);
    assert!(matches!(wide_prepared.ops()[0], Op::Replace(_)));
    assert_eq!(replay(&wide_base, wide_prepared.ops()), *wide_prepared.value());
    wide_tracker.adopt(&wide_prepared).unwrap();

    let rows: Vec<Value> = (0..15_000_i64).map(|value| json!({ "value": value })).collect();
    let sparse_tracker = track(json!({ "rows": rows }));
    let sparse_base = sparse_tracker.value();
    let sparse_change = sparse_tracker.begin_change();
    let sparse_rows = sparse_change
        .overlay()
        .unwrap()
        .get("rows")
        .unwrap()
        .as_overlay()
        .unwrap()
        .clone();
    for index in (0..15_000_i64).step_by(3) {
        sparse_rows
            .get_index(index)
            .unwrap()
            .as_overlay()
            .unwrap()
            .set("value", -index - 1)
            .unwrap();
    }
    let sparse_prepared = sparse_change.prepare().unwrap();
    assert_eq!(sparse_prepared.ops().len(), 1);
    assert!(matches!(sparse_prepared.ops()[0], Op::Replace(_)));
    assert_eq!(replay(&sparse_base, sparse_prepared.ops()), *sparse_prepared.value());
    sparse_tracker.adopt(&sparse_prepared).unwrap();
}

#[test]
fn shares_immutable_operation_placements_with_the_materialized_candidate() {
    let tracker = track(json!({ "rows": [] }));
    let change = tracker.begin_change();
    change
        .overlay()
        .unwrap()
        .get("rows")
        .unwrap()
        .as_overlay()
        .unwrap()
        .push(&[json!({ "value": 1 }).into()])
        .unwrap();
    let prepared = change.prepare().unwrap();
    let Op::Splice(OpSplice(_, _, _, _, ref items)) = prepared.ops()[0] else {
        panic!("expected splice op");
    };
    assert_eq!(items[0], prepared.value()["rows"][0]);
    tracker.adopt(&prepared).unwrap();
    assert_eq!(tracker.value(), *prepared.value());
}

// =========================================================================
// delta tracker piece arrays
// =========================================================================

#[test]
fn keeps_adoption_independent_from_mutable_detached_permutation_metadata() {
    let tracker = track(json!({ "values": [3, 1, 2] }));
    let change = tracker.begin_change();
    change
        .overlay()
        .unwrap()
        .get("values")
        .unwrap()
        .as_overlay()
        .unwrap()
        .sort_by(|l, r| {
            Ok(l.as_json()
                .unwrap()
                .as_i64()
                .unwrap()
                .cmp(&r.as_json().unwrap().as_i64().unwrap()))
        })
        .unwrap();
    let prepared = change.prepare().unwrap();
    let mut detached = prepared.ops().to_vec();
    let Some(Op::Move(OpMove(_, _, perm))) = detached.iter_mut().find(|op| matches!(op, Op::Move(_))) else {
        panic!("expected permutation op");
    };
    perm.reverse();
    tracker.adopt(&prepared).unwrap();
    assert_eq!(tracker.value()["values"], json!([1, 2, 3]));
}

#[test]
fn matches_native_splice_with_zero_or_one_argument() {
    let no_args = track(json!({ "values": [1, 2, 3] }));
    let no_args_change = no_args.begin_change();
    let removed = no_args_change
        .overlay()
        .unwrap()
        .get("values")
        .unwrap()
        .as_overlay()
        .unwrap()
        .splice(None, None, &[])
        .unwrap();
    assert!(removed.is_empty());
    let no_args_prepared = no_args_change.prepare().unwrap();
    assert_eq!(no_args_prepared.value()["values"], json!([1, 2, 3]));
    assert!(no_args_prepared.ops().is_empty());
    no_args.adopt(&no_args_prepared).unwrap();

    let one_arg = track(json!({ "values": [1, 2, 3, 4] }));
    let one_arg_change = one_arg.begin_change();
    let removed: Vec<JsonValue> = one_arg_change
        .overlay()
        .unwrap()
        .get("values")
        .unwrap()
        .as_overlay()
        .unwrap()
        .splice(Some(1), None, &[])
        .unwrap()
        .into_iter()
        .map(|v| v.as_json().unwrap().clone())
        .collect();
    assert_eq!(removed, vec![json!(2), json!(3), json!(4)]);
    let one_arg_prepared = one_arg_change.prepare().unwrap();
    assert_eq!(one_arg_prepared.value()["values"], json!([1]));
    assert_eq!(
        replay(&one_arg.value(), one_arg_prepared.ops()),
        *one_arg_prepared.value()
    );
    one_arg.adopt(&one_arg_prepared).unwrap();

    let past_end = track(json!({ "values": [1, 2, 3] }));
    let past_end_change = past_end.begin_change();
    let removed = past_end_change
        .overlay()
        .unwrap()
        .get("values")
        .unwrap()
        .as_overlay()
        .unwrap()
        .splice(Some(3), None, &[])
        .unwrap();
    assert!(removed.is_empty());
    let past_end_prepared = past_end_change.prepare().unwrap();
    assert_eq!(past_end_prepared.value()["values"], json!([1, 2, 3]));
    past_end.adopt(&past_end_prepared).unwrap();
}

#[test]
fn supports_all_structural_mutators_and_native_return_values() {
    let tracker = track(json!({ "values": [3, 1, 2] }));
    let base = tracker.value();
    let change = tracker.begin_change();
    let values = change
        .overlay()
        .unwrap()
        .get("values")
        .unwrap()
        .as_overlay()
        .unwrap()
        .clone();

    assert_eq!(values.push(&[4_i64.into()]).unwrap(), 4);
    assert_eq!(values.pop().unwrap().as_json().unwrap(), &json!(4));
    assert_eq!(values.unshift(&[0_i64.into()]).unwrap(), 4);
    assert_eq!(values.shift().unwrap().as_json().unwrap(), &json!(0));
    let spliced: Vec<JsonValue> = values
        .splice(Some(1), Some(1), &[5_i64.into(), 4_i64.into()])
        .unwrap()
        .into_iter()
        .map(|v| v.as_json().unwrap().clone())
        .collect();
    assert_eq!(spliced, vec![json!(1)]);
    assert_eq!(
        values
            .sort_by(|l, r| {
                Ok(l.as_json()
                    .unwrap()
                    .as_i64()
                    .unwrap()
                    .cmp(&r.as_json().unwrap().as_i64().unwrap()))
            })
            .unwrap(),
        values,
    );
    assert_eq!(values.reverse().unwrap(), values);
    values.fill(9_i64, Some(1), Some(3)).unwrap();
    values.copy_within(1, 0, Some(2)).unwrap();

    let prepared = change.prepare().unwrap();
    assert_eq!(prepared.value(), &json!({ "values": [5, 5, 9, 2] }));
    assert_eq!(replay(&base, prepared.ops()), *prepared.value());
    tracker.adopt(&prepared).unwrap();
}

#[test]
fn tracks_held_handles_after_reindex_and_suppresses_detached_writes() {
    let tracker = track(json!({ "values": [{ "value": "a" }, { "value": "b" }, { "value": "c" }] }));
    let change = tracker.begin_change();
    let values = change
        .overlay()
        .unwrap()
        .get("values")
        .unwrap()
        .as_overlay()
        .unwrap()
        .clone();
    let held = values.get_index(1).unwrap().as_overlay().unwrap().clone();
    values.unshift(&[json!({ "value": "front" }).into()]).unwrap();
    held.set("value", "moved").unwrap();
    assert_eq!(
        values
            .get_index(2)
            .unwrap()
            .as_overlay()
            .unwrap()
            .get("value")
            .unwrap()
            .as_json()
            .unwrap(),
        &json!("moved"),
    );
    values.splice(Some(2), Some(1), &[]).unwrap();
    held.set("value", "detached").unwrap();

    let prepared = change.prepare().unwrap();
    assert_eq!(
        prepared.value(),
        &json!({ "values": [{ "value": "front" }, { "value": "a" }, { "value": "c" }] }),
    );
    assert_eq!(replay(&tracker.value(), prepared.ops()), *prepared.value());
    tracker.adopt(&prepared).unwrap();
}

#[test]
fn replays_introduced_moved_then_edited_array_descendants() {
    let tracker = track(json!({ "values": [] }));
    let base = tracker.value();
    let change = tracker.begin_change();
    let values = change
        .overlay()
        .unwrap()
        .get("values")
        .unwrap()
        .as_overlay()
        .unwrap()
        .clone();
    values.push(&[json!({ "id": 1, "nested": [1] }).into()]).unwrap();
    let held = values.get_index(0).unwrap().as_overlay().unwrap().clone();
    values.unshift(&[json!({ "id": 0, "nested": [] }).into()]).unwrap();
    held.get("nested")
        .unwrap()
        .as_overlay()
        .unwrap()
        .push(&[2_i64.into()])
        .unwrap();
    values.reverse().unwrap();
    held.get("nested")
        .unwrap()
        .as_overlay()
        .unwrap()
        .push(&[3_i64.into()])
        .unwrap();

    let prepared = change.prepare().unwrap();
    assert_eq!(
        prepared.value(),
        &json!({
            "values": [
                { "id": 1, "nested": [1, 2, 3] },
                { "id": 0, "nested": [] },
            ],
        }),
    );
    assert_eq!(replay(&base, prepared.ops()), *prepared.value());
    tracker.adopt(&prepared).unwrap();
}

#[test]
fn combines_movement_and_edits_with_permutation_plus_final_paths() {
    let tracker = track(json!({
        "values": [
            { "rank": 3, "edited": 0 },
            { "rank": 1, "edited": 0 },
            { "rank": 2, "edited": 0 },
        ],
    }));
    let base = tracker.value();
    let change = tracker.begin_change();
    let values = change
        .overlay()
        .unwrap()
        .get("values")
        .unwrap()
        .as_overlay()
        .unwrap()
        .clone();
    let held = values.get_index(0).unwrap().as_overlay().unwrap().clone();
    values
        .sort_by(|l, r| {
            let lr = l
                .as_overlay()
                .unwrap()
                .get("rank")?
                .as_json()
                .unwrap()
                .as_i64()
                .unwrap();
            let rr = r
                .as_overlay()
                .unwrap()
                .get("rank")?
                .as_json()
                .unwrap()
                .as_i64()
                .unwrap();
            Ok(lr.cmp(&rr))
        })
        .unwrap();
    held.set("edited", 1_i64).unwrap();

    let prepared = change.prepare().unwrap();
    assert_eq!(
        prepared.ops()[0],
        Op::Move(OpMove(MoveVerb::M, vec![Seg::String("values".into())], vec![1, 2, 0])),
    );
    assert_eq!(replay(&base, prepared.ops()), *prepared.value());
    tracker.adopt(&prepared).unwrap();
}

#[test]
fn keeps_comparator_writes_and_structurally_reentrant_appends() {
    let tracker = track(json!({
        "values": [
            { "rank": 2, "comparisons": 0 },
            { "rank": 1, "comparisons": 0 },
        ],
    }));
    let base = tracker.value();
    let change = tracker.begin_change();
    let values = change
        .overlay()
        .unwrap()
        .get("values")
        .unwrap()
        .as_overlay()
        .unwrap()
        .clone();
    let mut appended = false;
    values
        .sort_by(|left, right| {
            let l_ov = left.as_overlay().unwrap();
            let r_ov = right.as_overlay().unwrap();
            let lc = l_ov.get("comparisons")?.as_json().unwrap().as_i64().unwrap();
            let rc = r_ov.get("comparisons")?.as_json().unwrap().as_i64().unwrap();
            l_ov.set("comparisons", lc + 1)?;
            r_ov.set("comparisons", rc + 1)?;
            if !appended {
                appended = true;
                values.set_index(0, json!({ "rank": 9, "comparisons": 0 }))?;
                values.push(&[json!({ "rank": 3, "comparisons": 0 }).into()])?;
            }
            let lr = l_ov.get("rank")?.as_json().unwrap().as_i64().unwrap();
            let rr = r_ov.get("rank")?.as_json().unwrap().as_i64().unwrap();
            Ok(lr.cmp(&rr))
        })
        .unwrap();

    let prepared = change.prepare().unwrap();
    let out = prepared.value()["values"].as_array().unwrap();
    assert_eq!(
        out.iter().map(|v| v["rank"].as_i64().unwrap()).collect::<Vec<_>>(),
        vec![1, 2, 3],
    );
    assert_eq!(
        out.iter()
            .map(|v| v["comparisons"].as_i64().unwrap())
            .collect::<Vec<_>>(),
        vec![1, 1, 0],
    );
    assert_eq!(replay(&base, prepared.ops()), *prepared.value());
    tracker.adopt(&prepared).unwrap();
}

#[test]
fn normalizes_duplicate_entries_created_by_structurally_reentrant_sort_callbacks() {
    let tracker = track(json!({
        "values": [
            { "rank": 2, "edited": 0, "nested": { "value": 0 } },
            { "rank": 1, "edited": 0, "nested": { "value": 0 } },
        ],
    }));
    let base = tracker.value();
    let change = tracker.begin_change();
    let values = change
        .overlay()
        .unwrap()
        .get("values")
        .unwrap()
        .as_overlay()
        .unwrap()
        .clone();
    let mut inserted = false;
    values
        .sort_by(|left, right| {
            let l_ov = left.as_overlay().unwrap();
            let r_ov = right.as_overlay().unwrap();
            let lr = l_ov.get("rank")?.as_json().unwrap().as_i64().unwrap();
            let rr = r_ov.get("rank")?.as_json().unwrap().as_i64().unwrap();
            l_ov.set("edited", lr * 10)?;
            l_ov.get("nested")?.as_overlay().unwrap().set("value", lr * 100)?;
            r_ov.set("edited", rr * 10)?;
            r_ov.get("nested")?.as_overlay().unwrap().set("value", rr * 100)?;
            if !inserted {
                inserted = true;
                values.unshift(&[json!({ "rank": 4, "edited": 0, "nested": { "value": 0 } }).into()])?;
            }
            Ok(lr.cmp(&rr))
        })
        .unwrap();

    let prepared = change.prepare().unwrap();
    let out = prepared.value()["values"].as_array().unwrap();
    assert_eq!(
        out.iter().map(|v| v["rank"].as_i64().unwrap()).collect::<Vec<_>>(),
        vec![1, 2, 1],
    );
    assert_eq!(
        out.iter().map(|v| v["edited"].as_i64().unwrap()).collect::<Vec<_>>(),
        vec![10, 20, 10],
    );
    assert_eq!(
        out.iter()
            .map(|v| v["nested"]["value"].as_i64().unwrap())
            .collect::<Vec<_>>(),
        vec![100, 200, 100],
    );
    assert_ne!(
        out[0].as_object().unwrap() as *const _,
        out[2].as_object().unwrap() as *const _,
    );
    assert_ne!(
        out[0]["nested"].as_object().unwrap() as *const _,
        out[2]["nested"].as_object().unwrap() as *const _,
    );
    assert_eq!(replay(&base, prepared.ops()), *prepared.value());
    tracker.adopt(&prepared).unwrap();
}

#[test]
fn folds_dense_child_and_direct_index_edits_into_one_full_array_operation() {
    let rows_init: Vec<Value> = (0..1_000_i64).map(|value| json!({ "value": value })).collect();
    let rows_tracker = track(json!({ "rows": rows_init }));
    let rows_base = rows_tracker.value();
    let rows_change = rows_tracker.begin_change();
    let rows = rows_change
        .overlay()
        .unwrap()
        .get("rows")
        .unwrap()
        .as_overlay()
        .unwrap()
        .clone();
    for i in 0..1_000_i64 {
        let row = rows.get_index(i).unwrap().as_overlay().unwrap().clone();
        let next = row.get("value").unwrap().as_json().unwrap().as_i64().unwrap() + 1;
        row.set("value", next).unwrap();
    }
    let rows_prepared = rows_change.prepare().unwrap();
    assert_eq!(rows_prepared.ops().len(), 1);
    assert!(matches!(rows_prepared.ops()[0], Op::Splice(_)));
    assert_eq!(replay(&rows_base, rows_prepared.ops()), *rows_prepared.value());
    rows_tracker.adopt(&rows_prepared).unwrap();

    let values_init: Vec<Value> = (0..1_000_i64).map(|value| json!(value)).collect();
    let values_tracker = track(json!({ "values": values_init }));
    let values_base = values_tracker.value();
    let values_change = values_tracker.begin_change();
    let values = values_change
        .overlay()
        .unwrap()
        .get("values")
        .unwrap()
        .as_overlay()
        .unwrap()
        .clone();
    for index in 0..600_i64 {
        values.set_index(index, -index - 1).unwrap();
    }
    let values_prepared = values_change.prepare().unwrap();
    assert_eq!(values_prepared.ops().len(), 1);
    assert!(matches!(values_prepared.ops()[0], Op::Splice(_)));
    assert_eq!(replay(&values_base, values_prepared.ops()), *values_prepared.value());
    values_tracker.adopt(&values_prepared).unwrap();
}

#[test]
fn folds_only_a_deeply_nested_dense_array_region() {
    let rows_init: Vec<Value> = (0..2_000_i64)
        .map(|value| json!({ "nested": { "value": value } }))
        .collect();
    let tracker = track(json!({ "rows": rows_init }));
    let base = tracker.value();
    let change = tracker.begin_change();
    let rows = change
        .overlay()
        .unwrap()
        .get("rows")
        .unwrap()
        .as_overlay()
        .unwrap()
        .clone();
    rows.get_index(0)
        .unwrap()
        .as_overlay()
        .unwrap()
        .get("nested")
        .unwrap()
        .as_overlay()
        .unwrap()
        .set("value", -1_i64)
        .unwrap();
    for index in 500..1_000_i64 {
        rows.get_index(index)
            .unwrap()
            .as_overlay()
            .unwrap()
            .get("nested")
            .unwrap()
            .as_overlay()
            .unwrap()
            .set("value", -index)
            .unwrap();
    }
    let prepared = change.prepare().unwrap();
    let Some(Op::Splice(OpSplice(_, path, start, delete_count, items))) =
        prepared.ops().iter().find(|op| matches!(op, Op::Splice(_)))
    else {
        panic!("expected regional splice");
    };
    assert_eq!(path, &[Seg::String("rows".into())]);
    assert_eq!((*start, *delete_count), (500, 500));
    assert_eq!(items.len(), 500);
    assert_eq!(prepared.ops().len(), 2);
    assert_eq!(replay(&base, prepared.ops()), *prepared.value());
    tracker.adopt(&prepared).unwrap();
}

#[test]
fn does_not_cache_emission_paths_for_nested_reserved_key_folds_covered_by_an_outer_region() {
    let mut rows: Vec<Value> = (0..400).map(|_| json!({ "flag": 0 })).collect();
    let vals: String = (0..400).map(|v| v.to_string()).collect::<Vec<_>>().join(",");
    rows[100]["special"] = pi_js::json::parse(&format!(r#"{{"__proto__":{{"values":[{vals}]}}}}"#)).unwrap();
    let tracker = track(json!({ "rows": rows }));
    let base = tracker.value();
    let change = tracker.begin_change();
    let rows_ov = change
        .overlay()
        .unwrap()
        .get("rows")
        .unwrap()
        .as_overlay()
        .unwrap()
        .clone();
    for index in 0..256_i64 {
        rows_ov
            .get_index(index)
            .unwrap()
            .as_overlay()
            .unwrap()
            .set("flag", 1_i64)
            .unwrap();
    }
    let inner_values = rows_ov
        .get_index(100)
        .unwrap()
        .as_overlay()
        .unwrap()
        .get("special")
        .unwrap()
        .as_overlay()
        .unwrap()
        .get("__proto__")
        .unwrap()
        .as_overlay()
        .unwrap()
        .get("values")
        .unwrap()
        .as_overlay()
        .unwrap()
        .clone();
    for index in 0..256_i64 {
        inner_values.set_index(index, -index - 1).unwrap();
    }
    let prepared = change.prepare().unwrap();
    assert_eq!(prepared.ops().len(), 1);
    let Op::Splice(OpSplice(_, path, start, delete_count, _)) = &prepared.ops()[0] else {
        panic!("expected splice");
    };
    assert_eq!(path, &[Seg::String("rows".into())]);
    assert_eq!((*start, *delete_count), (0, 256));
    assert_eq!(replay(&base, prepared.ops()), *prepared.value());
    tracker.adopt(&prepared).unwrap();
}

#[test]
fn suppresses_nested_structural_and_leaf_operations_covered_by_an_outer_dense_region() {
    let rows: Vec<Value> = (0..400)
        .map(|index| {
            let values: Vec<Value> = if index == 100 {
                (0..400_i64).map(|value| json!({ "value": value })).collect()
            } else {
                Vec::new()
            };
            json!({ "flag": 0, "values": values })
        })
        .collect();
    let tracker = track(json!({ "rows": rows }));
    let base = tracker.value();
    let change = tracker.begin_change();
    let rows_ov = change
        .overlay()
        .unwrap()
        .get("rows")
        .unwrap()
        .as_overlay()
        .unwrap()
        .clone();
    for index in 0..256_i64 {
        rows_ov
            .get_index(index)
            .unwrap()
            .as_overlay()
            .unwrap()
            .set("flag", 1_i64)
            .unwrap();
    }
    let row100_values = rows_ov
        .get_index(100)
        .unwrap()
        .as_overlay()
        .unwrap()
        .get("values")
        .unwrap()
        .as_overlay()
        .unwrap()
        .clone();
    for index in 0..256_i64 {
        row100_values
            .get_index(index)
            .unwrap()
            .as_overlay()
            .unwrap()
            .set("value", -index - 1)
            .unwrap();
    }
    row100_values.push(&[json!({ "value": 999 }).into()]).unwrap();

    let prepared = change.prepare().unwrap();
    assert_eq!(prepared.ops().len(), 1);
    let Op::Splice(OpSplice(_, path, start, delete_count, _)) = &prepared.ops()[0] else {
        panic!("expected splice");
    };
    assert_eq!(path, &[Seg::String("rows".into())]);
    assert_eq!((*start, *delete_count), (0, 256));
    assert_eq!(prepared.value()["rows"][100]["values"].as_array().unwrap().len(), 401);
    assert_eq!(replay(&base, prepared.ops()), *prepared.value());
    tracker.adopt(&prepared).unwrap();
}

#[test]
fn emits_multiple_disjoint_dense_regions_and_preserves_operations_outside_their_boundaries() {
    let values_init: Vec<Value> = (0..1_400_i64).map(|value| json!({ "value": value })).collect();
    let tracker = track(json!({ "values": values_init }));
    let base = tracker.value();
    let change = tracker.begin_change();
    let values = change
        .overlay()
        .unwrap()
        .get("values")
        .unwrap()
        .as_overlay()
        .unwrap()
        .clone();
    for index in [97_i64, 358, 897, 1_158] {
        values
            .get_index(index)
            .unwrap()
            .as_overlay()
            .unwrap()
            .set("value", -index - 1)
            .unwrap();
    }
    for index in 100..356_i64 {
        values
            .get_index(index)
            .unwrap()
            .as_overlay()
            .unwrap()
            .set("value", -index - 1)
            .unwrap();
    }
    for index in 900..1_156_i64 {
        values
            .get_index(index)
            .unwrap()
            .as_overlay()
            .unwrap()
            .set("value", -index - 1)
            .unwrap();
    }
    let prepared = change.prepare().unwrap();
    let splices: Vec<(i64, i64)> = prepared
        .ops()
        .iter()
        .filter_map(|op| match op {
            Op::Splice(OpSplice(_, path, start, delete_count, _)) => {
                assert_eq!(path, &[Seg::String("values".into())]);
                Some((*start, *delete_count))
            }
            _ => None,
        })
        .collect();
    assert_eq!(splices, vec![(100, 256), (900, 256)]);
    for index in [97_i64, 358, 897, 1_158] {
        assert!(prepared.ops().contains(&Op::Set(OpSet(
            SetVerb::S,
            vec![
                Seg::String("values".into()),
                Seg::Index(index),
                Seg::String("value".into()),
            ],
            json!(-index - 1),
        ))));
    }
    assert_eq!(prepared.ops().len(), 6);
    assert_eq!(replay(&base, prepared.ops()), *prepared.value());
    tracker.adopt(&prepared).unwrap();
}

#[test]
fn normalizes_a_20_000_operation_queue_transaction_to_two_operations() {
    let values_init: Vec<Value> = (0..20_000_i64).map(|value| json!({ "value": value })).collect();
    let tracker = track(json!({ "values": values_init }));
    let base = tracker.value();
    let change = tracker.begin_change();
    let values = change
        .overlay()
        .unwrap()
        .get("values")
        .unwrap()
        .as_overlay()
        .unwrap()
        .clone();
    for index in 0..10_000_i64 {
        values.shift().unwrap();
        values.push(&[json!({ "value": 20_000 + index }).into()]).unwrap();
    }
    let prepared = change.prepare().unwrap();
    assert_eq!(prepared.ops().len(), 2);
    assert_eq!(replay(&base, prepared.ops()), *prepared.value());
    tracker.adopt(&prepared).unwrap();
}

#[test]
fn handles_adversarial_fragmentation_and_resolves_held_handles_without_piece_scans() {
    let size = 20_000_i64;
    let values_init: Vec<Value> = (0..size).map(|value| json!({ "value": value })).collect();
    let tracker = track(json!({ "values": values_init }));
    let mut expected = tracker.value();
    let base = tracker.value();
    let change = tracker.begin_change();
    let values = change
        .overlay()
        .unwrap()
        .get("values")
        .unwrap()
        .as_overlay()
        .unwrap()
        .clone();
    let held_indices = [1_i64, 1_001, 5_001, 10_001, 15_001, 19_999];
    let held: Vec<DraftOverlay> = held_indices
        .iter()
        .map(|&idx| values.get_index(idx).unwrap().as_overlay().unwrap().clone())
        .collect();
    let expected_arr = expected["values"].as_array_mut().unwrap();
    for index in (0..size).step_by(2) {
        let replacement = json!({ "value": -index - 1 });
        values
            .splice(Some(index), Some(1), &[replacement.clone().into()])
            .unwrap();
        expected_arr[usize::try_from(index).unwrap()] = replacement;
    }
    for handle in held {
        let cur = handle.get("value").unwrap().as_json().unwrap().as_i64().unwrap();
        handle.set("value", cur + 100_000).unwrap();
    }
    for idx in held_indices {
        let u = usize::try_from(idx).unwrap();
        let cur = expected_arr[u]["value"].as_i64().unwrap();
        expected_arr[u]["value"] = json!(cur + 100_000);
    }
    let prepared = change.prepare().unwrap();
    assert_eq!(prepared.value(), &expected);
    assert_eq!(replay(&base, prepared.ops()), *prepared.value());
    tracker.adopt(&prepared).unwrap();
}

#[test]
fn normalizes_restored_overrides_and_cancelled_structural_edits_to_no_operations() {
    let tracker = track(json!({ "values": [1, 2, 3] }));
    let change = tracker.begin_change();
    let values = change
        .overlay()
        .unwrap()
        .get("values")
        .unwrap()
        .as_overlay()
        .unwrap()
        .clone();
    values.set_index(1, 9_i64).unwrap();
    values.set_index(1, 2_i64).unwrap();
    values.reverse().unwrap();
    values.reverse().unwrap();
    values.push(&[4_i64.into()]).unwrap();
    assert_eq!(values.pop().unwrap().as_json().unwrap(), &json!(4));

    let prepared = change.prepare().unwrap();
    assert_eq!(prepared.value(), &json!({ "values": [1, 2, 3] }));
    assert!(prepared.ops().is_empty());
    tracker.adopt(&prepared).unwrap();
}

#[test]
fn handles_null_sparse_overrides_without_confusing_absence() {
    let values_init: Vec<Value> = (0..10_000_i64).map(|value| json!(value)).collect();
    let tracker = track(json!({ "values": values_init }));
    settle_overlay(&tracker, |overlay| {
        let values = overlay.get("values").unwrap().as_overlay().unwrap().clone();
        values.set_index(17, JsonValue::Null).unwrap();
        values.set_index(9_000, JsonValue::Null).unwrap();
    });
    assert_eq!(tracker.value()["values"][17], JsonValue::Null);
    assert_eq!(tracker.value()["values"][9_000], JsonValue::Null);
}

#[test]
fn supports_self_overlapping_fill_and_copy_within_by_value() {
    let tracker = track(json!({ "values": [{ "n": 0 }, { "n": 1 }, { "n": 2 }, { "n": 3 }] }));
    settle_overlay(&tracker, |overlay| {
        let values = overlay.get("values").unwrap().as_overlay().unwrap().clone();
        values.copy_within(1, 0, Some(3)).unwrap();
        let slot1 = values.get_index(1).unwrap().as_overlay().unwrap().clone();
        values.fill(slot1, Some(0), Some(2)).unwrap();
        values
            .get_index(0)
            .unwrap()
            .as_overlay()
            .unwrap()
            .set("n", 9_i64)
            .unwrap();
    });
    let ns: Vec<i64> = tracker.value()["values"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["n"].as_i64().unwrap())
        .collect();
    assert_eq!(ns, vec![9, 0, 1, 2]);
}

#[test]
fn inserts_100_000_items_with_unshift_and_splice() {
    for method in ["unshift", "splice"] {
        let tracker = track(json!({ "values": [-1] }));
        let base = tracker.value();
        let change = tracker.begin_change();
        let values = change
            .overlay()
            .unwrap()
            .get("values")
            .unwrap()
            .as_overlay()
            .unwrap()
            .clone();
        let items: Vec<DraftInput> = (0..100_000_i64).map(DraftInput::from).collect();
        if method == "unshift" {
            values.unshift(&items).unwrap();
        } else {
            values.splice(Some(1), Some(0), &items).unwrap();
        }
        let prepared = change.prepare().unwrap();
        let out = prepared.value()["values"].as_array().unwrap();
        assert_eq!(out.len(), 100_001);
        assert_eq!(out[if method == "unshift" { 99_999 } else { 100_000 }], json!(99_999));
        assert_eq!(replay(&base, prepared.ops()), *prepared.value());
        tracker.adopt(&prepared).unwrap();
    }
}

// =========================================================================
// delta tracker randomized transactions
// =========================================================================

struct Rng {
    seed: u32,
}

impl Rng {
    fn new(seed: u32) -> Self {
        Self { seed }
    }

    fn next_f64(&mut self) -> f64 {
        self.seed = self.seed.wrapping_add(0x6d2b_79f5);
        let mut value = (self.seed ^ (self.seed >> 15)).wrapping_mul(1 | self.seed);
        value = value.wrapping_add((value ^ (value >> 7)).wrapping_mul(61 | value)) ^ value;
        f64::from(value ^ (value >> 14)) / 4_294_967_296.0
    }
}

fn rand_item(value: i64) -> Value {
    json!({ "id": value, "score": value % 7 })
}

fn mutate_expected_tx(document: &mut Value, choice: usize, value: i64) {
    let obj = document.as_object_mut().unwrap();
    match choice {
        0 => {
            let next = format!("{}-{value}", obj["text"].as_str().unwrap());
            obj.insert("text".into(), Value::String(next));
        }
        1 => {
            obj["values"].as_array_mut().unwrap().push(rand_item(value));
        }
        2 => {
            obj["values"].as_array_mut().unwrap().insert(0, rand_item(value));
        }
        3 => {
            let arr = obj["values"].as_array_mut().unwrap();
            if !arr.is_empty() {
                arr.remove(0);
            }
        }
        4 => {
            obj["values"].as_array_mut().unwrap().pop();
        }
        5 => {
            let arr = obj["values"].as_array_mut().unwrap();
            let v = usize::try_from(value).unwrap();
            let index = if arr.is_empty() { 0 } else { v % (arr.len() + 1) };
            let delete_count = if arr.is_empty() { 0 } else { v % 2 };
            let end = (index + delete_count).min(arr.len());
            arr.splice(index..end, [rand_item(value)]);
        }
        6 => {
            obj["values"].as_array_mut().unwrap().reverse();
        }
        7 => {
            obj["values"]
                .as_array_mut()
                .unwrap()
                .sort_by_key(|v| v["id"].as_i64().unwrap());
        }
        8 => {
            let arr = obj["values"].as_array_mut().unwrap();
            if !arr.is_empty() {
                let idx = usize::try_from(value).unwrap() % arr.len();
                arr[idx]["score"] = json!(value);
            }
        }
        9 => {
            let meta = obj["meta"].as_object_mut().unwrap();
            let next_rev = meta["revision"].as_i64().unwrap() + 1;
            meta.insert("revision".into(), json!(next_rev));
            meta.insert("label".into(), json!(format!("r-{value}")));
        }
        _ => {
            obj["meta"].as_object_mut().unwrap().shift_remove("label");
        }
    }
}

fn mutate_overlay_tx(document: &DraftOverlay, choice: usize, value: i64) {
    match choice {
        0 => {
            let prev = document.get("text").unwrap();
            let next = format!("{}-{value}", prev.as_json().unwrap().as_str().unwrap());
            document.set("text", next).unwrap();
        }
        1 => {
            let values = document.get("values").unwrap().as_overlay().unwrap().clone();
            values.push(&[rand_item(value).into()]).unwrap();
        }
        2 => {
            let values = document.get("values").unwrap().as_overlay().unwrap().clone();
            values.unshift(&[rand_item(value).into()]).unwrap();
        }
        3 => {
            let values = document.get("values").unwrap().as_overlay().unwrap().clone();
            if !values.is_empty().unwrap() {
                values.shift().unwrap();
            }
        }
        4 => {
            let values = document.get("values").unwrap().as_overlay().unwrap().clone();
            if !values.is_empty().unwrap() {
                values.pop().unwrap();
            }
        }
        5 => {
            let values = document.get("values").unwrap().as_overlay().unwrap().clone();
            let len = values.len().unwrap();
            let v = usize::try_from(value).unwrap();
            let index = if len == 0 { 0 } else { v % (len + 1) };
            let delete_count = if len == 0 { 0 } else { v % 2 };
            values
                .splice(
                    Some(i64::try_from(index).unwrap()),
                    Some(i64::try_from(delete_count).unwrap()),
                    &[rand_item(value).into()],
                )
                .unwrap();
        }
        6 => {
            let values = document.get("values").unwrap().as_overlay().unwrap().clone();
            values.reverse().unwrap();
        }
        7 => {
            let values = document.get("values").unwrap().as_overlay().unwrap().clone();
            values
                .sort_by(|l, r| {
                    let lid = l.as_overlay().unwrap().get("id")?.as_json().unwrap().as_i64().unwrap();
                    let rid = r.as_overlay().unwrap().get("id")?.as_json().unwrap().as_i64().unwrap();
                    Ok(lid.cmp(&rid))
                })
                .unwrap();
        }
        8 => {
            let values = document.get("values").unwrap().as_overlay().unwrap().clone();
            let len = values.len().unwrap();
            if len > 0 {
                let idx = usize::try_from(value).unwrap() % len;
                values
                    .get_index(i64::try_from(idx).unwrap())
                    .unwrap()
                    .as_overlay()
                    .unwrap()
                    .set("score", value)
                    .unwrap();
            }
        }
        9 => {
            let meta = document.get("meta").unwrap().as_overlay().unwrap().clone();
            let next_rev = meta.get("revision").unwrap().as_json().unwrap().as_i64().unwrap() + 1;
            meta.set("revision", next_rev).unwrap();
            meta.set("label", format!("r-{value}")).unwrap();
        }
        _ => {
            document
                .get("meta")
                .unwrap()
                .as_overlay()
                .unwrap()
                .delete("label")
                .unwrap();
        }
    }
}

#[test]
fn matches_policy_detached_replay_and_adopted_state_after_multi_operation_transactions() {
    for seed in 1_u32..=40 {
        let mut rng = Rng::new(seed);
        let initial = json!({
            "values": (0..4_i64).map(|id| json!({ "id": id, "score": 0 })).collect::<Vec<_>>(),
            "text": "start",
            "meta": { "revision": 0 },
        });
        let tracker = track(initial);
        let mut expected = tracker.value();

        for transaction in 0..25_i64 {
            let base_root = tracker.value();
            let base = base_root.clone();
            let change = tracker.begin_change();
            let overlay = change.overlay().unwrap();
            for operation in 0..5_i64 {
                let choice = (rng.next_f64() * 11.0).floor() as usize;
                let value = i64::from(seed) * 10_000 + transaction * 10 + operation;
                mutate_expected_tx(&mut expected, choice, value);
                mutate_overlay_tx(&overlay, choice, value);
            }
            let prepared = change.prepare().unwrap();
            let policy = prepared.value().clone();
            assert_eq!(base_root, base, "prepare changed base seed {seed} tx {transaction}");
            assert_eq!(policy, expected, "policy seed {seed} tx {transaction}");
            assert_eq!(
                replay(&base, prepared.ops()),
                policy,
                "replay seed {seed} tx {transaction}",
            );
            tracker.adopt(&prepared).unwrap();
            assert_eq!(base_root, base, "adopt changed base seed {seed} tx {transaction}");
            assert_eq!(tracker.value(), *prepared.value());
            assert_eq!(tracker.value(), policy, "adopt seed {seed} tx {transaction}");
        }
    }
}

// =========================================================================
// delta tracker object emission scaling & prepare_with
// =========================================================================

#[test]
fn orders_many_reverse_depth_object_edits_through_the_bounded_fallback() {
    let mut initial = json!({ "value": 0 });
    for _ in 0..512 {
        initial = json!({ "value": 0, "next": initial });
    }
    let tracker = track(initial.clone());
    let change = tracker.begin_change();
    let mut nodes: Vec<DraftOverlay> = Vec::new();
    let mut current: Option<DraftOverlay> = Some(change.overlay().unwrap());
    while let Some(node) = current {
        let next = match node.get("next").unwrap() {
            DraftValue::Overlay(o) => Some(o),
            DraftValue::Undefined | DraftValue::Json(_) => None,
        };
        nodes.push(node);
        current = next;
    }
    assert_eq!(nodes.len(), 513);
    for (index, node) in nodes.iter().enumerate().rev() {
        node.set("value", i64::try_from(index + 1).unwrap()).unwrap();
    }
    let prepared = change.prepare().unwrap();
    assert_eq!(prepared.ops().len(), nodes.len());
    assert_eq!(replay(&initial, prepared.ops()), *prepared.value());
    tracker.adopt(&prepared).unwrap();
}

// =========================================================================
// delta tracker security and storage-style cloning
// =========================================================================

#[test]
fn defines_own_properties_without_invoking_inherited_setters_and_handles_reserved_keys() {
    // PORT: Rust serde_json::Map writes own entries directly with no prototype setter chain.
    let initial: Value = pi_js::json::parse(r#"{"safe":{"__proto__":{"value":1}}}"#).unwrap();
    let tracker = track(initial);
    let change = tracker.begin_change();
    change
        .overlay()
        .unwrap()
        .get("safe")
        .unwrap()
        .as_overlay()
        .unwrap()
        .get("__proto__")
        .unwrap()
        .as_overlay()
        .unwrap()
        .set("value", 2_i64)
        .unwrap();

    let prepared = change.prepare().unwrap();
    assert_eq!(prepared.value()["safe"]["__proto__"]["value"], json!(2));
    assert_eq!(replay(&tracker.value(), prepared.ops()), *prepared.value());
    tracker.adopt(&prepared).unwrap();
    assert_eq!(tracker.value()["safe"]["__proto__"]["value"], json!(2));
}

#[test]
fn supports_storage_style_recursive_clone_and_prepare_with_draft_values() {
    fn storage_clone(value: &Value) -> Value {
        match value {
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => value.clone(),
            Value::Array(arr) => Value::Array(arr.iter().map(storage_clone).collect()),
            Value::Object(map) => {
                let mut out = Map::with_capacity(map.len());
                for (k, v) in map {
                    out.insert(k.clone(), storage_clone(v));
                }
                Value::Object(out)
            }
        }
    }

    let tracker = track(json!({ "rows": [{ "value": 1 }] }));
    let change = tracker.begin_change();
    let rows = change
        .overlay()
        .unwrap()
        .get("rows")
        .unwrap()
        .as_overlay()
        .unwrap()
        .clone();
    rows.get_index(0)
        .unwrap()
        .as_overlay()
        .unwrap()
        .set("value", 2_i64)
        .unwrap();
    rows.push(&[json!({ "value": 3 }).into()]).unwrap();
    let prepared = change.prepare().unwrap();
    assert_eq!(
        storage_clone(prepared.value()),
        json!({ "rows": [{ "value": 2 }, { "value": 3 }] }),
    );
    tracker.adopt(&prepared).unwrap();

    // Also verify `Change::state()` + `Change::prepare_with(next)` (`Draft<T> = T`) lifecycle.
    let change2 = tracker.begin_change();
    let mut draft = change2.state().unwrap();
    draft["rows"][0]["value"] = json!(10);
    let prepared2 = change2.prepare_with(draft).unwrap();
    assert_eq!(
        prepared2.ops(),
        &[Op::Set(OpSet(
            SetVerb::S,
            vec![Seg::String("rows".into()), Seg::Index(0), Seg::String("value".into())],
            json!(10),
        ))]
    );
    assert_eq!(replay(&tracker.value(), prepared2.ops()), *prepared2.value());
    tracker.adopt(&prepared2).unwrap();
    assert_eq!(tracker.value()["rows"][0]["value"], json!(10));
}
