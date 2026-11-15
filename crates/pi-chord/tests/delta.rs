//! Port of packages/chord/test/delta.test.ts

use pi_chord::delta::tracker::DraftInput;
use pi_chord::delta::{
    AppendVerb, DefineVerb, DeleteVerb, MoveVerb, Op, OpAppend, OpDelete, OpMove, OpReplace, OpSet, OpSplice,
    OpTruncate, PathError, PathRef, ReplaceVerb, Seg, SetVerb, SpliceVerb, TruncateVerb, UnsafePathError, WireOp,
    WireOpAppend, WireOpAppendShort, WireOpDefine, WireOpSet, WireOpSetShort, apply, apply_immutable, assert_valid_op,
    assert_valid_wire_op, decoder, encoder, is_base, overlap, track,
};
use serde_json::{Value, json};

#[test]
fn keeps_a_draft_open_across_steps_and_adopts_only_a_prepared_change() {
    let input = json!({ "count": 1, "nested": { "text": "a" }, "values": [1] });
    let tracker = track(input.clone());
    assert_eq!(tracker.value(), input);

    let change = tracker.begin_change();
    let overlay = change.overlay().expect("overlay should be active");
    overlay.set("count", 2).unwrap();
    let nested = overlay.get("nested").unwrap().as_overlay().unwrap().clone();
    let prev_text = nested
        .get("text")
        .unwrap()
        .as_json()
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned();
    nested.set("text", format!("{prev_text}b")).unwrap();
    let values = overlay.get("values").unwrap().as_overlay().unwrap().clone();
    values.push(&[DraftInput::from(2)]).unwrap();
    assert_eq!(tracker.value(), input);

    let prepared = change.prepare().unwrap();
    assert_eq!(prepared.base(), &input);
    assert_eq!(
        prepared.value(),
        &json!({ "count": 2, "nested": { "text": "ab" }, "values": [1, 2] })
    );
    assert_eq!(
        prepared.ops(),
        &[
            Op::Set(OpSet(SetVerb::S, vec![Seg::String("count".to_owned())], json!(2))),
            Op::Append(OpAppend(
                AppendVerb::A,
                vec![Seg::String("nested".to_owned()), Seg::String("text".to_owned())],
                "b".to_owned()
            )),
            Op::Splice(OpSplice(
                SpliceVerb::P,
                vec![Seg::String("values".to_owned())],
                1,
                0,
                vec![json!(2)]
            )),
        ]
    );
    let err = overlay.get("count").unwrap_err();
    assert_eq!(err.name(), "TypeError");
    assert_eq!(tracker.value(), input);

    tracker.adopt(&prepared).unwrap();
    assert_eq!(tracker.value(), *prepared.value());
}

#[test]
fn aborts_and_revokes_without_changing_committed_value() {
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
    child.set("value", 2).unwrap();
    change.abort();
    assert_eq!(tracker.value()["child"]["value"], 1);
    assert_eq!(child.get("value").unwrap_err().name(), "TypeError");
    change.abort();
    let err = change.prepare().err().unwrap();
    assert!(err.message().contains("settled"), "got: {err:?}");
    let next = tracker.begin_change();
    next.abort();
}

#[test]
fn grows_arrays_with_explicit_nulls_and_revokes_drafts_after_preparation() {
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
    values.set_len(4).unwrap();
    let prepared = change.prepare().unwrap();
    assert_eq!(prepared.value()["values"], json!([1, 2, null, null]));
    assert_eq!(values.len().unwrap_err().name(), "TypeError");
    tracker.adopt(&prepared).unwrap();
}

#[test]
fn normalizes_deep_noop_to_empty_ops() {
    let base = json!({ "value": { "nested": [1, 2] } });
    let tracker = track(base.clone());
    let change = tracker.begin_change();
    change
        .overlay()
        .unwrap()
        .set("value", json!({ "nested": [1, 2] }))
        .unwrap();
    let prepared = change.prepare().unwrap();
    assert_eq!(prepared.value(), prepared.base());
    assert!(prepared.ops().is_empty());
    tracker.adopt(&prepared).unwrap();
    assert_eq!(tracker.value(), base);
}

