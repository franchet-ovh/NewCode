//! Start an issue in a fresh thread after validating the live backend catalog.
use super::CodexUi;
use focus_addon_sdk::{AssistantCapabilities, AssistantChoice, AssistantModel, AssistantSession};
use serde_json::{Value, json};
use std::path::Path;

pub(super) struct Launch {
    pub request: u64,
    pub session: AssistantSession,
}
impl CodexUi {
    pub(crate) fn cancel_issue_start(&mut self) {
        self.issue_launch = None;
    }
    pub(crate) fn assistant_capabilities(&self) -> AssistantCapabilities {
        let models: Vec<_> = self
            .models
            .iter()
            .filter_map(|model| {
                let id = model["model"].as_str()?;
                let reasoning: Vec<_> = model["supportedReasoningEfforts"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|r| r["reasoningEffort"].as_str().map(str::to_owned))
                    .collect();
                Some(AssistantModel {
                    id: id.into(),
                    label: model["displayName"].as_str().unwrap_or(id).into(),
                    default_reasoning: model["defaultReasoningEffort"]
                        .as_str()
                        .filter(|r| reasoning.iter().any(|value| value == r))
                        .map(str::to_owned),
                    reasoning,
                })
            })
            .collect();
        let mut modes: Vec<_> = self
            .modes
            .iter()
            .filter_map(|mode| {
                let id = mode["mode"].as_str()?;
                Some(AssistantChoice {
                    id: id.into(),
                    label: mode["name"].as_str().unwrap_or(id).into(),
                })
            })
            .collect();
        if modes.is_empty() {
            modes.push(AssistantChoice {
                id: String::new(),
                label: "Default".into(),
            });
        }
        let ready = self.server.is_some()
            && self.thread_id.is_some()
            && self.initializing_id.is_none()
            && !self.busy
            && self.pending.is_empty()
            && self.issue_launch.is_none()
            && !models.is_empty()
            && !self.requests.values().any(|method| {
                matches!(
                    method.as_str(),
                    "model/list"
                        | "collaborationMode/list"
                        | "thread/start"
                        | "thread/resume"
                        | "thread/list"
                )
            });
        AssistantCapabilities {
            ready,
            status: self.status.clone(),
            models,
            modes,
            default_model: self.model.clone(),
            default_mode: self
                .mode
                .as_ref()
                .and_then(|m| m["mode"].as_str())
                .unwrap_or("")
                .into(),
        }
    }
    pub(crate) fn start_issue(
        &mut self,
        session: AssistantSession,
        cwd: Option<&Path>,
    ) -> Result<(), String> {
        let caps = self.assistant_capabilities();
        if !caps.ready {
            return Err(format!("Codex is not ready: {}", caps.status));
        }
        caps.validate(&session.selection)?;
        let request = self.send(
            "thread/start",
            json!({
                "cwd": self.remote_cwd(cwd), "model": session.selection.model
            }),
        )?;
        self.issue_launch = Some(Launch { request, session });
        self.busy = true;
        self.clear_plan();
        self.status = "Opening a new issue session…".into();
        Ok(())
    }
    pub(super) fn submit_issue(&mut self, id: &Value) {
        if !self
            .issue_launch
            .as_ref()
            .is_some_and(|launch| Some(launch.request) == id.as_u64())
        {
            return;
        }
        let launch = self.issue_launch.take().unwrap();
        let result = self
            .assistant_capabilities()
            .validate(&launch.session.selection)
            .and_then(|()| {
                self.model = launch.session.selection.model;
                self.effort = launch.session.selection.reasoning;
                self.mode = self
                    .modes
                    .iter()
                    .find(|mode| mode["mode"].as_str() == Some(&launch.session.selection.mode))
                    .cloned();
                self.submit_text(&launch.session.prompt).map(|_| ())
            });
        if let Err(error) = result {
            self.status = format!("Cannot start issue session: {error}");
            self.busy = false;
            log::warn!("Codex issue session startup failed; see panel for details");
        }
    }
}
