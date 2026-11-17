//! Port of packages/chord/test/delta-diff.test.ts

use pi_chord::delta::{
    AppendVerb, DeleteVerb, MoveVerb, Op, OpAppend, OpDelete, OpMove, OpReplace, OpSet, OpSplice, OpTruncate,
    ReplaceVerb, Seg, SetVerb, SpliceVerb, TruncateVerb, apply_immutable, assert_valid_op, assert_valid_wire_op,
    decoder, diff_revisions, encoder,
};
use serde_json::{Map, Value, json};

fn expect_diff(before: Value, after: Value, expected: &[Op]) {
    let ops = diff_revisions(&before, &after);
    assert_eq!(ops, expected);
    assert_eq!(apply_immutable(Some(before), &ops).unwrap(), after);
}

#[test]
fn emits_sets_and_deletes() {
    expect_diff(
        json!({ "keep": 1, "change": 1, "remove": true }),
        json!({ "keep": 1, "change": 2, "add": 3 }),
        &[
            Op::Set(OpSet(SetVerb::S, vec![Seg::String("change".to_owned())], json!(2))),
            Op::Set(OpSet(SetVerb::S, vec![Seg::String("add".to_owned())], json!(3))),
            Op::Delete(OpDelete(DeleteVerb::D, vec![Seg::String("remove".to_owned())])),
        ],
    );
}

#[test]
fn emits_string_append_and_front_truncation() {
    expect_diff(
        json!({ "text": "hello" }),
        json!({ "text": "hello world" }),
        &[Op::Append(OpAppend(
            AppendVerb::A,
            vec![Seg::String("text".to_owned())],
            " world".to_owned(),
        ))],
    );
    expect_diff(
        json!({ "text": "hello world" }),
        json!({ "text": "world" }),
        &[Op::Truncate(OpTruncate(
            TruncateVerb::T,
            vec![Seg::String("text".to_owned())],
            6,
        ))],
    );
    expect_diff(
        json!({ "text": "abcdefgh" }),
        json!({ "text": "defghxyz" }),
        &[
            Op::Truncate(OpTruncate(TruncateVerb::T, vec![Seg::String("text".to_owned())], 3)),
            Op::Append(OpAppend(
                AppendVerb::A,
                vec![Seg::String("text".to_owned())],
                "xyz".to_owned(),
            )),
        ],
    );
}

#[test]
fn represents_array_insertion_removal_shift_and_queue_with_splices() {
    let a = json!({ "id": "a" });
    let b = json!({ "id": "b" });
    let c = json!({ "id": "c" });
    let d = json!({ "id": "d" });
    expect_diff(
        json!({ "values": [a, b] }),
        json!({ "values": [a, c, b] }),
        &[Op::Splice(OpSplice(
            SpliceVerb::P,
            vec![Seg::String("values".to_owned())],
            1,
            0,
            vec![c.clone()],
        ))],
    );
    expect_diff(
        json!({ "values": [a, b, c] }),
        json!({ "values": [b, c] }),
        &[Op::Splice(OpSplice(
            SpliceVerb::P,
            vec![Seg::String("values".to_owned())],
            0,
            1,
            vec![],
        ))],
    );
    expect_diff(
        json!({ "values": [a, b, c] }),
        json!({ "values": [b, c, d] }),
        &[
            Op::Splice(OpSplice(
                SpliceVerb::P,
                vec![Seg::String("values".to_owned())],
                0,
                1,
                vec![],
            )),
            Op::Splice(OpSplice(
                SpliceVerb::P,
                vec![Seg::String("values".to_owned())],
                2,
                0,
                vec![d],
            )),
        ],
    );
}

#[test]
fn emits_permutation_and_normalizes_deeply_equal_reorder() {
    let a = json!({ "id": "a" });
    let b = json!({ "id": "b" });
    let c = json!({ "id": "c" });
    expect_diff(
        json!({ "values": [a, b, c] }),
        json!({ "values": [c, a, b] }),
        &[Op::Move(OpMove(
            MoveVerb::M,
            vec![Seg::String("values".to_owned())],
            vec![2, 0, 1],
        ))],
    );

    let first = json!({ "nested": { "value": 1 } });
    let second = json!({ "nested": { "value": 1 } });
    assert!(
        diff_revisions(
            &json!({ "values": [first.clone(), second.clone()] }),
            &json!({ "values": [second, first] })
        )
        .is_empty()
    );
}

