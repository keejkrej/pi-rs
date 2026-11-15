//! Port of packages/chord/test/state-draft.test.ts

use pi_chord::delta::tracker::{DraftInput, DraftOverlay, track};
use pi_chord::delta::{Op, OpSet, Seg, SetVerb, apply_immutable};
use serde_json::json;

#[test]
fn copies_only_changed_branches() {
    let tracker = track(json!({
        "changed": { "count": 1, "sibling": { "value": "kept" } },
        "untouched": { "value": 2 },
    }));
    let base = tracker.value();
    let change = tracker.begin_change();
    let overlay = change.overlay().unwrap();
    let first: DraftOverlay = overlay.get("changed").unwrap().as_overlay().unwrap().clone();
    assert_eq!(overlay.get("changed").unwrap().as_overlay().unwrap(), &first);

    first.set("count", 3_i64).unwrap();
    let prepared = change.prepare().unwrap();

    assert_ne!(prepared.value(), &base);
    assert_ne!(prepared.value()["changed"], base["changed"]);
    assert_eq!(prepared.value()["changed"]["sibling"], base["changed"]["sibling"]);
    assert_eq!(prepared.value()["untouched"], base["untouched"]);
    assert_eq!(
        prepared.ops(),
        &[Op::Set(OpSet(
            SetVerb::S,
            vec![Seg::String("changed".into()), Seg::String("count".into())],
            json!(3),
        ))]
    );
}

#[test]
fn supports_frozen_bases_deletion_and_native_array_mutators() {
    let tracker = track(json!({
        "optional": "remove",
        "values": [3, 1, 2],
    }));
    let change = tracker.begin_change();
    let overlay = change.overlay().unwrap();

    assert!(overlay.delete("optional").unwrap());
    let values = overlay.get("values").unwrap().as_overlay().unwrap().clone();

    values.push(&[4_i64.into()]).unwrap();
    assert_eq!(values.pop().unwrap().as_json().unwrap(), &json!(4));

    values.unshift(&[0_i64.into()]).unwrap();
    assert_eq!(values.shift().unwrap().as_json().unwrap(), &json!(0));

    let removed = values.splice(Some(1), Some(1), &[5_i64.into(), 4_i64.into()]).unwrap();
    assert_eq!(removed.len(), 1);
    assert_eq!(removed[0].as_json().unwrap(), &json!(1));

    values
        .sort_by(|left, right| {
            let l = left.as_json().unwrap().as_i64().unwrap();
            let r = right.as_json().unwrap().as_i64().unwrap();
            Ok(l.cmp(&r))
        })
        .unwrap();
    values.reverse().unwrap();
    values.fill(9_i64, Some(1), Some(3)).unwrap();
    values.copy_within(1, 0, Some(2)).unwrap();

    let prepared = change.prepare().unwrap();
    assert_eq!(prepared.value(), &json!({ "values": [5, 5, 9, 2] }));
}

#[test]
fn copies_inserted_draft_values_without_aliasing_their_original_handle() {
    let tracker = track(json!({ "values": [{ "value": 1 }, { "value": 2 }] }));
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
    values.unshift(&[held.clone().into()]).unwrap();
    held.set("value", 9_i64).unwrap();

    let prepared = change.prepare().unwrap();
    assert_eq!(
        prepared.value()["values"],
        json!([{ "value": 1 }, { "value": 9 }, { "value": 2 }]),
    );
    assert_ne!(
        prepared.value()["values"][0].as_object().unwrap() as *const _,
        prepared.value()["values"][1].as_object().unwrap() as *const _,
    );
}

#[test]
fn keeps_comparator_edits_when_sorting_object_drafts() {
    let tracker = track(json!({
        "rows": [
            { "rank": 2, "comparisons": 0 },
            { "rank": 1, "comparisons": 0 },
        ],
    }));
    let change = tracker.begin_change();
    let rows = change
        .overlay()
        .unwrap()
        .get("rows")
        .unwrap()
        .as_overlay()
        .unwrap()
        .clone();

    rows.sort_by(|left, right| {
        let left_row = left.as_overlay().unwrap();
        let right_row = right.as_overlay().unwrap();
        let lc = left_row.get("comparisons")?.as_json().unwrap().as_i64().unwrap();
        let rc = right_row.get("comparisons")?.as_json().unwrap().as_i64().unwrap();
        left_row.set("comparisons", lc + 1)?;
        right_row.set("comparisons", rc + 1)?;
        let lr = left_row.get("rank")?.as_json().unwrap().as_i64().unwrap();
        let rr = right_row.get("rank")?.as_json().unwrap().as_i64().unwrap();
        Ok(lr.cmp(&rr))
    })
    .unwrap();

    let prepared = change.prepare().unwrap();
    let result_rows = prepared.value()["rows"].as_array().unwrap();
    let ranks: Vec<i64> = result_rows.iter().map(|r| r["rank"].as_i64().unwrap()).collect();
    let comparisons: Vec<i64> = result_rows.iter().map(|r| r["comparisons"].as_i64().unwrap()).collect();
    assert_eq!(ranks, vec![1, 2]);
    assert_eq!(comparisons, vec![1, 1]);
}

