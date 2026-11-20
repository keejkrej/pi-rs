//! Port of packages/coding-agent/src/core/settings-schema.ts

use pi_schema::{
    Annotations, NumOptions, Schema, StrOptions, arr, bool, bool_with, int_with, lit, map_with, num, num_with,
    obj_with, opt, str, str_with, union,
};
use serde_json::{Value, json};

use crate::core::pi_ai_schemas::model_thinking_level_schema;
use crate::core::settings_defaults as d;

fn non_negative_safe_integer(default_val: Option<i64>) -> Schema {
    let mut schema = int_with(NumOptions {
        minimum: Some(0.0),
        maximum: Some(9_007_199_254_740_991.0),
        ..Default::default()
    });
    if let Some(val) = default_val {
        schema = schema.with("default", val);
    }
    schema
}

fn timeout_setting(description: &str) -> Schema {
    union([
        num_with(NumOptions {
            minimum: Some(0.0),
            ..Default::default()
        }),
        lit("disabled"),
    ])
    .description(description)
}

fn compaction_model_override_schema() -> Schema {
    obj_with(
        [
            ("reserveTokens", opt(non_negative_safe_integer(None))),
            ("keepRecentTokens", opt(non_negative_safe_integer(None))),
        ],
        pi_schema::ObjOptions::default(),
    )
}

fn compaction_settings_schema() -> Schema {
    obj_with(
        [
            ("enabled", opt(bool().with("default", d::DEFAULT_COMPACTION_ENABLED))),
            (
                "reserveTokens",
                opt(non_negative_safe_integer(Some(d::DEFAULT_COMPACTION_RESERVE_TOKENS))),
            ),
            (
                "keepRecentTokens",
                opt(non_negative_safe_integer(Some(
                    d::DEFAULT_COMPACTION_KEEP_RECENT_TOKENS,
                ))),
            ),
            (
                "modelOverrides",
                opt(map_with(
                    compaction_model_override_schema(),
                    Annotations {
                        description: Some(
                            "Per-model overrides keyed by exact \"provider/modelId\" strings.".to_owned(),
                        ),
                        ..Default::default()
                    },
                )),
            ),
        ],
        pi_schema::ObjOptions::default(),
    )
}

fn branch_summary_settings_schema() -> Schema {
    obj_with(
        [
            (
                "reserveTokens",
                opt(num()
                    .description("Tokens reserved for the prompt and LLM response.")
                    .with("default", d::DEFAULT_BRANCH_SUMMARY_RESERVE_TOKENS)),
            ),
            (
                "skipPrompt",
                opt(bool()
                    .description("When true, skips the \"Summarize branch?\" prompt and defaults to no summary.")
                    .with("default", d::DEFAULT_BRANCH_SUMMARY_SKIP_PROMPT)),
            ),
        ],
        pi_schema::ObjOptions::default(),
    )
}

fn provider_retry_settings_schema() -> Schema {
    obj_with(
        [
            (
                "timeoutMs",
                opt(num().description("SDK or provider request timeout in milliseconds.")),
            ),
            ("maxRetries", opt(num().description("SDK or provider retry attempts."))),
            (
                "maxRetryDelayMs",
                opt(num()
                    .description("Maximum server-requested delay before failing.")
                    .with("default", d::DEFAULT_RETRY_PROVIDER_MAX_RETRY_DELAY_MS)),
            ),
        ],
        pi_schema::ObjOptions::default(),
    )
}

fn retry_settings_schema() -> Schema {
    obj_with(
        [
            ("enabled", opt(bool().with("default", d::DEFAULT_RETRY_ENABLED))),
            ("maxRetries", opt(num().with("default", d::DEFAULT_RETRY_MAX_RETRIES))),
            (
                "baseDelayMs",
                opt(num()
                    .description("Exponential backoff base delay in milliseconds: 2s, 4s, 8s.")
                    .with("default", d::DEFAULT_RETRY_BASE_DELAY_MS)),
            ),
            (
                "maxAgentDelayMs",
                opt(num().with("default", d::DEFAULT_RETRY_MAX_AGENT_DELAY_MS)),
            ),
            ("provider", opt(provider_retry_settings_schema())),
            (
                "maxDelayMs",
                opt(num()
                    .description("Legacy retry delay setting. Use provider.maxRetryDelayMs instead.")
                    .with("deprecated", true)),
            ),
        ],
        pi_schema::ObjOptions::default(),
    )
}

