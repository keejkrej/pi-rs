//! Port of packages/coding-agent/src/core/resolve-config-value.ts

use std::sync::{LazyLock, Mutex};

use indexmap::IndexMap;
use pi_js::{Error, Result};

use crate::utils::shell::{CommandTransport, get_shell_config};

static COMMAND_RESULT_CACHE: LazyLock<Mutex<IndexMap<String, Option<String>>>> =
    LazyLock::new(|| Mutex::new(IndexMap::new()));

fn is_env_var_name_start(ch: char) -> bool {
    ch.is_ascii_alphabetic() || ch == '_'
}

fn is_env_var_name_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

fn is_valid_env_var_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    is_env_var_name_start(first) && chars.all(is_env_var_name_char)
}

#[derive(Clone, Debug)]
enum TemplatePart {
    Literal(String),
    Env(String),
}

enum ConfigValueReference {
    Command(String),
    Template(Vec<TemplatePart>),
}

fn append_literal(parts: &mut Vec<TemplatePart>, value: &str) {
    if value.is_empty() {
        return;
    }
    if let Some(TemplatePart::Literal(prev)) = parts.last_mut() {
        prev.push_str(value);
        return;
    }
    parts.push(TemplatePart::Literal(value.to_owned()));
}

fn parse_config_value_template(config: &str) -> Vec<TemplatePart> {
    let mut parts = Vec::new();
    let mut index = 0usize;

    while index < config.len() {
        let Some(rel_dollar) = config[index..].find('$') else {
            append_literal(&mut parts, &config[index..]);
            break;
        };
        let dollar_index = index + rel_dollar;
        append_literal(&mut parts, &config[index..dollar_index]);

        let rest = &config[dollar_index + 1..];
        let Some(next_char) = rest.chars().next() else {
            append_literal(&mut parts, "$");
            index = dollar_index + 1;
            continue;
        };

        if next_char == '$' || next_char == '!' {
            let mut buf = [0u8; 4];
            append_literal(&mut parts, next_char.encode_utf8(&mut buf));
            index = dollar_index + 2;
            continue;
        }

        if next_char == '{' {
            if let Some(rel_end) = rest[1..].find('}') {
                let end_index = dollar_index + 2 + rel_end;
                let name = &config[dollar_index + 2..end_index];
                if is_valid_env_var_name(name) {
                    parts.push(TemplatePart::Env(name.to_owned()));
                } else {
                    append_literal(&mut parts, &config[dollar_index..=end_index]);
                }
                index = end_index + 1;
                continue;
            }
            append_literal(&mut parts, "$");
            index = dollar_index + 1;
            continue;
        }

        if is_env_var_name_start(next_char) {
            let mut end_rel = 0usize;
            for (offset, ch) in rest.char_indices() {
                if offset == 0 || is_env_var_name_char(ch) {
                    end_rel = offset + ch.len_utf8();
                } else {
                    break;
                }
            }
            let name = &rest[..end_rel];
            parts.push(TemplatePart::Env(name.to_owned()));
            index = dollar_index + 1 + end_rel;
            continue;
        }

        append_literal(&mut parts, "$");
        index = dollar_index + 1;
    }

    parts
}

fn parse_config_value_reference(config: &str) -> ConfigValueReference {
    if config.starts_with('!') {
        ConfigValueReference::Command(config.to_owned())
    } else {
        ConfigValueReference::Template(parse_config_value_template(config))
    }
}

fn resolve_env_config_value(name: &str, env: Option<&IndexMap<String, String>>) -> Option<String> {
    if let Some(map) = env
        && let Some(val) = map.get(name).filter(|s| !s.is_empty())
    {
        return Some(val.clone());
    }
    pi_js::env::var(name).filter(|s| !s.is_empty())
}

fn get_template_env_var_names(parts: &[TemplatePart]) -> Vec<String> {
    let mut names = Vec::new();
    for part in parts {
        if let TemplatePart::Env(name) = part
            && !names.contains(name)
        {
            names.push(name.clone());
        }
    }
    names
}

fn resolve_template(parts: &[TemplatePart], env: Option<&IndexMap<String, String>>) -> Option<String> {
    let mut resolved = String::new();
    for part in parts {
        match part {
            TemplatePart::Literal(lit) => resolved.push_str(lit),
            TemplatePart::Env(name) => {
                let val = resolve_env_config_value(name, env)?;
                resolved.push_str(&val);
            }
        }
    }
    Some(resolved)
}

pub fn get_config_value_env_var_name(config: &str) -> Option<String> {
    match parse_config_value_reference(config) {
        ConfigValueReference::Template(parts) if parts.len() == 1 => match &parts[0] {
            TemplatePart::Env(name) => Some(name.clone()),
            _ => None,
        },
        _ => None,
    }
}

pub fn get_config_value_env_var_names(config: &str) -> Vec<String> {
    match parse_config_value_reference(config) {
        ConfigValueReference::Template(parts) => get_template_env_var_names(&parts),
        ConfigValueReference::Command(_) => Vec::new(),
    }
}

pub fn get_missing_config_value_env_var_names(config: &str, env: Option<&IndexMap<String, String>>) -> Vec<String> {
    get_config_value_env_var_names(config)
        .into_iter()
        .filter(|name| resolve_env_config_value(name, env).is_none())
        .collect()
}

pub fn is_command_config_value(config: &str) -> bool {
    matches!(parse_config_value_reference(config), ConfigValueReference::Command(_))
}

pub fn is_config_value_configured(config: &str, env: Option<&IndexMap<String, String>>) -> bool {
    get_missing_config_value_env_var_names(config, env).is_empty()
}