#[test]
fn validates_and_encodes_permutations() {
    let ops = vec![
        Op::Move(OpMove(
            MoveVerb::M,
            vec![Seg::String("values".to_owned())],
            vec![2, 0, 1],
        )),
        Op::Move(OpMove(
            MoveVerb::M,
            vec![Seg::String("values".to_owned())],
            vec![1, 2, 0],
        )),
    ];
    for op in &ops {
        assert_valid_op(&serde_json::to_value(op).unwrap()).unwrap();
    }
    let wire = encoder().encode(&ops);
    assert_eq!(decoder().decode(&wire).unwrap(), ops);
    assert_valid_wire_op(&json!(["m", [1, 2, 0]])).unwrap();
    assert!(
        assert_valid_op(&json!(["m", ["values"], [0, 0]]))
            .unwrap_err()
            .message()
            .contains("bijection")
    );
}

#[test]
fn emits_nothing_for_deeply_equal_reconstructed_values_and_keeps_leaf_edit_narrow() {
    assert!(
        diff_revisions(
            &json!({ "value": { "nested": [1, 2] } }),
            &json!({ "value": { "nested": [1, 2] } })
        )
        .is_empty()
    );
    assert!(
        diff_revisions(
            &json!({ "values": [{ "id": 1 }, { "id": 2 }] }),
            &json!({ "values": [{ "id": 1 }, { "id": 2 }] })
        )
        .is_empty()
    );
    assert!(
        diff_revisions(
            &json!({ "values": [true, true, true] }),
            &json!({ "values": [true, true, true] })
        )
        .is_empty()
    );

    expect_diff(
        json!({ "values": [{ "id": 1, "label": "one" }, { "id": 2, "label": "two" }] }),
        json!({ "values": [{ "id": 1, "label": "one" }, { "id": 2, "label": "changed" }] }),
        &[Op::Set(OpSet(
            SetVerb::S,
            vec![
                Seg::String("values".to_owned()),
                Seg::Index(1),
                Seg::String("label".to_owned()),
            ],
            json!("changed"),
        ))],
    );
}

#[test]
fn emits_payload_free_splices_for_scattered_and_canonical_removals() {
    let [a, b, c, d, e] = ["a", "b", "c", "d", "e"].map(|id| json!({ "id": id }));
    expect_diff(
        json!({ "values": [a, b, c, d, e] }),
        json!({ "values": [a, c, e] }),
        &[
            Op::Splice(OpSplice(
                SpliceVerb::P,
                vec![Seg::String("values".to_owned())],
                1,
                1,
                vec![],
            )),
            Op::Splice(OpSplice(
                SpliceVerb::P,
                vec![Seg::String("values".to_owned())],
                2,
                1,
                vec![],
            )),
        ],
    );
    expect_diff(
        json!({ "values": [1, 2, 3, 4] }),
        json!({ "values": [2, 3, 4] }),
        &[Op::Splice(OpSplice(
            SpliceVerb::P,
            vec![Seg::String("values".to_owned())],
            0,
            1,
            vec![],
        ))],
    );
    expect_diff(
        json!({ "values": [1, 2, 3, 4] }),
        json!({ "values": [1, 2, 3] }),
        &[Op::Splice(OpSplice(
            SpliceVerb::P,
            vec![Seg::String("values".to_owned())],
            3,
            1,
            vec![],
        ))],
    );
    expect_diff(
        json!({ "values": [1, 2, 3, 4] }),
        json!({ "values": [1, 3, 4] }),
        &[Op::Splice(OpSplice(
            SpliceVerb::P,
            vec![Seg::String("values".to_owned())],
            1,
            1,
            vec![],
        ))],
    );
    expect_diff(
        json!({ "values": [1, 2, 3, 4] }),
        json!({ "values": [] }),
        &[Op::Splice(OpSplice(
            SpliceVerb::P,
            vec![Seg::String("values".to_owned())],
            0,
            4,
            vec![],
        ))],
    );
    expect_diff(
        json!({ "values": [1, 2, 3, 4] }),
        json!({ "values": [1, 2, 3, 4] }),
        &[],
    );
}

#[test]
fn does_not_field_diff_unrelated_shifted_objects_or_coincidental_ids() {
    let before: Vec<Value> = (1..=3).map(|value| json!({ "type": "row", "value": value })).collect();
    let after: Vec<Value> = (2..=4).map(|value| json!({ "type": "row", "value": value })).collect();
    expect_diff(
        json!({ "values": before }),
        json!({ "values": after }),
        &[
            Op::Splice(OpSplice(
                SpliceVerb::P,
                vec![Seg::String("values".to_owned())],
                0,
                1,
                vec![],
            )),
            Op::Splice(OpSplice(
                SpliceVerb::P,
                vec![Seg::String("values".to_owned())],
                2,
                0,
                vec![json!({ "type": "row", "value": 4 })],
            )),
        ],
    );

    let left = json!({ "value": "left" });
    let right = json!({ "value": "right" });
    let before =
        json!([left, { "id": 1, "key": "a", "value": "first" }, { "id": 2, "key": "b", "value": "second" }, right]);
    let replacements = vec![
        json!({ "id": 2, "key": "b", "value": "edited-second" }),
        json!({ "id": 1, "key": "a", "value": "edited-first" }),
    ];
    let after = json!([left, replacements[0], replacements[1], right]);
    expect_diff(
        json!({ "values": before }),
        json!({ "values": after }),
        &[Op::Splice(OpSplice(
            SpliceVerb::P,
            vec![Seg::String("values".to_owned())],
            1,
            2,
            replacements,
        ))],
    );
}

