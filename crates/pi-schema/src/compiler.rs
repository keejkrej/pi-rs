//! Schema compiler: compiles a JSON Schema tree into plain `Send + Sync` closures once.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use indexmap::IndexMap;
use serde_json::{Number, Value};

use crate::annotations::{
    AnnotatingCheck, annotate_all, annotate_any, annotate_array, annotate_dependent, annotate_if, annotate_object,
    annotating_false, annotating_true, ignore_annotations, indirect_annotating, isolate_annotations, scoped_annotating,
    unevaluated_items, unevaluated_properties, with_fresh_annotations,
};
use crate::arena::{NodeData, NodeId, is_integer_number};
use crate::checks::{
    Check, FastCheck, Guard, NumMode, PrimValue, PropEntry, and_fast, any_check, array_length_check, bare_type_check,
    const_check, contains_check, declared_object, dependent_required_check, dependent_schemas_check, enum_check,
    guard_array, guard_object, if_check, indirect_check, is_array_check, is_object_check, items_check, list_check,
    masked_any_check, not_check, number_check, one_check, open_object, property_count_check, property_names_check,
    scoped_check, string_check, tagged_check, type_check_from_list, unique_check,
};
use crate::resolver::{Resolver, document_of, resolve_uri};
use crate::shared::shared_regex;
use crate::types::{CompileOptions, Draft, FormatCheck, SchemaError};

/// Sentinel error signaling a restart with dynamic scope tracking enabled.
pub(crate) const RESTART_DYNAMIC: &str = "__restart_dynamic__";

fn is_neutral_keyword(k: &str) -> bool {
    matches!(
        k,
        "$schema"
            | "$comment"
            | "title"
            | "description"
            | "default"
            | "examples"
            | "deprecated"
            | "readOnly"
            | "writeOnly"
            | "$defs"
            | "definitions"
            | "contentEncoding"
            | "contentMediaType"
            | "minContains"
            | "maxContains"
            | "$vocabulary"
    )
}

fn is_annotating_keyword(k: &str) -> bool {
    matches!(
        k,
        "properties"
            | "patternProperties"
            | "additionalProperties"
            | "prefixItems"
            | "items"
            | "additionalItems"
            | "contains"
            | "minContains"
            | "maxContains"
            | "allOf"
            | "anyOf"
            | "oneOf"
            | "if"
            | "then"
            | "else"
            | "$ref"
            | "$dynamicRef"
            | "dependentSchemas"
            | "unevaluatedProperties"
            | "unevaluatedItems"
    )
}

const OBJECT_EXTRAS: &[&str] = &[
    "propertyNames",
    "minProperties",
    "maxProperties",
    "dependentRequired",
    "dependentSchemas",
    "dependencies",
];

type DynamicCheckSlot = (String, Arc<OnceLock<HashMap<String, Check>>>);
type DynamicAnnotatingSlot = (String, Arc<OnceLock<HashMap<String, AnnotatingCheck>>>);

pub(crate) struct Compiler {
    pub(crate) r: Resolver,
    pub(crate) draft: Draft,
    pub(crate) formats: Option<IndexMap<String, FormatCheck>>,
    pub(crate) dynamic: bool,
    cache: HashMap<NodeId, Check>,
    fast_cache: HashMap<NodeId, FastCheck>,
    annotating_cache: HashMap<NodeId, AnnotatingCheck>,
    annotating_no_uneval_cache: HashMap<NodeId, AnnotatingCheck>,
    pending_dynamic_checks: Vec<DynamicCheckSlot>,
    pending_dynamic_annotating: Vec<DynamicAnnotatingSlot>,
}

impl Compiler {
    pub(crate) fn new(root_value: &Value, options: &CompileOptions, dynamic: bool) -> Self {
        let r = Resolver::new(root_value, options);
        let draft = r.draft;
        Self {
            r,
            draft,
            formats: options.formats.clone(),
            dynamic,
            cache: HashMap::new(),
            fast_cache: HashMap::new(),
            annotating_cache: HashMap::new(),
            annotating_no_uneval_cache: HashMap::new(),
            pending_dynamic_checks: Vec::new(),
            pending_dynamic_annotating: Vec::new(),
        }
    }

    /// Evaluates a compiled node check without mutating `Compiler`.
    pub(crate) fn ok_readonly(&self, node: NodeId, v: &Value, scope: &mut Vec<String>) -> Option<bool> {
        if let Some(b) = self.r.arena.as_bool(node) {
            return Some(b);
        }
        if let Some(fast) = self.fast_cache.get(&node) {
            return Some(fast.run(v, scope));
        }
        if let Some(check) = self.cache.get(&node) {
            return Some(check(v, scope));
        }
        if let Some(bare) = self.bare_type(node) {
            return Some(bare.run(v, scope));
        }
        None
    }

    pub(crate) fn annotating_no_uneval_readonly(&self, node: NodeId) -> Option<&AnnotatingCheck> {
        self.annotating_no_uneval_cache.get(&node)
    }

    pub(crate) fn compile_root(&mut self) -> Result<Check, CompileError> {
        let root = self.r.root;
        let base = self.r.root_base.clone();
        let check = self.check(root, &base)?;
        if self.dynamic {
            self.finish_dynamic()?;
        }
        Ok(check)
    }

