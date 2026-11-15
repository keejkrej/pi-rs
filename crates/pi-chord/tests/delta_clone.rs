//! Port of packages/chord/test/delta-clone.test.ts

use pi_chord::delta::tracker::DraftInput;
use pi_chord::delta::{apply, track};
use serde_json::json;

#[test]
fn takes_immutable_ownership_of_imported_revision() {
    let input = json!({
        "point": { "x": 3, "y": 7, "pressure": 0.1 },
        "rows": [{ "values": [0, false, null, "text", { "n": 1 }] }],
    });
    let tracker = track(input.clone());
    assert_eq!(tracker.value(), input);
    assert_eq!(tracker.value()["point"], input["point"]);
    assert_eq!(tracker.value()["rows"][0]["values"][4], input["rows"][0]["values"][4]);
}

#[test]
fn copies_assigned_and_inserted_values_immediately() {
    let tracker = track(json!({ "rows": [] }));
    let mut assigned = json!({ "nested": { "value": 1 } });
    let change = tracker.begin_change();
    let rows = change
        .overlay()
        .unwrap()
        .get("rows")
        .unwrap()
        .as_overlay()
        .unwrap()
        .clone();
    rows.push(&[DraftInput::from(assigned.clone())]).unwrap();
    assigned["nested"]["value"] = json!(9);

    assert_eq!(
        rows.get_index(0)
            .unwrap()
            .as_overlay()
            .unwrap()
            .get("nested")
            .unwrap()
            .as_overlay()
            .unwrap()
            .get("value")
            .unwrap()
            .as_json()
            .unwrap(),
        &json!(1)
    );
    let prepared = change.prepare().unwrap();
    assert_eq!(prepared.value()["rows"][0]["nested"]["value"], 1);
    let replica = apply(Some(prepared.base().clone()), prepared.ops()).unwrap();
    assert_eq!(replica, *prepared.value());
}
