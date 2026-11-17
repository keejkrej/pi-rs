//! Port of packages/coding-agent/src/migrations.ts

use std::io::Read;

use serde_json::{Map, Value};

use crate::config::{CONFIG_DIR_NAME, get_agent_dir, get_bin_dir};
use crate::core::keybindings::migrate_keybindings_config;
use crate::utils::text::strip_bom;
use crate::vendor::chalk::chalk;

const MIGRATION_GUIDE_URL: &str =
    "https://github.com/earendil-works/pi/blob/main/packages/coding-agent/CHANGELOG.md#extensions-migration";
const EXTENSIONS_DOC_URL: &str =
    "https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/extensions.md";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MigrationResult {
    pub migrated_auth_providers: Vec<String>,
    pub deprecation_warnings: Vec<String>,
}

/// Migrate legacy oauth.json and settings.json apiKeys to auth.json.
pub fn migrate_auth_to_auth_json() -> Vec<String> {
    let agent_dir = get_agent_dir();
    let auth_path = pi_js::path::join(&[&agent_dir, "auth.json"]);
    let oauth_path = pi_js::path::join(&[&agent_dir, "oauth.json"]);
    let settings_path = pi_js::path::join(&[&agent_dir, "settings.json"]);

    if pi_js::fs::exists(&auth_path) {
        return Vec::new();
    }

    let mut migrated = Map::new();
    let mut providers = Vec::new();

    if pi_js::fs::exists(&oauth_path)
        && let Ok(raw) = pi_js::fs::read_to_string(&oauth_path)
        && let Ok(Value::Object(oauth_obj)) = pi_js::json::parse(strip_bom(&raw))
    {
        for (provider, cred) in oauth_obj {
            if let Value::Object(cred_obj) = cred {
                let mut entry = Map::new();
                entry.insert("type".to_owned(), Value::String("oauth".to_owned()));
                for (k, v) in cred_obj {
                    entry.insert(k, v);
                }
                migrated.insert(provider.clone(), Value::Object(entry));
                providers.push(provider);
            }
        }
        let _ = pi_js::fs::rename(&oauth_path, &format!("{oauth_path}.migrated"));
    }

    if pi_js::fs::exists(&settings_path)
        && let Ok(raw) = pi_js::fs::read_to_string(&settings_path)
        && let Ok(Value::Object(mut settings)) = pi_js::json::parse(strip_bom(&raw))
        && let Some(Value::Object(api_keys)) = settings.get("apiKeys").cloned()
    {
        for (provider, key) in api_keys {
            if !migrated.contains_key(&provider)
                && let Value::String(k) = key
            {
                let mut entry = Map::new();
                entry.insert("type".to_owned(), Value::String("api_key".to_owned()));
                entry.insert("key".to_owned(), Value::String(k));
                migrated.insert(provider.clone(), Value::Object(entry));
                providers.push(provider);
            }
        }
        settings.shift_remove("apiKeys");
        let _ = pi_js::fs::write_file(
            &settings_path,
            pi_js::json::stringify_pretty(&settings, "  ").as_bytes(),
            None,
        );
    }

    if !migrated.is_empty() {
        let dir = pi_js::path::dirname(&auth_path);
        let _ = pi_js::fs::mkdir(&dir, true, None);
        let _ = pi_js::fs::write_file(
            &auth_path,
            pi_js::json::stringify_pretty(&migrated, "  ").as_bytes(),
            Some(0o600),
        );
    }

    providers
}

fn encode_session_dir_name(cwd: &str) -> String {
    let trimmed = cwd.trim_start_matches(['/', '\\']);
    let mut safe = String::with_capacity(trimmed.len() + 4);
    safe.push_str("--");
    for ch in trimmed.chars() {
        if matches!(ch, '/' | '\\' | ':') {
            safe.push('-');
        } else {
            safe.push(ch);
        }
    }
    safe.push_str("--");
    safe
}

pub fn migrate_sessions_from_agent_root() {
    let agent_dir = get_agent_dir();
    let Ok(entries) = pi_js::fs::readdir(&agent_dir) else {
        return;
    };
    let files: Vec<String> = entries
        .into_iter()
        .filter(|f| f.ends_with(".jsonl"))
        .map(|f| pi_js::path::join(&[&agent_dir, &f]))
        .collect();
    if files.is_empty() {
        return;
    }

    for file in files {
        let Ok(content) = pi_js::fs::read_to_string(&file) else {
            continue;
        };
        let Some(first_line) = content.split('\n').next() else {
            continue;
        };
        if pi_js::str16::trim(first_line).is_empty() {
            continue;
        }
        let Ok(Value::Object(header)) = pi_js::json::parse(first_line) else {
            continue;
        };
        if header.get("type").and_then(Value::as_str) != Some("session") {
            continue;
        }
        let Some(cwd) = header.get("cwd").and_then(Value::as_str).filter(|s| !s.is_empty()) else {
            continue;
        };

        let correct_dir = pi_js::path::join(&[&agent_dir, "sessions", &encode_session_dir_name(cwd)]);
        if !pi_js::fs::exists(&correct_dir) && pi_js::fs::mkdir(&correct_dir, true, None).is_err() {
            continue;
        }

        let file_name = file.rsplit(['/', '\\']).next().unwrap_or("");
        let new_path = pi_js::path::join(&[&correct_dir, file_name]);
        if pi_js::fs::exists(&new_path) {
            continue;
        }
        let _ = pi_js::fs::rename(&file, &new_path);
    }
}

