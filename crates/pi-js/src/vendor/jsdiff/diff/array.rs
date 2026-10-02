//! Port of `diff/libesm/diff/array.js` (diff@8.0.4).

use std::marker::PhantomData;

use crate::vendor::jsdiff::diff::base::Diff;
use crate::vendor::jsdiff::types::{AbortableDiffOptions, ArrayChange, DiffArraysOptions, truthy};

// PORT: `ArrayDiff`: every array element is a token.
#[derive(Clone, Copy, Debug, Default)]
pub struct ArrayDiff<T>(PhantomData<T>);

impl<T> ArrayDiff<T> {
    pub fn new() -> Self {
        Self(PhantomData)
    }
}

impl<T: Clone + PartialEq> Diff for ArrayDiff<T> {
    type Value = [T];
    type Output = Vec<T>;
    type Token = T;
    type Options = DiffArraysOptions<T>;

    fn tokenize(&self, value: &[T], _options: &DiffArraysOptions<T>) -> Vec<T> {
        value.to_vec()
    }

    fn remove_empty(&self, tokens: Vec<T>) -> Vec<T> {
        tokens
    }

    fn equals(&self, left: &T, right: &T, options: &DiffArraysOptions<T>) -> bool {
        match &options.comparator {
            Some(comparator) => comparator(left, right),
            // PORT: JS compares elements with `===`; Rust uses `PartialEq` (identical for
            // primitives, structural rather than by identity for objects).
            None => left == right,
        }
    }

    fn join(&self, tokens: &[T]) -> Vec<T> {
        tokens.to_vec()
    }

    fn one_change_per_token(&self, options: &DiffArraysOptions<T>) -> bool {
        truthy(options.one_change_per_token)
    }
}

/// diffs two arrays of tokens, comparing each item for strict equality (===).
/// @returns a list of change objects.
pub fn diff_arrays<T: Clone + PartialEq>(
    old_arr: &[T],
    new_arr: &[T],
    options: Option<DiffArraysOptions<T>>,
) -> Vec<ArrayChange<T>> {
    ArrayDiff::new().diff(old_arr, new_arr, &options.unwrap_or_default())
}

// PORT: `diffArrays` with `maxEditLength` / `timeout`; `None` where jsdiff returns `undefined`.
pub fn diff_arrays_abortable<T: Clone + PartialEq>(
    old_arr: &[T],
    new_arr: &[T],
    options: Option<DiffArraysOptions<T>>,
    abortable: AbortableDiffOptions,
) -> Option<Vec<ArrayChange<T>>> {
    ArrayDiff::new().diff_abortable(old_arr, new_arr, &options.unwrap_or_default(), &abortable)
}
