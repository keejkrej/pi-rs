// Generates TypeBox 1.3.27 golden vectors for the pi-js port.
// Run: node crates/pi-js/src/vendor/typebox/gen_vectors.mjs
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { Type } from "file:///C:/Users/ctyja/workspace/pi/node_modules/typebox/build/index.mjs";
import { Compile } from "file:///C:/Users/ctyja/workspace/pi/node_modules/typebox/build/compile/index.mjs";
import * as Value from "file:///C:/Users/ctyja/workspace/pi/node_modules/typebox/build/value/value.mjs";

const builders = {
	object_basic: () =>
		Type.Object({
			path: Type.String({ description: "Path to the file" }),
			limit: Type.Optional(Type.Number({ minimum: 1 })),
		}),
	object_all_optional: () =>
		Type.Object({
			offset: Type.Optional(Type.Number()),
			enabled: Type.Optional(Type.Boolean()),
		}),
	object_nested: () =>
		Type.Object({
			path: Type.String(),
			offset: Type.Optional(Type.Number()),
			nullable: Type.Optional(Type.Union([Type.String(), Type.Null()])),
			metadata: Type.Object({ enabled: Type.Optional(Type.Boolean()) }),
		}),
	literal_string: () => Type.Literal("a"),
	literal_number: () => Type.Literal(1),
	literal_bool: () => Type.Literal(true),
	record_string: () => Type.Record(Type.String(), Type.Number()),
	record_integer: () => Type.Record(Type.Integer(), Type.String()),
	record_number: () => Type.Record(Type.Number(), Type.Boolean()),
	record_literal: () => Type.Record(Type.Literal("id"), Type.String()),
	record_bool_key: () => Type.Record(Type.Boolean(), Type.Number()),
	union: () => Type.Union([Type.String(), Type.Null()], { description: "nullable" }),
	intersect: () => Type.Intersect([Type.Object({ a: Type.String() }), Type.Object({ b: Type.Number() })]),
	array: () => Type.Array(Type.String(), { minItems: 1 }),
	tuple: () => Type.Tuple([Type.String(), Type.Number()]),
	enum_str: () => Type.Enum(["add", "subtract"]),
	enum_num: () => Type.Enum([1, 2, 3]),
	any: () => Type.Any(),
	unknown: () => Type.Unknown(),
	string: () => Type.String(),
	number: () => Type.Number(),
	integer: () => Type.Integer({ minimum: 0 }),
	boolean: () => Type.Boolean(),
	null: () => Type.Null(),
	string_constraints: () => Type.String({ minLength: 1, maxLength: 4, pattern: "^[a-z]+$" }),
	unsafe_enum: () =>
		Type.Unsafe({
			type: "string",
			enum: ["add", "subtract"],
			description: "The operation to perform",
		}),
	ref_defs: () =>
		Type.Unsafe({
			type: "object",
			properties: { value: { $ref: "#/$defs/value" } },
			$defs: { value: { anyOf: [{ type: "number" }, { type: "null" }] } },
		}),
	defs_keyword: () =>
		Type.Unsafe({
			type: "object",
			properties: { value: { $ref: "#/definitions/value" } },
			definitions: { value: { type: "string" } },
		}),
	number_bounds: () =>
		Type.Number({ minimum: 1, maximum: 10, exclusiveMinimum: 0, exclusiveMaximum: 11, multipleOf: 1 }),
	array_keywords: () =>
		Type.Array(Type.Number(), { minItems: 1, maxItems: 3, uniqueItems: true }),
	prefix: () =>
		Type.Unsafe({
			type: "array",
			prefixItems: [{ type: "string" }, { type: "number" }],
			items: { type: "boolean" },
		}),
	additional: () => Type.Object({ a: Type.String() }, { additionalProperties: false }),
	additional_schema: () => Type.Object({ a: Type.String() }, { additionalProperties: Type.Number() }),
	pattern_props: () =>
		Type.Unsafe({
			type: "object",
			patternProperties: { "^x-": { type: "number" } },
			additionalProperties: false,
		}),
	not_schema: () => Type.Unsafe({ not: { type: "string" } }),
	one_of: () => Type.Unsafe({ oneOf: [{ type: "string" }, { type: "number" }] }),
	all_of: () => Type.Unsafe({ allOf: [{ type: "object" }, { required: ["a"], properties: { a: { type: "string" } } }] }),
	format_email: () => Type.String({ format: "email" }),
	format_uuid: () => Type.String({ format: "uuid" }),
	format_date: () => Type.String({ format: "date" }),
	format_date_time: () => Type.String({ format: "date-time" }),
	format_uri: () => Type.String({ format: "uri" }),
	format_ipv4: () => Type.String({ format: "ipv4" }),
	type_array: () => Type.Unsafe({ type: ["number", "string"] }),
	with_default: () => Type.Object({ a: Type.String({ default: "hi" }), b: Type.Optional(Type.Number({ default: 1 })) }),
	union_obj: () => Type.Union([Type.Object({ nested: Type.String() }), Type.Null()]),
	enum_as_union_input: () => Type.Enum(["a", "b"]),
};

