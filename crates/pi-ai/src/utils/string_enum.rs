//! Port of packages/ai/src/utils/string-enum.ts

pub use pi_schema::{Schema, StringEnumOptions};

/// Creates a string enum schema compatible with Google's API and other providers
/// that don't support anyOf/const patterns: `{ "type": "string", "enum": [...] }`.
pub fn string_enum<I, S>(values: I, options: Option<StringEnumOptions>) -> Schema
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    pi_schema::string_enum(values, options)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn builds_string_enum_with_and_without_options() {
        let plain = string_enum(["add", "subtract"], None);
        assert_eq!(
            plain.to_value(),
            json!({ "type": "string", "enum": ["add", "subtract"] })
        );

        let rich = string_enum(
            ["a", "b"],
            Some(StringEnumOptions {
                description: Some("Pick one".to_owned()),
                default_value: Some("a".to_owned()),
            }),
        );
        assert_eq!(
            rich.to_value(),
            json!({
                "type": "string",
                "enum": ["a", "b"],
                "description": "Pick one",
                "default": "a"
            })
        );
    }
}
