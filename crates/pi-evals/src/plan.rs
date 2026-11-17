//! Port of packages/evals/src/plan.ts

use std::fmt;

use indexmap::IndexSet;
use pi_js::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Documentation-lift comparison variants, in baseline-first order.
pub const DOCUMENTATION_VARIANTS: [DocumentationVariant; 2] =
    [DocumentationVariant::WithoutDocs, DocumentationVariant::WithDocs];

/// Paired documentation variant for eval execution.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum DocumentationVariant {
    #[serde(rename = "without_docs")]
    WithoutDocs,
    #[serde(rename = "with_docs")]
    WithDocs,
}

impl DocumentationVariant {
    /// Returns the wire/environment name of this variant.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::WithoutDocs => "without_docs",
            Self::WithDocs => "with_docs",
        }
    }
}

impl fmt::Display for DocumentationVariant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Discovered documentation eval case parsed from discovery output.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredEvalCase {
    pub file: String,
    pub full_name: String,
    pub eval_set: String,
    pub case_id: String,
}

/// Scheduled eval execution unit for one case, variant, model, and repetition.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvalTask {
    pub file: String,
    pub full_name: String,
    pub eval_set: String,
    pub case_id: String,
    pub variant: DocumentationVariant,
    pub model: String,
    pub run_number: i64,
}

impl EvalTask {
    /// Constructs an [`EvalTask`] from a discovered case and execution parameters.
    pub fn from_case(
        eval_case: &DiscoveredEvalCase,
        variant: DocumentationVariant,
        model: impl Into<String>,
        run_number: i64,
    ) -> Self {
        Self {
            file: eval_case.file.clone(),
            full_name: eval_case.full_name.clone(),
            eval_set: eval_case.eval_set.clone(),
            case_id: eval_case.case_id.clone(),
            variant,
            model: model.into(),
            run_number,
        }
    }
}

/// Parses and validates raw discovered test entries from JSON discovery output.
pub fn parse_discovered_cases(value: &Value) -> Result<Vec<DiscoveredEvalCase>> {
    let Some(items) = value.as_array() else {
        return Err(Error::js("TypeError", "Discovered eval cases must be an array."));
    };
    let mut identities = IndexSet::new();
    let mut cases = Vec::with_capacity(items.len());
    for item in items {
        let Some(obj) = item.as_object() else {
            return Err(Error::js("TypeError", "Discovered eval case is invalid."));
        };
        let Some(name) = obj.get("name").and_then(Value::as_str) else {
            return Err(Error::js("TypeError", "Discovered eval case is invalid."));
        };
        let Some(file) = obj.get("file").and_then(Value::as_str) else {
            return Err(Error::js("TypeError", "Discovered eval case is invalid."));
        };
        let parts: Vec<&str> = name.split(" > ").collect();
        if parts.len() != 2 || parts[0].trim().is_empty() || parts[1].trim().is_empty() {
            return Err(Error::js(
                "TypeError",
                format!("Documentation eval must use \"<eval set> > <case>\": {name}"),
            ));
        }
        let eval_set = parts[0];
        let case_id = parts[1];
        let identity = pi_js::json::try_stringify(&(eval_set, case_id))?;
        if !identities.insert(identity) {
            return Err(Error::js("TypeError", format!("Duplicate eval case identity: {name}")));
        }
        cases.push(DiscoveredEvalCase {
            file: file.to_string(),
            full_name: name.to_string(),
            eval_set: eval_set.to_string(),
            case_id: case_id.to_string(),
        });
    }
    Ok(cases)
}

/// Expands discovered eval cases into balanced paired execution tasks across repetitions.
pub fn create_task_plan(cases: &[DiscoveredEvalCase], model: &str, runs_per_variant: i64) -> Result<Vec<EvalTask>> {
    if !model.contains('/') || model.starts_with('/') || model.ends_with('/') {
        return Err(Error::js(
            "TypeError",
            "Model identity must contain a provider and model.",
        ));
    }
    if runs_per_variant < 1 {
        return Err(Error::js("TypeError", "Runs per variant must be a positive integer."));
    }
    let mut tasks = Vec::with_capacity(cases.len().saturating_mul(runs_per_variant as usize).saturating_mul(2));
    for eval_case in cases {
        for run_number in 1..=runs_per_variant {
            let variants = if run_number % 2 == 1 {
                [DocumentationVariant::WithoutDocs, DocumentationVariant::WithDocs]
            } else {
                [DocumentationVariant::WithDocs, DocumentationVariant::WithoutDocs]
            };
            for variant in variants {
                tasks.push(EvalTask::from_case(eval_case, variant, model, run_number));
            }
        }
    }
    Ok(tasks)
}
