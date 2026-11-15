//! Port of packages/chord/src/delta/diff.ts

use std::hash::{Hash, Hasher};

use indexmap::{IndexMap, IndexSet};

use crate::delta::{
    AppendVerb, DeleteVerb, MoveVerb, NonEmptyPath, Op, OpAppend, OpDelete, OpMove, OpReplace, OpSet, OpSplice,
    OpTruncate, Path, RESERVED_SEGMENTS, ReplaceVerb, Seg, SetVerb, SpliceVerb, TruncateVerb, overlap,
};
use crate::types::JsonValue;

// PORT: `greedy_identity_anchors` and `permutation` key by structural `JsonValue` equality
// (`equal_json`) paired with order-independent hashing in `IdentityPositionMap` because Rust
// `JsonValue` values are owned trees rather than JS object references.
struct IdentityPositionMap<'a> {
    map: IndexMap<CanonicalJsonRef<'a>, Vec<i64>>,
}

impl<'a> IdentityPositionMap<'a> {
    fn new() -> Self {
        Self { map: IndexMap::new() }
    }

    fn push(&mut self, value: &'a JsonValue, index: i64) {
        self.map.entry(CanonicalJsonRef(value)).or_default().push(index);
    }

    fn get(&self, value: &'a JsonValue) -> Option<&[i64]> {
        self.map.get(&CanonicalJsonRef(value)).map(Vec::as_slice)
    }
}

#[derive(Clone, Copy)]
struct CanonicalJsonRef<'a>(&'a JsonValue);

impl PartialEq for CanonicalJsonRef<'_> {
    fn eq(&self, other: &Self) -> bool {
        equal_json(self.0, other.0)
    }
}

impl Eq for CanonicalJsonRef<'_> {}

impl Hash for CanonicalJsonRef<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        hash_json(self.0, state);
    }
}

fn hash_json<H: Hasher>(value: &JsonValue, state: &mut H) {
    match value {
        JsonValue::Null => 0u8.hash(state),
        JsonValue::Bool(b) => {
            1u8.hash(state);
            b.hash(state);
        }
        JsonValue::Number(n) => {
            2u8.hash(state);
            if let Some(f) = n.as_f64() {
                let norm = if f == 0.0 { 0.0f64.to_bits() } else { f.to_bits() };
                norm.hash(state);
            } else {
                n.to_string().hash(state);
            }
        }
        JsonValue::String(s) => {
            3u8.hash(state);
            s.hash(state);
        }
        JsonValue::Array(arr) => {
            4u8.hash(state);
            arr.len().hash(state);
            for item in arr {
                hash_json(item, state);
            }
        }
        JsonValue::Object(map) => {
            5u8.hash(state);
            map.len().hash(state);
            // Order-independent hash matching `equal_json` (which ignores object key order).
            let mut acc: u64 = 0;
            for (k, v) in map {
                let mut entry_hasher = std::collections::hash_map::DefaultHasher::new();
                k.hash(&mut entry_hasher);
                hash_json(v, &mut entry_hasher);
                acc = acc.wrapping_add(entry_hasher.finish());
            }
            acc.hash(state);
        }
    }
}

const DEFAULT_OVERLAP_SCAN: i64 = 65_536;
const MAX_DELTA_OPERATIONS: usize = 4_096;
const MAX_IDENTITY_CANDIDATES: i64 = 200_000;
const MAX_SEMANTIC_CELLS: i64 = 65_536;

// PORT: JS `WeakSet<Op[]>` batch overflow tracking uses the active `Vec<Op>` buffer id and clears
// on return from `diff_revisions` so stack addresses are never aliased across calls.
static OVERFLOWED_BATCHES: std::sync::LazyLock<std::sync::Mutex<IndexSet<u64>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(IndexSet::new()));

type ArrayMatch = (i64, i64);

struct MatchCandidate {
    before: i64,
    after: i64,
    previous: i64,
}

struct OverflowGuard(u64);

impl Drop for OverflowGuard {
    fn drop(&mut self) {
        let mut set = OVERFLOWED_BATCHES.lock().unwrap_or_else(|e| e.into_inner());
        set.shift_remove(&self.0);
    }
}

