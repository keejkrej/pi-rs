//! Official JSON-Schema-Test-Suite conformance runner for Draft 2020-12 and Draft 7
//! (including all optional and format test suites).

use std::fs;
use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use pi_schema::{CompileOptions, Draft, compile, formats};
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
struct SuiteGroup {
    description: String,
    schema: Value,
    tests: Vec<SuiteCase>,
}

#[derive(Deserialize)]
struct SuiteCase {
    description: String,
    data: Value,
    valid: bool,
}

const KNOWN_FAILURES: &[&str] = &[
    // Vocabularies switched off by a custom meta-schema are not supported.
    "draft2020-12/vocabulary > schema that uses custom metaschema with with no validation vocabulary > no validation: invalid number, but it still validates",
    "draft2020-12/optional/format-assertion > schema that uses custom metaschema with format-assertion: false > format-assertion: false: invalid string",
    "draft2020-12/optional/format-assertion > schema that uses custom metaschema with format-assertion: true > format-assertion: true: invalid string",
    // A $ref to a document of another draft is evaluated with the compile's draft.
    "draft2020-12/optional/cross-draft > refs to historic drafts are processed as historic drafts > first item not a string is valid",
    "draft7/optional/cross-draft > refs to future drafts are processed as future drafts > missing bar is invalid",
    // contentMediaType and contentEncoding are annotations.
    "draft7/optional/content > validation of string-encoded content based on media type > an invalid JSON document",
    "draft7/optional/content > validation of binary string-encoding > an invalid base64 string (% is not a valid character)",
    "draft7/optional/content > validation of binary-encoded media type documents > a validly-encoded invalid JSON document",
    "draft7/optional/content > validation of binary-encoded media type documents > an invalid base64 string that is valid JSON",
];

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures")
}

fn json_files_sorted(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut entries: Vec<_> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", dir.display()))
        .map(|res| res.expect("dir entry").path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            out.extend(json_files_sorted(&path));
        } else if path.extension().and_then(|e| e.to_str()) == Some("json") {
            out.push(path);
        }
    }
    out
}

fn read_json(path: &Path) -> Value {
    let text = fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

fn load_suite_schemas() -> IndexMap<String, Value> {
    let fixtures = fixtures_dir();
    let mut schemas = IndexMap::new();
    let remotes = fixtures.join("json-schema-test-suite").join("remotes");
    for path in json_files_sorted(&remotes) {
        let rel = path
            .strip_prefix(&remotes)
            .expect("prefix")
            .to_string_lossy()
            .replace('\\', "/");
        schemas.insert(format!("http://localhost:1234/{rel}"), read_json(&path));
    }
    let meta = fixtures.join("meta-schemas");
    schemas.insert(
        "http://json-schema.org/draft-07/schema".to_string(),
        read_json(&meta.join("draft-07-schema.json")),
    );
    schemas.insert(
        "https://json-schema.org/draft/2020-12/schema".to_string(),
        read_json(&meta.join("draft2020-12-schema.json")),
    );
    for vocab in [
        "core",
        "applicator",
        "unevaluated",
        "validation",
        "meta-data",
        "format-annotation",
        "content",
    ] {
        schemas.insert(
            format!("https://json-schema.org/draft/2020-12/meta/{vocab}"),
            read_json(&meta.join(format!("draft2020-12-meta-{vocab}.json"))),
        );
    }
    schemas
}

#[test]
fn json_schema_test_suite_conformance() {
    let fixtures = fixtures_dir();
    let tests_root = fixtures.join("json-schema-test-suite").join("tests");
    let schemas = load_suite_schemas();
    let std_formats = formats::formats();

    let mut passed = 0usize;
    let mut expected_failures = 0usize;
    let mut unexpected = Vec::new();

    for (dir_name, draft) in [("draft2020-12", Draft::Draft2020_12), ("draft7", Draft::Draft7)] {
        let dir = tests_root.join(dir_name);
        for path in json_files_sorted(&dir) {
            let rel = path
                .strip_prefix(&tests_root)
                .expect("prefix")
                .to_string_lossy()
                .replace('\\', "/");
            let file_name = rel.strip_suffix(".json").unwrap_or(&rel);
            let is_format_suite = file_name.contains("/optional/format/");

            let raw = fs::read_to_string(&path).expect("read test file");
            let groups: Vec<SuiteGroup> =
                serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parse test file {file_name}: {e}"));

            let options = CompileOptions {
                draft: Some(draft),
                formats: if is_format_suite {
                    Some(std_formats.clone())
                } else {
                    None
                },
                schemas: schemas.clone(),
                coerce: false,
                unresolved_refs: pi_schema::UnresolvedRefs::Throw,
            };

            for group in groups {
                let validator = match compile(&group.schema, options.clone()) {
                    Ok(v) => v,
                    Err(err) => {
                        for test in &group.tests {
                            let id = format!("{file_name} > {} > {}", group.description, test.description);
                            unexpected.push(format!("COMPILE ERROR [{id}]: {err}"));
                        }
                        continue;
                    }
                };
                for test in group.tests {
                    let id = format!("{file_name} > {} > {}", group.description, test.description);
                    let known = KNOWN_FAILURES.contains(&id.as_str());
                    let check_ok = validator.check(&test.data);
                    let errors = validator.errors(&test.data);

                    if errors.is_empty() != check_ok {
                        unexpected.push(format!(
                            "ERRORS DISAGREE WITH CHECK [{id}]: check={check_ok}, errors={errors:?}"
                        ));
                    }

                    if known {
                        if check_ok == test.valid {
                            unexpected.push(format!("KNOWN FAILURE NOW PASSES [{id}]"));
                        } else {
                            expected_failures += 1;
                        }
                    } else if check_ok == test.valid {
                        passed += 1;
                    } else {
                        unexpected.push(format!(
                            "MISMATCH [{id}]: expected valid={}, got check={check_ok}, data={}",
                            test.valid, test.data
                        ));
                    }
                }
            }
        }
    }

    assert!(
        unexpected.is_empty(),
        "JSON Schema Test Suite had {} failures (passed={passed}, expected_failures={expected_failures}):\n{}",
        unexpected.len(),
        unexpected.join("\n")
    );
    assert_eq!(expected_failures, KNOWN_FAILURES.len());
    println!("JSON-Schema-Test-Suite: passed={passed}, expected_failures={expected_failures}");
    assert!(passed > 2000, "expected >2000 passing suite tests, got {passed}");
}