#[test]
fn takes_ownership_of_replacement_and_applies_noop_normalization() {
    let tracker = track(json!({ "nested": { "value": 1 } }));
    let replacement = json!({ "nested": { "value": 2 } });
    let prepared = tracker.prepare_replace(replacement.clone()).unwrap();
    assert_eq!(prepared.value(), &replacement);
    assert_eq!(
        prepared.ops(),
        &[Op::Replace(OpReplace(ReplaceVerb::R, replacement.clone()))]
    );
    tracker.adopt(&prepared).unwrap();

    let noop = tracker.prepare_replace(json!({ "nested": { "value": 2 } })).unwrap();
    assert_eq!(noop.value(), &tracker.value());
    assert!(noop.ops().is_empty());
}

#[test]
fn shares_operation_placements_with_prepared_candidate() {
    let tracker = track(json!({ "rows": [] }));
    let change = tracker.begin_change();
    let rows = change
        .overlay()
        .unwrap()
        .get("rows")
        .unwrap()
        .as_overlay()
        .unwrap()
        .clone();
    rows.push(&[DraftInput::from(json!({ "id": 1 }))]).unwrap();
    let prepared = change.prepare().unwrap();
    match &prepared.ops()[0] {
        Op::Splice(OpSplice(_, _, _, _, items)) => {
            assert_eq!(items[0], prepared.value()["rows"][0]);
        }
        other => panic!("expected splice, got {other:?}"),
    }
}

#[test]
fn rejects_foreign_stale_and_repeated_preparations_while_allowing_competing_changes() {
    let first = track(json!({ "value": 0 }));
    let second = track(json!({ "value": 0 }));
    let change = first.begin_change();
    let competing = first.begin_change();
    change.overlay().unwrap().set("value", 1).unwrap();
    competing.overlay().unwrap().set("value", 2).unwrap();
    let prepared = change.prepare().unwrap();
    let competing_prepared = competing.prepare().unwrap();
    assert!(
        second
            .adopt(&prepared)
            .unwrap_err()
            .message()
            .contains("different tracker")
    );
    first.adopt(&prepared).unwrap();
    assert!(
        first
            .adopt(&prepared)
            .unwrap_err()
            .message()
            .contains("already been used")
    );
    assert!(
        first
            .adopt(&competing_prepared)
            .unwrap_err()
            .message()
            .contains("stale")
    );

    let stale = first.prepare_replace(json!({ "value": 2 })).unwrap();
    let winner = first.prepare_replace(json!({ "value": 3 })).unwrap();
    first.adopt(&winner).unwrap();
    assert!(first.adopt(&stale).unwrap_err().message().contains("stale"));
    assert!(first.adopt(&stale).unwrap_err().message().contains("stale"));
}

#[test]
fn invalidates_prepared_result_when_change_is_aborted() {
    let tracker = track(json!({ "value": 0 }));
    let change = tracker.begin_change();
    change.overlay().unwrap().set("value", 1).unwrap();
    let prepared = change.prepare().unwrap();
    change.abort();
    assert!(tracker.adopt(&prepared).unwrap_err().message().contains("aborted"));
    change.abort();
}

#[test]
fn makes_competing_same_base_preparations_stale_after_adopting_noop() {
    let tracker = track(json!({ "value": { "count": 1 } }));
    let first = tracker.prepare_replace(json!({ "value": { "count": 1 } })).unwrap();
    let competing = tracker.prepare_replace(json!({ "value": { "count": 1 } })).unwrap();
    assert_eq!(first.value(), first.base());
    assert_eq!(competing.value(), competing.base());
    tracker.adopt(&first).unwrap();
    assert!(
        tracker
            .adopt(&first)
            .unwrap_err()
            .message()
            .contains("already been used")
    );
    assert!(tracker.adopt(&competing).unwrap_err().message().contains("stale"));
}

#[test]
fn emits_owned_root_replacement_for_large_replacement_input() {
    let rows: Vec<Value> = (0..10_000)
        .map(|value| json!({ "value": value, "stable": { "value": value } }))
        .collect();
    let tracker = track(json!({ "rows": rows }));
    let mut rep_rows = tracker.value()["rows"].as_array().unwrap().clone();
    rep_rows[5_000] = json!({ "value": -1, "stable": rep_rows[5_000]["stable"].clone() });
    let replacement = json!({ "rows": rep_rows });
    let prepared = tracker.prepare_replace(replacement.clone()).unwrap();
    assert_eq!(prepared.value(), &replacement);
    assert_eq!(prepared.ops(), &[Op::Replace(OpReplace(ReplaceVerb::R, replacement))]);
    assert_eq!(
        apply_immutable(Some(prepared.base().clone()), prepared.ops()).unwrap(),
        *prepared.value()
    );
}

