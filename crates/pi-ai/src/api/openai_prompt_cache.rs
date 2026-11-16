//! Port of packages/ai/src/api/openai-prompt-cache.ts

pub const OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH: usize = 64;

pub fn clamp_openai_prompt_cache_key(key: Option<&str>) -> Option<String> {
    let key = key?;
    let mut chars = key.chars();
    let count = chars.clone().count();
    if count <= OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH {
        Some(key.to_string())
    } else {
        Some(chars.by_ref().take(OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH).collect())
    }
}
