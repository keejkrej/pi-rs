//! Port of packages/chord/test/state-fuzz.test.ts

use pi_chord::delta::Op;
use pi_chord::delta::apply_immutable;
use pi_chord::delta::tracker::{DraftInput, DraftOverlay, JsonValue, track};
use serde_json::json;

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

fn make_item(value: i64) -> JsonValue {
    json!({
        "id": value,
        "text": format!("item-{value}"),
        "score": value % 7,
    })
}

fn mutate_expected(document: &mut JsonValue, choice: usize, value: i64) {
    let obj = document.as_object_mut().unwrap();
    match choice {
        0 => {
            let next = format!("{}-{value}", obj["text"].as_str().unwrap());
            obj.insert("text".into(), JsonValue::String(next));
        }
        1 => {
            let current = obj["text"].as_str().unwrap();
            let start = current.len().min(2);
            let next = format!("{}{value}", &current[start..]);
            obj.insert("text".into(), JsonValue::String(next));
        }
        2 => {
            obj["items"].as_array_mut().unwrap().push(make_item(value));
        }
        3 => {
            obj["items"].as_array_mut().unwrap().insert(0, make_item(value));
        }
        4 => {
            let items = obj["items"].as_array_mut().unwrap();
            if !items.is_empty() {
                items.remove(0);
            }
        }
        5 => {
            obj["items"].as_array_mut().unwrap().pop();
        }
        6 => {
            let items = obj["items"].as_array_mut().unwrap();
            let v = usize::try_from(value).unwrap();
            let index = if items.is_empty() { 0 } else { v % (items.len() + 1) };
            let delete_count = if items.is_empty() { 0 } else { v % 2 };
            let end = (index + delete_count).min(items.len());
            items.splice(index..end, [make_item(value)]);
        }
        7 => {
            obj["items"].as_array_mut().unwrap().reverse();
        }
        8 => {
            obj["items"]
                .as_array_mut()
                .unwrap()
                .sort_by_key(|item| item["id"].as_i64().unwrap());
        }
        9 => {
            let items = obj["items"].as_array_mut().unwrap();
            if !items.is_empty() {
                let idx = usize::try_from(value).unwrap() % items.len();
                items[idx]["score"] = json!(value);
            }
        }
        10 => {
            let meta = obj["meta"].as_object_mut().unwrap();
            let next_rev = meta["revision"].as_i64().unwrap() + 1;
            meta.insert("revision".into(), json!(next_rev));
            meta.insert("label".into(), json!(format!("revision-{value}")));
        }
        11 => {
            let meta = obj["meta"].as_object_mut().unwrap();
            meta.shift_remove("label");
        }
        12 => {
            let items = obj["items"].as_array_mut().unwrap();
            if items.len() > 1 {
                items[1] = items[0].clone();
            }
        }
        _ => {
            let items = obj["items"].as_array_mut().unwrap();
            let count = items.len().min(2);
            for slot in items.iter_mut().take(count) {
                *slot = make_item(value);
            }
        }
    }
}