#[test]
fn emits_canonical_string_append_and_rolling_window_operations_and_bounded_overlaps() {
    let tracker = track(json!({ "text": "abcdefgh" }));
    let change = tracker.begin_change();
    let ov = change.overlay().unwrap();
    let s = ov.get("text").unwrap().as_json().unwrap().as_str().unwrap().to_owned();
    ov.set("text", format!("{s}ij")).unwrap();
    let prepared = change.prepare().unwrap();
    assert_eq!(
        prepared.ops(),
        &[Op::Append(OpAppend(
            AppendVerb::A,
            vec![Seg::String("text".to_owned())],
            "ij".to_owned()
        ))]
    );
    tracker.adopt(&prepared).unwrap();

    let change = tracker.begin_change();
    let ov = change.overlay().unwrap();
    let s = ov.get("text").unwrap().as_json().unwrap().as_str().unwrap().to_owned();
    ov.set("text", format!("{}xyz", &s[3..])).unwrap();
    let prepared = change.prepare().unwrap();
    assert_eq!(
        prepared.ops(),
        &[
            Op::Truncate(OpTruncate(TruncateVerb::T, vec![Seg::String("text".to_owned())], 3)),
            Op::Append(OpAppend(
                AppendVerb::A,
                vec![Seg::String("text".to_owned())],
                "xyz".to_owned()
            )),
        ]
    );

    assert_eq!(overlap("abcdefgh", "defghxyz", 65_536, None, None), 5);
    assert_eq!(overlap("abcdef", "defghi", 0, None, None), 0);
}

#[test]
fn applies_mutable_and_immutable_operations() {
    let ops = vec![
        Op::Append(OpAppend(
            AppendVerb::A,
            vec![Seg::String("text".to_owned())],
            "b".to_owned(),
        )),
        Op::Splice(OpSplice(
            SpliceVerb::P,
            vec![Seg::String("values".to_owned())],
            1,
            1,
            vec![json!(3), json!(4)],
        )),
        Op::Set(OpSet(
            SetVerb::S,
            vec![Seg::String("nested".to_owned()), Seg::String("value".to_owned())],
            json!(2),
        )),
    ];
    let base = json!({
        "text": "a",
        "values": [1, 2],
        "nested": { "value": 1 },
        "stable": { "value": 9 }
    });
    let immutable = apply_immutable(Some(base.clone()), &ops).unwrap();
    assert_eq!(
        immutable,
        json!({ "text": "ab", "values": [1, 3, 4], "nested": { "value": 2 }, "stable": { "value": 9 } })
    );
    assert_eq!(base["stable"], immutable["stable"]);
    assert_eq!(apply(Some(base), &ops).unwrap(), immutable);
}

#[test]
fn supports_root_replacement_root_splice_and_permutations() {
    let base = vec![Op::Replace(OpReplace(ReplaceVerb::R, json!([1, 2, 3])))];
    assert!(is_base(&base));
    let value: Value = apply(None, &base).unwrap();
    let value = apply(
        Some(value),
        &[Op::Splice(OpSplice(SpliceVerb::P, vec![], 1, 1, vec![json!(4)]))],
    )
    .unwrap();
    let value = apply(Some(value), &[Op::Move(OpMove(MoveVerb::M, vec![], vec![2, 0, 1]))]).unwrap();
    assert_eq!(value, json!([3, 1, 4]));
}

#[test]
fn rejects_unsafe_and_malformed_paths() {
    let err = apply(
        Some(json!({})),
        &[Op::Set(OpSet(
            SetVerb::S,
            vec![
                Seg::String("constructor".to_owned()),
                Seg::String("prototype".to_owned()),
                Seg::String("x".to_owned()),
            ],
            json!(true),
        ))],
    )
    .unwrap_err();
    assert!(err.downcast_ref::<UnsafePathError>().is_some() || err.name() == "UnsafePathError");

    let err = apply(
        Some(json!({ "values": [1] })),
        &[Op::Set(OpSet(
            SetVerb::S,
            vec![Seg::String("values".to_owned()), Seg::Index(3)],
            json!(2),
        ))],
    )
    .unwrap_err();
    assert!(err.downcast_ref::<UnsafePathError>().is_some() || err.name() == "UnsafePathError");

    assert!(
        apply(
            Some(json!({ "value": 1 })),
            &[Op::Append(OpAppend(
                AppendVerb::A,
                vec![Seg::String("value".to_owned())],
                "x".to_owned()
            ))]
        )
        .is_err()
    );
    assert!(assert_valid_op(&json!(["s", "value", 1])).is_err());
    assert!(
        assert_valid_op(&json!(["m", [], [0, 0]]))
            .unwrap_err()
            .message()
            .contains("bijection")
    );
}