fn batch_id(operations: &Vec<Op>) -> u64 {
    (operations as *const Vec<Op>) as usize as u64
}

fn is_overflowed(operations: &Vec<Op>) -> bool {
    let id = batch_id(operations);
    OVERFLOWED_BATCHES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains(&id)
}

/// Compute a compact operation batch from two immutable JSON revisions.
pub fn diff_revisions(before: &JsonValue, after: &JsonValue) -> Vec<Op> {
    let mut operations = Vec::new();
    let id = batch_id(&operations);
    OVERFLOWED_BATCHES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .shift_remove(&id);
    let _guard = OverflowGuard(id);

    diff_value(before, after, &[], &mut operations);
    if is_overflowed(&operations) {
        return vec![Op::Replace(OpReplace(ReplaceVerb::R, after.clone()))];
    }
    if operations.is_empty() || matches!(operations.first(), Some(Op::Replace(_))) {
        return operations;
    }
    let mut delta_cost = 2i64;
    for operation in &operations {
        delta_cost += operation_cost(operation) + 1;
    }
    if delta_cost < 65_536 {
        return operations;
    }
    let snapshot_cost = json_cost(after) + 6;
    if delta_cost >= snapshot_cost {
        vec![Op::Replace(OpReplace(ReplaceVerb::R, after.clone()))]
    } else {
        operations
    }
}

fn emit_operation(operations: &mut Vec<Op>, operation: Op) {
    let id = batch_id(operations);
    let mut overflowed = OVERFLOWED_BATCHES.lock().unwrap_or_else(|e| e.into_inner());
    if overflowed.contains(&id) {
        return;
    }
    if operations.len() >= MAX_DELTA_OPERATIONS {
        overflowed.insert(id);
        return;
    }
    drop(overflowed);
    operations.push(operation);
}

fn is_container(value: &JsonValue) -> bool {
    matches!(value, JsonValue::Array(_) | JsonValue::Object(_))
}

fn is_empty_container(value: &JsonValue) -> bool {
    match value {
        JsonValue::Array(arr) => arr.is_empty(),
        JsonValue::Object(map) => map.is_empty(),
        _ => false,
    }
}

fn emit_set(path: &[Seg], value: &JsonValue, operations: &mut Vec<Op>) {
    if path.is_empty() {
        emit_operation(operations, Op::Replace(OpReplace(ReplaceVerb::R, value.clone())));
    } else {
        emit_operation(operations, Op::Set(OpSet(SetVerb::S, path.to_vec(), value.clone())));
    }
}

fn equal_numbers(a: &serde_json::Number, b: &serde_json::Number) -> bool {
    if a == b {
        return true;
    }
    if let (Some(ia), Some(ib)) = (a.as_i64(), b.as_i64()) {
        return ia == ib;
    }
    if let (Some(ua), Some(ub)) = (a.as_u64(), b.as_u64()) {
        return ua == ub;
    }
    if let (Some(fa), Some(fb)) = (a.as_f64(), b.as_f64()) {
        return fa == fb;
    }
    false
}

fn equal_json(left: &JsonValue, right: &JsonValue) -> bool {
    if std::ptr::eq(left, right) {
        return true;
    }
    match (left, right) {
        (JsonValue::Null, JsonValue::Null) => true,
        (JsonValue::Bool(a), JsonValue::Bool(b)) => a == b,
        (JsonValue::Number(a), JsonValue::Number(b)) => equal_numbers(a, b),
        (JsonValue::String(a), JsonValue::String(b)) => a == b,
        (JsonValue::Array(a), JsonValue::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b.iter()).all(|(l, r)| equal_json(l, r))
        }
        (JsonValue::Object(a), JsonValue::Object(b)) => {
            a.len() == b.len() && a.iter().all(|(k, v)| b.get(k).is_some_and(|rv| equal_json(v, rv)))
        }
        _ => false,
    }
}

