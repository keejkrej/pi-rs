//! Port of packages/durable/src/harness/registry.ts

use std::sync::{Arc, LazyLock, Mutex, MutexGuard, PoisonError};

use indexmap::{IndexMap, IndexSet};

use crate::harness::agent::INSTRUCTIONS_KEY;
use crate::harness::compaction::COMPACTION_TASK;
use crate::harness::generation::GENERATION_TASK;
use crate::harness::tool::TOOL_TASK;
use crate::harness::types::{Extension, ExtensionSection, ExtensionTool, RegistryReader, RegistrySnapshot};
use crate::tasks::AnyTask;
use crate::types::JsonValue;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn is_valid_section_key(key: &str) -> bool {
    let bytes = key.as_bytes();
    !bytes.is_empty()
        && bytes[0].is_ascii_lowercase()
        && bytes[1..]
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_' || *b == b'-')
}

/// Built-in task definitions every registry holds; they are not an extension and cannot be removed or replaced.
pub static BUILTIN_TASKS: LazyLock<[AnyTask; 3]> = LazyLock::new(|| {
    [
        GENERATION_TASK.as_any().clone(),
        TOOL_TASK.as_any().clone(),
        COMPACTION_TASK.as_any().clone(),
    ]
});

/// Application-owned registry of extensions.
pub trait Registry: RegistryReader {
    /// Install `extension`, or replace the installed extension with its name in place. Publishes at once.
    fn install(&self, extension: Extension) -> pi_js::Result<()>;

    /// Remove the installed extension with `extension.name`, whichever object it is. A later install appends.
    fn uninstall(&self, extension: &Extension);
}

/// Immutable published registry state.
struct RegistryState {
    extensions: Vec<Extension>,
    by_name: IndexMap<String, Extension>,
    tasks: IndexMap<String, AnyTask>,
}

impl RegistryState {
    fn try_new(extensions: Vec<Extension>) -> pi_js::Result<Self> {
        let mut by_name = IndexMap::with_capacity(extensions.len());
        let mut tasks = IndexMap::new();
        for task in BUILTIN_TASKS.iter() {
            tasks.insert(task.name().to_owned(), task.clone());
        }
        for extension in &extensions {
            by_name.insert(extension.name.clone(), extension.clone());
            for task in &extension.tasks {
                let name = task.name().to_owned();
                if tasks.contains_key(&name) {
                    return Err(pi_js::Error::msg(format!(
                        "Task {name} of extension {} is already installed",
                        extension.name
                    )));
                }
                tasks.insert(name, task.clone());
            }
        }
        Ok(Self {
            extensions,
            by_name,
            tasks,
        })
    }
}

impl RegistrySnapshot for RegistryState {
    fn installed(&self) -> &[Extension] {
        &self.extensions
    }

    fn extension(&self, name: &str) -> Option<&Extension> {
        self.by_name.get(name)
    }

    fn tools(&self) -> Vec<ExtensionTool> {
        self.extensions
            .iter()
            .flat_map(|extension| {
                extension.tools.iter().map(move |tool| ExtensionTool {
                    extension: extension.clone(),
                    tool: tool.clone(),
                })
            })
            .collect()
    }

    fn sections(&self) -> Vec<ExtensionSection> {
        self.extensions
            .iter()
            .flat_map(|extension| {
                extension.sections.iter().map(move |section| ExtensionSection {
                    extension: extension.clone(),
                    section: section.clone(),
                })
            })
            .collect()
    }

    fn tasks(&self) -> Vec<AnyTask> {
        self.tasks.values().cloned().collect()
    }

    fn task(&self, name: &str) -> Option<&AnyTask> {
        self.tasks.get(name)
    }
}

struct Listeners {
    next_id: u64,
    map: IndexMap<u64, Arc<dyn Fn() + Send + Sync>>,
}

/// Concrete registry backing both [`Registry`] and [`RegistryReader`].
pub struct RegistryImpl {
    current: Mutex<Arc<RegistryState>>,
    listeners: Arc<Mutex<Listeners>>,
}

impl Default for RegistryImpl {
    fn default() -> Self {
        Self::new()
    }
}

