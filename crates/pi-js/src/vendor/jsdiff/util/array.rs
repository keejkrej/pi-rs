//! Port of `diff/libesm/util/array.js` (diff@8.0.4).

pub fn array_equal<T: PartialEq>(a: &[T], b: &[T]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    array_starts_with(a, b)
}

pub fn array_starts_with<T: PartialEq>(array: &[T], start: &[T]) -> bool {
    if start.len() > array.len() {
        return false;
    }
    start.iter().zip(array).all(|(s, a)| s == a)
}
