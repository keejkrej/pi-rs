//! Port of packages/chord/test/state-value.test.ts

use pi_chord::delta::tracker::{DraftInput, track};
use serde_json::json;

#[test]
fn takes_ownership_of_an_alias_free_mutable_json_root_without_freezing() {
    // PORT: Rust `serde_json::Value` owns independent allocations without JS `Object.freeze` or reference aliasing.
    let input = json!({
        "left": { "value": 1 },
        "right": { "value": 1 },
    });
    let tracker = track(input.clone());
    assert_eq!(tracker.value(), input);
    assert_ne!(
        tracker.value()["left"].as_object().unwrap() as *const _,
        tracker.value()["right"].as_object().unwrap() as *const _,
    );

    // PORT: Also verify transactional mutability via `begin_change` + `prepare_with` for `Draft<T> = T`.
    let change = tracker.begin_change();
    let mut next = change.state().unwrap();
    next["left"]["value"] = json!(2);
    let prepared = change.prepare_with(next).unwrap();
    assert_eq!(prepared.value()["left"]["value"], json!(2));
    assert_eq!(prepared.value()["right"]["value"], json!(1));
}

#[test]
fn commits_transaction_copies_in_place_while_sharing_unchanged_branches() {
    let tracker = track(json!({
        "changed": { "value": 1 },
        "retained": { "value": 2 },
    }));
    let base = tracker.value();
    let base_snapshot = base.clone();

    let change = tracker.begin_change();
    let overlay = change.overlay().unwrap();
    overlay
        .get("changed")
        .unwrap()
        .as_overlay()
        .unwrap()
        .set("value", 3_i64)
        .unwrap();
    let prepared = change.prepare().unwrap();

    assert_ne!(prepared.value()["changed"], base["changed"]);
    assert_eq!(prepared.value()["retained"], base["retained"]);
    assert_eq!(prepared.value()["changed"]["value"], json!(3));

    tracker.adopt(&prepared).unwrap();
    assert_eq!(base, base_snapshot);
    assert_eq!(tracker.value(), *prepared.value());
}

#[test]
fn makes_repeated_placements_independent() {
    let tracker = track(json!({ "values": [] }));
    let change = tracker.begin_change();
    let shared = json!({ "value": 1 });

    let values = change
        .overlay()
        .unwrap()
        .get("values")
        .unwrap()
        .as_overlay()
        .unwrap()
        .clone();
    values
        .push(&[DraftInput::from(shared.clone()), DraftInput::from(shared)])
        .unwrap();

    // Mutate only slot 0 inside the overlay to prove the two placements do not alias.
    values
        .get_index(0)
        .unwrap()
        .as_overlay()
        .unwrap()
        .set("value", 2_i64)
        .unwrap();
    let prepared = change.prepare().unwrap();

    assert_eq!(prepared.value()["values"], json!([{ "value": 2 }, { "value": 1 }]),);
    assert_ne!(
        prepared.value()["values"][0].as_object().unwrap() as *const _,
        prepared.value()["values"][1].as_object().unwrap() as *const _,
    );
}
