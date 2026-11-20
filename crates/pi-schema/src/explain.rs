//! Failure explanation walk producing [`ValidationError`] entries with JSON Pointer instance and schema paths.

use std::cmp::Ordering;
use std::collections::HashMap;

use indexmap::IndexMap;
use serde_json::{Number, Value};

use crate::annotations::Annotations;
use crate::arena::{NodeId, deep_equal_values, format_number, is_finite_number, is_integer_number, number_cmp};
use crate::checks::{code_points, is_multiple, unique_check_fn};
use crate::compiler::Compiler;
use crate::shared::shared_regex;
use crate::types::{Draft, ValidationError};

const LISTED_VALUES: usize = 20;

#[derive(Clone)]
struct MemoEntry {
    path: String,
    errors: Vec<ValidationError>,
}

/// Explains why values fail a compiled schema.
pub(crate) struct Explainer {
    c: Compiler,
}

impl Explainer {
    pub(crate) fn new(c: Compiler) -> Self {
        Self { c }
    }

    pub(crate) fn errors(&self, value: &Value) -> Vec<ValidationError> {
        let mut out = Vec::new();
        let mut scope = Vec::new();
        let mut memo: HashMap<(NodeId, usize), MemoEntry> = HashMap::new();
        self.walk(
            self.c.r.root,
            &self.c.r.root_base,
            value,
            "",
            "#",
            &mut out,
            &mut scope,
            &mut memo,
        );
        if out.is_empty() {
            out.push(ValidationError::new("", "#", "schema", "invalid"));
        }
        out
    }

    fn ok(&self, node: NodeId, v: &Value, scope: &mut Vec<String>) -> bool {
        self.c.ok_readonly(node, v, scope).unwrap_or(true)
    }