fn permutation(before: &[JsonValue], after: &[JsonValue]) -> Option<Vec<i64>> {
    if before.len() != after.len() {
        return None;
    }
    let mut positions: IndexMap<CanonicalJsonRef<'_>, (Vec<i64>, usize)> = IndexMap::new();
    for (index, value) in before.iter().enumerate() {
        positions
            .entry(CanonicalJsonRef(value))
            .or_insert_with(|| (Vec::new(), 0))
            .0
            .push(index as i64);
    }
    let mut result = Vec::with_capacity(after.len());
    for value in after {
        let (indices, used) = positions.get_mut(&CanonicalJsonRef(value))?;
        if *used == indices.len() {
            return None;
        }
        result.push(indices[*used]);
        *used += 1;
    }
    Some(result)
}

fn emit_string(before: &str, after: &str, path: &NonEmptyPath, operations: &mut Vec<Op>) {
    if before == after {
        return;
    }
    if before.is_ascii() && after.is_ascii() {
        if after.len() > before.len() && after.starts_with(before) {
            emit_operation(
                operations,
                Op::Append(OpAppend(AppendVerb::A, path.clone(), after[before.len()..].to_owned())),
            );
            return;
        }
        let shared = overlap(before, after, DEFAULT_OVERLAP_SCAN, None, None);
        if shared == 0 {
            emit_operation(
                operations,
                Op::Set(OpSet(SetVerb::S, path.clone(), JsonValue::String(after.to_owned()))),
            );
            return;
        }
        let shared_usize = shared as usize;
        emit_operation(
            operations,
            Op::Truncate(OpTruncate(
                TruncateVerb::T,
                path.clone(),
                (before.len() - shared_usize) as i64,
            )),
        );
        if after.len() > shared_usize {
            emit_operation(
                operations,
                Op::Append(OpAppend(AppendVerb::A, path.clone(), after[shared_usize..].to_owned())),
            );
        }
        return;
    }

    let before_len = pi_js::str16::len(before) as i64;
    let after_len = pi_js::str16::len(after) as i64;
    if after_len > before_len && pi_js::str16::slice(after, 0, Some(before_len)) == before {
        emit_operation(
            operations,
            Op::Append(OpAppend(
                AppendVerb::A,
                path.clone(),
                pi_js::str16::slice(after, before_len, None),
            )),
        );
        return;
    }
    let shared = overlap(before, after, DEFAULT_OVERLAP_SCAN, None, None);
    if shared == 0 {
        emit_operation(
            operations,
            Op::Set(OpSet(SetVerb::S, path.clone(), JsonValue::String(after.to_owned()))),
        );
        return;
    }
    emit_operation(
        operations,
        Op::Truncate(OpTruncate(TruncateVerb::T, path.clone(), before_len - shared)),
    );
    if after_len > shared {
        emit_operation(
            operations,
            Op::Append(OpAppend(
                AppendVerb::A,
                path.clone(),
                pi_js::str16::slice(after, shared, None),
            )),
        );
    }
}

fn same_value(left: &JsonValue, right: &JsonValue) -> bool {
    equal_json(left, right)
}

fn lcs_matches(
    before: &[JsonValue],
    after: &[JsonValue],
    equal: &dyn Fn(&JsonValue, &JsonValue) -> bool,
    max_cells: i64,
) -> Option<Vec<ArrayMatch>> {
    if before.is_empty() || after.is_empty() {
        return Some(Vec::new());
    }
    if (before.len() as i64).saturating_mul(after.len() as i64) > max_cells {
        return None;
    }
    let width = after.len() + 1;
    let mut lengths = vec![0u32; (before.len() + 1) * width];
    for left in (0..before.len()).rev() {
        for right in (0..after.len()).rev() {
            let at = left * width + right;
            lengths[at] = if equal(&before[left], &after[right]) {
                lengths[(left + 1) * width + right + 1] + 1
            } else {
                lengths[(left + 1) * width + right].max(lengths[left * width + right + 1])
            };
        }
    }
    let mut matches = Vec::new();
    let mut left = 0usize;
    let mut right = 0usize;
    while left < before.len() && right < after.len() {
        if equal(&before[left], &after[right])
            && lengths[left * width + right] == lengths[(left + 1) * width + right + 1] + 1
        {
            matches.push((left as i64, right as i64));
            left += 1;
            right += 1;
        } else if lengths[(left + 1) * width + right] >= lengths[left * width + right + 1] {
            left += 1;
        } else {
            right += 1;
        }
    }
    Some(matches)
}

