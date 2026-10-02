//! Port of `diff/libesm/diff/base.js` (diff@8.0.4).

// PORT: the async `callback` option (computing the diff across `setTimeout` ticks) is not ported;
// the synchronous mode returns identical results.
// PORT: `castInput` and `useLongestToken` only matter for `diffJson`, which is not ported.

use std::rc::Rc;

use crate::vendor::jsdiff::types::{AbortableDiffOptions, ChangeObject};

// PORT: the `Diff` base class becomes a trait. `Value` is the input (`InputValueT`), `Output` the
// joined value of a change object (`ValueT`), `Token` is `TokenT`, and `Options` the option struct
// of the concrete diff. Methods without a JS default are required.
pub trait Diff {
    type Value: ?Sized;
    type Output;
    type Token;
    type Options;

    fn tokenize(&self, value: &Self::Value, options: &Self::Options) -> Vec<Self::Token>;

    fn remove_empty(&self, tokens: Vec<Self::Token>) -> Vec<Self::Token>;

    fn equals(&self, left: &Self::Token, right: &Self::Token, options: &Self::Options) -> bool;

    fn join(&self, tokens: &[Self::Token]) -> Self::Output;

    fn post_process(
        &self,
        change_objects: Vec<ChangeObject<Self::Output>>,
        _options: &Self::Options,
    ) -> Vec<ChangeObject<Self::Output>> {
        change_objects
    }

    // PORT: reads `options.oneChangePerToken` from the concrete option struct.
    fn one_change_per_token(&self, options: &Self::Options) -> bool;

    // PORT: the overloads of `diff` become `diff` (no `maxEditLength` / `timeout`, so it never
    // returns `undefined`) and `diff_abortable` (returns `None` where JS returns `undefined`).
    fn diff(
        &self,
        old_str: &Self::Value,
        new_str: &Self::Value,
        options: &Self::Options,
    ) -> Vec<ChangeObject<Self::Output>>
    where
        Self: Sized,
    {
        self.diff_abortable(old_str, new_str, options, &AbortableDiffOptions::default())
            .unwrap_or_default()
    }

    fn diff_abortable(
        &self,
        old_str: &Self::Value,
        new_str: &Self::Value,
        options: &Self::Options,
        abortable: &AbortableDiffOptions,
    ) -> Option<Vec<ChangeObject<Self::Output>>>
    where
        Self: Sized,
    {
        let old_tokens = self.remove_empty(self.tokenize(old_str, options));
        let new_tokens = self.remove_empty(self.tokenize(new_str, options));
        self.diff_with_options_obj(&old_tokens, &new_tokens, options, abortable)
    }

    fn diff_with_options_obj(
        &self,
        old_tokens: &[Self::Token],
        new_tokens: &[Self::Token],
        options: &Self::Options,
        abortable: &AbortableDiffOptions,
    ) -> Option<Vec<ChangeObject<Self::Output>>>
    where
        Self: Sized,
    {
        let equals = |left: &Self::Token, right: &Self::Token| self.equals(left, right, options);
        let components = find_components(
            old_tokens,
            new_tokens,
            &equals,
            self.one_change_per_token(options),
            abortable,
        )?;
        let value = self.build_values(components, new_tokens, old_tokens);
        Some(self.post_process(value, options))
    }

    // PORT: receives the components already in order; see `collect_components`.
    fn build_values(
        &self,
        components: Vec<RawComponent>,
        new_tokens: &[Self::Token],
        old_tokens: &[Self::Token],
    ) -> Vec<ChangeObject<Self::Output>>
    where
        Self: Sized,
    {
        let mut new_pos = 0usize;
        let mut old_pos = 0usize;
        let mut out = Vec::with_capacity(components.len());
        for component in components {
            let value;
            if !component.removed {
                value = self.join(token_slice(new_tokens, new_pos, component.count));
                new_pos += component.count;
                // Common case
                if !component.added {
                    old_pos += component.count;
                }
            } else {
                value = self.join(token_slice(old_tokens, old_pos, component.count));
                old_pos += component.count;
            }
            out.push(ChangeObject {
                count: component.count,
                added: component.added,
                removed: component.removed,
                value,
            });
        }
        out
    }
}

// PORT: a component of the final path, before `buildValues` fills in its value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawComponent {
    pub count: usize,
    pub added: bool,
    pub removed: bool,
}

// PORT: `tokens.slice(pos, pos + count)`, clamped like JS.
fn token_slice<T>(tokens: &[T], pos: usize, count: usize) -> &[T] {
    let start = pos.min(tokens.len());
    let end = pos.saturating_add(count).min(tokens.len());
    &tokens[start..end]
}

// PORT: the `{count, added, removed, previousComponent}` linked list as `Rc` nodes.
struct Component {
    count: usize,
    added: bool,
    removed: bool,
    previous_component: Option<Rc<Component>>,
}

