//! Three-pass copy-on-write value coercion: optional null dropping, TypeBox `Value.Convert` rules, and
//! Ajv-style primitive coercion by `type`.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::hash::Hasher;

use indexmap::IndexMap;
use serde_json::{Map, Value};

use crate::arena::{NodeId, deep_equal_values, format_number, hash_value, is_finite_number, is_integer_number};
use crate::compiler::Compiler;
use crate::shared::shared_regex;

/// Coerces loosely typed values toward a compiled schema.
pub(crate) struct Coercer {
    c: Compiler,
}

#[derive(Default)]
struct CoerceState {
    entered: Vec<(NodeId, Value)>,
    cuts: usize,
    memo: HashMap<NodeId, Vec<(u64, Value, Value)>>,
    scope: Vec<String>,
}

impl Coercer {
    pub(crate) fn new(c: Compiler) -> Self {
        Self { c }
    }

    pub(crate) fn coerce(&self, value: &Value) -> Value {
        let base = &self.c.r.root_base;
        let mut scope = Vec::new();
        let without_nulls = self.drop_optional_nulls(value, self.c.r.root, base, &mut scope);
        let converted = self.convert_only_cow(without_nulls.as_ref());
        self.coerce_types(converted.as_ref(), self.c.r.root, base, &mut scope)
            .into_owned()
    }

    /// Pass 2 alone, matching TypeBox's `Value.Convert`.
    pub(crate) fn convert_only(&self, value: &Value) -> Value {
        self.convert_only_cow(value).into_owned()
    }