fn semantically_aligned(left: &JsonValue, right: &JsonValue) -> bool {
    if same_value(left, right) {
        return true;
    }
    if !is_container(left) || !is_container(right) {
        return false;
    }
    match (left, right) {
        (JsonValue::Array(l_arr), JsonValue::Array(r_arr)) => {
            if l_arr.len() != r_arr.len() {
                return false;
            }
            l_arr
                .iter()
                .zip(r_arr.iter())
                .any(|(lv, rv)| is_container(lv) && !is_empty_container(lv) && equal_json(lv, rv))
        }
        (JsonValue::Object(l_map), JsonValue::Object(r_map)) => {
            for (key, value) in l_map {
                if is_container(value) && !is_empty_container(value) {
                    if let Some(right_value) = r_map.get(key) {
                        if equal_json(value, right_value) {
                            return true;
                        }
                    }
                }
            }
            false
        }
        _ => false,
    }
}

fn lower_bound(values: &[i64], value: i64) -> usize {
    let mut low = 0usize;
    let mut high = values.len();
    while low < high {
        let middle = low + ((high - low) >> 1);
        if values[middle] < value {
            low = middle + 1;
        } else {
            high = middle;
        }
    }
    low
}

fn identity_subsequence(
    before: &[JsonValue],
    before_start: i64,
    before_end: i64,
    after: &[JsonValue],
    after_start: i64,
    after_end: i64,
) -> Option<Vec<ArrayMatch>> {
    let before_count = before_end - before_start;
    let after_count = after_end - after_start;
    let mut matches = Vec::new();
    if after_count < before_count {
        let mut before_index = before_start;
        for after_index in after_start..after_end {
            while before_index < before_end && !same_value(&before[before_index as usize], &after[after_index as usize])
            {
                before_index += 1;
            }
            if before_index == before_end {
                return None;
            }
            matches.push((before_index, after_index));
            before_index += 1;
        }
        return Some(matches);
    }
    if before_count < after_count {
        let mut after_index = after_start;
        for before_index in before_start..before_end {
            while after_index < after_end && !same_value(&before[before_index as usize], &after[after_index as usize]) {
                after_index += 1;
            }
            if after_index == after_end {
                return None;
            }
            matches.push((before_index, after_index));
            after_index += 1;
        }
        return Some(matches);
    }
    None
}

fn greedy_identity_anchors(
    positions: &IdentityPositionMap<'_>,
    after: &[JsonValue],
    after_start: i64,
    after_end: i64,
) -> Vec<ArrayMatch> {
    let mut matches = Vec::new();
    let mut previous = -1i64;
    for after_index in after_start..after_end {
        let Some(candidates) = positions.get(&after[after_index as usize]) else {
            continue;
        };
        let at = lower_bound(candidates, previous + 1);
        let Some(&before_index) = candidates.get(at) else {
            continue;
        };
        matches.push((before_index, after_index));
        previous = before_index;
    }
    matches
}