    fn finish_dynamic(&mut self) -> Result<(), CompileError> {
        loop {
            let prev_count = self.r.dynamic_anchors.len();
            let check_slots = std::mem::take(&mut self.pending_dynamic_checks);
            let ann_slots = std::mem::take(&mut self.pending_dynamic_annotating);
            if check_slots.is_empty() && ann_slots.is_empty() {
                break;
            }
            let snapshot: Vec<(String, NodeId)> = self.r.dynamic_anchors.iter().map(|(k, &v)| (k.clone(), v)).collect();

            for (name, slot) in check_slots {
                let suffix = format!("#{name}");
                let mut map = HashMap::new();
                for (key, target) in &snapshot {
                    if let Some(uri) = key.strip_suffix(&suffix) {
                        let target_base = self.r.base_of.get(target).cloned().unwrap_or_else(|| uri.to_string());
                        let compiled = self.check(*target, &target_base)?;
                        map.insert(uri.to_string(), compiled);
                    }
                }
                let _ = slot.set(map);
            }

            for (name, slot) in ann_slots {
                let suffix = format!("#{name}");
                let mut map = HashMap::new();
                for (key, target) in &snapshot {
                    if let Some(uri) = key.strip_suffix(&suffix) {
                        let target_base = self.r.base_of.get(target).cloned().unwrap_or_else(|| uri.to_string());
                        let compiled = self.annotating(*target, &target_base)?;
                        map.insert(uri.to_string(), compiled);
                    }
                }
                let _ = slot.set(map);
            }

            if self.r.dynamic_anchors.len() == prev_count
                && self.pending_dynamic_checks.is_empty()
                && self.pending_dynamic_annotating.is_empty()
            {
                break;
            }
        }
        Ok(())
    }

    pub(crate) fn check(&mut self, node: NodeId, base: &str) -> Result<Check, CompileError> {
        Ok(self.fast_check(node, base)?.into_check())
    }

    pub(crate) fn fast_check(&mut self, node: NodeId, base: &str) -> Result<FastCheck, CompileError> {
        if let Some(b) = self.r.arena.as_bool(node) {
            return Ok(if b { FastCheck::Always } else { FastCheck::Never });
        }
        if !self.r.arena.is_schema_object(node) {
            let val = self.r.arena.to_value(node);
            return Err(CompileError::Schema(SchemaError::InvalidSchema(
                pi_js::json::stringify(&val),
            )));
        }
        if let Some(cached) = self.fast_cache.get(&node) {
            return Ok(cached.clone());
        }
        if let Some(cached) = self.cache.get(&node) {
            return Ok(FastCheck::Closure(cached.clone()));
        }
        if let Some(bare) = self.bare_type(node) {
            return Ok(bare);
        }

        let slot: Arc<OnceLock<Check>> = Arc::new(OnceLock::new());
        let ind = indirect_check(slot.clone());
        self.cache.insert(node, ind);
        let node_base = self.r.base_for(node, base);
        let fast = self.build(node, &node_base)?;
        let built = fast.clone().into_check();
        let _ = slot.set(built.clone());
        self.cache.insert(node, built);
        self.fast_cache.insert(node, fast.clone());
        Ok(fast)
    }

    fn build(&mut self, node: NodeId, base: &str) -> Result<FastCheck, CompileError> {
        if self.draft == Draft::Draft7
            && let Some(ref_str) = self.r.arena.str_prop(node, "$ref").map(ToString::to_string)
        {
            return self.compile_ref(&ref_str, base);
        }
        if self.draft != Draft::Draft7
            && (self.r.arena.prop(node, "unevaluatedProperties").is_some()
                || self.r.arena.prop(node, "unevaluatedItems").is_some())
        {
            let ann = self.annotating(node, base)?;
            return Ok(FastCheck::Closure(with_fresh_annotations(ann)));
        }
        let fast = self.plain_node(node, base)?;
        if self.dynamic && self.r.is_resource(node, base) {
            Ok(FastCheck::Closure(scoped_check(base.to_string(), fast.into_check())))
        } else {
            Ok(fast)
        }
    }

    fn bare_type(&self, node: NodeId) -> Option<FastCheck> {
        let obj = self.r.arena.as_object(node)?;
        let mut type_node = None;
        for (k, &v) in obj {
            if k == "type" {
                type_node = Some(v);
            } else if !is_neutral_keyword(k) {
                return None;
            }
        }
        let Some(t) = type_node else {
            return Some(FastCheck::Always);
        };
        self.r.arena.as_str(t).and_then(bare_type_check)
    }

    fn plain_node(&mut self, node: NodeId, base: &str) -> Result<FastCheck, CompileError> {
        let Some(obj) = self.r.arena.as_object(node).cloned() else {
            return Ok(FastCheck::Always);
        };
        self.plain_map(&obj, base)
    }

