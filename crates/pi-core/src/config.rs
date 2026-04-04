use anyhow::{Context, Result};
use dirs::home_dir;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SettingsFile {
    #[serde(rename = "defaultModel")]
    pub default_model: Option<String>,
    #[serde(rename = "defaultThinkingLevel")]
    pub default_thinking_level: Option<String>,
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
            default_thinking_level: project
                .default_thinking_level
                .or(global.default_thinking_level),
        })
    }

    pub fn save_project(&self, settings: &SettingsFile) -> Result<()> {
        save_json_file(&self.project_path, settings)
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

    pub fn remove(&self, provider: &str) -> Result<()> {
        let mut data = self.load_all().unwrap_or_default();
        data.remove(provider);
        save_json_file(&self.path, &data)
    }
}

pub fn agent_dir() -> Result<PathBuf> {
    let home = home_dir().context("could not resolve home directory")?;
    let path = home.join(".pi").join("agent");
    fs::create_dir_all(&path)?;
    Ok(path)
}

pub fn sessions_root() -> Result<PathBuf> {
    let path = agent_dir()?.join("sessions");
    fs::create_dir_all(&path)?;
    Ok(path)
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