fn identity_anchors(
    before: &[JsonValue],
    before_start: i64,
    before_end: i64,
    after: &[JsonValue],
    after_start: i64,
    after_end: i64,
) -> Vec<ArrayMatch> {
    let mut positions = IdentityPositionMap::new();
    for index in before_start..before_end {
        positions.push(&before[index as usize], index);
    }
    let mut candidate_count = 0i64;
    for index in after_start..after_end {
        let len = positions
            .get(&after[index as usize])
            .map_or(0, |slice| slice.len() as i64);
        candidate_count += len;
        if candidate_count > MAX_IDENTITY_CANDIDATES {
            return greedy_identity_anchors(&positions, after, after_start, after_end);
        }
    }
    if candidate_count == 0 {
        return Vec::new();
    }

    let mut candidates: Vec<MatchCandidate> = Vec::new();
    let mut tails: Vec<usize> = Vec::new();
    let mut tail_values: Vec<i64> = Vec::new();
    for after_index in after_start..after_end {
        let Some(before_positions) = positions.get(&after[after_index as usize]) else {
            continue;
        };
        for &before_index in before_positions.iter().rev() {
            let at = lower_bound(&tail_values, before_index);
            let candidate_index = candidates.len();
            let previous = if at == 0 { -1 } else { tails[at - 1] as i64 };
            candidates.push(MatchCandidate {
                before: before_index,
                after: after_index,
                previous,
            });
            if at == tails.len() {
                tails.push(candidate_index);
                tail_values.push(before_index);
            } else {
                tails[at] = candidate_index;
                tail_values[at] = before_index;
            }
        }
    }
    let mut matches = Vec::new();
    let mut candidate_index = tails.last().map_or(-1i64, |&idx| idx as i64);
    while candidate_index >= 0 {
        let candidate = &candidates[candidate_index as usize];
        matches.push((candidate.before, candidate.after));
        candidate_index = candidate.previous;
    }
    matches.reverse();
    matches
}

fn process_array_matches(
    before: &[JsonValue],
    after: &[JsonValue],
    path: &[Seg],
    operations: &mut Vec<Op>,
    before_start: i64,
    before_end: i64,
    after_start: i64,
    after_end: i64,
    output_start: i64,
    matches: &[ArrayMatch],
) {
    let mut before_at = before_start;
    let mut after_at = after_start;
    let mut output_at = output_start;
    for &(before_match, after_match) in matches {
        if is_overflowed(operations) {
            return;
        }
        diff_array_region(
            before,
            after,
            path,
            operations,
            before_at,
            before_match,
            after_at,
            after_match,
            output_at,
        );
        output_at += after_match - after_at;
        if !same_value(&before[before_match as usize], &after[after_match as usize]) {
            let mut next_path = Vec::with_capacity(path.len() + 1);
            next_path.extend_from_slice(path);
            next_path.push(Seg::Index(output_at));
            diff_value(
                &before[before_match as usize],
                &after[after_match as usize],
                &next_path,
                operations,
            );
        }
        output_at += 1;
        before_at = before_match + 1;
        after_at = after_match + 1;
    }
    if !is_overflowed(operations) {
        diff_array_region(
            before, after, path, operations, before_at, before_end, after_at, after_end, output_at,
        );
    }
}

