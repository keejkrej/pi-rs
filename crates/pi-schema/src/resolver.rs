//! Reference resolution for one compile: schema resources, anchors, dynamic anchors, and JSON Pointers.

use std::collections::{HashMap, HashSet};

use indexmap::IndexMap;
use serde_json::Value;

use crate::arena::{NodeData, NodeId, SchemaArena};
use crate::types::{CompileOptions, Draft, SchemaError, UnresolvedRefs};

/// The base URI of a root schema without `$id`; relative references resolve against it.
pub const DEFAULT_BASE: &str = "https://json-schema.invalid/root.json";

/// A resolved schema node and the base URI under which its own references resolve.
#[derive(Clone, Debug)]
pub(crate) struct Resolved {
    pub(crate) node: NodeId,
    pub(crate) base: String,
}

const SCHEMA_KEYWORDS: &[&str] = &[
    "additionalProperties",
    "propertyNames",
    "contains",
    "not",
    "if",
    "then",
    "else",
    "unevaluatedProperties",
    "unevaluatedItems",
    "additionalItems",
    "contentSchema",
    "items",
];

const MAP_KEYWORDS: &[&str] = &[
    "properties",
    "patternProperties",
    "$defs",
    "definitions",
    "dependentSchemas",
    "dependencies",
];

const ARRAY_KEYWORDS: &[&str] = &["allOf", "anyOf", "oneOf", "prefixItems", "items"];

/// Detects the effective draft from explicit options or the root schema's `$schema` keyword.
pub(crate) fn detect_draft(root: &Value, explicit: Option<Draft>) -> Draft {
    if let Some(d) = explicit {
        return d;
    }
    if let Some(schema_uri) = root.as_object().and_then(|o| o.get("$schema")).and_then(Value::as_str) {
        let normalized = schema_uri.trim_end_matches('#');
        if normalized == "http://json-schema.org/draft-07/schema"
            || normalized == "https://json-schema.org/draft-07/schema"
        {
            return Draft::Draft7;
        }
    }
    Draft::Draft2020_12
}

/// Resolves `reference` against `base`. URNs and bases rejected by WHATWG URL resolution keep fragment-only refs.
pub fn resolve_uri(reference: &str, base: &str) -> String {
    if reference.starts_with('#') && base.starts_with("urn:") {
        return format!("{}{}", document_of(base), reference);
    }
    if let Ok(base_url) = url::Url::parse(base)
        && let Ok(joined) = base_url.join(reference)
    {
        return joined.into();
    }
    if reference.starts_with('#') {
        format!("{}{}", document_of(base), reference)
    } else if let Ok(abs) = url::Url::parse(reference) {
        abs.into()
    } else {
        reference.to_string()
    }
}

/// Strips any fragment from `uri`.
pub fn document_of(uri: &str) -> &str {
    match uri.find('#') {
        Some(hash) => &uri[..hash],
        None => uri,
    }
}

/// Reference resolver owning the [`SchemaArena`] for one compilation/explanation/coercion run.
pub(crate) struct Resolver {
    pub(crate) arena: SchemaArena,
    pub(crate) draft: Draft,
    pub(crate) root: NodeId,
    pub(crate) root_base: String,
    pub(crate) base_of: HashMap<NodeId, String>,
    pub(crate) dynamic_anchors: IndexMap<String, NodeId>,
    raw_registry: IndexMap<String, Value>,
    registry: IndexMap<String, NodeId>,
    reject_unresolved: bool,
    unresolved: HashSet<String>,
    resources: HashMap<String, NodeId>,
    anchors: HashMap<String, NodeId>,
    resolved_cache: HashMap<(String, String), Resolved>,
    indexed: bool,
}

impl Resolver {
    pub(crate) fn new(root_value: &Value, options: &CompileOptions) -> Self {
        let draft = detect_draft(root_value, options.draft);
        let mut arena = SchemaArena::new();
        let root = arena.insert(root_value);
        Self {
            arena,
            draft,
            root,
            root_base: DEFAULT_BASE.to_string(),
            base_of: HashMap::new(),
            dynamic_anchors: IndexMap::new(),
            raw_registry: options.schemas.clone(),
            registry: IndexMap::new(),
            reject_unresolved: options.unresolved_refs == UnresolvedRefs::Reject,
            unresolved: HashSet::new(),
            resources: HashMap::new(),
            anchors: HashMap::new(),
            resolved_cache: HashMap::new(),
            indexed: false,
        }
    }

    /// Read-only base lookup after compilation has indexed the schema.
    pub(crate) fn base_for_readonly(&self, node: NodeId, base: &str) -> String {
        if let Some(indexed) = self.base_of.get(&node) {
            return indexed.clone();
        }
        let Some(id) = self.arena.str_prop(node, "$id") else {
            return base.to_string();
        };
        if self.draft == Draft::Draft7 && (self.arena.str_prop(node, "$ref").is_some() || id.starts_with('#')) {
            return base.to_string();
        }
        document_of(&resolve_uri(id, base)).to_string()
    }

