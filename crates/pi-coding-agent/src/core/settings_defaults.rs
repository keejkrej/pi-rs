//! Port of packages/coding-agent/src/core/settings-defaults.ts

use pi_ai::utils::retry::DEFAULT_MAX_AGENT_RETRY_DELAY_MS;

pub const DEFAULT_TRANSPORT: &str = "auto";
pub const DEFAULT_SESSION_SHARE_DESTINATION: &str = "auto";
pub const DEFAULT_STEERING_MODE: &str = "one-at-a-time";
pub const DEFAULT_FOLLOW_UP_MODE: &str = "one-at-a-time";

pub const DEFAULT_COMPACTION_ENABLED: bool = true;
pub const DEFAULT_COMPACTION_RESERVE_TOKENS: i64 = 16_384;
pub const DEFAULT_COMPACTION_KEEP_RECENT_TOKENS: i64 = 20_000;

pub const DEFAULT_BRANCH_SUMMARY_RESERVE_TOKENS: i64 = 16_384;
pub const DEFAULT_BRANCH_SUMMARY_SKIP_PROMPT: bool = false;

pub const DEFAULT_RETRY_ENABLED: bool = true;
pub const DEFAULT_RETRY_MAX_RETRIES: i64 = 3;
pub const DEFAULT_RETRY_BASE_DELAY_MS: i64 = 2_000;
pub const DEFAULT_RETRY_MAX_AGENT_DELAY_MS: u64 = DEFAULT_MAX_AGENT_RETRY_DELAY_MS;
pub const DEFAULT_RETRY_PROVIDER_MAX_RETRY_DELAY_MS: i64 = 60_000;

pub const DEFAULT_HIDE_THINKING_BLOCK: bool = false;
pub const DEFAULT_SHOW_CACHE_MISS_NOTICES: bool = false;
pub const DEFAULT_QUIET_STARTUP: bool = false;
pub const DEFAULT_PROJECT_TRUST_MODE: &str = "ask";
pub const DEFAULT_COLLAPSE_CHANGELOG: bool = false;
pub const DEFAULT_ENABLE_INSTALL_TELEMETRY: bool = true;
pub const DEFAULT_ENABLE_ANALYTICS: bool = false;
pub const DEFAULT_ENABLE_SKILL_COMMANDS: bool = true;

pub const DEFAULT_TERMINAL_SHOW_IMAGES: bool = true;
pub const DEFAULT_TERMINAL_IMAGE_WIDTH_CELLS: i64 = 60;
pub const DEFAULT_TERMINAL_CLEAR_ON_SHRINK: bool = false;
pub const DEFAULT_TERMINAL_SHOW_TERMINAL_PROGRESS: bool = false;

pub const DEFAULT_IMAGES_AUTO_RESIZE: bool = true;
pub const DEFAULT_IMAGES_BLOCK_IMAGES: bool = false;

pub const DEFAULT_DOUBLE_ESCAPE_ACTION: &str = "tree";
pub const DEFAULT_TREE_FILTER_MODE: &str = "default";
pub const DEFAULT_EDITOR_PADDING_X: i64 = 0;
pub const DEFAULT_OUTPUT_PAD: u8 = 1;
pub const DEFAULT_AUTOCOMPLETE_MAX_VISIBLE: i64 = 5;

pub const DEFAULT_MARKDOWN_CODE_BLOCK_INDENT: &str = "  ";
pub const DEFAULT_MARKDOWN_MERMAID: &str = "streaming";

pub const DEFAULT_WARNING_ANTHROPIC_EXTRA_USAGE: bool = true;

pub const DEFAULT_CODEMODE_MODE: &str = "on";
pub const DEFAULT_CODEMODE_INLINE_BUDGET: i64 = 3_000;

pub const DEFAULT_CACHE_WARMING: &str = "idle";
pub const DEFAULT_TUI_MODE: &str = "fullscreen";
pub const DEFAULT_FULLSCREEN_EXIT_OUTPUT: &str = "transcript";
pub const DEFAULT_FULLSCREEN_SCROLLBAR: &str = "auto";
pub const DEFAULT_FULLSCREEN_COPY_ON_SELECT: bool = true;
pub const DEFAULT_FULLSCREEN_WHEEL_SCROLL_LINES: &str = "auto";
