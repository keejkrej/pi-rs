//! Port of packages/ai/src/providers/compat-schema.ts

use indexmap::IndexMap;
use pi_schema::{
    Annotations, ArrOptions, Schema, StrOptions, arr_with, bool, bool_with, nul, num, num_with, opt, str, str_with,
    union, union_with,
};
use serde_json::Value;

use crate::providers::model_schema::model_cost_schema;
use crate::providers::schema_builders::{open_object, string_literal, string_record};
pub use crate::types::{
    AnthropicAllowedFallbackModel, AnthropicMessagesCompat, BedrockCompat, ChatTemplateKwargValue,
    MistralConversationsCompat, OpenAICompletionsCompat, OpenAIResponsesCompat, OpenRouterRouting,
    SessionAffinityFormat, SystemOneCompat, ThinkingTokenBudgetField, VercelGatewayRouting,
};

fn desc(description: &str) -> Annotations {
    Annotations {
        description: Some(description.to_owned()),
        ..Default::default()
    }
}

fn bool_desc(description: &str) -> Schema {
    bool_with(desc(description))
}

fn bool_desc_default(description: &str, default: bool) -> Schema {
    bool_with(Annotations {
        description: Some(description.to_owned()),
        default_value: Some(Value::Bool(default)),
        ..Default::default()
    })
}

fn session_affinity_format(annotations: Annotations) -> Schema {
    union_with(
        [
            string_literal("openai", Annotations::default()),
            string_literal("openai-nosession", Annotations::default()),
            string_literal("openrouter", Annotations::default()),
        ],
        annotations,
    )
}

pub fn session_affinity_format_schema() -> Schema {
    session_affinity_format(desc(
        "Session-affinity header format used to route related requests consistently.",
    ))
}

fn thinking_token_budget_field(annotations: Annotations) -> Schema {
    union_with(
        [
            string_literal("thinking_token_budget", Annotations::default()),
            string_literal("thinking_budget", Annotations::default()),
            string_literal("thinking_budget_tokens", Annotations::default()),
        ],
        annotations,
    )
}

pub fn thinking_token_budget_field_schema() -> Schema {
    thinking_token_budget_field(desc(
        "Top-level request field name used by OpenAI-compatible endpoints to cap reasoning tokens. \"thinking_token_budget\" is used by vLLM, \"thinking_budget\" by Qwen, DashScope, or SGLang, and \"thinking_budget_tokens\" by llama.cpp.",
    ))
}

pub fn chat_template_kwarg_value_schema() -> Schema {
    union([
        str(),
        num(),
        bool(),
        nul(),
        open_object(
            [
                (
                    "$var",
                    union([
                        string_literal("thinking.enabled", Annotations::default()),
                        string_literal("thinking.effort", Annotations::default()),
                        string_literal("thinking.budget", Annotations::default()),
                    ]),
                ),
                ("omitWhenOff", opt(bool())),
            ],
            Annotations::default(),
            false,
        ),
    ])
}

fn percentile_cutoffs(metric: &str) -> Schema {
    open_object(
        [
            (
                "p50",
                opt(num_with(NumDesc::new(format!("{metric} at the 50th percentile.")))),
            ),
            (
                "p75",
                opt(num_with(NumDesc::new(format!("{metric} at the 75th percentile.")))),
            ),
            (
                "p90",
                opt(num_with(NumDesc::new(format!("{metric} at the 90th percentile.")))),
            ),
            (
                "p99",
                opt(num_with(NumDesc::new(format!("{metric} at the 99th percentile.")))),
            ),
        ],
        Annotations::default(),
        false,
    )
}

struct NumDesc;
impl NumDesc {
    fn new(description: String) -> pi_schema::NumOptions {
        pi_schema::NumOptions {
            annotations: Annotations {
                description: Some(description),
                ..Default::default()
            },
            ..Default::default()
        }
    }
}