    fn convert_only_cow<'a>(&self, value: &'a Value) -> Cow<'a, Value> {
        let mut state = CoerceState::default();
        self.convert(self.c.r.root, &self.c.r.root_base, value, &mut state)
    }

    fn passes(&self, node: NodeId, value: &Value, scope: &mut Vec<String>) -> Option<bool> {
        self.c.ok_readonly(node, value, scope)
    }

    fn base_of(&self, node: NodeId, base: &str) -> String {
        if self.c.r.arena.is_schema_object(node) {
            self.c.r.base_for_readonly(node, base)
        } else {
            base.to_string()
        }
    }

    // ---------------------------------------------------------------------------------------------------------------
    // Pass 1: optional nulls
    // ---------------------------------------------------------------------------------------------------------------

    fn drop_optional_nulls<'a>(
        &self,
        value: &'a Value,
        schema: NodeId,
        base: &str,
        scope: &mut Vec<String>,
    ) -> Cow<'a, Value> {
        let Some(schema_obj) = self.c.r.arena.as_object(schema) else {
            return Cow::Borrowed(value);
        };
        let b = self.base_of(schema, base);
        if let Value::Array(items) = value {
            return map_items(value, items, |item, i| match self.item_schema_at(schema_obj, i) {
                Some(item_schema) => self.drop_optional_nulls(item, item_schema, &b, scope),
                None => Cow::Borrowed(item),
            });
        }
        let Value::Object(obj) = value else {
            return Cow::Borrowed(value);
        };
        let props = schema_obj
            .get("properties")
            .and_then(|&id| self.c.r.arena.as_object(id));
        let pats = schema_obj
            .get("patternProperties")
            .and_then(|&id| self.c.r.arena.as_object(id))
            .map(|m| {
                m.iter()
                    .filter_map(|(pat, &s)| shared_regex(pat).ok().map(|re| (re, s)))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let additional = schema_obj
            .get("additionalProperties")
            .copied()
            .filter(|&id| self.c.r.arena.is_schema_object(id));

        if pats.is_empty() && additional.is_none() && props.is_none() {
            return Cow::Borrowed(value);
        }

        let required: HashSet<&str> = schema_obj
            .get("required")
            .and_then(|&id| self.c.r.arena.as_array(id))
            .map(|arr| arr.iter().filter_map(|&id| self.c.r.arena.as_str(id)).collect())
            .unwrap_or_default();

        let mut out: Option<Map<String, Value>> = None;
        if let Some(props_map) = props {
            for (key, &prop_schema) in props_map {
                let Some(existing) = obj.get(key) else {
                    continue;
                };
                let is_ref = self.c.r.arena.str_prop(prop_schema, "$ref").is_some();
                if existing.is_null()
                    && !required.contains(key.as_str())
                    && !is_ref
                    && self.passes(prop_schema, &Value::Null, scope) == Some(false)
                {
                    let map = out.get_or_insert_with(|| obj.clone());
                    map.remove(key);
                    continue;
                }
                let next = self.drop_optional_nulls(existing, prop_schema, &b, scope);
                if let Cow::Owned(new_val) = next {
                    let map = out.get_or_insert_with(|| obj.clone());
                    map.insert(key.clone(), new_val);
                }
            }
        }

        if !pats.is_empty() || additional.is_some() {
            for (key, orig_val) in obj {
                let mut matched = props.is_some_and(|p| p.contains_key(key));
                for (re, item_schema) in &pats {
                    if !pi_js::regex::test(re, key) {
                        continue;
                    }
                    matched = true;
                    let cur = out.as_ref().and_then(|m| m.get(key)).unwrap_or(orig_val);
                    if let Cow::Owned(new_val) = self.drop_optional_nulls(cur, *item_schema, &b, scope) {
                        let map = out.get_or_insert_with(|| obj.clone());
                        map.insert(key.clone(), new_val);
                    }
                }
                if !matched
                    && let Some(add_schema) = additional
                    && let Cow::Owned(new_val) = self.drop_optional_nulls(orig_val, add_schema, &b, scope)
                {
                    let map = out.get_or_insert_with(|| obj.clone());
                    map.insert(key.clone(), new_val);
                }
            }
        }

        match out {
            Some(map) => Cow::Owned(Value::Object(map)),
            None => Cow::Borrowed(value),
        }
    }

    // ---------------------------------------------------------------------------------------------------------------
    // Pass 2: TypeBox conversion rules
    // ---------------------------------------------------------------------------------------------------------------

    fn reenter<'a>(
        &self,
        node: NodeId,
        base: &str,
        value: &'a Value,
        memoize: bool,
        state: &mut CoerceState,
    ) -> Cow<'a, Value> {
        for (entered_node, entered_val) in &state.entered {
            if *entered_node == node && deep_equal_values(entered_val, value) {
                state.cuts += 1;
                return Cow::Borrowed(value);
            }
        }
        let val_hash = if memoize { Some(value_hash_u64(value)) } else { None };
        if let Some(h) = val_hash
            && let Some(entries) = state.memo.get(&node)
        {
            for (eh, ev, eres) in entries {
                if *eh == h && deep_equal_values(ev, value) {
                    return Cow::Owned(eres.clone());
                }
            }
        }

        let cuts = state.cuts;
        state.entered.push((node, value.clone()));
        let result = self.convert(node, base, value, state);
        state.entered.pop();

        if let Some(h) = val_hash
            && state.cuts == cuts
        {
            state
                .memo
                .entry(node)
                .or_default()
                .push((h, value.clone(), result.as_ref().clone()));
        }
        result
    }

    fn convert<'a>(&self, node: NodeId, base: &str, value: &'a Value, state: &mut CoerceState) -> Cow<'a, Value> {
        let Some(obj) = self.c.r.arena.as_object(node) else {
            return Cow::Borrowed(value);
        };
        let b = self.base_of(node, base);
        if let Some(&ref_id) = obj.get("$ref")
            && let Some(ref_str) = self.c.r.arena.as_str(ref_id)
        {
            let Some(target) = self.c.r.resolve_readonly(ref_str, &b) else {
                return Cow::Borrowed(value);
            };
            if self.c.r.arena.is_schema_object(target.node) {
                return self.reenter(target.node, &target.base, value, true, state);
            }
            return Cow::Borrowed(value);
        }
        self.convert_node(node, obj, &b, value, state)
    }

    fn convert_node<'a>(
        &self,
        node: NodeId,
        obj: &IndexMap<String, NodeId>,
        b: &str,
        value: &'a Value,
        state: &mut CoerceState,
    ) -> Cow<'a, Value> {
        if let Some(&const_id) = obj.get("const") {
            let literal = self.c.r.arena.to_value(const_id);
            return convert_literal(&literal, value);
        }
        if let Some(&enum_id) = obj.get("enum")
            && let Some(items) = self.c.r.arena.as_array(enum_id)
        {
            return self.convert_enum(node, items, value, state);
        }
        if let Some(&any_of_id) = obj.get("anyOf")
            && let Some(branches) = self.c.r.arena.as_array(any_of_id)
        {
            return self.convert_union(node, obj, branches, b, value, state);
        }
        if let Some(&one_of_id) = obj.get("oneOf")
            && let Some(branches) = self.c.r.arena.as_array(one_of_id)
        {
            return self.convert_union(node, obj, branches, b, value, state);
        }
        if let Some(&all_of_id) = obj.get("allOf")
            && let Some(members) = self.c.r.arena.as_array(all_of_id)
        {
            let mut next = Cow::Borrowed(value);
            for &member in members {
                let converted = self.convert(member, b, next.as_ref(), state);
                if let Cow::Owned(owned) = converted {
                    next = Cow::Owned(owned);
                }
            }
            return match next {
                Cow::Borrowed(v) => self.convert_members(obj, b, v, state),
                Cow::Owned(v) => Cow::Owned(self.convert_members(obj, b, &v, state).into_owned()),
            };
        }

        match obj.get("type") {
            Some(&tid) => match self.c.r.arena.as_str(tid) {
                Some("string") => try_string(value).map(Cow::Owned).unwrap_or(Cow::Borrowed(value)),
                Some("number") => try_number(value).map(Cow::Owned).unwrap_or(Cow::Borrowed(value)),
                Some("integer") => match try_number(value) {
                    Some(Value::Number(n)) => {
                        if is_integer_number(&n) {
                            Cow::Owned(Value::Number(n))
                        } else if let Some(f) = n.as_f64() {
                            Cow::Owned(pi_js::json::number_value(f.trunc()))
                        } else {
                            Cow::Borrowed(value)
                        }
                    }
                    _ => Cow::Borrowed(value),
                },
                Some("boolean") => try_boolean(value).map(Cow::Owned).unwrap_or(Cow::Borrowed(value)),
                Some("null") => try_null(value).map(Cow::Owned).unwrap_or(Cow::Borrowed(value)),
                Some("array") => self.convert_array(obj, b, value, state),
                Some("object") => self.convert_object(obj, b, value, state),
                _ => Cow::Borrowed(value),
            },
            None => {
                if obj.contains_key("properties") || obj.contains_key("patternProperties") {
                    self.convert_object(obj, b, value, state)
                } else if obj.contains_key("items") || obj.contains_key("prefixItems") {
                    self.convert_array(obj, b, value, state)
                } else {
                    Cow::Borrowed(value)
                }
            }
        }
    }

    fn convert_enum<'a>(
        &self,
        node: NodeId,
        items: &[NodeId],
        value: &'a Value,
        state: &mut CoerceState,
    ) -> Cow<'a, Value> {
        let members: Vec<Value> = items.iter().map(|&id| self.c.r.arena.to_value(id)).collect();
        if members.iter().any(|m| deep_equal_values(m, value)) {
            return Cow::Borrowed(value);
        }
        for member in &members {
            if let Cow::Owned(candidate) = convert_literal(member, value)
                && self.passes(node, &candidate, &mut state.scope) == Some(true)
            {
                return Cow::Owned(candidate);
            }
        }
        Cow::Borrowed(value)
    }

    fn convert_union<'a>(
        &self,
        node: NodeId,
        obj: &IndexMap<String, NodeId>,
        branches: &[NodeId],
        base: &str,
        value: &'a Value,
        state: &mut CoerceState,
    ) -> Cow<'a, Value> {
        if branches
            .iter()
            .any(|&branch| self.passes(branch, value, &mut state.scope) == Some(true))
        {
            return self.convert_members(obj, base, value, state);
        }
        for &branch in branches {
            let converted_branch = self.convert(branch, base, value, state);
            let candidate = self.convert_members(obj, base, converted_branch.as_ref(), state);
            if self.passes(node, candidate.as_ref(), &mut state.scope) == Some(true) {
                return Cow::Owned(candidate.into_owned());
            }
        }
        self.convert_members(obj, base, value, state)
    }

    fn convert_members<'a>(
        &self,
        obj: &IndexMap<String, NodeId>,
        base: &str,
        value: &'a Value,
        state: &mut CoerceState,
    ) -> Cow<'a, Value> {
        if obj.contains_key("properties")
            || obj.contains_key("patternProperties")
            || obj.contains_key("additionalProperties")
        {
            self.convert_object(obj, base, value, state)
        } else {
            Cow::Borrowed(value)
        }
    }

    fn convert_array<'a>(
        &self,
        obj: &IndexMap<String, NodeId>,
        base: &str,
        value: &'a Value,
        state: &mut CoerceState,
    ) -> Cow<'a, Value> {
        let items_id = obj.get("items").copied();
        let items_arr = items_id.and_then(|id| self.c.r.arena.as_array(id));
        let prefix_arr = items_arr.or_else(|| obj.get("prefixItems").and_then(|&id| self.c.r.arena.as_array(id)));

        if let Some(prefix) = prefix_arr {
            let Value::Array(arr) = value else {
                return Cow::Borrowed(value);
            };
            let rest = if items_arr.is_some() { None } else { items_id };
            return map_items(value, arr, |item, i| {
                let schema = if i < prefix.len() { Some(prefix[i]) } else { rest };
                match schema {
                    Some(s) => self.convert(s, base, item, state),
                    None => Cow::Borrowed(item),
                }
            });
        }

        if let Value::Array(arr) = value {
            return match items_id {
                Some(item_schema) => map_items(value, arr, |item, _| self.convert(item_schema, base, item, state)),
                None => Cow::Borrowed(value),
            };
        }

        let wrapped = match items_id {
            Some(item_schema) if self.c.r.arena.is_schema_object(item_schema) => {
                self.reenter(item_schema, base, value, false, state).into_owned()
            }
            _ => value.clone(),
        };
        Cow::Owned(Value::Array(vec![wrapped]))
    }

    fn convert_object<'a>(
        &self,
        obj: &IndexMap<String, NodeId>,
        base: &str,
        value: &'a Value,
        state: &mut CoerceState,
    ) -> Cow<'a, Value> {
        let Value::Object(map) = value else {
            return Cow::Borrowed(value);
        };
        let props = obj.get("properties").and_then(|&id| self.c.r.arena.as_object(id));
        let patterns = obj
            .get("patternProperties")
            .and_then(|&id| self.c.r.arena.as_object(id))
            .map(|m| {
                m.iter()
                    .filter_map(|(pat, &s)| shared_regex(pat).ok().map(|re| (re, s)))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let additional = obj
            .get("additionalProperties")
            .copied()
            .filter(|&id| self.c.r.arena.is_schema_object(id));

        let mut out: Option<Map<String, Value>> = None;
        for (key, orig_val) in map {
            let mut next = Cow::Borrowed(orig_val);
            let mut matched = false;
            if let Some(props_map) = props
                && let Some(&prop_schema) = props_map.get(key)
            {
                matched = true;
                next = self.convert(prop_schema, base, orig_val, state);
            }
            for (re, pat_schema) in &patterns {
                if !pi_js::regex::test(re, key) {
                    continue;
                }
                matched = true;
                let converted = self.convert(*pat_schema, base, next.as_ref(), state);
                if let Cow::Owned(owned) = converted {
                    next = Cow::Owned(owned);
                }
            }
            if !matched && let Some(add_schema) = additional {
                let converted = self.convert(add_schema, base, next.as_ref(), state);
                if let Cow::Owned(owned) = converted {
                    next = Cow::Owned(owned);
                }
            }
            if next.as_ref() != orig_val {
                let out_map = out.get_or_insert_with(|| map.clone());
                out_map.insert(key.clone(), next.into_owned());
            }
        }

        match out {
            Some(m) => Cow::Owned(Value::Object(m)),
            None => Cow::Borrowed(value),
        }
    }

    // ---------------------------------------------------------------------------------------------------------------
    // Pass 3: primitive coercion by type
    // ---------------------------------------------------------------------------------------------------------------

    fn coerce_types<'a>(
        &self,
        value: &'a Value,
        schema: NodeId,
        base: &str,
        scope: &mut Vec<String>,
    ) -> Cow<'a, Value> {
        let Some(obj) = self.c.r.arena.as_object(schema) else {
            return Cow::Borrowed(value);
        };
        let b = self.base_of(schema, base);
        let mut next = Cow::Borrowed(value);

        if let Some(&all_of_id) = obj.get("allOf")
            && let Some(members) = self.c.r.arena.as_array(all_of_id)
        {
            for &member in members {
                if let Cow::Owned(owned) = self.coerce_types(next.as_ref(), member, &b, scope) {
                    next = Cow::Owned(owned);
                }
            }
        }
        if let Some(&any_of_id) = obj.get("anyOf")
            && let Some(branches) = self.c.r.arena.as_array(any_of_id)
            && let Cow::Owned(owned) = self.coerce_union(next.as_ref(), branches, &b, scope)
        {
            next = Cow::Owned(owned);
        }
        if let Some(&one_of_id) = obj.get("oneOf")
            && let Some(branches) = self.c.r.arena.as_array(one_of_id)
            && let Cow::Owned(owned) = self.coerce_union(next.as_ref(), branches, &b, scope)
        {
            next = Cow::Owned(owned);
        }

        let types = self.types_of(obj);
        let matches_union_member = types.len() > 1 && types.iter().any(|t| matches_type(next.as_ref(), t));
        if !types.is_empty() && !matches_union_member {
            for t in &types {
                if let Some(candidate) = coerce_primitive(next.as_ref(), t) {
                    next = Cow::Owned(candidate);
                    break;
                }
            }
        }

        if types.contains(&"object") && next.is_object() {
            next = match next {
                Cow::Borrowed(v) => self.coerce_properties(v, obj, &b, scope),
                Cow::Owned(v) => match self.coerce_properties(&v, obj, &b, scope) {
                    Cow::Borrowed(_) => Cow::Owned(v),
                    Cow::Owned(new_v) => Cow::Owned(new_v),
                },
            };
        }

        if types.contains(&"array") && next.is_array() {
            next = match next {
                Cow::Borrowed(v) => self.coerce_items(v, obj, &b, scope),
                Cow::Owned(v) => match self.coerce_items(&v, obj, &b, scope) {
                    Cow::Borrowed(_) => Cow::Owned(v),
                    Cow::Owned(new_v) => Cow::Owned(new_v),
                },
            };
        }

        next
    }

    fn coerce_union<'a>(
        &self,
        value: &'a Value,
        branches: &[NodeId],
        base: &str,
        scope: &mut Vec<String>,
    ) -> Cow<'a, Value> {
        for &branch in branches {
            if self.passes(branch, value, scope) == Some(true) {
                return Cow::Borrowed(value);
            }
        }
        for &branch in branches {
            let coerced = self.coerce_types(value, branch, base, scope);
            if self.passes(branch, coerced.as_ref(), scope) == Some(true) {
                return coerced;
            }
        }
        Cow::Borrowed(value)
    }

    fn coerce_properties<'a>(
        &self,
        orig_value: &'a Value,
        schema_obj: &IndexMap<String, NodeId>,
        base: &str,
        scope: &mut Vec<String>,
    ) -> Cow<'a, Value> {
        let Some(value) = orig_value.as_object() else {
            return Cow::Borrowed(orig_value);
        };
        let props = schema_obj
            .get("properties")
            .and_then(|&id| self.c.r.arena.as_object(id));
        let patterns = schema_obj
            .get("patternProperties")
            .and_then(|&id| self.c.r.arena.as_object(id))
            .map(|m| {
                m.iter()
                    .filter_map(|(pat, &s)| shared_regex(pat).ok().map(|re| (re, s)))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let additional = schema_obj
            .get("additionalProperties")
            .copied()
            .filter(|&id| self.c.r.arena.is_schema_object(id));

        let mut out: Option<Map<String, Value>> = None;
        if let Some(props_map) = props {
            for (key, &prop_schema) in props_map {
                if let Some(orig) = value.get(key)
                    && let Cow::Owned(new_val) = self.coerce_types(orig, prop_schema, base, scope)
                    && new_val != *orig
                {
                    let map = out.get_or_insert_with(|| value.clone());
                    map.insert(key.clone(), new_val);
                }
            }
        }
        for (key, orig) in value {
            let mut matched = props.is_some_and(|p| p.contains_key(key));
            for (re, pat_schema) in &patterns {
                if !pi_js::regex::test(re, key) {
                    continue;
                }
                matched = true;
                let cur = out.as_ref().and_then(|m| m.get(key)).unwrap_or(orig);
                if let Cow::Owned(new_val) = self.coerce_types(cur, *pat_schema, base, scope)
                    && new_val != *cur
                {
                    let map = out.get_or_insert_with(|| value.clone());
                    map.insert(key.clone(), new_val);
                }
            }
            if !matched
                && let Some(add_schema) = additional
                && let Cow::Owned(new_val) = self.coerce_types(orig, add_schema, base, scope)
                && new_val != *orig
            {
                let map = out.get_or_insert_with(|| value.clone());
                map.insert(key.clone(), new_val);
            }
        }

        match out {
            Some(map) => Cow::Owned(Value::Object(map)),
            None => Cow::Borrowed(orig_value),
        }
    }

    fn coerce_items<'a>(
        &self,
        orig_value: &'a Value,
        schema_obj: &IndexMap<String, NodeId>,
        base: &str,
        scope: &mut Vec<String>,
    ) -> Cow<'a, Value> {
        let Some(items) = orig_value.as_array() else {
            return Cow::Borrowed(orig_value);
        };
        map_items(orig_value, items, |item, i| match self.item_schema_at(schema_obj, i) {
            Some(item_schema) if self.c.r.arena.is_schema_object(item_schema) => {
                self.coerce_types(item, item_schema, base, scope)
            }
            _ => Cow::Borrowed(item),
        })
    }

    fn item_schema_at(&self, obj: &IndexMap<String, NodeId>, index: usize) -> Option<NodeId> {
        if let Some(&items_id) = obj.get("items")
            && let Some(arr) = self.c.r.arena.as_array(items_id)
        {
            return arr.get(index).copied();
        }
        if let Some(&prefix_id) = obj.get("prefixItems")
            && let Some(arr) = self.c.r.arena.as_array(prefix_id)
            && index < arr.len()
        {
            return Some(arr[index]);
        }
        obj.get("items").copied()
    }

    fn types_of<'a>(&'a self, obj: &IndexMap<String, NodeId>) -> Vec<&'a str> {
        let Some(&type_id) = obj.get("type") else {
            return Vec::new();
        };
        if let Some(s) = self.c.r.arena.as_str(type_id) {
            return vec![s];
        }
        if let Some(arr) = self.c.r.arena.as_array(type_id) {
            return arr.iter().filter_map(|&id| self.c.r.arena.as_str(id)).collect();
        }
        Vec::new()
    }
}