#[test]
fn validates_decoded_and_wire_vocabularies_separately() {
    assert!(assert_valid_op(&json!(["s", ["value"], 1])).is_ok());
    assert!(assert_valid_op(&json!(["s", 1])).is_err());
    assert!(assert_valid_wire_op(&json!(["s", 1])).is_ok());
    assert!(assert_valid_wire_op(&json!(["#", 0, ["value"]])).is_ok());
    assert!(assert_valid_op(&json!(["s", ["value"], { "mapLike": true }])).is_ok());
    assert!(assert_valid_wire_op(&json!(["r", { "dateLike": true }])).is_ok());
}

#[test]
fn codec_interns_paths_omits_adjacent_paths_and_roundtrips() {
    let path = vec![Seg::String("nested".to_owned()), Seg::String("text".to_owned())];
    let enc = encoder();
    let dec = decoder();
    let first = vec![
        Op::Truncate(OpTruncate(TruncateVerb::T, path.clone(), 1)),
        Op::Append(OpAppend(AppendVerb::A, path.clone(), "x".to_owned())),
    ];
    assert_eq!(dec.decode(&enc.encode(&first)).unwrap(), first);
    let second = vec![Op::Append(OpAppend(AppendVerb::A, path.clone(), "y".to_owned()))];
    let wire = enc.encode(&second);
    assert_eq!(
        wire,
        vec![
            WireOp::Define(WireOpDefine(DefineVerb::Define, 0, path)),
            WireOp::Append(WireOpAppend(AppendVerb::A, PathRef::Id(0), "y".to_owned())),
        ]
    );
    assert_eq!(dec.decode(&wire).unwrap(), second);
}

#[test]
fn codec_resets_path_dictionaries_on_base_and_rejects_unresolved_short_forms_and_unsafe_paths() {
    let enc = encoder();
    let path = vec![Seg::String("value".to_owned())];
    enc.encode(&[Op::Set(OpSet(SetVerb::S, path.clone(), json!(1)))]);
    enc.encode(&[Op::Set(OpSet(SetVerb::S, path.clone(), json!(2)))]);
    assert_eq!(
        enc.encode(&[Op::Replace(OpReplace(ReplaceVerb::R, json!({ "value": 3 })))]),
        vec![WireOp::Replace(OpReplace(ReplaceVerb::R, json!({ "value": 3 })))]
    );
    assert_eq!(
        enc.encode(&[Op::Set(OpSet(SetVerb::S, path.clone(), json!(4)))]),
        vec![WireOp::Set(WireOpSet(SetVerb::S, PathRef::Path(path), json!(4)))]
    );

    assert!(
        decoder()
            .decode(&[WireOp::AppendShort(WireOpAppendShort(AppendVerb::A, "x".to_owned()))])
            .is_err()
    );
    let unsafe_wire = vec![
        WireOp::Define(WireOpDefine(
            DefineVerb::Define,
            0,
            vec![Seg::String("__proto__".to_owned())],
        )),
        WireOp::Set(WireOpSet(SetVerb::S, PathRef::Id(0), json!(true))),
    ];
    let err = decoder().decode(&unsafe_wire).unwrap_err();
    assert!(err.downcast_ref::<UnsafePathError>().is_some() || err.name() == "UnsafePathError");
}

#[test]
fn codec_omits_adjacent_repeated_path_and_interns_on_second_use() {
    let enc = encoder();
    let path = vec![Seg::String("value".to_owned())];
    assert_eq!(
        enc.encode(&[
            Op::Set(OpSet(SetVerb::S, path.clone(), json!(1))),
            Op::Set(OpSet(SetVerb::S, path.clone(), json!(2))),
        ]),
        vec![
            WireOp::Set(WireOpSet(SetVerb::S, PathRef::Path(path), json!(1))),
            WireOp::SetShort(WireOpSetShort(SetVerb::S, json!(2))),
        ]
    );

    let enc = encoder();
    let deep = vec![Seg::String("a".to_owned()), Seg::String("deep".to_owned())];
    assert_eq!(
        enc.encode(&[Op::Append(OpAppend(AppendVerb::A, deep.clone(), "1".to_owned()))]),
        vec![WireOp::Append(WireOpAppend(
            AppendVerb::A,
            PathRef::Path(deep.clone()),
            "1".to_owned()
        ))]
    );
    assert_eq!(
        enc.encode(&[Op::Append(OpAppend(AppendVerb::A, deep.clone(), "2".to_owned()))]),
        vec![
            WireOp::Define(WireOpDefine(DefineVerb::Define, 0, deep)),
            WireOp::Append(WireOpAppend(AppendVerb::A, PathRef::Id(0), "2".to_owned())),
        ]
    );
}