pub fn open_router_routing_schema() -> Schema {
    open_object(
        [
            (
                "allow_fallbacks",
                opt(bool_desc_default(
                    "Whether to allow backup providers to serve requests.",
                    true,
                )),
            ),
            (
                "require_parameters",
                opt(bool_desc_default(
                    "Whether to filter providers to only those that support all parameters in the request.",
                    false,
                )),
            ),
            (
                "data_collection",
                opt(union_with(
                    [
                        string_literal("deny", Annotations::default()),
                        string_literal("allow", Annotations::default()),
                    ],
                    Annotations {
                        description: Some(
                            "Data collection setting. \"allow\": allow providers that may store or train on data. \"deny\": only use providers that do not collect user data."
                                .to_owned(),
                        ),
                        default_value: Some(Value::String("allow".to_owned())),
                        ..Default::default()
                    },
                )),
            ),
            (
                "zdr",
                opt(bool_desc(
                    "Whether to restrict routing to only ZDR (Zero Data Retention) endpoints.",
                )),
            ),
            (
                "enforce_distillable_text",
                opt(bool_desc(
                    "Whether to restrict routing to only models that allow text distillation.",
                )),
            ),
            (
                "order",
                opt(arr_with(
                    str(),
                    ArrOptions {
                        annotations: desc(
                            "An ordered list of provider names or slugs to try in sequence, falling back to the next if unavailable.",
                        ),
                        ..Default::default()
                    },
                )),
            ),
            (
                "only",
                opt(arr_with(
                    str(),
                    ArrOptions {
                        annotations: desc("List of provider names or slugs to exclusively allow for this request."),
                        ..Default::default()
                    },
                )),
            ),
            (
                "ignore",
                opt(arr_with(
                    str(),
                    ArrOptions {
                        annotations: desc("List of provider names or slugs to skip for this request."),
                        ..Default::default()
                    },
                )),
            ),
            (
                "quantizations",
                opt(arr_with(
                    str(),
                    ArrOptions {
                        annotations: desc(
                            "A list of quantization levels to filter providers by, for example [\"fp16\", \"bf16\", \"fp8\", \"fp6\", \"int8\", \"int4\", \"fp4\", \"fp32\"].",
                        ),
                        ..Default::default()
                    },
                )),
            ),
            (
                "sort",
                opt(union_with(
                    [
                        str(),
                        open_object(
                            [
                                (
                                    "by",
                                    opt(str_with(StrOptions {
                                        annotations: desc(
                                            "The sorting metric, such as \"price\", \"throughput\", or \"latency\".",
                                        ),
                                        ..Default::default()
                                    })),
                                ),
                                (
                                    "partition",
                                    opt(union_with(
                                        [str(), nul()],
                                        Annotations {
                                            description: Some("Partitioning strategy: \"model\" or \"none\".".to_owned()),
                                            default_value: Some(Value::String("model".to_owned())),
                                            ..Default::default()
                                        },
                                    )),
                                ),
                            ],
                            Annotations::default(),
                            false,
                        ),
                    ],
                    desc(
                        "Sorting strategy. Can be a string such as \"price\", \"throughput\", or \"latency\", or an object.",
                    ),
                )),
            ),
            (
                "max_price",
                opt(open_object(
                    [
                        (
                            "prompt",
                            opt(union_with(
                                [num(), str()],
                                desc("Price per million prompt tokens."),
                            )),
                        ),
                        (
                            "completion",
                            opt(union_with(
                                [num(), str()],
                                desc("Price per million completion tokens."),
                            )),
                        ),
                        ("image", opt(union_with([num(), str()], desc("Price per image.")))),
                        (
                            "audio",
                            opt(union_with([num(), str()], desc("Price per audio unit."))),
                        ),
                        (
                            "request",
                            opt(union_with([num(), str()], desc("Price per request."))),
                        ),
                    ],
                    desc("Maximum price per million tokens in USD."),
                    false,
                )),
            ),
            (
                "preferred_min_throughput",
                opt(union_with(
                    [num(), percentile_cutoffs("Minimum tokens per second")],
                    desc("Preferred minimum throughput in tokens per second. A number applies to p50."),
                )),
            ),
            (
                "preferred_max_latency",
                opt(union_with(
                    [num(), percentile_cutoffs("Maximum latency in seconds")],
                    desc("Preferred maximum latency in seconds. A number applies to p50."),
                )),
            ),
        ],
        desc(
            "OpenRouter provider routing preferences. Controls which upstream providers OpenRouter routes requests to. Sent as the provider field in the OpenRouter API request body. See https://openrouter.ai/docs/guides/routing/provider-selection.",
        ),
        false,
    )
}