fn mutate_overlay(document: &DraftOverlay, choice: usize, value: i64) {
    match choice {
        0 => {
            let prev = document.get("text").unwrap();
            let next = format!("{}-{value}", prev.as_json().unwrap().as_str().unwrap());
            document.set("text", JsonValue::String(next)).unwrap();
        }
        1 => {
            let prev = document.get("text").unwrap();
            let current = prev.as_json().unwrap().as_str().unwrap();
            let start = current.len().min(2);
            let next = format!("{}{value}", &current[start..]);
            document.set("text", JsonValue::String(next)).unwrap();
        }
        2 => {
            let items = document.get("items").unwrap().as_overlay().unwrap().clone();
            items.push(&[make_item(value).into()]).unwrap();
        }
        3 => {
            let items = document.get("items").unwrap().as_overlay().unwrap().clone();
            items.unshift(&[make_item(value).into()]).unwrap();
        }
        4 => {
            let items = document.get("items").unwrap().as_overlay().unwrap().clone();
            if !items.is_empty().unwrap() {
                items.shift().unwrap();
            }
        }
        5 => {
            let items = document.get("items").unwrap().as_overlay().unwrap().clone();
            if !items.is_empty().unwrap() {
                items.pop().unwrap();
            }
        }
        6 => {
            let items = document.get("items").unwrap().as_overlay().unwrap().clone();
            let len = items.len().unwrap();
            let v = usize::try_from(value).unwrap();
            let index = if len == 0 { 0 } else { v % (len + 1) };
            let delete_count = if len == 0 { 0 } else { v % 2 };
            items
                .splice(
                    Some(i64::try_from(index).unwrap()),
                    Some(i64::try_from(delete_count).unwrap()),
                    &[make_item(value).into()],
                )
                .unwrap();
        }
        7 => {
            let items = document.get("items").unwrap().as_overlay().unwrap().clone();
            items.reverse().unwrap();
        }
        8 => {
            let items = document.get("items").unwrap().as_overlay().unwrap().clone();
            items
                .sort_by(|left, right| {
                    let lid = left
                        .as_overlay()
                        .unwrap()
                        .get("id")?
                        .as_json()
                        .unwrap()
                        .as_i64()
                        .unwrap();
                    let rid = right
                        .as_overlay()
                        .unwrap()
                        .get("id")?
                        .as_json()
                        .unwrap()
                        .as_i64()
                        .unwrap();
                    Ok(lid.cmp(&rid))
                })
                .unwrap();
        }
        9 => {
            let items = document.get("items").unwrap().as_overlay().unwrap().clone();
            let len = items.len().unwrap();
            if len > 0 {
                let idx = usize::try_from(value).unwrap() % len;
                items
                    .get_index(i64::try_from(idx).unwrap())
                    .unwrap()
                    .as_overlay()
                    .unwrap()
                    .set("score", value)
                    .unwrap();
            }
        }
        10 => {
            let meta = document.get("meta").unwrap().as_overlay().unwrap().clone();
            let next_rev = meta.get("revision").unwrap().as_json().unwrap().as_i64().unwrap() + 1;
            meta.set("revision", next_rev).unwrap();
            meta.set("label", format!("revision-{value}")).unwrap();
        }
        11 => {
            let meta = document.get("meta").unwrap().as_overlay().unwrap().clone();
            meta.delete("label").unwrap();
        }
        12 => {
            let items = document.get("items").unwrap().as_overlay().unwrap().clone();
            if items.len().unwrap() > 1 {
                let cloned = items.get_index(0).unwrap().to_json().unwrap().unwrap();
                items.set_index(1, DraftInput::from(cloned)).unwrap();
            }
        }
        _ => {
            let items = document.get("items").unwrap().as_overlay().unwrap().clone();
            let count = items.len().unwrap().min(2);
            for index in 0..count {
                items
                    .set_index(i64::try_from(index).unwrap(), make_item(value))
                    .unwrap();
            }
        }
    }
}

#[test]
fn converges_across_randomized_prepared_revisions() {
    for seed in 1_u32..=100 {
        let mut rng = Rng::new(seed);
        let initial = json!({
            "items": (0..4_i64).map(|id| json!({ "id": id, "text": format!("item-{id}"), "score": 0 })).collect::<Vec<_>>(),
            "text": "start",
            "meta": { "revision": 0 },
        });
        let tracker = track(initial);
        let mut expected = tracker.value();
        let mut replica = tracker.value();

        for step in 0..100_i64 {
            let choice = (rng.next_f64() * 14.0).floor() as usize;
            let value = i64::from(seed) * 1_000 + step;
            let base_root = tracker.value();
            let base = base_root.clone();

            mutate_expected(&mut expected, choice, value);

            let change = tracker.begin_change();
            let overlay = change.overlay().unwrap();
            mutate_overlay(&overlay, choice, value);
            let prepared = change.prepare().unwrap();
            let operations: Vec<Op> = prepared.ops().to_vec();

            assert_eq!(base_root, base, "prepare base seed {seed} step {step} choice {choice}");
            assert_eq!(prepared.base(), &base_root);
            replica = apply_immutable(Some(replica), &operations).unwrap();
            assert_eq!(
                replica,
                *prepared.value(),
                "replay seed {seed} step {step} choice {choice}",
            );

            tracker.adopt(&prepared).unwrap();
            assert_eq!(base_root, base, "adopt base seed {seed} step {step} choice {choice}");
            assert_eq!(tracker.value(), *prepared.value());
            assert_eq!(
                tracker.value(),
                expected,
                "state seed {seed} step {step} choice {choice}",
            );
            assert_eq!(replica, expected, "replica seed {seed} step {step} choice {choice}",);
        }
    }
}
