//! Annotation tracking for Draft 2020-12 `unevaluatedProperties` and `unevaluatedItems`.

use std::collections::HashSet;
use std::sync::{Arc, OnceLock};

use serde_json::Value;

use crate::checks::{Check, FastCheck};

/// Evaluated property names, evaluated item prefix length, and `contains`-matched item indices.
#[derive(Clone, Debug, Default)]
pub(crate) struct Annotations {
    pub(crate) props: HashSet<String>,
    pub(crate) all_props: bool,
    pub(crate) items: usize,
    pub(crate) all_items: bool,
    pub(crate) indexes: HashSet<usize>,
}

impl Annotations {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn merge(&mut self, other: &Annotations) {
        if !other.props.is_empty() {
            self.props.extend(other.props.iter().cloned());
        }
        self.all_props |= other.all_props;
        if other.items > self.items {
            self.items = other.items;
        }
        self.all_items |= other.all_items;
        if !other.indexes.is_empty() {
            self.indexes.extend(other.indexes.iter().copied());
        }
    }
}

/// Thread-safe annotating check tracking evaluated properties/items into `Annotations` when it passes.
pub(crate) type AnnotatingCheck = Arc<dyn Fn(&Value, &mut Annotations, &mut Vec<String>) -> bool + Send + Sync>;

pub(crate) fn annotating_true() -> AnnotatingCheck {
    static TRUE: OnceLock<AnnotatingCheck> = OnceLock::new();
    TRUE.get_or_init(|| Arc::new(|_, _, _| true)).clone()
}

pub(crate) fn annotating_false() -> AnnotatingCheck {
    static FALSE: OnceLock<AnnotatingCheck> = OnceLock::new();
    FALSE.get_or_init(|| Arc::new(|_, _, _| false)).clone()
}

pub(crate) fn indirect_annotating(slot: Arc<OnceLock<AnnotatingCheck>>) -> AnnotatingCheck {
    Arc::new(move |v, a, s| slot.get().is_some_and(|c| c(v, a, s)))
}

pub(crate) fn ignore_annotations(check: FastCheck) -> AnnotatingCheck {
    Arc::new(move |v, _, s| check.run(v, s))
}

pub(crate) fn with_fresh_annotations(check: AnnotatingCheck) -> Check {
    Arc::new(move |v, s| {
        let mut a = Annotations::new();
        check(v, &mut a, s)
    })
}

pub(crate) fn scoped_annotating(uri: String, check: AnnotatingCheck) -> AnnotatingCheck {
    Arc::new(move |v, a, s| {
        s.push(uri.clone());
        let ok = check(v, a, s);
        s.pop();
        ok
    })
}

pub(crate) fn isolate_annotations(check: AnnotatingCheck) -> AnnotatingCheck {
    Arc::new(move |v, a, s| {
        let mut b = Annotations::new();
        if !check(v, &mut b, s) {
            return false;
        }
        a.merge(&b);
        true
    })
}

pub(crate) fn annotate_all(plain: FastCheck, parts: Vec<AnnotatingCheck>) -> AnnotatingCheck {
    Arc::new(move |v, a, s| {
        if !plain.run(v, s) {
            return false;
        }
        for part in &parts {
            if !part(v, a, s) {
                return false;
            }
        }
        true
    })
}

pub(crate) fn annotate_object(
    props: Vec<(String, FastCheck)>,
    patterns: Vec<(Arc<regress::Regex>, FastCheck)>,
    additional: Option<FastCheck>,
) -> AnnotatingCheck {
    let declared: HashSet<String> = props.iter().map(|(k, _)| k.clone()).collect();
    Arc::new(move |v, a, s| {
        let Some(map) = v.as_object() else {
            return true;
        };
        let mut seen = Vec::new();
        for (k, check) in &props {
            let Some(val) = map.get(k) else {
                continue;
            };
            if !check.run(val, s) {
                return false;
            }
            seen.push(k.clone());
        }
        for (k, val) in map {
            let mut matched = declared.contains(k);
            for (re, check) in &patterns {
                if pi_js::regex::test(re, k) {
                    if !check.run(val, s) {
                        return false;
                    }
                    matched = true;
                    seen.push(k.clone());
                }
            }
            if !matched && let Some(add) = &additional {
                if !add.run(val, s) {
                    return false;
                }
                seen.push(k.clone());
            }
        }
        a.props.extend(seen);
        true
    })
}

pub(crate) fn annotate_array(
    prefix: Vec<FastCheck>,
    rest: Option<FastCheck>,
    contains: Option<FastCheck>,
    min_contains: f64,
    max_contains: f64,
) -> AnnotatingCheck {
    Arc::new(move |v, a, s| {
        let Some(arr) = v.as_array() else {
            return true;
        };
        let m = arr.len().min(prefix.len());
        for i in 0..m {
            if !prefix[i].run(&arr[i], s) {
                return false;
            }
        }
        if let Some(r) = &rest {
            for item in arr.iter().skip(prefix.len()) {
                if !r.run(item, s) {
                    return false;
                }
            }
            a.all_items = true;
        }
        if m > a.items {
            a.items = m;
        }
        if let Some(c) = &contains {
            let mut n = 0usize;
            for (i, item) in arr.iter().enumerate() {
                if c.run(item, s) {
                    n += 1;
                    a.indexes.insert(i);
                }
            }
            let nf = n as f64;
            if nf < min_contains || nf > max_contains {
                return false;
            }
        }
        true
    })
}

pub(crate) fn annotate_any(branches: Vec<AnnotatingCheck>, exactly_one: bool) -> AnnotatingCheck {
    Arc::new(move |v, a, s| {
        let mut n = 0usize;
        for branch in &branches {
            let mut b = Annotations::new();
            if branch(v, &mut b, s) {
                n += 1;
                a.merge(&b);
            }
        }
        if exactly_one { n == 1 } else { n > 0 }
    })
}

pub(crate) fn annotate_if(
    cond: AnnotatingCheck,
    then: Option<AnnotatingCheck>,
    otherwise: Option<AnnotatingCheck>,
) -> AnnotatingCheck {
    Arc::new(move |v, a, s| {
        let mut b = Annotations::new();
        if cond(v, &mut b, s) {
            a.merge(&b);
            then.as_ref().is_none_or(|t| t(v, a, s))
        } else {
            otherwise.as_ref().is_none_or(|o| o(v, a, s))
        }
    })
}

pub(crate) fn annotate_dependent(key: String, check: AnnotatingCheck) -> AnnotatingCheck {
    Arc::new(move |v, a, s| {
        let Some(map) = v.as_object() else {
            return true;
        };
        !map.contains_key(&key) || check(v, a, s)
    })
}

pub(crate) fn unevaluated_properties(check: FastCheck) -> AnnotatingCheck {
    Arc::new(move |v, a, s| {
        let Some(map) = v.as_object() else {
            return true;
        };
        if a.all_props {
            return true;
        }
        for (k, val) in map {
            if !a.props.contains(k) && !check.run(val, s) {
                return false;
            }
        }
        a.all_props = true;
        true
    })
}

pub(crate) fn unevaluated_items(check: FastCheck) -> AnnotatingCheck {
    Arc::new(move |v, a, s| {
        let Some(arr) = v.as_array() else {
            return true;
        };
        if a.all_items {
            return true;
        }
        for (i, val) in arr.iter().enumerate().skip(a.items) {
            if !a.indexes.contains(&i) && !check.run(val, s) {
                return false;
            }
        }
        a.all_items = true;
        true
    })
}