pub fn vercel_gateway_routing_schema() -> Schema {
    open_object(
        [
            (
                "only",
                opt(arr_with(
                    str(),
                    ArrOptions {
                        annotations: desc(
                            "List of provider slugs to exclusively use for this request, for example [\"bedrock\", \"anthropic\"].",
                        ),
                        ..Default::default()
                    },
                )),
            ),
            (
                "order",
                opt(arr_with(
                    str(),
                    ArrOptions {
                        annotations: desc(
                            "List of provider slugs to try in order, for example [\"anthropic\", \"openai\"].",
                        ),
                        ..Default::default()
                    },
                )),
            ),
        ],
        desc(
            "Vercel AI Gateway routing preferences. Controls which upstream providers the gateway routes requests to. See https://vercel.com/docs/ai-gateway/models-and-providers/provider-options.",
        ),
        false,
    )
}

pub fn anthropic_allowed_fallback_model_schema() -> Schema {
    open_object(
        [
            (
                "provider",
                str_with(StrOptions {
                    min_length: Some(1),
                    ..Default::default()
                }),
            ),
            (
                "model",
                str_with(StrOptions {
                    min_length: Some(1),
                    ..Default::default()
                }),
            ),
            ("cost", model_cost_schema()),
        ],
        desc("An Anthropic server-side refusal fallback model with local pricing metadata."),
        false,
    )
}

fn optional_prompt_cache_key_mode(annotations: Annotations) -> Schema {
    opt(union_with(
        [
            string_literal("auto", Annotations::default()),
            string_literal("enabled", Annotations::default()),
            string_literal("disabled", Annotations::default()),
        ],
        annotations,
    ))
}