fn value_hash_u64(v: &Value) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    hash_value(v, &mut h);
    h.finish()
}

fn map_items<'a>(
    orig_value: &'a Value,
    items: &'a [Value],
    mut f: impl FnMut(&'a Value, usize) -> Cow<'a, Value>,
) -> Cow<'a, Value> {
    let mut out: Option<Vec<Value>> = None;
    for (i, item) in items.iter().enumerate() {
        let next = f(item, i);
        if next.as_ref() != item {
            let vec = out.get_or_insert_with(|| items.to_vec());
            vec[i] = next.into_owned();
        }
    }
    match out {
        Some(v) => Cow::Owned(Value::Array(v)),
        None => Cow::Borrowed(orig_value),
    }
}

// -------------------------------------------------------------------------------------------------------------------
// TypeBox conversion rules (value/convert/try in TypeBox 1.3.27)
// -------------------------------------------------------------------------------------------------------------------

fn convert_literal<'a>(literal: &Value, value: &'a Value) -> Cow<'a, Value> {
    if deep_equal_values(literal, value) {
        return Cow::Borrowed(value);
    }
    let converted = match literal {
        Value::String(_) => try_string(value),
        Value::Number(_) => try_number(value),
        Value::Bool(_) => try_boolean(value),
        Value::Null => try_null(value),
        _ => None,
    };
    match converted {
        Some(cand) if deep_equal_values(literal, &cand) => Cow::Owned(cand),
        _ => Cow::Borrowed(value),
    }
}

