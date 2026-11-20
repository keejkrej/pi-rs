//! Port of packages/coding-agent/src/core/keybindings-schema.ts

use pi_schema::{Annotations, Schema, obj_with, opt, str};

pub use crate::core::key_id_schema::{keybinding_value_schema, keybinding_value_schema_with};
use crate::core::keybindings::KEYBINDINGS;

pub fn keybindings_schema() -> Schema {
    let mut props: Vec<(String, Schema)> = Vec::with_capacity(KEYBINDINGS.len() + 1);
    props.push(("$schema".to_owned(), opt(str())));
    for (id, definition) in KEYBINDINGS.iter() {
        props.push((
            id.clone(),
            opt(keybinding_value_schema_with(definition.description.as_deref())),
        ));
    }
    let additional = keybinding_value_schema().into_value();
    obj_with(
        props,
        pi_schema::ObjOptions {
            annotations: Annotations::default(),
            open: true,
        },
    )
    .with("additionalProperties", additional)
}
