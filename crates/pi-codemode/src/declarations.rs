//! Port of packages/codemode/src/declarations.ts

use indexmap::{IndexMap, IndexSet};
use serde_json::Value;

pub use crate::identifier::to_codemode_identifier;

use crate::types::{CodemodeJsonSchema, CodemodeTool};

const INDENT: &str = "  ";
/// Largest rendered input type, in characters, before it becomes `unknown`.
pub const DEFAULT_INPUT_SCHEMA_MAX_CHARS: i64 = 16_000;
/// Local `$ref` expansions per rendered schema, so shared definitions cannot blow up the output.
const MAX_REF_EXPANSIONS: i64 = 32;
/// JSON Schema keywords with a number or string value shown as `@keyword value` property tags.
const TAG_KEYWORDS: &[&str] = &[
    "minimum",
    "exclusiveMinimum",
    "maximum",
    "exclusiveMaximum",
    "multipleOf",
    "minLength",
    "maxLength",
    "pattern",
    "format",
    "minItems",
    "maxItems",
];
/// Longest property tag; longer ones (large defaults, long patterns) are left out.
const MAX_TAG_CHARS: usize = 80;

/// TypeScript types for MCP results, from the MCP `CallToolResult` schema, so `CallToolResult<T>`
/// declarations can refer to them.
pub const MCP_TYPESCRIPT_PREAMBLE: &str = r#"type Role = "user" | "assistant";
type MetaObject = Record<string, unknown>;
type Annotations = {
  audience?: Role[];
  priority?: number;
  lastModified?: string;
};
type Icon = {
  src: string;
  mimeType?: string;
  sizes?: string[];
  theme?: "light" | "dark";
};
type TextResourceContents = {
  uri: string;
  mimeType?: string;
  _meta?: MetaObject;
  text: string;
};
type BlobResourceContents = {
  uri: string;
  mimeType?: string;
  _meta?: MetaObject;
  blob: string;
};
type TextContent = {
  type: "text";
  text: string;
  annotations?: Annotations;
  _meta?: MetaObject;
};
type ImageContent = {
  type: "image";
  data: string;
  mimeType: string;
  annotations?: Annotations;
  _meta?: MetaObject;
};
type AudioContent = {
  type: "audio";
  data: string;
  mimeType: string;
  annotations?: Annotations;
  _meta?: MetaObject;
};
type ResourceLink = {
  icons?: Icon[];
  name: string;
  title?: string;
  uri: string;
  description?: string;
  mimeType?: string;
  annotations?: Annotations;
  size?: number;
  _meta?: MetaObject;
  type: "resource_link";
};
type EmbeddedResource = {
  type: "resource";
  resource: TextResourceContents | BlobResourceContents;
  annotations?: Annotations;
  _meta?: MetaObject;
};
type ContentBlock =
  | TextContent
  | ImageContent
  | AudioContent
  | ResourceLink
  | EmbeddedResource;
type CallToolResult<TStructured = { [key: string]: unknown }> = {
  _meta?: MetaObject;
  content: ContentBlock[];
  isError?: boolean;
  structuredContent?: TStructured;
  [key: string]: unknown;
};"#;

#[derive(Clone, Debug, Default)]
pub struct RenderDeclarationsOptions {
    pub tools: Option<Vec<CodemodeTool>>,
    pub globals: Option<Vec<CodemodeTool>>,
}

/// `{ inputMaxChars?: number }` for [`render_tool_signature`] and [`render_tool_sample`].
#[derive(Clone, Debug, Default)]
pub struct RenderToolSignatureOptions {
    pub input_max_chars: Option<i64>,
}

/// `{ maxChars?: number }` for [`schema_to_type`].
#[derive(Clone, Debug, Default)]
pub struct SchemaToTypeOptions {
    pub max_chars: Option<i64>,
}