fn open_ai_completions_compat_properties() -> Vec<(&'static str, Schema)> {
    vec![
        (
            "supportsStore",
            opt(bool_desc(
                "Whether the provider supports the store field. Default: auto-detected from URL.",
            )),
        ),
        (
            "supportsDeveloperRole",
            opt(bool_desc(
                "Whether the provider supports the developer role instead of system. Default: auto-detected from URL.",
            )),
        ),
        (
            "supportsReasoningEffort",
            opt(bool_desc(
                "Whether the provider supports reasoning_effort. Default: auto-detected from URL.",
            )),
        ),
        (
            "supportsUsageInStreaming",
            opt(bool_desc_default(
                "Whether the provider supports stream_options.include_usage for token usage in streaming responses.",
                true,
            )),
        ),
        (
            "supportsFinishReason",
            opt(bool_desc_default(
                "Whether streamed responses include finish_reason. When false, pi infers stop or toolUse when the stream ends.",
                true,
            )),
        ),
        (
            "maxTokensField",
            opt(union_with(
                [
                    string_literal("max_completion_tokens", Annotations::default()),
                    string_literal("max_tokens", Annotations::default()),
                ],
                desc("Which field to use for max tokens. Default: auto-detected from URL."),
            )),
        ),
        (
            "requiresToolResultName",
            opt(bool_desc(
                "Whether tool results require the name field. Default: auto-detected from URL.",
            )),
        ),
        (
            "requiresAssistantAfterToolResult",
            opt(bool_desc(
                "Whether a user message after tool results requires an assistant message in between. Default: auto-detected from URL.",
            )),
        ),
        (
            "requiresNonNullAssistantContent",
            opt(bool_desc(
                "Whether replayed assistant messages with no text (e.g. tool-call-only messages) must send \"\" instead of null content. Default: false.",
            )),
        ),
        (
            "requiresToolsWithToolHistory",
            opt(bool_desc(
                "Whether requests without tools must send \"tools\": [] when the conversation contains tool calls or tool results. Needed by some proxies in front of Anthropic, such as older LiteLLM versions. Default: false, the field is omitted.",
            )),
        ),
        (
            "requiresThinkingAsText",
            opt(bool_desc(
                "Whether thinking blocks must be converted to text blocks with <thinking> delimiters. Default: auto-detected from URL.",
            )),
        ),
        (
            "requiresReasoningContentOnAssistantMessages",
            opt(bool_desc(
                "Whether all replayed assistant messages must include an empty reasoning_content field when reasoning is enabled. Default: auto-detected from URL.",
            )),
        ),
        (
            "thinkingFormat",
            opt(union_with(
                [
                    string_literal("openai", Annotations::default()),
                    string_literal("openrouter", Annotations::default()),
                    string_literal("deepseek", Annotations::default()),
                    string_literal("together", Annotations::default()),
                    string_literal("baseten", Annotations::default()),
                    string_literal("zai", Annotations::default()),
                    string_literal("qwen", Annotations::default()),
                    string_literal("chat-template", Annotations::default()),
                    string_literal("qwen-chat-template", Annotations::default()),
                    string_literal("string-thinking", Annotations::default()),
                    string_literal("ant-ling", Annotations::default()),
                ],
                desc(
                    "Format for reasoning or thinking parameters. When omitted, Pi auto-detects the format from the provider URL. \"openai\" uses reasoning_effort, \"openrouter\" uses reasoning.effort, \"deepseek\" uses thinking.type plus reasoning_effort when supported, \"together\" uses reasoning.enabled plus reasoning_effort when supported, \"baseten\" uses configurable chat_template_args plus reasoning_effort when supported, \"zai\" uses thinking.type, \"qwen\" uses top-level enable_thinking, \"qwen-chat-template\" uses chat_template_kwargs.enable_thinking and preserve_thinking, \"chat-template\" uses configurable chat_template_kwargs, \"string-thinking\" uses top-level thinking, and \"ant-ling\" uses reasoning.effort only when the mapped effort is non-null.",
                ),
            )),
        ),
        (
            "chatTemplateKwargs",
            opt(string_record(
                chat_template_kwarg_value_schema(),
                desc(
                    "Kwargs sent as chat_template_kwargs when thinkingFormat is \"chat-template\". Use $var with \"thinking.enabled\", \"thinking.effort\", or \"thinking.budget\" for pi-controlled values.",
                ),
            )),
        ),
        (
            "chatTemplateArgs",
            opt(string_record(
                chat_template_kwarg_value_schema(),
                desc(
                    "Arguments sent as chat_template_args when thinkingFormat is \"baseten\". Use $var with \"thinking.enabled\", \"thinking.effort\", or \"thinking.budget\" for pi-controlled values.",
                ),
            )),
        ),
        ("openRouterRouting", opt(open_router_routing_schema())),
        ("vercelGatewayRouting", opt(vercel_gateway_routing_schema())),
        (
            "zaiToolStream",
            opt(bool_desc_default(
                "Whether z.ai supports top-level tool_stream for streaming tool call deltas.",
                false,
            )),
        ),
        (
            "thinkingTokenBudgetField",
            opt(thinking_token_budget_field(desc(
                "Top-level request field used to cap reasoning tokens from thinkingBudgets. Reasoning and the answer share max_tokens on these endpoints. \"thinking_token_budget\" is vLLM, \"thinking_budget\" is Qwen, DashScope, or SGLang, and \"thinking_budget_tokens\" is llama.cpp. Off by default and not set on the generated catalog.",
            ))),
        ),
        (
            "supportsThinkingTokenBudget",
            opt(bool_desc_default(
                "Alias for thinkingTokenBudgetField: \"thinking_token_budget\" (vLLM). Prefer thinkingTokenBudgetField.",
                false,
            )),
        ),
        (
            "supportsOpenAIGrammarTools",
            opt(bool_desc_default(
                "Whether the provider supports OpenAI custom tools with Lark or regex grammar formats. When false, grammar-constrained tools fall back to normal function tools. The generated catalog enables this for capable models.",
                false,
            )),
        ),
        (
            "supportsMidConvoSystemMessages",
            opt(bool_desc_default(
                "Whether the exact model accepts system or developer messages after the conversation has started. When false, later system messages are folded into the leading system message. The generated catalog enables this for verified models.",
                false,
            )),
        ),
        (
            "supportsMidConvoToolAdditions",
            opt(bool_desc_default(
                "Whether system messages can introduce additional tools mid-conversation. Requires supportsMidConvoSystemMessages. The generated catalog enables this for capable models.",
                false,
            )),
        ),
        (
            "supportsStrictMode",
            opt(bool_desc_default(
                "Whether the provider supports the strict field in tool definitions. Generated capable models enable it explicitly.",
                false,
            )),
        ),
        (
            "cacheControlFormat",
            opt(string_literal(
                "anthropic",
                desc(
                    "Cache control convention for prompt caching. Anthropic applies cache_control markers to the system prompt, last tool definition, and last user, assistant, or tool-result text content.",
                ),
            )),
        ),
        (
            "sendSessionAffinityHeaders",
            opt(bool_desc(
                "Whether to send session-affinity data from options.sessionId. Default: true for OpenRouter endpoints, false otherwise.",
            )),
        ),
        (
            "sessionAffinityFormat",
            opt(session_affinity_format(desc(
                "Session-affinity header format. openai sends session_id, x-client-request-id, and x-session-affinity; openai-nosession sends x-client-request-id and x-session-affinity; openrouter sends x-session-id. Does not affect prompt_cache_key. Default: auto-detected.",
            ))),
        ),
        (
            "promptCacheKeyMode",
            optional_prompt_cache_key_mode(desc(
                "Controls the prompt_cache_key request field. auto sends it to api.openai.com, or with long cache retention when supportsLongCacheRetention is true; enabled sends it whenever caching is active, for compatible proxies; disabled never sends it. Default: auto.",
            )),
        ),
        (
            "supportsLongCacheRetention",
            opt(bool_desc(
                "Whether the provider supports long prompt cache retention (prompt_cache_retention: \"24h\" or Anthropic-style cache_control.ttl: \"1h\", depending on format). Default: auto-detected from provider and URL.",
            )),
        ),
        (
            "vllmPriority",
            opt(num_with(NumDesc::new(
                "vLLM scheduler priority sent as the top-level priority request field. Lower values are handled earlier and the server default is 0. Only meaningful with --scheduling-policy priority. Off by default and not set on the generated catalog.".to_owned(),
            ))),
        ),
    ]
}