fn try_string(value: &Value) -> Option<Value> {
    match value {
        Value::Bool(true) => Some(Value::String("true".to_string())),
        Value::Bool(false) => Some(Value::String("false".to_string())),
        Value::Number(n) if is_finite_number(n) => Some(Value::String(format_number(n))),
        Value::Null => Some(Value::String("null".to_string())),
        Value::String(_) => Some(value.clone()),
        _ => None,
    }
}

const MAX_SAFE_INT_I128: i128 = 9_007_199_254_740_991;

fn try_number(value: &Value) -> Option<Value> {
    match value {
        Value::Bool(b) => Some(pi_js::json::number_value(if *b { 1.0 } else { 0.0 })),
        Value::Number(n) if is_finite_number(n) => Some(Value::Number(n.clone())),
        Value::Null => Some(pi_js::json::number_value(0.0)),
        Value::String(s) => {
            let coerced = pi_js::num::number(s);
            if coerced.is_finite() {
                return Some(pi_js::json::number_value(coerced));
            }
            if s.eq_ignore_ascii_case("false") {
                return Some(pi_js::json::number_value(0.0));
            }
            if s.eq_ignore_ascii_case("true") {
                return Some(pi_js::json::number_value(1.0));
            }
            if let Some(big) = bigint_of_string(s)
                && (-MAX_SAFE_INT_I128..=MAX_SAFE_INT_I128).contains(&big)
            {
                return Some(pi_js::json::number_value(big as f64));
            }
            None
        }
        _ => None,
    }
}