/// Render TypeScript declarations for the script-visible API. Tools become members of
/// `declare const tools`, globals become `declare function` statements, and `ns.member` globals
/// members of `declare const ns`. Descriptions become doc comments; schemas become types.
pub fn render_declarations(options: RenderDeclarationsOptions) -> String {
    let mut sections: Vec<String> = Vec::new();
    let tools = options.tools.unwrap_or_default();
    if !tools.is_empty() {
        let members: Vec<String> = tools
            .iter()
            .map(|tool| {
                format!(
                    "{}{INDENT}{}",
                    doc_comment(tool.description.as_deref(), INDENT),
                    render_tool_signature(tool, None)
                )
            })
            .collect();
        sections.push(format!("declare const tools: {{\n{}\n}};", members.join("\n")));
    }

    let mut namespaces: IndexMap<String, Vec<String>> = IndexMap::new();
    for global in options.globals.unwrap_or_default() {
        let Some(dot) = global.name.find('.') else {
            sections.push(render_global(&format!("declare function {}", global.name), &global, ""));
            continue;
        };
        let namespace = global.name[..dot].to_string();
        let member_name = &global.name[dot + 1..];
        let entry = namespaces.entry(namespace).or_default();
        entry.push(render_global(member_name, &global, INDENT));
    }
    for (namespace, members) in namespaces {
        sections.push(format!("declare const {namespace}: {{\n{}\n}};", members.join("\n")));
    }
    sections.join("\n\n")
}

/// One tool as a member of the `tools` object: `name(args: T): Promise<R>;` with the
/// name as the identifier scripts use. Input types longer than `inputMaxChars` render as `unknown`.
pub fn render_tool_signature(tool: &CodemodeTool, options: Option<RenderToolSignatureOptions>) -> String {
    let max_chars = options
        .and_then(|o| o.input_max_chars)
        .unwrap_or(DEFAULT_INPUT_SCHEMA_MAX_CHARS);
    let input = match &tool.input_schema {
        None => "unknown".to_string(),
        Some(schema) => schema_to_type(
            schema,
            Some(SchemaToTypeOptions {
                max_chars: Some(max_chars),
            }),
        ),
    };
    format!(
        "{}(args: {input}): Promise<{}>;",
        to_codemode_identifier(&tool.name),
        render_tool_output_type(tool.output_schema.as_ref())
    )
}

/// A tool's sample: the description followed by the tool's declaration. Used for tool
/// listings and `ALL_TOOLS` entries.
pub fn render_tool_sample(tool: &CodemodeTool, options: Option<RenderToolSignatureOptions>) -> String {
    let declaration = format!("declare const tools: {{ {} }};", render_tool_signature(tool, options));
    let description = tool.description.as_deref().map(pi_js::str16::trim).unwrap_or("");
    format!("{description}\n\ncodemode tool declaration:\n```ts\n{declaration}\n```")
}

/// The `structuredContent` schema of an MCP `CallToolResult` output schema (detected by a
/// `content` array of objects, boolean `isError`, and object `_meta`), `true` when it declares none, or
/// `None` when the schema is not a `CallToolResult`.
pub fn mcp_structured_content_schema(schema: Option<&CodemodeJsonSchema>) -> Option<CodemodeJsonSchema> {
    let CodemodeJsonSchema::Object(map) = schema? else {
        return None;
    };
    let props = map.get("properties")?.as_object()?;
    let content = props.get("content")?.as_object()?;
    if content.get("type")?.as_str()? != "array" {
        return None;
    }
    let items = content.get("items")?.as_object()?;
    if items.get("type")?.as_str()? != "object" {
        return None;
    }
    let is_error = props.get("isError")?.as_object()?;
    if is_error.get("type")?.as_str()? != "boolean" {
        return None;
    }
    let meta = props.get("_meta")?.as_object()?;
    if meta.get("type")?.as_str()? != "object" {
        return None;
    }

    match props.get("structuredContent") {
        Some(Value::Object(obj)) => Some(json_map_to_schema(obj)),
        Some(Value::Bool(b)) => Some(CodemodeJsonSchema::Boolean(*b)),
        _ => Some(CodemodeJsonSchema::Boolean(true)),
    }
}

/// The type a tool call resolves to: the output schema's type, and `unknown` without a schema.
pub fn render_tool_output_type(schema: Option<&CodemodeJsonSchema>) -> String {
    match schema {
        None => "unknown".to_string(),
        Some(schema) => schema_to_type(schema, None),
    }
}