pub fn open_ai_completions_compat_schema() -> Schema {
    open_object(
        open_ai_completions_compat_properties(),
        desc(
            "Compatibility settings for OpenAI-compatible completions APIs. Use this to override URL-based auto-detection for custom providers.",
        ),
        true,
    )
}

fn open_ai_responses_compat_properties() -> Vec<(&'static str, Schema)> {
    vec![
        (
            "supportsDeveloperRole",
            opt(bool_desc_default(
                "Whether the provider supports the developer role instead of system.",
                true,
            )),
        ),
        (
            "supportsMidConvoSystemMessages",
            opt(bool_desc_default(
                "Whether the exact model accepts developer or system messages after the conversation has started. When false, later system messages are folded into the leading system message. The generated catalog enables this for verified models.",
                false,
            )),
        ),
        (
            "sessionAffinityFormat",
            opt(session_affinity_format(desc(
                "Session-affinity header format. openai sends session-id and x-client-request-id; openai-nosession sends x-client-request-id; openrouter sends x-session-id. Does not affect prompt_cache_key. Default: auto-detected.",
            ))),
        ),
        (
            "promptCacheKeyMode",
            optional_prompt_cache_key_mode(desc(
                "Controls the prompt_cache_key request field. auto and enabled send it whenever caching is active; disabled never sends it, for compatible proxies that reject it. Default: auto.",
            )),
        ),
        (
            "supportsLongCacheRetention",
            opt(bool_desc_default(
                "Whether the provider supports long prompt cache retention. This uses prompt_cache_options.ttl: \"30m\" on GPT-5.6+ and prompt_cache_retention: \"24h\" on earlier models.",
                true,
            )),
        ),
        (
            "supportsStrictMode",
            opt(bool_desc(
                "Whether the provider supports strict JSON-schema function tools. Defaults are API-specific; generated OpenAI models enable it explicitly.",
            )),
        ),
        (
            "supportsOpenAIGrammarTools",
            opt(bool_desc_default(
                "Whether to emit OpenAI custom tools with Lark or regex grammar formats. When false, grammar-constrained tools fall back to normal function tools. The generated catalog enables this for capable models.",
                false,
            )),
        ),
        (
            "supportsAdditionalTools",
            opt(bool_desc_default(
                "Whether the model supports message-anchored additional_tools input items.",
                false,
            )),
        ),
        (
            "supportsToolSearch",
            opt(bool_desc_default(
                "Whether the model supports client-executed tool search for transcript-anchored additions.",
                false,
            )),
        ),
        (
            "supportsExplicitPromptCacheMode",
            opt(bool_desc_default(
                "Whether the model accepts prompt_cache_options. Older OpenAI models reject the parameter.",
                false,
            )),
        ),
        (
            "supportsMidConvoEffort",
            opt(bool_desc_default(
                "Whether thinking level changes are sent as configuration_update input items instead of changing reasoning.effort, so a change keeps the prompt cache. Only used by the openai-responses API, and ignored with Sign in with ChatGPT on the openai provider.",
                false,
            )),
        ),
        (
            "supportsMaxOutputTokens",
            opt(bool_desc_default(
                "Whether the provider accepts max_output_tokens. Some Codex-protocol gateways reject it.",
                true,
            )),
        ),
    ]
}