fn diff_array_region(
    before: &[JsonValue],
    after: &[JsonValue],
    path: &[Seg],
    operations: &mut Vec<Op>,
    mut before_start: i64,
    mut before_end: i64,
    mut after_start: i64,
    mut after_end: i64,
    mut output_start: i64,
) {
    if is_overflowed(operations) {
        return;
    }
    while before_start < before_end
        && after_start < after_end
        && same_value(&before[before_start as usize], &after[after_start as usize])
    {
        before_start += 1;
        after_start += 1;
        output_start += 1;
    }
    while before_start < before_end
        && after_start < after_end
        && same_value(&before[(before_end - 1) as usize], &after[(after_end - 1) as usize])
    {
        before_end -= 1;
        after_end -= 1;
    }
    let before_count = before_end - before_start;
    let after_count = after_end - after_start;
    if before_count == 0 && after_count == 0 {
        return;
    }
    if before_count == 0 || after_count == 0 {
        emit_operation(
            operations,
            Op::Splice(OpSplice(
                SpliceVerb::P,
                path.to_vec(),
                output_start,
                before_count,
                after[after_start as usize..after_end as usize].to_vec(),
            )),
        );
        return;
    }

    if before_count == after_count {
        let mut positional = Vec::new();
        for offset in 0..before_count {
            if same_value(
                &before[(before_start + offset) as usize],
                &after[(after_start + offset) as usize],
            ) {
                positional.push((before_start + offset, after_start + offset));
            }
        }
        if !positional.is_empty() {
            process_array_matches(
                before,
                after,
                path,
                operations,
                before_start,
                before_end,
                after_start,
                after_end,
                output_start,
                &positional,
            );
            return;
        }
    }

    if let Some(subsequence) = identity_subsequence(before, before_start, before_end, after, after_start, after_end) {
        if !subsequence.is_empty() {
            process_array_matches(
                before,
                after,
                path,
                operations,
                before_start,
                before_end,
                after_start,
                after_end,
                output_start,
                &subsequence,
            );
            return;
        }
    }

    let identity = identity_anchors(before, before_start, before_end, after, after_start, after_end);
    if !identity.is_empty() {
        process_array_matches(
            before,
            after,
            path,
            operations,
            before_start,
            before_end,
            after_start,
            after_end,
            output_start,
            &identity,
        );
        return;
    }

    if let Some(semantic) = lcs_matches(
        &before[before_start as usize..before_end as usize],
        &after[after_start as usize..after_end as usize],
        &semantically_aligned,
        MAX_SEMANTIC_CELLS,
    ) {
        if !semantic.is_empty() {
            let absolute: Vec<ArrayMatch> = semantic
                .into_iter()
                .map(|(bi, ai)| (before_start + bi, after_start + ai))
                .collect();
            process_array_matches(
                before,
                after,
                path,
                operations,
                before_start,
                before_end,
                after_start,
                after_end,
                output_start,
                &absolute,
            );
            return;
        }
    }

    if before_count == 1 && after_count == 1 {
        let mut next_path = Vec::with_capacity(path.len() + 1);
        next_path.extend_from_slice(path);
        next_path.push(Seg::Index(output_start));
        diff_value(
            &before[before_start as usize],
            &after[after_start as usize],
            &next_path,
            operations,
        );
        return;
    }
    emit_operation(
        operations,
        Op::Splice(OpSplice(
            SpliceVerb::P,
            path.to_vec(),
            output_start,
            before_count,
            after[after_start as usize..after_end as usize].to_vec(),
        )),
    );
}

fn diff_array(before: &[JsonValue], after: &[JsonValue], path: &[Seg], operations: &mut Vec<Op>) {
    if std::ptr::eq(before.as_ptr(), after.as_ptr()) && before.len() == after.len() {
        return;
    }
    if before.len() == after.len() && before.iter().zip(after.iter()).all(|(l, r)| equal_json(l, r)) {
        return;
    }
    if before.len() == after.len()
        && before.len() > 1
        && !same_value(&before[0], &after[0])
        && !same_value(&before[before.len() - 1], &after[after.len() - 1])
    {
        if let Some(order) = permutation(before, after) {
            emit_operation(operations, Op::Move(OpMove(MoveVerb::M, path.to_vec(), order)));
            return;
        }
    }
    diff_array_region(
        before,
        after,
        path,
        operations,
        0,
        before.len() as i64,
        0,
        after.len() as i64,
        0,
    );
}

fn diff_object(before: &JsonValue, after: &JsonValue, path: &[Seg], operations: &mut Vec<Op>) {
    let (JsonValue::Object(before_map), JsonValue::Object(after_map)) = (before, after) else {
        return;
    };
    if before_map
        .keys()
        .chain(after_map.keys())
        .any(|key| RESERVED_SEGMENTS.contains(key.as_str()))
    {
        if !equal_json(before, after) {
            emit_set(path, after, operations);
        }
        return;
    }
    for (key, after_val) in after_map {
        if is_overflowed(operations) {
            return;
        }
        let mut next_path = Vec::with_capacity(path.len() + 1);
        next_path.extend_from_slice(path);
        next_path.push(Seg::String(key.clone()));
        if let Some(before_val) = before_map.get(key) {
            diff_value(before_val, after_val, &next_path, operations);
        } else {
            emit_set(&next_path, after_val, operations);
        }
    }
    for key in before_map.keys() {
        if is_overflowed(operations) {
            return;
        }
        if !after_map.contains_key(key) {
            let mut del_path = Vec::with_capacity(path.len() + 1);
            del_path.extend_from_slice(path);
            del_path.push(Seg::String(key.clone()));
            emit_operation(operations, Op::Delete(OpDelete(DeleteVerb::D, del_path)));
        }
    }
}