#[test]
fn combines_removals_append_and_shared_subtree_survivor_edit() {
    // PORT: In Rust `JsonValue` has no JS reference identity, so `{}` on `b` and `c` would be
    // structurally equal. Use distinct non-empty `stable` subtrees to test `semantically_aligned`.
    let a = json!({ "id": "a", "stable": { "k": "a" }, "detail": { "text": "a" } });
    let b = json!({ "id": "b", "stable": { "k": "b" }, "detail": { "text": "b" } });
    let c = json!({ "id": "c", "stable": { "k": "c" }, "detail": { "text": "c" } });
    let d = json!({ "id": "d", "stable": { "k": "d" }, "detail": { "text": "d" } });
    let changed_c = json!({ "id": "c", "stable": c["stable"], "detail": { "text": "changed" } });
    let appended = json!({ "id": "e", "stable": { "k": "e" }, "detail": { "text": "e" } });
    expect_diff(
        json!({ "values": [a, b, c, d] }),
        json!({ "values": [a, changed_c, d, appended] }),
        &[
            Op::Splice(OpSplice(
                SpliceVerb::P,
                vec![Seg::String("values".to_owned())],
                1,
                1,
                vec![],
            )),
            Op::Append(OpAppend(
                AppendVerb::A,
                vec![
                    Seg::String("values".to_owned()),
                    Seg::Index(1),
                    Seg::String("detail".to_owned()),
                    Seg::String("text".to_owned()),
                ],
                "hanged".to_owned(),
            )),
            Op::Splice(OpSplice(
                SpliceVerb::P,
                vec![Seg::String("values".to_owned())],
                3,
                0,
                vec![appended],
            )),
        ],
    );
}

#[test]
fn does_not_retain_large_removed_neighbor_payload() {
    for size in [256 * 1024, 1024 * 1024] {
        let payload = "x".repeat(size);
        let retained: Vec<Value> = ["a", "b", "c", "d", "e"]
            .iter()
            .map(|id| json!({ "id": id, "payload": payload }))
            .collect();
        let before = json!({ "values": retained.clone() });
        let after = json!({ "values": [retained[0], retained[2], retained[4]] });
        let ops = diff_revisions(&before, &after);
        assert_eq!(
            ops,
            vec![
                Op::Splice(OpSplice(
                    SpliceVerb::P,
                    vec![Seg::String("values".to_owned())],
                    1,
                    1,
                    vec![]
                )),
                Op::Splice(OpSplice(
                    SpliceVerb::P,
                    vec![Seg::String("values".to_owned())],
                    2,
                    1,
                    vec![]
                )),
            ]
        );
        assert!(pi_js::json::stringify(&ops).len() < 100);
        assert_eq!(apply_immutable(Some(before), &ops).unwrap(), after);
    }
}