pub fn open_ai_responses_compat_schema() -> Schema {
    open_object(
        open_ai_responses_compat_properties(),
        desc("Compatibility settings for OpenAI Responses APIs."),
        true,
    )
}

fn anthropic_messages_compat_properties() -> Vec<(&'static str, Schema)> {
    vec![
        (
            "supportsEagerToolInputStreaming",
            opt(bool_desc_default(
                "Whether the provider accepts per-tool eager_input_streaming. When false, the Anthropic provider omits tools[].eager_input_streaming and sends the legacy fine-grained-tool-streaming-2025-05-14 beta header for tool-enabled requests.",
                true,
            )),
        ),
        (
            "supportsLongCacheRetention",
            opt(bool_desc_default(
                "Whether the provider supports Anthropic long cache retention through cache_control.ttl.",
                true,
            )),
        ),
        (
            "sendSessionAffinityHeaders",
            opt(bool_desc(
                "Whether to send x-session-affinity from options.sessionId when caching is enabled. Required for providers like Fireworks that use session affinity for prompt cache routing; requests to the same replica maximize cache hits. Default: true for OpenRouter endpoints, false otherwise.",
            )),
        ),
        (
            "sessionAffinityFormat",
            opt(string_literal(
                "openrouter",
                desc("Session-affinity format. openrouter sends x-session-id; when unset, sends x-session-affinity."),
            )),
        ),
        (
            "supportsCacheControlOnTools",
            opt(bool_desc_default(
                "Whether the provider supports Anthropic-style cache_control markers on tool definitions. When false, cache_control is omitted from tool parameters. Some Anthropic-compatible providers, such as Fireworks, do not support this field on tools and may reject or ignore it.",
                true,
            )),
        ),
        (
            "supportsTemperature",
            opt(bool_desc_default(
                "Whether the model accepts the Anthropic temperature request field. Claude Opus 4.7+ rejects non-default values.",
                true,
            )),
        ),
        (
            "forceAdaptiveThinking",
            opt(bool_desc_default(
                "Whether to force adaptive thinking (thinking.type: adaptive plus output_config.effort) regardless of model ID. Built-in models that require adaptive thinking set this in generated metadata. Custom Anthropic-compatible providers can set this to true for any model whose upstream requires the adaptive format. Set false to opt out on overridden built-in models.",
                false,
            )),
        ),
        (
            "allowEmptySignature",
            opt(bool_desc_default(
                "Whether to replay empty thinking signatures instead of converting thinking to text.",
                false,
            )),
        ),
        (
            "supportsStrictTools",
            opt(bool_desc_default(
                "Whether the provider supports Anthropic strict tool schemas. Generated Anthropic models enable it explicitly.",
                false,
            )),
        ),
        (
            "supportsMidConvoEffort",
            opt(bool_desc_default(
                "Whether the exact model transport supports effort-only system messages and thinking binding controls.",
                false,
            )),
        ),
        (
            "supportsMidConvoSystemMessages",
            opt(bool_desc_default(
                "Whether the exact model accepts system-role messages inside the conversation. When false, later system messages are folded into the top-level system prompt.",
                false,
            )),
        ),
        (
            "supportsMidConvoToolChanges",
            opt(bool_desc_default(
                "Whether the exact model accepts mid-conversation tool_addition and tool_removal blocks. Requires supportsMidConvoSystemMessages.",
                false,
            )),
        ),
        (
            "supportsMessageThreads",
            opt(bool_desc(
                "Whether the endpoint supports Anthropic message threads, which let a request with a session id send only the new messages and continue from the previous reply. Default: true for provider anthropic with a baseUrl on api.anthropic.com, false otherwise. Set true for gateways that forward the thread request field.",
            )),
        ),
        (
            "allowedFallbackModels",
            opt(arr_with(
                anthropic_allowed_fallback_model_schema(),
                ArrOptions {
                    max_items: Some(3),
                    annotations: desc(
                        "Models Anthropic accepts for server-side refusal fallback, with local pricing metadata for returned fallback responses. When absent or empty, callers must omit fallbacks; Anthropic rejects the field for models with no permitted fallback targets.",
                    ),
                    ..Default::default()
                },
            )),
        ),
    ]
}