fn render_global(head: &str, global: &CodemodeTool, indent: &str) -> String {
    let comment = doc_comment(global.description.as_deref(), indent);
    if let Some(signature) = &global.signature {
        return format!("{comment}{indent}{head}{signature};");
    }
    let input = match &global.input_schema {
        None => "unknown".to_string(),
        Some(schema) => schema_to_type(schema, None),
    };
    let output = match &global.output_schema {
        None => "unknown".to_string(),
        Some(schema) => schema_to_type(schema, None),
    };
    format!("{comment}{indent}{head}(args: {input}): Promise<{output}>;")
}

fn doc_comment(description: Option<&str>, indent: &str) -> String {
    let Some(text) = description.map(pi_js::str16::trim).filter(|s| !s.is_empty()) else {
        return String::new();
    };
    let normalized = text.replace("*/", "*\\/").replace("\r\n", "\n");
    let lines: Vec<&str> = normalized.split('\n').collect();
    if lines.len() == 1 {
        return format!("{indent}/** {} */\n", lines[0]);
    }
    let body: Vec<String> = lines
        .into_iter()
        .map(|line| {
            if line.is_empty() {
                format!("{indent} *")
            } else {
                format!("{indent} * {line}")
            }
        })
        .collect();
    format!("{indent}/**\n{}\n{indent} */\n", body.join("\n"))
}

fn is_ascii_js_identifier(name: &str) -> bool {
    let mut bytes = name.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    let first_ok = first.is_ascii_alphabetic() || first == b'_' || first == b'$';
    first_ok && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'$')
}

fn property_key(name: &str) -> String {
    if is_ascii_js_identifier(name) {
        name.to_string()
    } else {
        pi_js::json::stringify(name)
    }
}

fn union(types: &[String]) -> String {
    let mut unique: IndexSet<&str> = IndexSet::new();
    for t in types {
        unique.insert(t.as_str());
    }
    if unique.contains("unknown") {
        return "unknown".to_string();
    }
    if unique.is_empty() {
        "never".to_string()
    } else {
        unique.into_iter().collect::<Vec<_>>().join(" | ")
    }
}

/// Convert a JSON Schema to a TypeScript type expression: objects on one line (`{ a: string; b?: number; }`) with properties sorted by name,
/// or one property per line with `//` comments when a property has a description or constraints the
/// type cannot express (`// @integer @minimum 1 @default 20`); `Array<T>` for arrays. Local references
/// (`#/$defs/...`, `#/definitions/...`) resolve against `schema`; recursive and remote references
/// render as `unknown`. A result longer than `maxChars` is rendered again without the constraint tags,
/// and renders as `unknown` if it is still too long.
pub fn schema_to_type(schema: &CodemodeJsonSchema, options: Option<SchemaToTypeOptions>) -> String {
    let render = |tags: bool| {
        let mut context = SchemaContext {
            root: schema.clone(),
            resolving: IndexSet::new(),
            expansions: 0,
            tags,
        };
        to_type(schema, &mut context)
    };
    let ty = render(true);
    let Some(max_chars) = options.and_then(|o| o.max_chars) else {
        return ty;
    };
    if (pi_js::str16::len(&ty) as i64) <= max_chars {
        return ty;
    }
    let untagged = render(false);
    if (pi_js::str16::len(&untagged) as i64) <= max_chars {
        untagged
    } else {
        "unknown".to_string()
    }
}

struct SchemaContext {
    root: CodemodeJsonSchema,
    /// References being expanded on the current path, to stop at recursive types.
    resolving: IndexSet<String>,
    expansions: i64,
    /// Whether property comments include constraint tags.
    tags: bool,
}

#[derive(Clone, Copy)]
enum SchemaMap<'a> {
    Index(&'a IndexMap<String, Value>),
    Json(&'a serde_json::Map<String, Value>),
}

impl<'a> SchemaMap<'a> {
    fn get(&self, key: &str) -> Option<&'a Value> {
        match self {
            SchemaMap::Index(m) => m.get(key),
            SchemaMap::Json(m) => m.get(key),
        }
    }

    fn contains_key(&self, key: &str) -> bool {
        match self {
            SchemaMap::Index(m) => m.contains_key(key),
            SchemaMap::Json(m) => m.contains_key(key),
        }
    }

    fn clone_with_type(&self, entry: Value) -> IndexMap<String, Value> {
        let mut cloned = match self {
            SchemaMap::Index(m) => (*m).clone(),
            SchemaMap::Json(m) => m.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        };
        cloned.insert("type".to_string(), entry);
        cloned
    }
}

