//! Port of packages/chord/test/delta-apply-immutable.test.ts

use pi_chord::delta::tracker::DraftInput;
use pi_chord::delta::{
    AppendVerb, DeleteVerb, MoveVerb, Op, OpAppend, OpDelete, OpMove, OpReplace, OpSet, OpSplice, OpTruncate,
    PathError, ReplaceVerb, Seg, SetVerb, SpliceVerb, TruncateVerb, UnsafePathError, apply, apply_immutable,
    apply_immutable_batches, track,
};
use serde_json::{Map, Value, json};

#[test]
fn copies_each_touched_container_once_while_preserving_input_and_payload_ownership() {
    let shared = json!({ "nested": { "value": 1 } });
    let untouched = json!({ "value": 9 });
    let row_payload = json!({ "id": 4, "label": "placed" });
    let base = json!({
        "text": "abcdef",
        "stable": { "value": 7 },
        "branch": { "value": 1 },
        "copy": null,
        "placed": null,
        "untouched": null,
        "left": null,
        "right": null,
        "meta": { "count": 0, "obsolete": true },
        "rows": [
            { "id": 1, "label": "one" },
            { "id": 2, "label": "two" },
            { "id": 3, "label": "three" },
        ],
    });
    let operations = vec![
        Op::Truncate(OpTruncate(TruncateVerb::T, vec![Seg::String("text".to_owned())], 2)),
        Op::Append(OpAppend(
            AppendVerb::A,
            vec![Seg::String("text".to_owned())],
            "!".to_owned(),
        )),
        Op::Set(OpSet(
            SetVerb::S,
            vec![Seg::String("meta".to_owned()), Seg::String("count".to_owned())],
            json!(1),
        )),
        Op::Set(OpSet(
            SetVerb::S,
            vec![Seg::String("meta".to_owned()), Seg::String("count".to_owned())],
            json!(2),
        )),
        Op::Delete(OpDelete(
            DeleteVerb::D,
            vec![Seg::String("meta".to_owned()), Seg::String("obsolete".to_owned())],
        )),
        Op::Set(OpSet(
            SetVerb::S,
            vec![Seg::String("copy".to_owned())],
            base["branch"].clone(),
        )),
        Op::Set(OpSet(
            SetVerb::S,
            vec![Seg::String("copy".to_owned()), Seg::String("value".to_owned())],
            json!(2),
        )),
        Op::Set(OpSet(
            SetVerb::S,
            vec![Seg::String("placed".to_owned())],
            shared.clone(),
        )),
        Op::Set(OpSet(
            SetVerb::S,
            vec![
                Seg::String("placed".to_owned()),
                Seg::String("nested".to_owned()),
                Seg::String("value".to_owned()),
            ],
            json!(2),
        )),
        Op::Set(OpSet(
            SetVerb::S,
            vec![Seg::String("untouched".to_owned())],
            untouched.clone(),
        )),
        Op::Set(OpSet(SetVerb::S, vec![Seg::String("left".to_owned())], shared.clone())),
        Op::Set(OpSet(SetVerb::S, vec![Seg::String("right".to_owned())], shared.clone())),
        Op::Set(OpSet(
            SetVerb::S,
            vec![
                Seg::String("left".to_owned()),
                Seg::String("nested".to_owned()),
                Seg::String("value".to_owned()),
            ],
            json!(3),
        )),
        Op::Splice(OpSplice(
            SpliceVerb::P,
            vec![Seg::String("rows".to_owned())],
            1,
            1,
            vec![row_payload.clone()],
        )),
        Op::Set(OpSet(
            SetVerb::S,
            vec![
                Seg::String("rows".to_owned()),
                Seg::Index(1),
                Seg::String("label".to_owned()),
            ],
            json!("edited"),
        )),
        Op::Move(OpMove(MoveVerb::M, vec![Seg::String("rows".to_owned())], vec![1, 0, 2])),
        Op::Set(OpSet(
            SetVerb::S,
            vec![
                Seg::String("rows".to_owned()),
                Seg::Index(0),
                Seg::String("label".to_owned()),
            ],
            json!("moved"),
        )),
    ];

    let result = apply_immutable(Some(base.clone()), &operations).unwrap();
    let mutable_result = apply(Some(base.clone()), &operations).unwrap();

    assert_eq!(result, mutable_result);
    assert_eq!(result["text"], "cdef!");
    // PORT: `JsonValue` owns values rather than JS pointer identity; verify equality and input non-mutation.
    assert_eq!(result["stable"], base["stable"]);
    assert_eq!(result["untouched"], untouched);
    assert_eq!(result["copy"], json!({ "value": 2 }));
    assert_eq!(result["placed"], json!({ "nested": { "value": 2 } }));
    assert_eq!(result["right"], shared);
    assert_eq!(result["rows"][0], json!({ "id": 4, "label": "moved" }));
    assert_eq!(base["branch"]["value"], 1);
    assert_eq!(shared["nested"]["value"], 1);
    assert_eq!(row_payload["label"], "placed");
}