pub fn anthropic_messages_compat_schema() -> Schema {
    open_object(
        anthropic_messages_compat_properties(),
        desc("Compatibility settings for Anthropic Messages-compatible APIs."),
        true,
    )
}

fn bedrock_compat_properties() -> Vec<(&'static str, Schema)> {
    vec![(
        "supportsStrictMode",
        opt(bool_desc_default(
            "Whether the model supports Bedrock strict tool schemas.",
            false,
        )),
    )]
}

pub fn bedrock_compat_schema() -> Schema {
    open_object(
        bedrock_compat_properties(),
        desc("Compatibility settings for Amazon Bedrock models."),
        true,
    )
}

fn mistral_conversations_compat_properties() -> Vec<(&'static str, Schema)> {
    vec![(
        "supportsMidConvoSystemMessages",
        opt(bool_desc_default(
            "Whether the exact model accepts system messages after the conversation has started. When false, later system messages are folded into the leading system message.",
            false,
        )),
    )]
}

pub fn mistral_conversations_compat_schema() -> Schema {
    open_object(
        mistral_conversations_compat_properties(),
        desc("Compatibility settings for the Mistral chat API."),
        true,
    )
}

fn system_one_compat_properties() -> Vec<(&'static str, Schema)> {
    vec![
        (
            "imageInput",
            opt(union_with(
                [
                    string_literal("assets", Annotations::default()),
                    string_literal("images", Annotations::default()),
                    string_literal("state-image-urls", Annotations::default()),
                ],
                desc(
                    "How the service takes classifier images. \"assets\" sends TypeSafe's assets map (PNG and JPEG) and references the images from the state, as TypeSafe and Jev on Workers AI expect. \"images\" sends Clef's images list of data URLs (PNG, JPEG, and WebP, at most four), as Clef on Workers AI expects. \"state-image-urls\" places image_url parts before the state, as OpenRouter expects; OpenRouter drops assets without an error. Auto-detected from the provider, baseUrl, and model ID. Classifier models read it from the provider-level compat; models and modelOverrides entries apply to chat models only.",
                ),
            )),
        ),
        (
            "sendImageOptInHeader",
            opt(bool_desc(
                "Whether requests with images send the TypeSafe-Danger-Zone-Images: true header, which TypeSafe's own API requires for its experimental image input. Defaults to true for the typesafe-system-one API unless the model is detected as OpenRouter.",
            )),
        ),
    ]
}

pub fn system_one_compat_schema() -> Schema {
    open_object(
        system_one_compat_properties(),
        desc("Compatibility settings for System One classifier APIs."),
        true,
    )
}

