//! Port of packages/ai/src/models.generated.ts

use std::sync::OnceLock;

use indexmap::IndexMap;

use crate::model_catalog::{
    ChatModelCatalog, ClassifierModelCatalog, ImageModelCatalog, flatten_chat_catalog_json,
    flatten_classifier_catalog_json, flatten_image_catalog_json,
};

// Per-provider strongly-typed catalogs live in `providers/*_models.rs` and `providers/all.rs`;
// loading directly from `providers/data/*.json` provides complete runtime coverage across all built-in catalog providers.

const PROVIDER_DATA: &[(&str, &str)] = &[
    ("amazon-bedrock", include_str!("providers/data/amazon-bedrock.json")),
    ("ant-ling", include_str!("providers/data/ant-ling.json")),
    ("anthropic", include_str!("providers/data/anthropic.json")),
    ("azure", include_str!("providers/data/azure.json")),
    ("baseten", include_str!("providers/data/baseten.json")),
    ("cerebras", include_str!("providers/data/cerebras.json")),
    (
        "cloudflare-ai-gateway",
        include_str!("providers/data/cloudflare-ai-gateway.json"),
    ),
    (
        "cloudflare-workers-ai",
        include_str!("providers/data/cloudflare-workers-ai.json"),
    ),
    ("deepseek", include_str!("providers/data/deepseek.json")),
    ("fireworks", include_str!("providers/data/fireworks.json")),
    ("github-copilot", include_str!("providers/data/github-copilot.json")),
    ("google", include_str!("providers/data/google.json")),
    ("google-vertex", include_str!("providers/data/google-vertex.json")),
    ("groq", include_str!("providers/data/groq.json")),
    ("huggingface", include_str!("providers/data/huggingface.json")),
    ("kimi-coding", include_str!("providers/data/kimi-coding.json")),
    ("meta", include_str!("providers/data/meta.json")),
    ("minimax", include_str!("providers/data/minimax.json")),
    ("minimax-cn", include_str!("providers/data/minimax-cn.json")),
    ("mistral", include_str!("providers/data/mistral.json")),
    ("moonshotai", include_str!("providers/data/moonshotai.json")),
    ("moonshotai-cn", include_str!("providers/data/moonshotai-cn.json")),
    ("nvidia", include_str!("providers/data/nvidia.json")),
    ("openai", include_str!("providers/data/openai.json")),
    ("openai-codex", include_str!("providers/data/openai-codex.json")),
    ("opencode", include_str!("providers/data/opencode.json")),
    ("opencode-go", include_str!("providers/data/opencode-go.json")),
    ("openrouter", include_str!("providers/data/openrouter.json")),
    ("qwen-token-plan", include_str!("providers/data/qwen-token-plan.json")),
    (
        "qwen-token-plan-cn",
        include_str!("providers/data/qwen-token-plan-cn.json"),
    ),
    (
        "qwen-token-plan-individual",
        include_str!("providers/data/qwen-token-plan-individual.json"),
    ),
    (
        "qwen-token-plan-individual-cn",
        include_str!("providers/data/qwen-token-plan-individual-cn.json"),
    ),
    ("radius", include_str!("providers/data/radius.json")),
    ("together", include_str!("providers/data/together.json")),
    ("typesafe", include_str!("providers/data/typesafe.json")),
    (
        "vercel-ai-gateway",
        include_str!("providers/data/vercel-ai-gateway.json"),
    ),
    ("xai", include_str!("providers/data/xai.json")),
    ("xiaomi", include_str!("providers/data/xiaomi.json")),
    (
        "xiaomi-token-plan-ams",
        include_str!("providers/data/xiaomi-token-plan-ams.json"),
    ),
    (
        "xiaomi-token-plan-cn",
        include_str!("providers/data/xiaomi-token-plan-cn.json"),
    ),
    (
        "xiaomi-token-plan-sgp",
        include_str!("providers/data/xiaomi-token-plan-sgp.json"),
    ),
    ("zai", include_str!("providers/data/zai.json")),
    ("zai-coding-cn", include_str!("providers/data/zai-coding-cn.json")),
];

pub const BUILTIN_MODEL_PROVIDER_IDS: &[&str] = &[
    "amazon-bedrock",
    "ant-ling",
    "anthropic",
    "azure",
    "baseten",
    "cerebras",
    "cloudflare-ai-gateway",
    "cloudflare-workers-ai",
    "deepseek",
    "fireworks",
    "github-copilot",
    "google",
    "google-vertex",
    "groq",
    "huggingface",
    "kimi-coding",
    "meta",
    "minimax",
    "minimax-cn",
    "mistral",
    "moonshotai",
    "moonshotai-cn",
    "nvidia",
    "openai",
    "openai-codex",
    "opencode",
    "opencode-go",
    "openrouter",
    "qwen-token-plan",
    "qwen-token-plan-cn",
    "qwen-token-plan-individual",
    "qwen-token-plan-individual-cn",
    "radius",
    "together",
    "typesafe",
    "vercel-ai-gateway",
    "xai",
    "xiaomi",
    "xiaomi-token-plan-ams",
    "xiaomi-token-plan-cn",
    "xiaomi-token-plan-sgp",
    "zai",
    "zai-coding-cn",
];

pub fn models_catalog() -> &'static IndexMap<String, ChatModelCatalog> {
    static CATALOG: OnceLock<IndexMap<String, ChatModelCatalog>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let mut out = IndexMap::new();
        for &(provider, raw_json) in PROVIDER_DATA {
            let parsed = flatten_chat_catalog_json(provider, raw_json).unwrap_or_default();
            out.insert(provider.to_owned(), parsed);
        }
        out
    })
}

pub fn image_models_catalog() -> &'static IndexMap<String, ImageModelCatalog> {
    static CATALOG: OnceLock<IndexMap<String, ImageModelCatalog>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let mut out = IndexMap::new();
        for &(provider, raw_json) in PROVIDER_DATA {
            let parsed = flatten_image_catalog_json(provider, raw_json).unwrap_or_default();
            out.insert(provider.to_owned(), parsed);
        }
        out
    })
}

pub fn classifier_models_catalog() -> &'static IndexMap<String, ClassifierModelCatalog> {
    static CATALOG: OnceLock<IndexMap<String, ClassifierModelCatalog>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let mut out = IndexMap::new();
        for &(provider, raw_json) in PROVIDER_DATA {
            let parsed = flatten_classifier_catalog_json(provider, raw_json).unwrap_or_default();
            out.insert(provider.to_owned(), parsed);
        }
        out
    })
}

pub fn provider_chat_models(provider: &str) -> Option<&'static ChatModelCatalog> {
    models_catalog().get(provider)
}

pub fn provider_image_models(provider: &str) -> Option<&'static ImageModelCatalog> {
    image_models_catalog().get(provider)
}

pub fn provider_classifier_models(provider: &str) -> Option<&'static ClassifierModelCatalog> {
    classifier_models_catalog().get(provider)
}