    #[allow(clippy::too_many_arguments)]
    fn walk(
        &self,
        node: NodeId,
        base: &str,
        v: &Value,
        path: &str,
        schema_path: &str,
        out: &mut Vec<ValidationError>,
        scope: &mut Vec<String>,
        memo: &mut HashMap<(NodeId, usize), MemoEntry>,
    ) {
        if let Some(b) = self.c.r.arena.as_bool(node) {
            if !b {
                out.push(ValidationError::new(
                    path,
                    schema_path,
                    "false",
                    "no value is allowed here",
                ));
            }
            return;
        }
        let Some(obj) = self.c.r.arena.as_object(node) else {
            return;
        };
        let b = self.c.r.base_for_readonly(node, base);
        if self.ok(node, v, scope) {
            return;
        }
        let start = out.len();

        if self.c.draft == Draft::Draft7
            && let Some(&ref_id) = obj.get("$ref")
            && let Some(ref_str) = self.c.r.arena.as_str(ref_id)
        {
            self.walk_ref(ref_str, &b, v, path, schema_path, out, scope, memo);
            return;
        }

        if let Some(&type_id) = obj.get("type") {
            let types: Vec<Option<&str>> = match self.c.r.arena.as_array(type_id) {
                Some(arr) => arr.iter().map(|&id| self.c.r.arena.as_str(id)).collect(),
                None => vec![self.c.r.arena.as_str(type_id)],
            };
            let matches_any = types.iter().flatten().any(|t| value_matches_type(v, t));
            if !matches_any {
                let expected: Vec<&str> = types.iter().map(|t| t.unwrap_or("unknown")).collect();
                out.push(ValidationError::new(
                    path,
                    join_schema_path(schema_path, "type"),
                    "type",
                    format!("expected {}, got {}", expected.join(" or "), type_name(v)),
                ));
                return;
            }
        }

        if let Some(&enum_id) = obj.get("enum")
            && let Some(items) = self.c.r.arena.as_array(enum_id)
        {
            let vals: Vec<Value> = items.iter().map(|&id| self.c.r.arena.to_value(id)).collect();
            if !vals.iter().any(|item| deep_equal_values(v, item)) {
                out.push(ValidationError::new(
                    path,
                    join_schema_path(schema_path, "enum"),
                    "enum",
                    format!("expected one of {}", list_values(&vals)),
                ));
            }
        }

        if let Some(&const_id) = obj.get("const") {
            let const_val = self.c.r.arena.to_value(const_id);
            if !deep_equal_values(v, &const_val) {
                out.push(ValidationError::new(
                    path,
                    join_schema_path(schema_path, "const"),
                    "const",
                    format!("expected {}", pi_js::json::stringify(&const_val)),
                ));
            }
        }

        if let Value::String(s) = v
            && self.c.r.arena.str_prop(node, "type") != Some("bytes")
        {
            self.walk_string(obj, s, path, schema_path, out);
        }

        if let Value::Number(n) = v {
            self.walk_number(obj, n, path, schema_path, out);
        }

        if let Value::Object(map) = v {
            self.walk_object(obj, &b, map, path, schema_path, out, scope, memo);
        }

        if let Value::Array(arr) = v {
            self.walk_array(obj, &b, arr, path, schema_path, out, scope, memo);
        }

        if let Some(&all_of_id) = obj.get("allOf")
            && let Some(items) = self.c.r.arena.as_array(all_of_id)
        {
            for (i, &s) in items.iter().enumerate() {
                let sp = format!("{}/{i}", join_schema_path(schema_path, "allOf"));
                self.walk(s, &b, v, path, &sp, out, scope, memo);
            }
        }

        if let Some(&any_of_id) = obj.get("anyOf")
            && let Some(branches) = self.c.r.arena.as_array(any_of_id)
        {
            let sp = join_schema_path(schema_path, "anyOf");
            self.walk_union(branches, &b, v, path, &sp, out, scope, memo, false);
        }

        if let Some(&one_of_id) = obj.get("oneOf")
            && let Some(branches) = self.c.r.arena.as_array(one_of_id)
        {
            let sp = join_schema_path(schema_path, "oneOf");
            self.walk_union(branches, &b, v, path, &sp, out, scope, memo, true);
        }

        if let Some(&not_id) = obj.get("not")
            && self.ok(not_id, v, scope)
        {
            out.push(ValidationError::new(
                path,
                join_schema_path(schema_path, "not"),
                "not",
                "must not match the schema in not",
            ));
        }

        if let Some(&if_id) = obj.get("if")
            && (obj.contains_key("then") || obj.contains_key("else"))
        {
            let (branch_opt, kw) = if self.ok(if_id, v, scope) {
                (obj.get("then").copied(), "then")
            } else {
                (obj.get("else").copied(), "else")
            };
            if let Some(branch) = branch_opt {
                let sp = join_schema_path(schema_path, kw);
                self.walk(branch, &b, v, path, &sp, out, scope, memo);
            }
        }

        if let Some(&ref_id) = obj.get("$ref")
            && let Some(ref_str) = self.c.r.arena.as_str(ref_id)
        {
            self.walk_ref(ref_str, &b, v, path, schema_path, out, scope, memo);
        }

        if out.len() == start
            && self.c.draft != Draft::Draft7
            && (obj.contains_key("unevaluatedProperties") || obj.contains_key("unevaluatedItems"))
        {
            self.walk_unevaluated(node, obj, &b, v, path, schema_path, out, scope, memo);
        }

        if out.len() == start && self.c.draft != Draft::Draft7 && obj.contains_key("$dynamicRef") {
            out.push(ValidationError::new(
                path,
                join_schema_path(schema_path, "$dynamicRef"),
                "$dynamicRef",
                "does not match the dynamically referenced schema",
            ));
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn walk_ref(
        &self,
        ref_str: &str,
        base: &str,
        v: &Value,
        path: &str,
        schema_path: &str,
        out: &mut Vec<ValidationError>,
        scope: &mut Vec<String>,
        memo: &mut HashMap<(NodeId, usize), MemoEntry>,
    ) {
        let Some(target) = self.c.r.resolve_readonly(ref_str, base) else {
            return;
        };
        if self.c.r.arena.as_bool(target.node) == Some(false) && self.c.r.is_unresolved(ref_str, base) {
            out.push(ValidationError::new(
                path,
                join_schema_path(schema_path, "$ref"),
                "$ref",
                format!("unresolved reference {ref_str}"),
            ));
            return;
        }
        let target_sp = if ref_str.starts_with('#') {
            ref_str.to_string()
        } else {
            join_schema_path(schema_path, "$ref")
        };
        let should_memo = self.c.r.arena.is_schema_object(target.node) && (v.is_object() || v.is_array());
        if !should_memo {
            self.walk(target.node, &target.base, v, path, &target_sp, out, scope, memo);
            return;
        }
        let v_key = std::ptr::from_ref(v) as usize;
        if let Some(known) = memo.get(&(target.node, v_key))
            && known.path == path
        {
            out.extend(known.errors.iter().cloned());
            return;
        }
        let mut errors = Vec::new();
        self.walk(target.node, &target.base, v, path, &target_sp, &mut errors, scope, memo);
        memo.insert(
            (target.node, v_key),
            MemoEntry {
                path: path.to_string(),
                errors: errors.clone(),
            },
        );
        out.extend(errors);
    }

    fn walk_string(
        &self,
        obj: &IndexMap<String, NodeId>,
        s: &str,
        path: &str,
        schema_path: &str,
        out: &mut Vec<ValidationError>,
    ) {
        let n = code_points(s) as f64;
        if let Some(&id) = obj.get("minLength")
            && let Some(num) = self.c.r.arena.as_number(id)
            && let Some(min) = num.as_f64()
            && n < min
        {
            out.push(ValidationError::new(
                path,
                join_schema_path(schema_path, "minLength"),
                "minLength",
                format!("expected at least {} characters", format_number(num)),
            ));
        }
        if let Some(&id) = obj.get("maxLength")
            && let Some(num) = self.c.r.arena.as_number(id)
            && let Some(max) = num.as_f64()
            && n > max
        {
            out.push(ValidationError::new(
                path,
                join_schema_path(schema_path, "maxLength"),
                "maxLength",
                format!("expected at most {} characters", format_number(num)),
            ));
        }
        if let Some(&id) = obj.get("pattern")
            && let Some(pattern) = self.c.r.arena.as_str(id)
            && let Ok(re) = shared_regex(pattern)
            && !pi_js::regex::test(&re, s)
        {
            out.push(ValidationError::new(
                path,
                join_schema_path(schema_path, "pattern"),
                "pattern",
                format!("expected to match {pattern}"),
            ));
        }
        if let Some(formats) = &self.c.formats
            && let Some(&id) = obj.get("format")
            && let Some(name) = self.c.r.arena.as_str(id)
            && let Some(fmt_check) = formats.get(name)
            && !fmt_check.test(s)
        {
            out.push(ValidationError::new(
                path,
                join_schema_path(schema_path, "format"),
                "format",
                format!("expected format {name}"),
            ));
        }
    }

    fn walk_number(
        &self,
        obj: &IndexMap<String, NodeId>,
        v: &Number,
        path: &str,
        schema_path: &str,
        out: &mut Vec<ValidationError>,
    ) {
        if let Some(&id) = obj.get("minimum")
            && let Some(min) = self.c.r.arena.as_number(id)
            && number_cmp(v, min) == Ordering::Less
        {
            out.push(ValidationError::new(
                path,
                join_schema_path(schema_path, "minimum"),
                "minimum",
                format!("expected >= {}", format_number(min)),
            ));
        }
        if let Some(&id) = obj.get("maximum")
            && let Some(max) = self.c.r.arena.as_number(id)
            && number_cmp(v, max) == Ordering::Greater
        {
            out.push(ValidationError::new(
                path,
                join_schema_path(schema_path, "maximum"),
                "maximum",
                format!("expected <= {}", format_number(max)),
            ));
        }
        if let Some(&id) = obj.get("exclusiveMinimum")
            && let Some(xmin) = self.c.r.arena.as_number(id)
            && number_cmp(v, xmin) != Ordering::Greater
        {
            out.push(ValidationError::new(
                path,
                join_schema_path(schema_path, "exclusiveMinimum"),
                "exclusiveMinimum",
                format!("expected > {}", format_number(xmin)),
            ));
        }
        if let Some(&id) = obj.get("exclusiveMaximum")
            && let Some(xmax) = self.c.r.arena.as_number(id)
            && number_cmp(v, xmax) != Ordering::Less
        {
            out.push(ValidationError::new(
                path,
                join_schema_path(schema_path, "exclusiveMaximum"),
                "exclusiveMaximum",
                format!("expected < {}", format_number(xmax)),
            ));
        }
        if let Some(&id) = obj.get("multipleOf")
            && let Some(mult) = self.c.r.arena.as_number(id)
            && !is_multiple(v, mult)
        {
            out.push(ValidationError::new(
                path,
                join_schema_path(schema_path, "multipleOf"),
                "multipleOf",
                format!("expected a multiple of {}", format_number(mult)),
            ));
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn walk_object(
        &self,
        obj: &IndexMap<String, NodeId>,
        b: &str,
        v: &serde_json::Map<String, Value>,
        path: &str,
        schema_path: &str,
        out: &mut Vec<ValidationError>,
        scope: &mut Vec<String>,
        memo: &mut HashMap<(NodeId, usize), MemoEntry>,
    ) {
        let props = obj.get("properties").and_then(|&id| self.c.r.arena.as_object(id));
        if let Some(&req_id) = obj.get("required")
            && let Some(req) = self.c.r.arena.as_array(req_id)
        {
            let req_sp = join_schema_path(schema_path, "required");
            for &kid in req {
                if let Some(k) = self.c.r.arena.as_str(kid)
                    && !v.contains_key(k)
                {
                    out.push(ValidationError::new(pointer(path, k), &req_sp, "required", "missing"));
                }
            }
        }

        if let Some(props_map) = props {
            let props_sp = join_schema_path(schema_path, "properties");
            for (k, &prop_schema) in props_map {
                if let Some(val) = v.get(k) {
                    self.walk(
                        prop_schema,
                        b,
                        val,
                        &pointer(path, k),
                        &join_schema_path(&props_sp, k),
                        out,
                        scope,
                        memo,
                    );
                }
            }
        }

        let patterns = obj
            .get("patternProperties")
            .and_then(|&id| self.c.r.arena.as_object(id));
        let pats_sp = join_schema_path(schema_path, "patternProperties");
        let add_id = obj.get("additionalProperties").copied();
        let add_sp = join_schema_path(schema_path, "additionalProperties");
        let pn_id = obj.get("propertyNames").copied();
        let pn_sp = join_schema_path(schema_path, "propertyNames");

        for (k, val) in v {
            let mut matched = props.is_some_and(|p| p.contains_key(k));
            if let Some(pats) = patterns {
                for (p, &pat_schema) in pats {
                    if let Ok(re) = shared_regex(p)
                        && pi_js::regex::test(&re, k)
                    {
                        matched = true;
                        self.walk(
                            pat_schema,
                            b,
                            val,
                            &pointer(path, k),
                            &join_schema_path(&pats_sp, p),
                            out,
                            scope,
                            memo,
                        );
                    }
                }
            }
            if !matched && let Some(aid) = add_id {
                if self.c.r.arena.as_bool(aid) == Some(false) {
                    out.push(ValidationError::new(
                        pointer(path, k),
                        &add_sp,
                        "additionalProperties",
                        "unknown field",
                    ));
                } else {
                    self.walk(aid, b, val, &pointer(path, k), &add_sp, out, scope, memo);
                }
            }
            if let Some(pn) = pn_id {
                let key_val = Value::String(k.clone());
                if !self.ok(pn, &key_val, scope) {
                    out.push(ValidationError::new(
                        pointer(path, k),
                        &pn_sp,
                        "propertyNames",
                        "invalid property name",
                    ));
                }
            }
        }

        let count = v.len() as f64;
        if let Some(&id) = obj.get("minProperties")
            && let Some(num) = self.c.r.arena.as_number(id)
            && let Some(min) = num.as_f64()
            && count < min
        {
            out.push(ValidationError::new(
                path,
                join_schema_path(schema_path, "minProperties"),
                "minProperties",
                format!("expected at least {} properties", format_number(num)),
            ));
        }
        if let Some(&id) = obj.get("maxProperties")
            && let Some(num) = self.c.r.arena.as_number(id)
            && let Some(max) = num.as_f64()
            && count > max
        {
            out.push(ValidationError::new(
                path,
                join_schema_path(schema_path, "maxProperties"),
                "maxProperties",
                format!("expected at most {} properties", format_number(num)),
            ));
        }

        let mut merged_deps: IndexMap<String, (NodeId, String)> = IndexMap::new();
        if self.c.draft != Draft::Draft7
            && let Some(&dr_id) = obj.get("dependentRequired")
            && let Some(dr_map) = self.c.r.arena.as_object(dr_id)
        {
            let sp = join_schema_path(schema_path, "dependentRequired");
            for (k, &d) in dr_map {
                merged_deps.insert(k.clone(), (d, join_schema_path(&sp, k)));
            }
        }
        if let Some(&deps_id) = obj.get("dependencies")
            && let Some(deps_map) = self.c.r.arena.as_object(deps_id)
        {
            let sp = join_schema_path(schema_path, "dependencies");
            for (k, &d) in deps_map {
                merged_deps.insert(k.clone(), (d, join_schema_path(&sp, k)));
            }
        }
        for (k, (d, sp)) in merged_deps {
            if !v.contains_key(&k) {
                continue;
            }
            if let Some(reqs) = self.c.r.arena.as_array(d) {
                for &rid in reqs {
                    if let Some(r) = self.c.r.arena.as_str(rid)
                        && !v.contains_key(r)
                    {
                        out.push(ValidationError::new(
                            pointer(path, r),
                            &sp,
                            "dependentRequired",
                            format!("required when {k} is present"),
                        ));
                    }
                }
            } else {
                self.walk(d, b, &Value::Object(v.clone()), path, &sp, out, scope, memo);
            }
        }

        if self.c.draft != Draft::Draft7
            && let Some(&ds_id) = obj.get("dependentSchemas")
            && let Some(ds_map) = self.c.r.arena.as_object(ds_id)
        {
            let sp = join_schema_path(schema_path, "dependentSchemas");
            let obj_val = Value::Object(v.clone());
            for (k, &s) in ds_map {
                if v.contains_key(k) {
                    self.walk(s, b, &obj_val, path, &join_schema_path(&sp, k), out, scope, memo);
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn walk_array(
        &self,
        obj: &IndexMap<String, NodeId>,
        b: &str,
        v: &[Value],
        path: &str,
        schema_path: &str,
        out: &mut Vec<ValidationError>,
        scope: &mut Vec<String>,
        memo: &mut HashMap<(NodeId, usize), MemoEntry>,
    ) {
        let mut prefix: &[NodeId] = &[];
        let mut prefix_sp = String::new();
        let mut rest: Option<(NodeId, String)> = None;

        if let Some(&items_id) = obj.get("items") {
            if let Some(arr) = self.c.r.arena.as_array(items_id) {
                prefix = arr;
                prefix_sp = join_schema_path(schema_path, "items");
                if let Some(&add_id) = obj.get("additionalItems") {
                    rest = Some((add_id, join_schema_path(schema_path, "additionalItems")));
                }
            } else {
                if self.c.draft != Draft::Draft7
                    && let Some(&pref_id) = obj.get("prefixItems")
                    && let Some(arr) = self.c.r.arena.as_array(pref_id)
                {
                    prefix = arr;
                    prefix_sp = join_schema_path(schema_path, "prefixItems");
                }
                rest = Some((items_id, join_schema_path(schema_path, "items")));
            }
        } else if self.c.draft != Draft::Draft7
            && let Some(&pref_id) = obj.get("prefixItems")
            && let Some(arr) = self.c.r.arena.as_array(pref_id)
        {
            prefix = arr;
            prefix_sp = join_schema_path(schema_path, "prefixItems");
        }

        for (i, item) in v.iter().enumerate() {
            if i < prefix.len() {
                let sp = format!("{prefix_sp}/{i}");
                self.walk(prefix[i], b, item, &format!("{path}/{i}"), &sp, out, scope, memo);
            } else if let Some((rest_id, ref rest_sp)) = rest {
                self.walk(rest_id, b, item, &format!("{path}/{i}"), rest_sp, out, scope, memo);
            }
        }

        let len = v.len() as f64;
        if let Some(&id) = obj.get("minItems")
            && let Some(num) = self.c.r.arena.as_number(id)
            && let Some(min) = num.as_f64()
            && len < min
        {
            out.push(ValidationError::new(
                path,
                join_schema_path(schema_path, "minItems"),
                "minItems",
                format!("expected at least {} items", format_number(num)),
            ));
        }
        if let Some(&id) = obj.get("maxItems")
            && let Some(num) = self.c.r.arena.as_number(id)
            && let Some(max) = num.as_f64()
            && len > max
        {
            out.push(ValidationError::new(
                path,
                join_schema_path(schema_path, "maxItems"),
                "maxItems",
                format!("expected at most {} items", format_number(num)),
            ));
        }
        if obj.get("uniqueItems").and_then(|&id| self.c.r.arena.as_bool(id)) == Some(true)
            && !unique_check_fn(&Value::Array(v.to_vec()))
        {
            out.push(ValidationError::new(
                path,
                join_schema_path(schema_path, "uniqueItems"),
                "uniqueItems",
                "expected unique items",
            ));
        }
        if let Some(&contains_id) = obj.get("contains") {
            let draft7 = self.c.draft == Draft::Draft7;
            let min = if draft7 {
                1.0
            } else {
                obj.get("minContains")
                    .and_then(|&id| self.c.r.arena.as_number(id))
                    .and_then(Number::as_f64)
                    .unwrap_or(1.0)
            };
            let max = if draft7 {
                f64::INFINITY
            } else {
                obj.get("maxContains")
                    .and_then(|&id| self.c.r.arena.as_number(id))
                    .and_then(Number::as_f64)
                    .unwrap_or(f64::INFINITY)
            };
            let count = v.iter().filter(|item| self.ok(contains_id, item, scope)).count() as f64;
            if count < min || count > max {
                out.push(ValidationError::new(
                    path,
                    join_schema_path(schema_path, "contains"),
                    "contains",
                    contains_message(min, max),
                ));
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn walk_union(
        &self,
        branches: &[NodeId],
        b: &str,
        v: &Value,
        path: &str,
        schema_path: &str,
        out: &mut Vec<ValidationError>,
        scope: &mut Vec<String>,
        memo: &mut HashMap<(NodeId, usize), MemoEntry>,
        exactly_one: bool,
    ) {
        let matches = branches.iter().filter(|&&s| self.ok(s, v, scope)).count();
        if matches == 1 || (!exactly_one && matches > 0) {
            return;
        }
        let keyword = if exactly_one { "oneOf" } else { "anyOf" };
        if matches > 1 {
            out.push(ValidationError::new(
                path,
                schema_path,
                keyword,
                "matches more than one alternative",
            ));
            return;
        }

        if let Some((tag_key, tag_values)) = self.tag_of(branches, b)
            && let Some(map) = v.as_object()
        {
            let matched_idx = map
                .get(&tag_key)
                .and_then(|tv| tag_values.iter().position(|expected| deep_equal_values(tv, expected)));
            match matched_idx {
                None => {
                    out.push(ValidationError::new(
                        pointer(path, &tag_key),
                        schema_path,
                        keyword,
                        format!("expected one of {}", list_values(&tag_values)),
                    ));
                }
                Some(idx) => {
                    let sp = format!("{schema_path}/{idx}");
                    self.walk(branches[idx], b, v, path, &sp, out, scope, memo);
                }
            }
            return;
        }

        let mut best: Option<(Vec<ValidationError>, usize)> = None;
        for (i, &s) in branches.iter().enumerate() {
            let mut errors = Vec::new();
            let sp = format!("{schema_path}/{i}");
            self.walk(s, b, v, path, &sp, &mut errors, scope, memo);
            if errors.is_empty() {
                continue;
            }
            let declared = match v.as_object() {
                Some(obj_map) => self.declared_keys(s, b, obj_map),
                None => 0,
            };
            let is_better = match &best {
                None => true,
                Some((best_errs, best_decl)) => {
                    declared > *best_decl || (declared == *best_decl && errors[0].path.len() > best_errs[0].path.len())
                }
            };
            if is_better {
                best = Some((errors, declared));
            }
        }
        if let Some((errors, _)) = best {
            out.extend(errors);
        } else {
            out.push(ValidationError::new(
                path,
                schema_path,
                keyword,
                "matches no alternative",
            ));
        }
    }

    fn declared_keys(&self, s: NodeId, b: &str, v: &serde_json::Map<String, Value>) -> usize {
        let node = if let Some(ref_str) = self.c.r.arena.str_prop(s, "$ref") {
            let sb = self.c.r.base_for_readonly(s, b);
            self.c.r.resolve_readonly(ref_str, &sb).map(|r| r.node).unwrap_or(s)
        } else {
            s
        };
        let Some(props) = self.c.r.arena.obj_prop(node, "properties") else {
            return 0;
        };
        v.keys().filter(|k| props.contains_key(k.as_str())).count()
    }

    fn tag_of(&self, branches: &[NodeId], b: &str) -> Option<(String, Vec<Value>)> {
        let resolved: Vec<NodeId> = branches
            .iter()
            .map(|&s| {
                if let Some(ref_str) = self.c.r.arena.str_prop(s, "$ref") {
                    let sb = self.c.r.base_for_readonly(s, b);
                    self.c.r.resolve_readonly(ref_str, &sb).map(|r| r.node).unwrap_or(s)
                } else {
                    s
                }
            })
            .collect();
        let first_props = self.c.r.arena.obj_prop(*resolved.first()?, "properties")?;
        for key in first_props.keys() {
            let mut values = Vec::with_capacity(resolved.len());
            let mut ok = true;
            for &s in &resolved {
                let Some(props) = self.c.r.arena.obj_prop(s, "properties") else {
                    ok = false;
                    break;
                };
                let Some(&tag_id) = props.get(key) else {
                    ok = false;
                    break;
                };
                let Some(const_id) = self.c.r.arena.prop(tag_id, "const") else {
                    ok = false;
                    break;
                };
                let val = self.c.r.arena.to_value(const_id);
                if val.is_object() || val.is_array() || values.iter().any(|e| deep_equal_values(e, &val)) {
                    ok = false;
                    break;
                }
                values.push(val);
            }
            if ok {
                return Some((key.clone(), values));
            }
        }
        None
    }

    #[allow(clippy::too_many_arguments)]
    fn walk_unevaluated(
        &self,
        node: NodeId,
        obj: &IndexMap<String, NodeId>,
        b: &str,
        v: &Value,
        path: &str,
        schema_path: &str,
        out: &mut Vec<ValidationError>,
        scope: &mut Vec<String>,
        memo: &mut HashMap<(NodeId, usize), MemoEntry>,
    ) {
        let Some(ann_check) = self.c.annotating_no_uneval_readonly(node) else {
            return;
        };
        let mut a = Annotations::new();
        ann_check(v, &mut a, scope);

        if let Value::Object(map) = v
            && let Some(&up_id) = obj.get("unevaluatedProperties")
            && !a.all_props
        {
            let sp = join_schema_path(schema_path, "unevaluatedProperties");
            for (k, val) in map {
                if a.props.contains(k) {
                    continue;
                }
                if self.c.r.arena.as_bool(up_id) == Some(false) {
                    out.push(ValidationError::new(
                        pointer(path, k),
                        &sp,
                        "unevaluatedProperties",
                        "unknown field",
                    ));
                } else {
                    self.walk(up_id, b, val, &pointer(path, k), &sp, out, scope, memo);
                }
            }
        }

        if let Value::Array(arr) = v
            && let Some(&ui_id) = obj.get("unevaluatedItems")
            && !a.all_items
        {
            let sp = join_schema_path(schema_path, "unevaluatedItems");
            for (i, val) in arr.iter().enumerate().skip(a.items) {
                if a.indexes.contains(&i) {
                    continue;
                }
                if self.c.r.arena.as_bool(ui_id) == Some(false) {
                    out.push(ValidationError::new(
                        format!("{path}/{i}"),
                        &sp,
                        "unevaluatedItems",
                        "unexpected item",
                    ));
                } else {
                    self.walk(ui_id, b, val, &format!("{path}/{i}"), &sp, out, scope, memo);
                }
            }
        }
    }
}

fn value_matches_type(v: &Value, name: &str) -> bool {
    match name {
        "string" => v.is_string(),
        "number" => matches!(v, Value::Number(n) if is_finite_number(n)),
        "integer" => matches!(v, Value::Number(n) if is_integer_number(n)),
        "boolean" => v.is_boolean(),
        "null" => v.is_null(),
        "array" => v.is_array(),
        "object" => v.is_object(),
        _ => false,
    }
}

fn list_values(values: &[Value]) -> String {
    if values.len() <= LISTED_VALUES {
        return pi_js::json::stringify(values);
    }
    let listed = pi_js::json::stringify(&values[..LISTED_VALUES]);
    let prefix = listed.strip_suffix(']').unwrap_or(&listed);
    format!("{prefix}, ... and {} more]", values.len() - LISTED_VALUES)
}

fn contains_message(min: f64, max: f64) -> String {
    if max.is_infinite() {
        let word = if (min - 1.0).abs() < f64::EPSILON {
            "item"
        } else {
            "items"
        };
        format!("expected at least {} matching {word}", pi_js::num::to_js_string(min))
    } else if min == 0.0 {
        let word = if (max - 1.0).abs() < f64::EPSILON {
            "item"
        } else {
            "items"
        };
        format!("expected at most {} matching {word}", pi_js::num::to_js_string(max))
    } else {
        format!(
            "expected between {} and {} matching items",
            pi_js::num::to_js_string(min),
            pi_js::num::to_js_string(max)
        )
    }
}

fn pointer(path: &str, key: &str) -> String {
    let escaped = key.replace('~', "~0").replace('/', "~1");
    format!("{path}/{escaped}")
}

fn join_schema_path(base: &str, segment: &str) -> String {
    let escaped = segment.replace('~', "~0").replace('/', "~1");
    format!("{base}/{escaped}")
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(n) => {
            if is_integer_number(n) {
                "integer"
            } else {
                "number"
            }
        }
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}