fn run_cmd_with_timeout(mut cmd: std::process::Command, stdin_bytes: Option<&[u8]>) -> Option<Option<String>> {
    use std::io::Write;
    cmd.env_clear()
        .envs(pi_js::env::vars())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    if stdin_bytes.is_some() {
        cmd.stdin(std::process::Stdio::piped());
    } else {
        cmd.stdin(std::process::Stdio::null());
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(_) => return None,
    };

    if let Some(bytes) = stdin_bytes
        && let Some(mut stdin) = child.stdin.take()
    {
        let _ = stdin.write_all(bytes);
    }

    let deadline = pi_js::time::now_ms() + 10_000;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return Some(None);
                }
                let out = child.wait_with_output().ok()?;
                let s = pi_js::str16::trim(&String::from_utf8_lossy(&out.stdout)).to_owned();
                return Some(if s.is_empty() { None } else { Some(s) });
            }
            Ok(None) => {
                if pi_js::time::now_ms() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Some(None);
                }
                std::thread::yield_now();
            }
            Err(_) => return Some(None),
        }
    }
}

fn execute_with_configured_shell(command: &str) -> (bool, Option<String>) {
    let Ok(cfg) = get_shell_config(None) else {
        return (false, None);
    };
    let mut cmd = std::process::Command::new(&cfg.shell);
    let stdin_mode = cfg.command_transport == Some(CommandTransport::Stdin);
    cmd.args(&cfg.args);
    if !stdin_mode {
        cmd.arg(command);
    }
    match run_cmd_with_timeout(cmd, if stdin_mode { Some(command.as_bytes()) } else { None }) {
        None => (false, None),
        Some(v) => (true, v),
    }
}

fn execute_with_default_shell(command: &str) -> Option<String> {
    let cmd = if pi_js::env::platform() == "win32" {
        let mut c = std::process::Command::new("cmd.exe");
        c.args(["/d", "/s", "/c", command]);
        c
    } else {
        let mut c = std::process::Command::new("/bin/sh");
        c.args(["-c", command]);
        c
    };
    run_cmd_with_timeout(cmd, None).flatten()
}

fn execute_command_uncached(command_config: &str) -> Option<String> {
    let command = command_config.get(1..).unwrap_or("");
    if pi_js::env::platform() == "win32" {
        let (executed, val) = execute_with_configured_shell(command);
        if executed {
            val
        } else {
            execute_with_default_shell(command)
        }
    } else {
        execute_with_default_shell(command)
    }
}

fn execute_command(command_config: &str) -> Option<String> {
    if let Ok(guard) = COMMAND_RESULT_CACHE.lock()
        && let Some(cached) = guard.get(command_config)
    {
        return cached.clone();
    }
    let result = execute_command_uncached(command_config);
    if let Ok(mut guard) = COMMAND_RESULT_CACHE.lock() {
        guard.insert(command_config.to_owned(), result.clone());
    }
    result
}

pub fn resolve_config_value(config: &str, env: Option<&IndexMap<String, String>>) -> Option<String> {
    match parse_config_value_reference(config) {
        ConfigValueReference::Command(cfg) => execute_command(&cfg),
        ConfigValueReference::Template(parts) => resolve_template(&parts, env),
    }
}

pub fn resolve_config_value_uncached(config: &str, env: Option<&IndexMap<String, String>>) -> Option<String> {
    match parse_config_value_reference(config) {
        ConfigValueReference::Command(cfg) => execute_command_uncached(&cfg),
        ConfigValueReference::Template(parts) => resolve_template(&parts, env),
    }
}

pub fn resolve_config_value_or_throw(
    config: &str,
    description: &str,
    env: Option<&IndexMap<String, String>>,
) -> Result<String> {
    if let Some(val) = resolve_config_value_uncached(config, env) {
        return Ok(val);
    }

    match parse_config_value_reference(config) {
        ConfigValueReference::Command(cfg) => Err(Error::msg(format!(
            "Failed to resolve {description} from shell command: {}",
            cfg.get(1..).unwrap_or("")
        ))),
        ConfigValueReference::Template(_) => {
            let missing = get_missing_config_value_env_var_names(config, env);
            if missing.len() == 1 {
                return Err(Error::msg(format!(
                    "Failed to resolve {description} from environment variable: {}",
                    missing[0]
                )));
            }
            if missing.len() > 1 {
                return Err(Error::msg(format!(
                    "Failed to resolve {description} from environment variables: {}",
                    missing.join(", ")
                )));
            }
            Err(Error::msg(format!("Failed to resolve {description}")))
        }
    }
}

pub fn resolve_headers(
    headers: Option<&IndexMap<String, String>>,
    env: Option<&IndexMap<String, String>>,
) -> Option<IndexMap<String, String>> {
    let headers = headers?;
    let mut resolved = IndexMap::new();
    for (key, value) in headers {
        if let Some(v) = resolve_config_value(value, env)
            && !v.is_empty()
        {
            resolved.insert(key.clone(), v);
        }
    }
    if resolved.is_empty() { None } else { Some(resolved) }
}

pub fn resolve_headers_or_throw(
    headers: Option<&IndexMap<String, String>>,
    description: &str,
    env: Option<&IndexMap<String, String>>,
) -> Result<Option<IndexMap<String, String>>> {
    let Some(headers) = headers else {
        return Ok(None);
    };
    let mut resolved = IndexMap::new();
    for (key, value) in headers {
        let v = resolve_config_value_or_throw(value, &format!("{description} header \"{key}\""), env)?;
        resolved.insert(key.clone(), v);
    }
    Ok(if resolved.is_empty() { None } else { Some(resolved) })
}

pub fn clear_config_value_cache() {
    if let Ok(mut guard) = COMMAND_RESULT_CACHE.lock() {
        guard.clear();
    }
}