function err(e) {
	return {
		keyword: e.keyword,
		schemaPath: e.schemaPath,
		instancePath: e.instancePath,
		params: e.params,
		message: e.message,
	};
}

function run(schema, value) {
	const input = structuredClone(value);
	let converted;
	let convertThrew = null;
	try {
		converted = Value.Convert(schema, structuredClone(value));
	} catch (e) {
		convertThrew = String(e && e.message ? e.message : e);
	}
	let cleaned;
	let cleanThrew = null;
	try {
		cleaned = Value.Clean(schema, structuredClone(value));
	} catch (e) {
		cleanThrew = String(e && e.message ? e.message : e);
	}
	let defaulted;
	let defaultThrew = null;
	try {
		defaulted = Value.Default(schema, structuredClone(value));
	} catch (e) {
		defaultThrew = String(e && e.message ? e.message : e);
	}
	const check = Value.Check(schema, input);
	const errors = Value.Errors(schema, input).map(err);
	const compiled = Compile(schema);
	const compileCheck = compiled.Check(input);
	const compileErrors = compiled.Errors(input).map(err);
	return {
		check,
		compileCheck,
		errors,
		compileErrors,
		converted,
		convertThrew,
		cleaned,
		cleanThrew,
		defaulted,
		defaultThrew,
	};
}

const cases = [];
function add(id, value) {
	cases.push({ id, value, ...run(builders[id](), value) });
}