fn migrate_commands_to_prompts(base_dir: &str, label: &str) -> bool {
    let commands_dir = pi_js::path::join(&[base_dir, "commands"]);
    let prompts_dir = pi_js::path::join(&[base_dir, "prompts"]);

    if pi_js::fs::exists(&commands_dir) && !pi_js::fs::exists(&prompts_dir) {
        match pi_js::fs::rename(&commands_dir, &prompts_dir) {
            Ok(()) => {
                pi_js::console::log(&chalk().green().apply(&format!("Migrated {label} commands/ → prompts/")));
                return true;
            }
            Err(err) => {
                pi_js::console::log(&chalk().yellow().apply(&format!(
                    "Warning: Could not migrate {label} commands/ to prompts/: {err}"
                )));
            }
        }
    }
    false
}

fn migrate_keybindings_config_file() {
    let config_path = pi_js::path::join(&[&get_agent_dir(), "keybindings.json"]);
    if !pi_js::fs::exists(&config_path) {
        return;
    }
    let Ok(raw) = pi_js::fs::read_to_string(&config_path) else {
        return;
    };
    let Ok(Value::Object(obj)) = pi_js::json::parse(strip_bom(&raw)) else {
        return;
    };
    let (migrated_cfg, migrated) = migrate_keybindings_config(&obj);
    if !migrated {
        return;
    }
    let formatted = format!("{}\n", pi_js::json::stringify_pretty(&migrated_cfg, "  "));
    let _ = pi_js::fs::write_file(&config_path, formatted.as_bytes(), None);
}

fn migrate_tools_to_bin() {
    let agent_dir = get_agent_dir();
    let tools_dir = pi_js::path::join(&[&agent_dir, "tools"]);
    let bin_dir = get_bin_dir();

    if !pi_js::fs::exists(&tools_dir) {
        return;
    }

    let binaries = ["fd", "rg", "fd.exe", "rg.exe"];
    let mut moved_any = false;
    for bin in binaries {
        let old_path = pi_js::path::join(&[&tools_dir, bin]);
        let new_path = pi_js::path::join(&[&bin_dir, bin]);
        if pi_js::fs::exists(&old_path) {
            if !pi_js::fs::exists(&bin_dir) {
                let _ = pi_js::fs::mkdir(&bin_dir, true, None);
            }
            if !pi_js::fs::exists(&new_path) {
                if pi_js::fs::rename(&old_path, &new_path).is_ok() {
                    moved_any = true;
                }
            } else {
                let _ = pi_js::fs::rm(&old_path, false, true);
            }
        }
    }

    if moved_any {
        pi_js::console::log(&chalk().green().apply("Migrated managed binaries tools/ → bin/"));
    }
}

fn check_deprecated_extension_dirs(base_dir: &str, label: &str) -> Vec<String> {
    let hooks_dir = pi_js::path::join(&[base_dir, "hooks"]);
    let tools_dir = pi_js::path::join(&[base_dir, "tools"]);
    let mut warnings = Vec::new();

    if pi_js::fs::exists(&hooks_dir) {
        warnings.push(format!(
            "{label} hooks/ directory found. Hooks have been renamed to extensions."
        ));
    }

    if pi_js::fs::exists(&tools_dir)
        && let Ok(entries) = pi_js::fs::readdir(&tools_dir)
    {
        let has_custom = entries.iter().any(|e| {
            let lower = e.to_ascii_lowercase();
            lower != "fd" && lower != "rg" && lower != "fd.exe" && lower != "rg.exe" && !e.starts_with('.')
        });
        if has_custom {
            warnings.push(format!(
                "{label} tools/ directory contains custom tools. Custom tools have been merged into extensions."
            ));
        }
    }

    warnings
}

fn migrate_extension_system(cwd: &str) -> Vec<String> {
    let agent_dir = get_agent_dir();
    let project_dir = pi_js::path::join(&[cwd, &CONFIG_DIR_NAME]);

    migrate_commands_to_prompts(&agent_dir, "Global");
    migrate_commands_to_prompts(&project_dir, "Project");

    let mut warnings = check_deprecated_extension_dirs(&agent_dir, "Global");
    warnings.extend(check_deprecated_extension_dirs(&project_dir, "Project"));
    warnings
}

pub async fn show_deprecation_warnings(warnings: &[String]) {
    if warnings.is_empty() {
        return;
    }

    for warning in warnings {
        pi_js::console::log(&chalk().yellow().apply(&format!("Warning: {warning}")));
    }
    pi_js::console::log(
        &chalk()
            .yellow()
            .apply("\nMove your extensions to the extensions/ directory."),
    );
    pi_js::console::log(
        &chalk()
            .yellow()
            .apply(&format!("Migration guide: {MIGRATION_GUIDE_URL}")),
    );
    pi_js::console::log(&chalk().yellow().apply(&format!("Documentation: {EXTENSIONS_DOC_URL}")));
    pi_js::console::log(&chalk().dim().apply("\nPress any key to continue..."));

    let _ = tokio::task::spawn_blocking(|| {
        let mut buf = [0u8; 1];
        let _ = std::io::stdin().read(&mut buf);
    })
    .await;
    pi_js::console::log("");
}

pub fn run_migrations(cwd: &str) -> MigrationResult {
    let migrated_auth_providers = migrate_auth_to_auth_json();
    migrate_sessions_from_agent_root();
    migrate_tools_to_bin();
    migrate_keybindings_config_file();
    let deprecation_warnings = migrate_extension_system(cwd);
    MigrationResult {
        migrated_auth_providers,
        deprecation_warnings,
    }
}