#[test]
fn drops_writes_through_detached_child_handles() {
    let tracker = track(json!({
        "child": { "value": "removed" },
        "items": [{ "value": "removed" }, { "value": "kept" }],
    }));
    let change = tracker.begin_change();
    let overlay = change.overlay().unwrap();
    let child = overlay.get("child").unwrap().as_overlay().unwrap().clone();
    let items = overlay.get("items").unwrap().as_overlay().unwrap().clone();
    let shifted = items.shift().unwrap().as_overlay().unwrap().clone();

    assert!(overlay.delete("child").unwrap());
    child.set("value", "detached child").unwrap();
    shifted.set("value", "detached item").unwrap();

    assert_eq!(
        change.prepare().unwrap().value(),
        &json!({ "items": [{ "value": "kept" }] }),
    );
}

#[test]
fn inserts_100_000_items_with_unshift_without_argument_overflow() {
    let tracker = track(json!({ "values": [-1] }));
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
    values.unshift(&items).unwrap();

    let prepared = change.prepare().unwrap();
    let out = prepared.value()["values"].as_array().unwrap();
    assert_eq!(out.len(), 100_001);
    assert_eq!(out[0], json!(0));
    assert_eq!(out[99_999], json!(99_999));
    assert_eq!(
        apply_immutable(Some(prepared.base().clone()), prepared.ops()).unwrap(),
        *prepared.value(),
    );
}

#[test]
fn inserts_100_000_items_with_splice_without_argument_overflow() {
    let tracker = track(json!({ "values": [-1] }));
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
    values.splice(Some(1), Some(0), &items).unwrap();

    let prepared = change.prepare().unwrap();
    let out = prepared.value()["values"].as_array().unwrap();
    assert_eq!(out.len(), 100_001);
    assert_eq!(out[1], json!(0));
    assert_eq!(out[100_000], json!(99_999));
    assert_eq!(
        apply_immutable(Some(prepared.base().clone()), prepared.ops()).unwrap(),
        *prepared.value(),
    );
}

#[test]
fn rejects_non_strict_json_placements_without_mutating_the_draft_or_base() {
    let tracker = track(json!({ "number": 0, "payload": null, "values": [1, 2] }));
    let change = tracker.begin_change();
    let overlay = change.overlay().unwrap();
    let values = overlay.get("values").unwrap().as_overlay().unwrap().clone();

    let err1 = values.set_index(0, DraftInput::Undefined).unwrap_err();
    assert!(err1.message().contains("strict JSON"));

    // PORT: `DraftInput::from(f64::NAN)` lowers non-finite floats to `DraftInput::Undefined`,
    // and in Rust `JsonValue` cannot hold JS `undefined` object fields or `Date` instances.
    let err2 = values.push(&[DraftInput::Undefined]).unwrap_err();
    assert!(err2.message().contains("strict JSON"));

    let err3 = overlay.set("number", f64::NAN).unwrap_err();
    assert!(err3.message().contains("strict JSON"));

    assert_eq!(overlay.to_json().unwrap(), tracker.value());
    change.abort();
    assert_eq!(
        tracker.value(),
        json!({ "number": 0, "payload": null, "values": [1, 2] }),
    );
}

#[test]
fn rejects_array_holes_without_mutating_the_committed_base() {
    let tracker = track(json!({ "values": [1, 2] }));
    let change = tracker.begin_change();
    let values = change
        .overlay()
        .unwrap()
        .get("values")
        .unwrap()
        .as_overlay()
        .unwrap()
        .clone();

    let err = values.delete_index(0).unwrap_err();
    let msg = err.message();
    assert!(msg.contains("dense") || msg.contains("holes"));

    change.abort();
    assert_eq!(tracker.value()["values"], json!([1, 2]));
}