fn diff_value(before: &JsonValue, after: &JsonValue, path: &[Seg], operations: &mut Vec<Op>) {
    if std::ptr::eq(before, after) || is_overflowed(operations) {
        return;
    }
    if let (JsonValue::String(b), JsonValue::String(a)) = (before, after) {
        if !path.is_empty() {
            let non_empty: NonEmptyPath = path.to_vec();
            emit_string(b, a, &non_empty, operations);
            return;
        }
    }
    if let (JsonValue::Array(b), JsonValue::Array(a)) = (before, after) {
        diff_array(b, a, path, operations);
        return;
    }
    if matches!(before, JsonValue::Object(_)) && matches!(after, JsonValue::Object(_)) {
        diff_object(before, after, path, operations);
        return;
    }
    if let (JsonValue::Number(bn), JsonValue::Number(an)) = (before, after) {
        if equal_numbers(bn, an) {
            return;
        }
    } else if before == after {
        return;
    }
    emit_set(path, after, operations);
}

fn int_decimal_len(mut n: i64) -> i64 {
    if n == 0 {
        return 1;
    }
    let mut len = if n < 0 {
        n = n.saturating_neg();
        1
    } else {
        0
    };
    while n > 0 {
        len += 1;
        n /= 10;
    }
    len
}

fn json_cost(value: &JsonValue) -> i64 {
    match value {
        JsonValue::Null => 4,
        JsonValue::String(s) => (pi_js::str16::len(s) as i64) + 2,
        JsonValue::Number(n) => {
            if let Some(f) = n.as_f64() {
                pi_js::json::number_to_string(f).len() as i64
            } else {
                n.to_string().len() as i64
            }
        }
        JsonValue::Bool(b) => {
            if *b {
                4
            } else {
                5
            }
        }
        JsonValue::Array(arr) => {
            let mut cost = 2i64;
            for (index, item) in arr.iter().enumerate() {
                cost += json_cost(item) + if index == 0 { 0 } else { 1 };
            }
            cost
        }
        JsonValue::Object(map) => {
            let mut cost = 2i64;
            for (index, (key, val)) in map.iter().enumerate() {
                cost += (pi_js::str16::len(key) as i64) + 3 + json_cost(val) + if index == 0 { 0 } else { 1 };
            }
            cost
        }
    }
}

fn path_cost(path: &Path) -> i64 {
    let mut cost = 2i64;
    for (index, seg) in path.iter().enumerate() {
        let seg_len = match seg {
            Seg::String(s) => (pi_js::str16::len(s) as i64) + 2,
            Seg::Index(i) => int_decimal_len(*i),
        };
        cost += seg_len + if index == 0 { 0 } else { 1 };
    }
    cost
}

fn operation_cost(operation: &Op) -> i64 {
    match operation {
        Op::Replace(OpReplace(_, value)) => 6 + json_cost(value),
        Op::Set(OpSet(_, path, value)) => 7 + path_cost(path) + json_cost(value),
        Op::Delete(OpDelete(_, path)) => 6 + path_cost(path),
        Op::Append(OpAppend(_, path, text)) => 7 + path_cost(path) + (pi_js::str16::len(text) as i64) + 2,
        Op::Truncate(OpTruncate(_, path, count)) => 7 + path_cost(path) + int_decimal_len(*count),
        Op::Splice(OpSplice(_, path, index, remove, items)) => {
            let mut items_cost = 2i64;
            for (i, item) in items.iter().enumerate() {
                items_cost += json_cost(item) + if i == 0 { 0 } else { 1 };
            }
            10 + path_cost(path) + int_decimal_len(*index) + int_decimal_len(*remove) + items_cost
        }
        Op::Move(OpMove(_, path, permutation)) => {
            let mut perm_cost = 2i64;
            for (i, &p) in permutation.iter().enumerate() {
                perm_cost += int_decimal_len(p) + if i == 0 { 0 } else { 1 };
            }
            7 + path_cost(path) + perm_cost
        }
    }
}