fn bigint_of_string(value: &str) -> Option<i128> {
    if let Some(body) = value.strip_suffix('n')
        && is_canonical_int_str(body)
    {
        return body.parse().ok();
    }
    if let Some((int_part, frac_part)) = value.split_once('.')
        && !frac_part.is_empty()
        && frac_part.bytes().all(|b| b.is_ascii_digit())
        && is_canonical_int_str(int_part)
    {
        return int_part.parse().ok();
    }
    if is_canonical_int_str(value) {
        return value.parse().ok();
    }
    None
}

fn is_canonical_int_str(s: &str) -> bool {
    let rest = s.strip_prefix('-').unwrap_or(s);
    if rest == "0" {
        return true;
    }
    let bytes = rest.as_bytes();
    !bytes.is_empty() && (b'1'..=b'9').contains(&bytes[0]) && bytes.iter().all(u8::is_ascii_digit)
}

fn try_boolean(value: &Value) -> Option<Value> {
    match value {
        Value::Bool(_) => Some(value.clone()),
        Value::Number(n) if is_finite_number(n) => match n.as_f64() {
            Some(0.0) => Some(Value::Bool(false)),
            Some(1.0) => Some(Value::Bool(true)),
            _ => None,
        },
        Value::Null => Some(Value::Bool(false)),
        Value::String(s) => {
            if s.eq_ignore_ascii_case("false") || s == "0" {
                Some(Value::Bool(false))
            } else if s.eq_ignore_ascii_case("true") || s == "1" {
                Some(Value::Bool(true))
            } else {
                None
            }
        }
        _ => None,
    }
}

