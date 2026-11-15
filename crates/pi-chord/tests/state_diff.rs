//! Port of packages/chord/test/state-diff.test.ts

use pi_chord::delta::{
    AppendVerb, DeleteVerb, MoveVerb, Op, OpAppend, OpDelete, OpMove, OpReplace, OpSet, OpSplice, OpTruncate,
    ReplaceVerb, Seg, SetVerb, SpliceVerb, TruncateVerb, apply_immutable, assert_valid_op, decoder, diff_revisions,
    encoder,
};
use serde_json::{Map, Value, json};

fn expect_diff(before: Value, after: Value, expected: &[Op]) {
    let ops = diff_revisions(&before, &after);
    assert_eq!(ops, expected);
    assert_eq!(apply_immutable(Some(before), &ops).unwrap(), after);
}

#[test]
fn emits_sets_deletes_and_string_operations() {
    expect_diff(
        json!({ "keep": 1, "change": 1, "remove": true }),
        json!({ "keep": 1, "change": 2, "add": 3 }),
        &[
            Op::Set(OpSet(SetVerb::S, vec![Seg::String("change".to_owned())], json!(2))),
            Op::Set(OpSet(SetVerb::S, vec![Seg::String("add".to_owned())], json!(3))),
            Op::Delete(OpDelete(DeleteVerb::D, vec![Seg::String("remove".to_owned())])),
        ],
    );
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
fn represents_array_edits_permutations_and_equal_normalizations() {
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
    assert!(
        assert_valid_op(&json!(["m", ["values"], [0, 0]]))
            .unwrap_err()
            .message()
            .contains("bijection")
    );
}

#[test]
fn handles_canonical_removals_unrelated_shifts_and_survivor_subtree_edit() {
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

    // PORT: Distinct non-empty `stable` subtrees preserve identity alignment without JS pointer aliasing.
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
fn scales_for_sparse_large_arrays_and_falls_back_for_wide_objects_and_base_arrays() {
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
    assert_eq!(
        diff_revisions(&before, &after),
        vec![Op::Replace(OpReplace(ReplaceVerb::R, after))]
    );
}
