use anyhow::{Context, Result};
use dirs::home_dir;
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum PackageSource {
    Simple(String),
    Filtered {
        source: String,
        #[serde(default)]
        extensions: Vec<String>,
        #[serde(default)]
        skills: Vec<String>,
        #[serde(default, rename = "prompts")]
        prompt_templates: Vec<String>,
        #[serde(default)]
        themes: Vec<String>,
    },
}

impl PackageSource {
    pub fn source(&self) -> &str {
        match self {
            Self::Simple(source) => source,
            Self::Filtered { source, .. } => source,
        }
    }
}

impl From<String> for PackageSource {
    fn from(value: String) -> Self {
        Self::Simple(value)
    }
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct SettingsFile {
    #[serde(rename = "defaultModel")]
    pub default_model: Option<String>,
    #[serde(rename = "defaultProvider")]
    pub default_provider: Option<String>,
    #[serde(rename = "defaultThinkingLevel")]
    pub default_thinking_level: Option<String>,
    #[serde(default)]
    pub packages: Vec<PackageSource>,
    #[serde(default)]
    pub extensions: Vec<String>,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default, rename = "enableSkillCommands")]
    pub enable_skill_commands: Option<bool>,
    #[serde(default, rename = "prompts")]
    pub prompt_templates: Vec<String>,
    #[serde(default)]
    pub themes: Vec<String>,
    #[serde(default, rename = "enabledModels")]
    pub enabled_models: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct RawSettingsFile {
    #[serde(rename = "defaultModel")]
    default_model: Option<String>,
    #[serde(rename = "defaultProvider")]
    default_provider: Option<String>,
    #[serde(rename = "defaultThinkingLevel")]
    default_thinking_level: Option<String>,
    #[serde(default)]
    packages: Vec<PackageSource>,
    #[serde(default)]
    extensions: Vec<String>,
    skills: Option<serde_json::Value>,
    #[serde(default, rename = "enableSkillCommands")]
    enable_skill_commands: Option<bool>,
    #[serde(default, rename = "prompts")]
    prompt_templates: Vec<String>,
    #[serde(default)]
    themes: Vec<String>,
    #[serde(default, rename = "enabledModels")]
    enabled_models: Vec<String>,
}

impl<'de> Deserialize<'de> for SettingsFile {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawSettingsFile::deserialize(deserializer)?;
        let (skills, nested_enable) = parse_skills_settings(raw.skills)
            .map_err(serde::de::Error::custom)?;
        Ok(Self {
            default_model: raw.default_model,
            default_provider: raw.default_provider,
            default_thinking_level: raw.default_thinking_level,
            packages: raw.packages,
            extensions: raw.extensions,
            skills,
            enable_skill_commands: raw.enable_skill_commands.or(nested_enable),
            prompt_templates: raw.prompt_templates,
            themes: raw.themes,
            enabled_models: raw.enabled_models,
        })
    }
}

fn parse_skills_settings(value: Option<serde_json::Value>) -> Result<(Vec<String>, Option<bool>)> {
    let Some(value) = value else {
        return Ok((Vec::new(), None));
    };
    if value.is_null() {
        return Ok((Vec::new(), None));
    }
    if let Some(items) = value.as_array() {
        let skills = items
            .iter()
            .filter_map(|item| item.as_str().map(str::to_string))
            .collect::<Vec<_>>();
        return Ok((skills, None));
    }
    if let Some(object) = value.as_object() {
        let enable = object.get("enableSkillCommands").and_then(|value| value.as_bool());
        let skills = object
            .get("customDirectories")
            .and_then(|value| value.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_string))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        return Ok((skills, enable));
    }
    Ok((Vec::new(), None))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum StoredCredential {
    #[serde(rename = "oauth")]
    OAuth {
        access: String,
        refresh: String,
        expires: i64,
        #[serde(rename = "accountId")]
        account_id: String,
    },
    #[serde(rename = "api_key")]
    ApiKey { key: String },
}

#[derive(Debug, Clone)]
pub struct SettingsManager {
    global_path: PathBuf,
    project_path: PathBuf,
}

impl SettingsManager {
    pub fn new(cwd: &Path) -> Result<Self> {
        let agent_dir = agent_dir()?;
        Ok(Self {
            global_path: agent_dir.join("settings.json"),
            project_path: cwd.join(".pi").join("settings.json"),
        })
    }

    pub fn load_global(&self) -> Result<SettingsFile> {
        load_json_file(&self.global_path)
    }

    pub fn load_project(&self) -> Result<SettingsFile> {
        load_json_file(&self.project_path)
    }

