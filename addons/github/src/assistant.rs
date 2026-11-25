//! Issue-to-assistant handoff using catalogs advertised by installed addons.
use crate::{
    App, Detail,
    api::{Host, string},
};
use focus_addon_sdk::{
    Action, AssistantCapabilities, AssistantSession, InstalledAssistant, Service, Widget as W,
};

impl App {
    fn selected_assistant(&self) -> Option<&InstalledAssistant> {
        self.assistants.iter().find(|a| a.id == self.assistant)
    }
    pub(crate) fn update_assistants(&mut self, entries: Vec<InstalledAssistant>) {
        self.assistants = entries;
        if !self.assistants.iter().any(|a| a.id == self.assistant) {
            self.assistant = self
                .assistants
                .iter()
                .find(|a| a.enabled)
                .or_else(|| self.assistants.first())
                .map(|a| a.id.clone())
                .unwrap_or_default();
            self.assistant_selection = Default::default();
        }
        if let Some(caps) = self
            .selected_assistant()
            .and_then(|a| a.capabilities.as_ref())
        {
            if caps.validate(&self.assistant_selection).is_err() {
                self.assistant_selection = caps.preferred();
            }
        }
        self.assistant_notice.clear();
    }
    pub(crate) fn load_assistants(&mut self, host: &mut impl Host) {
        let result = host
            .service(self.context.generation, Service::Assistants)
            .and_then(|reply| reply.result())
            .and_then(|text| {
                serde_json::from_str(&text).map_err(|e| format!("Invalid assistant catalog: {e}"))
            });
        match result {
            Ok(entries) => self.update_assistants(entries),
            Err(error) => self.assistant_notice = format!("Cannot load AI options: {error}"),
        }
    }
    pub(crate) fn assistant_options(&mut self, action: &Action) {
        let id = action.text("assistant");
        let changed = id != self.assistant;
        if self.assistants.iter().any(|a| a.id == id) {
            self.assistant = id;
        }
        let Some(mut caps) = self
            .selected_assistant()
            .and_then(|a| a.capabilities.clone())
        else {
            return;
        };
        if changed {
            self.assistant_selection = caps.preferred();
            return;
        }
        let model = action.text("assistant_model");
        if caps.models.iter().any(|m| m.id == model) {
            caps.default_model = model;
        }
        let mut selection = caps.preferred();
        let mode = action.text("assistant_mode");
        if caps.modes.iter().any(|m| m.id == mode) {
            selection.mode = mode;
        }
        let reasoning = action.text("assistant_reasoning");
        if reasoning.is_empty() {
            selection.reasoning = None;
        } else if caps
            .models
            .iter()
            .find(|m| m.id == selection.model)
            .is_some_and(|m| m.reasoning.contains(&reasoning))
        {
            selection.reasoning = Some(reasoning);
        }
        self.assistant_selection = selection;
    }
    pub(crate) fn issue_assistant(
        &mut self,
        host: &mut impl Host,
        action: &Action,
    ) -> Result<(), String> {
        let chosen = action.text("assistant");
        if !chosen.is_empty() {
            self.assistant = chosen;
        }
        let selected = self
            .selected_assistant()
            .ok_or("Install Codex or OpenCode in Addons first")?;
        if !selected.enabled {
            return Err("Enable the selected assistant in Addons first".into());
        }
        let caps = selected
            .capabilities
            .as_ref()
            .ok_or("Connect or update the selected assistant to load AI options")?;
        if !caps.ready {
            return Err(format!("{} is not ready: {}", selected.name, caps.status));
        }
        let mut selection = self.assistant_selection.clone();
        if action.values.contains_key("assistant_model") {
            selection.model = action.text("assistant_model");
        }
        if action.values.contains_key("assistant_mode") {
            selection.mode = action.text("assistant_mode");
        }
        if action.values.contains_key("assistant_reasoning") {
            let reasoning = action.text("assistant_reasoning");
            selection.reasoning = (!reasoning.is_empty()).then_some(reasoning);
        }
        caps.validate(&selection)?;
        let Detail::Item { value, pull: false } = &self.detail else {
            return Err("Select an issue first".into());
        };
        let name = selected.name.clone();
        let session = AssistantSession {
            selection,
            prompt: format!(
                "Work on this GitHub issue in the current workspace using the selected mode.\n\nRepository: {}\nIssue #{}: {}\nURL: {}\n\nIssue description:\n{}",
                self.repo()?.slug(),
                value["number"],
                string(value, "title"),
                string(value, "html_url"),
                value["body"].as_str().unwrap_or("No description provided.")
            ),
        };
        self.issue_branch(host, action)?;
        host.service(
            self.context.generation,
            Service::StartAssistant {
                addon: self.assistant.clone(),
                session,
            },
        )
        .and_then(|reply| reply.result())
        .map_err(|error| {
            format!("Issue branch checked out, but {name} could not start: {error}")
        })?;
        self.notice = format!("Issue branch checked out. A new session is starting in {name}.");
        Ok(())
    }
    pub(crate) fn issue_assistant_widgets(&self) -> Vec<W> {
        let mut widgets = vec![W::heading("Work with an assistant")];
        if !self.assistant_notice.is_empty() {
            widgets.push(W::text(&self.assistant_notice));
        }
        if self.assistants.is_empty() {
            widgets.push(W::text(
                "Install and enable Codex or OpenCode in Addons to start an AI session.",
            ));
        } else {
            widgets.push(select(
                "assistant",
                "Assistant",
                &self.assistant,
                self.assistants
                    .iter()
                    .map(|a| {
                        (
                            a.id.clone(),
                            format!("{}{}", a.name, if a.enabled { "" } else { " (disabled)" }),
                        )
                    })
                    .collect(),
            ));
        }
        let assistant = self.selected_assistant();
        let caps = assistant.and_then(|a| a.capabilities.as_ref());
        if let Some(caps) = caps {
            widgets.extend(self.capability_widgets(caps));
        }
        if let Some(assistant) = assistant {
            if !assistant.enabled {
                widgets.push(W::text("Enable this assistant in Addons first."));
            } else if let Some(error) = &assistant.error {
                widgets.push(W::text(error));
            } else if caps.is_none() {
                widgets.push(W::text("Waiting for assistant capabilities. Connect the assistant, or update it if these options do not appear."));
            } else if !caps.unwrap().ready {
                widgets.push(W::text(format!(
                    "{}: {}",
                    assistant.name,
                    caps.unwrap().status
                )));
            }
        }
        widgets.push(W::button("assistant_refresh", "Reload AI options"));
        widgets.push(W::Button {
            style: focus_addon_sdk::ButtonStyle {
                role: focus_addon_sdk::ButtonRole::Primary,
                ..Default::default()
            },
            id: "issue_assistant".into(),
            label: "Create branch, checkout & start AI".into(),
            enabled: assistant.is_some_and(|a| a.enabled && a.error.is_none())
                && caps.is_some_and(|c| c.ready && c.validate(&self.assistant_selection).is_ok()),
        });
        widgets
    }
    fn capability_widgets(&self, caps: &AssistantCapabilities) -> Vec<W> {
        let selection = &self.assistant_selection;
        let mut widgets = vec![
            select(
                "assistant_model",
                "Model",
                &selection.model,
                caps.models
                    .iter()
                    .map(|m| (m.id.clone(), m.label.clone()))
                    .collect(),
            ),
            select(
                "assistant_mode",
                "Mode",
                &selection.mode,
                caps.modes
                    .iter()
                    .map(|m| (m.id.clone(), m.label.clone()))
                    .collect(),
            ),
        ];
        if let Some(model) = caps.models.iter().find(|m| m.id == selection.model) {
            let mut options = vec![(String::new(), "Provider default".into())];
            options.extend(model.reasoning.iter().map(|r| (r.clone(), r.clone())));
            widgets.push(select(
                "assistant_reasoning",
                "Reasoning",
                selection.reasoning.as_deref().unwrap_or(""),
                options,
            ));
        }
        widgets
    }
}
fn select(id: &str, label: &str, value: &str, options: Vec<(String, String)>) -> W {
    W::Select {
        id: id.into(),
        label: label.into(),
        value: value.into(),
        options,
        on_change: Some("assistant_options".into()),
    }
}
