//! Provider-neutral assistant choices; IDs always come from the selected backend.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssistantChoice {
    pub id: String,
    pub label: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssistantModel {
    pub id: String,
    pub label: String,
    pub reasoning: Vec<String>,
    pub default_reasoning: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssistantCapabilities {
    pub ready: bool,
    pub status: String,
    pub models: Vec<AssistantModel>,
    pub modes: Vec<AssistantChoice>,
    pub default_model: String,
    pub default_mode: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssistantSelection {
    pub model: String,
    pub mode: String,
    pub reasoning: Option<String>,
}

impl AssistantCapabilities {
    pub fn preferred(&self) -> AssistantSelection {
        let model = self
            .models
            .iter()
            .find(|m| m.id == self.default_model)
            .or_else(|| self.models.first());
        let mode = self
            .modes
            .iter()
            .find(|m| m.id == "plan")
            .or_else(|| self.modes.iter().find(|m| m.id == self.default_mode))
            .or_else(|| self.modes.first());
        AssistantSelection {
            model: model.map(|m| m.id.clone()).unwrap_or_default(),
            mode: mode.map(|m| m.id.clone()).unwrap_or_default(),
            reasoning: model.and_then(|m| {
                m.reasoning
                    .iter()
                    .find(|r| r.eq_ignore_ascii_case("high"))
                    .cloned()
                    .or_else(|| m.default_reasoning.clone())
            }),
        }
    }

    pub fn validate(&self, selection: &AssistantSelection) -> Result<(), String> {
        let model = self
            .models
            .iter()
            .find(|m| m.id == selection.model)
            .ok_or("The selected model is no longer available; reload AI options")?;
        if !self.modes.iter().any(|m| m.id == selection.mode) {
            return Err("The selected mode is no longer available; reload AI options".into());
        }
        if selection
            .reasoning
            .as_ref()
            .is_some_and(|r| !model.reasoning.contains(r))
        {
            return Err("The selected reasoning option is not supported by this model".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AssistantSession {
    pub selection: AssistantSelection,
    pub prompt: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InstalledAssistant {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub capabilities: Option<AssistantCapabilities>,
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preferences_use_only_advertised_choices() {
        let mut caps = AssistantCapabilities {
            models: vec![AssistantModel {
                id: "model".into(),
                reasoning: vec!["low".into(), "high".into()],
                default_reasoning: Some("low".into()),
                ..Default::default()
            }],
            modes: vec![
                AssistantChoice {
                    id: "build".into(),
                    label: "Default".into(),
                },
                AssistantChoice {
                    id: "plan".into(),
                    label: "Plan".into(),
                },
            ],
            ..Default::default()
        };
        let preferred = caps.preferred();
        assert_eq!(preferred.mode, "plan");
        assert_eq!(preferred.reasoning.as_deref(), Some("high"));
        caps.validate(&preferred).unwrap();
        caps.models[0].reasoning.retain(|r| r != "high");
        caps.modes.retain(|m| m.id != "plan");
        assert!(caps.validate(&preferred).is_err());
        assert_eq!(caps.preferred().mode, "build");
        assert_eq!(caps.preferred().reasoning.as_deref(), Some("low"));
        caps.models[0].reasoning.clear();
        caps.models[0].default_reasoning = None;
        assert_eq!(caps.preferred().reasoning, None);
    }
}