impl RegistryImpl {
    pub fn new() -> Self {
        let initial = RegistryState::try_new(Vec::new()).unwrap_or_else(|_| RegistryState {
            extensions: Vec::new(),
            by_name: IndexMap::new(),
            tasks: IndexMap::new(),
        });
        Self {
            current: Mutex::new(Arc::new(initial)),
            listeners: Arc::new(Mutex::new(Listeners {
                next_id: 1,
                map: IndexMap::new(),
            })),
        }
    }

    /// Remove an extension by name (convenience alongside [`Registry::uninstall`]).
    pub fn uninstall_name(&self, name: &str) {
        let next = {
            let guard = lock(&self.current);
            if !guard.extensions.iter().any(|installed| installed.name == name) {
                return;
            }
            guard
                .extensions
                .iter()
                .filter(|installed| installed.name != name)
                .cloned()
                .collect::<Vec<_>>()
        };
        let _ = self.publish(next);
    }

    /// Build and validate the next state, failing on a task name collision, then publish synchronously.
    fn publish(&self, extensions: Vec<Extension>) -> pi_js::Result<()> {
        let next = Arc::new(RegistryState::try_new(extensions)?);
        *lock(&self.current) = next;
        let listeners: Vec<Arc<dyn Fn() + Send + Sync>> = {
            let guard = lock(&self.listeners);
            guard.map.values().cloned().collect()
        };
        for listener in listeners {
            listener();
        }
        Ok(())
    }
}

impl RegistryReader for RegistryImpl {
    fn snapshot(&self) -> Arc<dyn RegistrySnapshot> {
        Arc::clone(&lock(&self.current))
    }

    fn subscribe(&self, listener: Arc<dyn Fn() + Send + Sync>) -> pi_js::Unsubscribe {
        let id = {
            let mut guard = lock(&self.listeners);
            let id = guard.next_id;
            guard.next_id += 1;
            guard.map.insert(id, listener);
            id
        };
        let listeners = Arc::clone(&self.listeners);
        Arc::new(move || {
            lock(&listeners).map.shift_remove(&id);
        })
    }
}

impl Registry for RegistryImpl {
    fn install(&self, extension: Extension) -> pi_js::Result<()> {
        validate_extension(&extension)?;
        let next = {
            let guard = lock(&self.current);
            let current = &guard.extensions;
            if let Some(index) = current.iter().position(|installed| installed.name == extension.name) {
                current
                    .iter()
                    .enumerate()
                    .map(|(at, installed)| {
                        if at == index {
                            extension.clone()
                        } else {
                            installed.clone()
                        }
                    })
                    .collect::<Vec<_>>()
            } else {
                let mut next = Vec::with_capacity(current.len() + 1);
                next.extend(current.iter().cloned());
                next.push(extension);
                next
            }
        };
        self.publish(next)
    }

    fn uninstall(&self, extension: &Extension) {
        self.uninstall_name(&extension.name);
    }
}

/// Unique tool names and section keys within one extension; valid, unreserved section keys.
fn validate_extension(extension: &Extension) -> pi_js::Result<()> {
    let mut tools = IndexSet::new();
    for tool in &extension.tools {
        if !tools.insert(tool.name.as_str()) {
            return Err(pi_js::Error::msg(format!(
                "Extension {} has two tools named {}",
                extension.name, tool.name
            )));
        }
    }
    let mut sections = IndexSet::new();
    for section in &extension.sections {
        if !is_valid_section_key(&section.key) {
            return Err(pi_js::Error::js(
                "TypeError",
                format!(
                    "Section key {} must match /^[a-z][a-z0-9_-]*$/",
                    pi_js::json::stringify(&JsonValue::String(section.key.clone()))
                ),
            ));
        }
        if section.key == INSTRUCTIONS_KEY {
            return Err(pi_js::Error::msg(format!(
                "Section key {} is reserved for the agent's instructions",
                section.key
            )));
        }
        if !sections.insert(section.key.as_str()) {
            return Err(pi_js::Error::msg(format!(
                "Extension {} has two sections with key {}",
                extension.name, section.key
            )));
        }
    }
    Ok(())
}

/// Create an application-owned registry holding only the built-in tasks.
pub fn create_registry() -> Arc<RegistryImpl> {
    Arc::new(RegistryImpl::new())
}