fn provider_compat_property_overrides() -> Vec<(&'static str, Schema)> {
    vec![
        (
            "supportsDeveloperRole",
            opt(bool_desc(
                "Whether the provider supports the developer role instead of system. Defaults are API-specific.",
            )),
        ),
        (
            "supportsMidConvoSystemMessages",
            opt(bool_desc_default(
                "Whether the exact model accepts system or developer messages after the conversation has started. When false, later system messages are folded into the leading system message.",
                false,
            )),
        ),
        (
            "sessionAffinityFormat",
            opt(session_affinity_format(desc(
                "Session-affinity header format. Defaults are API-specific or auto-detected.",
            ))),
        ),
        (
            "promptCacheKeyMode",
            optional_prompt_cache_key_mode(desc(
                "Controls the prompt_cache_key request field for openai-completions and openai-responses: auto keeps each API's default, enabled sends it whenever caching is active, disabled never sends it. Default: auto.",
            )),
        ),
        (
            "supportsMidConvoEffort",
            opt(bool_desc_default(
                "Whether thinking level changes are sent inside the conversation instead of changing the request's effort, so a change keeps the prompt cache. Used by the anthropic-messages and openai-responses APIs.",
                false,
            )),
        ),
        (
            "supportsLongCacheRetention",
            opt(bool_desc(
                "Whether the provider supports long prompt cache retention. Defaults are API-specific or auto-detected.",
            )),
        ),
        (
            "supportsStrictMode",
            opt(bool_desc(
                "Whether the provider supports strict tool schemas. Defaults are API-specific.",
            )),
        ),
        (
            "supportsOpenAIGrammarTools",
            opt(bool_desc_default(
                "Whether the provider supports OpenAI custom tools with Lark or regex grammar formats. When false, grammar-constrained tools fall back to normal function tools.",
                false,
            )),
        ),
        (
            "sendSessionAffinityHeaders",
            opt(bool_desc(
                "Whether to send session-affinity data from options.sessionId. Defaults are API-specific.",
            )),
        ),
    ]
}

fn merge_compat_properties(
    property_groups: Vec<Vec<(&'static str, Schema)>>,
    overrides: Vec<(&'static str, Schema)>,
) -> Vec<(&'static str, Schema)> {
    let mut merged: IndexMap<&'static str, Schema> = IndexMap::new();
    let mut duplicate_names: indexmap::IndexSet<&'static str> = indexmap::IndexSet::new();
    let override_map: IndexMap<&'static str, Schema> = overrides.into_iter().collect();

    for properties in property_groups {
        for (name, schema) in properties {
            if merged.contains_key(name) {
                duplicate_names.insert(name);
                assert!(
                    override_map.contains_key(name),
                    "Duplicate compatibility schema property requires an override: {name}"
                );
            }
            merged.insert(name, schema);
        }
    }

    for name in override_map.keys() {
        assert!(
            duplicate_names.contains(name),
            "Compatibility schema override does not resolve a duplicate property: {name}"
        );
    }

    for (name, schema) in override_map {
        merged.insert(name, schema);
    }

    merged.into_iter().collect()
}

/// Compatibility schema for each API whose models accept `compat`.
pub fn compat_schemas_by_api() -> IndexMap<&'static str, Schema> {
    let mut map = IndexMap::new();
    map.insert("openai-completions", open_ai_completions_compat_schema());
    map.insert("openai-responses", open_ai_responses_compat_schema());
    map.insert("azure-openai-responses", open_ai_responses_compat_schema());
    map.insert("openai-codex-responses", open_ai_responses_compat_schema());
    map.insert("anthropic-messages", anthropic_messages_compat_schema());
    map.insert("bedrock-converse-stream", bedrock_compat_schema());
    map.insert("mistral-conversations", mistral_conversations_compat_schema());
    map.insert("typesafe-system-one", system_one_compat_schema());
    map.insert("cloudflare-workers-ai-system-one", system_one_compat_schema());
    map
}

pub fn provider_compat_schema() -> Schema {
    let props = merge_compat_properties(
        vec![
            open_ai_completions_compat_properties(),
            open_ai_responses_compat_properties(),
            anthropic_messages_compat_properties(),
            bedrock_compat_properties(),
            mistral_conversations_compat_properties(),
            system_one_compat_properties(),
        ],
        provider_compat_property_overrides(),
    );
    open_object(props, desc("Provider and model compatibility overrides."), true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_schema::{CompileOptions, UnresolvedRefs, compile};

    #[test]
    fn compat_schemas_compile_cleanly() {
        let opts = CompileOptions::default()
            .with_default_formats()
            .unresolved_refs(UnresolvedRefs::Reject);
        compile(provider_compat_schema(), opts.clone()).expect("provider compat schema compiles");
        for (api, schema) in compat_schemas_by_api() {
            compile(schema, opts.clone()).unwrap_or_else(|e| panic!("compat schema for {api} failed: {e}"));
        }
    }
}