impl Drop for Component {
    // PORT: unlink iteratively so that dropping a long component chain cannot overflow the stack.
    fn drop(&mut self) {
        let mut next = self.previous_component.take();
        while let Some(rc) = next {
            match Rc::try_unwrap(rc) {
                Ok(mut component) => next = component.previous_component.take(),
                Err(_) => break,
            }
        }
    }
}

#[derive(Clone)]
struct DiffPath {
    old_pos: i64,
    last_component: Option<Rc<Component>>,
}

// PORT: `bestPath`, a JS array indexed by (possibly negative) diagonal numbers.
#[derive(Default)]
struct BestPaths {
    non_negative: Vec<Option<DiffPath>>,
    negative: Vec<Option<DiffPath>>,
}

impl BestPaths {
    fn slot(&mut self, diagonal: i64) -> &mut Option<DiffPath> {
        let (vec, idx) = if diagonal >= 0 {
            (&mut self.non_negative, diagonal as usize)
        } else {
            (&mut self.negative, (-diagonal - 1) as usize)
        };
        if idx >= vec.len() {
            vec.resize_with(idx + 1, || None);
        }
        &mut vec[idx]
    }

    fn get(&self, diagonal: i64) -> Option<&DiffPath> {
        if diagonal >= 0 {
            self.non_negative.get(diagonal as usize).and_then(Option::as_ref)
        } else {
            self.negative.get((-diagonal - 1) as usize).and_then(Option::as_ref)
        }
    }
}

// PORT: `Math.min` (NaN-propagating).
fn js_min(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() { f64::NAN } else { a.min(b) }
}

// PORT: the edit-graph search of `diffWithOptionsObj`, generic over token equality. Returns the
// components in order, or `None` where JS returns `undefined`.
pub(crate) fn find_components<T>(
    old_tokens: &[T],
    new_tokens: &[T],
    equals: &dyn Fn(&T, &T) -> bool,
    one_change_per_token: bool,
    abortable: &AbortableDiffOptions,
) -> Option<Vec<RawComponent>> {
    let new_len = new_tokens.len() as i64;
    let old_len = old_tokens.len() as i64;
    let mut edit_length: i64 = 1;
    let mut max_edit_length = (new_len + old_len) as f64;
    if let Some(limit) = abortable.max_edit_length {
        max_edit_length = js_min(max_edit_length, limit);
    }
    let max_execution_time = abortable.timeout;
    let abort_after_timestamp = max_execution_time.map(|t| crate::time::now_ms() as f64 + t);

    let mut best_path = BestPaths::default();
    let mut seed = DiffPath {
        old_pos: -1,
        last_component: None,
    };
    // Seed editLength = 0, i.e. the content starts with the same values
    let new_pos = extract_common(&mut seed, new_tokens, old_tokens, 0, equals, one_change_per_token);
    if seed.old_pos + 1 >= old_len && new_pos + 1 >= new_len {
        // Identity per the equality and tokenizer
        return Some(collect_components(seed.last_component));
    }
    *best_path.slot(0) = Some(seed);

    // Once we hit the right edge of the edit graph on some diagonal k, we can
    // definitely reach the end of the edit graph in no more than k edits, so
    // there's no point in considering any moves to diagonal k+1 any more (from
    // which we're guaranteed to need at least k+1 more edits).
    // Similarly, once we've reached the bottom of the edit graph, there's no
    // point considering moves to lower diagonals.
    // We record this fact by setting minDiagonalToConsider and
    // maxDiagonalToConsider to some finite value once we've hit the edge of
    // the edit graph.
    // This optimization is not faithful to the original algorithm presented in
    // Myers's paper, which instead pointlessly extends D-paths off the end of
    // the edit graph - see page 7 of Myers's paper which notes this point
    // explicitly and illustrates it with a diagram. This has major performance
    // implications for some common scenarios. For instance, to compute a diff
    // where the new text simply appends d characters on the end of the
    // original text of length n, the true Myers algorithm will take O(n+d^2)
    // time while this optimization needs only O(n+d) time.
    let mut min_diagonal_to_consider = i64::MIN;
    let mut max_diagonal_to_consider = i64::MAX;

    // Performs the length of edit iteration. Is a bit fugly as this has to support the
    // sync and async mode which is never fun. Loops over execEditLength until a value
    // is produced, or until the edit length exceeds options.maxEditLength (if given),
    // in which case it will return undefined.
    while (edit_length as f64) <= max_edit_length
        && abort_after_timestamp.is_none_or(|deadline| (crate::time::now_ms() as f64) <= deadline)
    {
        // Main worker method. checks all permutations of a given edit length for acceptance.
        let mut diagonal_path = min_diagonal_to_consider.max(-edit_length);
        while diagonal_path <= max_diagonal_to_consider.min(edit_length) {
            // No one else is going to attempt to use this value, clear it
            let remove_path = best_path.slot(diagonal_path - 1).take();
            let add_path = best_path.get(diagonal_path + 1).cloned();
            let mut can_add = false;
            if let Some(add_path) = &add_path {
                // what newPos will be after we do an insertion:
                let add_path_new_pos = add_path.old_pos - diagonal_path;
                can_add = 0 <= add_path_new_pos && add_path_new_pos < new_len;
            }
            let can_remove = remove_path.as_ref().is_some_and(|p| p.old_pos + 1 < old_len);
            if !can_add && !can_remove {
                // If this path is a terminal then prune
                *best_path.slot(diagonal_path) = None;
                diagonal_path += 2;
                continue;
            }
            // Select the diagonal that we want to branch from. We select the prior
            // path whose position in the old string is the farthest from the origin
            // and does not pass the bounds of the diff graph
            let use_add = match (&remove_path, &add_path) {
                _ if !can_remove => true,
                (Some(remove), Some(add)) => can_add && remove.old_pos < add.old_pos,
                _ => false,
            };
            // PORT: canAdd implies addPath is set and canRemove implies removePath is set.
            let mut base_path = match (use_add, &add_path, &remove_path) {
                (true, Some(add), _) => add_to_path(add, true, false, 0, one_change_per_token),
                (false, _, Some(remove)) => add_to_path(remove, false, true, 1, one_change_per_token),
                _ => unreachable!("the chosen diagonal always has a path"),
            };
            let new_pos = extract_common(
                &mut base_path,
                new_tokens,
                old_tokens,
                diagonal_path,
                equals,
                one_change_per_token,
            );
            if base_path.old_pos + 1 >= old_len && new_pos + 1 >= new_len {
                // If we have hit the end of both strings, then we are done
                return Some(collect_components(base_path.last_component));
            }
            if base_path.old_pos + 1 >= old_len {
                max_diagonal_to_consider = max_diagonal_to_consider.min(diagonal_path - 1);
            }
            if new_pos + 1 >= new_len {
                min_diagonal_to_consider = min_diagonal_to_consider.max(diagonal_path + 1);
            }
            *best_path.slot(diagonal_path) = Some(base_path);
            diagonal_path += 2;
        }
        edit_length += 1;
    }
    None
}