    fn plain_map(&mut self, obj: &IndexMap<String, NodeId>, base: &str) -> Result<FastCheck, CompileError> {
        let mut parts: Vec<FastCheck> = Vec::new();
        let types: Option<Vec<Option<String>>> = obj.get("type").map(|&tid| match self.r.arena.get(tid) {
            NodeData::Array(arr) => arr
                .iter()
                .map(|&id| self.r.arena.as_str(id).map(ToString::to_string))
                .collect(),
            _ => vec![self.r.arena.as_str(tid).map(ToString::to_string)],
        });
        let single = match &types {
            Some(list) if list.len() == 1 => list[0].as_deref(),
            _ => None,
        };
        let mut type_in_group = false;

        if single == Some("bytes") || single == Some("undefined") || single == Some("bigint") {
            // JSON values never match non-JSON JS types (`bytes`, `undefined`, `bigint`).
            parts.push(FastCheck::Never);
            type_in_group = true;
        } else {
            if let Some(str_check) = self.string_group(obj, single == Some("string"))? {
                if single == Some("string") {
                    type_in_group = true;
                }
                parts.push(str_check);
            }
            let mode = match single {
                Some("number") => NumMode::Number,
                Some("integer") => NumMode::Integer,
                Some("bigint") => NumMode::BigInt,
                _ => NumMode::Loose,
            };
            if let Some(num_check) = self.number_group(obj, mode) {
                if mode != NumMode::Loose {
                    type_in_group = true;
                }
                parts.push(num_check);
            }
            if let Some(obj_check) = self.object_group(obj, base, single == Some("object"))? {
                if single == Some("object") {
                    type_in_group = true;
                }
                parts.push(obj_check);
            }
            if let Some(arr_check) = self.array_group(obj, base, single == Some("array"))? {
                if single == Some("array") {
                    type_in_group = true;
                }
                parts.push(arr_check);
            }
        }

        if let Some(tlist) = &types
            && !type_in_group
        {
            let refs: Vec<Option<&str>> = tlist.iter().map(Option::as_deref).collect();
            parts.insert(0, type_check_from_list(&refs));
        }

        self.in_place(obj, base, &mut parts)?;
        Ok(and_fast(parts))
    }

    fn in_place(
        &mut self,
        obj: &IndexMap<String, NodeId>,
        base: &str,
        parts: &mut Vec<FastCheck>,
    ) -> Result<(), CompileError> {
        if let Some(&enum_id) = obj.get("enum")
            && let Some(items) = self.r.arena.as_array(enum_id)
        {
            let vals: Vec<Value> = items.iter().map(|&id| self.r.arena.to_value(id)).collect();
            parts.push(enum_check(vals));
        }
        if let Some(&const_id) = obj.get("const") {
            parts.push(const_check(self.r.arena.to_value(const_id)));
        }
        if let Some(&all_of_id) = obj.get("allOf")
            && let Some(items) = self.r.arena.as_array(all_of_id).map(|s| s.to_vec())
        {
            let mut sub = Vec::with_capacity(items.len());
            for item in items {
                sub.push(self.fast_check(item, base)?);
            }
            parts.push(and_fast(sub));
        }
        if let Some(&any_of_id) = obj.get("anyOf")
            && let Some(branches) = self.r.arena.as_array(any_of_id).map(|s| s.to_vec())
        {
            parts.push(self.compile_union(&branches, base, false)?);
        }
        if let Some(&one_of_id) = obj.get("oneOf")
            && let Some(branches) = self.r.arena.as_array(one_of_id).map(|s| s.to_vec())
        {
            parts.push(self.compile_union(&branches, base, true)?);
        }
        if let Some(&not_id) = obj.get("not") {
            parts.push(not_check(self.fast_check(not_id, base)?));
        }
        if let Some(&if_id) = obj.get("if")
            && (obj.contains_key("then") || obj.contains_key("else"))
        {
            let cond = self.fast_check(if_id, base)?;
            let then = match obj.get("then") {
                Some(&tid) => self.fast_check(tid, base)?,
                None => FastCheck::Always,
            };
            let otherwise = match obj.get("else") {
                Some(&eid) => self.fast_check(eid, base)?,
                None => FastCheck::Always,
            };
            parts.push(if_check(cond, then, otherwise));
        }
        if let Some(&ref_id) = obj.get("$ref")
            && let Some(ref_str) = self.r.arena.as_str(ref_id).map(ToString::to_string)
        {
            parts.push(self.compile_ref(&ref_str, base)?);
        }
        if self.draft != Draft::Draft7
            && let Some(&dyn_id) = obj.get("$dynamicRef")
            && let Some(ref_str) = self.r.arena.as_str(dyn_id).map(ToString::to_string)
        {
            parts.push(self.compile_dynamic_ref(&ref_str, base)?);
        }
        Ok(())
    }

    fn compile_union(&mut self, branches: &[NodeId], base: &str, exactly_one: bool) -> Result<FastCheck, CompileError> {
        if let Some(c) = self.const_union(branches, exactly_one) {
            for &b in branches {
                if !self.fast_cache.contains_key(&b)
                    && let Some(const_id) = self.r.arena.prop(b, "const")
                {
                    self.fast_cache.insert(b, const_check(self.r.arena.to_value(const_id)));
                }
            }
            return Ok(c);
        }
        if let Some(c) = self.tagged_union(branches, base)? {
            return Ok(c);
        }
        if exactly_one {
            let mut checks = Vec::with_capacity(branches.len());
            for &b in branches {
                checks.push(self.fast_check(b, base)?);
            }
            Ok(one_check(checks))
        } else {
            self.compile_any_of(branches, base)
        }
    }

    fn compile_any_of(&mut self, branches: &[NodeId], base: &str) -> Result<FastCheck, CompileError> {
        let mut mask = 0u16;
        let mut rest = Vec::new();
        for &b in branches {
            if let Some(bare) = self.bare_type(b)
                && let FastCheck::TypeMask(bit) = bare
            {
                mask |= bit;
                continue;
            }
            rest.push(self.fast_check(b, base)?);
        }
        if mask == 0 {
            Ok(any_check(rest))
        } else {
            Ok(masked_any_check(mask, rest))
        }
    }