#[test]
fn protects_root_replacement_payloads_before_later_object_and_array_edits() {
    let replacement = json!({ "nested": { "value": 1 }, "values": [1, 2, 3] });
    let batches: Vec<Vec<Op>> = vec![
        vec![Op::Replace(OpReplace(ReplaceVerb::R, replacement.clone()))],
        vec![Op::Set(OpSet(
            SetVerb::S,
            vec![Seg::String("nested".to_owned()), Seg::String("value".to_owned())],
            json!(2),
        ))],
        vec![
            Op::Splice(OpSplice(
                SpliceVerb::P,
                vec![Seg::String("values".to_owned())],
                1,
                1,
                vec![json!(4), json!(5)],
            )),
            Op::Move(OpMove(
                MoveVerb::M,
                vec![Seg::String("values".to_owned())],
                vec![3, 0, 1, 2],
            )),
        ],
    ];
    let result = apply_immutable_batches::<Value, _, _>(None, &batches).unwrap();
    assert_eq!(result, json!({ "nested": { "value": 2 }, "values": [3, 1, 4, 5] }));
    assert_eq!(replacement, json!({ "nested": { "value": 1 }, "values": [1, 2, 3] }));

    let array = json!([1, 2, 3]);
    let array_result = apply_immutable(
        Some(array.clone()),
        &[
            Op::Splice(OpSplice(SpliceVerb::P, vec![], 1, 1, vec![json!(4), json!(5)])),
            Op::Move(OpMove(MoveVerb::M, vec![], vec![3, 0, 1, 2])),
            Op::Delete(OpDelete(DeleteVerb::D, vec![Seg::Index(1)])),
        ],
    )
    .unwrap();
    assert_eq!(array_result, json!([3, 4, 5]));
    assert_eq!(array, json!([1, 2, 3]));
}

#[test]
fn shares_one_copy_on_write_scope_across_batch_partitions() {
    let base = json!({
        "text": "abcdef",
        "meta": { "count": 0 },
        "values": [
            { "id": 1, "value": 1 },
            { "id": 2, "value": 2 },
            { "id": 3, "value": 3 },
        ],
    });
    let batches: Vec<Vec<Op>> = vec![
        vec![
            Op::Set(OpSet(
                SetVerb::S,
                vec![Seg::String("meta".to_owned()), Seg::String("count".to_owned())],
                json!(1),
            )),
            Op::Splice(OpSplice(
                SpliceVerb::P,
                vec![Seg::String("values".to_owned())],
                1,
                1,
                vec![json!({ "id": 4, "value": 4 })],
            )),
        ],
        vec![],
        vec![
            Op::Move(OpMove(
                MoveVerb::M,
                vec![Seg::String("values".to_owned())],
                vec![2, 0, 1],
            )),
            Op::Set(OpSet(
                SetVerb::S,
                vec![
                    Seg::String("values".to_owned()),
                    Seg::Index(2),
                    Seg::String("value".to_owned()),
                ],
                json!(40),
            )),
        ],
        vec![
            Op::Truncate(OpTruncate(TruncateVerb::T, vec![Seg::String("text".to_owned())], 2)),
            Op::Append(OpAppend(
                AppendVerb::A,
                vec![Seg::String("text".to_owned())],
                "!".to_owned(),
            )),
        ],
    ];

    let intermediate = apply_immutable(Some(base.clone()), &batches[0]).unwrap();
    let intermediate_snapshot = intermediate.clone();
    let mut sequential = intermediate.clone();
    for batch in &batches[1..] {
        sequential = apply_immutable(Some(sequential), batch).unwrap();
    }
    let streamed = apply_immutable_batches(Some(base.clone()), &batches).unwrap();
    let flattened_ops: Vec<Op> = batches.iter().flatten().cloned().collect();
    let flattened = apply_immutable(Some(base.clone()), &flattened_ops).unwrap();

    assert_eq!(streamed, sequential);
    assert_eq!(streamed, flattened);
    assert_eq!(intermediate, intermediate_snapshot);
    assert_eq!(base["meta"]["count"], 0);
    assert_eq!(base["values"][1]["id"], 2);
}