    /// Read-only reference lookup after compilation has resolved reachable references.
    pub(crate) fn resolve_readonly(&self, reference: &str, base: &str) -> Option<&Resolved> {
        self.resolved_cache.get(&(base.to_string(), reference.to_string()))
    }

    /// The base URI of `node`, whose parent has `base`: applies `$id` unless Draft 7 makes it inert.
    pub(crate) fn base_for(&mut self, node: NodeId, base: &str) -> String {
        if let Some(indexed) = self.base_of.get(&node) {
            return indexed.clone();
        }
        let Some(id) = self.arena.str_prop(node, "$id").map(ToString::to_string) else {
            return base.to_string();
        };
        if !self.indexed {
            self.ensure_indexed();
            if let Some(indexed) = self.base_of.get(&node) {
                return indexed.clone();
            }
        }
        if self.draft == Draft::Draft7 && (self.arena.str_prop(node, "$ref").is_some() || id.starts_with('#')) {
            return base.to_string();
        }
        document_of(&resolve_uri(&id, base)).to_string()
    }

    /// Indexes the root schema and registered document URIs once.
    pub(crate) fn ensure_indexed(&mut self) {
        if self.indexed {
            return;
        }
        self.indexed = true;
        self.resources.insert(DEFAULT_BASE.to_string(), self.root);
        self.index(self.root, DEFAULT_BASE);
    }

    /// Resolves `reference` from `base`, returning a target node and base URI.
    pub(crate) fn resolve(&mut self, reference: &str, base: &str) -> Result<Resolved, SchemaError> {
        self.ensure_indexed();
        let key = (base.to_string(), reference.to_string());
        if let Some(cached) = self.resolved_cache.get(&key) {
            return Ok(cached.clone());
        }
        let resolved = if !self.reject_unresolved {
            self.find(reference, base)?
        } else {
            match self.find(reference, base) {
                Ok(r) => r,
                Err(_) => {
                    self.unresolved.insert(resolve_uri(reference, base));
                    let false_node = self.arena.insert(&Value::Bool(false));
                    Resolved {
                        node: false_node,
                        base: base.to_string(),
                    }
                }
            }
        };
        self.resolved_cache.insert(key, resolved.clone());
        Ok(resolved)
    }

    /// Whether `reference` from `base` failed to resolve and was replaced with `false`.
    pub(crate) fn is_unresolved(&self, reference: &str, base: &str) -> bool {
        self.unresolved.contains(&resolve_uri(reference, base))
    }

    /// Whether `node` is the root of the schema resource at `base`.
    pub(crate) fn is_resource(&mut self, node: NodeId, base: &str) -> bool {
        self.ensure_indexed();
        self.resources.get(base).copied() == Some(node)
    }

    fn find(&mut self, reference: &str, base: &str) -> Result<Resolved, SchemaError> {
        let uri = resolve_uri(reference, base);
        let doc = document_of(&uri).to_string();
        let raw_frag = if uri.len() > doc.len() {
            &uri[doc.len() + 1..]
        } else {
            ""
        };
        let fragment = if raw_frag.is_empty() {
            String::new()
        } else {
            pi_js::uri::decode_uri_component(raw_frag).map_err(|_| SchemaError::UnresolvedRef {
                reference: reference.to_string(),
                detail: format!("invalid URI encoding in fragment #{raw_frag}"),
            })?
        };
        let root = match self.resources.get(&doc).copied() {
            Some(node) => node,
            None => self.load(&doc)?.ok_or_else(|| SchemaError::UnresolvedRef {
                reference: reference.to_string(),
                detail: format!("no schema registered for {doc}"),
            })?,
        };
        if fragment.is_empty() {
            return Ok(Resolved { node: root, base: doc });
        }
        if fragment.starts_with('/') {
            return self.pointer(root, &doc, &fragment);
        }
        let anchor_key = format!("{doc}#{fragment}");
        let target = self
            .anchors
            .get(&anchor_key)
            .copied()
            .ok_or_else(|| SchemaError::UnresolvedRef {
                reference: reference.to_string(),
                detail: format!("no anchor {fragment} in {doc}"),
            })?;
        let target_base = self.base_of.get(&target).cloned().unwrap_or(doc);
        Ok(Resolved {
            node: target,
            base: target_base,
        })
    }