    fn const_union(&self, branches: &[NodeId], exactly_one: bool) -> Option<FastCheck> {
        let mut values = Vec::with_capacity(branches.len());
        for &b in branches {
            let obj = self.r.arena.as_object(b)?;
            let &const_id = obj.get("const")?;
            for k in obj.keys() {
                if k != "const" && !is_neutral_keyword(k) {
                    return None;
                }
            }
            let val = self.r.arena.to_value(const_id);
            if val.is_object() || val.is_array() {
                return None;
            }
            if exactly_one
                && values
                    .iter()
                    .any(|existing| crate::arena::deep_equal_values(existing, &val))
            {
                return None;
            }
            values.push(val);
        }
        Some(enum_check(values))
    }

    fn tagged_union(&mut self, branches: &[NodeId], base: &str) -> Result<Option<FastCheck>, CompileError> {
        if branches.len() < 2 {
            return Ok(None);
        }
        let mut resolved = Vec::with_capacity(branches.len());
        for &branch in branches {
            let Some(obj) = self.r.arena.as_object(branch) else {
                return Ok(None);
            };
            let target = if obj.len() == 1
                && let Some(&ref_id) = obj.get("$ref")
                && let Some(ref_str) = self.r.arena.as_str(ref_id).map(ToString::to_string)
            {
                let b_base = self.r.base_for(branch, base);
                self.r.resolve(&ref_str, &b_base)?.node
            } else {
                branch
            };
            if !self.r.arena.is_schema_object(target) {
                return Ok(None);
            }
            resolved.push(target);
        }
        let Some(first_props) = self.r.arena.obj_prop(resolved[0], "properties") else {
            return Ok(None);
        };
        let candidate_keys: Vec<String> = first_props.keys().cloned().collect();
        for key in candidate_keys {
            let mut tags = Vec::with_capacity(resolved.len());
            let mut valid = true;
            for &node in &resolved {
                let Some(tag) = self.tag_of(node, &key) else {
                    valid = false;
                    break;
                };
                if tags.iter().any(|existing: &PrimValue| match (existing, &tag) {
                    (PrimValue::Null, PrimValue::Null) => true,
                    (PrimValue::Bool(a), PrimValue::Bool(b)) => a == b,
                    (PrimValue::Number(a), PrimValue::Number(b)) => crate::arena::number_eq(a, b),
                    (PrimValue::String(a), PrimValue::String(b)) => a == b,
                    _ => false,
                }) {
                    valid = false;
                    break;
                }
                tags.push(tag);
            }
            if valid {
                let mut checks = Vec::with_capacity(branches.len());
                for &b in branches {
                    checks.push(self.fast_check(b, base)?);
                }
                return Ok(Some(tagged_check(key, tags, checks)));
            }
        }
        Ok(None)
    }

    fn tag_of(&self, branch: NodeId, key: &str) -> Option<PrimValue> {
        if self.r.arena.str_prop(branch, "type") != Some("object") {
            return None;
        }
        let req = self.r.arena.array_prop(branch, "required")?;
        if !req.iter().any(|&id| self.r.arena.as_str(id) == Some(key)) {
            return None;
        }
        let props = self.r.arena.obj_prop(branch, "properties")?;
        let &tag_id = props.get(key)?;
        let tag_obj = self.r.arena.as_object(tag_id)?;
        let &const_id = tag_obj.get("const")?;
        let val = self.r.arena.to_value(const_id);
        PrimValue::from_value(&val)
    }

    fn compile_ref(&mut self, ref_str: &str, base: &str) -> Result<FastCheck, CompileError> {
        let target = self.r.resolve(ref_str, base)?;
        let check = self.fast_check(target.node, &target.base)?;
        if self.dynamic && target.base != base {
            Ok(FastCheck::Closure(scoped_check(target.base, check.into_check())))
        } else {
            Ok(check)
        }
    }

    fn bookended(&mut self, ref_str: &str, base: &str) -> Result<Option<(NodeId, String, String)>, CompileError> {
        let initial = self.r.resolve(ref_str, base)?;
        let uri = resolve_uri(ref_str, base);
        let name = &uri[document_of(&uri).len()..];
        let name = name.strip_prefix('#').unwrap_or("");
        let anchored = self.r.arena.str_prop(initial.node, "$dynamicAnchor") == Some(name);
        if name.is_empty() || name.starts_with('/') || !anchored {
            return Ok(None);
        }
        if !self.dynamic {
            return Err(CompileError::RestartDynamic);
        }
        Ok(Some((initial.node, initial.base, name.to_string())))
    }

    fn compile_dynamic_ref(&mut self, ref_str: &str, base: &str) -> Result<FastCheck, CompileError> {
        let Some((init_node, init_base, name)) = self.bookended(ref_str, base)? else {
            return self.compile_ref(ref_str, base);
        };
        let fallback = self.fast_check(init_node, &init_base)?;
        let slot: Arc<OnceLock<HashMap<String, Check>>> = Arc::new(OnceLock::new());
        self.pending_dynamic_checks.push((name, slot.clone()));
        Ok(FastCheck::Closure(Arc::new(move |v, s| {
            if let Some(table) = slot.get() {
                for i in 0..s.len() {
                    if let Some(check) = table.get(&s[i]) {
                        return check(v, s);
                    }
                }
            }
            fallback.run(v, s)
        })))
    }

