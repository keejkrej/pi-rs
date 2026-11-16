//! Port of packages/ai/test/google-thinking-signature.test.ts

use pi_ai::api::google_shared::{GooglePart, is_thinking_part, retain_thought_signature};
use pretty_assertions::assert_eq;

#[test]
fn treats_part_thought_true_as_thinking() {
    assert!(is_thinking_part(&GooglePart {
        thought: Some(true),
        thought_signature: None,
        ..Default::default()
    }));
    assert!(is_thinking_part(&GooglePart {
        thought: Some(true),
        thought_signature: Some("opaque-signature".to_owned()),
        ..Default::default()
    }));
}

#[test]
fn does_not_treat_thought_signature_alone_as_thinking() {
    assert!(!is_thinking_part(&GooglePart {
        thought: None,
        thought_signature: Some("opaque-signature".to_owned()),
        ..Default::default()
    }));
    assert!(!is_thinking_part(&GooglePart {
        thought: Some(false),
        thought_signature: Some("opaque-signature".to_owned()),
        ..Default::default()
    }));
}

#[test]
fn does_not_treat_empty_or_missing_signatures_as_thinking_when_thought_unset() {
    assert!(!is_thinking_part(&GooglePart {
        thought: None,
        thought_signature: None,
        ..Default::default()
    }));
    assert!(!is_thinking_part(&GooglePart {
        thought: Some(false),
        thought_signature: Some(String::new()),
        ..Default::default()
    }));
}

#[test]
fn preserves_existing_signature_when_subsequent_deltas_omit_thought_signature() {
    let first = retain_thought_signature(None, Some("sig-1"));
    assert_eq!(first.as_deref(), Some("sig-1"));

    let second = retain_thought_signature(first.as_deref(), None);
    assert_eq!(second.as_deref(), Some("sig-1"));

    let third = retain_thought_signature(second.as_deref(), Some(""));
    assert_eq!(third.as_deref(), Some("sig-1"));
}

#[test]
fn updates_signature_when_new_non_empty_signature_arrives() {
    let updated = retain_thought_signature(Some("sig-1"), Some("sig-2"));
    assert_eq!(updated.as_deref(), Some("sig-2"));
}