add("object_basic", { path: "a.txt", limit: 2 });
add("object_basic", { path: "a.txt" });
add("object_basic", { limit: 1 });
add("object_basic", { path: "a.txt", limit: "4" });
add("object_basic", { path: 1, limit: true });
add("object_basic", { path: null, extra: 1 });
add("object_all_optional", {});
add("object_all_optional", { offset: "3", enabled: "true", extra: 1 });
add("object_nested", { path: "file.txt", offset: null, nullable: null, metadata: { enabled: null } });
add("object_nested", { path: "file.txt", nullable: null, metadata: {} });
add("literal_string", "a");
add("literal_string", "b");
add("literal_string", 1);
add("literal_number", 1);
add("literal_number", "1");
add("literal_bool", true);
add("literal_bool", "true");
add("literal_bool", "TRUE");
add("record_string", { a: "1", b: 2 });
add("record_string", { a: "nope" });
add("record_integer", { "1": "a", "-2": "b", x: "c" });
add("record_number", { "1.5": "true", a: 1 });
add("record_literal", { id: "abc", other: 1 });
add("union", "x");
add("union", null);
add("union", "42");
add("union", 1);
add("intersect", { a: "s", b: "2", c: true });
add("intersect", { a: "s" });
add("array", ["a", "b"]);
add("array", "a");
add("array", [1, "true"]);
add("array", []);
add("tuple", ["a", "1", true]);
add("tuple", ["a"]);
add("tuple", ["a", 1]);
add("enum_str", "add");
add("enum_str", "nope");
add("enum_str", "ADD");
add("enum_num", 2);
add("enum_num", "2");
add("enum_num", 4);
add("any", { x: 1 });
add("unknown", null);
add("string", null);
add("string", true);
add("string", 1);
add("string", "hi");
add("number", "42");
add("number", "42.5");
add("number", true);
add("number", false);
add("number", null);
add("number", "true");
add("number", "TRUE");
add("number", "");
add("number", "  7 ");
add("number", "nope");
add("integer", "42.9");
add("integer", "42");
add("integer", true);
add("integer", null);
add("integer", "-3.2");
add("integer", -1);
add("boolean", "true");
add("boolean", "FALSE");
add("boolean", 1);
add("boolean", 0);
add("boolean", null);
add("boolean", "1");
add("boolean", "0");
add("boolean", 2);
add("null", "");
add("null", 0);
add("null", false);
add("null", "null");
add("null", "NULL");
add("null", "undefined");
add("null", "nope");
add("null", 1);
add("string_constraints", "ab");
add("string_constraints", "");
add("string_constraints", "abcde");
add("string_constraints", "A");
add("string_constraints", "a\nb");
add("unsafe_enum", "add");
add("unsafe_enum", "multiply");
add("ref_defs", { value: null });
add("ref_defs", { value: 1 });
add("ref_defs", { value: "1" });
add("defs_keyword", { value: "ok" });
add("defs_keyword", { value: 1 });
add("number_bounds", 5);
add("number_bounds", 0);
add("number_bounds", 1);
add("number_bounds", 11);
add("number_bounds", 10);
add("number_bounds", 2.5);
add("array_keywords", [1, 2, 2]);
add("array_keywords", [1, 2, 3, 4]);
add("array_keywords", []);
add("array_keywords", [1, "2", 3]);
add("prefix", ["a", 1, true, false]);
add("prefix", ["a"]);
add("prefix", [1, 2]);
add("additional", { a: "x", b: 1 });
add("additional", { a: "x" });
add("additional_schema", { a: "x", b: "3", c: true });
add("pattern_props", { "x-a": "1", b: 2 });
add("pattern_props", { "x-a": 1 });
add("not_schema", 1);
add("not_schema", "a");
add("one_of", "a");
add("one_of", 1);
add("one_of", true);
add("all_of", { a: "x", b: 1 });
add("all_of", { b: 1 });
add("format_email", "a@b.co");
add("format_email", "not-an-email");
add("format_uuid", "550e8400-e29b-41d4-a716-446655440000");
add("format_uuid", "nope");
add("format_date", "2020-12-12");
add("format_date", "2020-13-12");
add("format_date_time", "2020-12-12T20:20:40Z");
add("format_date_time", "2020-12-12");
add("format_uri", "https://example.com/a");
add("format_uri", "not a uri");
add("format_ipv4", "127.0.0.1");
add("format_ipv4", "127.0.0");
add("type_array", "1");
add("type_array", 1);
add("type_array", true);
add("with_default", {});
add("with_default", { a: "z" });
add("union_obj", { nested: "x", extra: 1 });
add("union_obj", null);
add("union_obj", { nested: 1 });
add("string", "😀");
add("string_constraints", "😀");

// length probes (grapheme vs utf-16)
const lengthSchema = Type.String({ minLength: 2, maxLength: 2 });
cases.push({
	id: "length_emoji",
	value: "😀",
	schemaJson: JSON.stringify(lengthSchema),
	...run(lengthSchema, "😀"),
});
cases.push({
	id: "length_ascii",
	value: "ab",
	schemaJson: JSON.stringify(lengthSchema),
	...run(lengthSchema, "ab"),
});
const combining = "e\u0301";
cases.push({
	id: "length_combining",
	value: combining,
	schemaJson: JSON.stringify(lengthSchema),
	...run(lengthSchema, combining),
});

const built = {};
for (const [id, fn] of Object.entries(builders)) {
	built[id] = JSON.stringify(fn());
}

const out = { built, cases };
const here = dirname(fileURLToPath(import.meta.url));
const dest = join(here, "testdata", "vectors.json");
mkdirSync(join(here, "testdata"), { recursive: true });
writeFileSync(dest, JSON.stringify(out, null, 2));
console.log("wrote", dest, "cases", cases.length);