#[test]
fn codec_handles_null_chars_clears_ids_on_base_and_roundtrips_streams() {
    let ops = vec![
        Op::Set(OpSet(SetVerb::S, vec![Seg::String("a\u{0000}b".to_owned())], json!(1))),
        Op::Set(OpSet(
            SetVerb::S,
            vec![Seg::String("a".to_owned()), Seg::String("b".to_owned())],
            json!(2),
        )),
    ];
    assert_eq!(decoder().decode(&encoder().encode(&ops)).unwrap(), ops);

    let dec = decoder();
    dec.decode(&[
        WireOp::Define(WireOpDefine(DefineVerb::Define, 0, vec![Seg::String("a".to_owned())])),
        WireOp::Append(WireOpAppend(AppendVerb::A, PathRef::Id(0), "1".to_owned())),
    ])
    .unwrap();
    dec.decode(&[WireOp::Replace(OpReplace(ReplaceVerb::R, json!({ "a": "" })))])
        .unwrap();
    assert!(
        dec.decode(&[WireOp::Append(WireOpAppend(
            AppendVerb::A,
            PathRef::Id(0),
            "2".to_owned()
        ))])
        .is_err()
    );

    let batches: Vec<Vec<Op>> = (0..100i64)
        .map(|index| {
            vec![
                Op::Set(OpSet(
                    SetVerb::S,
                    vec![
                        Seg::String("rows".to_owned()),
                        Seg::Index(index),
                        Seg::String("value".to_owned()),
                    ],
                    json!(index),
                )),
                Op::Append(OpAppend(
                    AppendVerb::A,
                    vec![Seg::String("output".to_owned())],
                    index.to_string(),
                )),
                Op::Splice(OpSplice(
                    SpliceVerb::P,
                    vec![Seg::String("tail".to_owned())],
                    index,
                    0,
                    vec![json!(index)],
                )),
            ]
        })
        .collect();
    let enc = encoder();
    let dec = decoder();
    let roundtripped: Vec<Vec<Op>> = batches.iter().map(|b| dec.decode(&enc.encode(b)).unwrap()).collect();
    assert_eq!(roundtripped, batches);
}

