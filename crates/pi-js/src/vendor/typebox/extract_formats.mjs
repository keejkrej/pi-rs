import { readFileSync, writeFileSync } from "node:fs";

const dir = "C:/Users/ctyja/workspace/pi/node_modules/typebox/build/format/";
const files = [
	"uri",
	"email",
	"uuid",
	"ipv4",
	"ipv6",
	"duration",
	"json_pointer",
	"relative_json_pointer",
	"json_pointer_uri_fragment",
	"uri_template",
	"uri_reference",
	"time",
];

function extract(src) {
	const line = src.split(/\r?\n/).find((l) => l.includes("= /"));
	if (!line) return null;
	const expr = line.replace(/^const\s+\w+\s*=\s*/, "").replace(/;\s*$/, "");
	const re = eval(expr);
	return { pat: re.source, flags: re.flags };
}

let out = "// Format patterns copied from TypeBox 1.3.27 format/*.mjs.\n";
for (const f of files) {
	const src = readFileSync(dir + f + ".mjs", "utf8");
	const e = extract(src);
	if (!e) throw new Error("no pattern " + f);
	const name = f.toUpperCase();
	out += `pub(crate) const ${name}: &str = r###"${e.pat}"###;\n`;
	out += `pub(crate) const ${name}_FLAGS: &str = "${e.flags}";\n`;
	if (e.pat.includes('###')) throw new Error('raw delimiter in ' + f);
	console.error(f, e.pat.length, JSON.stringify(e.flags));
}
writeFileSync(new URL("./format_patterns.rs", import.meta.url), out);