fn json_map_to_schema(map: &serde_json::Map<String, Value>) -> CodemodeJsonSchema {
    CodemodeJsonSchema::Object(map.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
}

enum RefTarget<'a> {
    Bool(bool),
    Map(SchemaMap<'a>),
}

fn decode_pointer_segments(r#ref: &str) -> Option<Vec<String>> {
    if r#ref != "#" && !r#ref.starts_with("#/") {
        return None;
    }
    let rest = r#ref.get(2..).unwrap_or("");
    let mut keys = Vec::new();
    for segment in rest.split('/').filter(|s| !s.is_empty()) {
        // Schemas come from tools and MCP servers: a malformed escape names nothing instead of failing the caller.
        let decoded = pi_js::uri::decode_uri_component(segment).ok()?;
        keys.push(decoded.replace("~1", "/").replace("~0", "~"));
    }
    Some(keys)
}

/// The subschema a local `$ref` (`#`, `#/$defs/Item`) names in `root`, or `None` for remote
/// references and references that name nothing. Pointer segments are URI-decoded and JSON Pointer
/// unescaped (`~1` is `/`, `~0` is `~`).
pub fn resolve_schema_ref(r#ref: &str, root: &Value) -> Option<CodemodeJsonSchema> {
    let keys = decode_pointer_segments(r#ref)?;
    let mut current = root;
    for key in keys {
        let Value::Object(obj) = current else {
            return None;
        };
        current = obj.get(&key)?;
    }
    match current {
        Value::Bool(b) => Some(CodemodeJsonSchema::Boolean(*b)),
        Value::Object(map) => Some(json_map_to_schema(map)),
        _ => None,
    }
}

fn resolve_ref_inner<'a>(r#ref: &str, root: &'a CodemodeJsonSchema) -> Option<RefTarget<'a>> {
    let keys = decode_pointer_segments(r#ref)?;
    if keys.is_empty() {
        return Some(match root {
            CodemodeJsonSchema::Boolean(b) => RefTarget::Bool(*b),
            CodemodeJsonSchema::Object(m) => RefTarget::Map(SchemaMap::Index(m)),
        });
    }

    enum Cursor<'a> {
        Root(&'a CodemodeJsonSchema),
        Val(&'a Value),
    }

    let mut current = Cursor::Root(root);
    for key in keys {
        let next = match current {
            Cursor::Root(CodemodeJsonSchema::Object(m)) => m.get(&key)?,
            Cursor::Root(CodemodeJsonSchema::Boolean(_)) => return None,
            Cursor::Val(Value::Object(m)) => m.get(&key)?,
            Cursor::Val(_) => return None,
        };
        current = Cursor::Val(next);
    }

    match current {
        Cursor::Root(CodemodeJsonSchema::Boolean(b)) => Some(RefTarget::Bool(*b)),
        Cursor::Root(CodemodeJsonSchema::Object(m)) => Some(RefTarget::Map(SchemaMap::Index(m))),
        Cursor::Val(Value::Bool(b)) => Some(RefTarget::Bool(*b)),
        Cursor::Val(Value::Object(m)) => Some(RefTarget::Map(SchemaMap::Json(m))),
        Cursor::Val(_) => None,
    }
}

fn resolve_ref(r#ref: &str, root: &CodemodeJsonSchema) -> Option<CodemodeJsonSchema> {
    match resolve_ref_inner(r#ref, root)? {
        RefTarget::Bool(b) => Some(CodemodeJsonSchema::Boolean(b)),
        RefTarget::Map(SchemaMap::Index(m)) => Some(CodemodeJsonSchema::Object(m.clone())),
        RefTarget::Map(SchemaMap::Json(m)) => Some(json_map_to_schema(m)),
    }
}

fn to_type(schema: &CodemodeJsonSchema, context: &mut SchemaContext) -> String {
    match schema {
        CodemodeJsonSchema::Boolean(true) => "unknown".to_string(),
        CodemodeJsonSchema::Boolean(false) => "never".to_string(),
        CodemodeJsonSchema::Object(map) => to_type_map(SchemaMap::Index(map), context),
    }
}

fn to_type_value(value: &Value, context: &mut SchemaContext) -> String {
    match value {
        Value::Bool(true) => "unknown".to_string(),
        Value::Bool(false) => "never".to_string(),
        Value::Object(map) => to_type_map(SchemaMap::Json(map), context),
        _ => "unknown".to_string(),
    }
}

fn to_type_map(schema: SchemaMap<'_>, context: &mut SchemaContext) -> String {
    if let Some(Value::String(ref_str)) = schema.get("$ref") {
        let ref_owned = ref_str.clone();
        if context.resolving.contains(&ref_owned) || context.expansions >= MAX_REF_EXPANSIONS {
            return "unknown".to_string();
        }
        let target = resolve_ref(&ref_owned, &context.root);
        let Some(target) = target else {
            return "unknown".to_string();
        };
        context.expansions += 1;
        context.resolving.insert(ref_owned.clone());
        let rendered = to_type(&target, context);
        context.resolving.shift_remove(&ref_owned);
        return rendered;
    }

    if let Some(const_val) = schema.get("const") {
        return pi_js::json::stringify(const_val);
    }
    if let Some(Value::Array(enum_vals)) = schema.get("enum") {
        let parts: Vec<String> = enum_vals.iter().map(pi_js::json::stringify).collect();
        return union(&parts);
    }

    let variants = match schema.get("anyOf") {
        Some(Value::Array(arr)) => Some(arr),
        _ => match schema.get("oneOf") {
            Some(Value::Array(arr)) => Some(arr),
            _ => None,
        },
    };
    if let Some(variants) = variants {
        let parts: Vec<String> = variants.iter().map(|v| to_type_value(v, context)).collect();
        return union(&parts);
    }

    if let Some(Value::Array(all_of)) = schema.get("allOf") {
        let parts: Vec<String> = all_of
            .iter()
            .map(|part| to_type_value(part, context))
            .filter(|part| part != "unknown")
            .collect();
        if parts.is_empty() {
            return "unknown".to_string();
        }
        return parts
            .into_iter()
            .map(|part| {
                if part.contains(" | ") {
                    format!("({part})")
                } else {
                    part
                }
            })
            .collect::<Vec<_>>()
            .join(" & ");
    }

    match schema.get("type") {
        Some(Value::Array(entries)) => {
            let parts: Vec<String> = entries
                .iter()
                .map(|entry| {
                    let cloned = schema.clone_with_type(entry.clone());
                    to_type_map(SchemaMap::Index(&cloned), context)
                })
                .collect();
            union(&parts)
        }
        Some(Value::String(ty)) => match ty.as_str() {
            "string" => "string".to_string(),
            "number" | "integer" => "number".to_string(),
            "boolean" => "boolean".to_string(),
            "null" => "null".to_string(),
            "array" => array_type_map(schema, context),
            "object" => object_type_map(schema, context),
            _ => "unknown".to_string(),
        },
        None => {
            if schema.contains_key("properties")
                || schema.contains_key("additionalProperties")
                || schema.contains_key("required")
            {
                object_type_map(schema, context)
            } else if schema.contains_key("items") || schema.contains_key("prefixItems") {
                array_type_map(schema, context)
            } else {
                "unknown".to_string()
            }
        }
        Some(_) => "unknown".to_string(),
    }
}

fn array_type_map(schema: SchemaMap<'_>, context: &mut SchemaContext) -> String {
    if let Some(items) = schema.get("items")
        && !items.is_array()
    {
        return format!("Array<{}>", to_type_value(items, context));
    }
    let tuple = match schema.get("prefixItems") {
        Some(Value::Array(arr)) => Some(arr.as_slice()),
        _ => match schema.get("items") {
            Some(Value::Array(arr)) => Some(arr.as_slice()),
            _ => None,
        },
    };
    if let Some(tuple) = tuple
        && !tuple.is_empty()
    {
        let rendered: Vec<String> = tuple.iter().map(|item| to_type_value(item, context)).collect();
        return format!("[{}]", rendered.join(", "));
    }
    "unknown[]".to_string()
}

fn description_of(property: &Value) -> String {
    property
        .as_object()
        .and_then(|obj| obj.get("description"))
        .and_then(Value::as_str)
        .map(|s| pi_js::str16::trim(s).to_string())
        .unwrap_or_default()
}

fn is_line_terminator(c: char) -> bool {
    matches!(c, '\r' | '\n' | '\u{2028}' | '\u{2029}')
}

fn escape_line_terminators(tag: &str) -> String {
    let mut out = String::with_capacity(tag.len());
    for c in tag.chars() {
        if is_line_terminator(c) {
            out.push_str(&format!("\\u{:04x}", c as u32));
        } else {
            out.push(c);
        }
    }
    out
}

fn is_lower_bound(keyword: &str) -> bool {
    matches!(keyword, "minimum" | "exclusiveMinimum" | "minLength" | "minItems")
}

fn is_upper_bound(keyword: &str) -> bool {
    matches!(keyword, "maximum" | "exclusiveMaximum" | "maxLength" | "maxItems")
}

fn normalize_constraint_branch(branch: &serde_json::Map<String, Value>) -> IndexMap<String, Value> {
    let mut map: IndexMap<String, Value> = branch.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    // Draft-04 and OpenAPI 3.0 make `minimum`/`maximum` exclusive with a boolean `exclusiveMinimum`/`exclusiveMaximum`.
    if map.get("exclusiveMinimum") == Some(&Value::Bool(true)) {
        if let Some(min_val) = map.get("minimum").cloned() {
            map.insert("exclusiveMinimum".to_string(), min_val);
        } else {
            map.shift_remove("exclusiveMinimum");
        }
        map.shift_remove("minimum");
    }
    if map.get("exclusiveMaximum") == Some(&Value::Bool(true)) {
        if let Some(max_val) = map.get("maximum").cloned() {
            map.insert("exclusiveMaximum".to_string(), max_val);
        } else {
            map.shift_remove("exclusiveMaximum");
        }
        map.shift_remove("maximum");
    }
    map
}

fn has_integer_type(schema: &IndexMap<String, Value>) -> bool {
    match schema.get("type") {
        Some(Value::String(s)) => s == "integer",
        Some(Value::Array(items)) => items.iter().any(|item| item.as_str() == Some("integer")),
        _ => false,
    }
}

/// Constraints of a property that its TypeScript type cannot express, as JSDoc-style tags:
/// `@integer @minimum 1 @maximum 100 @default 20`. Empty when there are none.
fn constraint_tags(schema: &Value) -> String {
    let Some(property) = schema.as_object() else {
        return String::new();
    };
    // Optional fields such as Pydantic's `int | None` keep their constraints in the non-null branch.
    // The branch's constraints apply together with the property's, so both are read.
    let variants = property
        .get("anyOf")
        .and_then(Value::as_array)
        .or_else(|| property.get("oneOf").and_then(Value::as_array));
    let non_null: Vec<&Value> = variants
        .map(|items| {
            items
                .iter()
                .filter(|variant| {
                    variant
                        .as_object()
                        .and_then(|obj| obj.get("type"))
                        .and_then(Value::as_str)
                        != Some("null")
                })
                .collect()
        })
        .unwrap_or_default();
    let mut branches = vec![property];
    if non_null.len() == 1
        && let Some(branch) = non_null[0].as_object()
    {
        branches.push(branch);
    }
    let schemas: Vec<IndexMap<String, Value>> = branches.into_iter().map(normalize_constraint_branch).collect();

    let mut tags: Vec<String> = Vec::new();
    if schemas.iter().any(has_integer_type) {
        tags.push("@integer".to_string());
    }
    for &keyword in TAG_KEYWORDS {
        if is_lower_bound(keyword) || is_upper_bound(keyword) {
            let mut best: Option<f64> = None;
            for branch in &schemas {
                if let Some(num) = branch.get(keyword).and_then(Value::as_f64) {
                    best = Some(match best {
                        None => num,
                        Some(cur) if is_lower_bound(keyword) => cur.max(num),
                        Some(cur) => cur.min(num),
                    });
                }
            }
            if let Some(num) = best {
                tags.push(format!("@{keyword} {}", pi_js::json::number_to_string(num)));
            }
            continue;
        }
        #[derive(PartialEq, Eq, Hash)]
        enum UniqueVal {
            Num(String),
            Str(String),
        }
        let mut seen: IndexSet<UniqueVal> = IndexSet::new();
        for branch in &schemas {
            match branch.get(keyword) {
                Some(Value::Number(n)) => {
                    if let Some(f) = n.as_f64() {
                        seen.insert(UniqueVal::Num(pi_js::json::number_to_string(f)));
                    }
                }
                Some(Value::String(s)) => {
                    seen.insert(UniqueVal::Str(s.clone()));
                }
                _ => {}
            }
        }
        for val in seen {
            match val {
                UniqueVal::Num(s) | UniqueVal::Str(s) => tags.push(format!("@{keyword} {s}")),
            }
        }
    }
    if schemas
        .iter()
        .any(|branch| branch.get("uniqueItems") == Some(&Value::Bool(true)))
    {
        tags.push("@uniqueItems".to_string());
    }
    // Annotations, unlike constraints, come from the property itself.
    if let Some(default_val) = property.get("default")
        && !default_val.is_null()
    {
        tags.push(format!("@default {}", pi_js::json::stringify(default_val)));
    }
    if property.get("deprecated") == Some(&Value::Bool(true)) {
        tags.push("@deprecated".to_string());
    }
    tags.into_iter()
        .filter(|tag| pi_js::str16::len(tag) <= MAX_TAG_CHARS)
        .map(|tag| escape_line_terminators(&tag))
        .collect::<Vec<_>>()
        .join(" ")
}

fn object_type_map(schema: SchemaMap<'_>, context: &mut SchemaContext) -> String {
    let empty_props = serde_json::Map::new();
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .unwrap_or(&empty_props);

    let mut required: IndexSet<String> = IndexSet::new();
    if let Some(Value::Array(req)) = schema.get("required") {
        for item in req {
            if let Some(s) = item.as_str() {
                required.insert(s.to_string());
            }
        }
    }

    let mut names: Vec<String> = properties.keys().cloned().collect();
    names.sort_by(|a, b| pi_js::str16::cmp(a, b));

    let mut members: Vec<String> = names
        .iter()
        .map(|name| {
            let optional = if required.contains(name) { "" } else { "?" };
            let prop_ty = to_type_value(&properties[name], context);
            format!("{}{optional}: {prop_ty};", property_key(name))
        })
        .collect();

    match schema.get("additionalProperties") {
        Some(Value::Bool(false)) => {}
        Some(additional) => {
            let ty = if additional == &Value::Bool(true) {
                "unknown".to_string()
            } else {
                to_type_value(additional, context)
            };
            members.push(format!("[key: string]: {ty};"));
        }
        None if names.is_empty() => {
            members.push("[key: string]: unknown;".to_string());
        }
        None => {}
    }

    if members.is_empty() {
        return "{}".to_string();
    }
    let comments: Vec<Vec<String>> = names
        .iter()
        .map(|name| {
            let prop = &properties[name];
            let mut comment_lines: Vec<String> = description_of(prop)
                .split(is_line_terminator)
                .map(pi_js::str16::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_string)
                .collect();
            if context.tags {
                let tags = constraint_tags(prop);
                if !tags.is_empty() {
                    comment_lines.push(tags);
                }
            }
            comment_lines
        })
        .collect();
    if comments.iter().all(Vec::is_empty) {
        return format!("{{ {} }}", members.join(" "));
    }

    let mut lines = vec!["{".to_string()];
    for (index, comment) in comments.iter().enumerate() {
        for line in comment {
            lines.push(format!("{INDENT}// {line}"));
        }
        lines.push(format!(
            "{INDENT}{}",
            members[index].replace('\n', &format!("\n{INDENT}"))
        ));
    }
    for member in &members[names.len()..] {
        lines.push(format!("{INDENT}{member}"));
    }
    lines.push("}".to_string());
    lines.join("\n")
}
