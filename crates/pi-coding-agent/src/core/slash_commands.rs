//! Port of packages/coding-agent/src/core/slash-commands.ts

use std::sync::LazyLock;

use serde::{Deserialize, Serialize};

use crate::config::APP_NAME;
use crate::core::source_info::SourceInfo;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SlashCommandSource {
    Extension,
    Prompt,
    Skill,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SlashCommandInfo {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub source: SlashCommandSource,
    pub source_info: SourceInfo,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuiltinSlashCommand {
    pub name: &'static str,
    pub description: String,
    pub argument_hint: Option<&'static str>,
}

pub static BUILTIN_SLASH_COMMANDS: LazyLock<Vec<BuiltinSlashCommand>> = LazyLock::new(|| {
    vec![
        BuiltinSlashCommand {
            name: "settings",
            description: "Open settings menu".to_owned(),
            argument_hint: None,
        },
        BuiltinSlashCommand {
            name: "model",
            description: "Select model (opens selector UI)".to_owned(),
            argument_hint: Some("<provider/model>"),
        },
        BuiltinSlashCommand {
            name: "tree",
            description: "Navigate session tree (switch branches)".to_owned(),
            argument_hint: None,
        },
        BuiltinSlashCommand {
            name: "thinking",
            description: "Set thinking level".to_owned(),
            argument_hint: Some("<level>"),
        },
        BuiltinSlashCommand {
            name: "scoped-models",
            description: "Enable/disable models for Ctrl+P cycling".to_owned(),
            argument_hint: None,
        },
        BuiltinSlashCommand {
            name: "export",
            description: "Export session (HTML default, or specify path: .html/.jsonl)".to_owned(),
            argument_hint: None,
        },
        BuiltinSlashCommand {
            name: "import",
            description: "Import and resume a session from a JSONL file".to_owned(),
            argument_hint: None,
        },
        BuiltinSlashCommand {
            name: "share",
            description: "Share session as a secret GitHub gist".to_owned(),
            argument_hint: None,
        },
        BuiltinSlashCommand {
            name: "bug",
            description: "Report a bug to the Pi developers".to_owned(),
            argument_hint: Some("<description>"),
        },
        BuiltinSlashCommand {
            name: "copy",
            description: "Copy last agent message to clipboard".to_owned(),
            argument_hint: None,
        },
        BuiltinSlashCommand {
            name: "name",
            description: "Set session display name".to_owned(),
            argument_hint: None,
        },
        BuiltinSlashCommand {
            name: "session",
            description: "Show session info and stats".to_owned(),
            argument_hint: None,
        },
        BuiltinSlashCommand {
            name: "changelog",
            description: "Show changelog entries".to_owned(),
            argument_hint: None,
        },
        BuiltinSlashCommand {
            name: "hotkeys",
            description: "Show all keyboard shortcuts".to_owned(),
            argument_hint: None,
        },
        BuiltinSlashCommand {
            name: "fork",
            description: "Create a new fork from a previous user message".to_owned(),
            argument_hint: None,
        },
        BuiltinSlashCommand {
            name: "clone",
            description: "Duplicate the current session at the current position".to_owned(),
            argument_hint: None,
        },
        BuiltinSlashCommand {
            name: "trust",
            description: "Save project trust decision for future sessions".to_owned(),
            argument_hint: None,
        },
        BuiltinSlashCommand {
            name: "login",
            description: "Configure provider authentication".to_owned(),
            argument_hint: Some("<provider>"),
        },
        BuiltinSlashCommand {
            name: "logout",
            description: "Remove provider authentication".to_owned(),
            argument_hint: None,
        },
        BuiltinSlashCommand {
            name: "new",
            description: "Start a new session".to_owned(),
            argument_hint: None,
        },
        BuiltinSlashCommand {
            name: "compact",
            description: "Manually compact the session context".to_owned(),
            argument_hint: None,
        },
        BuiltinSlashCommand {
            name: "resume",
            description: "Resume a different session".to_owned(),
            argument_hint: None,
        },
        BuiltinSlashCommand {
            name: "reload",
            description: "Reload keybindings, extensions, skills, prompts, themes, and context files".to_owned(),
            argument_hint: None,
        },
        BuiltinSlashCommand {
            name: "quit",
            description: format!("Quit {}", *APP_NAME),
            argument_hint: None,
        },
    ]
});
