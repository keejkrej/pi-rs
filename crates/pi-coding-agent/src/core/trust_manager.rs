//! Port of packages/coding-agent/src/core/trust-manager.ts

use indexmap::IndexMap;
use pi_js::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::config::CONFIG_DIR_NAME;
use crate::utils::paths::{canonicalize_path, paths_equal, resolve_path};
use crate::utils::text::strip_bom;
use crate::vendor::proper_lockfile::{LockOptions, LockRelease, lock_sync};

pub type ProjectTrustDecision = Option<bool>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectTrustStoreEntry {
    pub path: String,
    pub decision: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectTrustUpdate {
    pub path: String,
    pub decision: ProjectTrustDecision,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectTrustOption {
    pub label: String,
    pub trusted: bool,
    pub updates: Vec<ProjectTrustUpdate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saved_path: Option<String>,
}

type TrustFile = IndexMap<String, Option<bool>>;

const TRUST_REQUIRING_PROJECT_CONFIG_RESOURCES: &[&str] = &[
    "settings.json",
    "mcp.json",
    "extensions",
    "skills",
    "prompts",
    "themes",
    "SYSTEM.md",
    "APPEND_SYSTEM.md",
];

fn normalize_cwd(cwd: &str) -> String {
    canonicalize_path(&resolve_path(cwd, None, Default::default()))
}

fn find_trust_file_keys(data: &TrustFile, path: &str) -> Vec<String> {
    data.keys().filter(|key| paths_equal(key, path)).cloned().collect()
}

fn find_nearest_trust_entry(data: &TrustFile, cwd: &str) -> Option<ProjectTrustStoreEntry> {
    let mut current_dir = normalize_cwd(cwd);
    loop {
        for key in find_trust_file_keys(data, &current_dir) {
            if let Some(Some(decision)) = data.get(&key).copied() {
                return Some(ProjectTrustStoreEntry { path: key, decision });
            }
        }
        let parent_dir = pi_js::path::dirname(&current_dir);
        if parent_dir == current_dir {
            return None;
        }
        current_dir = parent_dir;
    }
}

pub fn get_project_trust_parent_path(cwd: &str) -> Option<String> {
    let trust_path = normalize_cwd(cwd);
    let parent_dir = pi_js::path::dirname(&trust_path);
    if parent_dir == trust_path {
        None
    } else {
        Some(parent_dir)
    }
}

pub fn get_project_trust_options(cwd: &str, include_session_only: bool) -> Vec<ProjectTrustOption> {
    let trust_path = normalize_cwd(cwd);
    let mut trust_options = vec![ProjectTrustOption {
        label: "Trust".to_owned(),
        trusted: true,
        updates: vec![ProjectTrustUpdate {
            path: trust_path.clone(),
            decision: Some(true),
        }],
        saved_path: Some(trust_path.clone()),
    }];

    if let Some(parent_path) = get_project_trust_parent_path(cwd) {
        trust_options.push(ProjectTrustOption {
            label: format!("Trust parent folder ({parent_path})"),
            trusted: true,
            updates: vec![
                ProjectTrustUpdate {
                    path: parent_path.clone(),
                    decision: Some(true),
                },
                ProjectTrustUpdate {
                    path: trust_path.clone(),
                    decision: None,
                },
            ],
            saved_path: Some(parent_path),
        });
    }

    if include_session_only {
        trust_options.push(ProjectTrustOption {
            label: "Trust (this session only)".to_owned(),
            trusted: true,
            updates: Vec::new(),
            saved_path: None,
        });
    }

    trust_options.push(ProjectTrustOption {
        label: "Do not trust".to_owned(),
        trusted: false,
        updates: vec![ProjectTrustUpdate {
            path: trust_path.clone(),
            decision: Some(false),
        }],
        saved_path: Some(trust_path),
    });

    if include_session_only {
        trust_options.push(ProjectTrustOption {
            label: "Do not trust (this session only)".to_owned(),
            trusted: false,
            updates: Vec::new(),
            saved_path: None,
        });
    }

    trust_options
}

fn read_trust_file(path: &str) -> Result<TrustFile> {
    if !pi_js::fs::exists(path) {
        return Ok(TrustFile::new());
    }
    let content = pi_js::fs::read_to_string(path)
        .map_err(|err| Error::msg(format!("Failed to read trust store {path}: {err}")))?;
    let parsed = pi_js::json::parse(strip_bom(&content))
        .map_err(|err| Error::msg(format!("Failed to read trust store {path}: {err}")))?;
    let Some(obj) = parsed.as_object() else {
        return Err(Error::msg(format!("Invalid trust store {path}: expected an object")));
    };

    let mut data = TrustFile::new();
    for (key, value) in obj {
        let decision = match value {
            Value::Bool(b) => Some(*b),
            Value::Null => None,
            _ => {
                return Err(Error::msg(format!(
                    "Invalid trust store {path}: value for {} must be true, false, or null",
                    pi_js::json::stringify(key)
                )));
            }
        };
        data.insert(key.clone(), decision);
    }
    Ok(data)
}

fn write_trust_file(path: &str, data: &TrustFile) -> Result<()> {
    let mut keys: Vec<&String> = data.keys().collect();
    keys.sort();
    let mut sorted = Map::new();
    for key in keys {
        match data.get(key) {
            Some(Some(b)) => {
                sorted.insert(key.clone(), Value::Bool(*b));
            }
            Some(None) => {
                sorted.insert(key.clone(), Value::Null);
            }
            None => {}
        }
    }
    let dir = pi_js::path::dirname(path);
    pi_js::fs::mkdir(&dir, true, None)?;
    let body = format!("{}\n", pi_js::json::stringify_pretty(&sorted, "  "));
    pi_js::fs::write_file(path, body.as_bytes(), None)
}

fn acquire_trust_lock_sync(path: &str) -> Result<LockRelease> {
    let trust_dir = pi_js::path::dirname(path);
    pi_js::fs::mkdir(&trust_dir, true, None)?;
    let max_attempts = 10;
    let delay_ms: i64 = 20;

    for attempt in 1..=max_attempts {
        match lock_sync(
            &trust_dir,
            LockOptions {
                realpath: Some(false),
                lockfile_path: Some(format!("{path}.lock")),
                ..Default::default()
            },
        ) {
            Ok(release) => return Ok(release),
            Err(err) => {
                if err.code() != Some("ELOCKED") || attempt == max_attempts {
                    return Err(err);
                }
                let start = pi_js::time::now_ms();
                while pi_js::time::now_ms() - start < delay_ms {
                    std::thread::yield_now();
                }
            }
        }
    }
    Err(Error::msg("Failed to acquire trust store lock"))
}

fn with_trust_file_lock<T>(path: &str, f: impl FnOnce() -> Result<T>) -> Result<T> {
    let release = acquire_trust_lock_sync(path)?;
    let res = f();
    let unlock_res = release.release_sync();
    match (res, unlock_res) {
        (Err(e), _) => Err(e),
        (Ok(_), Err(u)) => Err(u),
        (Ok(val), Ok(())) => Ok(val),
    }
}

pub fn has_trust_requiring_project_resources(cwd: &str) -> bool {
    let home_raw = pi_js::env::var("HOME")
        .filter(|s| !s.is_empty())
        .unwrap_or_else(pi_js::env::home_dir);
    let home_dir = canonicalize_path(&resolve_path(&home_raw, None, Default::default()));
    let user_agents_skills_dir = pi_js::path::join(&[&home_dir, ".agents", "skills"]);
    let mut current_dir = canonicalize_path(&resolve_path(cwd, None, Default::default()));

    let config_dir = pi_js::path::join(&[&current_dir, &CONFIG_DIR_NAME]);
    if TRUST_REQUIRING_PROJECT_CONFIG_RESOURCES
        .iter()
        .any(|entry| pi_js::fs::exists(&pi_js::path::join(&[&config_dir, entry])))
    {
        return true;
    }

    loop {
        let agents_skills_dir = pi_js::path::join(&[&current_dir, ".agents", "skills"]);
        if !paths_equal(&agents_skills_dir, &user_agents_skills_dir) && pi_js::fs::exists(&agents_skills_dir) {
            return true;
        }
        let parent_dir = pi_js::path::dirname(&current_dir);
        if parent_dir == current_dir {
            return false;
        }
        current_dir = parent_dir;
    }
}

#[derive(Clone, Debug)]
pub struct ProjectTrustStore {
    trust_path: String,
}

impl ProjectTrustStore {
    pub fn new(agent_dir: &str) -> Self {
        let resolved = resolve_path(agent_dir, None, Default::default());
        Self {
            trust_path: pi_js::path::join(&[&resolved, "trust.json"]),
        }
    }

    pub fn get(&self, cwd: &str) -> Result<ProjectTrustDecision> {
        Ok(self.get_entry(cwd)?.map(|e| e.decision))
    }

    pub fn get_entry(&self, cwd: &str) -> Result<Option<ProjectTrustStoreEntry>> {
        with_trust_file_lock(&self.trust_path, || {
            let data = read_trust_file(&self.trust_path)?;
            Ok(find_nearest_trust_entry(&data, cwd))
        })
    }

    pub fn set(&self, cwd: &str, decision: ProjectTrustDecision) -> Result<()> {
        self.set_many(&[ProjectTrustUpdate {
            path: cwd.to_owned(),
            decision,
        }])
    }

    pub fn set_many(&self, decisions: &[ProjectTrustUpdate]) -> Result<()> {
        with_trust_file_lock(&self.trust_path, || {
            let mut data = read_trust_file(&self.trust_path)?;
            for item in decisions {
                let key = normalize_cwd(&item.path);
                for existing_key in find_trust_file_keys(&data, &key) {
                    data.shift_remove(&existing_key);
                }
                if let Some(b) = item.decision {
                    data.insert(key, Some(b));
                }
            }
            write_trust_file(&self.trust_path, &data)
        })
    }
}