#[test]
fn immutable_application_preserves_replacement_ownership_and_reserved_values() {
    let replacement = json!({ "nested": { "value": 1 } });
    let next: Value = apply_immutable(
        None,
        &[
            Op::Replace(OpReplace(ReplaceVerb::R, replacement.clone())),
            Op::Set(OpSet(
                SetVerb::S,
                vec![Seg::String("nested".to_owned()), Seg::String("value".to_owned())],
                json!(2),
            )),
        ],
    )
    .unwrap();
    assert_eq!(replacement["nested"]["value"], 1);
    assert_eq!(next["nested"]["value"], 2);

    let value = pi_js::json::parse(r#"{"__proto__":{"z":1}}"#).unwrap();
    let out = apply(
        Some(json!({})),
        &[Op::Set(OpSet(SetVerb::S, vec![Seg::String("value".to_owned())], value))],
    )
    .unwrap();
    assert!(out["value"].as_object().unwrap().contains_key("__proto__"));
}

#[test]
fn array_index_and_operation_structure_safety() {
    assert_eq!(
        apply(
            Some(json!({ "values": [1, 2, 3] })),
            &[Op::Set(OpSet(
                SetVerb::S,
                vec![Seg::String("values".to_owned()), Seg::Index(1)],
                json!(9)
            ))]
        )
        .unwrap(),
        json!({ "values": [1, 9, 3] })
    );
    assert_eq!(
        apply(
            Some(json!({ "values": [1, 2, 3] })),
            &[Op::Set(OpSet(
                SetVerb::S,
                vec![Seg::String("values".to_owned()), Seg::Index(3)],
                json!(9)
            ))]
        )
        .unwrap(),
        json!({ "values": [1, 2, 3, 9] })
    );
    assert!(
        apply(
            Some(json!({ "values": [1, 2, 3] })),
            &[Op::Set(OpSet(
                SetVerb::S,
                vec![Seg::String("values".to_owned()), Seg::Index(5)],
                json!(9)
            ))]
        )
        .is_err()
    );
    assert!(
        apply(
            Some(json!({ "values": [] })),
            &[Op::Set(OpSet(
                SetVerb::S,
                vec![Seg::String("values".to_owned()), Seg::Index(4_294_967_290)],
                json!(1)
            ))]
        )
        .is_err()
    );
    assert!(
        apply(
            Some(json!({ "values": [1] })),
            &[Op::Set(OpSet(
                SetVerb::S,
                vec![Seg::String("values".to_owned()), Seg::String("0".to_owned())],
                json!(9)
            ))]
        )
        .is_err()
    );
    assert!(
        apply(
            Some(json!({ "values": ["a"] })),
            &[Op::Append(OpAppend(
                AppendVerb::A,
                vec![Seg::String("values".to_owned()), Seg::String("0".to_owned())],
                "b".to_owned()
            ))]
        )
        .is_err()
    );

    assert_eq!(
        apply(
            Some(json!({ "values": [1] })),
            &[Op::Splice(OpSplice(
                SpliceVerb::P,
                vec![Seg::String("values".to_owned())],
                1,
                0,
                vec![Value::Null, Value::Null, json!(9)]
            ))]
        )
        .unwrap(),
        json!({ "values": [1, null, null, 9] })
    );
    assert!(
        apply(
            Some(json!({ "values": [1] })),
            &[Op::Delete(OpDelete(
                DeleteVerb::D,
                vec![Seg::String("values".to_owned()), Seg::Index(1)]
            ))]
        )
        .is_err()
    );

    let items = vec![Value::Null; 300_000];
    let result = apply(
        Some(json!({ "values": [] })),
        &[Op::Splice(OpSplice(
            SpliceVerb::P,
            vec![Seg::String("values".to_owned())],
            0,
            0,
            items,
        ))],
    )
    .unwrap();
    assert_eq!(result["values"].as_array().unwrap().len(), 300_000);

    assert!(assert_valid_op(&json!(["ZZZ", ["value"], 9])).is_err());
    assert!(assert_valid_op(&json!(["p", ["values"], 0, 0, "not-an-array"])).is_err());
    assert!(assert_valid_op(&json!(["s", "value", 9])).is_err());
    assert!(assert_valid_op(&json!({ "op": "s" })).is_err());
    assert!(assert_valid_op(&Value::Null).is_err());
    assert!(
        apply(
            Some(json!({ "value": 1 })),
            &[Op::Append(OpAppend(
                AppendVerb::A,
                vec![Seg::String("missing".to_owned())],
                "x".to_owned()
            ))]
        )
        .is_err()
    );
    assert!(
        apply(
            Some(json!({ "value": 1 })),
            &[Op::Append(OpAppend(
                AppendVerb::A,
                vec![Seg::String("value".to_owned())],
                "x".to_owned()
            ))]
        )
        .is_err()
    );
    assert!(
        apply(
            Some(json!({ "value": "abc" })),
            &[Op::Truncate(OpTruncate(
                TruncateVerb::T,
                vec![Seg::String("value".to_owned())],
                -1
            ))]
        )
        .is_err()
    );
    assert!(assert_valid_op(&json!(["t", ["value"], -1])).is_err());
    assert!(assert_valid_wire_op(&json!(["t", ["value"], -1])).is_err());

    assert_eq!(
        apply(
            Some(json!({ "values": [1, 2] })),
            &[Op::Splice(OpSplice(
                SpliceVerb::P,
                vec![Seg::String("values".to_owned())],
                0,
                1_000_000_000,
                vec![]
            ))]
        )
        .unwrap(),
        json!({ "values": [] })
    );

    assert!(
        apply_immutable(
            Some(json!({ "values": [] })),
            &[Op::Set(OpSet(
                SetVerb::S,
                vec![
                    Seg::String("values".to_owned()),
                    Seg::String("missing".to_owned()),
                    Seg::String("value".to_owned())
                ],
                json!(1)
            ))]
        )
        .unwrap_err()
        .downcast_ref::<PathError>()
        .is_some()
    );
}
