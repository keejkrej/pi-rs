//! Port of packages/coding-agent/src/core/pi-manifest.ts

use serde_json::Value;

use crate::utils::text::strip_bom;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PiManifest {
    pub extensions: Option<Vec<String>>,
    pub skills: Option<Vec<String>>,
    pub prompts: Option<Vec<String>>,
    pub themes: Option<Vec<String>>,
    pub changelog_path: Option<String>,
}

fn read_string_array(obj: &serde_json::Map<String, Value>, key: &str) -> Option<Vec<String>> {
    let entries = obj.get(key)?.as_array()?;
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        out.push(entry.as_str()?.to_owned());
    }
    Some(out)
}

pub fn read_pi_manifest(package_json_path: &str) -> Option<PiManifest> {
    let content = pi_js::fs::read_to_string(package_json_path).ok()?;
    let parsed = pi_js::json::parse(strip_bom(&content)).ok()?;
    let pkg = parsed.as_object()?;
    let pi_obj = pkg.get("pi")?.as_object()?;
    let changelog_path = pi_obj
        .get("changelogPath")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);

    Some(PiManifest {
        extensions: read_string_array(pi_obj, "extensions"),
        skills: read_string_array(pi_obj, "skills"),
        prompts: read_string_array(pi_obj, "prompts"),
        themes: read_string_array(pi_obj, "themes"),
        changelog_path,
    })
}
