//! Port of packages/coding-agent/src/cli/args.ts

//! CLI argument parsing and help display

use std::path::{Path, PathBuf};
use std::process::Stdio;

use indexmap::IndexMap;
use pi_ai::ModelThinkingLevel;
use pi_js::str16;
use pi_tui::TuiMode;
use serde::{Deserialize, Serialize};

use crate::config::{APP_NAME, CONFIG_DIR_NAME, ENV_AGENT_DIR, ENV_SESSION_DIR};
use crate::core::extensions::types::{ExtensionFlag, FlagType};
pub use crate::core::model_resolver::VALID_THINKING_LEVELS;
use crate::vendor::chalk;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Text,
    Json,
    Rpc,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UnknownFlagValue {
    Bool(bool),
    String(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArgDiagnosticType {
    Warning,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArgDiagnostic {
    pub r#type: ArgDiagnosticType,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ListModelsOption {
    All,
    Pattern(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Args {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub api_key: Option<String>,
    pub system_prompt: Option<String>,
    pub append_system_prompt: Option<Vec<String>>,
    pub thinking: Option<ModelThinkingLevel>,
    pub r#continue: Option<bool>,
    pub resume: Option<bool>,
    pub help: Option<bool>,
    pub version: Option<bool>,
    // PORT: --gui flag for dispatching to sibling pi-gui binary
    pub gui: Option<bool>,
    pub cwd: Option<String>,
    pub mode: Option<Mode>,
    pub name: Option<String>,
    pub no_session: Option<bool>,
    pub session: Option<String>,
    pub session_id: Option<String>,
    pub fork: Option<String>,
    pub session_dir: Option<String>,
    pub models: Option<Vec<String>>,
    pub tools: Option<Vec<String>>,
    pub exclude_tools: Option<Vec<String>>,
    pub no_tools: Option<bool>,
    pub no_builtin_tools: Option<bool>,
    pub extensions: Option<Vec<String>>,
    pub no_extensions: Option<bool>,
    pub print: Option<bool>,
    pub export: Option<String>,
    pub no_skills: Option<bool>,
    pub skills: Option<Vec<String>>,
    pub prompt_templates: Option<Vec<String>>,
    pub no_prompt_templates: Option<bool>,
    pub themes: Option<Vec<String>>,
    pub use_theme: Option<String>,
    pub no_themes: Option<bool>,
    pub no_context_files: Option<bool>,
    pub list_models: Option<ListModelsOption>,
    pub offline: Option<bool>,
    pub tui_mode: Option<TuiMode>,
    pub verbose: Option<bool>,
    pub project_trust_override: Option<bool>,
    pub messages: Vec<String>,
    pub file_args: Vec<String>,
    /// Unknown flags (potentially extension flags) - map of flag name to value
    pub unknown_flags: IndexMap<String, UnknownFlagValue>,
    pub diagnostics: Vec<ArgDiagnostic>,
}

pub fn is_valid_thinking_level(level: &str) -> bool {
    crate::core::model_resolver::is_valid_thinking_level(level)
}

pub fn parse_thinking_level(level: &str) -> Option<ModelThinkingLevel> {
    crate::core::model_resolver::parse_valid_thinking_level(level)
}

pub fn normalize_session_name(value: &str) -> Option<String> {
    let name = str16::trim(value);
    if name.is_empty() { None } else { Some(name.to_string()) }
}

pub fn parse_args<S: AsRef<str>>(args: &[S]) -> Args {
    let mut result = Args::default();
    let mut i = 0;

    while i < args.len() {
        let arg = args[i].as_ref();

        if arg == "--" {
            for positional_arg in &args[i + 1..] {
                let positional_arg = positional_arg.as_ref();
                if let Some(stripped) = positional_arg.strip_prefix('@') {
                    result.file_args.push(stripped.to_string());
                } else {
                    result.messages.push(positional_arg.to_string());
                }
            }
            break;
        } else if arg == "--help" || arg == "-h" {
            result.help = Some(true);
        } else if arg == "--version" || arg == "-v" {
            result.version = Some(true);
        } else if arg == "--gui" {
            // PORT: --gui flag for dispatching to sibling pi-gui executable
            result.gui = Some(true);
        } else if arg == "--cwd" && i + 1 < args.len() {
            i += 1;
            result.cwd = Some(args[i].as_ref().to_string());
        } else if let Some(val) = arg.strip_prefix("--cwd=") {
            result.cwd = Some(val.to_string());
        } else if arg == "--mode" {
            let mode = args.get(i + 1).map(AsRef::as_ref);
            let Some(mode) = mode else {
                result.diagnostics.push(ArgDiagnostic {
                    r#type: ArgDiagnosticType::Error,
                    message: "--mode requires text, json, or rpc".to_string(),
                });
                i += 1;
                continue;
            };
            if mode.starts_with('-') {
                result.diagnostics.push(ArgDiagnostic {
                    r#type: ArgDiagnosticType::Error,
                    message: "--mode requires text, json, or rpc".to_string(),
                });
                i += 1;
                continue;
            }
            i += 1;
            match mode {
                "text" => result.mode = Some(Mode::Text),
                "json" => result.mode = Some(Mode::Json),
                "rpc" => result.mode = Some(Mode::Rpc),
                _ => {
                    result.diagnostics.push(ArgDiagnostic {
                        r#type: ArgDiagnosticType::Error,
                        message: format!("Invalid mode \"{mode}\". Valid values: text, json, rpc"),
                    });
                }
            }
        } else if arg == "--continue" || arg == "-c" {
            result.r#continue = Some(true);
        } else if arg == "--resume" || arg == "-r" {
            result.resume = Some(true);
        } else if arg == "--provider" && i + 1 < args.len() {
            i += 1;
            result.provider = Some(args[i].as_ref().to_string());
        } else if arg == "--model" && i + 1 < args.len() {
            i += 1;
            result.model = Some(args[i].as_ref().to_string());
        } else if arg == "--api-key" && i + 1 < args.len() {
            i += 1;
            result.api_key = Some(args[i].as_ref().to_string());
        } else if arg == "--system-prompt" && i + 1 < args.len() {
            i += 1;
            result.system_prompt = Some(args[i].as_ref().to_string());
        } else if arg == "--append-system-prompt" && i + 1 < args.len() {
            i += 1;
            result
                .append_system_prompt
                .get_or_insert_with(Vec::new)
                .push(args[i].as_ref().to_string());
        } else if arg == "--name" || arg == "-n" {
            if i + 1 < args.len() {
                i += 1;
                result.name = Some(args[i].as_ref().to_string());
            } else {
                result.diagnostics.push(ArgDiagnostic {
                    r#type: ArgDiagnosticType::Error,
                    message: "--name requires a value".to_string(),
                });
            }
        } else if arg == "--no-session" {
            result.no_session = Some(true);
        } else if arg == "--session" && i + 1 < args.len() {
            i += 1;
            result.session = Some(args[i].as_ref().to_string());
        } else if arg == "--session-id" && i + 1 < args.len() {
            i += 1;
            result.session_id = Some(args[i].as_ref().to_string());
        } else if arg == "--fork" && i + 1 < args.len() {
            i += 1;
            result.fork = Some(args[i].as_ref().to_string());
        } else if arg == "--session-dir" && i + 1 < args.len() {
            i += 1;
            result.session_dir = Some(args[i].as_ref().to_string());
        } else if arg == "--models" && i + 1 < args.len() {
            i += 1;
            result.models = Some(
                args[i]
                    .as_ref()
                    .split(',')
                    .map(|s| str16::trim(s).to_string())
                    .filter(|s| !s.is_empty())
                    .collect(),
            );
        } else if arg == "--no-tools" || arg == "-nt" {
            result.no_tools = Some(true);
        } else if arg == "--no-builtin-tools" || arg == "-nbt" {
            result.no_builtin_tools = Some(true);
        } else if (arg == "--tools" || arg == "-t") && i + 1 < args.len() {
            i += 1;
            result.tools = Some(
                args[i]
                    .as_ref()
                    .split(',')
                    .map(|s| str16::trim(s).to_string())
                    .filter(|name| !name.is_empty())
                    .collect(),
            );
        } else if (arg == "--exclude-tools" || arg == "-xt") && i + 1 < args.len() {
            i += 1;
            result.exclude_tools = Some(
                args[i]
                    .as_ref()
                    .split(',')
                    .map(|s| str16::trim(s).to_string())
                    .filter(|name| !name.is_empty())
                    .collect(),
            );
        } else if arg == "--thinking" && i + 1 < args.len() {
            i += 1;
            let level = args[i].as_ref();
            if let Some(parsed_level) = parse_thinking_level(level) {
                result.thinking = Some(parsed_level);
            } else {
                result.diagnostics.push(ArgDiagnostic {
                    r#type: ArgDiagnosticType::Warning,
                    message: format!(
                        "Invalid thinking level \"{level}\". Valid values: {}",
                        VALID_THINKING_LEVELS.join(", ")
                    ),
                });
            }
        } else if arg == "--print" || arg == "-p" {
            result.print = Some(true);
            if let Some(next) = args.get(i + 1).map(AsRef::as_ref)
                && !next.starts_with('@')
                && (!next.starts_with('-') || next.starts_with("---"))
            {
                result.messages.push(next.to_string());
                i += 1;
            }
        } else if arg == "--export" && i + 1 < args.len() {
            i += 1;
            result.export = Some(args[i].as_ref().to_string());
        } else if (arg == "--extension" || arg == "-e") && i + 1 < args.len() {
            i += 1;
            result
                .extensions
                .get_or_insert_with(Vec::new)
                .push(args[i].as_ref().to_string());
        } else if arg == "--no-extensions" || arg == "-ne" {
            result.no_extensions = Some(true);
        } else if arg == "--skill" && i + 1 < args.len() {
            i += 1;
            result
                .skills
                .get_or_insert_with(Vec::new)
                .push(args[i].as_ref().to_string());
        } else if arg == "--prompt-template" && i + 1 < args.len() {
            i += 1;
            result
                .prompt_templates
                .get_or_insert_with(Vec::new)
                .push(args[i].as_ref().to_string());
        } else if arg == "--theme" && i + 1 < args.len() {
            i += 1;
            result
                .themes
                .get_or_insert_with(Vec::new)
                .push(args[i].as_ref().to_string());
        } else if arg == "--use-theme" {
            let theme_name = args.get(i + 1).map(AsRef::as_ref);
            match theme_name {
                Some(name) if !name.starts_with('-') => {
                    result.use_theme = Some(name.to_string());
                    i += 1;
                }
                _ => {
                    result.diagnostics.push(ArgDiagnostic {
                        r#type: ArgDiagnosticType::Error,
                        message: "--use-theme requires a theme name".to_string(),
                    });
                }
            }
        } else if arg == "--no-skills" || arg == "-ns" {
            result.no_skills = Some(true);
        } else if arg == "--no-prompt-templates" || arg == "-np" {
            result.no_prompt_templates = Some(true);
        } else if arg == "--no-themes" {
            result.no_themes = Some(true);
        } else if arg == "--no-context-files" || arg == "-nc" {
            result.no_context_files = Some(true);
        } else if arg == "--list-models" {
            // Check if next arg is a search pattern (not a flag or file arg)
            if let Some(next) = args.get(i + 1).map(AsRef::as_ref)
                && !next.starts_with('-')
                && !next.starts_with('@')
            {
                result.list_models = Some(ListModelsOption::Pattern(next.to_string()));
                i += 1;
            } else {
                result.list_models = Some(ListModelsOption::All);
            }
        } else if arg == "--tui-mode" {
            let mode = args.get(i + 1).map(AsRef::as_ref);
            match mode {
                Some("regular") => {
                    result.tui_mode = Some(TuiMode::Regular);
                    i += 1;
                }
                Some("fullscreen") => {
                    result.tui_mode = Some(TuiMode::Fullscreen);
                    i += 1;
                }
                Some(m) if !m.starts_with('-') => {
                    i += 1;
                    result.diagnostics.push(ArgDiagnostic {
                        r#type: ArgDiagnosticType::Error,
                        message: format!("Invalid TUI mode \"{m}\". Valid values: regular, fullscreen"),
                    });
                }
                _ => {
                    result.diagnostics.push(ArgDiagnostic {
                        r#type: ArgDiagnosticType::Error,
                        message: "--tui-mode requires regular or fullscreen".to_string(),
                    });
                }
            }
        } else if arg == "--verbose" {
            result.verbose = Some(true);
        } else if arg == "--approve" || arg == "-a" {
            result.project_trust_override = Some(true);
        } else if arg == "--no-approve" || arg == "-na" {
            result.project_trust_override = Some(false);
        } else if arg == "--offline" {
            result.offline = Some(true);
        } else if let Some(stripped) = arg.strip_prefix('@') {
            result.file_args.push(stripped.to_string());
        } else if let Some(rest) = arg.strip_prefix("--") {
            if let Some(eq_index) = rest.find('=') {
                let name = &rest[..eq_index];
                let val = &rest[eq_index + 1..];
                result
                    .unknown_flags
                    .insert(name.to_string(), UnknownFlagValue::String(val.to_string()));
            } else if let Some(next) = args.get(i + 1).map(AsRef::as_ref)
                && !next.starts_with('-')
                && !next.starts_with('@')
            {
                result
                    .unknown_flags
                    .insert(rest.to_string(), UnknownFlagValue::String(next.to_string()));
                i += 1;
            } else {
                result
                    .unknown_flags
                    .insert(rest.to_string(), UnknownFlagValue::Bool(true));
            }
        } else if arg.starts_with('-') && !arg.starts_with("--") {
            result.diagnostics.push(ArgDiagnostic {
                r#type: ArgDiagnosticType::Error,
                message: format!("Unknown option: {arg}"),
            });
        } else if !arg.starts_with('-') {
            result.messages.push(arg.to_string());
        }

        i += 1;
    }

    result
}

pub fn print_help(extension_flags: Option<&[ExtensionFlag]>) {
    let extension_flags_text = match extension_flags {
        Some(flags) if !flags.is_empty() => {
            let lines: Vec<String> = flags
                .iter()
                .map(|flag| {
                    let value = if flag.r#type == FlagType::String {
                        " <value>"
                    } else {
                        ""
                    };
                    let description = flag
                        .description
                        .clone()
                        .unwrap_or_else(|| format!("Registered by {}", flag.extension_path));
                    let left = format!("  --{}{}", flag.name, value);
                    format!("{left:<30}{description}")
                })
                .collect();
            format!("\n{}\n{}\n", chalk::bold("Extension CLI Flags:"), lines.join("\n"))
        }
        _ => String::new(),
    };

    let app: &str = *APP_NAME;
    let cfg_dir: &str = *CONFIG_DIR_NAME;
    let env_agent_padded = format!("{:<32}", *ENV_AGENT_DIR);
    let env_session_padded = format!("{:<32}", *ENV_SESSION_DIR);

    let text = format!(
        "{app_bold} - AI coding assistant with read, bash, edit, write tools

{usage_hdr}
  {app} [options] [--] [@files...] [messages...]

{cmds_hdr}
  {app} install <source> [-l]     Install extension source and add to settings
  {app} remove <source> [-l]      Remove extension source from settings
  {app} uninstall <source> [-l]   Alias for remove
  {app} update [source|self|pi]   Update pi, extensions, or model catalogs
  {app} list                      List installed extensions from settings
  {app} config [-l]               Open TUI to enable/disable package resources (Tab switches scope)
  {app} auth <command>            Print credentials or check provider readiness
  {app} mcp <command>             Check MCP servers, sign in to or out of OAuth servers
  {app} gui [options]             Launch the graphical pi-gui desktop client
  {app} <command> --help          Show help for install/remove/uninstall/update/list/config/auth/mcp

{opts_hdr}
  --provider <name>              Provider to search for --model (requires --model)
  --model <pattern>              Model pattern or ID (supports \"provider/id\" and optional \":<thinking>\")
  --api-key <key>                API key (defaults to env vars)
  --system-prompt <text>         System prompt (default: coding assistant prompt)
  --append-system-prompt <text>  Append text or file contents to the system prompt (can be used multiple times)
  --mode <mode>                  Output mode: text (default), json, or rpc
  --print, -p                    Non-interactive mode: process prompt and exit
  --continue, -c                 Continue previous session
  --resume, -r                   Select a session to resume
  --session <path|id>            Use specific session file or partial UUID
  --session-id <id>              Use exact project session ID, creating it if missing
  --fork <path|id>               Fork specific session file or partial UUID into a new session
  --session-dir <dir>            Directory for session storage and lookup
  --no-session                   Don't save session (ephemeral)
  --name, -n <name>              Set session display name
  --models <patterns>            Comma-separated model patterns for Ctrl+P cycling
                                 Supports globs (anthropic/*, *sonnet*) and fuzzy matching
  --no-tools, -nt                Disable all tools by default (built-in and extension)
  --no-builtin-tools, -nbt       Disable built-in tools by default but keep extension/custom tools enabled
  --tools, -t <tools>            Comma-separated allowlist of tool names to enable
                                 Applies to built-in, extension, and custom tools
  --exclude-tools, -xt <tools>   Comma-separated denylist of tool names to disable
                                 Applies to built-in, extension, and custom tools
  --thinking <level>             Set thinking level: off, minimal, low, medium, high, xhigh, max
  --extension, -e <path>         Load an extension file or builtin:<name> (can be used multiple times)
  --no-extensions, -ne           Disable extension discovery and built-in extensions (explicit -e paths still work)
  --skill <path>                 Load a skill file or directory (can be used multiple times)
  --no-skills, -ns               Disable skills discovery and loading
  --prompt-template <path>       Load a prompt template file or directory (can be used multiple times)
  --no-prompt-templates, -np     Disable prompt template discovery and loading
  --theme <path>                 Load a theme file or directory (can be used multiple times)
  --use-theme <name[/name]>      Set the initial interactive theme for this run
  --no-themes                    Disable theme discovery and loading
  --no-context-files, -nc        Disable AGENTS.md and CLAUDE.md discovery and loading
  --export <file>                Export session file to HTML and exit
  --list-models [search]         List available models (with optional fuzzy search)
  --verbose                      Force verbose startup (overrides quietStartup setting)
  --tui-mode <mode>              TUI mode: fullscreen (default) or regular
  --gui                          Launch the graphical pi-gui desktop client
  --approve, -a                  Trust project-local files for this run
  --no-approve, -na              Ignore project-local files for this run
  --offline                      Disable startup network operations (same as PI_OFFLINE=1)
  --                             End option parsing; treat remaining arguments as messages/files
  --help, -h                     Show this help
  --version, -v                  Show version number

Extensions can register additional flags (e.g., --plan from plan-mode extension).{extension_flags_text}

{examples_hdr}
  # Print a provider API key for an external client
  {app} auth print-api-key --provider openai

  # Print an OAuth bearer token for an external client (refreshes if expired)
  {app} auth print-bearer-token --provider openai-codex

  # Interactive mode
  {app}

  # Interactive mode with initial prompt
  {app} \"List all .ts files in src/\"

  # Include files in initial message
  {app} @prompt.md @image.png \"What color is the sky?\"

  # Non-interactive mode (process and exit)
  {app} -p \"List all .ts files in src/\"

  # Prompt beginning with a dash
  {app} -p -- \"- Summarize these points\"

  # Multiple messages (interactive)
  {app} \"Read package.json\" \"What dependencies do we have?\"

  # Continue previous session
  {app} --continue \"What did we discuss?\"

  # Start a named session
  {app} --name \"Refactor auth module\"

  # Use different model
  {app} --provider openai --model gpt-4o-mini \"Help me refactor this code\"

  # Use model with provider prefix (no --provider needed)
  {app} --model openai/gpt-4o \"Help me refactor this code\"

  # Use model with thinking level shorthand
  {app} --model sonnet:high \"Solve this complex problem\"

  # Limit model cycling to specific models
  {app} --models claude-sonnet,claude-haiku,gpt-4o

  # Limit to a specific provider with glob pattern
  {app} --models \"github-copilot/*\"

  # Cycle models with fixed thinking levels
  {app} --models sonnet:high,haiku:low

  # Start with a specific thinking level
  {app} --thinking high \"Solve this complex problem\"

  # Read-only mode (no file modifications possible)
  {app} --tools read,grep,find,ls -p \"Review the code in src/\"

  # Disable one tool while keeping the rest available
  {app} --exclude-tools ask_question

  # Export a session file to HTML
  {app} --export ~/{cfg_dir}/agent/sessions/--path--/session.jsonl
  {app} --export session.jsonl output.html

{env_hdr}
  ANTHROPIC_AUTH_TOKEN             - Anthropic bearer auth token
  ANTHROPIC_API_KEY                - Anthropic Claude API key
  ANTHROPIC_OAUTH_TOKEN            - Anthropic OAuth token (alternative to API key)
  ANT_LING_API_KEY                 - Ant Ling API key
  OPENAI_API_KEY                   - OpenAI GPT API key
  AZURE_OPENAI_API_KEY             - Azure OpenAI API key
  AZURE_OPENAI_BASE_URL            - Azure OpenAI/Cognitive Services base URL (e.g. https://{{resource}}.openai.azure.com)
  AZURE_OPENAI_RESOURCE_NAME       - Azure OpenAI resource name (alternative to base URL)
  AZURE_OPENAI_API_VERSION         - Azure OpenAI API version (default: v1)
  AZURE_OPENAI_DEPLOYMENT_NAME_MAP - Azure OpenAI model=deployment map (comma-separated)
  DEEPSEEK_API_KEY                 - DeepSeek API key
  NVIDIA_API_KEY                   - NVIDIA NIM API key
  GEMINI_API_KEY                   - Google Gemini API key
  GROQ_API_KEY                     - Groq API key
  CEREBRAS_API_KEY                 - Cerebras API key
  XAI_API_KEY                      - xAI Grok API key
  FIREWORKS_API_KEY                - Fireworks API key
  TOGETHER_API_KEY                 - Together AI API key
  BASETEN_API_KEY                  - Baseten API key
  OPENROUTER_API_KEY               - OpenRouter API key
  AI_GATEWAY_API_KEY               - Vercel AI Gateway API key
  ZAI_API_KEY                      - ZAI Coding Plan API key (Global)
  ZAI_CODING_CN_API_KEY            - ZAI Coding Plan API key (China)
  MISTRAL_API_KEY                  - Mistral API key
  MINIMAX_API_KEY                  - MiniMax API key
  MOONSHOT_API_KEY                 - Moonshot AI API key
  OPENCODE_API_KEY                 - OpenCode Zen/OpenCode Go API key
  KIMI_API_KEY                     - Kimi For Coding API key
  META_API_KEY                     - Meta Model API key
  CLOUDFLARE_API_KEY               - Cloudflare API token (Workers AI and AI Gateway)
  CLOUDFLARE_ACCOUNT_ID            - Cloudflare account id (required for both)
  CLOUDFLARE_GATEWAY_ID            - Cloudflare AI Gateway slug (required for AI Gateway)
  QWEN_TOKEN_PLAN_API_KEY          - Qwen Token Plan API key (international region)
  QWEN_TOKEN_PLAN_CN_API_KEY       - Qwen Token Plan API key (China region)
  XIAOMI_API_KEY                   - Xiaomi MiMo API key (api.xiaomimimo.com billing)
  XIAOMI_TOKEN_PLAN_CN_API_KEY     - Xiaomi MiMo Token Plan API key (China region)
  XIAOMI_TOKEN_PLAN_AMS_API_KEY    - Xiaomi MiMo Token Plan API key (Amsterdam region)
  XIAOMI_TOKEN_PLAN_SGP_API_KEY    - Xiaomi MiMo Token Plan API key (Singapore region)
  AWS_PROFILE                      - AWS profile for Amazon Bedrock
  AWS_ACCESS_KEY_ID                - AWS access key for Amazon Bedrock
  AWS_SECRET_ACCESS_KEY            - AWS secret key for Amazon Bedrock
  AWS_BEARER_TOKEN_BEDROCK         - Bedrock API key (bearer token)
  AWS_REGION                       - AWS region for Amazon Bedrock (e.g., us-east-1)
  {env_agent_padded} - Config directory (default: ~/{cfg_dir}/agent)
  {env_session_padded} - Session storage directory (overridden by --session-dir)
  PI_PACKAGE_DIR                   - Override package directory (for Nix/Guix store paths)
  PI_OFFLINE                       - Disable startup network operations when set to 1/true/yes
  PI_TELEMETRY                     - Override install telemetry when set to 1/true/yes or 0/false/no
  PI_SHARE_VIEWER_URL              - Base URL for /share command (default: https://pi.dev/session/)

{tools_hdr}
  read       - Read file contents
  bash       - Execute bash commands
  powershell - Execute PowerShell commands on Windows
  edit       - Edit files with find/replace
  write      - Write files (creates/overwrites)
  grep       - Search file contents (read-only, off by default)
  find       - Find files by glob pattern (read-only, off by default)
  ls         - List directory contents (read-only, off by default)
",
        app_bold = chalk::bold(app),
        usage_hdr = chalk::bold("Usage:"),
        cmds_hdr = chalk::bold("Commands:"),
        opts_hdr = chalk::bold("Options:"),
        examples_hdr = chalk::bold("Examples:"),
        env_hdr = chalk::bold("Environment Variables:"),
        tools_hdr = chalk::bold("Built-in Tool Names:"),
    );

    pi_js::console::log(&text);
}

// PORT: GUI dispatch helpers for `--gui` flag and `gui` subcommand (zero dependency cycle with pi-gui)

/// Returns `true` when the CLI invocation uses the `gui` subcommand (`pi gui ...`).
pub fn is_gui_subcommand<S: AsRef<str>>(args: &[S]) -> bool {
    args.first().map(AsRef::as_ref) == Some("gui")
}

/// Builds the argument list forwarded to the sibling `pi-gui` binary.
///
/// Strips the leading `gui` subcommand or any `--gui` flags before `--`, while preserving
/// `--cwd`, `--session`, and all remaining arguments. If `--cwd` was not already specified
/// before `--`, prepends `["--cwd", cwd]`.
pub fn build_gui_forward_args<S: AsRef<str>>(args: &[S], cwd: &str) -> Vec<String> {
    let slice: &[S] = if is_gui_subcommand(args) { &args[1..] } else { args };
    let mut forwarded = Vec::with_capacity(slice.len() + 2);
    let mut has_cwd = false;
    let mut seen_end_of_options = false;
    let mut i = 0;

    while i < slice.len() {
        let arg = slice[i].as_ref();
        if !seen_end_of_options && arg == "--" {
            seen_end_of_options = true;
            forwarded.push(arg.to_string());
            i += 1;
            continue;
        }
        if !seen_end_of_options {
            if arg == "--gui" {
                i += 1;
                continue;
            }
            if arg == "--cwd" || arg.starts_with("--cwd=") {
                has_cwd = true;
            }
        }
        forwarded.push(arg.to_string());
        i += 1;
    }

    if !has_cwd && !cwd.is_empty() {
        let mut with_cwd = Vec::with_capacity(forwarded.len() + 2);
        with_cwd.push("--cwd".to_string());
        with_cwd.push(cwd.to_string());
        with_cwd.extend(forwarded);
        with_cwd
    } else {
        forwarded
    }
}

fn gui_binary_name() -> &'static str {
    if pi_js::env::platform() == "win32" {
        "pi-gui.exe"
    } else {
        "pi-gui"
    }
}

/// Resolves the `pi-gui` executable path from a concrete current executable path, falling back to `"pi-gui"` on `PATH`.
pub fn resolve_gui_executable_from(current_exe: Option<&Path>) -> PathBuf {
    let bin_name = gui_binary_name();
    if let Some(exe) = current_exe {
        let sibling = exe.with_file_name(bin_name);
        if sibling.is_file() {
            return sibling;
        }
    }
    PathBuf::from(bin_name)
}

/// Resolves the sibling `pi-gui` executable (`current_exe().with_file_name("pi-gui")`, falling back to `"pi-gui"` on `PATH`).
pub fn resolve_gui_executable() -> PathBuf {
    let current = std::env::current_exe().ok();
    resolve_gui_executable_from(current.as_deref())
}

type GuiDispatchOverrideFn = dyn Fn(&Path, &[String], &str) -> pi_js::BoxFuture<pi_js::Result<i32>> + Send + Sync;

thread_local! {
    static DISPATCH_GUI_OVERRIDE: pi_js::seam::Slot<GuiDispatchOverrideFn> = pi_js::seam::Slot::new();
}

/// Spawns the sibling `pi-gui` executable with forwarded arguments and returns its exit code.
pub async fn dispatch_gui<S: AsRef<str>>(args: &[S], cwd: &str) -> pi_js::Result<i32> {
    let exe = resolve_gui_executable();
    let forward_args = build_gui_forward_args(args, cwd);
    if let Some(handler) = DISPATCH_GUI_OVERRIDE.with(|s| s.get()) {
        return handler(&exe, &forward_args, cwd).await;
    }

    let mut cmd = tokio::process::Command::new(&exe);
    cmd.args(&forward_args)
        .current_dir(cwd)
        .env_clear()
        .envs(pi_js::env::vars())
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    let status = cmd
        .status()
        .await
        .map_err(|err| pi_js::Error::msg(format!("Failed to launch {}: {err}", exe.display())))?;
    Ok(status.code().unwrap_or(1))
}

pub mod testing {
    use std::path::Path;
    use std::sync::Arc;

    use pi_js::BoxFuture;

    pub fn override_dispatch_gui(
        f: impl Fn(&Path, &[String], &str) -> BoxFuture<pi_js::Result<i32>> + Send + Sync + 'static,
    ) -> pi_js::seam::Guard {
        super::DISPATCH_GUI_OVERRIDE.with(|s| s.set(Arc::new(f)))
    }
}