    fn string_group(&self, obj: &IndexMap<String, NodeId>, strict: bool) -> Result<Option<FastCheck>, CompileError> {
        let min = obj
            .get("minLength")
            .and_then(|&id| self.r.arena.as_number(id))
            .and_then(Number::as_f64)
            .unwrap_or(0.0);
        let max = obj
            .get("maxLength")
            .and_then(|&id| self.r.arena.as_number(id))
            .and_then(Number::as_f64)
            .unwrap_or(f64::INFINITY);
        let re = match obj.get("pattern").and_then(|&id| self.r.arena.as_str(id)) {
            Some(p) => Some(shared_regex(p)?),
            None => None,
        };
        let format = self.formats.as_ref().and_then(|map| {
            let name = obj.get("format").and_then(|&id| self.r.arena.as_str(id))?;
            map.get(name).cloned()
        });
        if min == 0.0 && max.is_infinite() && re.is_none() && format.is_none() {
            return Ok(None);
        }
        Ok(Some(string_check(strict, min, max, re, format)))
    }

    fn number_group(&self, obj: &IndexMap<String, NodeId>, mode: NumMode) -> Option<FastCheck> {
        let min = obj.get("minimum").and_then(|&id| self.r.arena.as_number(id)).cloned();
        let max = obj.get("maximum").and_then(|&id| self.r.arena.as_number(id)).cloned();
        let xmin = obj
            .get("exclusiveMinimum")
            .and_then(|&id| self.r.arena.as_number(id))
            .cloned();
        let xmax = obj
            .get("exclusiveMaximum")
            .and_then(|&id| self.r.arena.as_number(id))
            .cloned();
        let multiple_of = obj
            .get("multipleOf")
            .and_then(|&id| self.r.arena.as_number(id))
            .cloned();
        if min.is_none() && max.is_none() && xmin.is_none() && xmax.is_none() && multiple_of.is_none() {
            return None;
        }
        Some(number_check(mode, min, max, xmin, xmax, multiple_of))
    }

    fn object_group(
        &mut self,
        obj: &IndexMap<String, NodeId>,
        base: &str,
        strict: bool,
    ) -> Result<Option<FastCheck>, CompileError> {
        let only_core = OBJECT_EXTRAS.iter().all(|k| !obj.contains_key(*k));
        let core_guard = if only_core {
            if strict { Guard::Strict } else { Guard::Loose }
        } else {
            Guard::None
        };
        let core = self.object_core(obj, base, core_guard)?;
        if core.is_some() && only_core {
            return Ok(core);
        }
        let mut parts = Vec::new();
        if let Some(c) = core {
            parts.push(c);
        }
        if let Some(&pn_id) = obj.get("propertyNames") {
            parts.push(property_names_check(self.fast_check(pn_id, base)?));
        }
        let min_props = obj
            .get("minProperties")
            .and_then(|&id| self.r.arena.as_number(id))
            .and_then(Number::as_f64);
        let max_props = obj
            .get("maxProperties")
            .and_then(|&id| self.r.arena.as_number(id))
            .and_then(Number::as_f64);
        if min_props.is_some() || max_props.is_some() {
            parts.push(property_count_check(
                min_props.unwrap_or(0.0),
                max_props.unwrap_or(f64::INFINITY),
            ));
        }
        let mut dep_req: Vec<(String, Vec<String>)> = Vec::new();
        let mut dep_schemas: Vec<(String, FastCheck)> = Vec::new();
        if self.draft != Draft::Draft7 {
            if let Some(&dr_id) = obj.get("dependentRequired")
                && let Some(dr_map) = self.r.arena.as_object(dr_id)
            {
                for (k, &list_id) in dr_map {
                    if let Some(arr) = self.r.arena.as_array(list_id) {
                        let reqs = arr
                            .iter()
                            .filter_map(|&id| self.r.arena.as_str(id).map(ToString::to_string))
                            .collect();
                        dep_req.push((k.clone(), reqs));
                    }
                }
            }
            if let Some(&ds_id) = obj.get("dependentSchemas")
                && let Some(ds_map) = self.r.arena.as_object(ds_id).cloned()
            {
                for (k, sid) in ds_map {
                    dep_schemas.push((k, self.fast_check(sid, base)?));
                }
            }
        }
        if let Some(&deps_id) = obj.get("dependencies")
            && let Some(deps_map) = self.r.arena.as_object(deps_id).cloned()
        {
            for (k, did) in deps_map {
                if let Some(arr) = self.r.arena.as_array(did) {
                    let reqs = arr
                        .iter()
                        .filter_map(|&id| self.r.arena.as_str(id).map(ToString::to_string))
                        .collect();
                    dep_req.push((k, reqs));
                } else {
                    dep_schemas.push((k, self.fast_check(did, base)?));
                }
            }
        }
        if !dep_req.is_empty() {
            parts.push(dependent_required_check(dep_req));
        }
        if !dep_schemas.is_empty() {
            parts.push(dependent_schemas_check(dep_schemas));
        }
        if parts.is_empty() {
            return Ok(if strict {
                Some(FastCheck::Closure(is_object_check()))
            } else {
                None
            });
        }
        Ok(Some(guard_object(and_fast(parts), strict)))
    }