    fn load(&mut self, doc: &str) -> Result<Option<NodeId>, SchemaError> {
        if let Some(&id) = self.registry.get(doc) {
            return Ok(Some(id));
        }
        // Lazily populate normalized registry URIs on first external lookup.
        if self.registry.is_empty() && !self.raw_registry.is_empty() {
            let entries: Vec<(String, Value)> = self
                .raw_registry
                .iter()
                .map(|(uri, schema)| (document_of(&resolve_uri(uri, DEFAULT_BASE)).to_string(), schema.clone()))
                .collect();
            for (norm_uri, schema) in entries {
                if norm_uri == doc {
                    let id = self.arena.insert(&schema);
                    self.registry.insert(norm_uri, id);
                    break;
                }
            }
        }
        // Check again or find matching raw entry if registry had multiple items.
        let id = if let Some(&id) = self.registry.get(doc) {
            id
        } else {
            let found = self
                .raw_registry
                .iter()
                .find(|(uri, _)| document_of(&resolve_uri(uri, DEFAULT_BASE)) == doc)
                .map(|(_, schema)| schema.clone());
            let Some(schema) = found else {
                return Ok(None);
            };
            let id = self.arena.insert(&schema);
            self.registry.insert(doc.to_string(), id);
            id
        };

        self.resources.insert(doc.to_string(), id);
        self.index(id, doc);
        Ok(Some(id))
    }

    fn index(&mut self, node: NodeId, base: &str) {
        if !self.arena.is_schema_object(node) || self.base_of.contains_key(&node) {
            return;
        }
        let mut b = base.to_string();
        let ref_only = self.draft == Draft::Draft7 && self.arena.str_prop(node, "$ref").is_some();
        if !ref_only && let Some(id) = self.arena.str_prop(node, "$id").map(ToString::to_string) {
            if self.draft == Draft::Draft7 && id.starts_with('#') {
                let key = format!("{}{}", document_of(&b), id);
                self.anchors.insert(key, node);
            } else {
                let uri = resolve_uri(&id, &b);
                b = document_of(&uri).to_string();
                self.resources.insert(b.clone(), node);
                if uri.len() > b.len() + 1 {
                    self.anchors.insert(uri, node);
                }
            }
        }
        self.base_of.insert(node, b.clone());
        if ref_only {
            return;
        }
        if self.draft != Draft::Draft7 {
            if let Some(anchor) = self.arena.str_prop(node, "$anchor") {
                self.anchors.insert(format!("{b}#{anchor}"), node);
            }
            if let Some(dyn_anchor) = self.arena.str_prop(node, "$dynamicAnchor") {
                let key = format!("{b}#{dyn_anchor}");
                self.anchors.insert(key.clone(), node);
                self.dynamic_anchors.insert(key, node);
            }
        }

        let schema_children: Vec<NodeId> = SCHEMA_KEYWORDS
            .iter()
            .filter_map(|k| self.arena.prop(node, k))
            .collect();
        for child in schema_children {
            self.index(child, &b);
        }

        let map_children: Vec<NodeId> = MAP_KEYWORDS
            .iter()
            .filter_map(|k| self.arena.obj_prop(node, k))
            .flat_map(|map| map.values().copied())
            .collect();
        for child in map_children {
            self.index(child, &b);
        }

        let array_children: Vec<NodeId> = ARRAY_KEYWORDS
            .iter()
            .filter_map(|k| self.arena.array_prop(node, k))
            .flat_map(|list| list.iter().copied())
            .collect();
        for child in array_children {
            self.index(child, &b);
        }
    }

    fn pointer(&self, root: NodeId, base: &str, fragment: &str) -> Result<Resolved, SchemaError> {
        let mut node = root;
        let mut b = base.to_string();
        for raw in fragment.split('/').skip(1) {
            let key = unescape_json_pointer(raw);
            let next = match self.arena.get(node) {
                NodeData::Object(map) => map.get(&key).copied(),
                NodeData::Array(arr) => parse_array_index(&key).and_then(|idx| arr.get(idx).copied()),
                _ => None,
            };
            let Some(child) = next else {
                return Err(SchemaError::UnresolvedRef {
                    reference: format!("#{fragment}"),
                    detail: format!("pointer not found in {base}"),
                });
            };
            node = child;
            if self.arena.is_schema_object(node)
                && let Some(indexed) = self.base_of.get(&node)
            {
                b = indexed.clone();
            }
        }
        Ok(Resolved { node, base: b })
    }
}

fn unescape_json_pointer(raw: &str) -> String {
    if !raw.contains('~') {
        return raw.to_string();
    }
    let mut out = String::with_capacity(raw.len());
    let bytes = raw.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'~' && i + 1 < bytes.len() {
            match bytes[i + 1] {
                b'0' => {
                    out.push('~');
                    i += 2;
                    continue;
                }
                b'1' => {
                    out.push('/');
                    i += 2;
                    continue;
                }
                _ => {}
            }
        }
        let ch = raw[i..].chars().next().unwrap_or('\0');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn parse_array_index(key: &str) -> Option<usize> {
    if key == "0" {
        return Some(0);
    }
    let bytes = key.as_bytes();
    if bytes.is_empty() || !(b'1'..=b'9').contains(&bytes[0]) || !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    key.parse().ok()
}
