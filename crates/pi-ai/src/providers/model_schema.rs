//! Port of packages/ai/src/providers/model-schema.ts

use pi_schema::{
    Annotations, ArrOptions, NumOptions, Schema, arr_with, int_with, nul, num_with, one_of, opt, str, union,
};

use crate::providers::schema_builders::open_object;
pub use crate::types::{
    CacheRetention, ModelCost, ModelCostRates, ModelCostTier, ModelImageInputLimits, ModelImageResizeOptions,
    ModelInput as ModelInputModality, ModelInputLimits, ModelPromptCache, ModelThinkingLevel, ThinkingLevel,
    ThinkingLevelMap,
};

pub const THINKING_LEVELS: [&str; 6] = ["minimal", "low", "medium", "high", "xhigh", "max"];
pub const MODEL_THINKING_LEVELS: [&str; 7] = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];
pub const MODEL_INPUT_MODALITIES: [&str; 2] = ["text", "image"];
pub const CACHE_RETENTIONS: [&str; 3] = ["none", "short", "long"];
pub const MODEL_PROMPT_CACHE_RETENTIONS: [&str; 2] = ["short", "long"];

pub fn thinking_level_schema() -> Schema {
    one_of(THINKING_LEVELS)
}

pub fn model_thinking_level_schema() -> Schema {
    one_of(MODEL_THINKING_LEVELS)
}

pub fn model_input_modality_schema() -> Schema {
    one_of(MODEL_INPUT_MODALITIES)
}

pub fn cache_retention_schema() -> Schema {
    one_of(CACHE_RETENTIONS)
}

pub fn thinking_level_map_schema() -> Schema {
    let value_schema = || union([str(), nul()]);
    open_object(
        [
            ("off", opt(value_schema())),
            ("minimal", opt(value_schema())),
            ("low", opt(value_schema())),
            ("medium", opt(value_schema())),
            ("high", opt(value_schema())),
            ("xhigh", opt(value_schema())),
            ("max", opt(value_schema())),
        ],
        Annotations {
            description: Some(
                "Provider value sent for each thinking level. null marks a level unsupported. Levels the model does not map use the provider's default value through high; xhigh and max are unsupported.".to_owned(),
            ),
            ..Default::default()
        },
        false,
    )
}

fn prompt_cache_lifetime_schema() -> Schema {
    num_with(NumOptions {
        exclusive_minimum: Some(0.0),
        ..Default::default()
    })
}

pub fn model_prompt_cache_schema() -> Schema {
    open_object(
        [
            ("short", opt(prompt_cache_lifetime_schema())),
            ("long", opt(prompt_cache_lifetime_schema())),
        ],
        Annotations {
            description: Some(
                "Best-effort prompt cache lifetime in seconds for the short and long retention tiers. Pi warms only tiers with a known lifetime.".to_owned(),
            ),
            ..Default::default()
        },
        false,
    )
}

fn model_cost_rates_properties() -> [(&'static str, Schema); 4] {
    [
        (
            "input",
            num_with(NumOptions {
                annotations: Annotations {
                    description: Some("Input cost in USD per million tokens.".to_owned()),
                    ..Default::default()
                },
                ..Default::default()
            }),
        ),
        (
            "output",
            num_with(NumOptions {
                annotations: Annotations {
                    description: Some("Output cost in USD per million tokens.".to_owned()),
                    ..Default::default()
                },
                ..Default::default()
            }),
        ),
        (
            "cacheRead",
            num_with(NumOptions {
                annotations: Annotations {
                    description: Some("Cache-read cost in USD per million tokens.".to_owned()),
                    ..Default::default()
                },
                ..Default::default()
            }),
        ),
        (
            "cacheWrite",
            num_with(NumOptions {
                annotations: Annotations {
                    description: Some("Cache-write cost in USD per million tokens.".to_owned()),
                    ..Default::default()
                },
                ..Default::default()
            }),
        ),
    ]
}

pub fn model_cost_rates_schema() -> Schema {
    open_object(model_cost_rates_properties(), Annotations::default(), false)
}

pub fn model_cost_tier_schema() -> Schema {
    let mut props: Vec<(&'static str, Schema)> = Vec::with_capacity(5);
    props.push((
        "inputTokensAbove",
        num_with(NumOptions {
            annotations: Annotations {
                description: Some("Use this tier when total request input exceeds this token count.".to_owned()),
                ..Default::default()
            },
            ..Default::default()
        }),
    ));
    props.extend(model_cost_rates_properties());
    open_object(props, Annotations::default(), false)
}

pub fn model_cost_schema() -> Schema {
    let mut props: Vec<(&'static str, Schema)> = Vec::with_capacity(5);
    props.extend(model_cost_rates_properties());
    props.push((
        "tiers",
        opt(arr_with(
            model_cost_tier_schema(),
            ArrOptions {
                annotations: Annotations {
                    description: Some(
                        "Request-wide pricing tiers. The highest matching input threshold applies to the full request."
                            .to_owned(),
                    ),
                    ..Default::default()
                },
                ..Default::default()
            },
        )),
    ));
    open_object(props, Annotations::default(), false)
}

fn model_image_resize_options_schema_with(annotations: Annotations) -> Schema {
    open_object(
        [
            (
                "maxWidth",
                opt(int_with(NumOptions {
                    minimum: Some(1.0),
                    ..Default::default()
                })),
            ),
            (
                "maxHeight",
                opt(int_with(NumOptions {
                    minimum: Some(1.0),
                    ..Default::default()
                })),
            ),
            (
                "maxBytes",
                opt(int_with(NumOptions {
                    annotations: Annotations {
                        description: Some("Maximum base64-encoded payload size in bytes.".to_owned()),
                        ..Default::default()
                    },
                    minimum: Some(1.0),
                    ..Default::default()
                })),
            ),
            (
                "jpegQuality",
                opt(int_with(NumOptions {
                    minimum: Some(1.0),
                    maximum: Some(100.0),
                    ..Default::default()
                })),
            ),
        ],
        annotations,
        false,
    )
}

pub fn model_image_resize_options_schema() -> Schema {
    model_image_resize_options_schema_with(Annotations::default())
}

pub fn model_image_input_limits_schema() -> Schema {
    open_object(
        [
            (
                "resize",
                opt(model_image_resize_options_schema_with(Annotations {
                    description: Some(
                        "Cache-safe resize profile applied before a new image enters conversation history.".to_owned(),
                    ),
                    ..Default::default()
                })),
            ),
            (
                "maxPerMessage",
                opt(int_with(NumOptions {
                    annotations: Annotations {
                        description: Some("Maximum images accepted in one provider message.".to_owned()),
                        ..Default::default()
                    },
                    minimum: Some(1.0),
                    ..Default::default()
                })),
            ),
            (
                "maxPerRequest",
                opt(int_with(NumOptions {
                    annotations: Annotations {
                        description: Some("Maximum images accepted across one provider request.".to_owned()),
                        ..Default::default()
                    },
                    minimum: Some(1.0),
                    ..Default::default()
                })),
            ),
        ],
        Annotations::default(),
        false,
    )
}

pub fn model_input_limits_schema() -> Schema {
    open_object(
        [
            (
                "maxRequestBytes",
                opt(int_with(NumOptions {
                    annotations: Annotations {
                        description: Some("Maximum serialized provider request size in bytes.".to_owned()),
                        ..Default::default()
                    },
                    minimum: Some(1.0),
                    ..Default::default()
                })),
            ),
            ("images", opt(model_image_input_limits_schema())),
        ],
        Annotations::default(),
        false,
    )
}
