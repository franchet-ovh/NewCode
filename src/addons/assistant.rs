//! Route assistant requests through the host without exposing backend credentials.
use super::*;
use focus_addon_sdk::{AssistantCapabilities, InstalledAssistant};
use serde_json::json;
use std::time::Instant;

#[derive(Default)]
pub(super) struct Broker {
    capabilities: BTreeMap<String, (u64, u64, AssistantCapabilities)>,
    pending: BTreeMap<u64, Pending>,
    next_id: u64,
    published: String,
    pub reveal: Option<String>,
}
struct Pending {
    source: Task,
    target: String,
    incarnation: u64,
    started: Instant,
}
impl Addons {
    pub(super) fn assistant_pending(&self, id: &str) -> bool {
        self.assistants
            .pending
            .values()
            .any(|p| p.target == id || p.source.addon == id)
    }

    fn installed_assistants(&self) -> Vec<InstalledAssistant> {
        self.entries
            .iter()
            .filter(|a| matches!(a.manifest.id.as_str(), "codex" | "opencode"))
            .map(|a| InstalledAssistant {
                id: a.manifest.id.clone(),
                name: a.manifest.name.clone(),
                enabled: a.enabled,
                error: a.error.clone(),
                capabilities: self
                    .assistants
                    .capabilities
                    .get(&a.manifest.id)
                    .filter(|(incarnation, generation, _)| {
                        *incarnation == a.incarnation
                            && *generation == self.context.generation
                            && a.process.is_some()
                    })
                    .map(|(_, _, caps)| caps.clone()),
            })
            .collect()
    }
    pub(super) fn assistant_service(&mut self, task: Task, ctx: &egui::Context) {
        match task.call.service.clone() {
            Service::Assistants => {
                let output = serde_json::to_string(&self.installed_assistants()).unwrap();
                self.respond(task, ServiceReply::success(output));
            }
            Service::StartAssistant { addon, session } => {
                let result = (|| {
                    if self.installation_pending(&addon) {
                        return Err("The assistant is waiting for an addon update".to_owned());
                    }
                    if self.context.workspace.is_none() || self.worktree_jobs > 0 {
                        return Err("Wait for workspace checkout to finish".to_owned());
                    }
                    let entry = self
                        .installed_assistants()
                        .into_iter()
                        .find(|a| a.id == addon)
                        .ok_or("The selected assistant addon is not installed")?;
                    if !entry.enabled {
                        return Err("Enable the selected assistant in Addons first".into());
                    }
                    if let Some(error) = entry.error {
                        return Err(error);
                    }
                    let caps = entry.capabilities.ok_or(
                        "Connect or update the selected assistant addon to load its capabilities",
                    )?;
                    if !caps.ready {
                        return Err(caps.status);
                    }
                    caps.validate(&session.selection)?;
                    if session.prompt.trim().is_empty() {
                        return Err("The issue prompt is empty".into());
                    }
                    if self.assistants.pending.values().any(|p| p.target == addon) {
                        return Err("A session is already starting in this assistant".into());
                    }
                    let target = self
                        .entries
                        .iter()
                        .find(|a| a.manifest.id == addon)
                        .unwrap();
                    let process = target
                        .process
                        .as_ref()
                        .filter(|p| p.ready)
                        .ok_or("The selected assistant is not connected")?;
                    self.assistants.next_id = self.assistants.next_id.wrapping_add(1).max(1);
                    let id = self.assistants.next_id;
                    process.send(Message::request(
                        id,
                        "assistant.start",
                        json!({"generation":self.context.generation,"session":session}),
                    ))?;
                    Ok((id, target.incarnation))
                })();
                match result {
                    Ok((id, incarnation)) => {
                        self.assistants.pending.insert(
                            id,
                            Pending {
                                source: task,
                                target: addon,
                                incarnation,
                                started: Instant::now(),
                            },
                        );
                    }
                    Err(error) => self.respond(task, ServiceReply::error("assistant", error)),
                }
            }
            _ => unreachable!(),
        }
        ctx.request_repaint();
    }
    pub(super) fn assistant_event(&mut self, addon: String, incarnation: u64, message: Message) {
        if !self.entries.iter().any(|entry| {
            entry.manifest.id == addon
                && entry.incarnation == incarnation
                && entry.process.is_some()
        }) {
            return;
        }
        if message.method.as_deref() == Some("assistant.capabilities") {
            let params = message.params.unwrap_or_default();
            if params["generation"].as_u64() == Some(self.context.generation) {
                if let Ok(caps) = serde_json::from_value(params["capabilities"].clone()) {
                    self.assistants
                        .capabilities
                        .insert(addon, (incarnation, self.context.generation, caps));
                }
            }
        } else if let Some(id) = message.id {
            let matches = self
                .assistants
                .pending
                .get(&id)
                .is_some_and(|p| p.target == addon && p.incarnation == incarnation);
            if matches {
                let pending = self.assistants.pending.remove(&id).unwrap();
                let reply =
                    serde_json::from_value::<ServiceReply>(message.result.unwrap_or_default())
                        .unwrap_or_else(|_| {
                            ServiceReply::error("assistant", "Invalid assistant session reply")
                        });
                if pending.source.call.generation == self.context.generation {
                    if reply.ok {
                        self.assistants.reveal = Some(addon);
                    }
                    self.respond(pending.source, reply);
                }
            }
        }
    }
    pub(super) fn poll_assistants(&mut self) {
        let expired: Vec<_> = self
            .assistants
            .pending
            .iter()
            .filter(|(_, p)| {
                p.started.elapsed() > Duration::from_secs(15)
                    || !self.entries.iter().any(|a| {
                        a.manifest.id == p.target
                            && a.enabled
                            && a.incarnation == p.incarnation
                            && a.process.is_some()
                    })
            })
            .map(|(id, _)| *id)
            .collect();
        for id in expired {
            let pending = self.assistants.pending.remove(&id).unwrap();
            self.respond(pending.source, ServiceReply::error("assistant",
                "Assistant session startup was not acknowledged. Check its panel before retrying."));
        }
        let entries = self.installed_assistants();
        let published = serde_json::to_string(&(self.context.generation, &entries)).unwrap();
        if published != self.assistants.published {
            if let Some(process) = self
                .entries
                .iter()
                .find(|a| a.manifest.id == "github")
                .and_then(|a| a.process.as_ref())
                .filter(|p| p.ready)
            {
                if process
                    .send(Message::notification(
                        "assistants.changed",
                        json!({"generation":self.context.generation,"assistants":entries}),
                    ))
                    .is_ok()
                {
                    self.assistants.published = published;
                }
            }
        }
    }
    pub(super) fn cancel_assistant_requests(&mut self) {
        for (_, pending) in std::mem::take(&mut self.assistants.pending) {
            self.respond(
                pending.source,
                ServiceReply::error("context", "Workspace changed"),
            );
        }
        self.assistants.capabilities.clear();
        self.assistants.published.clear();
        self.assistants.reveal = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use focus_addon_sdk::{AssistantChoice, AssistantModel, AssistantSession};
    use std::{fs, os::unix::fs::PermissionsExt, thread};

    fn fixture() -> (tempfile::TempDir, Addons, egui::Context) {
        let root = tempfile::tempdir().unwrap();
        let mut host = Addons::default();
        host.context.workspace = Some(root.path().to_path_buf());
        host.context.generation = 1;
        host.generation.store(1, Ordering::SeqCst);
        for id in ["github", "codex"] {
            let directory = root.path().join("addons").join(id);
            fs::create_dir_all(&directory).unwrap();
            fs::write(
                directory.join("program"),
                r#"#!/usr/bin/env python3
import json, pathlib, sys
for line in sys.stdin:
    value = json.loads(line)
    method = value.get('method')
    def send(v):
        print(json.dumps(dict(jsonrpc='2.0', **v)), flush=True)
    if method == 'initialize':
        send(dict(id=value['id'], result=dict(protocol=1)))
    elif method == 'assistant.start':
        pathlib.Path('launch.json').write_text(json.dumps(value))
        send(dict(id=value['id'], result=dict(ok=True, code='', output='starting')))
    elif method == 'shutdown':
        break
    elif 'method' not in value:
        with open('replies.jsonl', 'a') as output:
            output.write(json.dumps(value) + '\n')
"#,
            )
            .unwrap();
            fs::set_permissions(directory.join("program"), fs::Permissions::from_mode(0o755))
                .unwrap();
            host.entries.push(Addon {
                manifest: Manifest {
                    id: id.into(),
                    name: id.into(),
                    executable: "program".into(),
                    panel: if id == "github" {
                        focus_addon_sdk::Panel::Left
                    } else {
                        focus_addon_sdk::Panel::Right
                    },
                    ..Default::default()
                },
                directory: directory.clone(),
                storage: directory.join("data"),
                enabled: true,
                process: None,
                error: None,
                view: Default::default(),
                values: Default::default(),
                icon: None,
                working: false,
                incarnation: 0,
                surface: Default::default(),
                seed: Default::default(),
                status: String::new(),
            });
        }
        let ctx = egui::Context::default();
        host.activate("github".into(), &ctx);
        host.activate("codex".into(), &ctx);
        wait(&mut host, &ctx, |host| {
            host.entries
                .iter()
                .all(|a| a.process.as_ref().is_some_and(|p| p.ready))
        });
        let incarnation = host.entries[1].incarnation;
        let caps = AssistantCapabilities {
            ready: true,
            status: "Ready".into(),
            models: vec![AssistantModel {
                id: "model".into(),
                reasoning: vec!["high".into()],
                ..Default::default()
            }],
            modes: vec![AssistantChoice {
                id: "plan".into(),
                label: "Plan".into(),
            }],
            ..Default::default()
        };
        host.assistant_event(
            "codex".into(),
            incarnation,
            Message::notification(
                "assistant.capabilities",
                json!({"generation":1,"capabilities":caps}),
            ),
        );
        (root, host, ctx)
    }
    fn wait(host: &mut Addons, ctx: &egui::Context, mut predicate: impl FnMut(&Addons) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !predicate(host) {
            assert!(Instant::now() < deadline, "Addon fixture did not respond");
            host.tick(ctx, false, false);
            thread::sleep(Duration::from_millis(10));
        }
    }
    fn request(host: &Addons, id: u64) -> Task {
        let selection = host.installed_assistants()[0]
            .capabilities
            .as_ref()
            .unwrap()
            .preferred();
        Task {
            addon: "github".into(),
            process_generation: host.entries[0].incarnation,
            request: id,
            call: ServiceCall {
                generation: 1,
                service: Service::StartAssistant {
                    addon: "codex".into(),
                    session: AssistantSession {
                        selection,
                        prompt: "Issue #42".into(),
                    },
                },
            },
        }
    }
    fn reply(root: &Path, id: u64) -> Option<ServiceReply> {
        fs::read_to_string(root.join("addons/github/replies.jsonl"))
            .ok()?
            .lines()
            .filter_map(|line| serde_json::from_str::<Message>(line).ok())
            .find(|m| m.id == Some(id))
            .and_then(|m| serde_json::from_value(m.result?).ok())
    }
    #[test]
    fn host_routes_session_to_selected_addon_and_reveals_its_tab() {
        let (root, mut host, ctx) = fixture();
        host.assistant_service(request(&host, 42), &ctx);
        wait(&mut host, &ctx, |_| reply(root.path(), 42).is_some());
        assert!(reply(root.path(), 42).unwrap().ok);
        assert_eq!(host.assistants.reveal.as_deref(), Some("codex"));
        let launch: serde_json::Value = serde_json::from_slice(
            &fs::read(root.path().join("addons/codex/launch.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(launch["params"]["session"]["selection"]["mode"], "plan");
        assert_eq!(
            launch["params"]["session"]["selection"]["reasoning"],
            "high"
        );
        assert_eq!(launch["params"]["session"]["prompt"], "Issue #42");
    }
    #[test]
    fn stale_context_and_process_catalogs_cannot_replace_current_capabilities() {
        let (_root, mut host, _) = fixture();
        let incarnation = host.entries[1].incarnation;
        for (generation, process) in [(0, incarnation), (1, incarnation + 1)] {
            host.assistant_event("codex".into(), process, Message::notification("assistant.capabilities",
                json!({"generation":generation,"capabilities":AssistantCapabilities::default()})));
        }
        assert!(
            host.installed_assistants()[0]
                .capabilities
                .as_ref()
                .unwrap()
                .ready
        );
    }
    #[test]
    fn stale_launch_is_rejected_and_disabled_addons_receive_no_prompt() {
        let (root, mut host, ctx) = fixture();
        let mut stale = request(&host, 1);
        stale.call.generation = 0;
        host.spawn(stale, &ctx);
        host.entries[1].enabled = false;
        host.assistant_service(request(&host, 2), &ctx);
        wait(&mut host, &ctx, |_| {
            reply(root.path(), 1).is_some() && reply(root.path(), 2).is_some()
        });
        assert_eq!(reply(root.path(), 1).unwrap().code, "context");
        assert!(!reply(root.path(), 2).unwrap().ok);
        assert!(!root.path().join("addons/codex/launch.json").exists());
    }
}