fn terminal_settings_schema() -> Schema {
    obj_with(
        [
            (
                "showImages",
                opt(bool()
                    .description("Show images when the terminal supports them.")
                    .with("default", d::DEFAULT_TERMINAL_SHOW_IMAGES)),
            ),
            (
                "imageWidthCells",
                opt(num()
                    .description("Preferred inline image width in terminal cells.")
                    .with("default", d::DEFAULT_TERMINAL_IMAGE_WIDTH_CELLS)),
            ),
            (
                "clearOnShrink",
                opt(bool()
                    .description("Clear empty rows when content shrinks.")
                    .with("default", d::DEFAULT_TERMINAL_CLEAR_ON_SHRINK)),
            ),
            (
                "showTerminalProgress",
                opt(bool()
                    .description("Show OSC 9;4 terminal progress indicators.")
                    .with("default", d::DEFAULT_TERMINAL_SHOW_TERMINAL_PROGRESS)),
            ),
            ("hyperlinks", opt(union([bool(), lit("auto")]))),
            (
                "images",
                opt(union([lit("kitty"), lit("iterm2"), lit("auto"), lit(false)])),
            ),
            ("trueColor", opt(union([bool(), lit("auto")]))),
        ],
        pi_schema::ObjOptions::default(),
    )
}

fn image_settings_schema() -> Schema {
    obj_with(
        [
            (
                "autoResize",
                opt(bool()
                    .description("Resize images to 2000x2000 maximum for better model compatibility.")
                    .with("default", d::DEFAULT_IMAGES_AUTO_RESIZE)),
            ),
            (
                "blockImages",
                opt(bool()
                    .description("When true, prevents all images from being sent to LLM providers.")
                    .with("default", d::DEFAULT_IMAGES_BLOCK_IMAGES)),
            ),
        ],
        pi_schema::ObjOptions::default(),
    )
}

fn thinking_budgets_settings_schema(description: Option<&str>) -> Schema {
    let mut s = obj_with(
        [
            ("minimal", opt(num())),
            ("low", opt(num())),
            ("medium", opt(num())),
            ("high", opt(num())),
        ],
        pi_schema::ObjOptions::default(),
    );
    if let Some(desc) = description {
        s = s.description(desc);
    }
    s
}

fn markdown_settings_schema() -> Schema {
    obj_with(
        [
            (
                "codeBlockIndent",
                opt(str().with("default", d::DEFAULT_MARKDOWN_CODE_BLOCK_INDENT)),
            ),
            (
                "mermaid",
                opt(union([lit("off"), lit("final"), lit("streaming")]).with("default", d::DEFAULT_MARKDOWN_MERMAID)),
            ),
        ],
        pi_schema::ObjOptions::default(),
    )
}

fn warning_settings_schema() -> Schema {
    obj_with(
        [(
            "anthropicExtraUsage",
            opt(bool().with("default", d::DEFAULT_WARNING_ANTHROPIC_EXTRA_USAGE)),
        )],
        pi_schema::ObjOptions::default(),
    )
}

fn codemode_settings_schema() -> Schema {
    obj_with(
        [
            (
                "mode",
                opt(union([lit("on"), lit("only")])
                    .description("How codemode presents tools. \"on\" keeps direct tools declared and annotates tools callable from scripts; codemode lists only tools without direct exposure. \"only\" lists every script-callable tool in codemode and does not declare active direct tools.")
                    .with("default", d::DEFAULT_CODEMODE_MODE)),
            ),
            (
                "inlineBudget",
                opt(num_with(NumOptions {
                    annotations: Annotations {
                        description: Some(
                            "Estimated tokens available for inline codemode tool declarations.".to_owned(),
                        ),
                        ..Default::default()
                    },
                    minimum: Some(0.0),
                    ..Default::default()
                })
                .with("default", d::DEFAULT_CODEMODE_INLINE_BUDGET)),
            ),
        ],
        pi_schema::ObjOptions::default(),
    )
}