#[test]
fn keeps_large_push_pop_middle_removal_sparse_leaf_rotation_and_unshift_narrow() {
    for size in [1_001i64, 10_000i64] {
        let values: Vec<Value> = (0..size).map(|v| json!({ "value": v })).collect();
        let appended = json!({ "value": size });
        let mut pushed = values.clone();
        pushed.push(appended.clone());
        expect_diff(
            json!({ "values": values }),
            json!({ "values": pushed }),
            &[Op::Splice(OpSplice(
                SpliceVerb::P,
                vec![Seg::String("values".to_owned())],
                size,
                0,
                vec![appended],
            ))],
        );
        let popped = values[..values.len() - 1].to_vec();
        expect_diff(
            json!({ "values": values }),
            json!({ "values": popped }),
            &[Op::Splice(OpSplice(
                SpliceVerb::P,
                vec![Seg::String("values".to_owned())],
                size - 1,
                1,
                vec![],
            ))],
        );
        let mid = size / 2;
        let mut removed_mid = values.clone();
        removed_mid.drain(mid as usize..=mid as usize);
        expect_diff(
            json!({ "values": values }),
            json!({ "values": removed_mid }),
            &[Op::Splice(OpSplice(
                SpliceVerb::P,
                vec![Seg::String("values".to_owned())],
                mid,
                1,
                vec![],
            ))],
        );
    }

    let values: Vec<Value> = (0..40_000i64)
        .map(|v| json!({ "value": v, "stable": { "value": v } }))
        .collect();
    let mut after = values.clone();
    let mut expected = Vec::new();
    for index in (100..40_000i64).step_by(400) {
        after[index as usize] = json!({ "value": -index, "stable": values[index as usize]["stable"] });
        expected.push(Op::Set(OpSet(
            SetVerb::S,
            vec![
                Seg::String("values".to_owned()),
                Seg::Index(index),
                Seg::String("value".to_owned()),
            ],
            json!(-index),
        )));
    }
    let ops = diff_revisions(&json!({ "values": values }), &json!({ "values": after }));
    assert_eq!(ops, expected);
    assert!(pi_js::json::stringify(&ops).len() < 7_500);
    assert_eq!(
        apply_immutable(Some(json!({ "values": values })), &ops).unwrap(),
        json!({ "values": after })
    );

    let before_1k: Vec<Value> = (0..1_000i64)
        .map(|v| json!({ "value": v, "label": format!("row-{v}") }))
        .collect();
    let mut after_1k = before_1k.clone();
    after_1k[700]["label"] = json!("changed");
    let ops = diff_revisions(&json!({ "values": before_1k }), &json!({ "values": after_1k }));
    assert_eq!(
        ops,
        vec![Op::Set(OpSet(
            SetVerb::S,
            vec![
                Seg::String("values".to_owned()),
                Seg::Index(700),
                Seg::String("label".to_owned())
            ],
            json!("changed")
        ))]
    );
    assert!(pi_js::json::stringify(&ops).len() < 100);
    assert_eq!(
        apply_immutable(Some(json!({ "values": before_1k })), &ops).unwrap(),
        json!({ "values": after_1k })
    );

    let left = json!({ "value": "left" });
    let right = json!({ "value": "right" });
    let replacements = vec![
        json!({ "id": 2, "value": "edited" }),
        json!({ "id": 1, "value": "also-edited" }),
    ];
    expect_diff(
        json!({ "values": [left, { "id": 1, "value": "a" }, { "id": 2, "value": "b" }, right] }),
        json!({ "values": [left, replacements[0], replacements[1], right] }),
        &[Op::Splice(OpSplice(
            SpliceVerb::P,
            vec![Seg::String("values".to_owned())],
            1,
            2,
            replacements,
        ))],
    );

    let rot_values: Vec<Value> = (0..10_000i64).map(|v| json!({ "value": v })).collect();
    let mut rotated = rot_values[1_000..].to_vec();
    rotated.extend_from_slice(&rot_values[..1_000]);
    let ops = diff_revisions(&json!({ "values": rot_values }), &json!({ "values": rotated }));
    assert_eq!(ops.len(), 1);
    assert!(matches!(ops[0], Op::Move(_)));
    assert!(pi_js::json::stringify(&ops).len() < 60_000);
    assert_eq!(
        apply_immutable(Some(json!({ "values": rot_values })), &ops).unwrap(),
        json!({ "values": rotated })
    );

    let pad = "x".repeat(100);
    let retained: Vec<Value> = (0..10_000i64).map(|v| json!({ "value": v, "payload": pad })).collect();
    let inserted: Vec<Value> = (0..500i64).map(|v| json!({ "value": -v - 1 })).collect();
    let mut unshifted = inserted.clone();
    unshifted.extend_from_slice(&retained);
    let ops = diff_revisions(&json!({ "values": retained }), &json!({ "values": unshifted }));
    assert_eq!(
        ops,
        vec![Op::Splice(OpSplice(
            SpliceVerb::P,
            vec![Seg::String("values".to_owned())],
            0,
            0,
            inserted
        ))]
    );
    assert!(pi_js::json::stringify(&ops).len() < 20_000);
    assert_eq!(
        apply_immutable(Some(json!({ "values": retained })), &ops).unwrap(),
        json!({ "values": unshifted })
    );
}

#[test]
fn bounds_wide_object_and_wide_normalized_array_with_root_replacement() {
    let mut before_map = Map::new();
    let mut after_map = Map::new();
    for index in 0..20_000 {
        let key = format!("field{index}");
        before_map.insert(key.clone(), json!(0));
        after_map.insert(key, json!(1));
    }
    let before = Value::Object(before_map);
    let after = Value::Object(after_map);
    assert_eq!(
        diff_revisions(&before, &after),
        vec![Op::Replace(OpReplace(ReplaceVerb::R, after))]
    );

    let before = json!({ "values": vec![0; 40_000] });
    let after = json!({ "values": vec![1; 40_000] });
    let ops = diff_revisions(&before, &after);
    assert_eq!(ops, vec![Op::Replace(OpReplace(ReplaceVerb::R, after.clone()))]);
    assert_eq!(apply_immutable(Some(before), &ops).unwrap(), after);
}