#[test]
fn handles_tracker_produced_batches_across_arbitrary_revision_boundaries() {
    let initial_values: Vec<Value> = (0..8).map(|id| json!({ "id": id, "score": 0 })).collect();
    let initial = json!({
        "text": "start",
        "values": initial_values,
        "revision": 0,
    });
    let tracker = track(initial.clone());
    let mut batches: Vec<Vec<Op>> = Vec::new();
    for revision in 1..=40i64 {
        let change = tracker.begin_change();
        let root = change.overlay().unwrap();
        root.set("revision", revision).unwrap();
        let old_text = root
            .get("text")
            .unwrap()
            .as_json()
            .unwrap()
            .as_str()
            .unwrap()
            .to_owned();
        root.set("text", format!("{}{}", &old_text[1..], revision)).unwrap();
        let values = root.get("values").unwrap().as_overlay().unwrap().clone();
        match revision % 5 {
            0 => {
                values.reverse().unwrap();
            }
            1 => {
                values
                    .push(&[DraftInput::from(json!({ "id": 100 + revision, "score": revision }))])
                    .unwrap();
            }
            2 => {
                values.shift().unwrap();
            }
            3 => {
                let len = values.len().unwrap() as i64;
                let item = values.get_index(revision % len).unwrap().as_overlay().unwrap().clone();
                item.set("score", revision).unwrap();
            }
            _ => {
                values
                    .splice(
                        Some(1),
                        Some(1),
                        &[DraftInput::from(json!({ "id": 200 + revision, "score": revision }))],
                    )
                    .unwrap();
            }
        }
        let prepared = change.prepare().unwrap();
        batches.push(prepared.ops().to_vec());
        tracker.adopt(&prepared).unwrap();
    }

    assert_eq!(
        apply_immutable_batches(Some(initial), &batches).unwrap(),
        tracker.value()
    );
}

#[test]
fn rejects_unsafe_or_unresolvable_paths_without_partial_application() {
    let base = json!({ "nested": { "value": 1 } });
    let invalid_batches = vec![
        vec![Op::Set(OpSet(
            SetVerb::S,
            vec![Seg::String("nested".to_owned()), Seg::String("value".to_owned())],
            json!(2),
        ))],
        vec![Op::Set(OpSet(
            SetVerb::S,
            vec![
                Seg::String("constructor".to_owned()),
                Seg::String("prototype".to_owned()),
                Seg::String("polluted".to_owned()),
            ],
            json!(true),
        ))],
    ];
    assert!(apply_immutable_batches(Some(base.clone()), &invalid_batches).is_err());
    assert_eq!(base["nested"]["value"], 1);

    let err = apply_immutable(
        Some(json!({ "values": [] })),
        &[Op::Set(OpSet(
            SetVerb::S,
            vec![
                Seg::String("values".to_owned()),
                Seg::String("missing".to_owned()),
                Seg::String("value".to_owned()),
            ],
            json!(1),
        ))],
    )
    .unwrap_err();
    assert!(err.downcast_ref::<PathError>().is_some() || err.name() == "PathError");

    let err = apply_immutable(
        Some(json!({ "values": [{}] })),
        &[Op::Set(OpSet(
            SetVerb::S,
            vec![
                Seg::String("values".to_owned()),
                Seg::String("0".to_owned()),
                Seg::String("value".to_owned()),
            ],
            json!(1),
        ))],
    )
    .unwrap_err();
    assert!(err.downcast_ref::<UnsafePathError>().is_some() || err.name() == "UnsafePathError");
}

#[test]
fn allows_one_immutable_batch_to_fan_out_without_mutating_shared_payloads() {
    let payload = json!({ "nested": { "value": 1 } });
    let ops = vec![
        Op::Set(OpSet(
            SetVerb::S,
            vec![Seg::String("placed".to_owned())],
            payload.clone(),
        )),
        Op::Set(OpSet(
            SetVerb::S,
            vec![
                Seg::String("placed".to_owned()),
                Seg::String("nested".to_owned()),
                Seg::String("value".to_owned()),
            ],
            json!(2),
        )),
    ];
    let base = json!({ "placed": null });
    let first = apply_immutable(Some(base.clone()), &ops).unwrap();
    let second = apply_immutable(Some(base), &ops).unwrap();
    assert_eq!(first, second);
    assert_eq!(first["placed"], json!({ "nested": { "value": 2 } }));
    assert_eq!(payload["nested"]["value"], 1);
}

#[test]
fn copies_wide_object_efficiently_for_many_writes() {
    let mut map = Map::new();
    for index in 0..20_000 {
        map.insert(format!("field{index}"), json!(index));
    }
    let base = Value::Object(map);
    let ops: Vec<Op> = (0..1_000i64)
        .map(|index| {
            Op::Set(OpSet(
                SetVerb::S,
                vec![Seg::String(format!("field{index}"))],
                json!(-index),
            ))
        })
        .collect();
    let result = apply_immutable(Some(base.clone()), &ops).unwrap();
    assert_eq!(result["field999"], -999);
    assert_eq!(base["field999"], 999);
}
