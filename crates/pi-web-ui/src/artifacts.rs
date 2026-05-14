use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ArtifactKind {
    Html,
    Svg,
    Markdown,
    Image,
    Text,
    Other(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Artifact {
    pub id: String,
    pub title: String,
    pub kind: ArtifactKind,
    pub content: String,
}

impl Artifact {
    pub fn new(
        id: impl Into<String>,
        title: impl Into<String>,
        kind: ArtifactKind,
        content: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            kind,
            content: content.into(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ArtifactsPanel {
    artifacts: BTreeMap<String, Artifact>,
    collapsed: bool,
}

impl ArtifactsPanel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, artifact: Artifact) {
        self.artifacts.insert(artifact.id.clone(), artifact);
    }

    pub fn remove(&mut self, id: &str) -> Option<Artifact> {
        self.artifacts.remove(id)
    }

    pub fn get(&self, id: &str) -> Option<&Artifact> {
        self.artifacts.get(id)
    }

    pub fn artifacts(&self) -> impl Iterator<Item = &Artifact> {
        self.artifacts.values()
    }

    pub fn len(&self) -> usize {
        self.artifacts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.artifacts.is_empty()
    }

    pub fn set_collapsed(&mut self, collapsed: bool) {
        self.collapsed = collapsed;
    }

    pub fn collapsed(&self) -> bool {
        self.collapsed
    }
}