fn add_to_path(path: &DiffPath, added: bool, removed: bool, old_pos_inc: i64, one_change_per_token: bool) -> DiffPath {
    let last = &path.last_component;
    if let Some(last) = last
        && !one_change_per_token
        && last.added == added
        && last.removed == removed
    {
        return DiffPath {
            old_pos: path.old_pos + old_pos_inc,
            last_component: Some(Rc::new(Component {
                count: last.count + 1,
                added,
                removed,
                previous_component: last.previous_component.clone(),
            })),
        };
    }
    DiffPath {
        old_pos: path.old_pos + old_pos_inc,
        last_component: Some(Rc::new(Component {
            count: 1,
            added,
            removed,
            previous_component: last.clone(),
        })),
    }
}

fn extract_common<T>(
    base_path: &mut DiffPath,
    new_tokens: &[T],
    old_tokens: &[T],
    diagonal_path: i64,
    equals: &dyn Fn(&T, &T) -> bool,
    one_change_per_token: bool,
) -> i64 {
    let new_len = new_tokens.len() as i64;
    let old_len = old_tokens.len() as i64;
    let mut old_pos = base_path.old_pos;
    let mut new_pos = old_pos - diagonal_path;
    let mut common_count = 0usize;
    while new_pos + 1 < new_len
        && old_pos + 1 < old_len
        && equals(&old_tokens[(old_pos + 1) as usize], &new_tokens[(new_pos + 1) as usize])
    {
        new_pos += 1;
        old_pos += 1;
        common_count += 1;
        if one_change_per_token {
            base_path.last_component = Some(Rc::new(Component {
                count: 1,
                previous_component: base_path.last_component.take(),
                added: false,
                removed: false,
            }));
        }
    }
    if common_count > 0 && !one_change_per_token {
        base_path.last_component = Some(Rc::new(Component {
            count: common_count,
            previous_component: base_path.last_component.take(),
            added: false,
            removed: false,
        }));
    }
    base_path.old_pos = old_pos;
    new_pos
}

fn collect_components(last_component: Option<Rc<Component>>) -> Vec<RawComponent> {
    // First we convert our linked list of components in reverse order to an
    // array in the right order:
    let mut components = Vec::new();
    let mut next = last_component.as_deref();
    while let Some(component) = next {
        components.push(RawComponent {
            count: component.count,
            added: component.added,
            removed: component.removed,
        });
        next = component.previous_component.as_deref();
    }
    components.reverse();
    components
}

// PORT: `Diff#removeEmpty` for string tokens.
pub(crate) fn remove_empty_strings(tokens: Vec<String>) -> Vec<String> {
    tokens.into_iter().filter(|t| !t.is_empty()).collect()
}

// PORT: `Diff#equals` for string tokens, with `options.ignoreCase` passed in.
pub(crate) fn base_equals_str(left: &str, right: &str, ignore_case: bool) -> bool {
    left == right || (ignore_case && left.to_lowercase() == right.to_lowercase())
}