    fn object_core(
        &mut self,
        obj: &IndexMap<String, NodeId>,
        base: &str,
        guard: Guard,
    ) -> Result<Option<FastCheck>, CompileError> {
        let props: IndexMap<String, NodeId> = obj
            .get("properties")
            .and_then(|&id| self.r.arena.as_object(id))
            .cloned()
            .unwrap_or_default();
        let required: Vec<String> = obj
            .get("required")
            .and_then(|&id| self.r.arena.as_array(id))
            .map(|arr| {
                arr.iter()
                    .filter_map(|&id| self.r.arena.as_str(id).map(ToString::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let patterns: IndexMap<String, NodeId> = obj
            .get("patternProperties")
            .and_then(|&id| self.r.arena.as_object(id))
            .cloned()
            .unwrap_or_default();
        let additional = obj.get("additionalProperties").copied();
        let additional_bool = additional.and_then(|id| self.r.arena.as_bool(id));

        if props.is_empty()
            && required.is_empty()
            && patterns.is_empty()
            && (additional.is_none() || additional_bool == Some(true))
        {
            return Ok(None);
        }

        let mut entries = Vec::with_capacity(props.len());
        let mut required_in_props = 0usize;
        for (k, &prop_id) in &props {
            let check = self.fast_check(prop_id, base)?;
            let is_req = required.iter().any(|r| r == k);
            if is_req {
                required_in_props += 1;
            }
            entries.push(PropEntry {
                key: k.clone(),
                check,
                required: is_req,
            });
        }
        let extra_required: Vec<String> = required.into_iter().filter(|k| !props.contains_key(k)).collect();
        let mut compiled_patterns = Vec::with_capacity(patterns.len());
        for (p, pid) in patterns {
            compiled_patterns.push((shared_regex(&p)?, self.fast_check(pid, base)?));
        }
        let extra = match additional {
            Some(aid) if additional_bool != Some(true) => Some(self.fast_check(aid, base)?),
            _ => None,
        };
        if compiled_patterns.is_empty()
            && extra_required.is_empty()
            && (extra.is_none() || additional_bool == Some(false))
        {
            return Ok(Some(declared_object(
                entries,
                required_in_props,
                additional_bool == Some(false),
                guard,
            )));
        }
        Ok(Some(open_object(
            entries,
            extra_required,
            compiled_patterns,
            extra,
            guard,
        )))
    }

    fn array_group(
        &mut self,
        obj: &IndexMap<String, NodeId>,
        base: &str,
        strict: bool,
    ) -> Result<Option<FastCheck>, CompileError> {
        let mut prefix_ids: Vec<NodeId> = Vec::new();
        let mut rest_id: Option<NodeId> = None;

        if let Some(&items_id) = obj.get("items") {
            if let Some(arr) = self.r.arena.as_array(items_id) {
                prefix_ids = arr.to_vec();
                rest_id = obj.get("additionalItems").copied();
            } else {
                if self.draft != Draft::Draft7
                    && let Some(&prefix_id) = obj.get("prefixItems")
                    && let Some(arr) = self.r.arena.as_array(prefix_id)
                {
                    prefix_ids = arr.to_vec();
                }
                rest_id = Some(items_id);
            }
        } else if self.draft != Draft::Draft7
            && let Some(&prefix_id) = obj.get("prefixItems")
            && let Some(arr) = self.r.arena.as_array(prefix_id)
        {
            prefix_ids = arr.to_vec();
        }

        let mut prefix_checks = Vec::with_capacity(prefix_ids.len());
        for pid in prefix_ids {
            prefix_checks.push(self.fast_check(pid, base)?);
        }
        let rest_check = match rest_id {
            Some(rid) if self.r.arena.as_bool(rid) != Some(true) => Some(self.fast_check(rid, base)?),
            _ => None,
        };
        let min_items = obj
            .get("minItems")
            .and_then(|&id| self.r.arena.as_number(id))
            .and_then(Number::as_f64);
        let max_items = obj
            .get("maxItems")
            .and_then(|&id| self.r.arena.as_number(id))
            .and_then(Number::as_f64);
        let has_contains = obj.contains_key("contains");
        let unique = obj.get("uniqueItems").and_then(|&id| self.r.arena.as_bool(id)) == Some(true);

        if !has_contains && !unique && (!prefix_checks.is_empty() || rest_check.is_some()) {
            let guard = if strict { Guard::Strict } else { Guard::Loose };
            return Ok(Some(list_check(
                guard,
                min_items.unwrap_or(0.0),
                max_items.unwrap_or(f64::INFINITY),
                prefix_checks,
                rest_check,
            )));
        }

        let mut parts = Vec::new();
        if !prefix_checks.is_empty() || rest_check.is_some() {
            parts.push(items_check(prefix_checks, rest_check));
        }
        if min_items.is_some() || max_items.is_some() {
            parts.push(array_length_check(
                min_items.unwrap_or(0.0),
                max_items.unwrap_or(f64::INFINITY),
            ));
        }
        if let Some(&contains_id) = obj.get("contains") {
            let min = if self.draft == Draft::Draft7 {
                1.0
            } else {
                obj.get("minContains")
                    .and_then(|&id| self.r.arena.as_number(id))
                    .and_then(Number::as_f64)
                    .unwrap_or(1.0)
            };
            let max = if self.draft == Draft::Draft7 {
                f64::INFINITY
            } else {
                obj.get("maxContains")
                    .and_then(|&id| self.r.arena.as_number(id))
                    .and_then(Number::as_f64)
                    .unwrap_or(f64::INFINITY)
            };
            parts.push(contains_check(self.fast_check(contains_id, base)?, min, max));
        }
        if unique {
            parts.push(unique_check());
        }
        if parts.is_empty() {
            return Ok(if strict {
                Some(FastCheck::Closure(is_array_check()))
            } else {
                None
            });
        }
        Ok(Some(guard_array(and_fast(parts), strict)))
    }

    pub(crate) fn annotating(&mut self, node: NodeId, base: &str) -> Result<AnnotatingCheck, CompileError> {
        if let Some(b) = self.r.arena.as_bool(node) {
            return Ok(if b { annotating_true() } else { annotating_false() });
        }
        if !self.r.arena.is_schema_object(node) {
            let val = self.r.arena.to_value(node);
            return Err(CompileError::Schema(SchemaError::InvalidSchema(
                pi_js::json::stringify(&val),
            )));
        }
        if let Some(cached) = self.annotating_cache.get(&node) {
            return Ok(cached.clone());
        }
        let slot: Arc<OnceLock<AnnotatingCheck>> = Arc::new(OnceLock::new());
        self.annotating_cache.insert(node, indirect_annotating(slot.clone()));
        let node_base = self.r.base_for(node, base);
        if self.r.arena.prop(node, "unevaluatedProperties").is_some()
            || self.r.arena.prop(node, "unevaluatedItems").is_some()
        {
            let no_uneval = self.build_annotating(node, &node_base, false)?;
            self.annotating_no_uneval_cache.insert(node, no_uneval);
        }
        let check = self.build_annotating(node, &node_base, true)?;
        let _ = slot.set(check.clone());
        self.annotating_cache.insert(node, check.clone());
        Ok(check)
    }

    fn build_annotating(
        &mut self,
        node: NodeId,
        base: &str,
        include_unevaluated: bool,
    ) -> Result<AnnotatingCheck, CompileError> {
        let Some(obj) = self.r.arena.as_object(node).cloned() else {
            return Ok(annotating_true());
        };
        let mut rest = IndexMap::new();
        let mut annotates = false;
        for (k, &v) in &obj {
            if is_annotating_keyword(k) {
                if include_unevaluated || (k != "unevaluatedProperties" && k != "unevaluatedItems") {
                    annotates = true;
                }
            } else {
                rest.insert(k.clone(), v);
            }
        }
        let plain = self.plain_map(&rest, base)?;
        let mut parts = Vec::new();
        if annotates {
            self.annotating_parts(&obj, base, include_unevaluated, &mut parts)?;
        }
        let all = if parts.is_empty() {
            ignore_annotations(plain)
        } else {
            annotate_all(plain, parts)
        };
        let own = if include_unevaluated
            && (obj.contains_key("unevaluatedProperties") || obj.contains_key("unevaluatedItems"))
        {
            isolate_annotations(all)
        } else {
            all
        };
        if include_unevaluated && self.dynamic && self.r.is_resource(node, base) {
            Ok(scoped_annotating(base.to_string(), own))
        } else {
            Ok(own)
        }
    }

    fn annotating_parts(
        &mut self,
        obj: &IndexMap<String, NodeId>,
        base: &str,
        include_unevaluated: bool,
        parts: &mut Vec<AnnotatingCheck>,
    ) -> Result<(), CompileError> {
        if obj.contains_key("properties")
            || obj.contains_key("patternProperties")
            || obj.contains_key("additionalProperties")
        {
            let props_map = obj
                .get("properties")
                .and_then(|&id| self.r.arena.as_object(id))
                .cloned()
                .unwrap_or_default();
            let mut props = Vec::with_capacity(props_map.len());
            for (k, sid) in props_map {
                props.push((k, self.fast_check(sid, base)?));
            }
            let pats_map = obj
                .get("patternProperties")
                .and_then(|&id| self.r.arena.as_object(id))
                .cloned()
                .unwrap_or_default();
            let mut pats = Vec::with_capacity(pats_map.len());
            for (p, sid) in pats_map {
                pats.push((shared_regex(&p)?, self.fast_check(sid, base)?));
            }
            let extra = match obj.get("additionalProperties") {
                Some(&aid) => Some(self.fast_check(aid, base)?),
                None => None,
            };
            parts.push(annotate_object(props, pats, extra));
        }

        if obj.contains_key("prefixItems") || obj.contains_key("items") || obj.contains_key("contains") {
            let prefix_ids: Vec<NodeId> = if let Some(&pid) = obj.get("prefixItems")
                && let Some(arr) = self.r.arena.as_array(pid)
            {
                arr.to_vec()
            } else if let Some(&iid) = obj.get("items")
                && let Some(arr) = self.r.arena.as_array(iid)
            {
                arr.to_vec()
            } else {
                Vec::new()
            };
            let rest_id: Option<NodeId> = if let Some(&iid) = obj.get("items")
                && self.r.arena.as_array(iid).is_some()
            {
                obj.get("additionalItems").copied()
            } else {
                obj.get("items").copied()
            };
            let mut prefix = Vec::with_capacity(prefix_ids.len());
            for pid in prefix_ids {
                prefix.push(self.fast_check(pid, base)?);
            }
            let rest = match rest_id {
                Some(rid) => Some(self.fast_check(rid, base)?),
                None => None,
            };
            let contains = match obj.get("contains") {
                Some(&cid) => Some(self.fast_check(cid, base)?),
                None => None,
            };
            let min_contains = obj
                .get("minContains")
                .and_then(|&id| self.r.arena.as_number(id))
                .and_then(Number::as_f64)
                .unwrap_or(1.0);
            let max_contains = obj
                .get("maxContains")
                .and_then(|&id| self.r.arena.as_number(id))
                .and_then(Number::as_f64)
                .unwrap_or(f64::INFINITY);
            parts.push(annotate_array(prefix, rest, contains, min_contains, max_contains));
        }

        if let Some(&all_of_id) = obj.get("allOf")
            && let Some(arr) = self.r.arena.as_array(all_of_id).map(|s| s.to_vec())
        {
            for s in arr {
                parts.push(self.annotating(s, base)?);
            }
        }
        if let Some(&any_of_id) = obj.get("anyOf")
            && let Some(arr) = self.r.arena.as_array(any_of_id).map(|s| s.to_vec())
        {
            let mut branches = Vec::with_capacity(arr.len());
            for s in arr {
                branches.push(self.annotating(s, base)?);
            }
            parts.push(annotate_any(branches, false));
        }
        if let Some(&one_of_id) = obj.get("oneOf")
            && let Some(arr) = self.r.arena.as_array(one_of_id).map(|s| s.to_vec())
        {
            let mut branches = Vec::with_capacity(arr.len());
            for s in arr {
                branches.push(self.annotating(s, base)?);
            }
            parts.push(annotate_any(branches, true));
        }
        if let Some(&if_id) = obj.get("if") {
            let cond = self.annotating(if_id, base)?;
            let then = match obj.get("then") {
                Some(&tid) => Some(self.annotating(tid, base)?),
                None => None,
            };
            let otherwise = match obj.get("else") {
                Some(&eid) => Some(self.annotating(eid, base)?),
                None => None,
            };
            parts.push(annotate_if(cond, then, otherwise));
        }
        if let Some(&ref_id) = obj.get("$ref")
            && let Some(ref_str) = self.r.arena.as_str(ref_id).map(ToString::to_string)
        {
            parts.push(self.annotating_ref(&ref_str, base)?);
        }
        if let Some(&dyn_id) = obj.get("$dynamicRef")
            && let Some(ref_str) = self.r.arena.as_str(dyn_id).map(ToString::to_string)
        {
            parts.push(self.annotating_dynamic_ref(&ref_str, base)?);
        }
        if let Some(&ds_id) = obj.get("dependentSchemas")
            && let Some(ds_map) = self.r.arena.as_object(ds_id).cloned()
        {
            for (k, sid) in ds_map {
                parts.push(annotate_dependent(k, self.annotating(sid, base)?));
            }
        }
        if include_unevaluated {
            if let Some(&up_id) = obj.get("unevaluatedProperties") {
                parts.push(unevaluated_properties(self.fast_check(up_id, base)?));
            }
            if let Some(&ui_id) = obj.get("unevaluatedItems") {
                parts.push(unevaluated_items(self.fast_check(ui_id, base)?));
            }
        }
        Ok(())
    }

    fn annotating_ref(&mut self, ref_str: &str, base: &str) -> Result<AnnotatingCheck, CompileError> {
        let target = self.r.resolve(ref_str, base)?;
        let check = self.annotating(target.node, &target.base)?;
        if self.dynamic && target.base != base {
            Ok(scoped_annotating(target.base, check))
        } else {
            Ok(check)
        }
    }

    fn annotating_dynamic_ref(&mut self, ref_str: &str, base: &str) -> Result<AnnotatingCheck, CompileError> {
        let Some((init_node, init_base, name)) = self.bookended(ref_str, base)? else {
            return self.annotating_ref(ref_str, base);
        };
        let fallback = self.annotating(init_node, &init_base)?;
        let slot: Arc<OnceLock<HashMap<String, AnnotatingCheck>>> = Arc::new(OnceLock::new());
        self.pending_dynamic_annotating.push((name, slot.clone()));
        Ok(Arc::new(move |v, a, s| {
            if let Some(table) = slot.get() {
                for i in 0..s.len() {
                    if let Some(check) = table.get(&s[i]) {
                        return check(v, a, s);
                    }
                }
            }
            fallback(v, a, s)
        }))
    }
}

#[derive(Debug)]
pub(crate) enum CompileError {
    RestartDynamic,
    Schema(SchemaError),
}

impl From<SchemaError> for CompileError {
    fn from(e: SchemaError) -> Self {
        Self::Schema(e)
    }
}

pub(crate) fn build_compiler(root_value: &Value, options: &CompileOptions) -> Result<(Compiler, Check), SchemaError> {
    let mut plain = Compiler::new(root_value, options, false);
    match plain.compile_root() {
        Ok(check) => Ok((plain, check)),
        Err(CompileError::RestartDynamic) => {
            let mut dynamic = Compiler::new(root_value, options, true);
            match dynamic.compile_root() {
                Ok(check) => Ok((dynamic, check)),
                Err(CompileError::Schema(e)) => Err(e),
                Err(CompileError::RestartDynamic) => Err(SchemaError::InvalidSchema(RESTART_DYNAMIC.to_string())),
            }
        }
        Err(CompileError::Schema(e)) => Err(e),
    }
}

/// Checks if `n` matches any integer check (re-exported helper for `explain` / `coerce`).
#[allow(dead_code)]
pub(crate) fn value_is_integer(v: &Value) -> bool {
    matches!(v, Value::Number(n) if is_integer_number(n))
}
