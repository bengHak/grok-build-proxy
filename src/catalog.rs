use serde::Serialize;
use std::collections::{HashMap, HashSet};

const FAST_SUFFIX: &str = "-fast";

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ReasoningEffort {
    pub value: String,
    pub label: String,
    pub description: String,
    pub default: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReasoningCapability {
    pub default_effort: String,
    pub efforts: Vec<ReasoningEffort>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Model {
    pub id: String,
    pub display_name: String,
    pub description: String,
    pub context_window: u64,
    pub responses_lite: bool,
    pub reasoning: Option<ReasoningCapability>,
}

fn reasoning(default: &str) -> ReasoningCapability {
    let values = [
        ("low", "Low", "Faster responses with lighter reasoning."),
        ("medium", "Medium", "Balanced reasoning for most tasks."),
        ("high", "High", "Deeper reasoning for complex tasks."),
        ("xhigh", "Extra high", "Maximum supported reasoning depth."),
    ];
    ReasoningCapability {
        default_effort: default.into(),
        efforts: values
            .into_iter()
            .map(|(value, label, description)| ReasoningEffort {
                value: value.into(),
                label: label.into(),
                description: description.into(),
                default: value == default,
            })
            .collect(),
    }
}

fn known(id: &str) -> Option<Model> {
    let (name, description, context, lite, effort) = match id {
        "gpt-5.6-sol" => (
            "GPT-5.6 Sol",
            "Latest frontier agentic coding model.",
            372_000,
            true,
            Some("low"),
        ),
        "gpt-5.6-terra" => (
            "GPT-5.6 Terra",
            "Balanced agentic coding model for everyday work.",
            372_000,
            true,
            Some("medium"),
        ),
        "gpt-5.6-luna" => (
            "GPT-5.6 Luna",
            "Fast agentic coding model.",
            372_000,
            true,
            Some("medium"),
        ),
        "gpt-5.5" => (
            "GPT-5.5",
            "Frontier model for complex coding and real-world work.",
            272_000,
            false,
            Some("medium"),
        ),
        "gpt-5.2" => (
            "GPT-5.2",
            "Model optimized for professional work and long-running agents.",
            272_000,
            false,
            None,
        ),
        _ => return None,
    };
    Some(Model {
        id: id.into(),
        display_name: name.into(),
        description: description.into(),
        context_window: context,
        responses_lite: lite,
        reasoning: effort.map(reasoning),
    })
}

pub fn normalize_id(id: &str) -> (String, bool) {
    let id = id.trim();
    if let Some(base) = id.strip_suffix(FAST_SUFFIX).filter(|s| !s.is_empty()) {
        (base.into(), true)
    } else {
        (id.into(), false)
    }
}

#[derive(Clone, Debug)]
pub struct Catalog {
    models: HashMap<String, Model>,
    order: Vec<String>,
}

impl Catalog {
    pub fn new(csv: &str) -> Self {
        let defaults = "gpt-5.6-sol,gpt-5.6-terra,gpt-5.6-luna,gpt-5.5,gpt-5.2";
        let source = if csv.trim().is_empty() { defaults } else { csv };
        let mut seen = HashSet::new();
        let mut models = HashMap::new();
        let mut order = Vec::new();
        for raw in source.split(',') {
            let (id, _) = normalize_id(raw);
            if id.is_empty() || !seen.insert(id.clone()) {
                continue;
            }
            let model = known(&id).unwrap_or_else(|| Model {
                id: id.clone(),
                display_name: id.clone(),
                description: String::new(),
                context_window: 272_000,
                responses_lite: id.starts_with("gpt-5.6-"),
                reasoning: None,
            });
            models.insert(id.clone(), model);
            order.push(id);
        }
        Self { models, order }
    }

    pub fn lookup(&self, id: &str) -> (Model, bool) {
        let (base, _) = normalize_id(id);
        if let Some(model) = self.models.get(&base) {
            return (model.clone(), true);
        }
        if let Some(model) = known(&base) {
            return (model, true);
        }
        (
            Model {
                id: base.clone(),
                display_name: base.clone(),
                description: String::new(),
                context_window: 272_000,
                responses_lite: base.starts_with("gpt-5.6-"),
                reasoning: None,
            },
            false,
        )
    }
    pub fn models(&self) -> Vec<Model> {
        self.order
            .iter()
            .filter_map(|id| self.models.get(id).cloned())
            .collect()
    }
    pub fn ids(&self) -> Vec<String> {
        self.order.clone()
    }
}

impl Default for Catalog {
    fn default() -> Self {
        Self::new("")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn catalog_defaults_and_fast_normalization() {
        let c = Catalog::default();
        assert_eq!(c.ids()[0], "gpt-5.6-sol");
        assert_eq!(
            normalize_id(" gpt-5.6-sol-fast "),
            ("gpt-5.6-sol".into(), true)
        );
        assert!(c.lookup("new-model").0.display_name == "new-model");
    }
}