    pub fn merged(&self) -> Result<SettingsFile> {
        let global = self.load_global().unwrap_or_default();
        let project = self.load_project().unwrap_or_default();
        Ok(SettingsFile {
            default_model: project.default_model.or(global.default_model),
            default_provider: project.default_provider.or(global.default_provider),
            default_thinking_level: project
                .default_thinking_level
                .or(global.default_thinking_level),
            packages: if project.packages.is_empty() {
                global.packages
            } else {
                project.packages
            },
            extensions: if project.extensions.is_empty() {
                global.extensions
            } else {
                project.extensions
            },
            skills: if project.skills.is_empty() {
                global.skills
            } else {
                project.skills
            },
            enable_skill_commands: project.enable_skill_commands.or(global.enable_skill_commands),
            prompt_templates: if project.prompt_templates.is_empty() {
                global.prompt_templates
            } else {
                project.prompt_templates
            },
            themes: if project.themes.is_empty() {
                global.themes
            } else {
                project.themes
            },
            enabled_models: if project.enabled_models.is_empty() {
                global.enabled_models
            } else {
                project.enabled_models
            },
        })
    }

    pub fn save_global(&self, settings: &SettingsFile) -> Result<()> {
        save_json_file(&self.global_path, settings)
    }

    pub fn save_project(&self, settings: &SettingsFile) -> Result<()> {
        save_json_file(&self.project_path, settings)
    }

    pub fn global_path(&self) -> &Path {
        &self.global_path
    }

    pub fn project_path(&self) -> &Path {
        &self.project_path
    }
}

#[derive(Debug, Clone)]
pub struct AuthStorage {
    path: PathBuf,
}

impl AuthStorage {
    pub fn new() -> Result<Self> {
        Ok(Self {
            path: agent_dir()?.join("auth.json"),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load_all(&self) -> Result<BTreeMap<String, StoredCredential>> {
        load_json_file(&self.path)
    }

    pub fn load(&self, provider: &str) -> Result<Option<StoredCredential>> {
        Ok(self.load_all()?.remove(provider))
    }

    pub fn save(&self, provider: &str, credential: StoredCredential) -> Result<()> {
        let mut data = self.load_all().unwrap_or_default();
        data.insert(provider.to_string(), credential);
        save_json_file(&self.path, &data)
    }

    pub fn api_key(&self, provider: &str) -> Result<Option<String>> {
        Ok(match self.load(provider)? {
            Some(StoredCredential::ApiKey { key }) => Some(key),
            _ => None,
        })
    }

    pub fn remove(&self, provider: &str) -> Result<()> {
        let mut data = self.load_all().unwrap_or_default();
        data.remove(provider);
        save_json_file(&self.path, &data)
    }
}

pub fn agent_dir() -> Result<PathBuf> {
    let path = match std::env::var("PI_AGENT_DIR") {
        Ok(path) if !path.trim().is_empty() => expand_tilde_path(&path),
        _ => {
            let home = home_dir().context("could not resolve home directory")?;
            home.join(".pi").join("agent")
        }
    };
    fs::create_dir_all(&path)?;
    Ok(path)
}

pub fn sessions_root() -> Result<PathBuf> {
    let path = match std::env::var("PI_SESSION_DIR") {
        Ok(path) if !path.trim().is_empty() => expand_tilde_path(&path),
        _ => agent_dir()?.join("sessions"),
    };
    fs::create_dir_all(&path)?;
    Ok(path)
}

fn expand_tilde_path(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(path)
}

fn load_json_file<T>(path: &Path) -> Result<T>
where
    T: for<'de> Deserialize<'de> + Default,
{
    if !path.exists() {
        return Ok(T::default());
    }
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    Ok(serde_json::from_str(&content)
        .with_context(|| format!("failed to parse {}", path.display()))?)
}

fn save_json_file<T>(path: &Path, value: &T) -> Result<()>
where
    T: Serialize,
{
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_vec_pretty(value)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_legacy_skills_object_settings() {
        let settings: SettingsFile = serde_json::from_str(
            r#"{
                "skills": {
                    "enableSkillCommands": false,
                    "customDirectories": ["one", "two"]
                }
            }"#,
        )
        .unwrap();

        assert_eq!(settings.skills, vec!["one", "two"]);
        assert_eq!(settings.enable_skill_commands, Some(false));
    }

    #[test]
    fn parses_current_skills_array_and_top_level_enable_flag() {
        let settings: SettingsFile = serde_json::from_str(
            r#"{
                "skills": ["custom"],
                "enableSkillCommands": true
            }"#,
        )
        .unwrap();

        assert_eq!(settings.skills, vec!["custom"]);
        assert_eq!(settings.enable_skill_commands, Some(true));
    }
}