fn try_null(value: &Value) -> Option<Value> {
    match value {
        Value::Bool(false) | Value::Null => Some(Value::Null),
        Value::Number(n) if is_finite_number(n) && n.as_f64() == Some(0.0) => Some(Value::Null),
        Value::String(s) => {
            if s.eq_ignore_ascii_case("undefined") || s.eq_ignore_ascii_case("null") || s.is_empty() || s == "0" {
                Some(Value::Null)
            } else {
                None
            }
        }
        _ => None,
    }
}

// -------------------------------------------------------------------------------------------------------------------
// Primitive coercion by type (Pass 3)
// -------------------------------------------------------------------------------------------------------------------

fn matches_type(value: &Value, type_name: &str) -> bool {
    match type_name {
        "number" => matches!(value, Value::Number(n) if is_finite_number(n)),
        "integer" => matches!(value, Value::Number(n) if is_integer_number(n)),
        "boolean" => value.is_boolean(),
        "string" => value.is_string(),
        "null" => value.is_null(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        _ => false,
    }
}

fn coerce_primitive(value: &Value, type_name: &str) -> Option<Value> {
    match type_name {
        "number" | "integer" => match value {
            Value::Null => Some(pi_js::json::number_value(0.0)),
            Value::String(s) if !pi_js::str16::trim(s).is_empty() => {
                let parsed = pi_js::num::number(s);
                let ok = if type_name == "number" {
                    parsed.is_finite()
                } else {
                    pi_js::num::is_integer(parsed)
                };
                if ok {
                    Some(pi_js::json::number_value(parsed))
                } else {
                    None
                }
            }
            Value::Bool(b) => Some(pi_js::json::number_value(if *b { 1.0 } else { 0.0 })),
            _ => None,
        },
        "boolean" => match value {
            Value::Null => Some(Value::Bool(false)),
            Value::String(s) if s == "true" => Some(Value::Bool(true)),
            Value::String(s) if s == "false" => Some(Value::Bool(false)),
            Value::Number(n) if n.as_f64() == Some(1.0) => Some(Value::Bool(true)),
            Value::Number(n) if n.as_f64() == Some(0.0) => Some(Value::Bool(false)),
            _ => None,
        },
        "string" => match value {
            Value::Null => Some(Value::String(String::new())),
            Value::Number(n) if is_finite_number(n) => Some(Value::String(format_number(n))),
            Value::Bool(b) => Some(Value::String(if *b { "true" } else { "false" }.to_string())),
            _ => None,
        },
        "null" => match value {
            Value::String(s) if s.is_empty() => Some(Value::Null),
            Value::Number(n) if n.as_f64() == Some(0.0) => Some(Value::Null),
            Value::Bool(false) => Some(Value::Null),
            _ => None,
        },
        _ => None,
    }
}