fn package_source_schema() -> Schema {
    union([
        str_with(StrOptions {
            annotations: Annotations {
                description: Some("Load all resources from the package.".to_owned()),
                ..Default::default()
            },
            ..Default::default()
        }),
        obj_with(
            [
                ("source", str()),
                (
                    "autoload",
                    opt(bool().description("When false, start empty and only apply explicit resource patterns.")),
                ),
                ("extensions", opt(arr(str()))),
                ("skills", opt(arr(str()))),
                ("prompts", opt(arr(str()))),
                ("themes", opt(arr(str()))),
            ],
            pi_schema::ObjOptions::default(),
        ),
    ])
    .description("Package source for npm or git packages. Use the string form to load all resources or the object form to filter resources.")
}

fn skills_input_schema() -> Schema {
    union([
        arr(str()),
        obj_with(
            [
                ("enableSkillCommands", opt(bool())),
                ("customDirectories", opt(arr(str()))),
            ],
            pi_schema::ObjOptions::default(),
        )
        .with("deprecated", true),
    ])
    .description("Local skill file paths or directories.")
}

pub fn settings_schema() -> Schema {
    obj_with(
        [
            ("$schema", opt(str().description("JSON Schema reference."))),
            ("lastChangelogVersion", opt(str())),
            (
                "extensionChangelogVersions",
                opt(map_with(
                    str(),
                    Annotations {
                        description: Some("Last seen changelog version per extension package.".to_owned()),
                        ..Default::default()
                    },
                )),
            ),
            ("defaultProvider", opt(str())),
            ("defaultModel", opt(str())),
            ("defaultThinkingLevel", opt(model_thinking_level_schema())),
            (
                "modelThinkingLevels",
                opt(map_with(
                    model_thinking_level_schema(),
                    Annotations {
                        description: Some(
                            "Per-model default thinking level overrides keyed by \"provider/modelId\".".to_owned(),
                        ),
                        ..Default::default()
                    },
                )),
            ),
            (
                "transport",
                opt(union([
                    lit("auto"),
                    lit("sse"),
                    lit("websocket"),
                    lit("websocket-cached"),
                ])
                .with("default", d::DEFAULT_TRANSPORT)),
            ),
            (
                "sessionShareDestination",
                opt(union([lit("auto"), lit("gist"), lit("radius")])
                    .description("Destination used by /share. \"auto\" uses Radius when authenticated and otherwise creates a GitHub gist.")
                    .with("default", d::DEFAULT_SESSION_SHARE_DESTINATION)),
            ),
            (
                "steeringMode",
                opt(union([lit("all"), lit("one-at-a-time")]).with("default", d::DEFAULT_STEERING_MODE)),
            ),
            (
                "followUpMode",
                opt(union([lit("all"), lit("one-at-a-time")]).with("default", d::DEFAULT_FOLLOW_UP_MODE)),
            ),
            ("theme", opt(str())),
            ("compaction", opt(compaction_settings_schema())),
            ("branchSummary", opt(branch_summary_settings_schema())),
            ("retry", opt(retry_settings_schema())),
            (
                "hideThinkingBlock",
                opt(bool().with("default", d::DEFAULT_HIDE_THINKING_BLOCK)),
            ),
            (
                "showCacheMissNotices",
                opt(bool()
                    .description("Show cache cost and provider recovery notices.")
                    .with("default", d::DEFAULT_SHOW_CACHE_MISS_NOTICES)),
            ),
            (
                "externalEditor",
                opt(str().description("Command for Ctrl+G external editor; takes precedence over VISUAL and EDITOR.")),
            ),
            (
                "shellPath",
                opt(str().description(
                    "Custom shell path, for example for Cygwin on Windows, with support for leading ~ expansion.",
                )),
            ),
            (
                "quietStartup",
                opt(union([bool(), lit("header")])
                    .description("When true, hide all startup output. When \"header\", keep only the startup header.")
                    .with("default", d::DEFAULT_QUIET_STARTUP)),
            ),
            (
                "defaultProjectTrust",
                opt(union([lit("ask"), lit("always"), lit("never")])
                    .description("Global setting only.")
                    .with("default", d::DEFAULT_PROJECT_TRUST_MODE)),
            ),
            (
                "shellCommandPrefix",
                opt(str().description(
                    "Prefix prepended to every bash command, for example to enable shell aliases.",
                )),
            ),
            (
                "npmCommand",
                opt(arr(str()).description(
                    "Command used for npm package lookup and installation, in argv form such as [\"mise\", \"exec\", \"node@20\", \"--\", \"npm\"].",
                )),
            ),
            (
                "selfUpdateCommand",
                opt(arr(str())
                    .with("minItems", 1)
                    .description(
                        "Command `pi update` runs to update Pi itself, in argv form such as [\"mise\", \"use\", \"-g\", \"npm:{packageName}@{version}\"]. Placeholders: {packageName}, {version}, {installSpec}. PI_SELF_UPDATE_COMMAND takes precedence. Global setting only.",
                    )),
            ),
            (
                "collapseChangelog",
                opt(bool()
                    .description("Show the condensed changelog after update; use /changelog for the full changelog.")
                    .with("default", d::DEFAULT_COLLAPSE_CHANGELOG)),
            ),
            (
                "enableInstallTelemetry",
                opt(bool()
                    .description("Send an anonymous version and update ping after changelog-detected updates.")
                    .with("default", d::DEFAULT_ENABLE_INSTALL_TELEMETRY)),
            ),
            (
                "enableAnalytics",
                opt(bool()
                    .description("Opt in to analytics data sharing.")
                    .with("default", d::DEFAULT_ENABLE_ANALYTICS)),
            ),
            (
                "trackingId",
                opt(str().description("Analytics tracking identifier, generated when analytics is enabled.")),
            ),
            (
                "packages",
                opt(arr(package_source_schema())
                    .description("npm or git package sources, as strings or objects with resource filtering.")),
            ),
            (
                "extensions",
                opt(arr(str()).description("Local extension file paths or directories.")),
            ),
            (
                "deviceId",
                opt(str().description(
                    "Stable installation UUID, generated when authentication first needs it. Global only.",
                )),
            ),
            ("skills", opt(skills_input_schema())),
            (
                "prompts",
                opt(arr(str()).description("Local prompt template file paths or directories.")),
            ),
            (
                "themes",
                opt(arr(str()).description("Local theme file paths or directories.")),
            ),
            (
                "enableSkillCommands",
                opt(bool()
                    .description("Register skills as /skill:name commands.")
                    .with("default", d::DEFAULT_ENABLE_SKILL_COMMANDS)),
            ),
            ("terminal", opt(terminal_settings_schema())),
            ("images", opt(image_settings_schema())),
            (
                "enabledModels",
                opt(arr(str()).description("Model patterns for cycling, in the same format as the --models CLI flag.")),
            ),
            (
                "defaultTools",
                opt(arr(str()).description(
                    "Initial tool selection. Plain names replace the inherited selection; +name and -name entries add or remove tools.",
                )),
            ),
            (
                "doubleEscapeAction",
                opt(union([lit("fork"), lit("tree"), lit("none")])
                    .description("Action for double-escape with an empty editor.")
                    .with("default", d::DEFAULT_DOUBLE_ESCAPE_ACTION)),
            ),
            (
                "treeFilterMode",
                opt(union([
                    lit("default"),
                    lit("no-tools"),
                    lit("user-only"),
                    lit("labeled-only"),
                    lit("all"),
                ])
                .description("Default filter when opening /tree.")
                .with("default", d::DEFAULT_TREE_FILTER_MODE)),
            ),
            (
                "thinkingBudgets",
                opt(thinking_budgets_settings_schema(Some(
                    "Custom token budgets for thinking levels.",
                ))),
            ),
            (
                "editorPaddingX",
                opt(num()
                    .description("Horizontal padding for the input editor.")
                    .with("default", d::DEFAULT_EDITOR_PADDING_X)),
            ),
            (
                "outputPad",
                opt(union([lit(Value::from(0)), lit(Value::from(1))])
                    .description("Horizontal padding for transcript content.")
                    .with("default", i64::from(d::DEFAULT_OUTPUT_PAD))),
            ),
            (
                "autocompleteMaxVisible",
                opt(num()
                    .description("Maximum visible items in the autocomplete dropdown.")
                    .with("default", d::DEFAULT_AUTOCOMPLETE_MAX_VISIBLE)),
            ),
            (
                "showHardwareCursor",
                opt(bool().description("Show the terminal cursor while still positioning it for IME.")),
            ),
            ("markdown", opt(markdown_settings_schema())),
            ("warnings", opt(warning_settings_schema())),
            ("codemode", opt(codemode_settings_schema())),
            (
                "sessionDir",
                opt(str().description("Custom session storage directory, in the same format as the --session-dir CLI flag.")),
            ),
            (
                "httpProxy",
                opt(str().description("Proxy URL applied as HTTP_PROXY and HTTPS_PROXY for Pi-managed HTTP clients.")),
            ),
            (
                "httpIdleTimeoutMs",
                opt(timeout_setting(
                    "HTTP header or body idle timeout in milliseconds; 0 or \"disabled\" disables it.",
                )),
            ),
            (
                "cacheWarming",
                opt(union([lit("off"), lit("streaming"), lit("idle")])
                    .description("Cache-warming profile. \"idle\" also warms between agent runs. Global only because each refresh costs money.")
                    .with("default", d::DEFAULT_CACHE_WARMING)),
            ),
            (
                "websocketConnectTimeoutMs",
                opt(timeout_setting(
                    "WebSocket connect or open handshake timeout in milliseconds; 0 or \"disabled\" disables it.",
                )),
            ),
            (
                "tuiMode",
                opt(union([lit("regular"), lit("fullscreen")]).with("default", d::DEFAULT_TUI_MODE)),
            ),
            (
                "fullscreenExitOutput",
                opt(union([lit("transcript"), lit("resume-hint")])
                    .description("No effect in regular TUI mode.")
                    .with("default", d::DEFAULT_FULLSCREEN_EXIT_OUTPUT)),
            ),
            (
                "fullscreenScrollbar",
                opt(union([lit("auto"), lit("always"), lit("hidden")])
                    .description("No effect in regular TUI mode.")
                    .with("default", d::DEFAULT_FULLSCREEN_SCROLLBAR)),
            ),
            (
                "fullscreenCopyOnSelect",
                opt(bool()
                    .description("No effect in regular TUI mode.")
                    .with("default", d::DEFAULT_FULLSCREEN_COPY_ON_SELECT)),
            ),
            (
                "fullscreenWheelScrollLines",
                opt(union([num(), lit("auto")])
                    .description("Lines scrolled per wheel event in fullscreen mode; numeric values are clamped from 1 to 100.")
                    .with("default", d::DEFAULT_FULLSCREEN_WHEEL_SCROLL_LINES)),
            ),
            (
                "queueMode",
                opt(union([lit("all"), lit("one-at-a-time")])
                    .description("Legacy setting migrated to steeringMode.")
                    .with("deprecated", true)),
            ),
            (
                "websockets",
                opt(bool_with(Annotations {
                    description: Some("Legacy setting migrated to transport.".to_owned()),
                    ..Default::default()
                })
                .with("deprecated", true)),
            ),
        ],
        pi_schema::ObjOptions {
            annotations: Annotations::default(),
            open: true,
        },
    )
    .with("additionalProperties", json!(true))
}
