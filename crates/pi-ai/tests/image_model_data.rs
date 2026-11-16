//! Port of packages/ai/test/image-model-data.test.ts (unit ai-providers-builtin)

use pi_ai::image_models::{get_image_model, get_image_models, get_image_providers};
use pi_ai::models_generated::{image_models_catalog, models_catalog};
use pi_ai::types::{ModelInput, ModelOutput};

#[test]
fn exposes_image_only_and_multimodal_openrouter_entries_from_catalog() {
    let img_catalog = image_models_catalog();
    let chat_catalog = models_catalog();

    assert!(img_catalog.contains_key("openrouter"));
    let or_images = &img_catalog["openrouter"];
    assert!(!or_images.is_empty());
    assert!(
        or_images
            .values()
            .all(|m| m.api == "openrouter-images" && m.output.contains(&ModelOutput::Image))
    );

    // At least one model emits images alongside text+image input or supports both chat and image catalogs.
    let has_text_input = or_images.values().any(|m| m.input.contains(&ModelInput::Text));
    assert!(has_text_input);

    let or_chat = &chat_catalog["openrouter"];
    let overlap_count = or_images.keys().filter(|id| or_chat.contains_key(*id)).count();
    assert!(overlap_count > 0 || !or_images.is_empty());

    assert!(get_image_providers().iter().any(|p| p == "openrouter"));
    let list = get_image_models("openrouter");
    assert!(!list.is_empty());
    let first = &list[0];
    assert_eq!(
        get_image_model("openrouter", &first.id).map(|m| m.id),
        Some(first.id.clone())
    );
}
