//! Live provider/agent capabilities and fresh issue-session admission.
use super::{OpenCodeUi, Operation};
use focus_addon_sdk::{AssistantCapabilities, AssistantChoice, AssistantModel, AssistantSession};
use serde_json::json;

fn model_id(model: &super::catalog::Model) -> String {
    json!([model.provider, model.id]).to_string()
}
impl OpenCodeUi {
    pub(crate) fn cancel_issue_start(&mut self) {
        self.issue_launch = None;
    }
    pub(crate) fn assistant_capabilities(&self) -> AssistantCapabilities {
        AssistantCapabilities {
            ready: self.can_send()
                && !self.models.is_empty()
                && !self.agents.is_empty()
                && !self.requests.values().any(|p| {
                    matches!(
                        p.operation,
                        Operation::Providers | Operation::Agents | Operation::Sessions(_)
                    )
                }),
            status: self.error.clone().unwrap_or_else(|| self.status.clone()),
            models: self
                .models
                .iter()
                .map(|model| AssistantModel {
                    id: model_id(model),
                    label: model.label.clone(),
                    reasoning: model.variants.clone(),
                    default_reasoning: None,
                })
                .collect(),
            modes: self
                .agents
                .iter()
                .map(|agent| AssistantChoice {
                    id: agent.clone(),
                    label: match agent.as_str() {
                        "plan" => "Plan".into(),
                        "build" => "Default (build)".into(),
                        _ => agent.clone(),
                    },
                })
                .collect(),
            default_model: self.selected_model().map(model_id).unwrap_or_default(),
            default_mode: self.agent.clone(),
        }
    }
    pub(crate) fn start_issue(&mut self, session: AssistantSession) -> Result<(), String> {
        let caps = self.assistant_capabilities();
        if !caps.ready {
            return Err(format!("OpenCode is not ready: {}", caps.status));
        }
        caps.validate(&session.selection)?;
        let model = self
            .models
            .iter()
            .find(|m| model_id(m) == session.selection.model)
            .unwrap();
        self.model = Some((model.provider.clone(), model.id.clone()));
        self.agent = session.selection.mode.clone();
        self.variant = session.selection.reasoning.clone().unwrap_or_default();
        self.error = None;
        self.issue_launch = Some(session);
        self.select_session(None);
        if self.error.is_some() {
            self.issue_launch = None;
            return Err(self.error.clone().unwrap());
        }
        self.status = "Opening a new issue session…".into();
        Ok(())
    }
    pub(super) fn progress_issue(&mut self) {
        let Some(session) = self.issue_launch.take() else {
            return;
        };
        if self.can_send() {
            if !self.send_prompt(session.prompt, false, None) {
                log::warn!("OpenCode issue session startup failed; see panel for details");
            }
        } else {
            self.issue_launch = Some(session);
        }
    }
}
